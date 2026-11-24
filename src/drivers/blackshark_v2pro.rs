//! Razer BlackShark V2 Pro (2023) headset over its 2.4 GHz HyperSpeed dongle (VID 1532, PID 0555).
//!
//! Protocol as documented by OpenRazer PR #2862 (GPL-2.0-or-later, driver/razerblackshark_driver.c,
//! from a capture of this dongle), blackshark-battery by Matej Basic (MIT,
//! github.com/matej-basic/blackshark-battery, README "Protocol" with an example reply) and the
//! original C++ BatteryHub (GPL-3.0, src/drivers/razer_headset.cpp); implemented here from those
//! facts. The dongle does not speak Razer's 90-byte feature protocol: its vendor collection (usage
//! page FF00, 64-byte input / output report 02) carries "PA" frames:
//!
//!   remote on   02 80 07 00 00 50 41 0E 00 02 E1 01 | 00 ...
//!   remote off  02 80 07 00 00 50 41 0E 00 02 E1 00
//!   query       02 80 08 00 00 50 41 08 00 03 CC 00      CC 21 = battery, 2A = charging
//!   reply       02 0E 50 49 .. .. .. .. .. .. .. .. CC 01 NN VV ...
//!               [12] command echo  [13] 01 = ACK  [14] payload length  [15] value
//!               battery 0..100; charging 00 on battery, non-zero on the cable
//!
//! Each query is wrapped like Synapse does it: one extra "remote on" first (the first frame after
//! ~0.3 s idle only wakes the radio link), then up to three times "remote on" + query with a
//! 150 ms reply window, and always "remote off" at the end. The dongle also sends status frames
//! (02 0D 50 49 ...) that are not replies. No reply means the headset is off, out of range or in
//! Bluetooth mode. BatteryHub re-reads headsets every few seconds; the headset is only asked
//! every QUERY_EVERY (OFF_QUERY_EVERY while it does not answer), the last reading is kept between.
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::{HidColl, HidDev};
use crate::util::{hex4, sleep_ms, Deadline};

const VID: u16 = 0x1532;
const DONGLE: u16 = 0x0555;
pub const CMD_BATTERY: u8 = 0x21;
pub const CMD_CHARGING: u8 = 0x2A;
const ATTEMPTS: usize = 3;
const REPLY_MS: u64 = 150;
const GAP_MS: u64 = 35;
/// blackshark-battery reads every 5 minutes (2 while charging).
const QUERY_EVERY: Duration = Duration::from_secs(5 * 60);
const QUERY_EVERY_CHARGING: Duration = Duration::from_secs(2 * 60);
const OFF_QUERY_EVERY: Duration = Duration::from_secs(30);

pub fn remote_frame(on: bool) -> [u8; 64] {
    let mut f = [0u8; 64];
    f[..12].copy_from_slice(&[0x02, 0x80, 0x07, 0x00, 0x00, 0x50, 0x41, 0x0E, 0x00, 0x02, 0xE1, on as u8]);
    f
}

pub fn query_frame(cmd: u8) -> [u8; 64] {
    let mut f = [0u8; 64];
    f[..12].copy_from_slice(&[0x02, 0x80, 0x08, 0x00, 0x00, 0x50, 0x41, 0x08, 0x00, 0x03, cmd, 0x00]);
    f
}

/// The value of a reply to `cmd` (report ID included), or None if `r` is not one.
pub fn parse(r: &[u8], cmd: u8) -> Option<u8> {
    (r.len() >= 16 && r[0] == 0x02 && r[12] == cmd && r[13] == 0x01).then(|| r[15])
}

fn query(d: &HidDev, cmd: u8) -> Option<u8> {
    d.flush();
    d.write(&remote_frame(true)); // wakes a dozing link; may be dropped
    sleep_ms(GAP_MS);
    let mut value = None;
    for _ in 0..ATTEMPTS {
        d.write(&remote_frame(true));
        sleep_ms(GAP_MS);
        if !d.write(&query_frame(cmd)) {
            break;
        }
        let end = Deadline::in_ms(REPLY_MS);
        while value.is_none() && !end.passed() {
            value = d.read(end.left_ms()).and_then(|r| parse(&r, cmd));
        }
        if value.is_some() {
            break;
        }
    }
    d.write(&remote_frame(false));
    sleep_ms(GAP_MS);
    value
}

#[derive(Clone, Copy)]
struct Last {
    at: Instant,
    /// None: no reply (headset off).
    reading: Option<(i32, bool)>,
}

static LAST: Mutex<Option<Last>> = Mutex::new(None);

fn read(c: &HidColl) -> Option<Option<(i32, bool)>> {
    let d = HidDev::open(c)?;
    let Some(level) = query(&d, CMD_BATTERY) else { return Some(None) };
    if level > 100 {
        return Some(None);
    }
    let charging = query(&d, CMD_CHARGING).is_some_and(|v| v != 0);
    Some(Some((level as i32, charging)))
}

pub fn poll(ctx: &mut PollContext) {
    if ctx.is_claimed(VID, DONGLE) {
        return;
    }
    let colls = ctx.find(VID, DONGLE, |c| c.page() == 0xFF00 && c.in_len() >= 16 && c.out_len() >= 64);
    if colls.is_empty() {
        *LAST.lock().unwrap_or_else(|e| e.into_inner()) = None; // unplugged: ask at once next time
        return;
    }
    let cached = *LAST.lock().unwrap_or_else(|e| e.into_inner());
    let fresh = cached.filter(|l| {
        let every = match l.reading {
            Some((_, true)) => QUERY_EVERY_CHARGING,
            Some(_) => QUERY_EVERY,
            None => OFF_QUERY_EVERY,
        };
        l.at.elapsed() < every
    });
    let reading = match fresh {
        Some(l) => l.reading,
        None => {
            let Some(r) = colls.iter().find_map(read) else { return };
            *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some(Last { at: Instant::now(), reading: r });
            r
        }
    };
    let base = DeviceStatus {
        key: format!("razer:{}", hex4(DONGLE)),
        name: "Razer BlackShark V2 Pro".into(),
        kind: DeviceKind::Headset,
        via: "2.4 GHz dongle".into(),
        hw: Some((VID, DONGLE)),
        grace_sec: 60,
        ..Default::default()
    };
    let s = match reading {
        Some((level, charging)) => DeviceStatus { percent: level, charging, ..base },
        None => DeviceStatus { connected: false, ..base },
    };
    ctx.add(s, VID, DONGLE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames() {
        // blackshark-battery README "Protocol" table.
        assert_eq!(&remote_frame(true)[..12], &[0x02, 0x80, 0x07, 0x00, 0x00, 0x50, 0x41, 0x0E, 0x00, 0x02, 0xE1, 0x01]);
        assert_eq!(remote_frame(false)[11], 0x00);
        assert_eq!(&query_frame(CMD_BATTERY)[..12], &[0x02, 0x80, 0x08, 0x00, 0x00, 0x50, 0x41, 0x08, 0x00, 0x03, 0x21, 0x00]);
        assert_eq!(query_frame(CMD_CHARGING)[10], 0x2A);
        assert!(query_frame(CMD_BATTERY)[12..].iter().all(|&b| b == 0));
    }

    fn report(head: &[u8]) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[..head.len()].copy_from_slice(head);
        r
    }

    #[test]
    fn replies() {
        // blackshark-battery README example: 0x4B = 75 %.
        let bat = report(&[0x02, 0x0E, 0x50, 0x49, 0x08, 0xCE, 0x2D, 0x83, 0x03, 0x00, 0x04, 0x00, 0x21, 0x01, 0x01, 0x4B]);
        assert_eq!(parse(&bat, CMD_BATTERY), Some(75));
        assert_eq!(parse(&bat, CMD_CHARGING), None);
        let chg = report(&[0x02, 0x0E, 0x50, 0x49, 0, 0, 0, 0, 0, 0, 0, 0, 0x2A, 0x01, 0x01, 0x02]);
        assert_eq!(parse(&chg, CMD_CHARGING), Some(2));
    }

    #[test]
    fn not_replies() {
        // Status frame, a NAK, an echo of the request and a short report.
        assert_eq!(parse(&report(&[0x02, 0x0D, 0x50, 0x49, 0x01, 0xC0]), CMD_BATTERY), None);
        let mut nak = report(&[0x02, 0x0E, 0x50, 0x49]);
        nak[12] = 0x21;
        assert_eq!(parse(&nak, CMD_BATTERY), None);
        assert_eq!(parse(&query_frame(CMD_BATTERY), CMD_BATTERY), None);
        assert_eq!(parse(&[0x02, 0x0E, 0x50], CMD_BATTERY), None);
    }
}
