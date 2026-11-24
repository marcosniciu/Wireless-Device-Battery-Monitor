//! More mice on CompX's "report 8" protocol (see compx.rs, whose frame helpers are reused here):
//!
//! * Scyrox V6 / V8 on the "SCYROX 8K Dongle" 3554:F5F7 and on their USB cable 3554:F5F6. Both mice
//!   share the receiver, which does not say which one is linked: shown as "Scyrox V6 / V8".
//!   Ids and protocol from HaloBattery (MIT, github.com/HeyOkay/HaloBattery, providers/pulsar.py,
//!   measured on hardware in HaloBattery issue #182), MouseDock (MIT,
//!   github.com/late-autumn-gif/mousedock, docs/SCYROX-V6-V8-LEGACY.md, read off Scyrox's official
//!   S-Center web driver) and BrimDeck (Apache-2.0, github.com/MoFeng2223/BrimDeck,
//!   src/BrimDeck.Core/DeviceProtocols.cs).
//! * ATK mice on the "ATK Mouse 8K Dongle" 373B:101B (ATK Fire X 8K and other ATK 8K mice). Listed on
//!   the same frame by HaloBattery (providers/pulsar.py, CHANGELOG) and BrimDeck (ATK receivers);
//!   its collections are the CompX family's (FF02:0002 plus FF03..FF06).
//!
//! Implemented here from those facts. On interface 1, vendor collection FF02:0002 (17-byte input and
//! output report 08):
//!
//!   request  08 CC 00 ... 00 SS          SS (byte 16) = 0x55 - sum(bytes 0..15)
//!   cmd 03   reply [6] = 1 when the mouse is linked to the receiver, 0 when it is not
//!   cmd 04   reply [6] = level 0..100, [7] = 1 on USB power (charging), [8..9] = mV (big endian)
//!
//! Only these two commands are sent (read-only; no pairing, DPI or firmware command). Our own request
//! read back is not a reply. A receiver whose mouse is off, asleep or out of range is shown as
//! disconnected.
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::drivers::compx::{parse_power, parse_status, request, CMD_POWER, CMD_STATUS};
use crate::hid::{HidColl, HidDev};
use crate::util::Deadline;

const LEN: u16 = 17;
const REPLY_MS: u64 = 300;
/// Nothing answered at all: ask again only after this (the entry stays, disconnected).
const SILENT_QUERY_EVERY: Duration = Duration::from_secs(60);

struct Product {
    vid: u16,
    pid: u16,
    name: &'static str,
    /// Receiver and cable of one mouse share the key, so a charging mouse is listed once.
    key: &'static str,
    cable: bool,
}

const PRODUCTS: [Product; 3] = [
    Product { vid: 0x3554, pid: 0xF5F7, name: "Scyrox V6 / V8", key: "scyrox:v6v8", cable: false },
    Product { vid: 0x3554, pid: 0xF5F6, name: "Scyrox V6 / V8", key: "scyrox:v6v8", cable: true },
    Product { vid: 0x373B, pid: 0x101B, name: "ATK 8K mouse", key: "compx:373b:101b", cable: false },
];

#[derive(Debug, PartialEq, Eq)]
pub enum Reading {
    /// Level, on USB power, millivolts.
    Power(i32, bool, i32),
    /// The receiver answered, but no mouse is linked or the mouse did not answer.
    Away,
}

/// Combines the replies to cmd 03 (linked) and cmd 04 (power); None when nothing valid came back.
pub fn combine(linked: Option<bool>, power: Option<(i32, bool, i32)>) -> Option<Reading> {
    if linked == Some(false) {
        return Some(Reading::Away);
    }
    match power {
        Some((level, on_power, mv)) => Some(Reading::Power(level, on_power, mv)),
        None => linked.map(|_| Reading::Away),
    }
}

/// Parses one input report as the reply to `cmd`; our own request read back is not a reply.
pub fn parse_reply<T>(cmd: u8, r: &[u8], parse: fn(&[u8]) -> Option<T>) -> Option<T> {
    if r.len() >= LEN as usize && r[..LEN as usize] == request(cmd) {
        return None;
    }
    parse(r)
}

fn query<T>(d: &HidDev, cmd: u8, parse: fn(&[u8]) -> Option<T>) -> Option<T> {
    d.flush();
    if !d.write(&request(cmd)) {
        return None;
    }
    let end = Deadline::in_ms(REPLY_MS);
    while !end.passed() {
        if let Some(v) = d.read(end.left_ms()).and_then(|r| parse_reply(cmd, &r, parse)) {
            return Some(v);
        }
    }
    None
}

fn read(c: &HidColl) -> Option<Reading> {
    let d = HidDev::open(c)?;
    let linked = query(&d, CMD_STATUS, parse_status);
    if linked == Some(false) {
        return Some(Reading::Away);
    }
    combine(linked, query(&d, CMD_POWER, parse_power))
}

static NEXT_TRY: Mutex<Option<HashMap<(u16, u16), Instant>>> = Mutex::new(None);

pub fn poll(ctx: &mut PollContext) {
    for p in &PRODUCTS {
        let (vid, pid) = (p.vid, p.pid);
        if ctx.is_claimed(vid, pid) {
            continue;
        }
        let colls = ctx.find(vid, pid, |c| c.iface == 1 && c.page() == 0xFF02 && c.usage() == 0x0002 && c.in_len() == LEN && c.out_len() == LEN);
        if colls.is_empty() {
            continue;
        }
        let due = {
            let mut g = NEXT_TRY.lock().unwrap_or_else(|e| e.into_inner());
            g.get_or_insert_with(HashMap::new).get(&(vid, pid)).map_or(true, |t| Instant::now() >= *t)
        };
        let reading = if due { colls.iter().find_map(read) } else { None };
        if due {
            let mut g = NEXT_TRY.lock().unwrap_or_else(|e| e.into_inner());
            let m = g.get_or_insert_with(HashMap::new);
            if reading.is_none() {
                m.insert((vid, pid), Instant::now() + SILENT_QUERY_EVERY);
            } else {
                m.remove(&(vid, pid));
            }
        }
        let base = DeviceStatus {
            key: p.key.into(),
            name: p.name.into(),
            kind: DeviceKind::Mouse,
            via: if p.cable { "USB cable" } else { "2.4 GHz receiver" }.into(),
            hw: Some((vid, pid)),
            grace_sec: 1800,
            ..Default::default()
        };
        match reading {
            Some(Reading::Power(level, on_power, mv)) => {
                let charging = on_power || p.cable;
                // Receiver and cable both present (charging): one entry.
                if let Some(o) = ctx.out.iter_mut().find(|o| o.key == p.key) {
                    *o = DeviceStatus { percent: level, charging, millivolts: mv, hw: o.hw, via: o.via.clone(), ..base };
                    ctx.claim(vid, pid);
                } else {
                    ctx.add(DeviceStatus { percent: level, charging, millivolts: mv, ..base }, vid, pid);
                }
            }
            _ if p.cable => ctx.claim(vid, pid), // the cable alone does not answer: nothing to show
            _ => {
                if ctx.out.iter().any(|o| o.key == p.key) {
                    ctx.claim(vid, pid);
                } else {
                    ctx.add(DeviceStatus { connected: false, ..base }, vid, pid);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::compx::checksum;

    fn reply(bytes: &[u8]) -> Vec<u8> {
        let mut f = vec![0u8; 17];
        f[..bytes.len()].copy_from_slice(bytes);
        f[16] = checksum(&f);
        f
    }

    #[test]
    fn requests() {
        // 08 03 ... 4A and 08 04 ... 49: the whole frame sums to 0x55.
        assert_eq!(request(CMD_STATUS)[16], 0x4A);
        assert_eq!(request(CMD_POWER)[16], 0x49);
    }

    #[test]
    fn power_reply() {
        // HaloBattery's F5F7 test: 90 %, on battery, 4076 mV (0x0FEC).
        let r = reply(&[0x08, 0x04, 0, 0, 0, 0, 90, 0, 0x0F, 0xEC]);
        assert_eq!(combine(Some(true), parse_power(&r)), Some(Reading::Power(90, false, 4076)));
        let r = reply(&[0x08, 0x04, 0, 0, 0, 0, 63, 1, 0x10, 0x04]);
        assert_eq!(combine(Some(true), parse_power(&r)), Some(Reading::Power(63, true, 4100)));
    }

    #[test]
    fn mouse_away() {
        let off = reply(&[0x08, 0x03, 0, 0, 0, 0, 0]);
        assert_eq!(combine(parse_status(&off), None), Some(Reading::Away));
        let on = reply(&[0x08, 0x03, 0, 0, 0, 0, 1]);
        assert_eq!(combine(parse_status(&on), None), Some(Reading::Away));
        assert_eq!(combine(None, None), None);
    }

    #[test]
    fn rejected_replies() {
        let mut bad = reply(&[0x08, 0x04, 0, 0, 0, 0, 90]);
        bad[16] ^= 1; // checksum
        assert_eq!(parse_power(&bad), None);
        assert_eq!(parse_power(&reply(&[0x08, 0x04, 0, 0, 0, 0, 101])), None); // level
        assert_eq!(parse_power(&reply(&[0x08, 0x03, 0, 0, 0, 0, 90])), None); // other command
    }

    #[test]
    fn echo_is_not_a_reply() {
        // Our own request read back must not count as "0 %" or "not linked".
        assert_eq!(parse_reply(CMD_POWER, &request(CMD_POWER), parse_power), None);
        assert_eq!(parse_reply(CMD_STATUS, &request(CMD_STATUS), parse_status), None);
        let r = reply(&[0x08, 0x04, 0, 0, 0, 0, 90, 0, 0x0F, 0xEC]);
        assert_eq!(parse_reply(CMD_POWER, &r, parse_power), Some((90, false, 4076)));
    }

    #[test]
    fn only_listed_ids() {
        assert!(PRODUCTS.iter().all(|p| !(p.vid == 0x3554 && (p.pid == 0xFA09 || p.pid == 0xFB40))));
    }
}
