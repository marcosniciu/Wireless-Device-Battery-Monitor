//! JBL Quantum 910 Wireless on its 2.4 GHz dongle 0ECB:2088.
//!
//! Feature reports read on the dongle's vendor collection (interface 5, FF13:0001), as mapped for
//! the Quantum 610 by jbl-quantum-linux (github.com/Tymon3310/jbl-quantum-linux,
//! docs/QUANTUM610_PACKETS.md) and confirmed on a Quantum 910 (49 5F = 95 %, matching the headset's
//! own 08 5F push):
//!
//!   GET_REPORT 68   reply [0] = 68, [1] = 1 headset on / 0 off or out of range
//!   GET_REPORT 49   reply [0] = 49, [1] = battery 0..100
//!
//! The firmware answers an id it does not implement with another report, so a reply counts only
//! when byte 0 is the requested id. Read-only: nothing is written to the dongle or headset.
use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::{HidColl, HidDev};

const VID: u16 = 0x0ECB;
const MODELS: [(u16, &str); 1] = [(0x2088, "JBL Quantum 910")];
const LINK: u8 = 0x68;
const BATTERY: u8 = 0x49;

/// Reply to 68: Some(headset on).
pub fn parse_link(r: &[u8]) -> Option<bool> {
    (r.len() >= 2 && r[0] == LINK && r[1] <= 1).then(|| r[1] == 1)
}

/// Reply to 49: Some(percent).
pub fn parse_battery(r: &[u8]) -> Option<i32> {
    (r.len() >= 2 && r[0] == BATTERY && r[1] <= 100).then(|| r[1] as i32)
}

/// Some(Some(%)) on, Some(None) dongle answered but headset off, None nothing usable.
fn read(c: &HidColl) -> Option<Option<i32>> {
    let d = HidDev::open(c).or_else(|| HidDev::open_feature_only(c))?;
    if d.get_feature(LINK).as_deref().and_then(parse_link) == Some(false) {
        return Some(None);
    }
    d.get_feature(BATTERY).as_deref().and_then(parse_battery).filter(|&p| p > 0).map(Some)
}

pub fn poll(ctx: &mut PollContext) {
    for (pid, name) in MODELS {
        if ctx.is_claimed(VID, pid) {
            continue;
        }
        let colls = ctx.find(VID, pid, |c| c.page() >= 0xFF00 && c.feat_len() >= 2);
        let Some(reading) = colls.iter().find_map(read) else { continue };
        let base = DeviceStatus {
            key: format!("jbl:{pid:04x}"),
            name: name.into(),
            kind: DeviceKind::Headset,
            via: "2.4 GHz dongle".into(),
            hw: Some((VID, pid)),
            grace_sec: 300,
            ..Default::default()
        };
        let s = match reading {
            Some(percent) => DeviceStatus { percent, ..base },
            None => DeviceStatus { connected: false, ..base },
        };
        ctx.add(s, VID, pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_recorded_on_hardware() {
        // Quantum 910 dongle, headset on, 95 %.
        assert_eq!(parse_link(&[0x68, 0x01, 0x01, 0x01, 0x02, 0x02, 0x01, 0x03]), Some(true));
        assert_eq!(parse_battery(&[0x49, 0x5F, 0x01, 0x01, 0x02, 0x02, 0x01, 0x03]), Some(95));
    }

    #[test]
    fn rejected() {
        assert_eq!(parse_link(&[0x68, 0x00]), Some(false));
        // Fallback report (another id) is not the battery.
        assert_eq!(parse_battery(&[0x08, 0x00, 0x01, 0x01]), None);
        assert_eq!(parse_battery(&[0x49, 0x65]), None); // 101
    }
}
