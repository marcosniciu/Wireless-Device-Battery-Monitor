//! Evision keyboards on the shared 2.4 GHz receiver (320F:5088) or USB cable (320F:5055): the VGN /
//! VXE keyboards VGN HUB drives this way (S99, V98 PRO, V98pro V2, N75 PRO, VXE75, VXE V87), the ATK
//! V75X and keyboards sold on the same receiver as "Wireless Gaming Keyboard" (Feker Alice80).
//!
//! Protocol as documented by the original C++ BatteryHub (GPL-3.0, src/drivers/atk_keyboard.cpp):
//! taken from HaloBattery (MIT, github.com/HeyOkay/HaloBattery, providers/atk_keyboard.py, from ATK
//! HUB's Evision keyboard driver, confirmed by its author on the receiver and the cable) and from VGN
//! HUB's getElectricQuantity(); a VGN receiver read 93 % with it (VGN HUB showed 94 %, per
//! myfinder-device-battery's upstream-protocols.md) and a Feker Alice80 answered the same way.
//! Implemented here from those facts. Vendor collection FF1C:0092, report 04:
//!
//!   request  04 SL SH 1A VV 00 ...          SL SH = sequence (LE), VV = 06 for VGN models, else 00
//!   reply    04 SL SH 1A .. 00 00 .. PP ST   PP = level (0..100), ST = 0 on battery, 1/2 charging
//!
//! The reply echoes the sequence and arrives up to ~1 s later. The ids are shared by many Evision
//! keyboards, so only the product names above are asked. Over the receiver the keyboard is asked at
//! most every RECEIVER_EVERY (as the original does) so it is not kept awake; on the cable, every poll.
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::{self, HidColl, HidDev};
use crate::util::Deadline;

const VID: u16 = 0x320F;
const RECEIVER: u16 = 0x5088;
const CABLE: u16 = 0x5055;
const REPORT: u8 = 0x04;
const CMD_BATTERY: u8 = 0x1A;
const REPLY_MS: u64 = 2500;
const RECEIVER_EVERY: Duration = Duration::from_secs(10 * 60);
/// No reply over the receiver (keyboard asleep or off): ask again after this.
const SILENT_EVERY: Duration = Duration::from_secs(2 * 60);

/// Product strings (receiver and cable) and whether VGN HUB's variant of the request is used.
const MODELS: [(&str, bool); 17] = [
    ("ATK V75X 2.4G Dongle", false),
    ("ATK V75X", false),
    ("VGN S99 2.4G Dongle", true),
    ("VGN S99", true),
    ("VGN V98pro V2 2.4G Dongle", true),
    ("VGN V98pro V2", true),
    ("VGN V98 PRO 2.4G Dongle", true),
    ("VGN V98pro", true),
    ("VGN N75 PRO 2.4G Dongle", true),
    ("VGN N75 PRO", true),
    ("VGN VXE75 V2 2.4G Dongle", true),
    ("VGN VXE75 V2", true),
    ("VGN VXE75 2.4G Dongle", true),
    ("VGN VXE75", true),
    ("VXE V87 2.4G Dongle", true),
    ("VXE V87", true),
    ("VXE V87 RGB", true),
];
const GENERIC_NAME: &str = "Wireless Gaming Keyboard";

/// (keyboard name, VGN request) for a product string, None when it is not a known model.
pub fn model(product: &str) -> Option<(String, bool)> {
    let p = product.trim();
    if p.eq_ignore_ascii_case(GENERIC_NAME) {
        return Some((GENERIC_NAME.into(), false));
    }
    let &(name, vgn) = MODELS.iter().find(|(n, _)| n.eq_ignore_ascii_case(p))?;
    let name = name.strip_suffix(" 2.4G Dongle").unwrap_or(name);
    Some((name.to_string(), vgn))
}

pub fn request(seq: u16, vgn: bool) -> [u8; 32] {
    let mut r = [0u8; 32];
    r[0] = REPORT;
    r[1..3].copy_from_slice(&seq.to_le_bytes());
    r[3] = CMD_BATTERY;
    r[4] = if vgn { 0x06 } else { 0x00 };
    r
}

/// Some(Some((level, charging))) for the reply to `seq`, Some(None) for a reply with an invalid
/// value, None for any other report (report id included).
pub fn parse(r: &[u8], seq: u16) -> Option<Option<(i32, bool)>> {
    let [lo, hi] = seq.to_le_bytes();
    if r.len() < 10 || r[0] != REPORT || r[1] != lo || r[2] != hi || r[3] != CMD_BATTERY || r[5] != 0 || r[6] != 0 {
        return None;
    }
    Some((r[8] <= 100 && r[9] <= 2).then(|| (r[8] as i32, r[9] != 0)))
}

struct State {
    seq: u16,
    next_ask: Option<Instant>,
    last: Option<(i32, bool)>,
    answered_once: bool,
}

static STATE: Mutex<State> = Mutex::new(State { seq: 0, next_ask: None, last: None, answered_once: false });

fn read(c: &HidColl, seq: u16, vgn: bool) -> Option<(i32, bool)> {
    let d = HidDev::open(c)?;
    d.flush();
    if !d.write(&request(seq, vgn)) {
        return None;
    }
    let end = Deadline::in_ms(REPLY_MS);
    while !end.passed() {
        if let Some(v) = d.read(end.left_ms()).and_then(|r| parse(&r, seq)) {
            return v;
        }
    }
    None
}

pub fn poll(ctx: &mut PollContext) {
    for pid in [CABLE, RECEIVER] {
        // The cable wins when both are connected.
        if ctx.is_claimed(VID, pid) {
            continue;
        }
        let cable = pid == CABLE;
        let colls = ctx.find(VID, pid, |c| c.page() == 0xFF1C && c.usage() == 0x0092 && c.out_len() >= 10 && c.in_len() >= 10);
        let Some((c, (name, vgn))) = colls.iter().find_map(|c| model(&hid::product_string(c)).map(|m| (c, m))) else {
            continue;
        };
        let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if cable || st.next_ask.map_or(true, |t| now >= t) {
            st.seq = st.seq % 0xFFFF + 1;
            let got = read(c, st.seq, vgn);
            st.next_ask = Some(now + if got.is_some() || cable { RECEIVER_EVERY } else { SILENT_EVERY });
            if got.is_some() {
                st.answered_once = true;
            }
            st.last = got;
        }
        let (reading, answered_once) = (st.last, st.answered_once);
        drop(st);
        let mut s = DeviceStatus {
            key: "evision:5088".into(), // one entry for the keyboard on either connection
            name,
            kind: DeviceKind::Keyboard,
            via: if cable { "USB cable" } else { "2.4 GHz receiver" }.into(),
            hw: Some((VID, pid)),
            grace_sec: 1800,
            ..Default::default()
        };
        match reading {
            Some((level, charging)) => {
                s.percent = level;
                s.charging = charging;
            }
            // Never answered: receiver present, keyboard off. Answered before: asleep, the app keeps
            // the last level.
            None if !answered_once => s.connected = false,
            None => {
                ctx.claim(VID, pid);
                if cable {
                    ctx.claim(VID, RECEIVER);
                }
                return;
            }
        }
        ctx.add(s, VID, pid);
        if cable {
            ctx.claim(VID, RECEIVER);
        }
        return;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models() {
        assert_eq!(model("VGN V98 PRO 2.4G Dongle "), Some(("VGN V98 PRO".into(), true)));
        assert_eq!(model("vgn v98pro"), Some(("VGN V98pro".into(), true)));
        assert_eq!(model("ATK V75X 2.4G Dongle"), Some(("ATK V75X".into(), false)));
        assert_eq!(model("Wireless Gaming Keyboard"), Some(("Wireless Gaming Keyboard".into(), false)));
        assert_eq!(model("Some Other Dongle"), None);
    }

    #[test]
    fn request_frames() {
        let r = request(0x0102, true);
        assert_eq!(&r[..6], &[0x04, 0x02, 0x01, 0x1A, 0x06, 0x00]);
        assert!(r[5..].iter().all(|&b| b == 0));
        assert_eq!(request(7, false)[4], 0x00);
    }

    #[test]
    fn replies() {
        // 93 % on battery, as read on a VGN receiver.
        let mut r = [0u8; 64];
        r[..10].copy_from_slice(&[0x04, 0x05, 0x00, 0x1A, 0x06, 0, 0, 0, 93, 0]);
        assert_eq!(parse(&r, 5), Some(Some((93, false))));
        r[9] = 1;
        assert_eq!(parse(&r, 5), Some(Some((93, true))));
        r[9] = 2;
        assert_eq!(parse(&r, 5), Some(Some((93, true))));
        r[8] = 101;
        assert_eq!(parse(&r, 5), Some(None));
    }

    #[test]
    fn not_replies() {
        let r = [0x04, 0x05, 0x00, 0x1A, 0x06, 0, 0, 0, 50, 0];
        assert_eq!(parse(&r, 6), None); // another sequence
        let mut other = r;
        other[3] = 0x1B;
        assert_eq!(parse(&other, 5), None); // another command
        let mut busy = r;
        busy[5] = 1;
        assert_eq!(parse(&busy, 5), None);
        assert_eq!(parse(&[0x05, 0x05, 0x00, 0x1A, 0, 0, 0, 0, 50, 0], 5), None);
        assert_eq!(parse(&r[..9], 5), None);
    }
}
