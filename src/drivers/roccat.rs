//! ROCCAT Syn Max Air (VID 1E7D, PID 3A3F), a Turtle Beach "Stellar Wireless" platform.
//!
//! Protocol captured from ROCCAT Swarm (USBPcap, firmware 1.08). The receiver's vendor collection
//! (usage page FF00, 64-byte unnumbered input/output reports) relays the request to the headset:
//!
//!   request   8A 07 06 01
//!   reply     8A 83 06 02 LL 00 V1 V1 V2 V2 CC
//!             LL     coarse level 0..5 (what Swarm shows; 6 while charging)
//!             V1 V1  cell voltage in mV, little-endian, 20 mV steps
//!             V2 V2  second (filtered) voltage
//!             CC     01 while charging / on the dock
//!
//! Byte 0 is a tag that the headset echoes; the receiver answers other tags itself. No reply within
//! the timeout means the headset is switched off or asleep (the receiver stays enumerated).
//! The percentage is derived from the voltage with a curve measured under load (audio + RGB) over a
//! full 16 h discharge; while charging the voltage is not meaningful and only the level is shown.
use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::HidDev;
use crate::util::hex4;

const VID: u16 = 0x1E7D;
const SYN_MAX_AIR: u16 = 0x3A3F;

/// Voltage (mV) -> percent, discharging under typical load (measured on hardware, ~16 h total).
const DISCHARGE: &[(i32, i32)] = &[
    (4080, 100), (4040, 97), (4000, 92), (3960, 86), (3920, 78), (3900, 74), (3880, 70),
    (3840, 66), (3820, 63), (3800, 60), (3780, 53), (3760, 45), (3740, 34), (3720, 26),
    (3700, 20), (3680, 15), (3660, 11), (3640, 7), (3600, 3), (3500, 0),
];

pub fn percent_from_mv(mv: i32) -> i32 {
    if mv >= DISCHARGE[0].0 {
        return DISCHARGE[0].1;
    }
    if mv <= DISCHARGE[DISCHARGE.len() - 1].0 {
        return DISCHARGE[DISCHARGE.len() - 1].1;
    }
    for w in DISCHARGE.windows(2) {
        let ((v1, p1), (v2, p2)) = (w[0], w[1]);
        if mv <= v1 && mv >= v2 {
            return p2 + (mv - v2) * (p1 - p2) / (v1 - v2);
        }
    }
    0
}

/// Decodes the 8A 83 06 02 … status reply. `r` is the stripped input report (r[0] == 0x8A).
pub fn decode(pid: u16, r: &[u8]) -> Option<DeviceStatus> {
    if r.len() < 10 || r[0] != 0x8A || r[1] != 0x83 || r[2] != 0x06 {
        return None;
    }
    let level = r[4] as i32;
    let mv = r[6] as i32 | ((r[7] as i32) << 8);
    let charging = r.len() > 10 && r[10] == 0x01;
    let mut s = DeviceStatus {
        key: format!("roccat:{}", hex4(pid)),
        name: "ROCCAT Syn Max Air".into(),
        kind: DeviceKind::Headset,
        via: "2.4 GHz receiver".into(),
        millivolts: mv,
        charging,
        grace_sec: 60,
        ..Default::default()
    };
    if charging {
        // Terminal voltage says little while charging; show the device's own coarse level.
        s.percent = -1;
        s.level_text = if mv >= 4180 { "Full".into() } else { "Charging".into() };
        s.full = mv >= 4180;
    } else {
        s.percent = percent_from_mv(mv);
        s.detail = format!("{} mV, level {}/5", mv, level.min(5));
    }
    Some(s)
}

fn query(d: &HidDev) -> Option<Vec<u8>> {
    d.flush();
    if !d.write(&[0x00, 0x8A, 0x07, 0x06, 0x01]) {
        return None;
    }
    // The headset also pushes unsolicited E0..E6 event reports; wait for our tag.
    let end = crate::util::Deadline::in_ms(1500);
    loop {
        let left = end.left_ms();
        if left == 0 {
            return None;
        }
        let r = d.read(left)?;
        if !r.is_empty() && r[0] == 0x8A {
            return Some(r);
        }
    }
}

pub fn poll(ctx: &mut PollContext) {
    for (vid, pid) in ctx.products(VID) {
        if pid != SYN_MAX_AIR {
            continue;
        }
        for c in ctx.find(vid, pid, |c| c.page() == 0xFF00 && c.out_len() >= 64) {
            let Some(d) = HidDev::open(&c) else { continue };
            match query(&d).and_then(|r| decode(pid, &r)) {
                Some(s) => ctx.add(DeviceStatus { hw: Some((vid, pid)), ..s }, vid, pid),
                None => {
                    // Receiver present, headset off or asleep.
                    let s = DeviceStatus {
                        key: format!("roccat:{}", hex4(pid)),
                        name: "ROCCAT Syn Max Air".into(),
                        kind: DeviceKind::Headset,
                        via: "2.4 GHz receiver".into(),
                        connected: false,
                        hw: Some((vid, pid)),
                        ..Default::default()
                    };
                    ctx.add(s, vid, pid);
                }
            }
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Replies recorded from a Syn Max Air (1E7D:3A3F) with USBPcap.
    #[test]
    fn on_battery() {
        let s = decode(SYN_MAX_AIR, &[0x8A, 0x83, 0x06, 0x02, 0x05, 0x00, 0x50, 0x0F, 0x50, 0x0F, 0x00]).unwrap();
        assert_eq!(s.millivolts, 3920);
        assert_eq!(s.percent, 78);
        assert!(!s.charging && s.connected);
    }

    #[test]
    fn on_dock() {
        let s = decode(SYN_MAX_AIR, &[0x8A, 0x83, 0x06, 0x02, 0x05, 0x00, 0x64, 0x0F, 0x64, 0x0F, 0x01]).unwrap();
        assert!(s.charging);
        assert_eq!(s.millivolts, 3940);
        assert_eq!(s.percent, -1);
    }

    #[test]
    fn curve_ends() {
        assert_eq!(percent_from_mv(4200), 100);
        assert_eq!(percent_from_mv(3400), 0);
        assert_eq!(percent_from_mv(3740), 34);
    }
}
