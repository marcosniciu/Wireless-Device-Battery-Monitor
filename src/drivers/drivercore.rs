//! Mice on the shared "DriverCore" vendor id 373E (LAMZU, Arbiter Studio and unbranded mice such as
//! the "R6 Mouse 2.4G" 373E:0022), on their 2.4 GHz receivers or USB cables.
//!
//! Protocol as documented by the original C++ BatteryHub (GPL-3.0, src/drivers/arbiter.cpp and
//! lamzu.cpp, from the Arbiter Web HUB and LAMZU Aurora web drivers' "IsNewProtocol" getBatPer()),
//! the same read-only status query HaloBattery (MIT, providers/wlmouse.py) documents for WLMOUSE
//! (wlmouse.rs); implemented here from those facts. Vendor collection FFFF (65-byte feature report 0):
//!
//!   SetFeature  00 | 00 00 02 02 00 83 | 00 ...          (status query)
//!   GetFeature  [00] SS .. .. 02 .. 83 CC NN ...         SS = A1 / A2 answered, A0 = mouse asleep
//!                                                        CC = 1 on the cable (charging), NN = level %
//!
//! A level of 0 from a receiver means the mouse is off or not linked. Nothing else is written.
use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::{self, HidColl, HidDev};
use crate::util::{hex4, sleep_ms};

const VID: u16 = 0x373E;
const QUERY: [u8; 7] = [0x00, 0x00, 0x00, 0x02, 0x02, 0x00, 0x83];

#[derive(Debug, PartialEq)]
pub enum Reply {
    Battery { percent: i32, charging: bool },
    /// The receiver answered but the mouse sleeps, is off or not linked.
    Offline,
}

/// Decodes a feature report, with or without the leading report id byte.
pub fn decode(r: &[u8]) -> Option<Reply> {
    let (st, chg, lvl) = if r.len() >= 9 && r[4] == 0x02 && r[6] == 0x83 && r[0] == 0x00 {
        (r[1], r[7], r[8])
    } else if r.len() >= 8 && r[3] == 0x02 && r[5] == 0x83 {
        (r[0], r[6], r[7])
    } else {
        return None;
    };
    match st {
        0xA0 => Some(Reply::Offline),
        0xA1 | 0xA2 if lvl == 0 && chg == 0 => Some(Reply::Offline),
        0xA1 | 0xA2 if lvl <= 100 => Some(Reply::Battery { percent: lvl as i32, charging: chg == 0x01 }),
        _ => None,
    }
}

fn query(c: &HidColl) -> Option<Reply> {
    let d = HidDev::open_feature_only(c).or_else(|| HidDev::open(c))?;
    if !d.set_feature(&QUERY) {
        return None;
    }
    // The receiver fetches the answer from the mouse; poll for it for up to ~0.8 s.
    for _ in 0..16 {
        sleep_ms(50);
        if let Some(v) = d.get_feature(0).and_then(|r| decode(&r)) {
            return Some(v);
        }
    }
    None
}

pub fn poll(ctx: &mut PollContext) {
    for (vid, pid) in ctx.products(VID) {
        let colls = ctx.find(vid, pid, |c| c.page() == 0xFFFF && c.feat_len() == 65);
        if colls.is_empty() {
            continue;
        }
        let product = crate::report::clean_name(&hid::product_string(&colls[0]));
        let name = if product.is_empty() || product == "?" { format!("Mouse {}", hex4(pid)) } else { product };
        let base = DeviceStatus {
            key: format!("drivercore:{}", hex4(pid)),
            name,
            kind: DeviceKind::Mouse,
            via: "2.4 GHz receiver".into(),
            hw: Some((vid, pid)),
            grace_sec: 1800,
            ..Default::default()
        };
        let s = match colls.iter().find_map(query) {
            Some(Reply::Battery { percent, charging }) => DeviceStatus { percent, charging, ..base },
            Some(Reply::Offline) | None => DeviceStatus { connected: false, ..base },
        };
        ctx.add(s, vid, pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(lead: bool, st: u8, chg: u8, lvl: u8) -> Vec<u8> {
        let mut r = vec![0u8; 65];
        let o = lead as usize;
        r[o..o + 8].copy_from_slice(&[st, 0x00, 0x00, 0x02, 0x02, 0x83, chg, lvl]);
        r
    }

    #[test]
    fn battery() {
        assert_eq!(decode(&reply(true, 0xA1, 0, 77)), Some(Reply::Battery { percent: 77, charging: false }));
        assert_eq!(decode(&reply(false, 0xA2, 1, 50)), Some(Reply::Battery { percent: 50, charging: true }));
    }

    #[test]
    fn offline() {
        assert_eq!(decode(&reply(true, 0xA0, 0, 0)), Some(Reply::Offline));
        assert_eq!(decode(&reply(false, 0xA1, 0, 0)), Some(Reply::Offline));
    }

    #[test]
    fn rejected() {
        // Our own query read back, an unknown status, and a level out of range.
        let mut q = vec![0u8; 65];
        q[..7].copy_from_slice(&QUERY);
        assert_eq!(decode(&q), None);
        assert_eq!(decode(&reply(true, 0x55, 0, 50)), None);
        assert_eq!(decode(&reply(true, 0xA1, 0, 101)), None);
    }
}
