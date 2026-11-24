//! Apple AirPods / Beats over Bluetooth. Windows does not read their battery, but they broadcast it
//! in clear in the BLE "proximity pairing" message (manufacturer data, company 0x004C, type 0x07):
//!   [0] 07  [1] 19  [2] 01  [3..4] model (LE)  [5] bit 5 = left bud broadcasts
//!   [6] dual-bud: low nibble = broadcasting bud, high = other (0-10 = x10 %, 15 = unknown)
//!       single-battery models: low nibble = battery
//!   [7] high nibble: bit0 broadcasting/single charging, bit1 other bud charging, bit2 case charging;
//!       low nibble: case level
//! Only messages whose model matches the PID of a paired device are used; strongest recent signal wins.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use windows::Win32::Devices::Properties::DEVPKEY_Device_FriendlyName;

use super::sdi::{DevSet, IS_CONNECTED};
use crate::ble;
use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::util::{hex4, sleep_ms};

#[derive(Clone, Copy)]
struct Advert {
    model: u16,
    single: bool,
    left: i32,
    right: i32,
    case: i32,
    left_chg: bool,
    right_chg: bool,
    case_chg: bool,
    rssi: i32,
    at: Instant,
}

fn adverts() -> &'static Mutex<HashMap<u64, Advert>> {
    static M: OnceLock<Mutex<HashMap<u64, Advert>>> = OnceLock::new();
    M.get_or_init(Default::default)
}

fn level(n: u8) -> i32 {
    if n <= 10 {
        n as i32 * 10
    } else {
        -1
    }
}

fn is_single_battery_model(m: u16) -> bool {
    matches!(
        m,
        0x200A | 0x201F | 0x202D | 0x2003 | 0x2005 | 0x2006 | 0x2009 | 0x200C | 0x200D | 0x2010 | 0x2017 | 0x2025
    )
}

fn parse_proximity(d: &[u8]) -> Option<Advert> {
    if d.len() < 27 || d[0] != 0x07 || d[1] != 0x19 || d[2] != 0x01 {
        return None;
    }
    let model = d[3] as u16 | (d[4] as u16) << 8;
    if model & 0xFF00 != 0x2000 {
        return None;
    }
    let single = is_single_battery_model(model);
    let left_broadcasts = d[5] & 0x20 != 0;
    let (cur, other, chg) = (d[6] & 0x0F, d[6] >> 4, d[7] >> 4);
    let mut a = Advert {
        model,
        single,
        left: -1,
        right: -1,
        case: -1,
        left_chg: false,
        right_chg: false,
        case_chg: false,
        rssi: -127,
        at: Instant::now(),
    };
    if single {
        let (mut pct, mut charging) = (level(cur), chg & 0x01 != 0);
        if cur == 0 && (1..=10).contains(&other) {
            pct = level(other);
            charging = chg & 0x02 != 0;
        }
        if pct < 0 {
            return None;
        }
        a.left = pct;
        a.left_chg = charging;
        return Some(a);
    }
    a.left = level(if left_broadcasts { cur } else { other });
    a.right = level(if left_broadcasts { other } else { cur });
    a.left_chg = a.left >= 0 && chg & (if left_broadcasts { 0x01 } else { 0x02 }) != 0;
    a.right_chg = a.right >= 0 && chg & (if left_broadcasts { 0x02 } else { 0x01 }) != 0;
    a.case = level(d[7] & 0x0F);
    a.case_chg = a.case >= 0 && chg & 0x04 != 0;
    (a.left >= 0 || a.right >= 0 || a.case >= 0).then_some(a)
}

/// Walks the Apple Continuity TLV sequence in case another TLV precedes 0x07.
fn parse(buf: &[u8]) -> Option<Advert> {
    let mut i = 0;
    while i + 1 < buf.len() {
        let (ty, len) = (buf[i], buf[i + 1] as usize);
        if i + 2 + len > buf.len() {
            break;
        }
        if ty == 0x07 {
            if let Some(a) = parse_proximity(&buf[i..i + 2 + len]) {
                return Some(a);
            }
        }
        i += 2 + len;
    }
    None
}

/// Called by the BLE watcher for every Apple manufacturer-data blob.
pub fn on_advert(addr: u64, rssi: i32, data: &[u8]) {
    let Some(mut a) = parse(data) else { return };
    a.rssi = rssi;
    if let Ok(mut m) = adverts().lock() {
        m.insert(addr, a);
        m.retain(|_, v| v.at.elapsed().as_secs() < 600); // addresses rotate: drop old ones
    }
}

#[derive(Default, Clone)]
struct AppleDevice {
    name: String,
    pid: u16,
    connected: bool,
    apple: bool,
}

fn paired_apple_audio() -> Vec<AppleDevice> {
    let Some(set) = DevSet::present(Some("BTHENUM")) else { return vec![] };
    let mut by_container: BTreeMap<String, AppleDevice> = BTreeMap::new();
    for n in set.nodes() {
        let Some(cid) = n.container() else { continue };
        let d = by_container.entry(cid).or_default();
        let id = n.instance_id.to_uppercase();
        if let Some(v) = id.find("_VID&0001004C") {
            d.apple = true;
            if let Some(p) = id[v..].find("_PID&") {
                let hex: String = id[v + p + 5..].chars().take_while(|c| c.is_ascii_hexdigit()).collect();
                d.pid = u16::from_str_radix(&hex, 16).unwrap_or(0);
            }
        }
        if n.instance_id.starts_with("BTHENUM\\DEV_") {
            if let Some(name) = n.string(&DEVPKEY_Device_FriendlyName) {
                d.name = name;
            }
            d.connected = n.boolean(&IS_CONNECTED).unwrap_or(false);
        }
    }
    let mut out: Vec<AppleDevice> = by_container
        .into_values()
        .filter(|d| {
            let n = d.name.to_uppercase();
            let looks_apple = n.contains("AIRPODS") || n.contains("BEATS");
            let apple_audio = d.apple && d.pid & 0xFF00 == 0x2000;
            // Shown only while connected to this PC: AirPods in range but connected to a phone,
            // or lying in their case, keep broadcasting and must not appear here.
            d.connected && (apple_audio || (looks_apple && d.pid == 0))
        })
        .collect();
    out.sort_by_key(|d| !d.connected);
    out
}

/// Best message for one device (see the module docs); fills a missing side from the other bud.
fn pick(dev: &AppleDevice) -> Option<Advert> {
    let max_age = if dev.connected { 300 } else { 90 };
    let min_rssi = if dev.connected { -90 } else { -80 };
    let m = adverts().lock().ok()?;
    let fresh = |a: &Advert| a.at.elapsed().as_secs() < 90;
    let mut best: Option<Advert> = None;
    for a in m.values() {
        if dev.pid != 0 && a.model != dev.pid {
            continue;
        }
        if a.at.elapsed().as_secs() > max_age || a.rssi < min_rssi {
            continue;
        }
        let better = match &best {
            None => true,
            Some(b) => {
                (fresh(a) && !fresh(b)) || (fresh(a) && fresh(b) && a.rssi > b.rssi) || (!fresh(a) && !fresh(b) && a.at > b.at)
            }
        };
        if better {
            best = Some(*a);
        }
    }
    let mut best = best?;
    if !best.single {
        for a in m.values() {
            if a.model != best.model || a.single {
                continue;
            }
            let gap = if best.at > a.at { best.at - a.at } else { a.at - best.at };
            if gap.as_secs() > 15 {
                continue;
            }
            if best.left < 0 && a.left >= 0 {
                (best.left, best.left_chg) = (a.left, a.left_chg);
            }
            if best.right < 0 && a.right >= 0 {
                (best.right, best.right_chg) = (a.right, a.right_chg);
            }
            if best.case < 0 && a.case >= 0 {
                (best.case, best.case_chg) = (a.case, a.case_chg);
            }
        }
    }
    Some(best)
}

fn pct(v: i32) -> String {
    if v < 0 {
        "?".into()
    } else {
        format!("{v}%")
    }
}

pub fn poll(ctx: &mut PollContext) {
    let devices = paired_apple_audio();
    if devices.is_empty() {
        return;
    }
    if ble::ensure_watcher() {
        // Just started (every read starts the watcher): give the headphones up to 5 s to broadcast;
        // in the ear they can be quieter than the 1-2 s interval of an open case.
        for _ in 0..25 {
            if devices.iter().any(|d| pick(d).is_some()) {
                break;
            }
            sleep_ms(200);
        }
    }
    let mut used_pid = HashSet::new();
    for d in &devices {
        if d.pid != 0 && !d.connected && used_pid.contains(&d.pid) {
            continue;
        }
        let Some(a) = pick(d) else { continue };
        if d.pid != 0 {
            used_pid.insert(d.pid);
        }
        let name = if d.name.is_empty() { "AirPods".to_string() } else { d.name.clone() };
        let mut s = DeviceStatus {
            key: format!("airpods:{}:{}", hex4(d.pid), name),
            name,
            kind: DeviceKind::Headset,
            via: "Bluetooth".into(),
            grace_sec: if d.connected { 300 } else { 0 },
            vid: 0x004C,
            ..Default::default()
        };
        if a.single {
            s.percent = a.left;
            s.charging = a.left_chg;
        } else {
            // Prefer the lower discharging bud (the one in use); charging only if every online bud is.
            let (mut dis, mut chg) = (-1, -1);
            for (p, c) in [(a.left, a.left_chg), (a.right, a.right_chg)] {
                if p < 0 {
                    continue;
                }
                let dst = if c { &mut chg } else { &mut dis };
                if *dst < 0 || p < *dst {
                    *dst = p;
                }
            }
            if dis >= 0 {
                s.percent = dis;
            } else if chg >= 0 {
                s.percent = chg;
                s.charging = true;
            } else {
                s.percent = a.case;
                s.charging = a.case_chg;
            }
            let z = |c: bool| if c { "⚡" } else { "" };
            s.detail = format!(
                "L {}{}  R {}{}  case {}{}",
                pct(a.left),
                z(a.left_chg),
                pct(a.right),
                z(a.right_chg),
                pct(a.case),
                z(a.case_chg)
            );
        }
        ctx.out.push(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A paired-mode proximity message (type 07, length 0x19) for `model`.
    fn msg(model: u16, status: u8, levels: u8, charge_case: u8) -> Vec<u8> {
        let mut d = vec![0x07, 0x19, 0x01, model as u8, (model >> 8) as u8, status, levels, charge_case];
        d.resize(27, 0);
        d
    }

    #[test]
    fn airpods_pro_2_left_broadcasting() {
        // Left bud broadcasts (status bit 5): left 90%, right 80%, case 50%, case charging.
        let a = parse(&msg(0x2014, 0x20, 0x89, 0x45)).unwrap();
        assert_eq!((a.left, a.right, a.case), (90, 80, 50));
        assert!(!a.left_chg && !a.right_chg && a.case_chg);
    }

    #[test]
    fn unknown_values_and_single_battery_models() {
        // Right bud broadcasts, other side unknown (15).
        let a = parse(&msg(0x2014, 0x00, 0xF7, 0x0F)).unwrap();
        assert_eq!((a.left, a.right, a.case), (-1, 70, -1));
        // AirPods Max: one battery in the low nibble, charging bit 0.
        let m = parse(&msg(0x200A, 0x00, 0x06, 0x90)).unwrap();
        assert!(m.single);
        assert_eq!(m.left, 60);
        assert!(m.left_chg);
    }

    #[test]
    fn rejects_other_messages() {
        assert!(parse(&[0x10, 0x05, 0x01, 0x02, 0x03, 0x04, 0x05]).is_none()); // nearby-info, not proximity
        assert!(parse(&msg(0x1234, 0x20, 0x89, 0x45)).is_none()); // not an audio model
    }
}
