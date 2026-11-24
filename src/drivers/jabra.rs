//! Jabra headsets behind a Jabra Link 390 dongle (VID 0B0E; PIDs 2E50 USB-A UC, 2E51 USB-A MS,
//! 2E56 USB-C UC, 2E57 USB-C MS).
//!
//! Jabra's GN Protocol (GNP) as documented by HeadsetControl (GPL-3.0, github.com/Sapd/HeadsetControl,
//! lib/devices/protocols/jabra_gnp_protocol.hpp and lib/devices/jabra_link_390.hpp, verified with an
//! Evolve3 75 on a Link 390 0B0E:2E51 on Windows) and as used by the original C++ BatteryHub
//! (GPL-3.0, src/drivers/jabra.cpp, verified on the same pair); implemented here from those facts.
//! Frames are 64 bytes, report 05, on the vendor collection FF00:0005 (interface 3):
//!
//!   query  05 DD 00 QQ 46 CC OO               DD = 04 (the headset behind the dongle), QQ = sequence,
//!                                             46 = GET (1 << 6) | 6 bytes from DD to OO
//!   reply  05 00 DD QQ LL CC OO PP ...        LL = C0 | bytes from byte 1 to the end of the payload
//!          05 00 DD QQ LL FE RR               NAK, RR = reason (F3 headset offline, F4 / F5 no device)
//!
//!   12 02  battery  payload [flags][level %], flags bit 0 = charging
//!   02 00  name     payload [n][n bytes UTF-8]
//!
//! Only these two GETs are sent; nothing is written. A NAK for "offline" or no reply at all means the
//! headset is off or out of range while the dongle stays plugged in: shown as disconnected.
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;

use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::{HidColl, HidDev};
use crate::util::{hex4, Deadline};

const VID: u16 = 0x0B0E;
const LINK_390: [u16; 4] = [0x2E50, 0x2E51, 0x2E56, 0x2E57];
const REPORT: u8 = 0x05;
const HOST: u8 = 0x00;
/// GNP address of the headset paired to a Link dongle.
const HEADSET: u8 = 0x04;
const TYPE_GET: u8 = 1;
const TYPE_RESPONSE: u8 = 3;
const NAK: u8 = 0xFE;
const STATUS: u8 = 0x12;
const HS_BATTERY: u8 = 0x02;
const IDENT: u8 = 0x02;
const IDENT_NAME: u8 = 0x00;
const WAIT_MS: u64 = 700;

#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    Ok(Vec<u8>),
    Nak(u8),
}

pub fn request(seq: u8, cmd: u8, sub: u8) -> [u8; 64] {
    let mut f = [0u8; 64];
    f[..7].copy_from_slice(&[REPORT, HEADSET, HOST, seq, (TYPE_GET << 6) | 6, cmd, sub]);
    f
}

/// Decodes one input report (report ID included) as the reply to query `seq` / `cmd` / `sub`.
pub fn decode(r: &[u8], seq: u8, cmd: u8, sub: u8) -> Option<Reply> {
    if r.len() < 7 || r[0] != REPORT || r[1] != HOST || r[2] != HEADSET || r[3] != seq || r[4] >> 6 != TYPE_RESPONSE {
        return None;
    }
    if r[5] == NAK {
        return Some(Reply::Nak(r[6]));
    }
    let len = (r[4] & 0x3F) as usize; // counts from byte 1
    if r[5] != cmd || r[6] != sub || len < 6 || len + 1 > r.len() {
        return None;
    }
    Some(Reply::Ok(r[7..len + 1].to_vec()))
}

/// Battery payload: Some((level, charging)); level -1 when above 100.
pub fn parse_battery(p: &[u8]) -> Option<(i32, bool)> {
    let (&flags, &level) = (p.first()?, p.get(1)?);
    Some((if level <= 100 { level as i32 } else { -1 }, flags & 0x01 != 0))
}

/// Name payload: [n][n bytes UTF-8] (HeadsetControl also accepts a bare string).
pub fn parse_name(p: &[u8]) -> Option<String> {
    let text = match p.first() {
        Some(&n) if n as usize + 1 <= p.len() && n > 0 => &p[1..n as usize + 1],
        _ => p,
    };
    let s = String::from_utf8_lossy(text).split('\0').next().unwrap_or("").trim().to_string();
    (!s.is_empty() && s.chars().all(|c| !c.is_control())).then_some(s)
}

static SEQ: AtomicU8 = AtomicU8::new(0x20);

fn query(d: &HidDev, cmd: u8, sub: u8) -> Option<Reply> {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    d.flush();
    if !d.write_timeout(&request(seq, cmd, sub), 300) {
        return None;
    }
    let end = Deadline::in_ms(WAIT_MS);
    while !end.passed() {
        if let Some(rep) = d.read(end.left_ms()).and_then(|r| decode(&r, seq, cmd, sub)) {
            return Some(rep);
        }
    }
    None
}

/// Headset name per dongle, asked once while the headset is on; forgotten when it goes off, so a
/// newly paired headset is picked up.
static NAMES: Mutex<Option<HashMap<u16, String>>> = Mutex::new(None);

fn read(c: &HidColl) -> Option<(Option<(i32, bool)>, Option<String>)> {
    let d = HidDev::open(c)?;
    match query(&d, STATUS, HS_BATTERY)? {
        Reply::Ok(p) => {
            let battery = parse_battery(&p)?;
            let known = NAMES.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&c.pid).cloned());
            let name = known.or_else(|| match query(&d, IDENT, IDENT_NAME) {
                Some(Reply::Ok(p)) => Some(parse_name(&p).unwrap_or_else(|| "Jabra headset".into())),
                _ => None,
            });
            Some((Some(battery), name))
        }
        Reply::Nak(_) => Some((None, None)), // headset off, out of range or not paired
    }
}

pub fn poll(ctx: &mut PollContext) {
    for (vid, pid) in ctx.products(VID) {
        if !LINK_390.contains(&pid) {
            continue;
        }
        let colls = ctx.find(vid, pid, |c| c.page() == 0xFF00 && c.usage() == 0x0005 && c.in_len() >= 64 && c.out_len() >= 64);
        if colls.is_empty() {
            continue;
        }
        let reading = colls.iter().find_map(read);
        let mut names = NAMES.lock().unwrap_or_else(|e| e.into_inner());
        let names = names.get_or_insert_with(HashMap::new);
        let base = DeviceStatus {
            key: format!("jabra:{}", hex4(pid)),
            kind: DeviceKind::Headset,
            via: "Jabra Link 390".into(),
            hw: Some((vid, pid)),
            grace_sec: 120,
            ..Default::default()
        };
        let s = match reading {
            Some((Some((level, charging)), name)) => {
                if let Some(n) = name {
                    names.insert(pid, n);
                }
                let name = names.get(&pid).cloned().unwrap_or_else(|| "Jabra headset".into());
                DeviceStatus { name, percent: level, charging, ..base }
            }
            _ => {
                let name = names.remove(&pid).unwrap_or_else(|| "Jabra headset".into());
                DeviceStatus { name, connected: false, ..base }
            }
        };
        ctx.add(s, vid, pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(bytes: &[u8]) -> Vec<u8> {
        let mut f = vec![0u8; 64];
        f[..bytes.len()].copy_from_slice(bytes);
        f
    }

    #[test]
    fn battery_query() {
        assert_eq!(request(0x21, STATUS, HS_BATTERY)[..8], [0x05, 0x04, 0x00, 0x21, 0x46, 0x12, 0x02, 0x00]);
    }

    #[test]
    fn battery_reply() {
        // RESPONSE (3 << 6) | 8 bytes: flags 01 (charging), 80 %.
        let r = frame(&[0x05, 0x00, 0x04, 0x21, 0xC8, 0x12, 0x02, 0x01, 0x50]);
        let Some(Reply::Ok(p)) = decode(&r, 0x21, STATUS, HS_BATTERY) else { panic!() };
        assert_eq!(parse_battery(&p), Some((80, true)));
        // HeadsetControl's battery payload with ADC bytes after the level: on battery, 45 %.
        let r = frame(&[0x05, 0x00, 0x04, 0x22, 0xCA, 0x12, 0x02, 0x00, 0x2D, 0x0F, 0x80]);
        let Some(Reply::Ok(p)) = decode(&r, 0x22, STATUS, HS_BATTERY) else { panic!() };
        assert_eq!(parse_battery(&p), Some((45, false)));
    }

    #[test]
    fn headset_offline() {
        let r = frame(&[0x05, 0x00, 0x04, 0x23, 0xC7, 0xFE, 0xF3]);
        assert_eq!(decode(&r, 0x23, STATUS, HS_BATTERY), Some(Reply::Nak(0xF3)));
    }

    #[test]
    fn not_our_reply() {
        let ok = [0x05, 0x00, 0x04, 0x24, 0xC8, 0x12, 0x02, 0x00, 0x50];
        assert_eq!(decode(&frame(&ok), 0x25, STATUS, HS_BATTERY), None); // other sequence
        let mut ev = ok;
        ev[4] = 0x08; // unsolicited event
        assert_eq!(decode(&frame(&ev), 0x24, STATUS, HS_BATTERY), None);
        assert_eq!(decode(&request(0x24, STATUS, HS_BATTERY), 0x24, STATUS, HS_BATTERY), None); // echo
        let mut other = ok;
        other[6] = 0x08; // busy state
        assert_eq!(decode(&frame(&other), 0x24, STATUS, HS_BATTERY), None);
        assert_eq!(decode(&frame(&[0x03, 0x00]), 0x24, STATUS, HS_BATTERY), None); // other report
    }

    #[test]
    fn names() {
        assert_eq!(parse_name(b"\x0AEvolve3 75").as_deref(), Some("Evolve3 75"));
        assert_eq!(parse_name(b"\x0AEvolve3 75\0\0\0").as_deref(), Some("Evolve3 75")); // padded
        assert_eq!(parse_name(b"Jabra Evolve2 65\0\0").as_deref(), Some("Jabra Evolve2 65"));
        assert_eq!(parse_name(&[]), None);
    }
}
