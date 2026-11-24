//! BenQ ZOWIE DW-series wireless mice (FK2-DW, U2-DW, S2-DW, EC-DW ...) on the DW receiver
//! (04A5:800A), and the ZOWIE U2 on its receiver (04A5:8008), which uses the same channel.
//!
//! Protocol as documented by zowie-battery-tray (MIT, github.com/IgnacioBarb/zowie-battery-tray,
//! src/ZowieHid.cs and src/Battery.cs, taken from ZOWIE's "FW update tool for ZOWIE DW mouse") and
//! as used by the original C++ BatteryHub (GPL-3.0, src/drivers/zowie.cpp); implemented here from
//! those facts. Two vendor collections on interface 1:
//!
//!   request  output report 08 on FF03 (16 bytes):  08 B4 15 00 ...
//!   reply    input report 09 on FF04 (16 bytes):   09 HH LL 00 C8 ...   HHLL = battery voltage in
//!            hundredths of a volt; 0 = the mouse is not linked (off or asleep)
//!
//! Read-only: this is the only command sent (other ZOWIE commands can reboot the receiver and
//! unpair the mouse). Over the radio only the voltage arrives, so the level is estimated from the
//! mouse's own LED thresholds (10 / 25 / 50 %), and charging on the dock is inferred from how the
//! voltage moves: 50 mV above the lowest reading = on the dock, 20 mV below the highest = off it.
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::HidDev;
use crate::util::{hex4, interpolate, Deadline};

const VID: u16 = 0x04A5;
const DW_RECEIVER: u16 = 0x800A;
const U2_RECEIVER: u16 = 0x8008;
const LEN: u16 = 16;
const REPLY_MS: u64 = 1500;
/// Nothing answered at all: ask again after this.
const SILENT_QUERY_EVERY: Duration = Duration::from_secs(60);

pub fn request() -> [u8; LEN as usize] {
    let mut r = [0u8; LEN as usize];
    r[..3].copy_from_slice(&[0x08, 0xB4, 0x15]);
    r
}

/// Battery voltage in mV from a reply (report id included); Some(0) = mouse not linked.
pub fn parse(r: &[u8]) -> Option<i32> {
    if r.len() < 5 || r[0] != 0x09 || r[1] == 0xE6 || r[4] != 0xC8 {
        return None;
    }
    Some((r[1] as i32 * 256 + r[2] as i32) * 10)
}

/// Level from the voltage; the middle points are the mouse's own LED thresholds.
pub fn percent(mv: i32) -> i32 {
    interpolate(mv, &[4200, 4000, 3890, 3760, 3680, 3500], &[100, 75, 50, 25, 10, 0])
}

/// Charging guess from the voltage trend of one receiver.
#[derive(Default)]
pub struct Trend {
    has: bool,
    charging: bool,
    peak: i32,
    trough: i32,
}

impl Trend {
    pub fn update(&mut self, mv: i32) -> bool {
        if !self.has {
            self.has = true;
            self.charging = mv >= 4250; // a resting Li-ion cell never sits above ~4.2 V
            self.peak = mv;
            self.trough = mv;
        } else if self.charging {
            self.peak = self.peak.max(mv);
            if mv <= self.peak - 20 {
                self.charging = false;
                self.trough = mv;
            }
        } else {
            self.trough = self.trough.min(mv);
            if mv >= self.trough + 50 {
                self.charging = true;
                self.peak = mv;
            }
        }
        self.charging
    }
}

struct State {
    trend: HashMap<u16, Trend>,
    next_try: HashMap<u16, Instant>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn query(ctx: &PollContext, pid: u16) -> Option<Option<i32>> {
    let cmd = ctx.find(VID, pid, |c| c.page() == 0xFF03 && c.out_len() == LEN);
    let rep = ctx.find(VID, pid, |c| c.page() == 0xFF04 && c.in_len() == LEN);
    let (cmd, rep) = (cmd.first()?, rep.first()?);
    // Open the reply channel first so the answer is not missed.
    let input = HidDev::open(rep)?;
    let output = HidDev::open(cmd)?;
    input.flush();
    if !output.write(&request()) {
        return Some(None);
    }
    let end = Deadline::in_ms(REPLY_MS);
    while !end.passed() {
        let Some(r) = input.read(end.left_ms()) else { break };
        if r.first() == Some(&0x09) {
            return Some(parse(&r));
        }
    }
    Some(None)
}

pub fn poll(ctx: &mut PollContext) {
    for pid in [DW_RECEIVER, U2_RECEIVER] {
        if ctx.is_claimed(VID, pid) {
            continue;
        }
        let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
        let st = guard.get_or_insert_with(|| State { trend: HashMap::new(), next_try: HashMap::new() });
        if st.next_try.get(&pid).is_some_and(|t| Instant::now() < *t) {
            continue;
        }
        let Some(answer) = query(ctx, pid) else { continue }; // receiver not present
        let Some(mv) = answer else {
            st.next_try.insert(pid, Instant::now() + SILENT_QUERY_EVERY);
            continue;
        };
        st.next_try.remove(&pid);
        let mut s = DeviceStatus {
            key: format!("zowie:{}", hex4(pid)),
            name: if pid == U2_RECEIVER { "ZOWIE U2".into() } else { "ZOWIE DW mouse".into() },
            kind: DeviceKind::Mouse,
            via: "2.4 GHz receiver".into(),
            hw: Some((VID, pid)),
            grace_sec: 1800,
            ..Default::default()
        };
        if mv == 0 {
            s.connected = false;
        } else {
            s.millivolts = mv;
            s.percent = percent(mv);
            s.charging = st.trend.entry(pid).or_default().update(mv);
        }
        drop(guard);
        ctx.add(s, VID, pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_frame() {
        let r = request();
        assert_eq!(&r[..3], &[0x08, 0xB4, 0x15]);
        assert!(r[3..].iter().all(|&b| b == 0));
    }

    #[test]
    fn replies() {
        let mut r = [0u8; 16];
        r[..5].copy_from_slice(&[0x09, 0x01, 0x86, 0x00, 0xC8]); // 390 -> 3.90 V
        assert_eq!(parse(&r), Some(3900));
        r[1] = 0;
        r[2] = 0;
        assert_eq!(parse(&r), Some(0)); // mouse not linked
        assert_eq!(parse(&[0x09, 0xE6, 0x00, 0x00, 0xC8]), None);
        assert_eq!(parse(&[0x09, 0x01, 0x86, 0x00, 0x00]), None);
        assert_eq!(parse(&[0x08, 0x01, 0x86, 0x00, 0xC8]), None);
        assert_eq!(parse(&[0x09, 0x01]), None);
    }

    #[test]
    fn levels() {
        assert_eq!(percent(4300), 100);
        assert_eq!(percent(3890), 50);
        assert_eq!(percent(3680), 10);
        assert_eq!(percent(3400), 0);
    }

    #[test]
    fn charge_trend() {
        let mut t = Trend::default();
        assert!(!t.update(3900));
        assert!(!t.update(3880));
        assert!(t.update(3940)); // +60 mV over the lowest: on the dock
        assert!(t.update(4300));
        assert!(!t.update(4270)); // -30 mV under the highest: taken off
        let mut t = Trend::default();
        assert!(t.update(4390)); // first reading above 4.25 V
    }
}
