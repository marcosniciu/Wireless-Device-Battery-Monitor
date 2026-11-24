//! Darmoshark M3 and M3S on their Telink 2.4 GHz receivers (VID 248A: M3 receiver FF30 / mouse FF12,
//! M3S receiver FF31 / mouse FF18).
//!
//! Protocol as documented by darmoshark-m3-configurator (MIT, github.com/Barba2k2/
//! darmoshark-m3-configurator, PROTOCOL.md, confirmed there on an M3 through its receiver) and used by
//! the original C++ BatteryHub (GPL-3.0, src/drivers/darmoshark.cpp, which adds the M3S ids);
//! implemented here from those facts. Receiver interface 1, usage page 008C (feature report 51 for
//! commands, input report 54 for acknowledgements). Only read opcodes are sent:
//!
//!   bond      SetFeature 51 03   GetFeature 51 -> 51 03 .. VV VV PP PP LL   (vid, mouse pid LE, link)
//!   identity  SetFeature 51 06   GetFeature 51 -> 51 06 01 .. VV VV PP PP .. .. .. NN   NN = level %
//!   input     54 E4 02 00        = no link to the mouse (asleep): stop asking
//!
//! The feature buffer keeps the previous reply until the new one lands, so a reply only counts with
//! the echoed opcode, and the bond read is sent first so an old identity reply can't be taken as new.
//! No link or no reply: the mouse sleeps or is off and is left out (the app keeps its last level).
use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::{HidColl, HidDev};
use crate::util::{hex4, Deadline};

const VID: u16 = 0x248A;
/// (receiver pid, mouse pid, name)
const MODELS: [(u16, u16, &str); 2] = [(0xFF30, 0xFF12, "Darmoshark M3"), (0xFF31, 0xFF18, "Darmoshark M3S")];
const FEATURE: u8 = 0x51;
const OP_BOND: u8 = 0x03;
const OP_IDENTITY: u8 = 0x06;
const RECEIVER_MS: u64 = 1000;
const QUERY_MS: u64 = 500;

fn id(r: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([r[at], r[at + 1]])
}

/// True when a bond reply (report id included) names `mouse` as linked.
pub fn bond_linked(r: &[u8], mouse: u16) -> bool {
    r.len() >= 12 && r[0] == FEATURE && r[1] == OP_BOND && id(r, 3) == VID && id(r, 5) == mouse && r[7] != 0
}

/// The level from an identity reply (report id included) of `mouse`.
pub fn identity_level(r: &[u8], mouse: u16) -> Option<i32> {
    (r.len() >= 12 && r[0] == FEATURE && r[1] == OP_IDENTITY && r[2] == 1 && id(r, 4) == VID && id(r, 6) == mouse && r[11] <= 100)
        .then(|| r[11] as i32)
}

/// Input report 54 E4 02: the receiver has no link to the mouse.
pub fn no_link(r: &[u8]) -> bool {
    r.len() >= 3 && r[0] == 0x54 && r[1] == 0xE4 && r[2] == 2
}

/// Sends a read opcode and returns the reply once the buffer echoes it.
fn query(d: &HidDev, op: u8, end: &Deadline) -> Option<Vec<u8>> {
    let mine = Deadline::in_ms(QUERY_MS);
    d.flush();
    if end.passed() || !d.set_feature(&[FEATURE, op]) {
        return None;
    }
    while !end.passed() && !mine.passed() {
        if d.read(end.left_ms().min(mine.left_ms()).min(20)).is_some_and(|r| no_link(&r)) {
            return None;
        }
        if let Some(r) = d.get_feature(FEATURE).filter(|r| r.len() >= 12 && r[0] == FEATURE && r[1] == op) {
            return Some(r);
        }
    }
    None
}

fn read_level(d: &HidDev, mouse: u16, end: &Deadline) -> Option<i32> {
    let bond = query(d, OP_BOND, end)?;
    if !bond_linked(&bond, mouse) {
        return None;
    }
    identity_level(&query(d, OP_IDENTITY, end)?, mouse)
}

fn read(c: &HidColl, mouse: u16) -> Option<i32> {
    let d = HidDev::open(c)?;
    let end = Deadline::in_ms(RECEIVER_MS);
    // The first request after idle is sometimes lost: one retry within the receiver's time budget.
    read_level(&d, mouse, &end).or_else(|| read_level(&d, mouse, &end))
}

pub fn poll(ctx: &mut PollContext) {
    for (receiver, mouse, name) in MODELS {
        if ctx.is_claimed(VID, receiver) {
            continue;
        }
        let colls = ctx.find(VID, receiver, |c| c.iface == 1 && c.page() == 0x008C && c.usage() == 0x0001 && c.in_len() >= 4 && c.feat_len() >= 20);
        if colls.is_empty() {
            continue;
        }
        let Some(level) = colls.iter().find_map(|c| read(c, mouse)) else {
            ctx.claim(VID, receiver); // asleep / off: no generic fallback for it
            continue;
        };
        let s = DeviceStatus {
            key: format!("darmoshark:{}", hex4(receiver)),
            name: name.into(),
            kind: DeviceKind::Mouse,
            via: "2.4 GHz receiver".into(),
            percent: level,
            hw: Some((VID, receiver)),
            grace_sec: 1800,
            ..Default::default()
        };
        ctx.add(s, VID, receiver);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feature(payload: &[u8]) -> Vec<u8> {
        let mut r = vec![FEATURE];
        r.extend_from_slice(payload);
        r.resize(20, 0);
        r
    }

    #[test]
    fn bond_replies() {
        // opcode 03, length, vid 248A, mouse FF18, linked.
        let r = feature(&[0x03, 0x06, 0x8A, 0x24, 0x18, 0xFF, 0x01]);
        assert!(bond_linked(&r, 0xFF18));
        assert!(!bond_linked(&r, 0xFF12)); // another model's mouse
        assert!(!bond_linked(&feature(&[0x03, 0x06, 0x8A, 0x24, 0x18, 0xFF, 0x00]), 0xFF18)); // not linked
        assert!(!bond_linked(&feature(&[0x06, 0x06, 0x8A, 0x24, 0x18, 0xFF, 0x01]), 0xFF18)); // stale identity
    }

    #[test]
    fn identity_replies() {
        // opcode 06, status 1, .., vid 248A, pid FF12, firmware 0209, .., 76 %.
        let r = feature(&[0x06, 0x01, 0x00, 0x8A, 0x24, 0x12, 0xFF, 0x09, 0x02, 0x00, 76]);
        assert_eq!(identity_level(&r, 0xFF12), Some(76));
        assert_eq!(identity_level(&r, 0xFF18), None);
        let mut bad = r.clone();
        bad[11] = 101;
        assert_eq!(identity_level(&bad, 0xFF12), None);
        bad = r.clone();
        bad[2] = 0;
        assert_eq!(identity_level(&bad, 0xFF12), None);
        assert_eq!(identity_level(&feature(&[0x03, 0x01]), 0xFF12), None);
    }

    #[test]
    fn acknowledgements() {
        assert!(no_link(&[0x54, 0xE4, 0x02, 0x00]));
        assert!(!no_link(&[0x54, 0xE4, 0x01, 0x00])); // ready
        assert!(!no_link(&[0x54, 0xE4, 0x00, 0x00])); // queued
    }
}
