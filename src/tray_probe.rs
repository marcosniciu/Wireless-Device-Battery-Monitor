//! Where the notification-area items (clock, bell...) start on a Windows 11 taskbar that has no
//! window for them. Windows 11 draws the secondary taskbars' clock in XAML, so there is no
//! `ClockButton` window to place the widget against; UI Automation still exposes those items
//! (automation id `SystemTrayIcon`) with their screen rectangles.
//!
//! UI Automation is asked from a worker thread (the widget's own window is a child of the taskbar,
//! so asking from the UI thread could wait on itself), and only when a taskbar is new, moved or
//! resized, when `taskbars_changed` says the main taskbar's notification area changed (the
//! notification bell appears on every taskbar at once and moves the clock), and every minute. The answer is
//! kept as a distance from the taskbar's right end, so it stays right while the taskbar is checked
//! again after a move or resize. When an answer arrives, `notify` gets `msg` so the widget can be
//! placed at once.
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation, TreeScope_Descendants};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

const REFRESH: Duration = Duration::from_secs(60);
/// A first answer normally takes well under a second; after this, give up waiting for it.
const FIRST_WAIT: Duration = Duration::from_secs(2);
const TRAY_ID: &str = "SystemTrayIcon";

/// Where a taskbar's notification area begins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayEdge {
    /// Screen x of the first notification-area item.
    Known(i32),
    /// First check still running: keep the widget hidden for now.
    Pending,
    /// UI Automation did not tell: use an estimate.
    Unknown,
}

struct Entry {
    bar: (i32, i32, i32, i32),
    /// Distance of the first item from the taskbar's right end.
    from_right: Option<i32>,
    at: Instant,
    /// Check again at the next call (the answer is kept until then).
    dirty: bool,
}

#[derive(Default)]
struct Cache {
    known: HashMap<isize, Entry>,
    /// First request for a taskbar not answered yet.
    asked: HashMap<isize, Instant>,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);

/// Where the notification area of `bar` (screen rectangle `bar_rect`) begins. Starts a background
/// check when the answer is missing, stale, or the taskbar changed.
pub fn tray_left(bar: HWND, bar_rect: RECT, notify: HWND, msg: u32) -> TrayEdge {
    let key = bar.0 as isize;
    let r = (bar_rect.left, bar_rect.top, bar_rect.right, bar_rect.bottom);
    let (edge, stale) = {
        let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        let c = g.get_or_insert_with(Cache::default);
        match c.known.get(&key) {
            Some(e) => (
                e.from_right.map_or(TrayEdge::Unknown, |d| TrayEdge::Known(r.2 - d)),
                e.dirty || e.bar != r || e.at.elapsed() >= REFRESH,
            ),
            None => {
                let since = *c.asked.entry(key).or_insert_with(Instant::now);
                (if since.elapsed() < FIRST_WAIT { TrayEdge::Pending } else { TrayEdge::Unknown }, true)
            }
        }
    };
    if stale && !BUSY.swap(true, Ordering::SeqCst) {
        let notify = notify.0 as isize;
        std::thread::spawn(move || {
            let from_right = probe(key).filter(|x| *x > r.0 && *x < r.2).map(|x| r.2 - x);
            {
                let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
                let c = g.get_or_insert_with(Cache::default);
                c.known.insert(key, Entry { bar: r, from_right, at: Instant::now(), dirty: false });
                c.known.retain(|_, e| e.at.elapsed() < REFRESH * 4);
                c.asked.remove(&key);
            }
            BUSY.store(false, Ordering::SeqCst);
            unsafe {
                let _ = PostMessageW(Some(HWND(notify as *mut _)), msg, WPARAM(0), LPARAM(0));
            }
        });
    }
    edge
}

/// The main taskbar's notification area changed size (e.g. the notification bell appeared or the
/// date got longer): check every taskbar again, keeping the current answers until then.
pub fn taskbars_changed() {
    let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = g.as_mut() {
        for e in c.known.values_mut() {
            e.dirty = true;
        }
    }
}

/// Leftmost `SystemTrayIcon` element under the taskbar window `bar`.
fn probe(bar: isize) -> Option<i32> {
    unsafe {
        let init = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let found = (|| -> windows::core::Result<Option<i32>> {
            let uia: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
            let root = uia.ElementFromHandle(HWND(bar as *mut _))?;
            let all = root.FindAll(TreeScope_Descendants, &uia.CreateTrueCondition()?)?;
            let mut best: Option<i32> = None;
            for i in 0..all.Length()? {
                let e = all.GetElement(i)?;
                if e.CurrentAutomationId().map(|s| s.to_string()).unwrap_or_default() != TRAY_ID {
                    continue;
                }
                let b = e.CurrentBoundingRectangle()?;
                if b.right > b.left {
                    best = Some(best.map_or(b.left, |x| x.min(b.left)));
                }
            }
            Ok(best)
        })();
        if init {
            CoUninitialize();
        }
        found.ok().flatten()
    }
}
