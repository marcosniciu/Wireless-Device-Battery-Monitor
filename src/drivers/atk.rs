//! ATK / VXE mice on the "NK mouse NANO dongle" (VID 373B, PID 1216): VXE R1 NK, ATK A9 PLUS 2.0
//! and other models sharing that dongle.
//!
//! Protocol (ATK's "BITMOUSE" protocol) as documented by HaloBattery PR #350
//! (github.com/HeyOkay/HaloBattery, providers/atk_bitmouse.py), mouse-battery
//! (github.com/qwr79/mouse-battery) and the original C++ BatteryHub (GPL-3.0,
//! src/drivers/atk_bitmouse.cpp); implemented here from those facts. The dongle's vendor
//! collection (usage page FF05, usage 0001, 64-byte output report 08):
//!
//!   request  08 SS 72 02 00 01 CC 01 | 00 ...     SS = sum of bytes 2..63; target 01 = the mouse
//!            CC 07 = battery level, 0F = charging flag
//!   reply    08 72 ST QQ 00 CC 01 VV ...          ST FF = error; VV = value (level 0..100 / 1 charging)
//!
//! No reply means the mouse is asleep or off; the app then shows it asleep with its last level, and
//! it is asked at most every ASLEEP_QUERY_EVERY.
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::{HidColl, HidDev};
use crate::util::{hex4, Deadline};

const VID: u16 = 0x373B;
const NANO_DONGLE: u16 = 0x1216;
pub const CMD_LEVEL: u8 = 0x07;
pub const CMD_CHARGING: u8 = 0x0F;
const REPLY_MS: u64 = 700;
const ASLEEP_QUERY_EVERY: Duration = Duration::from_secs(60);

pub fn request(cmd: u8) -> [u8; 64] {
    let mut r = [0u8; 64];
    r[0] = 0x08;
    r[2] = 0x72;
    r[3] = 0x02;
    r[5] = 0x01;
    r[6] = cmd;
    r[7] = 0x01;
    r[1] = r[2..].iter().fold(0u8, |a, &b| a.wrapping_add(b));
    r
}

/// The value of a reply to `cmd` (report ID included). Some(None) = the dongle reported an error.
pub fn parse(r: &[u8], cmd: u8) -> Option<Option<u8>> {
    if r.len() < 8 || r[0] != 0x08 || r[1] != 0x72 || r[5] != cmd {
        return None;
    }
    Some((r[2] != 0xFF).then_some(r[7]))
}

fn query(d: &HidDev, cmd: u8) -> Option<u8> {
    d.flush();
    if !d.write(&request(cmd)) {
        return None;
    }
    let end = Deadline::in_ms(REPLY_MS);
    while !end.passed() {
        if let Some(v) = d.read(end.left_ms()).and_then(|r| parse(&r, cmd)) {
            return v;
        }
    }
    None
}

fn read(c: &HidColl) -> Option<(i32, bool)> {
    let d = HidDev::open(c)?;
    let level = query(&d, CMD_LEVEL).filter(|l| (1..=100).contains(l))?;
    Some((level as i32, query(&d, CMD_CHARGING) == Some(1)))
}

static NEXT_TRY: Mutex<Option<Instant>> = Mutex::new(None);

pub fn poll(ctx: &mut PollContext) {
    if ctx.is_claimed(VID, NANO_DONGLE) {
        return;
    }
    let colls = ctx.find(VID, NANO_DONGLE, |c| c.page() == 0xFF05 && c.usage() == 0x0001 && c.out_len() >= 64 && c.in_len() >= 8);
    let mut next = NEXT_TRY.lock().unwrap_or_else(|e| e.into_inner());
    if colls.is_empty() {
        *next = None;
        return;
    }
    if next.is_some_and(|t| Instant::now() < t) {
        return;
    }
    let Some((level, charging)) = colls.iter().find_map(read) else {
        *next = Some(Instant::now() + ASLEEP_QUERY_EVERY);
        return;
    };
    *next = None;
    drop(next);
    let s = DeviceStatus {
        key: format!("atknk:{}", hex4(NANO_DONGLE)),
        name: "ATK / VXE mouse".into(), // the dongle is shared by several models
        kind: DeviceKind::Mouse,
        via: "2.4 GHz receiver".into(),
        percent: level,
        charging,
        hw: Some((VID, NANO_DONGLE)),
        grace_sec: 1800,
        ..Default::default()
    };
    ctx.add(s, VID, NANO_DONGLE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_frames() {
        let r = request(CMD_LEVEL);
        assert_eq!(&r[..8], &[0x08, 0x7D, 0x72, 0x02, 0x00, 0x01, 0x07, 0x01]);
        assert!(r[8..].iter().all(|&b| b == 0));
        assert_eq!(request(CMD_CHARGING)[1], 0x85);
    }

    #[test]
    fn replies() {
        assert_eq!(parse(&[0x08, 0x72, 0x00, 0x05, 0x00, 0x07, 0x01, 63], CMD_LEVEL), Some(Some(63)));
        assert_eq!(parse(&[0x08, 0x72, 0x00, 0x06, 0x00, 0x0F, 0x01, 1], CMD_CHARGING), Some(Some(1)));
        assert_eq!(parse(&[0x08, 0x72, 0xFF, 0x05, 0x00, 0x07, 0x01, 0], CMD_LEVEL), Some(None));
    }

    #[test]
    fn not_replies() {
        assert_eq!(parse(&[0x08, 0x72, 0x00, 0x05, 0x00, 0x0F, 0x01, 1], CMD_LEVEL), None); // other command
        assert_eq!(parse(&request(CMD_LEVEL), CMD_LEVEL), None); // echo of the request
        assert_eq!(parse(&[0x09, 0x72, 0x00, 0x05, 0x00, 0x07, 0x01, 50], CMD_LEVEL), None);
        assert_eq!(parse(&[0x08, 0x72, 0x00], CMD_LEVEL), None);
    }
}
