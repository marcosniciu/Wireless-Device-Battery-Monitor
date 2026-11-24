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

#[cfg(test)]
mod tests {
    use super::*;

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
