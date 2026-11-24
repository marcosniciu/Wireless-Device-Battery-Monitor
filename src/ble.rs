//! One passive BLE advertisement watcher shared by the drivers that read battery levels from
//! broadcasts (AirPods proximity messages and Google Fast Pair). It only listens; nothing is sent.
//! Port of ble_adverts.cpp / fastpair.cpp, using the `windows` crate's WinRT projection instead of
//! hand-written ABI declarations.
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use windows::Devices::Bluetooth::Advertisement::{
    BluetoothLEAdvertisementReceivedEventArgs, BluetoothLEAdvertisementWatcher, BluetoothLEAdvertisementWatcherStatus,
    BluetoothLEScanningMode,
};
use windows::Foundation::TypedEventHandler;
use windows::Storage::Streams::{DataReader, IBuffer};

use crate::drivers::airpods;

// ---- Fast Pair ---------------------------------------------------------------------------------

/// Battery split from the latest Fast Pair broadcast (percent, -1 unknown).
#[derive(Clone, Copy, Debug)]
pub struct FastPairBattery {
    pub left: i32,
    pub right: i32,
    pub case: i32,
    pub left_chg: bool,
    pub right_chg: bool,
    pub case_chg: bool,
    pub at: Instant,
}

fn fast_pair_map() -> &'static Mutex<HashMap<u64, FastPairBattery>> {
    static M: OnceLock<Mutex<HashMap<u64, FastPairBattery>>> = OnceLock::new();
    M.get_or_init(Default::default)
}

fn fp_component(b: u8) -> (i32, bool) {
    let level = (b & 0x7F) as i32;
    if level <= 100 {
        (level, b & 0x80 != 0)
    } else {
        (-1, false)
    }
}

/// Service data of 16-bit UUID 0xFE2C after the UUID: [0] version 0, then fields with header
/// 0bLLLLTTTT; types 3/4 = battery (left, right, case; bit 7 charging, 0x7F unknown).
fn parse_fast_pair(d: &[u8]) -> Option<FastPairBattery> {
    if d.len() < 2 || d[0] != 0 {
        return None;
    }
    let mut i = 1;
    while i < d.len() {
        let len = (d[i] >> 4) as usize;
        let ty = d[i] & 0x0F;
        if i + 1 + len > d.len() {
            return None;
        }
        if (ty == 3 || ty == 4) && len >= 1 {
            let (left, left_chg) = fp_component(d[i + 1]);
            let (right, right_chg) = if len >= 2 { fp_component(d[i + 2]) } else { (-1, false) };
            let (case, case_chg) = if len >= 3 { fp_component(d[i + 3]) } else { (-1, false) };
            let b = FastPairBattery { left, right, case, left_chg, right_chg, case_chg, at: Instant::now() };
            return (left >= 0 || right >= 0 || case >= 0).then_some(b);
        }
        i += 1 + len;
    }
    None
}

/// Latest broadcast of the device with this Bluetooth address, if heard in the last 5 minutes.
pub fn fast_pair_lookup(addr: u64) -> Option<FastPairBattery> {
    let m = fast_pair_map().lock().ok()?;
    m.get(&addr).filter(|b| b.at.elapsed().as_secs() < 300).copied()
}

// ---- watcher -----------------------------------------------------------------------------------

fn buffer_bytes(buf: &IBuffer) -> Option<Vec<u8>> {
    let len = buf.Length().ok()? as usize;
    let reader = DataReader::FromBuffer(buf).ok()?;
    let mut v = vec![0u8; len];
    reader.ReadBytes(&mut v).ok()?;
    Some(v)
}

static RECEIVED: AtomicUsize = AtomicUsize::new(0);

/// Number of advertisements received so far (diagnostics).
pub fn received_count() -> usize {
    RECEIVED.load(Ordering::Relaxed)
}

fn on_received(args: &BluetoothLEAdvertisementReceivedEventArgs) -> windows::core::Result<()> {
    RECEIVED.fetch_add(1, Ordering::Relaxed);
    let adv = args.Advertisement()?;
    let addr = args.BluetoothAddress()?;
    let rssi = args.RawSignalStrengthInDBm()? as i32;

    let mdata = adv.GetManufacturerDataByCompanyId(0x004C)?;
    for i in 0..mdata.Size()? {
        if let Some(bytes) = mdata.GetAt(i).and_then(|m| m.Data()).ok().and_then(|b| buffer_bytes(&b)) {
            airpods::on_advert(addr, rssi, &bytes);
        }
    }
    // AD type 0x16 = Service Data, 16-bit UUID; 2C FE (LE) = Google Fast Pair.
    let sections = adv.GetSectionsByType(0x16)?;
    for i in 0..sections.Size()? {
        if let Some(bytes) = sections.GetAt(i).and_then(|s| s.Data()).ok().and_then(|b| buffer_bytes(&b)) {
            if bytes.len() > 2 && bytes[0] == 0x2C && bytes[1] == 0xFE {
                if let Some(b) = parse_fast_pair(&bytes[2..]) {
                    if let Ok(mut m) = fast_pair_map().lock() {
                        m.insert(addr, b);
                    }
                }
            }
        }
    }
    Ok(())
}

fn watcher() -> &'static Mutex<Option<BluetoothLEAdvertisementWatcher>> {
    static W: OnceLock<Mutex<Option<BluetoothLEAdvertisementWatcher>>> = OnceLock::new();
    W.get_or_init(|| Mutex::new(None))
}

/// Starts the watcher, or restarts it after the radio was switched off. Returns true when it was
/// just started, so the caller can give devices a moment (they broadcast every 1-2 s) to be heard.
pub fn ensure_watcher() -> bool {
    let Ok(mut guard) = watcher().lock() else { return false };
    if let Some(w) = guard.as_ref() {
        // A stop requested at the end of the previous read may still be in progress.
        for _ in 0..20 {
            if w.Status().ok() != Some(BluetoothLEAdvertisementWatcherStatus::Stopping) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let st = w.Status().unwrap_or(BluetoothLEAdvertisementWatcherStatus::Started);
        if st == BluetoothLEAdvertisementWatcherStatus::Stopped
            || st == BluetoothLEAdvertisementWatcherStatus::Aborted
            || st == BluetoothLEAdvertisementWatcherStatus::Created
        {
            return w.Start().is_ok();
        }
        return false;
    }
    let start = || -> windows::core::Result<BluetoothLEAdvertisementWatcher> {
        let w = BluetoothLEAdvertisementWatcher::new()?;
        w.SetScanningMode(BluetoothLEScanningMode::Passive)?;
        w.Received(&TypedEventHandler::new(
            |_, args: windows::core::Ref<BluetoothLEAdvertisementReceivedEventArgs>| {
                if let Some(a) = args.as_ref() {
                    let _ = on_received(a);
                }
                Ok(())
            },
        ))?;
        w.Start()?;
        Ok(w)
    };
    match start() {
        Ok(w) => {
            *guard = Some(w);
            true
        }
        Err(_) => false,
    }
}

/// Stops listening until the next `ensure_watcher` (called at the end of every read). Broadcasts
/// heard so far stay available; the next read starts the watcher again and waits for fresh ones.
pub fn stop_watcher() {
    let Ok(guard) = watcher().lock() else { return };
    if let Some(w) = guard.as_ref() {
        if w.Status().ok() == Some(BluetoothLEAdvertisementWatcherStatus::Started) {
            let _ = w.Stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_pair_battery_field() {
        // version 0, then a type-3 field of length 3: left 85, right 80 charging, case unknown.
        let b = parse_fast_pair(&[0x00, 0x33, 85, 0x80 | 80, 0x7F]).unwrap();
        assert_eq!((b.left, b.right, b.case), (85, 80, -1));
        assert!(!b.left_chg && b.right_chg);
        // An account-key field before the battery field is skipped.
        let b = parse_fast_pair(&[0x00, 0x20, 0xAA, 0xBB, 0x13, 40]).unwrap();
        assert_eq!(b.left, 40);
        // Discoverable-mode model id only: no battery.
        assert!(parse_fast_pair(&[0x12, 0x34, 0x56]).is_none());
    }
}
