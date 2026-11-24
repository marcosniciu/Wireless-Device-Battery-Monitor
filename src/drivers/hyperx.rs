//! HyperX Cloud III S Wireless headset over its 2.4 GHz dongle (VID 03F0, PID 06BE or 02CC).
//!
//! Protocol as documented by HyperX-Battery-Tray by GuacamoleGH (MIT,
//! github.com/GuacamoleGH/HyperX-Battery-Tray), hyperx-battery-logger by OZX-OG (MIT,
//! github.com/OZX-OG/hyperx-battery-logger) and HyperHeadset by Lennard Kittner (MIT,
//! github.com/LennardKittner/HyperHeadset); implemented here from those facts. The dongle's vendor
//! collection (usage page 01C0, usage 0001, 64-byte input / output report 0C) answers a battery
//! query written as an output report:
//!
//!   request  0C 02 03 01 00 06 | 00 ... (64 bytes)
//!   reply    0C 02 03 01 00 06 NN VV VV ...
//!            NN  battery level 0..100 % (above 100, e.g. FF: headset off)
//!            VV  battery voltage in mV, big-endian
//!
//! The dongle also pushes events `0D 02 03 00 EE VV` (EE 0A = charging cable, VV 01 plugged /
//! 00 unplugged; EE 0C = headset, 01 on / 00 off); one seen while waiting for the reply is used.
//! Charging cannot be queried. No reply means the headset is off (the dongle stays enumerated).
use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::{HidColl, HidDev};
use crate::util::{hex4, Deadline};

const VID: u16 = 0x03F0;
const HEADER: [u8; 5] = [0x0C, 0x02, 0x03, 0x01, 0x00];
const CMD_BATTERY: u8 = 0x06;
const EVENT: [u8; 4] = [0x0D, 0x02, 0x03, 0x00];
const EV_CHARGER: u8 = 0x0A;
const EV_HEADSET: u8 = 0x0C;
const WAIT_MS: u64 = 1500;

fn model(pid: u16) -> Option<&'static str> {
    match pid {
        0x06BE | 0x02CC => Some("HyperX Cloud III S Wireless"),
        _ => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    /// Battery reply: level (None = headset off) and voltage in mV (0 if not given).
    Battery(Option<i32>, i32),
    Charger(bool),
    Headset(bool),
}

/// Decodes one input report (report ID included).
pub fn decode(r: &[u8]) -> Option<Frame> {
    if r.len() >= 7 && r[..5] == HEADER && r[5] == CMD_BATTERY {
        let level = (r[6] <= 100).then_some(r[6] as i32);
        let mv = if r.len() >= 9 && level.is_some() { ((r[7] as i32) << 8) | r[8] as i32 } else { 0 };
        return Some(Frame::Battery(level, mv));
    }
    if r.len() >= 6 && r[..4] == EVENT && r[5] <= 1 {
        return match r[4] {
            EV_CHARGER => Some(Frame::Charger(r[5] == 1)),
            EV_HEADSET => Some(Frame::Headset(r[5] == 1)),
            _ => None,
        };
    }
    None
}

struct Reading {
    level: Option<i32>,
    mv: i32,
    charging: bool,
}

fn query(c: &HidColl) -> Option<Reading> {
    let d = HidDev::open(c)?;
    d.flush();
    let mut req = [0u8; 64];
    req[..5].copy_from_slice(&HEADER);
    req[5] = CMD_BATTERY;
    if !d.write(&req) {
        return None;
    }
    let end = Deadline::in_ms(WAIT_MS);
    let mut charging = false;
    while !end.passed() {
        let Some(f) = d.read(end.left_ms()).and_then(|r| decode(&r)) else { continue };
        match f {
            Frame::Battery(level, mv) => return Some(Reading { level, mv, charging }),
            Frame::Charger(on) => charging = on,
            Frame::Headset(false) => return Some(Reading { level: None, mv: 0, charging: false }),
            Frame::Headset(true) => {}
        }
    }
    None
}

pub fn poll(ctx: &mut PollContext) {
    for (vid, pid) in ctx.products(VID) {
        let Some(name) = model(pid) else { continue };
        let colls = ctx.find(vid, pid, |c| c.page() == 0x01C0 && c.usage() == 0x0001 && c.out_len() >= 64 && c.in_len() >= 7);
        if colls.is_empty() {
            continue;
        }
        let found = colls.iter().find_map(query);
        let base = DeviceStatus {
            key: format!("hyperx:{}", hex4(pid)),
            name: name.into(),
            kind: DeviceKind::Headset,
            via: "2.4 GHz dongle".into(),
            hw: Some((vid, pid)),
            grace_sec: 60,
            ..Default::default()
        };
        let s = match found {
            Some(Reading { level: Some(level), mv, charging }) => DeviceStatus { percent: level, millivolts: mv, charging, ..base },
            _ => DeviceStatus { connected: false, ..base },
        };
        ctx.add(s, vid, pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(head: &[u8]) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[..head.len()].copy_from_slice(head);
        r
    }

    #[test]
    fn battery_reply() {
        // HyperX-Battery-Tray's test frame: 71 %, 3904 mV.
        assert_eq!(decode(&report(&[0x0C, 0x02, 0x03, 0x01, 0x00, 0x06, 0x47, 0x0F, 0x40])), Some(Frame::Battery(Some(71), 3904)));
        // hyperx-battery-logger's example: 90 %, 4172 mV.
        assert_eq!(decode(&report(&[0x0C, 0x02, 0x03, 0x01, 0x00, 0x06, 0x5A, 0x10, 0x4C])), Some(Frame::Battery(Some(90), 4172)));
    }

    #[test]
    fn headset_off_reply() {
        assert_eq!(decode(&report(&[0x0C, 0x02, 0x03, 0x01, 0x00, 0x06, 0xFF])), Some(Frame::Battery(None, 0)));
    }

    #[test]
    fn events() {
        assert_eq!(decode(&report(&[0x0D, 0x02, 0x03, 0x00, 0x0A, 0x01])), Some(Frame::Charger(true)));
        assert_eq!(decode(&report(&[0x0D, 0x02, 0x03, 0x00, 0x0C, 0x00])), Some(Frame::Headset(false)));
        assert_eq!(decode(&report(&[0x0D, 0x02, 0x03, 0x00, 0x03, 0x01])), None); // mic mute
    }

    #[test]
    fn not_battery() {
        // Connection-status reply, mic reply, other report IDs, too short.
        assert_eq!(decode(&report(&[0x0C, 0x02, 0x03, 0x01, 0x00, 0x02, 0x02])), None);
        assert_eq!(decode(&report(&[0x0C, 0x02, 0x03, 0x01, 0x00, 0x04, 0x01])), None);
        assert_eq!(decode(&report(&[0x05, 0x02])), None);
        assert_eq!(decode(&[0x0C, 0x02, 0x03, 0x01, 0x00, 0x06]), None);
    }
}
