//! AULA mice (X9 Ultra, V9 / V9 Max+ and others) on the "AULA-8K Dongle" (VID 320F, PID 22A0). The
//! receiver id is shared by several models, so the mouse gets a generic name.
//!
//! Protocol as documented by HaloBattery (MIT, github.com/HeyOkay/HaloBattery, providers/lofree.py:
//! measured on a V9 Max+ against AULA HUB and on the hardware) and the original C++ BatteryHub
//! (GPL-3.0, src/drivers/aula_mouse.cpp and aula_mouse_protocol.h, verified on an X9 Ultra on this
//! receiver); implemented here from those facts. Vendor collection FF1C:0092 on interface 1,
//! 64-byte input / output report 04. Each read is a small transaction:
//!
//!   host  04 00 00 01 00 ...        START       reply  04 00 00 01 00 00 00 00 ...
//!   host  04 00 00 1A 00 ...        battery     reply  04 00 00 1A 00 00 00 00 LL CC
//!   host  04 00 00 02 00 ...        END         reply  04 00 00 02 00 00 00 00 ...
//!         LL = level 1..100 (0 = the receiver has no reading yet), CC = 0 on battery,
//!         1 charging, 2 charged (both on the cable)
//!
//! The receiver answers from its cache whether or not the mouse is on. While the link is up it pushes
//! `04 AA 00 AA 00 00 00 00 SS` (SS FF = mouse on, 00 = off); a 00 seen during the read marks the mouse
//! disconnected. END is always sent, so no transaction is left open. Nothing else is written.
use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::{self, HidColl, HidDev};
use crate::util::Deadline;

const VID: u16 = 0x320F;
const PID: u16 = 0x22A0;
const REPORT: u8 = 0x04;
const START: u8 = 0x01;
const END: u8 = 0x02;
const BATTERY: u8 = 0x1A;

pub fn request(cmd: u8) -> [u8; 64] {
    let mut r = [0u8; 64];
    r[0] = REPORT;
    r[3] = cmd;
    r
}

#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    /// Reply to a command (the command byte).
    Reply(u8),
    /// Battery reply: level, charging (or charged) on the cable. None: no reading yet.
    Battery(Option<(i32, bool)>),
    /// Pushed link state: true while the mouse is on.
    Link(bool),
}

/// Decodes one input report (report ID included).
pub fn decode(r: &[u8]) -> Option<Frame> {
    if r.len() >= 9 && r[0] == REPORT && r[1] == 0xAA && r[2] == 0 && r[3] == 0xAA && (r[8] == 0 || r[8] == 0xFF) {
        return Some(Frame::Link(r[8] == 0xFF));
    }
    if r.len() < 8 || r[0] != REPORT || r[1] != 0 || r[2] != 0 || r[4..8] != [0, 0, 0, 0] {
        return None;
    }
    if r[3] != BATTERY {
        return Some(Frame::Reply(r[3]));
    }
    if r.len() < 10 {
        return None;
    }
    let (level, state) = (r[8], r[9]);
    Some(Frame::Battery((level >= 1 && level <= 100 && state <= 2).then(|| (level as i32, state != 0))))
}

enum Reading {
    Battery(i32, bool),
    /// The receiver answered, but the mouse is off or has not reported yet.
    Away,
}

/// Waits for the reply to `cmd`; Some(link-down seen) when it came, with the battery data.
fn wait(d: &HidDev, cmd: u8, end: &Deadline, link_down: &mut bool) -> Option<Option<Option<(i32, bool)>>> {
    while !end.passed() {
        match d.read(end.left_ms().min(100)).and_then(|r| decode(&r)) {
            Some(Frame::Link(on)) => *link_down = !on,
            Some(Frame::Reply(c)) if c == cmd => return Some(None),
            Some(Frame::Battery(b)) if cmd == BATTERY => return Some(Some(b)),
            _ => {}
        }
    }
    None
}

fn read(c: &HidColl) -> Option<Reading> {
    let d = HidDev::open(c)?;
    d.flush();
    let mut link_down = false;
    let mut battery = None;
    let mut answered = false;
    if d.write_timeout(&request(START), 150) && wait(&d, START, &Deadline::in_ms(450), &mut link_down).is_some() {
        answered = true;
        if d.write_timeout(&request(BATTERY), 150) {
            battery = wait(&d, BATTERY, &Deadline::in_ms(350), &mut link_down).flatten().flatten();
        }
    }
    // End even after a missing acknowledgement, to leave no transaction open.
    if d.write_timeout(&request(END), 100) {
        wait(&d, END, &Deadline::in_ms(200), &mut link_down);
    }
    match battery {
        Some((level, charging)) if !link_down => Some(Reading::Battery(level, charging)),
        _ if answered => Some(Reading::Away),
        _ => None,
    }
}

pub fn poll(ctx: &mut PollContext) {
    if ctx.is_claimed(VID, PID) {
        return;
    }
    let colls = ctx.find(VID, PID, |c| c.iface == 1 && c.page() == 0xFF1C && c.usage() == 0x0092 && c.in_len() == 64 && c.out_len() == 64);
    let Some(c) = colls.iter().find(|c| hid::product_string(c).trim() == "AULA-8K Dongle") else { return };
    let base = DeviceStatus {
        key: "aula_mouse:320f:22a0".into(),
        name: "AULA wireless mouse".into(),
        kind: DeviceKind::Mouse,
        via: "2.4 GHz receiver".into(),
        hw: Some((VID, PID)),
        grace_sec: 1800,
        ..Default::default()
    };
    let s = match read(c) {
        Some(Reading::Battery(level, charging)) => DeviceStatus { percent: level, charging, ..base },
        _ => DeviceStatus { connected: false, ..base },
    };
    ctx.add(s, VID, PID);
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
    fn requests() {
        assert_eq!(request(BATTERY)[..5], [0x04, 0x00, 0x00, 0x1A, 0x00]);
    }

    #[test]
    fn battery_replies() {
        // HaloBattery's measured V9 Max+ frames: 89 %, and 100 / 99 / 100 % idle, charging, charged.
        assert_eq!(decode(&frame(&[0x04, 0, 0, 0x1A, 0, 0, 0, 0, 0x59, 0x00])), Some(Frame::Battery(Some((89, false)))));
        assert_eq!(decode(&frame(&[0x04, 0, 0, 0x1A, 0, 0, 0, 0, 0x63, 0x01])), Some(Frame::Battery(Some((99, true)))));
        assert_eq!(decode(&frame(&[0x04, 0, 0, 0x1A, 0, 0, 0, 0, 0x64, 0x02])), Some(Frame::Battery(Some((100, true)))));
    }

    #[test]
    fn no_reading_yet() {
        assert_eq!(decode(&frame(&[0x04, 0, 0, 0x1A, 0, 0, 0, 0, 0x00, 0x00])), Some(Frame::Battery(None)));
        assert_eq!(decode(&frame(&[0x04, 0, 0, 0x1A, 0, 0, 0, 0, 0x65, 0x00])), Some(Frame::Battery(None)));
        assert_eq!(decode(&frame(&[0x04, 0, 0, 0x1A, 0, 0, 0, 0, 0x50, 0x03])), Some(Frame::Battery(None)));
    }

    #[test]
    fn handshake_and_link() {
        assert_eq!(decode(&frame(&[0x04, 0, 0, 0x01])), Some(Frame::Reply(START)));
        assert_eq!(decode(&frame(&[0x04, 0, 0, 0x02])), Some(Frame::Reply(END)));
        assert_eq!(decode(&frame(&[0x04, 0xAA, 0, 0xAA, 0, 0, 0, 0, 0xFF])), Some(Frame::Link(true)));
        assert_eq!(decode(&frame(&[0x04, 0xAA, 0, 0xAA, 0, 0, 0, 0, 0x00])), Some(Frame::Link(false)));
    }

    #[test]
    fn not_ours() {
        assert_eq!(decode(&frame(&[0x05, 0, 0, 0x1A, 0, 0, 0, 0, 0x50])), None);
        assert_eq!(decode(&frame(&[0x04, 0, 0, 0x1A, 1, 0, 0, 0, 0x50])), None);
        assert_eq!(decode(&[0x04, 0, 0]), None);
    }
}
