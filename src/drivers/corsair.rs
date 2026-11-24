//! Corsair Void family wireless headsets (Void / Void Pro / Void Elite, HS70 / HS70 Pro, ...) over
//! their 2.4 GHz receiver (VID 1B1C).
//!
//! Protocol as documented by HeadsetControl (GPL-3.0, github.com/Sapd/HeadsetControl,
//! lib/devices/corsair_device.hpp and corsair_void_rich.hpp, which also lists the product ids) and
//! the Linux kernel's hid-corsair-void driver by Stuart Hayhurst (GPL-2.0-or-later,
//! drivers/hid/hid-corsair-void.c); implemented here from those facts. The receiver's vendor
//! collection on interface 3 (usage page FFC5, usage 0001) takes an output report and answers
//! with input report 64:
//!
//!   request  C9 64                    (report C9 "status request", 64 = the status report)
//!   reply    64 PB LL CC SS
//!            PB  power button (top bit) and unknown bits: not used
//!            LL  battery level 0..100 in bits 0..6; bit 7 = mic boom up
//!            CC  connection: 177 normal; 49 lost, 51 / 52 disconnected (kernel); 16 wired, 38 init
//!            SS  0 disconnected, 1 normal, 2 low, 3 critical, 4 fully charged, 5 charging
//!
//! The kernel notes that the level reads too high while charging (capped at 100); it is shown as
//! reported, as HeadsetControl does. The receiver also pushes report 64 on its own (power button,
//! mic boom); one that arrives while waiting is just as good as the reply.
use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::{self, HidColl, HidDev};
use crate::util::{hex4, Deadline};

const VID: u16 = 0x1B1C;
/// HeadsetControl's Corsair Void list (CorsairVoidRich::SUPPORTED_PRODUCT_IDS).
const PIDS: [u16; 18] = [
    0x1B1C, 0x1B27, 0x0A14, 0x0A16, 0x0A17, 0x0A1D, 0x0A1A, 0x1B2A, 0x1B23, 0x1B29, 0x0A55, 0x0A51, 0x0A52, 0x0A38, 0x0A4F, 0x0A2B,
    0x0A75, 0x0A56,
];
const REQUEST: [u8; 2] = [0xC9, 0x64];
const STATUS_REPORT: u8 = 0x64;
const WAIT_MS: u64 = 1000;

#[derive(Debug, PartialEq, Eq)]
pub struct Reading {
    /// None: the headset is not linked to the receiver.
    pub level: Option<i32>,
    pub charging: bool,
    pub full: bool,
}

/// Decodes one input report (report ID included).
pub fn decode(r: &[u8]) -> Option<Reading> {
    if r.len() < 5 || r[0] != STATUS_REPORT {
        return None;
    }
    let off = Reading { level: None, charging: false, full: false };
    if matches!(r[3], 49 | 51 | 52) {
        return Some(off);
    }
    match r[4] {
        1..=5 => Some(Reading {
            level: Some(((r[2] & 0x7F) as i32).min(100)),
            charging: r[4] == 4 || r[4] == 5,
            full: r[4] == 4,
        }),
        0 => Some(off),
        _ => None,
    }
}

fn query(c: &HidColl) -> Option<Reading> {
    let d = HidDev::open(c)?;
    d.flush();
    if !d.write(&REQUEST) {
        return None;
    }
    let end = Deadline::in_ms(WAIT_MS);
    while !end.passed() {
        if let Some(f) = d.read(end.left_ms()).and_then(|r| decode(&r)) {
            return Some(f);
        }
    }
    None
}

fn name(c: &HidColl, pid: u16) -> String {
    let n = hid::product_string(c);
    let n = n.trim();
    if !n.is_empty() && !n.eq_ignore_ascii_case("hid interface") {
        return n.to_string();
    }
    match pid {
        0x0A4F => "Corsair HS70 Pro Wireless".into(),
        _ => "Corsair wireless headset".into(),
    }
}

pub fn poll(ctx: &mut PollContext) {
    for (vid, pid) in ctx.products(VID) {
        if !PIDS.contains(&pid) {
            continue;
        }
        let colls = ctx.find(vid, pid, |c| c.page() == 0xFFC5 && c.usage() == 0x0001 && c.in_len() >= 5 && c.out_len() >= 2);
        let Some((c, reading)) = colls.iter().find_map(|c| query(c).map(|r| (c, r))) else { continue };
        let base = DeviceStatus {
            key: format!("corsair:{}", hex4(pid)),
            name: name(c, pid),
            kind: DeviceKind::Headset,
            via: "2.4 GHz receiver".into(),
            hw: Some((vid, pid)),
            grace_sec: 60,
            ..Default::default()
        };
        let s = match reading {
            Reading { level: Some(level), charging, full } => DeviceStatus { percent: level, charging, full, ..base },
            _ => DeviceStatus { connected: false, ..base },
        };
        ctx.add(s, vid, pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery() {
        // Layout from HeadsetControl / hid-corsair-void: 64 00 LL B1 SS.
        assert_eq!(decode(&[0x64, 0x00, 55, 177, 1]), Some(Reading { level: Some(55), charging: false, full: false }));
        // Mic boom up sets bit 7 of the level byte.
        assert_eq!(decode(&[0x64, 0x00, 0x80 | 40, 177, 2]), Some(Reading { level: Some(40), charging: false, full: false }));
        assert_eq!(decode(&[0x64, 0x00, 80, 177, 5]), Some(Reading { level: Some(80), charging: true, full: false }));
        assert_eq!(decode(&[0x64, 0x00, 100, 177, 4]), Some(Reading { level: Some(100), charging: true, full: true }));
    }

    #[test]
    fn disconnected() {
        let off = Some(Reading { level: None, charging: false, full: false });
        assert_eq!(decode(&[0x64, 0x00, 0, 177, 0]), off);
        assert_eq!(decode(&[0x64, 0x00, 60, 51, 1]), off);
        assert_eq!(decode(&[0x64, 0x00, 60, 49, 1]), off);
    }

    #[test]
    fn not_status() {
        assert_eq!(decode(&[0x66, 0x01, 0x02, 0x03, 0x04]), None); // firmware report
        assert_eq!(decode(&[0x64, 0x00, 50, 177, 9]), None); // unknown state
        assert_eq!(decode(&[0x64, 0x00, 50]), None);
    }
}
