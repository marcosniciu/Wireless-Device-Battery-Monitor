//! Win32 / GDI helpers (fonts, colours, background polling, autostart).
use std::ffi::c_void;

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW,
    GetTextExtentPoint32W, SelectObject, SetBkMode, SetTextColor, ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER,
    HDC, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS, TRANSPARENT,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, PostMessageW, HICON, ICONINFO};

use crate::device::DeviceStatus;
use crate::util;


/// Windows "apps / taskbar use light theme" setting.
pub fn light_theme(value: &str) -> bool {
    let mut v: u32 = 0;
    let mut size = 4u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            &HSTRING::from(value),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    r.is_ok() && v != 0
}

pub fn taskbar_is_light() -> bool {
    light_theme("SystemUsesLightTheme")
}

pub fn apps_are_light() -> bool {
    light_theme("AppsUseLightTheme")
}

pub const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

pub fn split(c: COLORREF) -> (u8, u8, u8) {
    (c.0 as u8, (c.0 >> 8) as u8, (c.0 >> 16) as u8)
}

/// Green lightning bolt: the device is charging.
pub fn charging_color(light: bool) -> COLORREF {
    if light { rgb(16, 124, 16) } else { rgb(108, 203, 95) }
}

/// Blue lightning bolt: charged to 100% while still on power.
pub fn full_color(light: bool) -> COLORREF {
    if light { rgb(0, 120, 212) } else { rgb(96, 205, 255) }
}

/// Red: at or below the device's warning level.
pub fn low_color(light: bool) -> COLORREF {
    if light { rgb(196, 30, 40) } else { rgb(255, 90, 90) }
}

/// Bolt colour for a device on power, None when it runs on battery.
pub fn bolt_color(s: &DeviceStatus, light: bool) -> Option<COLORREF> {
    if s.is_full() {
        Some(full_color(light))
    } else if s.is_charging() {
        Some(charging_color(light))
    } else {
        None
    }
}

/// Colour of a battery level: red when low (per-device `threshold`), green while charging, blue
/// when full, grey when asleep, otherwise `normal`.
pub fn level_color(s: &DeviceStatus, light: bool, normal: COLORREF, threshold: i32) -> COLORREF {
    if s.asleep || !s.connected {
        return if light { rgb(140, 140, 140) } else { rgb(130, 130, 130) };
    }
    if let Some(c) = bolt_color(s, light) {
        return c;
    }
    if crate::settings::is_low(s, threshold) {
        return low_color(light);
    }
    normal
}

pub fn font(height: i32, weight: i32, face: &str) -> HFONT {
    unsafe {
        CreateFontW(
            -height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            0,
            &HSTRING::from(face),
        )
    }
}

pub fn text_width(dc: HDC, text: &str) -> i32 {
    let w: Vec<u16> = text.encode_utf16().collect();
    let mut sz = SIZE::default();
    unsafe {
        let _ = GetTextExtentPoint32W(dc, &w, &mut sz);
    }
    sz.cx
}

/// Draws `text` with DrawTextW into `rc`.
pub fn draw_text(dc: HDC, text: &str, rc: &mut RECT, flags: windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT) {
    let mut w: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        DrawTextW(dc, &mut w, rc, flags | DT_NOPREFIX);
    }
}

/// Renders a square tray icon: the level number (or `?`) in `color`, with a thin level bar below.
/// GDI text has no alpha, so it is drawn white-on-black and the luminance becomes the alpha.
pub fn number_icon(text: &str, color: COLORREF, level: i32, size: i32, light: bool) -> Option<HICON> {
    unsafe {
        let dc = CreateCompatibleDC(None);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let Ok(bmp) = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(dc);
            return None;
        };
        let old = SelectObject(dc, HGDIOBJ(bmp.0));
        let px = std::slice::from_raw_parts_mut(bits as *mut u32, (size * size) as usize);
        px.fill(0);

        // Leave room for the level bar at the bottom.
        let bar_h = (size / 8).max(2);
        let text_h = size - bar_h - 1;
        let mut height = text_h + size / 6;
        let mut f = font(height, 700, "Segoe UI");
        let mut prev = SelectObject(dc, HGDIOBJ(f.0));
        while height > 6 && text_width(dc, text) > size {
            SelectObject(dc, prev);
            let _ = DeleteObject(HGDIOBJ(f.0));
            height -= 1;
            f = font(height, 700, "Segoe UI");
            prev = SelectObject(dc, HGDIOBJ(f.0));
        }
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, rgb(255, 255, 255));
        let mut rc = RECT { left: -2, top: -1, right: size + 2, bottom: text_h };
        draw_text(dc, text, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        SelectObject(dc, prev);
        let _ = DeleteObject(HGDIOBJ(f.0));

        let (r, g, b) = split(color);
        for p in px.iter_mut() {
            let a = (*p & 0xFF).max((*p >> 8) & 0xFF).max((*p >> 16) & 0xFF);
            *p = premul(r, g, b, a as u8);
        }
        // Level bar: faint track plus a filled part.
        if level >= 0 {
            let track = if light { (0u8, 0u8, 0u8, 70u8) } else { (255, 255, 255, 70) };
            let fill_w = ((size - 2) * level.clamp(0, 100) + 50) / 100;
            for y in size - bar_h..size {
                for x in 1..size - 1 {
                    px[(y * size + x) as usize] = if x - 1 < fill_w.max(1) {
                        premul(r, g, b, 255)
                    } else {
                        premul(track.0, track.1, track.2, track.3)
                    };
                }
            }
        }
        SelectObject(dc, old);
        let _ = DeleteDC(dc);

        let mask = CreateBitmap(size, size, 1, 1, None);
        let info = ICONINFO { fIcon: true.into(), hbmColor: bmp, hbmMask: mask, ..Default::default() };
        let icon = CreateIconIndirect(&info).ok();
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteObject(HGDIOBJ(mask.0));
        icon
    }
}

fn premul(r: u8, g: u8, b: u8, a: u8) -> u32 {
    let m = |c: u8| (c as u32 * a as u32 / 255) & 0xFF;
    (a as u32) << 24 | m(r) << 16 | m(g) << 8 | m(b)
}

// ---- dark / light menus and tooltips --------------------------------------------------------

type SetPreferredAppMode = unsafe extern "system" fn(i32) -> i32;
type FlushMenuThemes = unsafe extern "system" fn();
type AllowDarkModeForWindow = unsafe extern "system" fn(HWND, i32) -> i32;

struct UxTheme {
    set_mode: Option<SetPreferredAppMode>,
    flush: Option<FlushMenuThemes>,
    allow_window: Option<AllowDarkModeForWindow>,
}

/// uxtheme.dll's unnamed exports (ordinals 133, 135, 136) that Explorer itself uses for dark
/// context menus (Windows 10 1903 and later). Missing ones are simply skipped.
fn uxtheme() -> &'static UxTheme {
    static UX: std::sync::OnceLock<UxTheme> = std::sync::OnceLock::new();
    UX.get_or_init(|| unsafe {
        use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};
        let Ok(m) = LoadLibraryExW(windows::core::w!("uxtheme.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) else {
            return UxTheme { set_mode: None, flush: None, allow_window: None };
        };
        let ord = |n: usize| GetProcAddress(m, windows::core::PCSTR(n as *const u8));
        UxTheme {
            allow_window: ord(133).map(|f| std::mem::transmute::<_, AllowDarkModeForWindow>(f)),
            set_mode: ord(135).map(|f| std::mem::transmute::<_, SetPreferredAppMode>(f)),
            flush: ord(136).map(|f| std::mem::transmute::<_, FlushMenuThemes>(f)),
        }
    })
}

/// Makes the process's popup menus dark or light (to match the taskbar they open from).
pub fn set_menu_theme(owner: HWND, dark: bool) {
    let ux = uxtheme();
    unsafe {
        if let Some(f) = ux.allow_window {
            f(owner, dark as i32);
        }
        if let Some(f) = ux.set_mode {
            f(if dark { 2 } else { 3 }); // ForceDark / ForceLight
        }
        if let Some(f) = ux.flush {
            f();
        }
    }
}

/// Dark or light tooltip (the "DarkMode_Explorer" theme class Explorer uses).
pub fn set_tooltip_theme(tip: HWND, dark: bool) {
    unsafe {
        let theme = if dark { windows::core::w!("DarkMode_Explorer") } else { windows::core::w!("Explorer") };
        let _ = windows::Win32::UI::Controls::SetWindowTheme(tip, theme, PCWSTR::null());
    }
}

// ---- background polling ------------------------------------------------------------------------

/// Runs `poll_devices(rescan)` on a worker thread and posts the result to `hwnd` as `msg`
/// (LPARAM = Box<Vec<DeviceStatus>> raw pointer; the receiver must take it back with `take_result`).
pub fn poll_async(hwnd: HWND, msg: u32, rescan: bool) {
    run_async(hwnd, msg, move || crate::device::poll_devices(rescan));
}

/// Runs `read` on a worker thread and posts its result to `hwnd` as `msg` (see `take_result`).
pub fn run_async(hwnd: HWND, msg: u32, read: impl FnOnce() -> Vec<DeviceStatus> + Send + 'static) {
    let h = hwnd.0 as usize;
    std::thread::spawn(move || {
        let devices = read();
        let ptr = Box::into_raw(Box::new(devices));
        unsafe {
            if PostMessageW(Some(HWND(h as *mut c_void)), msg, WPARAM(0), LPARAM(ptr as isize)).is_err() {
                drop(Box::from_raw(ptr));
            }
        }
    });
}

/// # Safety
/// `lparam` must come from a message posted by `poll_async`.
pub unsafe fn take_result(lparam: LPARAM) -> Vec<DeviceStatus> {
    unsafe { *Box::from_raw(lparam.0 as *mut Vec<DeviceStatus>) }
}

// ---- autostart ---------------------------------------------------------------------------------

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

pub fn exe_path() -> String {
    let mut buf = [0u16; 1024];
    let n = unsafe { windows::Win32::System::LibraryLoader::GetModuleFileNameW(None, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n])
}

pub fn autostart_enabled(name: &str) -> bool {
    util::reg_get_string(RUN_KEY, name).is_some_and(|v| v.to_lowercase().contains(&exe_path().to_lowercase()))
}

pub fn set_autostart(name: &str, on: bool) {
    if on {
        util::reg_set_string(RUN_KEY, name, &format!("\"{}\"", exe_path()));
    } else {
        util::reg_delete_value(RUN_KEY, name);
    }
}

pub fn wstr(s: &str) -> HSTRING {
    HSTRING::from(s)
}

pub fn pcw(h: &HSTRING) -> PCWSTR {
    PCWSTR(h.as_ptr())
}

/// Copies `s` into a fixed UTF-16 buffer (truncating), as NOTIFYICONDATAW wants.
pub fn fill_wide<const N: usize>(dst: &mut [u16; N], s: &str) {
    dst.fill(0);
    for (i, c) in s.encode_utf16().take(N - 1).enumerate() {
        dst[i] = c;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(percent: i32, charging: bool, full: bool) -> DeviceStatus {
        DeviceStatus { percent, charging, full, ..Default::default() }
    }

    #[test]
    fn bolt_is_green_while_charging_and_blue_when_full() {
        for light in [false, true] {
            assert_eq!(bolt_color(&dev(40, true, false), light), Some(charging_color(light)));
            assert_eq!(bolt_color(&dev(99, true, false), light), Some(charging_color(light)));
            assert_eq!(bolt_color(&dev(100, true, false), light), Some(full_color(light)));
            assert_eq!(bolt_color(&dev(100, true, true), light), Some(full_color(light)));
            assert_eq!(bolt_color(&dev(-1, false, true), light), Some(full_color(light)));
            assert_eq!(bolt_color(&dev(80, false, false), light), None);
        }
    }

    #[test]
    fn red_only_at_or_below_the_chosen_level_and_never_while_charging() {
        let normal = rgb(1, 2, 3);
        assert_eq!(level_color(&dev(30, false, false), false, normal, 30), low_color(false));
        assert_eq!(level_color(&dev(31, false, false), false, normal, 30), normal);
        assert_eq!(level_color(&dev(10, true, false), false, normal, 30), charging_color(false));
        assert_eq!(level_color(&dev(50, false, false), false, normal, 50), low_color(false));
    }
}
