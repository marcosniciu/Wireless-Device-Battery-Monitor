//! Battery levels of the last 24 hours, per device, for the hover card's chart.
//!
//! A point is kept when the level changes, and otherwise at most every 10 minutes, so a device
//! has at most ~150 points (a few hundred bytes). Points older than 24 hours are dropped, and a
//! device without points is forgotten. The history is saved as one REG_BINARY value in the
//! settings key, so it survives a restart and the uninstaller removes it with the settings.
//!
//! Format: for each device `key length (u8) | key (UTF-8) | point count (u16 LE) |
//! points (time u32 LE, unix seconds; level u8)`.
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// How long points are kept.
pub const KEEP_SECS: u64 = 24 * 3600;
/// An unchanged level is stored again after this long (keeps the line continuous).
const STEP_SECS: u64 = 600;
const VALUE: &str = "History";

#[derive(Default, Debug, PartialEq)]
pub struct History {
    devices: HashMap<String, Vec<(u64, u8)>>,
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl History {
    /// Adds a reading; returns true when a point was stored.
    pub fn record(&mut self, key: &str, now: u64, level: i32) -> bool {
        if !(0..=100).contains(&level) {
            return false;
        }
        let pts = self.devices.entry(key.to_string()).or_default();
        let keep = match pts.last() {
            Some(&(t, l)) => l as i32 != level || now.saturating_sub(t) >= STEP_SECS || now < t,
            None => true,
        };
        if keep {
            pts.push((now, level as u8));
        }
        keep
    }

    /// Drops points older than 24 hours and devices left without points. True if anything went.
    pub fn prune(&mut self, now: u64) -> bool {
        let from = now.saturating_sub(KEEP_SECS);
        let before: usize = self.devices.values().map(Vec::len).sum::<usize>() + self.devices.len();
        for pts in self.devices.values_mut() {
            pts.retain(|&(t, _)| t >= from && t <= now + 300);
        }
        self.devices.retain(|_, p| !p.is_empty());
        before != self.devices.values().map(Vec::len).sum::<usize>() + self.devices.len()
    }

    /// Points of one device, oldest first.
    pub fn points(&self, key: &str) -> &[(u64, u8)] {
        self.devices.get(key).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut keys: Vec<&String> = self.devices.keys().collect();
        keys.sort();
        let mut out = Vec::new();
        for k in keys {
            let kb = k.as_bytes();
            if kb.len() > 255 {
                continue;
            }
            let pts = &self.devices[k];
            let pts = &pts[pts.len().saturating_sub(u16::MAX as usize)..];
            out.push(kb.len() as u8);
            out.extend_from_slice(kb);
            out.extend_from_slice(&(pts.len() as u16).to_le_bytes());
            for &(t, l) in pts {
                out.extend_from_slice(&(t.min(u32::MAX as u64) as u32).to_le_bytes());
                out.push(l);
            }
        }
        out
    }

    /// Reads the format written by `encode`; stops at the first damaged entry.
    pub fn decode(b: &[u8]) -> Self {
        let mut h = History::default();
        let mut i = 0;
        while i < b.len() {
            let kl = b[i] as usize;
            let Some(key) = b.get(i + 1..i + 1 + kl).and_then(|k| std::str::from_utf8(k).ok()) else { break };
            i += 1 + kl;
            let Some(n) = b.get(i..i + 2).map(|n| u16::from_le_bytes([n[0], n[1]]) as usize) else { break };
            i += 2;
            let Some(raw) = b.get(i..i + n * 5) else { break };
            i += n * 5;
            let pts: Vec<(u64, u8)> = raw
                .chunks_exact(5)
                .map(|c| (u32::from_le_bytes([c[0], c[1], c[2], c[3]]) as u64, c[4].min(100)))
                .collect();
            if !pts.is_empty() {
                h.devices.insert(key.to_string(), pts);
            }
        }
        h
    }

    pub fn load(settings_key: &str) -> Self {
        crate::util::reg_get_binary(settings_key, VALUE).map(|b| Self::decode(&b)).unwrap_or_default()
    }

    pub fn save(&self, settings_key: &str) {
        let _ = crate::util::reg_set_binary(settings_key, VALUE, &self.encode());
    }
}

/// Seconds until the battery is empty (on battery) or full (charging), from the latest run of the
/// history in one direction: the level must have moved at least 3 points over at least 20
/// minutes. None when there is not enough history or the estimate is beyond 7 days.
pub fn estimate(points: &[(u64, u8)], now: u64, percent: i32, charging: bool) -> Option<u64> {
    if !(0..=100).contains(&percent) || (charging && percent >= 100) || (!charging && percent <= 0) {
        return None;
    }
    // Walk back while the level keeps moving the same way (up when charging, down otherwise).
    let mut start = *points.last()?;
    for &(t, l) in points.iter().rev().skip(1) {
        let same_way = if charging { l <= start.1 } else { l >= start.1 };
        if !same_way || t > start.0 {
            break;
        }
        start = (t, l);
    }
    let moved = (percent - start.1 as i32).unsigned_abs() as u64;
    let dt = now.saturating_sub(start.0);
    if moved < 3 || dt < 20 * 60 {
        return None;
    }
    let left = if charging { 100 - percent } else { percent } as u64;
    let secs = left * dt / moved;
    (secs <= 7 * 24 * 3600).then_some(secs)
}

/// "≈ 9 h", "≈ 1 h 10 min", "≈ 40 min", "≈ 2 dias".
pub fn duration_pt(secs: u64) -> String {
    let min = (secs + 30) / 60;
    if min < 60 {
        format!("≈ {} min", min.max(1))
    } else if min < 10 * 60 {
        let m = (min % 60) / 10 * 10;
        if m == 0 { format!("≈ {} h", min / 60) } else { format!("≈ {} h {m} min", min / 60) }
    } else if min < 48 * 60 {
        format!("≈ {} h", (min + 30) / 60)
    } else {
        format!("≈ {} dias", (min + 720) / 1440)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates() {
        // Charged to 80 % at 1 h, then 80 -> 70 % in 3 hours: 70 % left at 10 % / 3 h = 21 h.
        let pts = [(0, 50), (3600, 80), (7200, 75), (14400, 70)];
        assert_eq!(estimate(&pts, 14400, 70, false), Some(21 * 3600));
        // Charging 40 -> 60 in 1 hour: 40 % to go = 2 h.
        let up = [(0, 40), (1800, 50), (3600, 60)];
        assert_eq!(estimate(&up, 3600, 60, true), Some(2 * 3600));
        // Too little history.
        assert_eq!(estimate(&[(0, 50)], 600, 50, false), None);
        assert_eq!(estimate(&[(0, 51), (60, 50)], 2000, 50, false), None);
        assert_eq!(estimate(&up, 3600, 100, true), None);
        assert_eq!(duration_pt(40 * 60), "≈ 40 min");
        assert_eq!(duration_pt(70 * 60), "≈ 1 h 10 min");
        assert_eq!(duration_pt(9 * 3600 + 100), "≈ 9 h");
        assert_eq!(duration_pt(14 * 3600), "≈ 14 h");
        assert_eq!(duration_pt(3 * 24 * 3600), "≈ 3 dias");
    }

    #[test]
    fn keeps_changes_and_every_ten_minutes() {
        let mut h = History::default();
        assert!(h.record("m", 1000, 80));
        assert!(!h.record("m", 1060, 80)); // same level, 1 min later
        assert!(h.record("m", 1120, 79)); // changed
        assert!(h.record("m", 1120 + 600, 79)); // unchanged but 10 min later
        assert!(!h.record("m", 2000, -1)); // unknown level is not stored
        assert_eq!(h.points("m").len(), 3);
    }

    #[test]
    fn prunes_after_24_hours() {
        let mut h = History::default();
        h.record("a", 10, 50);
        h.record("a", 10 + KEEP_SECS, 40);
        h.record("b", 20, 90);
        assert!(h.prune(10 + KEEP_SECS + 15)); // 24 h back is t=25: drops a@10 and b@20
        assert_eq!(h.points("a"), &[(10 + KEEP_SECS, 40)]);
        assert!(h.points("b").is_empty());
        assert!(!h.prune(10 + KEEP_SECS + 16));
    }

    #[test]
    fn round_trip_and_damaged_data() {
        let mut h = History::default();
        h.record("logi:unit:ABC", 1_700_000_000, 85);
        h.record("logi:unit:ABC", 1_700_000_600, 84);
        h.record("steelseries:2269", 1_700_000_100, 99);
        let b = h.encode();
        assert_eq!(History::decode(&b), h);
        // A cut-off value keeps what was complete.
        let cut = History::decode(&b[..b.len() - 3]);
        assert!(cut.points("logi:unit:ABC").len() == 2 || cut.points("steelseries:2269").is_empty());
        assert_eq!(History::decode(&[]), History::default());
    }
}
