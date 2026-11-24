//! Attack Shark mice (X1, X11 and others) on the Xenta 2.4 GHz receiver (VID 1D57, PID FA60), and
//! Delux M900 Pro / Inphic IN10 mice on its "KP 8K" sibling (PID FA65, see below).
//!
//! Protocol from Attack Shark Battery KDE by Gabriel Marques Ferrarezi (MIT,
//! gitlab.com/gabriel17166/Attack-Shark-Battery-KDE) and its Windows port
//! attackshark-battery-windows by daeoxide (MIT, github.com/daeoxide/attackshark-battery-windows).
//! The receiver's interface 2 (on Windows the collection with usage page 000A, 5-byte input)
//! pushes a frame about every 2 s on its own; nothing has to be sent:
//!
//!   03 MM 40 ST NN
//!   03     report ID
//!   MM     model marker, differs per mouse (E5, 55 on the X11, ...): not checked
//!   40     battery frame
//!   ST     01 on battery, 02 on power (not charging), 03 charging
//!   NN     battery level 0..100 % (frozen and approximate while on power)
//!
//! Link-blink frames (`03 MM 01 00 00`) are not battery frames. Right after the link comes up the
//! mouse may send one `03 MM 40 01 64` (100 %) that is wrong, so a frame counts only once the same
//! state and level were seen twice in a row. No frame within the timeout means the mouse is off
//! (the receiver stays enumerated). With the cable plugged in, the receiver goes silent.
//!
//! The same receiver family's 1D57:FA65 ("KP 8K") carries the Delux M900 Pro and Inphic IN10 mice
//! with the same frames. Delux M900 Pro: event 40 (model 3D / 30) as documented by
//! omarchy-delux-m900 (MIT, github.com/AeonArcHQ/omarchy-delux-m900, docs/re/findings.md, read off
//! DELUX.exe and real frames), event 41 (model 50) captured from hardware by the original C++
//! BatteryHub (GPL-3.0, src/drivers/misc_mice.cpp); Inphic model 73 / 74 from the original's
//! docs/inphic.md (Inphic HUB's model table). On FA65 state 02 means fully charged (100 %), 03
//! charging with the last measured level frozen. Event 41 is also sent by the Attack Shark V6.
use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::HidDev;
use crate::util::hex4;

const VID: u16 = 0x1D57;
const RECEIVER: u16 = 0xFA60;
/// Delux / Inphic "KP 8K" receiver.
const RECEIVER_8K: u16 = 0xFA65;
const ON_BATTERY: u8 = 0x01;
const ON_POWER: u8 = 0x02;
const CHARGING: u8 = 0x03;
/// Frames come about every 2 s; two matching ones are needed.
const WAIT_MS: u64 = 5500;

/// Decodes one input report (report ID included): Some((level, state)).
pub fn decode(r: &[u8]) -> Option<(i32, u8)> {
    if r.len() < 5 || r[0] != 0x03 || (r[2] != 0x40 && r[2] != 0x41) || r[4] > 100 {
        return None;
    }
    Some((r[4] as i32, r[3]))
}

/// Name for a receiver and the model byte (byte 1) of its frames.
pub fn model_name(pid: u16, model: u8) -> &'static str {
    match (pid, model) {
        (RECEIVER_8K, 0x30 | 0x3D | 0x50) => "Delux M900 Pro",
        (RECEIVER_8K, 0x73) => "Inphic IN10R MAX",
        (RECEIVER_8K, 0x74) => "Inphic IN10 MAX",
        (RECEIVER_8K, _) => "Wireless mouse",
        _ => "Attack Shark mouse",
    }
}

/// Picks the reading from the frames seen during one poll: the first one confirmed by an equal
/// frame right after it, else the last one (unless it is the lone 100 % reconnect frame).
pub fn pick(frames: &[(i32, u8)]) -> Option<(i32, u8)> {
    if let Some(w) = frames.windows(2).find(|w| w[0] == w[1]) {
        return Some(w[0]);
    }
    match frames.last() {
        Some(&(100, ON_BATTERY)) if frames.len() == 1 => None,
        other => other.copied(),
    }
}

fn base(pid: u16, model: Option<u8>) -> DeviceStatus {
    DeviceStatus {
        key: format!("attackshark:{}", hex4(pid)),
        name: model_name(pid, model.unwrap_or(0)).into(),
        kind: DeviceKind::Mouse,
        via: "2.4 GHz receiver".into(),
        hw: Some((VID, pid)),
        grace_sec: 60,
        ..Default::default()
    }
}

/// Some(((level, state), model byte of the last battery frame)).
fn read_frames(d: &HidDev) -> Option<((i32, u8), u8)> {
    let end = crate::util::Deadline::in_ms(WAIT_MS);
    let mut frames = Vec::new();
    let mut model = 0;
    loop {
        let left = end.left_ms();
        if left == 0 {
            return pick(&frames).map(|f| (f, model));
        }
        let Some(r) = d.read(left) else { continue };
        if let Some(f) = decode(&r) {
            model = r[1];
            if frames.last() == Some(&f) {
                return Some((f, model)); // confirmed
            }
            frames.push(f);
        }
    }
}

pub fn poll(ctx: &mut PollContext) {
    for (vid, pid) in ctx.products(VID) {
        if pid != RECEIVER && pid != RECEIVER_8K {
            continue;
        }
        // The battery collection: page 000A (or a vendor page) with a 5-byte input report.
        let mut colls = ctx.find(vid, pid, |c| (c.page() == 0x000A || c.page() >= 0xFF00) && c.in_len() >= 5);
        if colls.is_empty() {
            continue;
        }
        colls.sort_by_key(|c| c.page() != 0x000A); // the status collection first
        let mut found = None;
        for c in &colls {
            let Some(d) = HidDev::open_read_only(c) else { continue };
            found = read_frames(&d);
            if found.is_some() {
                break;
            }
        }
        let s = match found {
            // KP 8K receiver: state 02 is "fully charged", shown as 100 %.
            Some(((_, ON_POWER), model)) if pid == RECEIVER_8K => {
                DeviceStatus { percent: 100, full: true, charging: true, detail: "fully charged".into(), ..base(pid, Some(model)) }
            }
            Some(((level, state), model)) => DeviceStatus {
                percent: level,
                charging: state == CHARGING,
                detail: match state {
                    ON_BATTERY => String::new(),
                    ON_POWER => "on power".into(),
                    CHARGING => "charging".into(),
                    other => format!("state {other:02X}"),
                },
                ..base(pid, Some(model))
            },
            None => DeviceStatus { connected: false, ..base(pid, None) },
        };
        ctx.add(s, vid, pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_model_marker() {
        assert_eq!(decode(&[0x03, 0xE5, 0x40, 0x01, 0x4B]), Some((75, ON_BATTERY)));
        assert_eq!(decode(&[0x03, 0x55, 0x40, 0x03, 0x32]), Some((50, CHARGING)));
        assert_eq!(decode(&[0x03, 0x00, 0x40, 0x02, 0x50]), Some((80, ON_POWER)));
    }

    #[test]
    fn delux_and_inphic_frames() {
        // omarchy-delux-m900 findings.md: with the cable / without it.
        assert_eq!(decode(&[0x03, 0x3D, 0x40, 0x03, 0x46]), Some((70, CHARGING)));
        assert_eq!(decode(&[0x03, 0x3D, 0x40, 0x01, 0x4D]), Some((77, ON_BATTERY)));
        // Event 41 (M900 Pro model 50, Inphic, Attack Shark V6).
        assert_eq!(decode(&[0x03, 0x73, 0x41, 0x01, 0x64]), Some((100, ON_BATTERY)));
        assert_eq!(decode(&[0x03, 0x50, 0x41, 0x01, 0x3C]), Some((60, ON_BATTERY)));
        assert_eq!(decode(&[0x03, 0x3D, 0x50, 0x00, 0x06]), None); // command ACK, not power
        assert_eq!(model_name(RECEIVER_8K, 0x3D), "Delux M900 Pro");
        assert_eq!(model_name(RECEIVER_8K, 0x50), "Delux M900 Pro");
        assert_eq!(model_name(RECEIVER_8K, 0x73), "Inphic IN10R MAX");
        assert_eq!(model_name(RECEIVER_8K, 0x99), "Wireless mouse");
        assert_eq!(model_name(RECEIVER, 0x3D), "Attack Shark mouse");
    }

    #[test]
    fn not_battery_frames() {
        assert_eq!(decode(&[0x03, 0xE5, 0x01, 0x00, 0x00]), None);
        assert_eq!(decode(&[0x03, 0xE5, 0x40, 0x01, 0xC8]), None);
        assert_eq!(decode(&[0x03, 0xE5, 0x40]), None);
        assert_eq!(decode(&[0x02, 0xE5, 0x40, 0x01, 0x32]), None);
    }

    #[test]
    fn confirmation() {
        // Reconnect frame alone is not trusted; the real level confirmed twice wins.
        assert_eq!(pick(&[(100, ON_BATTERY)]), None);
        assert_eq!(pick(&[(100, ON_BATTERY), (62, ON_BATTERY), (62, ON_BATTERY)]), Some((62, ON_BATTERY)));
        assert_eq!(pick(&[(62, ON_BATTERY)]), Some((62, ON_BATTERY)));
        assert_eq!(pick(&[]), None);
    }
}
