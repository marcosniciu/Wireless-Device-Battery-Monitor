//! Small helpers shared by drivers and the UI (port of the helpers at the end of hid.cpp).
use std::time::{Duration, Instant};

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};

use crate::device::DeviceKind;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

pub fn hex4(v: u16) -> String {
    format!("{v:04X}")
}

pub fn clamp100(v: i32) -> i32 {
    v.clamp(0, 100)
}

/// Linear map lo..hi -> 0..100, clamped.
pub fn map_to_100(v: i32, lo: i32, hi: i32) -> i32 {
    if hi <= lo {
        return clamp100(v);
    }
    clamp100((v - lo) * 100 / (hi - lo))
}

/// Piecewise-linear interpolation through (mv[i], pct[i]); tables sorted by descending voltage.
pub fn interpolate(mv: i32, mv_table: &[i32], pct_table: &[i32]) -> i32 {
    if mv >= mv_table[0] {
        return pct_table[0];
    }
    for i in 1..mv_table.len() {
        if mv >= mv_table[i] {
            let span = (mv_table[i - 1] - mv_table[i]).max(1);
            return pct_table[i] + (pct_table[i - 1] - pct_table[i]) * (mv - mv_table[i]) / span;
        }
    }
    *pct_table.last().unwrap()
}

pub fn guess_kind(name: &str) -> DeviceKind {
    let n = name.to_lowercase();
    let has = |s: &str| n.contains(s);
    if has("mouse") || has("mice") || has("trackball") {
        return DeviceKind::Mouse;
    }
    if has("keyboard") || has("keys") {
        return DeviceKind::Keyboard;
    }
    if has("controller") || has("gamepad") || has("xbox") || has("dualsense") || has("dualshock") || has("joy-con") {
        return DeviceKind::Gamepad;
    }
    if [
        "head", "buds", "pods", "ear", "wh-", "wf-", "audio", "speaker", "arctis", "cloud", "void", "virtuoso",
    ]
    .iter()
    .any(|s| has(s))
    {
        return DeviceKind::Headset;
    }
    DeviceKind::Other
}

/// Simple deadline helper replacing the GetTickCount64 loops of the original.
pub struct Deadline(Instant);

impl Deadline {
    pub fn in_ms(ms: u64) -> Self {
        Self(Instant::now() + Duration::from_millis(ms))
    }
    pub fn left_ms(&self) -> u32 {
        self.0.saturating_duration_since(Instant::now()).as_millis() as u32
    }
    pub fn passed(&self) -> bool {
        Instant::now() >= self.0
    }
}

pub fn sleep_ms(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

/// Diagnostic line in `%TEMP%\bh2.log`, only when BatteryHub runs with the BH_DEBUG environment
/// variable set (the same file the taskbar program logs to).
pub fn debug_log(msg: impl FnOnce() -> String) {
    if std::env::var_os("BH_DEBUG").is_some() {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join("bh2.log")) {
            let _ = writeln!(f, "{}", msg());
        }
    }
}

/// FNV-1a over a string (used for keys that must stay stable but short).
pub fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for ch in s.encode_utf16() {
        h ^= ch as u32;
        h = h.wrapping_mul(16777619);
    }
    h
}

// ---- registry (HKCU) ---------------------------------------------------------------------------

pub fn reg_get_string(subkey: &str, value: &str) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(subkey),
            &HSTRING::from(value),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    r.is_ok().then(|| from_wide(&buf))
}

pub fn reg_get_dword(subkey: &str, value: &str) -> Option<u32> {
    let mut v: u32 = 0;
    let mut size = 4u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(subkey),
            &HSTRING::from(value),
            windows::Win32::System::Registry::RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    r.is_ok().then_some(v)
}

pub fn reg_get_binary(subkey: &str, value: &str) -> Option<Vec<u8>> {
    let (k, v) = (HSTRING::from(subkey), HSTRING::from(value));
    let mut size = 0u32;
    unsafe {
        RegGetValueW(HKEY_CURRENT_USER, &k, &v, windows::Win32::System::Registry::RRF_RT_REG_BINARY, None, None, Some(&mut size)).ok().ok()?;
        let mut buf = vec![0u8; size as usize];
        RegGetValueW(
            HKEY_CURRENT_USER,
            &k,
            &v,
            windows::Win32::System::Registry::RRF_RT_REG_BINARY,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
        .ok()
        .ok()?;
        buf.truncate(size as usize);
        Some(buf)
    }
}

pub fn reg_set_binary(subkey: &str, value: &str, data: &[u8]) -> bool {
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(subkey),
            &HSTRING::from(value),
            windows::Win32::System::Registry::REG_BINARY.0,
            Some(data.as_ptr().cast()),
            data.len() as u32,
        )
        .is_ok()
    }
}

pub fn reg_set_string(subkey: &str, value: &str, data: &str) -> bool {
    let w = wide(data);
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(subkey),
            &HSTRING::from(value),
            REG_SZ.0,
            Some(w.as_ptr().cast()),
            (w.len() * 2) as u32,
        )
        .is_ok()
    }
}

pub fn reg_delete_value(subkey: &str, value: &str) -> bool {
    unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(subkey), &HSTRING::from(value)).is_ok() }
}

pub fn pcwstr(w: &[u16]) -> PCWSTR {
    PCWSTR(w.as_ptr())
}
