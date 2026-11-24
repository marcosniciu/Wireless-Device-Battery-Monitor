//! AULA wireless keyboards (F75, F87 Pro, F99 / F99 Pro, ...) and MCHOSE / Redragon keyboards on the
//! same "BY" 2.4 GHz receivers: 3554:FA09 and 3554:FB40 (Compx, shared by these brands) and
//! MCHOSE's 41E4:2001.
//!
//! Protocol as documented by Device-Battery-Info (github.com/PyFlat/Device-Battery-Info, PR 6),
//! womier-l65-linux (github.com/deepan-alve/womier-l65-linux, windows/l65w.ps1), sanshoku issue 36
//! (github.com/ushineko/sanshoku) and MCHOSE's M HUB web driver, as collected in the original C++
//! BatteryHub (GPL-3.0, src/drivers/aula.cpp); implemented here from those facts. The receiver's
//! vendor collection (usage page FF02 or FF04, usage 0002, 20-byte input and output reports):
//!
//!   request  13 4A PK 00 ... SS       PK 00 / 01 (alternated on resends), SS = sum of bytes 0..18
//!   reply    13 CC .. .. .. LL ST .. SS
//!            CC  4A, possibly with the top bit set
//!            LL  battery level 1..100
//!            ST  high nibble 1 = on the cable / charging (AULA: 01 on battery, 10 with the cable)
//!            SS  sum of bytes 0..18; MCHOSE firmware leaves it 00 in single-packet frames
//!            (byte 2 = 01, byte 3 = 00)
//!
//! The receiver also pushes unrelated 13 frames and can echo the request (all-zero payload); both
//! are skipped. A dozing keyboard leaves the first request unanswered, so it is resent every
//! 1.3 s within one 4 s deadline. The same receiver ids are used by keyboards that do not speak
//! this protocol (the Redragon K673 ignores it, and every request wakes it), so a receiver that has
//! never answered is asked less and less often once it has stayed silent for 5 minutes (5, 10, 20,
//! 40 minutes, then hourly). While a keyboard that has answered sleeps, the app shows it asleep
//! with its last level; it is then asked at most every ASLEEP_QUERY_EVERY.
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::{self, HidColl, HidDev};
use crate::util::{hex4, Deadline};

const RECEIVERS: [(u16, u16); 3] = [(0x3554, 0xFA09), (0x3554, 0xFB40), (0x41E4, 0x2001)];
const REPORT: u8 = 0x13;
const CMD_BATTERY: u8 = 0x4A;
const LEN: usize = 20;
const DEADLINE_MS: u64 = 4000;
const RESEND_MS: u64 = 1300;
const MISSES_BEFORE_BACKOFF: u32 = 3;
const BACKOFF_AFTER: Duration = Duration::from_secs(5 * 60);
const FIRST_BACKOFF: Duration = Duration::from_secs(5 * 60);
const MAX_BACKOFF: Duration = Duration::from_secs(60 * 60);
const ASLEEP_QUERY_EVERY: Duration = Duration::from_secs(60);

fn sum(b: &[u8]) -> u8 {
    b.iter().fold(0u8, |a, &x| a.wrapping_add(x))
}

pub fn request(packets: u8) -> [u8; LEN] {
    let mut r = [0u8; LEN];
    r[0] = REPORT;
    r[1] = CMD_BATTERY;
    r[2] = packets;
    r[LEN - 1] = sum(&r[..LEN - 1]);
    r
}

/// Decodes one input report (report ID included): Some((level, charging)).
pub fn decode(r: &[u8]) -> Option<(i32, bool)> {
    if r.len() < LEN || r[0] != REPORT || r[1] & 0x7F != CMD_BATTERY {
        return None;
    }
    if r[4..LEN - 1].iter().all(|&b| b == 0) {
        return None; // echo of a request
    }
    let checksum = r[LEN - 1] == sum(&r[..LEN - 1]);
    let mchose = r[LEN - 1] == 0 && r[2] == 1 && r[3] == 0;
    if !checksum && !mchose {
        return None;
    }
    let level = r[5];
    if !(1..=100).contains(&level) {
        return None;
    }
    Some((level as i32, r[6] >> 4 == 1))
}

fn query(d: &HidDev) -> Option<(i32, bool)> {
    let end = Deadline::in_ms(DEADLINE_MS);
    let mut sent = 0u8;
    let mut next_send = Deadline::in_ms(0);
    d.flush();
    while !end.passed() {
        if next_send.passed() {
            if !d.write(&request(sent & 1)) {
                return None;
            }
            sent = sent.wrapping_add(1);
            next_send = Deadline::in_ms(RESEND_MS);
        }
        let wait = next_send.left_ms().min(end.left_ms()).max(1);
        let started = Instant::now();
        match d.read(wait) {
            Some(r) => {
                if let Some(v) = decode(&r) {
                    return Some(v);
                }
            }
            // An error returns at once (receiver unplugged); a timeout takes the full wait.
            None if started.elapsed().as_millis() + 20 < wait as u128 => return None,
            None => {}
        }
    }
    None
}

#[derive(Default)]
struct State {
    answered: bool,
    misses: u32,
    backoffs: u32,
    first_miss: Option<Instant>,
    next_try: Option<Instant>,
}

static STATE: Mutex<Option<HashMap<(u16, u16), State>>> = Mutex::new(None);

fn with<R>(id: (u16, u16), f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new).entry(id).or_default())
}

fn is_by_collection(c: &HidColl) -> bool {
    matches!(c.page(), 0xFF02 | 0xFF04) && c.usage() == 0x0002 && c.out_len() as usize >= LEN && c.in_len() as usize >= LEN
}

fn name(c: &HidColl, vid: u16) -> String {
    let p = hid::product_string(c);
    let p = p.trim();
    let generic = p.is_empty() || p.contains("2.4G Wireless") || p.to_ascii_lowercase().contains("receiver");
    if !generic {
        p.to_string()
    } else if vid == 0x41E4 || p.contains("MCHOSE") {
        "MCHOSE wireless keyboard".into()
    } else {
        "AULA wireless keyboard".into()
    }
}

pub fn poll(ctx: &mut PollContext) {
    for (vid, pid) in RECEIVERS {
        let colls = ctx.find(vid, pid, is_by_collection);
        if !ctx.colls.iter().any(|c| c.vid == vid && c.pid == pid) {
            with((vid, pid), |s| *s = State::default()); // unplugged: start over
            continue;
        }
        if colls.is_empty() || ctx.is_claimed(vid, pid) {
            continue;
        }
        if with((vid, pid), |s| s.next_try.is_some_and(|t| Instant::now() < t)) {
            continue;
        }
        let found = colls.iter().find_map(|c| HidDev::open(c).and_then(|d| query(&d)).map(|v| (c, v)));
        let now = Instant::now();
        let Some((c, (level, charging))) = found else {
            with((vid, pid), |s| {
                if s.answered {
                    s.next_try = Some(now + ASLEEP_QUERY_EVERY);
                    return;
                }
                s.misses += 1;
                let first = *s.first_miss.get_or_insert(now);
                if s.misses >= MISSES_BEFORE_BACKOFF && now - first >= BACKOFF_AFTER {
                    let doublings = s.backoffs.min(4);
                    s.backoffs += 1;
                    s.next_try = Some(now + (FIRST_BACKOFF * (1 << doublings)).min(MAX_BACKOFF));
                }
            });
            // Not listed: a keyboard that answered before is shown asleep by the app.
            continue;
        };
        with((vid, pid), |s| *s = State { answered: true, ..Default::default() });
        let s = DeviceStatus {
            key: format!("aula:{}:{}", hex4(vid), hex4(pid)),
            name: name(c, vid),
            kind: DeviceKind::Keyboard,
            via: "2.4 GHz receiver".into(),
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

    #[test]
    fn request_frame() {
        let r = request(0);
        assert_eq!(&r[..3], &[0x13, 0x4A, 0x00]);
        assert_eq!(r[19], 0x5D);
        assert_eq!(request(1)[19], 0x5E);
    }

    fn reply(cmd: u8, b2: u8, b3: u8, level: u8, state: u8, with_sum: bool) -> Vec<u8> {
        let mut r = vec![0u8; LEN];
        r[0] = 0x13;
        r[1] = cmd;
        r[2] = b2;
        r[3] = b3;
        r[5] = level;
        r[6] = state;
        if with_sum {
            r[19] = sum(&r[..19]);
        }
        r
    }

    #[test]
    fn replies() {
        // AULA: 01 on battery, 10 with the cable in.
        assert_eq!(decode(&reply(0x4A, 0x00, 0x00, 76, 0x01, true)), Some((76, false)));
        assert_eq!(decode(&reply(0xCA, 0x00, 0x00, 54, 0x10, true)), Some((54, true)));
        // MCHOSE single-packet frame with the trailer left at 0.
        assert_eq!(decode(&reply(0x4A, 0x01, 0x00, 90, 0x00, false)), Some((90, false)));
    }

    #[test]
    fn not_replies() {
        assert_eq!(decode(&request(0)), None); // echo
        assert_eq!(decode(&reply(0x0A, 0x00, 0x00, 50, 0x01, true)), None); // pushed status frame
        assert_eq!(decode(&reply(0x4A, 0x00, 0x00, 50, 0x01, false)), None); // bad checksum
        assert_eq!(decode(&reply(0x4A, 0x00, 0x00, 0, 0x01, true)), None); // no level
        assert_eq!(decode(&reply(0x4A, 0x00, 0x00, 150, 0x01, true)), None);
        assert_eq!(decode(&[0x13, 0x4A, 0x00]), None);
    }
}
