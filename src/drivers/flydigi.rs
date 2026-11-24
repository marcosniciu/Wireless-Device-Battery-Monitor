//! Flydigi Apex 6 / Apex 6 Pro controllers (37D7:2502), on their 2.4 GHz dongle or USB cable.
//!
//! Protocol as documented by SDL (zlib license, src/joystick/hidapi/SDL_hidapi_flydigi.c:
//! USB_PRODUCT_FLYDIGI_V2_APEX6, GetReply, SendInfoRequest, HandleInfoResponse) and as used for the
//! Apex 5 / Vader 5 Pro by the original C++ BatteryHub (GPL-3.0, src/drivers/flydigi.cpp);
//! implemented here from those facts. Vendor collection FFA0:0001, 32-byte reports; the Apex 6
//! sends its commands as unnumbered output reports (report id 0):
//!
//!   07 check architecture   00 5A A5 07 00 00  ->  5A A5 07 [3] [4]      [3] = 1, [4] = 0: new architecture
//!   01 get info             00 5A A5 01 02 00  ->  5A A5 01 .. .. [5] model id, [6] connection,
//!                                                  [11] = status << 4 | level
//!   model id 149 Apex 6, 150 Apex 6 Pro, 152 Apex 6 Pro Phantom Blade Zero
//!   connection: new architecture 0 = cable, 1 = 2.4 GHz; old 1 = cable, 2 = 2.4 GHz
//!   status 0 = on battery, 1 = charging, 2 = full; Apex 6 level 0..10 = tens of percent
//!
//! Only these two queries are sent (no "acquire controller", no settings). A reply counts only
//! with the echoed command and a known Apex 6 model id. No reply = controller off (dongle present).
use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::{HidColl, HidDev};
use crate::util::Deadline;

const VID: u16 = 0x37D7;
const PID: u16 = 0x2502;
const CMD_ARCH: u8 = 0x07;
const CMD_INFO: u8 = 0x01;

#[derive(Debug, PartialEq)]
pub struct Reading {
    pub name: &'static str,
    pub percent: i32,
    pub charging: bool,
    pub full: bool,
    pub wireless: bool,
}

fn model(id: u8) -> Option<&'static str> {
    match id {
        149 => Some("Flydigi Apex 6"),
        150 | 152 => Some("Flydigi Apex 6 Pro"),
        _ => None,
    }
}

/// A reply frame without a leading report id byte (SDL drops one if the firmware keeps it).
fn frame(r: &[u8]) -> &[u8] {
    if r.len() >= 3 && r[0] != 0x5A && r[1] == 0x5A && r[2] == 0xA5 { &r[1..] } else { r }
}

/// Whether `r` is the reply to `cmd`.
fn is_reply(r: &[u8], cmd: u8) -> bool {
    let r = frame(r);
    r.len() >= 5 && r[0] == 0x5A && r[1] == 0xA5 && r[2] == cmd
}

/// Architecture reply -> true for the new architecture.
pub fn parse_arch(r: &[u8]) -> Option<bool> {
    let r = frame(r);
    is_reply(r, CMD_ARCH).then(|| r[3] == 1 && r[4] == 0)
}

/// Info reply -> reading (None for another frame, an unknown model or an invalid level).
pub fn parse_info(r: &[u8], new_arch: bool) -> Option<Reading> {
    let r = frame(r);
    if !is_reply(r, CMD_INFO) || r.len() < 12 {
        return None;
    }
    let name = model(r[5])?;
    let (status, level) = (r[11] >> 4, (r[11] & 0x0F) as i32);
    if status > 2 || level > 10 {
        return None;
    }
    let wireless = if new_arch { r[6] == 1 } else { r[6] == 2 };
    Some(Reading {
        name,
        percent: if status == 2 { 100 } else { level * 10 },
        charging: status == 1,
        full: status == 2,
        wireless,
    })
}

fn ask(d: &HidDev, cmd: u8, param: u8) -> Option<Vec<u8>> {
    d.flush();
    if !d.write(&[0x00, 0x5A, 0xA5, cmd, param, 0x00]) {
        return None;
    }
    let end = Deadline::in_ms(800);
    while !end.passed() {
        if let Some(r) = d.read(end.left_ms().max(1)) {
            if is_reply(&r, cmd) {
                return Some(r);
            }
        }
    }
    None
}

fn query(c: &HidColl) -> Option<Reading> {
    let d = HidDev::open(c)?;
    let new_arch = ask(&d, CMD_ARCH, 0).and_then(|r| parse_arch(&r)).unwrap_or(false);
    ask(&d, CMD_INFO, 0x02).and_then(|r| parse_info(&r, new_arch))
}

pub fn poll(ctx: &mut PollContext) {
    if ctx.is_claimed(VID, PID) {
        return;
    }
    let colls = ctx.find(VID, PID, |c| c.page() == 0xFFA0 && c.usage() == 0x0001 && c.in_len() >= 12 && c.out_len() >= 6);
    if colls.is_empty() {
        return;
    }
    let base = DeviceStatus {
        key: "flydigi:2502".into(),
        name: "Flydigi Apex 6".into(),
        kind: DeviceKind::Gamepad,
        via: "2.4 GHz receiver".into(),
        hw: Some((VID, PID)),
        grace_sec: 60,
        ..Default::default()
    };
    let s = match colls.iter().find_map(query) {
        Some(r) => DeviceStatus {
            name: r.name.into(),
            via: if r.wireless { "2.4 GHz receiver".into() } else { "USB cable".into() },
            percent: r.percent,
            charging: r.charging,
            full: r.full,
            ..base
        },
        None => DeviceStatus { connected: false, ..base },
    };
    ctx.add(s, VID, PID);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(model: u8, conn: u8, power: u8) -> Vec<u8> {
        let mut r = vec![0u8; 32];
        r[..3].copy_from_slice(&[0x5A, 0xA5, CMD_INFO]);
        r[5] = model;
        r[6] = conn;
        r[11] = power;
        r
    }

    #[test]
    fn architecture() {
        assert_eq!(parse_arch(&[0x5A, 0xA5, 0x07, 0x01, 0x00, 0, 0]), Some(true));
        assert_eq!(parse_arch(&[0x5A, 0xA5, 0x07, 0x00, 0x00, 0, 0]), Some(false));
        assert_eq!(parse_arch(&[0x5A, 0xA5, 0x01, 0x01, 0x00, 0, 0]), None);
    }

    #[test]
    fn info_replies() {
        // Apex 6 on the dongle (new architecture), on battery, level 7 -> 70 %.
        let r = parse_info(&info(149, 1, 0x07), true).unwrap();
        assert_eq!((r.name, r.percent, r.charging, r.full, r.wireless), ("Flydigi Apex 6", 70, false, false, true));
        // Apex 6 Pro on the cable, charging at 40 %.
        let r = parse_info(&info(150, 0, 0x14), true).unwrap();
        assert_eq!((r.percent, r.charging, r.wireless), (40, true, false));
        // Full.
        assert_eq!(parse_info(&info(152, 1, 0x20), true).map(|r| (r.percent, r.full)), Some((100, true)));
        // Old architecture: 2 = wireless.
        assert!(parse_info(&info(149, 2, 0x05), false).unwrap().wireless);
        // With the report id byte kept.
        let mut numbered = vec![0x03];
        numbered.extend(info(149, 1, 0x03));
        assert_eq!(parse_info(&numbered, true).map(|r| r.percent), Some(30));
    }

    #[test]
    fn rejected() {
        assert_eq!(parse_info(&info(128, 1, 0x05), true), None); // Apex 5 model id
        assert_eq!(parse_info(&info(149, 1, 0x35), true), None); // unknown status
        assert_eq!(parse_info(&info(149, 1, 0x0B), true), None); // level out of range
        let mut other = info(149, 1, 0x05);
        other[2] = 0x10;
        assert_eq!(parse_info(&other, true), None);
    }
}
