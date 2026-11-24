//! Logitech G533 Wireless headset over its dongle (046D:0A66).
//!
//! Protocol and voltage calibration as documented by HeadsetControl (GPL-3.0,
//! github.com/Sapd/HeadsetControl, lib/devices/logitech_g533.hpp, protocols/hidpp_protocol.hpp and
//! protocols/logitech_calibrations.hpp); implemented here from those facts. The G533 does not need
//! the HID++ feature lookup the general Logitech driver relies on: its battery voltage is read with a
//! fixed long HID++ request on the FF43:0202 collection of interface 3 (20-byte reports):
//!
//!   request  11 FF 07 01 00 ...           feature index 07, function 0, software id 1
//!   reply    11 FF 07 01 HH LL ST ...     HHLL = voltage in mV (big endian), ST 03 = charging
//!   offline  11 FF FF ...                 HID++ error: the headset is off or out of range
//!
//! A voltage below the lowest calibration point is not a valid reading. Read-only.
use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::{HidColl, HidDev};
use crate::util::{interpolate, Deadline};

const VID: u16 = 0x046D;
pub const PID: u16 = 0x0A66;
const LEN: usize = 20;
const REPLY_MS: u64 = 1000;
const CAL_MV: [i32; 6] = [4200, 3850, 3790, 3750, 3680, 3330];
const CAL_PCT: [i32; 6] = [100, 50, 30, 20, 5, 0];

pub fn request() -> [u8; LEN] {
    let mut r = [0u8; LEN];
    r[..4].copy_from_slice(&[0x11, 0xFF, 0x07, 0x01]);
    r
}

#[derive(Debug, PartialEq)]
pub enum Reply {
    /// Voltage in mV and charging flag.
    Battery(i32, bool),
    Offline,
}

/// Decodes an input report (report id 11 first). None = not the reply.
pub fn parse(r: &[u8]) -> Option<Reply> {
    if r.len() < 7 || r[0] != 0x11 || r[1] != 0xFF {
        return None;
    }
    if r[2] == 0xFF && r[3] == 0x07 {
        return Some(Reply::Offline);
    }
    if r[2] != 0x07 || r[3] != 0x01 {
        return None;
    }
    let mv = ((r[4] as i32) << 8) | r[5] as i32;
    if mv < CAL_MV[CAL_MV.len() - 1] {
        return Some(Reply::Offline);
    }
    Some(Reply::Battery(mv, r[6] == 0x03))
}

pub fn percent(mv: i32) -> i32 {
    interpolate(mv, &CAL_MV, &CAL_PCT)
}

fn query(c: &HidColl) -> Option<Reply> {
    let d = HidDev::open(c)?;
    d.flush();
    if !d.write(&request()) {
        return None;
    }
    let end = Deadline::in_ms(REPLY_MS);
    while !end.passed() {
        if let Some(r) = d.read(end.left_ms()).and_then(|r| parse(&r)) {
            return Some(r);
        }
    }
    None
}

pub fn poll(ctx: &mut PollContext) {
    if ctx.is_claimed(VID, PID) {
        return;
    }
    let colls = ctx.find(VID, PID, |c| c.iface == 3 && c.page() == 0xFF43 && c.usage() == 0x0202 && c.out_len() as usize == LEN);
    if colls.is_empty() {
        return;
    }
    let base = DeviceStatus {
        key: "logi-g533".into(),
        name: "Logitech G533".into(),
        kind: DeviceKind::Headset,
        via: "2.4 GHz dongle".into(),
        vid: VID,
        hw: Some((VID, PID)),
        grace_sec: 60,
        ..Default::default()
    };
    let s = match colls.iter().find_map(query) {
        Some(Reply::Battery(mv, charging)) => DeviceStatus { percent: percent(mv), millivolts: mv, charging, ..base },
        Some(Reply::Offline) => DeviceStatus { connected: false, ..base },
        // No answer at all: leave the dongle to the general Logitech driver.
        None => return,
    };
    ctx.add(s, VID, PID);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_frame() {
        let r = request();
        assert_eq!(&r[..4], &[0x11, 0xFF, 0x07, 0x01]);
        assert!(r[4..].iter().all(|&b| b == 0));
    }

    #[test]
    fn replies() {
        let mut r = [0u8; 20];
        r[..7].copy_from_slice(&[0x11, 0xFF, 0x07, 0x01, 0x0F, 0x0A, 0x01]); // 3850 mV, discharging
        assert_eq!(parse(&r), Some(Reply::Battery(3850, false)));
        r[6] = 0x03;
        assert_eq!(parse(&r), Some(Reply::Battery(3850, true)));
        r[4] = 0x0C; // 3082 mV: below the calibration, not valid
        assert_eq!(parse(&r), Some(Reply::Offline));
        assert_eq!(parse(&[0x11, 0xFF, 0xFF, 0x07, 0x01, 0x05, 0x00]), Some(Reply::Offline));
        assert_eq!(parse(&[0x11, 0xFF, 0x08, 0x01, 0x0F, 0x0A, 0x01]), None); // another feature
        assert_eq!(parse(&[0x10, 0xFF, 0x07, 0x01, 0x0F, 0x0A, 0x01]), None);
        assert_eq!(parse(&[0x11, 0xFF, 0x07]), None);
    }

    #[test]
    fn levels() {
        assert_eq!(percent(4250), 100);
        assert_eq!(percent(3850), 50);
        assert_eq!(percent(3330), 0);
    }
}
