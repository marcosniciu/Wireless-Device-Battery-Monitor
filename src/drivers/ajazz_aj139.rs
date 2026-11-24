//! AJAZZ AJ139 V2 MC mouse on its 2.4 GHz receiver (VID A8A5, PID 2255) or USB cable (A8A4:2255). The
//! product id is shared with other AJAZZ models on the same chip, so the mouse gets a generic name.
//!
//! Protocol as documented by ajazz-aj139-macos (MIT, github.com/werquess/ajazz-aj139-macos,
//! PROTOCOL.md, "checked against a real device") and as used by the original C++ BatteryHub (GPL-3.0,
//! src/drivers/misc_mice.cpp); implemented here from those facts. Vendor collection FF01:0010,
//! unnumbered 64-byte output / input reports:
//!
//!   request  55 30 A5 2E 2E 00 ...    command 30 (battery), read (A5), checksum = low byte of the
//!                                     sum of bytes 4..63 (2E), length 2E
//!   reply    AA 30 .. .. .. .. .. .. LL PP    LL = level %, PP non-zero on USB power
//!
//! Other reports (e.g. the unsolicited AA FA ...) are skipped. Only this read is sent; the
//! configuration-write command and the Bluetooth OTA service are never touched. A level of 0 from the
//! receiver means the mouse is off or out of range.
use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::{HidColl, HidDev};
use crate::util::{hex4, Deadline};

const PID: u16 = 0x2255;
const RECEIVER_VID: u16 = 0xA8A5;
const CABLE_VID: u16 = 0xA8A4;
const CMD_BATTERY: u8 = 0x30;
const WAIT_MS: u64 = 700;

/// The output report, report ID 0 first.
pub fn request() -> [u8; 65] {
    let mut r = [0u8; 65];
    let p = &mut r[1..];
    p[..5].copy_from_slice(&[0x55, CMD_BATTERY, 0xA5, 0x00, 0x2E]);
    p[3] = p[4..].iter().fold(0u8, |a, &x| a.wrapping_add(x));
    r
}

/// Battery reply (64 bytes, no report ID): Some((level, on USB power)).
pub fn decode(r: &[u8]) -> Option<(i32, bool)> {
    (r.len() >= 10 && r[0] == 0xAA && r[1] == CMD_BATTERY && r[8] <= 100).then(|| (r[8] as i32, r[9] != 0))
}

fn read(c: &HidColl) -> Option<(i32, bool)> {
    let d = HidDev::open(c)?;
    d.flush();
    if !d.write(&request()) {
        return None;
    }
    let end = Deadline::in_ms(WAIT_MS);
    while !end.passed() {
        if let Some(v) = d.read(end.left_ms()).and_then(|r| decode(&r)) {
            return Some(v);
        }
    }
    None
}

pub fn poll(ctx: &mut PollContext) {
    for vid in [RECEIVER_VID, CABLE_VID] {
        if ctx.is_claimed(vid, PID) {
            continue;
        }
        let colls = ctx.find(vid, PID, |c| c.page() == 0xFF01 && c.usage() == 0x0010 && c.out_len() >= 65 && c.in_len() >= 65);
        if colls.is_empty() {
            continue;
        }
        let cable = vid == CABLE_VID;
        let base = DeviceStatus {
            key: format!("ajazz139:{}:{}", hex4(vid), hex4(PID)),
            name: "AJAZZ wireless mouse".into(),
            kind: DeviceKind::Mouse,
            via: if cable { "USB cable" } else { "2.4 GHz receiver" }.into(),
            hw: Some((vid, PID)),
            grace_sec: 1800,
            ..Default::default()
        };
        let s = match colls.iter().find_map(read) {
            Some((level, power)) if cable || level > 0 => DeviceStatus { percent: level, charging: power || cable, ..base },
            _ if cable => continue, // the cable alone gives nothing to show
            _ => DeviceStatus { connected: false, ..base },
        };
        ctx.add(s, vid, PID);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_frame() {
        assert_eq!(request()[..7], [0x00, 0x55, 0x30, 0xA5, 0x2E, 0x2E, 0x00]);
        assert!(request()[7..].iter().all(|&b| b == 0));
    }

    #[test]
    fn battery_reply() {
        let mut r = vec![0u8; 64];
        r[..3].copy_from_slice(&[0xAA, 0x30, 0xA5]);
        r[8] = 76;
        assert_eq!(decode(&r), Some((76, false)));
        r[9] = 1;
        assert_eq!(decode(&r), Some((76, true)));
    }

    #[test]
    fn other_reports() {
        let mut r = vec![0u8; 64];
        r[..2].copy_from_slice(&[0xAA, 0xFA]); // unsolicited event
        r[8] = 50;
        assert_eq!(decode(&r), None);
        r[1] = 0x30;
        r[8] = 0xFF;
        assert_eq!(decode(&r), None);
        assert_eq!(decode(&request()[1..]), None); // our own request read back
    }
}
