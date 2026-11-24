//! MCHOSE V9 Pro headset on its 2.4 GHz receiver or USB cable (291D:385D, a C-Media chip; both
//! connections enumerate with this one id). A different protocol from the MCHOSE mice (mchose.rs).
//!
//! Protocol as documented by HaloBattery (MIT, github.com/HeyOkay/HaloBattery,
//! providers/mchose_v9.py, level confirmed on a V9 Pro under Windows), which took the exchange from
//! MCHOSE_v9_PRO_Controller (github.com/JoaoKSS/MCHOSE_v9_PRO_Controller, mchose_qt.py,
//! HIDService.query_status); implemented here from those facts. Output report 55, 64 bytes:
//!
//!   request  55 65 01 00 ...
//!   reply    55 65 LL SS ...     LL = level in percent, SS = status
//!
//! Status 00 was measured on the cable (charging), 02 on battery (HaloBattery); anything else is
//! shown as not charging. Only this id and only its 64/64-byte vendor collections are written to.
//! No reply means the headset is off or out of range (the receiver stays enumerated).
use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::{self, HidColl, HidDev};
use crate::util::Deadline;

const VID: u16 = 0x291D;
const PID: u16 = 0x385D;
const REPORT: u8 = 0x55;
const CMD_BATTERY: u8 = 0x65;
const LEN: usize = 64;
const REPLY_MS: u64 = 500;

pub fn request() -> [u8; LEN] {
    let mut r = [0u8; LEN];
    r[..3].copy_from_slice(&[REPORT, CMD_BATTERY, 0x01]);
    r
}

/// Some((level, charging)) from a reply (report id included).
pub fn parse(r: &[u8]) -> Option<(i32, bool)> {
    (r.len() >= 4 && r[0] == REPORT && r[1] == CMD_BATTERY && r[2] <= 100).then(|| (r[2] as i32, r[3] == 0x00))
}

fn read(c: &HidColl) -> Option<(i32, bool)> {
    let d = HidDev::open(c)?;
    d.flush();
    if !d.write(&request()) {
        return None;
    }
    let end = Deadline::in_ms(REPLY_MS);
    while !end.passed() {
        if let Some(v) = d.read(end.left_ms()).and_then(|r| parse(&r)) {
            return Some(v);
        }
    }
    None
}

pub fn poll(ctx: &mut PollContext) {
    if ctx.is_claimed(VID, PID) {
        return;
    }
    let colls = ctx.find(VID, PID, |c| c.page() >= 0xFF00 && c.out_len() as usize == LEN && c.in_len() as usize == LEN);
    let Some(first) = colls.first() else { return };
    let product = hid::product_string(first);
    let product = product.trim();
    let base = DeviceStatus {
        key: format!("mchosev9:{}", crate::util::hex4(PID)),
        name: if product.is_empty() { "MCHOSE V9 Pro".into() } else { product.to_string() },
        kind: DeviceKind::Headset,
        via: "2.4 GHz receiver".into(),
        hw: Some((VID, PID)),
        ..Default::default()
    };
    let s = match colls.iter().find_map(read) {
        Some((level, charging)) => DeviceStatus { percent: level, charging, ..base },
        None => DeviceStatus { connected: false, ..base },
    };
    ctx.add(s, VID, PID);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_frame() {
        let r = request();
        assert_eq!(r[..3], [0x55, 0x65, 0x01]);
        assert!(r[3..].iter().all(|&b| b == 0));
    }

    #[test]
    fn replies() {
        assert_eq!(parse(&[0x55, 0x65, 60, 0x00, 0x00]), Some((60, true))); // on the cable
        assert_eq!(parse(&[0x55, 0x65, 20, 0x02, 0x00]), Some((20, false))); // on battery
        assert_eq!(parse(&[0x55, 0x11, 3, 0x00]), None); // EQ-mode reply
        assert_eq!(parse(&[0x55, 0x65, 120, 0x02]), None);
        assert_eq!(parse(&[0x56, 0x65, 50, 0x02]), None);
        assert_eq!(parse(&[0x55, 0x65, 50]), None);
    }
}
