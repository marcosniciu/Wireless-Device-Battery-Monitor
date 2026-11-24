//! HyperX Cloud Flight Wireless headset over its 2.4 GHz USB dongle (VID 0951, PID 1723; the
//! dongle 16C4 of the first model speaks the same protocol).
//!
//! Protocol from hyperx-cloud-flight by kondinskis (MIT, github.com/kondinskis/hyperx-cloud-flight)
//! and hyperx-cloud-flight-wireless by srn (MIT, github.com/srn/hyperx-cloud-flight-wireless),
//! both reverse engineered from USB captures. The dongle's vendor collection (usage 0303, 20-byte
//! input and output reports; usage page FF90 on the 1723, FF53 on the 16C4) answers a status
//! request:
//!
//!   request   21 FF 05 00 ... (20 bytes, report ID 21)
//!   reply     .. .. .. ST VV ... (20 bytes)
//!             ST 0E / 0F   on battery: VV is a raw value, mapped to a level in steps of 5 %
//!             ST 10 / 11   on power: VV >= 20 still charging (no level), below that full
//!
//! The headset also pushes status reports on its own, and short reports for power, mute and
//! volume on other collections; only the status reply is used here. No decodable reply within the
//! timeout means the headset is switched off or out of range (the dongle stays enumerated). The
//! level is an estimate (5 % steps), and the raw value thresholds are the ones those projects use.
//!
//! The headset is asked at most once a minute while it answers, and again on every read while it
//! does not, so switching it on shows up quickly and switching it off within a minute.
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::HidDev;
use crate::util::{hex4, Deadline};

const VID: u16 = 0x0951;
const PIDS: [u16; 2] = [0x1723, 0x16C4];
const NAME: &str = "HyperX Cloud Flight Wireless";
/// A reply comes within milliseconds while the headset is on.
const REPLY_WAIT_MS: u64 = 1500;
const ASK_EVERY: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    /// On battery, 10..=100 in steps of 5.
    Level(i32),
    /// On power, no level reported.
    Charging,
    /// On power and full.
    Full,
}

pub fn request() -> [u8; 20] {
    let mut r = [0u8; 20];
    r[0] = 0x21;
    r[1] = 0xFF;
    r[2] = 0x05;
    r
}

/// Battery level for the raw value `v` reported in state `state` (0E: low half, 0F: high half).
pub fn level_from_raw(state: u8, v: u8) -> Option<i32> {
    let p = match state {
        0x0E => match v {
            0..=89 => 10,
            90..=119 => 15,
            120..=148 => 20,
            149..=159 => 25,
            160..=169 => 30,
            170..=179 => 35,
            180..=189 => 40,
            190..=199 => 45,
            200..=209 => 50,
            210..=219 => 55,
            220..=239 => 60,
            240..=255 => 65,
        },
        0x0F => match v {
            0..=19 => 70,
            20..=49 => 75,
            50..=69 => 80,
            70..=99 => 85,
            100..=119 => 90,
            120..=129 => 95,
            130..=255 => 100,
        },
        _ => return None,
    };
    Some(p)
}

/// Decodes one input report (20 bytes, report ID included).
pub fn decode(r: &[u8]) -> Option<Reading> {
    if r.len() < 5 {
        return None;
    }
    match r[3] {
        0x10 | 0x11 => Some(if r[4] >= 20 { Reading::Charging } else { Reading::Full }),
        state => level_from_raw(state, r[4]).map(Reading::Level),
    }
}

fn query(d: &HidDev) -> Option<Reading> {
    d.flush();
    if !d.write(&request()) {
        return None;
    }
    // Skip anything else the collection pushes until a status report shows up.
    let end = Deadline::in_ms(REPLY_WAIT_MS);
    loop {
        let left = end.left_ms();
        if left == 0 {
            return None;
        }
        let r = d.read(left)?;
        if let Some(reading) = decode(&r) {
            return Some(reading);
        }
    }
}

/// Last answer per dongle: when it came and what it said.
static LAST: Mutex<Option<HashMap<u16, (Instant, Reading)>>> = Mutex::new(None);

fn recent(pid: u16) -> Option<Reading> {
    let g = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let (at, reading) = g.as_ref()?.get(&pid)?;
    (at.elapsed() < ASK_EVERY).then_some(*reading)
}

fn remember(pid: u16, reading: Option<Reading>) {
    let mut g = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let map = g.get_or_insert_with(HashMap::new);
    match reading {
        Some(r) => {
            map.insert(pid, (Instant::now(), r));
        }
        None => {
            map.remove(&pid);
        }
    }
}

fn status(pid: u16, reading: Option<Reading>) -> DeviceStatus {
    let mut s = DeviceStatus {
        key: format!("hyperx:{}", hex4(pid)),
        name: NAME.into(),
        kind: DeviceKind::Headset,
        via: "2.4 GHz dongle".into(),
        hw: Some((VID, pid)),
        grace_sec: 60,
        ..Default::default()
    };
    match reading {
        Some(Reading::Level(n)) => s.percent = n,
        Some(Reading::Charging) => {
            s.charging = true;
            s.level_text = "Charging".into();
        }
        Some(Reading::Full) => {
            s.percent = 100;
            s.charging = true;
            s.full = true;
        }
        // Dongle present, headset off or out of range.
        None => s.connected = false,
    }
    s
}

pub fn poll(ctx: &mut PollContext) {
    for (vid, pid) in ctx.products(VID) {
        if !PIDS.contains(&pid) {
            continue;
        }
        let Some(c) = ctx
            .find(vid, pid, |c| c.page() >= 0xFF00 && c.usage() == 0x0303 && c.in_len() == 20 && c.out_len() == 20)
            .into_iter()
            .next()
        else {
            continue;
        };
        let reading = match recent(pid) {
            Some(r) => Some(r),
            None => {
                let Some(d) = HidDev::open(&c) else { continue };
                let r = query(&d);
                remember(pid, r);
                r
            }
        };
        ctx.add(status(pid, reading), vid, pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 20-byte status report with state `st` and raw value `v`.
    fn rep(st: u8, v: u8) -> Vec<u8> {
        let mut r = vec![0u8; 20];
        r[0] = 0x21;
        r[3] = st;
        r[4] = v;
        r
    }

    #[test]
    fn request_layout() {
        let r = request();
        assert_eq!(r.len(), 20);
        assert_eq!(&r[..4], &[0x21, 0xFF, 0x05, 0x00]);
        assert!(r[3..].iter().all(|&b| b == 0));
    }

    #[test]
    fn levels_on_battery() {
        assert_eq!(decode(&rep(0x0F, 130)), Some(Reading::Level(100)));
        assert_eq!(decode(&rep(0x0F, 0)), Some(Reading::Level(70)));
        assert_eq!(decode(&rep(0x0E, 255)), Some(Reading::Level(65)));
        assert_eq!(decode(&rep(0x0E, 200)), Some(Reading::Level(50)));
        assert_eq!(decode(&rep(0x0E, 0)), Some(Reading::Level(10)));
    }

    #[test]
    fn range_edges() {
        assert_eq!(level_from_raw(0x0E, 89), Some(10));
        assert_eq!(level_from_raw(0x0E, 90), Some(15));
        assert_eq!(level_from_raw(0x0E, 239), Some(60));
        assert_eq!(level_from_raw(0x0E, 240), Some(65));
        assert_eq!(level_from_raw(0x0F, 19), Some(70));
        assert_eq!(level_from_raw(0x0F, 20), Some(75));
        assert_eq!(level_from_raw(0x0F, 129), Some(95));
        assert_eq!(level_from_raw(0x0F, 130), Some(100));
    }

    #[test]
    fn levels_never_decrease_with_the_raw_value() {
        for st in [0x0E, 0x0F] {
            let mut last = 0;
            for v in 0..=255u8 {
                let p = level_from_raw(st, v).unwrap();
                assert!(p >= last, "state {st:02X} value {v}");
                last = p;
            }
        }
        // The whole 0E range sits below the 0F range.
        assert!(level_from_raw(0x0E, 255) < level_from_raw(0x0F, 0));
    }

    #[test]
    fn on_power() {
        assert_eq!(decode(&rep(0x10, 25)), Some(Reading::Charging));
        assert_eq!(decode(&rep(0x11, 20)), Some(Reading::Charging));
        assert_eq!(decode(&rep(0x10, 5)), Some(Reading::Full));
        assert_eq!(decode(&rep(0x11, 19)), Some(Reading::Full));
    }

    #[test]
    fn not_status_reports() {
        assert_eq!(decode(&rep(0x00, 100)), None);
        assert_eq!(decode(&rep(0x0D, 100)), None);
        assert_eq!(decode(&rep(0x12, 100)), None);
        assert_eq!(decode(&[0x21, 0xFF, 0x05, 0x0F]), None);
        assert_eq!(decode(&[]), None);
    }

    #[test]
    fn device_status() {
        let s = status(0x1723, Some(Reading::Level(85)));
        assert_eq!((s.percent, s.connected, s.charging), (85, true, false));
        assert_eq!(s.key, "hyperx:1723");
        assert_eq!(s.hw, Some((0x0951, 0x1723)));

        let s = status(0x1723, Some(Reading::Charging));
        assert!(s.is_charging());
        assert_eq!(s.percent, -1);
        assert_eq!(s.status_text(), "Charging ⚡");

        let s = status(0x16C4, Some(Reading::Full));
        assert!(s.is_full());
        assert_eq!(s.status_text(), "100% ✓");

        assert!(!status(0x1723, None).connected);
    }
}
