//! Razer wireless mice, keyboards and dongles (VID 1532), 90-byte feature report protocol:
//!   [0] status  [1] transaction id  [2..3] remaining packets  [4] protocol type  [5] data size
//!   [6] command class  [7] command id  [8..87] arguments  [88] crc (xor of 2..87)  [89] reserved
//! Battery: class 07 id 80 -> args[1] = 0..255. Charging: class 07 id 84 -> args[1] = 1.
//! Reference: OpenRazer.
use crate::device::{DeviceKind, PollContext, DeviceStatus};
use crate::hid::{self, HidColl, HidDev};
use crate::util::{clamp100, guess_kind, hex4, sleep_ms};

fn keyboard_battery_tid(pid: u16) -> u8 {
    match pid {
        0x025C | 0x0271 | 0x0290 | 0x0296 | 0x02BA | 0x02C9 | 0x02D2 | 0x02D5 => 0x9F,
        0x0258 | 0x0292 | 0x0298 | 0x02B9 | 0x02CD | 0x02D4 | 0x02D7 => 0x1F,
        0x025A => 0x3F,
        _ => 0,
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Rz {
    Ok,
    Offline,
    Unsupported,
    NoReply,
}

fn command(d: &HidDev, tid: u8, cls: u8, id: u8, size: u8) -> (Rz, Vec<u8>) {
    let mut rep = vec![0u8; 91]; // [0] = report id 0, then the 90-byte report
    {
        let r = &mut rep[1..];
        r[1] = tid;
        r[5] = size;
        r[6] = cls;
        r[7] = id;
        let crc = r[2..88].iter().fold(0u8, |a, b| a ^ b);
        r[88] = crc;
    }
    let kb = keyboard_battery_tid(d.coll().pid) != 0;
    let mut send = true;
    let mut res = Rz::NoReply;
    for attempt in 0..8 {
        if send && !d.set_feature(&rep) {
            return (Rz::NoReply, vec![]);
        }
        sleep_ms(if kb { 5 } else if attempt == 0 { 15 } else { 40 });
        send = true;
        res = Rz::NoReply;
        let Some(resp) = d.get_feature(0).filter(|r| r.len() >= 91) else { continue };
        let a = &resp[1..];
        let crc = a[2..88].iter().fold(0u8, |x, y| x ^ y);
        if a[1] != tid || a[2] != 0 || a[3] != 0 || a[4] != 0 || a[5] != size || a[6] != cls || a[7] != id || a[88] != crc {
            continue; // Synapse shares the mailbox
        }
        match a[0] {
            0x01 => send = false, // busy
            0x02 => return (Rz::Ok, a[8..88].to_vec()),
            0x04 => res = Rz::Offline,
            0x03 | 0x05 => return (Rz::Unsupported, vec![]),
            _ => {}
        }
    }
    (res, vec![])
}

fn clean_name(mut n: String) -> String {
    for suf in [" Dongle", " (Wired)", " (Wireless)", " Wireless Receiver", " Receiver", " HyperSpeed Dongle"] {
        if n.len() > suf.len() && n.ends_with(suf) {
            n.truncate(n.len() - suf.len());
        }
    }
    if n.is_empty() {
        "Razer device".into()
    } else {
        n
    }
}

fn kind(colls: &[HidColl], vid: u16, pid: u16, name: &str) -> DeviceKind {
    let n = name.to_lowercase();
    if n.contains("keyboard") || n.contains("blackwidow") || n.contains("huntsman") || n.contains("ornata") || n.contains("deathstalker") {
        return DeviceKind::Keyboard;
    }
    let k = hid::kind_from_collections(colls, vid, pid);
    if k != DeviceKind::Other { k } else { guess_kind(name) }
}

pub fn poll(ctx: &mut PollContext) {
    let first = ctx.out.len();
    for (vid, pid) in ctx.products(0x1532) {
        for c in ctx.find(vid, pid, |c| c.feat_len() == 91) {
            let Some(d) = HidDev::open_feature_only(&c) else { continue };
            let kb_tid = keyboard_battery_tid(pid);
            let tids: Vec<u8> = if kb_tid != 0 { vec![kb_tid] } else { vec![0x1F, 0x3F, 0xFF, 0x08, 0x9F] };
            let (mut res, mut tid, mut args, mut saw_offline) = (Rz::NoReply, 0u8, vec![], false);
            for t in tids {
                let (r, a) = command(&d, t, 0x07, 0x80, 0x02);
                if r == Rz::Ok {
                    (res, tid, args) = (r, t, a);
                    break;
                }
                saw_offline |= r == Rz::Offline;
            }
            if res != Rz::Ok && saw_offline {
                res = Rz::Offline;
            }
            if res != Rz::Ok && res != Rz::Offline {
                continue;
            }
            let name = clean_name(hid::product_string(&c));
            let mut s = DeviceStatus {
                key: format!("razer:{}", hex4(pid)),
                kind: kind(&ctx.colls, vid, pid, &name),
                name,
                via: "Razer receiver".into(),
                grace_sec: 120,
                ..Default::default()
            };
            if res == Rz::Offline {
                s.connected = false;
            } else {
                s.percent = clamp100((args[1] as i32 * 100 + 127) / 255);
                let (r, ch) = command(&d, tid, 0x07, 0x84, 0x02);
                if r == Rz::Ok {
                    s.charging = ch[1] == 1;
                }
            }
            // Cable + receiver can report the same device; keep a live reading over an offline one.
            if let Some(e) = ctx.out[first..].iter_mut().find(|o| o.name == s.name) {
                if !e.connected && s.connected {
                    e.connected = true;
                    e.percent = s.percent;
                }
                e.charging |= s.charging;
                ctx.claim(vid, pid);
            } else {
                ctx.add(DeviceStatus { hw: Some((vid, pid)), ..s }, vid, pid);
            }
            break;
        }
    }
}
