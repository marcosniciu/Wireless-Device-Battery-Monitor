//! When a mouse last moved, per receiver, from Windows' raw input (read-only).
//!
//! Some receivers keep answering for a mouse that is switched off, with its last level (the
//! Redragon M719 receiver does): the only sign that the mouse is off is that nothing moves. The
//! taskbar window registers for raw mouse input; each message only notes the time and which
//! device sent it (no positions or buttons are kept).
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM};
use windows::Win32::UI::Input::*;

static WATCHING: AtomicBool = AtomicBool::new(false);
static REDRAGON_LAST: AtomicU64 = AtomicU64::new(0);
static NAMES: Mutex<Option<HashMap<usize, bool>>> = Mutex::new(None);

/// Starts receiving raw mouse input on `hwnd` (also while another app has the focus).
pub fn watch(hwnd: HWND) {
    let dev = RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x02, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd };
    if unsafe { RegisterRawInputDevices(&[dev], size_of::<RAWINPUTDEVICE>() as u32) }.is_ok() {
        WATCHING.store(true, Ordering::Relaxed);
    }
}

fn is_redragon(h: HANDLE) -> bool {
    let mut names = NAMES.lock().unwrap();
    let names = names.get_or_insert_with(HashMap::new);
    *names.entry(h.0 as usize).or_insert_with(|| {
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let n = unsafe { GetRawInputDeviceInfoW(Some(h), RIDI_DEVICENAME, Some(buf.as_mut_ptr().cast()), &mut len) };
        n != u32::MAX && String::from_utf16_lossy(&buf[..(n as usize).min(512)]).to_ascii_uppercase().contains("VID_25A7&PID_FA7E")
    })
}

/// Handles WM_INPUT. True when a mouse that had been still long enough to be shown off moved
/// again (the caller re-reads the devices so it comes back at once).
pub fn on_input(lp: LPARAM) -> bool {
    let mut hdr = RAWINPUTHEADER::default();
    let mut size = size_of::<RAWINPUTHEADER>() as u32;
    let n = unsafe { GetRawInputData(HRAWINPUT(lp.0 as _), RID_HEADER, Some((&mut hdr as *mut RAWINPUTHEADER).cast()), &mut size, size_of::<RAWINPUTHEADER>() as u32) };
    if n == u32::MAX || hdr.dwType != RIM_TYPEMOUSE.0 || !is_redragon(hdr.hDevice) {
        return false;
    }
    let now = crate::history::now_secs();
    let before = REDRAGON_LAST.swap(now, Ordering::Relaxed);
    now.saturating_sub(before) >= REDRAGON_IDLE_SECS
}

/// A Redragon mouse that has not moved for this long is shown switched off.
pub const REDRAGON_IDLE_SECS: u64 = 15 * 60;

/// Seconds since the Redragon mouse last moved; None when raw input is not watched (console
/// tools), so the caller does not use it.
pub fn redragon_idle() -> Option<u64> {
    WATCHING.load(Ordering::Relaxed).then(|| crate::history::now_secs().saturating_sub(REDRAGON_LAST.load(Ordering::Relaxed)))
}
