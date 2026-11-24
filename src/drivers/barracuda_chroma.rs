//! Razer Barracuda X Chroma headset on its 2.4 GHz dongle (1532:0574, MediaTek). It speaks neither
//! the Razer 90-byte protocol (razer.rs) nor the "PA" frames of the BlackShark / Barracuda X (2022)
//! dongles, but a property protocol on vendor collection FF14:0001 (64-byte output / input report 02).
//!
//! Protocol as documented by batterywatch (GPL-3.0, github.com/itayavra/batterywatch, PR #55,
//! contents/bin/read_hid_devices: reverse engineered from Razer Synapse captures and tested on real
//! hardware); implemented here from those facts.
//!
//!   request  02 00 60 00 00 00 04 00 00 80 PP 00 ... CC       CC (byte 62) = XOR of bytes 0..61
//!   reply    02 02 60 00 00 00 05 00 80 80 PP 01 01 VV ... CC CC (byte 62) = XOR of bytes 1..61
//!
//! Property 21 = battery level (0..100), 2A = charging (00 unplugged / 01 plugged). Only these two
//! read properties are asked for. No reply means the headset is off or out of range.
use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::{HidColl, HidDev};
use crate::util::{hex4, Deadline};

const VID: u16 = 0x1532;
const PID: u16 = 0x0574;
const LEN: usize = 64;
const REPORT: u8 = 0x02;
pub const PROP_BATTERY: u8 = 0x21;
pub const PROP_CHARGING: u8 = 0x2A;
const VALUE: usize = 13;
const REPLY_MS: u64 = 500;

fn xor(b: &[u8]) -> u8 {
    b.iter().fold(0, |a, &x| a ^ x)
}

pub fn request(prop: u8) -> [u8; LEN] {
    let mut r = [0u8; LEN];
    r[..11].copy_from_slice(&[REPORT, 0x00, 0x60, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x80, prop]);
    r[62] = xor(&r[..62]);
    r
}

/// The value byte of a reply to `prop` (report id included), checked against header and checksum.
pub fn parse(r: &[u8], prop: u8) -> Option<u8> {
    let head = [REPORT, 0x02, 0x60, 0x00, 0x00, 0x00, 0x05, 0x00, 0x80, 0x80, prop, 0x01, 0x01];
    if r.len() < LEN || r[..13] != head || r[62] != xor(&r[1..62]) {
        return None;
    }
    let v = r[VALUE];
    match prop {
        PROP_BATTERY if v > 100 => None,
        PROP_CHARGING if v > 1 => None,
        _ => Some(v),
    }
}

fn query(d: &HidDev, prop: u8) -> Option<u8> {
    d.flush();
    if !d.write(&request(prop)) {
        return None;
    }
    let end = Deadline::in_ms(REPLY_MS);
    while !end.passed() {
        if let Some(v) = d.read(end.left_ms()).and_then(|r| parse(&r, prop)) {
            return Some(v);
        }
    }
    None
}

fn read(c: &HidColl) -> Option<(i32, bool)> {
    let d = HidDev::open(c)?;
    let level = query(&d, PROP_BATTERY).or_else(|| query(&d, PROP_BATTERY))?;
    Some((level as i32, query(&d, PROP_CHARGING) == Some(1)))
}

pub fn poll(ctx: &mut PollContext) {
    if ctx.is_claimed(VID, PID) {
        return;
    }
    let colls = ctx.find(VID, PID, |c| c.page() == 0xFF14 && c.out_len() as usize == LEN && c.in_len() as usize == LEN);
    if colls.is_empty() {
        return;
    }
    let base = DeviceStatus {
        key: format!("barracudax:{}", hex4(PID)),
        name: "Razer Barracuda X Chroma".into(),
        kind: DeviceKind::Headset,
        via: "2.4 GHz dongle".into(),
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

    fn reply(prop: u8, value: u8) -> Vec<u8> {
        let mut r = vec![0u8; LEN];
        r[..14].copy_from_slice(&[0x02, 0x02, 0x60, 0, 0, 0, 0x05, 0, 0x80, 0x80, prop, 0x01, 0x01, value]);
        r[62] = xor(&r[1..62]);
        r
    }

    #[test]
    fn requests() {
        let r = request(PROP_BATTERY);
        assert_eq!(r[..11], [0x02, 0x00, 0x60, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x80, 0x21]);
        // 02 ^ 60 ^ 04 ^ 80 ^ 21
        assert_eq!(r[62], 0xC7);
        assert_eq!(request(PROP_CHARGING)[62], 0xCC);
        assert!(r[11..62].iter().all(|&b| b == 0) && r[63] == 0);
    }

    #[test]
    fn replies() {
        assert_eq!(parse(&reply(PROP_BATTERY, 73), PROP_BATTERY), Some(73));
        assert_eq!(parse(&reply(PROP_CHARGING, 1), PROP_CHARGING), Some(1));
        assert_eq!(parse(&reply(PROP_BATTERY, 101), PROP_BATTERY), None);
        assert_eq!(parse(&reply(PROP_CHARGING, 2), PROP_CHARGING), None);
        assert_eq!(parse(&reply(PROP_CHARGING, 1), PROP_BATTERY), None); // other property
        let mut bad = reply(PROP_BATTERY, 50);
        bad[62] ^= 1;
        assert_eq!(parse(&bad, PROP_BATTERY), None);
        assert_eq!(parse(&request(PROP_BATTERY), PROP_BATTERY), None); // echoed request
    }
}
