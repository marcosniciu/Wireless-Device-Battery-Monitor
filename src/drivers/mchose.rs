//! MCHOSE wireless mice (K7 / K7 Ultra, L7, M7, A7 families) on their 2.4 GHz receiver or USB cable
//! (VID 5253; several models share a receiver PID, so the name comes from the product string).
//!
//! Protocol as documented by mchose-linux (github.com/alexfrih/mchose-linux, PROTOCOL.md, recovered
//! from MCHOSE's M HUB driver) and the original C++ BatteryHub (GPL-3.0, src/drivers/mchose.cpp,
//! verified on an L7 Ultra 5253:1020); implemented here from those facts. On the vendor collection
//! (usage page FF01, usage 0001, 65-byte feature report 12) every payload byte is inverted (^ FF),
//! padding included:
//!
//!   request  set feature  12 F9 FF FF ...         (command 06 = status)
//!   reply    get feature  12 F9 <payload ^ FF>
//!            payload: vid u16 LE (5253), model u16, firmware u32, flags u8 (09 receiver,
//!            00 cable), level u8 (0..100), charging u8
//!
//! While the mouse sleeps the receiver echoes the command with an all-zero payload: the mouse is
//! then reported asleep (the app keeps its last level) and asked again after ASLEEP_QUERY_EVERY.
//! The request has to be sent again for every read attempt.
//!
//! The same protocol runs on MCHOSE's Realtek ODM receivers with VID 3837 (original C++ BatteryHub,
//! mchose.cpp: A5 V2 Ultra 3837:100C verified there, K7 Ultra 3837:1001, A7 V2 Ultra+ 3837:100B,
//! 100A, 1012). Only those product ids are asked: other 3837 products (G3 V2, keyboards, headsets)
//! speak other protocols. Their vid field is 41E4 (100C) or 3837 (1001).
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::{self, HidColl, HidDev};
use crate::util::{hex4, sleep_ms};

const VID: u16 = 0x5253;
const VID_REALTEK: u16 = 0x3837;
const REALTEK_PIDS: [u16; 5] = [0x100C, 0x100B, 0x100A, 0x1001, 0x1012];
const REALTEK_FIELDS: [u16; 2] = [0x41E4, VID_REALTEK];
const REPORT: u8 = 0x12;
const CMD_STATUS: u8 = 0x06;
const ATTEMPTS: usize = 5;
const ASLEEP_QUERY_EVERY: Duration = Duration::from_secs(60);

#[derive(Debug, PartialEq, Eq)]
pub enum Status {
    /// level, charging, on the cable
    Reading(i32, bool, bool),
    Asleep,
}

pub fn request(len: usize) -> Vec<u8> {
    let mut r = vec![0xFF; len.max(2)];
    r[0] = REPORT;
    r[1] = CMD_STATUS ^ 0xFF;
    r
}

/// Decodes a feature report read back (report ID included).
pub fn decode(r: &[u8]) -> Option<Status> {
    decode_fields(r, &[VID])
}

/// Like decode, accepting any of `fields` as the payload's vid field.
pub fn decode_fields(r: &[u8], fields: &[u16]) -> Option<Status> {
    if r.len() < 13 || r[0] != REPORT || r[1] != CMD_STATUS ^ 0xFF {
        return None;
    }
    let p: Vec<u8> = r[2..13].iter().map(|b| b ^ 0xFF).collect();
    if p.iter().all(|&b| b == 0) {
        return Some(Status::Asleep);
    }
    if !fields.contains(&u16::from_le_bytes([p[0], p[1]])) || p[9] > 100 {
        return None;
    }
    let charging = p[10] != 0;
    Some(Status::Reading(p[9] as i32, charging, p[8] == 0 && charging))
}

fn query(c: &HidColl, fields: &[u16]) -> Option<Status> {
    let d = HidDev::open_feature_only(c)?;
    let req = request(c.feat_len() as usize);
    let mut best = None;
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            sleep_ms(100);
        }
        if !d.set_feature(&req) {
            break;
        }
        sleep_ms(120);
        match d.get_feature(REPORT).and_then(|r| decode_fields(&r, fields)) {
            Some(s @ Status::Reading(..)) => return Some(s),
            Some(Status::Asleep) => best = Some(Status::Asleep),
            None => {}
        }
    }
    best
}

static NEXT_TRY: Mutex<Option<HashMap<(u16, u16), Instant>>> = Mutex::new(None);

pub fn poll(ctx: &mut PollContext) {
    let mut products: Vec<(u16, u16)> = ctx.products(VID);
    products.extend(REALTEK_PIDS.iter().map(|&p| (VID_REALTEK, p)).filter(|&(v, p)| !ctx.is_claimed(v, p)));
    for (vid, pid) in products {
        let fields: &[u16] = if vid == VID_REALTEK { &REALTEK_FIELDS } else { &[VID] };
        let colls = ctx.find(vid, pid, |c| c.page() == 0xFF01 && c.usage() == 0x0001 && c.feat_len() >= 65);
        if colls.is_empty() {
            continue;
        }
        let due = {
            let mut g = NEXT_TRY.lock().unwrap_or_else(|e| e.into_inner());
            g.get_or_insert_with(HashMap::new).get(&(vid, pid)).map_or(true, |t| Instant::now() >= *t)
        };
        if !due {
            continue; // asleep: the app keeps showing the last level
        }
        let Some((c, status)) = colls.iter().find_map(|c| query(c, fields).map(|s| (c, s))) else { continue };
        {
            let mut g = NEXT_TRY.lock().unwrap_or_else(|e| e.into_inner());
            let m = g.get_or_insert_with(HashMap::new);
            if status == Status::Asleep {
                m.insert((vid, pid), Instant::now() + ASLEEP_QUERY_EVERY);
            } else {
                m.remove(&(vid, pid));
            }
        }
        let Status::Reading(level, charging, cable) = status else { continue };
        let name = hid::product_string(c);
        let name = name.trim();
        let s = DeviceStatus {
            key: if vid == VID { format!("mchose:{}", hex4(pid)) } else { format!("mchose:{}:{}", hex4(vid), hex4(pid)) },
            name: if name.is_empty() { "MCHOSE mouse".into() } else { name.to_string() },
            kind: DeviceKind::Mouse,
            via: if cable { "USB cable" } else { "2.4 GHz receiver" }.into(),
            percent: level,
            charging,
            hw: Some((vid, pid)),
            grace_sec: 1800,
            ..Default::default()
        };
        ctx.add(s, vid, pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(payload: &[u8]) -> Vec<u8> {
        let mut r = vec![REPORT, 0xF9];
        r.extend(payload.iter().map(|b| b ^ 0xFF));
        r.resize(65, 0xFF);
        r
    }

    #[test]
    fn request_frame() {
        let r = request(65);
        assert_eq!(r.len(), 65);
        assert_eq!(&r[..3], &[0x12, 0xF9, 0xFF]);
        assert!(r[2..].iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn readings() {
        // vid 5253, model, firmware, flags 09 (receiver), level 72, not charging.
        let rx = [0x53, 0x52, 0x20, 0x10, 0x01, 0x02, 0x03, 0x04, 0x09, 72, 0];
        assert_eq!(decode(&reply(&rx)), Some(Status::Reading(72, false, false)));
        // On the cable: flags 00, charging.
        let cable = [0x53, 0x52, 0x20, 0x10, 0x01, 0x02, 0x03, 0x04, 0x00, 95, 1];
        assert_eq!(decode(&reply(&cable)), Some(Status::Reading(95, true, true)));
    }

    #[test]
    fn asleep_and_invalid() {
        assert_eq!(decode(&reply(&[0; 11])), Some(Status::Asleep));
        // Wrong vid field, level out of range, the request read back, another report.
        assert_eq!(decode(&reply(&[0x37, 0x38, 0, 0, 0, 0, 0, 0, 0x09, 50, 0])), None);
        assert_eq!(decode(&reply(&[0x53, 0x52, 0, 0, 0, 0, 0, 0, 0x09, 150, 0])), None);
        assert_eq!(decode(&[0x13, 0xF9, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]), None);
        assert_eq!(decode(&[0x12, 0xF9]), None);
    }

    #[test]
    fn realtek_receivers() {
        // 3837 receivers: vid field 3837 (K7 Ultra 1001) or 41E4 (A5 V2 Ultra 100C).
        let k7 = [0x37, 0x38, 0x01, 0x10, 0, 0, 0, 0, 0x09, 58, 0];
        assert_eq!(decode_fields(&reply(&k7), &REALTEK_FIELDS), Some(Status::Reading(58, false, false)));
        let a5 = [0xE4, 0x41, 0x0C, 0x10, 0, 0, 0, 0, 0x00, 80, 1];
        assert_eq!(decode_fields(&reply(&a5), &REALTEK_FIELDS), Some(Status::Reading(80, true, true)));
        assert_eq!(decode_fields(&reply(&[0; 11]), &REALTEK_FIELDS), Some(Status::Asleep));
        // A 5253 payload is not taken from a 3837 receiver, nor a 3837 one from a 5253 receiver.
        assert_eq!(decode_fields(&reply(&[0x53, 0x52, 0, 0, 0, 0, 0, 0, 0x09, 50, 0]), &REALTEK_FIELDS), None);
        assert_eq!(decode(&reply(&k7)), None);
    }
}
