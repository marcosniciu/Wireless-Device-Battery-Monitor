//! FNATIC BOLT wireless receiver (2F0E:0203) and USB cable (2F0E:0204).
//!
//! USBPcap evidence: a read-only 0x0C query gives the battery percentage over the
//! 2.4 GHz receiver. The wired 0x0C response can report a bogus 100% even after
//! the mouse has been connected for a while. Never use wired battery readings.
//!
//! USB presence selects the displayed connection (one logical device), while
//! only the receiver supplies battery readings. If the receiver stops responding
//! while plugged in, briefly retain the last wireless reading, clearly marked
//! as cached. A missing or old reading remains unknown rather than fabricated.
//!
//! Charging is inferred from a connected USB cable (and a non-full/unknown
//! battery level), NOT from a confirmed hardware charging flag.
use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::HidDev;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const VID: u16 = 0x2F0E;
const RECEIVER: u16 = 0x0203;
const CABLE: u16 = 0x0204;
const BATTERY_COMMAND: u8 = 0x0C;
const TIMEOUT_MS: u32 = 1200;
// Limit how long a previously observed wireless percentage may be displayed.
const CACHE_MAX_AGE: Duration = Duration::from_secs(15 * 60);

#[derive(Default)]
struct BatteryCache {
    wireless: Option<(i32, Instant)>,
}

impl BatteryCache {
    /// Returns the fresh receiver reading, or a recent receiver reading.
    /// Never records a wired reply, which is known to be unreliable.
    fn select(&mut self, wireless: Option<i32>, now: Instant) -> (Option<i32>, bool) {
        if let Some(p @ 0..=100) = wireless {
            self.wireless = Some((p, now));
            return (Some(p), false);
        }
        match self.wireless {
            Some((p, at)) if now.saturating_duration_since(at) <= CACHE_MAX_AGE => (Some(p), true),
            _ => {
                self.wireless = None;
                (None, false)
            }
        }
    }
}

static CACHE: OnceLock<Mutex<BatteryCache>> = OnceLock::new();
fn cache() -> &'static Mutex<BatteryCache> {
    CACHE.get_or_init(|| Mutex::new(BatteryCache::default()))
}

/// Input report payload after HidDev strips a possible zero report ID.
pub fn decode(payload: &[u8]) -> Option<i32> {
    if payload.len() == 64 && payload[0] == BATTERY_COMMAND && payload[1] <= 100 {
        Some(payload[1] as i32)
    } else {
        None
    }
}

fn query(d: &HidDev) -> Option<i32> {
    d.flush();
    if !d.write(&[0x00, BATTERY_COMMAND]) {
        return None;
    }
    let deadline = crate::util::Deadline::in_ms(TIMEOUT_MS as u64);
    loop {
        let left = deadline.left_ms();
        if left == 0 {
            return None;
        }
        let data = d.read(left)?;
        if let Some(percent) = decode(&data) {
            return Some(percent);
        }
    }
}

fn read_wireless(ctx: &PollContext) -> Option<i32> {
    // Interface 1: vendor-defined HID collection, 65 byte input/output.
    let colls = ctx.find(VID, RECEIVER, |c| {
        c.iface == 1 && c.page() == 0x00FF && c.usage() == 0x0001
            && c.in_len() == 65 && c.out_len() == 65
    });
    colls.iter().find_map(|c| HidDev::open(c).and_then(|d| query(&d)))
}

pub fn poll(ctx: &mut PollContext) {
    let cable_present = !ctx.find(VID, CABLE, |_| true).is_empty();
    let dongle_present = !ctx.find(VID, RECEIVER, |_| true).is_empty();
    if !cable_present && !dongle_present {
        return;
    }
    if (cable_present && ctx.is_claimed(VID, CABLE))
        || (dongle_present && ctx.is_claimed(VID, RECEIVER)) {
        return;
    }

    // The 0204 cable may report 100% incorrectly indefinitely. Do not query it.
    let wireless = if dongle_present { read_wireless(ctx) } else { None };
    let (percent, cached) = cache().lock().unwrap_or_else(|e| e.into_inner())
        .select(wireless, Instant::now());

    let selected_pid = if cable_present { CABLE } else { RECEIVER };
    let mut status = DeviceStatus {
        key: "fnatic:bolt".into(),
        name: "FNATIC BOLT".into(),
        kind: DeviceKind::Mouse,
        via: if cable_present { "USB cable" } else { "2.4 GHz receiver" }.into(),
        hw: Some((VID, selected_pid)),
        grace_sec: 60,
        ..Default::default()
    };
    if let Some(p) = percent { status.percent = p; }
    // USB presence is known; the charge flag itself is not known.
    status.charging = cable_present && percent != Some(100);
    if cable_present {
        status.detail = if cached {
            "USB cable attached; battery last read from receiver (cached). Charging inferred".into()
        } else if percent.is_none() {
            "USB cable attached; battery unknown. Charging inferred".into()
        } else {
            "USB cable attached; battery read from receiver. Charging inferred".into()
        };
    } else if cached {
        status.detail = "Last known receiver battery level (cached)".into();
    }
    // A plugged-in USB cable is still connected even if battery is unknown.
    if !cable_present && percent.is_none() {
        status.connected = false;
    }
    ctx.add(status, VID, selected_pid);
    if cable_present { ctx.claim(VID, CABLE); }
    if dongle_present { ctx.claim(VID, RECEIVER); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_wireless_response() {
        for (b, expected) in [(0x2C, 44), (0x41, 65), (0x3F, 63), (0x64, 100)] {
            let mut report = [0u8; 64];
            report[0] = BATTERY_COMMAND;
            report[1] = b;
            assert_eq!(decode(&report), Some(expected));
        }
    }

    #[test]
    fn malformed_reports_rejected() {
        assert_eq!(decode(&[BATTERY_COMMAND, 44]), None);
        let mut report = [0u8; 64];
        report[0] = 0x0D;
        report[1] = 44;
        assert_eq!(decode(&report), None);
        report[0] = BATTERY_COMMAND;
        report[1] = 101;
        assert_eq!(decode(&report), None);
    }

    #[test]
    fn cached_receiver_level_expires() {
        let t = Instant::now();
        let mut state = BatteryCache::default();
        assert_eq!(state.select(None, t), (None, false));
        assert_eq!(state.select(Some(65), t), (Some(65), false));
        assert_eq!(state.select(None, t + Duration::from_secs(60)), (Some(65), true));
        assert_eq!(state.select(None, t + CACHE_MAX_AGE + Duration::from_secs(1)), (None, false));
    }

    #[test]
    fn fresh_receiver_reading_replaces_cache() {
        let t = Instant::now();
        let mut state = BatteryCache::default();
        state.select(Some(65), t);
        assert_eq!(state.select(Some(64), t + Duration::from_secs(30)), (Some(64), false));
        assert_eq!(state.select(None, t + Duration::from_secs(40)), (Some(64), true));
    }

    #[test]
    fn genuine_receiver_100_is_allowed() {
        let mut state = BatteryCache::default();
        assert_eq!(state.select(Some(100), Instant::now()), (Some(100), false));
    }
}
