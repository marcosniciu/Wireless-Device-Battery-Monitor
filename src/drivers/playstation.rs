//! Sony DualShock 4 / DualSense / DualSense Edge (VID 054C), USB or Bluetooth.
//! The battery is part of the regular input report:
//!   DualShock 4: USB report 01 byte 30, Bluetooth report 11 byte 32: bits 0-3 level, bit 4 cable
//!   DualSense:   USB report 01 byte 53, Bluetooth report 31 byte 54: bits 0-3 level (0-10), bits 4-7 state
//! Over Bluetooth both send a reduced report until a feature report (05, calibration) is read.
//! Layout reference: Linux drivers/hid/hid-playstation.c.
use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::HidDev;
use crate::util::{clamp100, fnv1a, hex4, Deadline};

fn is_ds4(pid: u16) -> bool {
    matches!(pid, 0x05C4 | 0x09CC | 0x0BA0)
}
fn is_dualsense(pid: u16) -> bool {
    matches!(pid, 0x0CE6 | 0x0DF2)
}

#[derive(Debug, PartialEq)]
enum Report {
    /// Battery percent and charging.
    Battery(i32, bool),
    /// The DS4 wireless adapter answered without a controller paired to it.
    NoController,
    /// Not a report that carries the battery (or the reduced Bluetooth report).
    Other,
}

/// Decodes one input report: DS4 USB 01 byte 30 / BT 11 byte 32 (bits 0-3 level, bit 4 cable),
/// DualSense USB 01 byte 53 / BT 31 byte 54 (bits 0-3 level 0-10, bits 4-7 state).
fn decode_report(pid: u16, bt: bool, r: &[u8]) -> Report {
    let (usb_at, bt_id, bt_at) = if is_ds4(pid) { (30, 0x11, 32) } else { (53, 0x31, 54) };
    let at = match r.first() {
        Some(0x01) if !bt => usb_at,
        Some(id) if *id == bt_id => bt_at,
        _ => return Report::Other,
    };
    let Some(&b) = r.get(at) else { return Report::Other };
    let level = (b & 0x0F) as i32;
    if is_ds4(pid) {
        if pid == 0x0BA0 && b == 0 {
            return Report::NoController;
        }
        let cable = b & 0x10 != 0;
        let pct = if cable { if level >= 11 { 100 } else { clamp100(level * 10) } } else { clamp100(level * 10 + 5) };
        Report::Battery(pct, cable && level < 11)
    } else {
        match b >> 4 {
            0x2 => Report::Battery(100, true), // fully charged on the cable
            state => Report::Battery(clamp100(level * 10 + 5), state == 0x1),
        }
    }
}

pub fn poll(ctx: &mut PollContext) {
    let colls: Vec<_> = ctx
        .colls
        .iter()
        .filter(|c| c.vid == 0x054C && (is_ds4(c.pid) || is_dualsense(c.pid)) && c.page() == 0x01 && c.usage() == 0x05)
        .cloned()
        .collect();
    for c in colls {
        let Some(d) = HidDev::open(&c) else { continue };
        let bt = c.in_len() > 64;
        if bt {
            let _ = d.get_feature(0x05); // switches the controller to full reports
        }
        let (mut pct, mut charging, mut found) = (-1, false, false);
        let end = Deadline::in_ms(500);
        while !found && !end.passed() {
            let Some(r) = d.read(100).filter(|r| r.len() >= 10) else { continue };
            match decode_report(c.pid, bt, &r) {
                Report::Battery(p, ch) => {
                    (pct, charging, found) = (p, ch, true);
                }
                Report::NoController => break, // wireless adapter without a controller
                Report::Other => {}
            }
        }
        if !found {
            continue;
        }
        let s = DeviceStatus {
            key: format!("ps:{}:{}", hex4(c.pid), fnv1a(&c.path)),
            name: if is_ds4(c.pid) {
                "DualShock 4"
            } else if c.pid == 0x0DF2 {
                "DualSense Edge"
            } else {
                "DualSense"
            }
            .into(),
            kind: DeviceKind::Gamepad,
            via: if bt {
                "Bluetooth"
            } else if c.pid == 0x0BA0 {
                "Wireless adapter"
            } else {
                "USB cable"
            }
            .into(),
            percent: pct,
            charging,
            full: charging && pct >= 100,
            vid: 0x054C,
            hw: Some((c.vid, c.pid)),
            ..Default::default()
        };
        ctx.out.push(s);
        ctx.claim(c.vid, c.pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(id: u8, len: usize, at: usize, b: u8) -> Vec<u8> {
        let mut r = vec![0u8; len];
        r[0] = id;
        r[at] = b;
        r
    }

    #[test]
    fn dualsense_usb_and_bluetooth() {
        // USB report 01, byte 53: level 6, discharging -> 65%.
        assert_eq!(decode_report(0x0CE6, false, &report(0x01, 64, 53, 0x06)), Report::Battery(65, false));
        // Bluetooth report 31, byte 54: level 3, charging (state 1).
        assert_eq!(decode_report(0x0CE6, true, &report(0x31, 78, 54, 0x13)), Report::Battery(35, true));
        // State 2: fully charged.
        assert_eq!(decode_report(0x0CE6, false, &report(0x01, 64, 53, 0x2A)), Report::Battery(100, true));
        // The reduced Bluetooth report 01 carries no battery.
        assert_eq!(decode_report(0x0CE6, true, &report(0x01, 10, 5, 0x06)), Report::Other);
    }

    #[test]
    fn dualshock4_cable_and_adapter() {
        // Cable plugged (bit 4), level 7 -> 70%, charging.
        assert_eq!(decode_report(0x09CC, false, &report(0x01, 64, 30, 0x17)), Report::Battery(70, true));
        // Level 11 on the cable = full.
        assert_eq!(decode_report(0x09CC, false, &report(0x01, 64, 30, 0x1B)), Report::Battery(100, false));
        // Wireless adapter without a controller.
        assert_eq!(decode_report(0x0BA0, false, &report(0x01, 64, 30, 0x00)), Report::NoController);
    }
}
