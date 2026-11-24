//! HyperX Cloud II Core Wireless headset over its 2.4 GHz dongle (VID 03F0, PID 0995; 0795 is the
//! second id the same headset shows, e.g. while charging).
//!
//! Protocol as documented by HaloBattery (MIT, github.com/HeyOkay/HaloBattery,
//! providers/hyperx_cloud3.py and docs/protocols.md: decoded from a USBPcap capture of NGENUITY in
//! HaloBattery issue #155 and verified on hardware there) and by HyperHeadset (MIT,
//! github.com/LennardKittner/HyperHeadset, the Cloud III Wireless exchange the Cloud II Core shares),
//! as also used by the original C++ BatteryHub (GPL-3.0, src/drivers/hyperx.cpp); implemented here
//! from those facts. Vendor collection FF13:0001 on interface 3, 62-byte output / input report 66:
//!
//!   request  66 89 00 ...      battery   reply 66 89|0D AA BB LL ...  LL = level %, valid only when
//!                                                                     AA or BB is non-zero
//!   request  66 8A 00 ...      charging  reply 66 8A|0C SS ...        SS 0 not charging, 1 charging,
//!                                                                     2 charged (both on the cable)
//!
//! Only these two reads are sent. No battery reply means the headset is off (the dongle stays).
use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::{HidColl, HidDev};
use crate::util::Deadline;

const VID: u16 = 0x03F0;
const PIDS: [u16; 2] = [0x0995, 0x0795];
const KEY: &str = "hyperx:cloud2core";
const REPORT: u8 = 0x66;
const CMD_BATTERY: u8 = 0x89;
const RESP_BATTERY: u8 = 0x0D;
const CMD_CHARGING: u8 = 0x8A;
const RESP_CHARGING: u8 = 0x0C;
const LEN: u16 = 62;
const WAIT_MS: u64 = 1200;

pub fn request(cmd: u8) -> [u8; 62] {
    let mut r = [0u8; 62];
    r[0] = REPORT;
    r[1] = cmd;
    r
}

/// Battery reply: Some(level).
pub fn parse_battery(r: &[u8]) -> Option<i32> {
    if r.len() < 5 || r[0] != REPORT || (r[1] != CMD_BATTERY && r[1] != RESP_BATTERY) {
        return None;
    }
    // All-zero state: no reading yet (also our own request read back).
    ((r[2] != 0 || r[3] != 0) && r[4] <= 100).then_some(r[4] as i32)
}

/// Charging reply: Some(on the cable).
pub fn parse_charging(r: &[u8]) -> Option<bool> {
    if r.len() < 3 || r[0] != REPORT || (r[1] != CMD_CHARGING && r[1] != RESP_CHARGING) || r[2] > 2 {
        return None;
    }
    Some(r[2] != 0)
}

fn query<T>(d: &HidDev, cmd: u8, parse: fn(&[u8]) -> Option<T>) -> Option<T> {
    d.flush();
    if !d.write(&request(cmd)) {
        return None;
    }
    let end = Deadline::in_ms(WAIT_MS);
    while !end.passed() {
        if let Some(v) = d.read(end.left_ms()).and_then(|r| parse(&r)) {
            return Some(v);
        }
    }
    None
}

fn read(c: &HidColl) -> Option<(i32, bool)> {
    let d = HidDev::open(c)?;
    let level = query(&d, CMD_BATTERY, parse_battery)?;
    Some((level, query(&d, CMD_CHARGING, parse_charging).unwrap_or(false)))
}

pub fn poll(ctx: &mut PollContext) {
    for pid in PIDS {
        if ctx.is_claimed(VID, pid) {
            continue;
        }
        let colls = ctx.find(VID, pid, |c| c.iface == 3 && c.page() == 0xFF13 && c.usage() == 0x0001 && c.in_len() >= LEN && c.out_len() >= LEN);
        if colls.is_empty() {
            continue;
        }
        let base = DeviceStatus {
            key: KEY.into(),
            name: "HyperX Cloud II Core Wireless".into(),
            kind: DeviceKind::Headset,
            via: "2.4 GHz dongle".into(),
            hw: Some((VID, pid)),
            grace_sec: 60,
            ..Default::default()
        };
        match colls.iter().find_map(read) {
            Some((level, charging)) => {
                // Both ids can answer at once while charging: one entry, the charging reading wins.
                if let Some(o) = ctx.out.iter_mut().find(|o| o.key == KEY) {
                    if charging || !o.connected {
                        *o = DeviceStatus { percent: level, charging, hw: o.hw, ..base };
                    }
                    ctx.claim(VID, pid);
                } else {
                    ctx.add(DeviceStatus { percent: level, charging, ..base }, VID, pid);
                }
            }
            None if ctx.out.iter().any(|o| o.key == KEY) => ctx.claim(VID, pid),
            None => ctx.add(DeviceStatus { connected: false, ..base }, VID, pid),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(bytes: &[u8]) -> Vec<u8> {
        let mut f = vec![0u8; 62];
        f[..bytes.len()].copy_from_slice(bytes);
        f
    }

    #[test]
    fn captured_replies() {
        // NGENUITY capture in HaloBattery #155: 66 89 0E D7 30 = 48 %, 66 8A 00 00 = off the cable.
        assert_eq!(parse_battery(&frame(&[0x66, 0x89, 0x0E, 0xD7, 0x30])), Some(48));
        assert_eq!(parse_charging(&frame(&[0x66, 0x8A, 0x00, 0x00])), Some(false));
        assert_eq!(parse_battery(&frame(&[0x66, 0x0D, 0x01, 0x00, 0x55])), Some(85));
        assert_eq!(parse_charging(&frame(&[0x66, 0x0C, 0x01])), Some(true));
        assert_eq!(parse_charging(&frame(&[0x66, 0x8A, 0x02])), Some(true));
    }

    #[test]
    fn no_reading() {
        assert_eq!(parse_battery(&request(CMD_BATTERY)), None); // echo / all-zero state
        assert_eq!(parse_battery(&frame(&[0x66, 0x89, 0x0E, 0xD7, 0xFF])), None);
        assert_eq!(parse_charging(&frame(&[0x66, 0x8A, 0x05])), None);
        assert_eq!(parse_battery(&frame(&[0x67, 0x89, 0x0E, 0xD7, 0x30])), None);
        assert_eq!(parse_battery(&frame(&[0x66, 0x8A, 0x0E, 0xD7, 0x30])), None);
    }
}
