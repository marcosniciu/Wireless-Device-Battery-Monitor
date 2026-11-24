//! Redragon M719 Invader Pro on its 2.4 GHz receiver 25A7:FA7E ("Compx 2.4G Wireless Receiver").
//!
//! Observed in Redragon's official software (OemDrv.exe) on real hardware, and checked against the
//! level it shows (4 = 40 %, 3 = 30 %). A CompX-style frame (see compx.rs), but sent as feature
//! report 08 and answered on a different collection, interface 1:
//!
//!   request  feature report 08 on FF02:0002 (17 bytes):  08 CC 00 ... 00 SS
//!   reply    input report 09 on FF01 (17 bytes):         09 CC ... SS
//!   SS (byte 16) = 0x55 - sum(bytes 0..15), in both directions
//!   cmd 02   reply [2] = 1 when the mouse is linked to the receiver
//!   cmd 04   reply [5] >= 1 valid, [6] = level 0..10 (x 10 %), [7] = 1 on the USB cable (charging)
//!
//! Only these two queries are sent (the ones the official software sends); nothing is written to
//! the mouse. A mouse that is off or out of range is shown disconnected.
use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::drivers::compx::checksum;
use crate::hid::{HidColl, HidDev};
use crate::util::Deadline;

const VID: u16 = 0x25A7;
const PID: u16 = 0xFA7E;
const LEN: usize = 17;
const CMD_LINK: u8 = 0x02;
const CMD_POWER: u8 = 0x04;
const REPLY_MS: u64 = 1500;

pub fn request(cmd: u8) -> [u8; LEN] {
    let mut r = [0u8; LEN];
    r[0] = 0x08;
    r[1] = cmd;
    r[LEN - 1] = checksum(&r);
    r
}

fn valid(r: &[u8], cmd: u8) -> bool {
    r.len() >= LEN && r[0] == 0x09 && r[1] == cmd && r[LEN - 1] == checksum(r)
}

/// Reply to cmd 02: Some(mouse linked).
pub fn parse_link(r: &[u8]) -> Option<bool> {
    valid(r, CMD_LINK).then(|| r[2] == 0x01)
}

/// Reply to cmd 04: Some((percent, charging)).
pub fn parse_power(r: &[u8]) -> Option<(i32, bool)> {
    (valid(r, CMD_POWER) && r[5] >= 1 && r[6] <= 10).then(|| (r[6] as i32 * 10, r[7] == 0x01))
}

fn query<T>(tx: &HidDev, rx: &HidDev, cmd: u8, parse: fn(&[u8]) -> Option<T>) -> Option<T> {
    rx.flush();
    if !tx.set_feature(&request(cmd)) {
        return None;
    }
    let end = Deadline::in_ms(REPLY_MS);
    while !end.passed() {
        if let Some(v) = rx.read(end.left_ms()).and_then(|r| parse(&r)) {
            return Some(v);
        }
    }
    None
}

enum Reading {
    Power(i32, bool),
    Away,
}

fn read(tx: &HidColl, rx: &HidColl) -> Option<Reading> {
    let t = HidDev::open(tx).or_else(|| HidDev::open_feature_only(tx))?;
    let r = HidDev::open_read_only(rx)?;
    match query(&t, &r, CMD_LINK, parse_link) {
        Some(false) => return Some(Reading::Away),
        None => return None,
        Some(true) => {}
    }
    Some(query(&t, &r, CMD_POWER, parse_power).map_or(Reading::Away, |(p, c)| Reading::Power(p, c)))
}

pub fn poll(ctx: &mut PollContext) {
    if ctx.is_claimed(VID, PID) {
        return;
    }
    let tx = ctx.find(VID, PID, |c| c.iface == 1 && c.page() == 0xFF02 && c.usage() == 0x0002 && c.feat_len() as usize == LEN);
    let rx = ctx.find(VID, PID, |c| c.iface == 1 && c.page() == 0xFF01 && c.in_len() as usize == LEN);
    let (Some(tx), Some(rx)) = (tx.first(), rx.first()) else { return };
    let Some(reading) = read(tx, rx) else { return };
    let base = DeviceStatus {
        key: "redragon:m719".into(),
        name: "Redragon M719 Invader Pro".into(),
        kind: DeviceKind::Mouse,
        via: "2.4 GHz receiver".into(),
        hw: Some((VID, PID)),
        grace_sec: 1800,
        ..Default::default()
    };
    let s = match reading {
        Reading::Power(percent, charging) => DeviceStatus { percent, charging, ..base },
        Reading::Away => DeviceStatus { connected: false, ..base },
    };
    ctx.add(s, VID, PID);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(bytes: &[u8]) -> Vec<u8> {
        let mut f = vec![0u8; LEN];
        f[..bytes.len()].copy_from_slice(bytes);
        f[LEN - 1] = checksum(&f);
        f
    }

    #[test]
    fn frames_recorded_on_hardware() {
        // Replies read from a real M719: linked; 40 % on battery; 30 % on the cable.
        assert_eq!(parse_link(&[0x09, 0x02, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x49]), Some(true));
        assert_eq!(parse_power(&[0x09, 0x04, 0, 0, 0, 0x02, 0x04, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0x42]), Some((40, false)));
        assert_eq!(parse_power(&[0x09, 0x04, 0, 0, 0, 0x02, 0x03, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0x42]), Some((30, true)));
    }

    #[test]
    fn rejected() {
        assert_eq!(parse_link(&reply(&[0x09, 0x02, 0x00])), Some(false));
        let mut bad = reply(&[0x09, 0x04, 0, 0, 0, 0x02, 0x04]);
        bad[16] ^= 1;
        assert_eq!(parse_power(&bad), None); // checksum
        assert_eq!(parse_power(&reply(&[0x09, 0x04, 0, 0, 0, 0x00, 0x04])), None); // not valid yet
        assert_eq!(parse_power(&reply(&[0x09, 0x04, 0, 0, 0, 0x02, 0x0B])), None); // > 10
        assert_eq!(parse_power(&request(CMD_POWER)), None); // our own request
    }
}
