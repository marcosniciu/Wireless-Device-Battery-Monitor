//! Rongyuan (RY) 2.4 GHz receivers, used by Attack Shark, AJAZZ, EWEADN and many other brands' mice
//! and keyboards (e.g. EWEADN GS01 on 3151:5031). The receiver keeps the level of its paired mouse
//! and keyboard, and one status query returns both slots.
//!
//! Protocol and receiver list as documented by BatteryTray (MIT, github.com/qu1ncky/BatteryTray,
//! src/providers/rongyuan.rs), taken from the official ATTACK SHARK web driver and verified on the
//! Attack Shark R3 receiver 3151:402D, and as used by the original C++ BatteryHub (GPL-3.0,
//! src/drivers/rongyuan.cpp); implemented here from those facts. On interface 2, vendor collection
//! FFFF:0002 (feature report 0, 64 bytes after the report id):
//!
//!   SetFeature  00 F7 00 ...                           (read-only status query)
//!   GetFeature  00 OK KB MS KO MO ...                  KB / MS = keyboard / mouse level (%),
//!                                                      KO / MO = 0 when that slot is connected
//!
//! An all-zero reply means the receiver has not filled it yet; it is asked up to 3 times. A level of
//! 0 right after waking means "not reported yet". An offline slot (device asleep or off) is left out,
//! so the app keeps its last level dimmed.
use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::{self, HidColl, HidDev};
use crate::util::{hex4, sleep_ms};

const STATUS_COMMAND: u8 = 0xF7;

/// (vid << 16) | pid of every receiver the web driver reads with this protocol (BatteryTray's list).
const RECEIVERS: &[u32] = &[
    0x0DB0197E, 0x0DB08E9A, 0x145F0376, 0x145F038D, 0x14A5C045, 0x25AA2008, 0x25AA4008, 0x25AA4018, 0x31514011,
    0x31514014, 0x31514017, 0x31514021, 0x31514027, 0x3151402D, 0x31514038, 0x31515006, 0x31515007, 0x31515008,
    0x31515020, 0x31515026, 0x3151502B, 0x31515031, 0x31515037, 0x31515038, 0x3151503D, 0x3151503F, 0x31515040,
    0x31515042, 0x31515045, 0x3151504B, 0x3151504E, 0x31515050, 0x31515052, 0x31515055, 0x3151505A, 0x342DE3E0,
    0x342DE3E1, 0x342DE3E2, 0x342DE3E6, 0x342DE3E7, 0x342DE3E8, 0x342DE3F5, 0x342DE3F6, 0x342DE3F7, 0x342DE3FB,
    0x342DE3FC, 0x342DE3FD, 0x347AA302, 0x347AA305, 0x374AA205, 0x374AA217, 0x374AA220, 0x374AA223, 0x374AA226,
    0x374AA229, 0x374AA234, 0x374AA237, 0x374AA251, 0x374AA263, 0x374AA268, 0x374AA311, 0x374AA314, 0x374AA319,
    0x374AA322, 0x374AA329, 0x374AA355, 0x379A1804, 0x379A1807, 0x38A90002, 0x38EE0000, 0x38EE0002, 0x38EE0039,
    0x391D1A04, 0x391D1A08, 0x391D1A0E, 0x391D1A13, 0x391D1A25, 0x391D1A2B, 0x391D1A34, 0x391D2017, 0x391D2A05,
    0x39AB7000, 0x39AB7003, 0x39AB7006, 0x39AB7007, 0x39AB7008, 0x39AB7009, 0x38EE0004,
];

fn is_receiver(vid: u16, pid: u16) -> bool {
    RECEIVERS.contains(&(((vid as u32) << 16) | pid as u32))
}

/// Levels of the connected slots: (keyboard, mouse), each Some(level 1..100) when the slot is
/// connected and has a level. None when the reply is not a status reply yet (all zero / too short).
pub fn parse(r: &[u8]) -> Option<(Option<u8>, Option<u8>)> {
    if r.len() < 7 || r[0] != 0 || (r[2] | r[3] | r[4] | r[5] | r[6]) == 0 {
        return None;
    }
    let slot = |level: u8, offline: u8| (offline == 0 && (1..=100).contains(&level)).then_some(level);
    Some((slot(r[2], r[4]), slot(r[3], r[5])))
}

fn query(c: &HidColl) -> Option<(Option<u8>, Option<u8>)> {
    let d = HidDev::open_feature_only(c)?;
    let mut cmd = vec![0u8; c.feat_len() as usize];
    cmd[1] = STATUS_COMMAND;
    for attempt in 0..3 {
        if attempt > 0 {
            sleep_ms(100);
        }
        if !d.set_feature(&cmd) {
            return None;
        }
        sleep_ms(20);
        if let Some(s) = d.get_feature(0).and_then(|r| parse(&r)) {
            return Some(s);
        }
    }
    None
}

/// "AJAZZ 2.4G 8K" -> "AJAZZ"; "2.4G Wireless Receiver" -> "".
pub fn brand(product: &str) -> String {
    const LINK_WORDS: [&str; 10] = ["2.4G", "2.4GHZ", "8K", "4K", "2K", "1K", "RECEIVER", "WIRELESS", "DONGLE", "USB"];
    product
        .split_whitespace()
        .filter(|w| !LINK_WORDS.contains(&w.to_ascii_uppercase().as_str()) && !w.eq_ignore_ascii_case("mouse") && !w.eq_ignore_ascii_case("keyboard"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn name(product: &str, keyboard: bool) -> String {
    let b = brand(product);
    match (b.is_empty(), keyboard) {
        (true, true) => "Wireless keyboard".into(),
        (true, false) => "Wireless mouse".into(),
        (false, true) => format!("{b} wireless keyboard"),
        (false, false) => format!("{b} wireless mouse"),
    }
}

pub fn poll(ctx: &mut PollContext) {
    let mut products: Vec<(u16, u16)> = ctx.colls.iter().filter(|c| is_receiver(c.vid, c.pid)).map(|c| (c.vid, c.pid)).collect();
    products.sort_unstable();
    products.dedup();
    for (vid, pid) in products {
        if ctx.is_claimed(vid, pid) {
            continue;
        }
        let colls = ctx.find(vid, pid, |c| c.iface == 2 && c.page() == 0xFFFF && c.usage() == 0x0002 && c.feat_len() >= 7);
        let Some((c, (kb, ms))) = colls.iter().find_map(|c| query(c).map(|s| (c, s))) else { continue };
        let product = hid::product_string(c);
        for (keyboard, level) in [(true, kb), (false, ms)] {
            let Some(level) = level else { continue };
            ctx.out.push(DeviceStatus {
                key: format!("rongyuan:{}:{}:{}", hex4(vid), hex4(pid), if keyboard { "kb" } else { "ms" }),
                name: name(&product, keyboard),
                kind: if keyboard { DeviceKind::Keyboard } else { DeviceKind::Mouse },
                via: "2.4 GHz receiver".into(),
                percent: level as i32,
                vid,
                hw: Some((vid, pid)),
                grace_sec: 1800,
                ..Default::default()
            });
        }
        ctx.claim(vid, pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_replies() {
        // [00][ok][kb %][mouse %][kb offline][mouse offline][can send]
        assert_eq!(parse(&[0x00, 0x01, 0x00, 0x4B, 0x01, 0x00, 0x01, 0x00]), Some((None, Some(75))));
        assert_eq!(parse(&[0x00, 0x01, 0x50, 0x00, 0x00, 0x01, 0x01]), Some((Some(80), None)));
        assert_eq!(parse(&[0x00, 0x01, 0x00, 0x00, 0x01, 0x00, 0x01]), Some((None, None))); // level not reported yet
        assert_eq!(parse(&[0x00, 0x01, 0x00, 0x32, 0x01, 0x01, 0x01]), Some((None, None))); // mouse asleep
    }

    #[test]
    fn not_ready() {
        assert_eq!(parse(&[0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00]), None);
        assert_eq!(parse(&[0x00, 0x01, 0x40]), None);
        assert_eq!(parse(&[0x05, 0x01, 0x40, 0x40, 0x00, 0x00, 0x01]), None);
    }

    #[test]
    fn receivers_and_names() {
        assert!(is_receiver(0x3151, 0x5031));
        assert!(is_receiver(0x3151, 0x402D));
        assert!(!is_receiver(0x3151, 0x3020));
        assert_eq!(brand("AJAZZ 2.4G 8K"), "AJAZZ");
        assert_eq!(brand("2.4G Wireless Receiver"), "");
        assert_eq!(name("2.4G Wireless Mouse", false), "Wireless mouse");
        assert_eq!(name("EWEADN 2.4G", false), "EWEADN wireless mouse");
    }
}
