//! Keychron devices on the 1K "Keychron Link" 2.4 GHz receiver (3434:D030 Type-A, 3434:D031 Type-C).
//!
//! Facts as documented by the original C++ BatteryHub (GPL-3.0, src/drivers/keychron.cpp and
//! docs/keychron_keyboard_firmware/README.md), implemented here from them:
//!
//! * Mice (M1, M2, M3, M3 mini, M6, M7 ...), as read by Keychron's own desktop app and checked on a
//!   real M3 on D030: vendor collection on usage page 008C with feature report 51.
//!     SetFeature [51][06]  then GetFeature 51 ->  [51][06][01 = ok][00][vid LE][pid LE][..][..][..][level %][01 = charging]
//!   vid/pid are the mouse's own (3434:D0xx). Only replies naming a Keychron mouse are used.
//!
//! * Keyboards: the receiver's raw HID collection (FF60:0061, unnumbered 32-byte reports).
//!     B2  ->  [B2][slots] then per slot [vid LE][pid LE][connected]   (answered by the receiver)
//!     A4  ->  [A4][level %][mV LE][0 battery / 1 charging / 2 full][transport][model]
//!   A4 is the KC_GET_BATTERY command of ldkv/qmk_firmware#1. Keychron's stock keyboard firmware
//!   reports its battery only over Bluetooth and never answers A4 through the receiver; then the
//!   query times out and A4 is not asked again for KB_RETRY_AFTER (nothing is shown for it).
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::{HidColl, HidDev};
use crate::util::{hex4, sleep_ms, Deadline};

const VID: u16 = 0x3434;
const RECEIVERS: [u16; 2] = [0xD030, 0xD031];
const LEGACY_REPORT: u8 = 0x51;
const LEGACY_STATUS: u8 = 0x06;
const RAW_LEN: usize = 32;
const CMD_SLOTS: u8 = 0xB2;
const CMD_BATTERY: u8 = 0xA4;
/// Stock keyboard firmware never answers A4; don't spend a timeout on it every poll.
const KB_RETRY_AFTER: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, PartialEq)]
pub struct MouseReading {
    pub pid: u16,
    pub percent: u8,
    pub charging: bool,
}

/// Reply to feature report 51 / command 06 (report id first).
pub fn parse_mouse(r: &[u8]) -> Option<MouseReading> {
    if r.len() < 13 || r[0] != LEGACY_REPORT || r[1] != LEGACY_STATUS || r[2] != 0x01 {
        return None;
    }
    let vid = u16::from_le_bytes([r[4], r[5]]);
    let pid = u16::from_le_bytes([r[6], r[7]]);
    // Keychron mice report D0xx product ids; anything else (a keyboard) is not read this way.
    if vid != VID || pid & 0xFF00 != 0xD000 || r[11] > 100 {
        return None;
    }
    Some(MouseReading { pid, percent: r[11], charging: r[12] == 0x01 })
}

pub fn mouse_name(pid: u16) -> &'static str {
    match pid {
        0xD032 | 0xD033 => "Keychron M3",
        0xD035 => "Keychron M1",
        0xD036 | 0xD03E => "Keychron M3 mini",
        0xD03B => "Keychron M2",
        0xD03D => "Keychron M2 mini",
        0xD03F => "Keychron M6",
        0xD044 => "Keychron M7",
        _ => "Keychron mouse",
    }
}

/// Connected Keychron product id from the receiver's B2 reply (report id 0 stripped).
pub fn parse_slots(r: &[u8]) -> Option<u16> {
    if r.len() < 2 || r[0] != CMD_SLOTS {
        return None;
    }
    (0..(r[1] as usize).min(5)).find_map(|i| {
        let s = r.get(2 + 5 * i..2 + 5 * i + 5)?;
        (u16::from_le_bytes([s[0], s[1]]) == VID && s[4] != 0).then(|| u16::from_le_bytes([s[2], s[3]]))
    })
}

#[derive(Debug, PartialEq)]
pub struct KeyboardReading {
    pub percent: u8,
    pub millivolts: i32,
    pub charging: bool,
    pub full: bool,
}

/// Reply to A4 (report id 0 stripped).
pub fn parse_keyboard(r: &[u8]) -> Option<KeyboardReading> {
    if r.len() < 5 || r[0] != CMD_BATTERY || r[1] > 100 {
        return None;
    }
    Some(KeyboardReading { percent: r[1], millivolts: u16::from_le_bytes([r[2], r[3]]) as i32, charging: r[4] == 1, full: r[4] == 2 })
}

fn raw_query(d: &HidDev, cmd: u8, timeout_ms: u64) -> Option<Vec<u8>> {
    let mut req = [0u8; RAW_LEN + 1]; // report id 0 + 32 bytes
    req[1] = cmd;
    d.flush();
    if !d.write(&req) {
        return None;
    }
    let end = Deadline::in_ms(timeout_ms);
    while !end.passed() {
        let r = d.read(end.left_ms())?;
        if r.first() == Some(&cmd) {
            return Some(r);
        }
    }
    None
}

fn base(key: String, name: &str, kind: DeviceKind, pid: u16) -> DeviceStatus {
    DeviceStatus { key, name: name.into(), kind, via: "2.4 GHz receiver".into(), hw: Some((VID, pid)), grace_sec: 1800, ..Default::default() }
}

fn read_mouse(c: &HidColl) -> Option<MouseReading> {
    let d = HidDev::open(c).or_else(|| HidDev::open_feature_only(c))?;
    if !d.set_feature(&[LEGACY_REPORT, LEGACY_STATUS]) {
        return None;
    }
    for _ in 0..5 {
        sleep_ms(40); // the reply is usually there at once; give the mouse up to ~200 ms
        if let Some(r) = d.get_feature(LEGACY_REPORT).filter(|r| r.len() >= 13 && r[1] == LEGACY_STATUS) {
            return parse_mouse(&r);
        }
    }
    None
}

struct KbState {
    retry: HashMap<u16, Instant>,
    /// Keyboards that answered A4 once (patched firmware): shown as off when they disconnect.
    answered: HashSet<u16>,
}

static KB: Mutex<Option<KbState>> = Mutex::new(None);

fn poll_keyboard(ctx: &mut PollContext, rpid: u16) {
    let colls = ctx.find(VID, rpid, |c| c.page() == 0xFF60 && c.usage() == 0x0061 && c.out_len() as usize == RAW_LEN + 1);
    let Some(d) = colls.iter().find_map(HidDev::open) else { return };
    let Some(slots) = raw_query(&d, CMD_SLOTS, 300) else { return };
    let mut guard = KB.lock().unwrap_or_else(|e| e.into_inner());
    let st = guard.get_or_insert_with(|| KbState { retry: HashMap::new(), answered: HashSet::new() });
    let key = |pid: u16| format!("keychron-kb:{}", hex4(pid));
    let Some(kb) = parse_slots(&slots) else {
        // Keyboard off, asleep or on its cable.
        let off: Vec<u16> = st.answered.iter().copied().collect();
        drop(guard);
        for pid in off {
            ctx.add(DeviceStatus { connected: false, ..base(key(pid), "Keychron keyboard", DeviceKind::Keyboard, rpid) }, VID, rpid);
        }
        return;
    };
    if st.retry.get(&kb).is_some_and(|t| Instant::now() < *t) {
        return;
    }
    // After the link has been idle the first request can be lost while it wakes up.
    let Some(b) = raw_query(&d, CMD_BATTERY, 400).or_else(|| raw_query(&d, CMD_BATTERY, 400)).and_then(|r| parse_keyboard(&r)) else {
        st.retry.insert(kb, Instant::now() + KB_RETRY_AFTER); // stock firmware
        return;
    };
    st.retry.remove(&kb);
    st.answered.insert(kb);
    drop(guard);
    let name = if (0x0A50..=0x0A58).contains(&kb) { "Keychron K5 Max" } else { "Keychron keyboard" };
    let s = DeviceStatus {
        percent: b.percent as i32,
        millivolts: b.millivolts,
        charging: b.charging,
        full: b.full,
        ..base(key(kb), name, DeviceKind::Keyboard, rpid)
    };
    ctx.add(s, VID, rpid);
}

pub fn poll(ctx: &mut PollContext) {
    for rpid in RECEIVERS {
        if ctx.is_claimed(VID, rpid) {
            continue;
        }
        let colls = ctx.find(VID, rpid, |c| c.page() == 0x008C && c.feat_len() >= 16);
        if let Some(m) = colls.iter().find_map(read_mouse) {
            let s = DeviceStatus {
                percent: m.percent as i32,
                charging: m.charging,
                ..base(format!("keychron:{}", hex4(m.pid)), mouse_name(m.pid), DeviceKind::Mouse, rpid)
            };
            ctx.add(s, VID, rpid);
            continue;
        }
        poll_keyboard(ctx, rpid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mouse_reply(vid: u16, pid: u16, level: u8, chg: u8) -> Vec<u8> {
        let mut r = vec![0u8; 1001];
        r[..4].copy_from_slice(&[0x51, 0x06, 0x01, 0x00]);
        r[4..6].copy_from_slice(&vid.to_le_bytes());
        r[6..8].copy_from_slice(&pid.to_le_bytes());
        r[11] = level;
        r[12] = chg;
        r
    }

    #[test]
    fn mouse_replies() {
        assert_eq!(parse_mouse(&mouse_reply(0x3434, 0xD033, 76, 0)), Some(MouseReading { pid: 0xD033, percent: 76, charging: false }));
        assert_eq!(parse_mouse(&mouse_reply(0x3434, 0xD03F, 40, 1)).map(|m| m.charging), Some(true));
        assert_eq!(parse_mouse(&mouse_reply(0x3434, 0x0A50, 76, 0)), None); // a keyboard
        assert_eq!(parse_mouse(&mouse_reply(0x1234, 0xD033, 76, 0)), None);
        assert_eq!(parse_mouse(&mouse_reply(0x3434, 0xD033, 101, 0)), None);
        let mut r = mouse_reply(0x3434, 0xD033, 76, 0);
        r[2] = 0x00; // not ok
        assert_eq!(parse_mouse(&r), None);
        assert_eq!(mouse_name(0xD033), "Keychron M3");
    }

    #[test]
    fn slots() {
        let mut r = vec![0u8; 32];
        r[..2].copy_from_slice(&[0xB2, 0x02]);
        r[2..7].copy_from_slice(&[0x34, 0x34, 0x50, 0x0A, 0x00]); // paired, not connected
        r[7..12].copy_from_slice(&[0x34, 0x34, 0x51, 0x0A, 0x01]);
        assert_eq!(parse_slots(&r), Some(0x0A51));
        r[11] = 0;
        assert_eq!(parse_slots(&r), None);
        assert_eq!(parse_slots(&[0xFF, 0x01]), None);
    }

    #[test]
    fn keyboard_replies() {
        let r = [0xA4, 52, 0x9F, 0x0E, 0x00, 0x02, 0x00];
        assert_eq!(parse_keyboard(&r), Some(KeyboardReading { percent: 52, millivolts: 3743, charging: false, full: false }));
        assert_eq!(parse_keyboard(&[0xA4, 100, 0x68, 0x10, 0x02]).map(|k| k.full), Some(true));
        assert_eq!(parse_keyboard(&[0xFF, 52, 0, 0, 0]), None); // VIA "unhandled"
        assert_eq!(parse_keyboard(&[0xA4, 120, 0, 0, 0]), None);
    }
}
