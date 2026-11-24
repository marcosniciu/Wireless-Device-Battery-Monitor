//! `batteryhub-taskbar.exe --listen VID:PID`: diagnostic for device requests. Lists every HID
//! collection of the device, records the input reports each one pushes for a while, and asks each
//! for its input / feature reports 1..8 (reads only, nothing is written to the device). The result
//! is saved as a text file on the desktop and opened in Notepad, ready to paste into an issue.
use std::fmt::Write as _;
use std::sync::mpsc;
use std::time::Instant;

use windows::core::HSTRING;
use windows::Win32::Foundation::GetLastError;
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK, MB_OKCANCEL, IDOK};

use crate::hid::{self, HidColl, HidDev};
use crate::util::hex4;

const LISTEN_MS: u64 = 15_000;
const MAX_FRAMES: usize = 40;

/// Hex dump, cut after 64 bytes (long feature reports are mostly zeros).
fn hex(b: &[u8]) -> String {
    let s = b.iter().take(64).map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(" ");
    if b.len() > 64 { format!("{s} ... ({} bytes)", b.len()) } else { s }
}

fn label(c: &HidColl) -> String {
    format!(
        "if{} {}:{} in/out/feature {}/{}/{}",
        c.iface,
        hex4(c.page()),
        hex4(c.usage()),
        c.in_len(),
        c.out_len(),
        c.feat_len()
    )
}

fn message(text: &str, flags: windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE) -> bool {
    unsafe { MessageBoxW(None, &HSTRING::from(text), &HSTRING::from("BatteryHub - listen"), flags) == IDOK }
}

fn desktop_dir() -> std::path::PathBuf {
    use windows::Win32::UI::Shell::{FOLDERID_Desktop, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    unsafe {
        if let Ok(p) = SHGetKnownFolderPath(&FOLDERID_Desktop, KF_FLAG_DEFAULT, None) {
            if let Ok(s) = p.to_string() {
                windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
                return s.into();
            }
        }
    }
    std::env::temp_dir()
}

/// Builds the report text (no UI).
pub fn listen(vid: u16, pid: u16, ms: u64) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "BatteryHub {} listen {}:{}", crate::install::version_label(), hex4(vid), hex4(pid));
    let colls: Vec<HidColl> = hid::enum_hid().into_iter().filter(|c| c.vid == vid && c.pid == pid).collect();
    if colls.is_empty() {
        let _ = writeln!(out, "No HID collections of this device found. Is the receiver plugged in?");
        return out;
    }
    let _ = writeln!(out, "Product string: {}", hid::product_string(&colls[0]));
    let (tx, rx) = mpsc::channel::<(usize, u128, Vec<u8>)>();
    let start = Instant::now();
    let mut threads = Vec::new();
    for (i, c) in colls.iter().enumerate() {
        let _ = writeln!(out, "\n[{i}] {}", label(c));
        // Polled reports first (control pipe, read only).
        if let Some(d) = HidDev::open_read_only(c).or_else(|| HidDev::open_feature_only(c)) {
            // Collections without report IDs answer every id the same way: print each answer once.
            let (mut last_in, mut last_feat) = (None, None);
            for id in 0u8..=8 {
                if c.in_len() > 0 {
                    if let Some(r) = d.get_input_report(id).filter(|r| Some(&r[1..]) != last_in.as_deref()) {
                        let _ = writeln!(out, "  get input   #{id}: {}", hex(&r));
                        last_in = Some(r[1..].to_vec());
                    }
                }
                if c.feat_len() > 0 {
                    if let Some(r) = d.get_feature(id).filter(|r| Some(&r[1..]) != last_feat.as_deref()) {
                        let _ = writeln!(out, "  get feature #{id}: {}", hex(&r));
                        last_feat = Some(r[1..].to_vec());
                    }
                }
            }
        }
        if c.in_len() == 0 {
            continue;
        }
        match HidDev::open_read_only(c) {
            Some(d) => {
                let tx = tx.clone();
                threads.push(std::thread::spawn(move || {
                    let end = crate::util::Deadline::in_ms(ms);
                    let mut n = 0;
                    while n < MAX_FRAMES {
                        let left = end.left_ms();
                        if left == 0 {
                            break;
                        }
                        if let Some(r) = d.read(left) {
                            let _ = tx.send((i, start.elapsed().as_millis(), r));
                            n += 1;
                        }
                    }
                }));
            }
            None => {
                let e = unsafe { GetLastError() };
                let _ = writeln!(out, "  cannot open for reading (error {})", e.0);
            }
        }
    }
    drop(tx);
    for t in threads {
        let _ = t.join();
    }
    let _ = writeln!(out, "\nInput reports pushed in {} s (collection, ms, bytes):", ms / 1000);
    let mut any = false;
    for (i, t, r) in rx.try_iter() {
        any = true;
        let _ = writeln!(out, "  [{i}] {t:>6} ms  {}", hex(&r));
    }
    if !any {
        let _ = writeln!(out, "  none");
    }
    out
}

/// "1D57:FA60" -> (0x1D57, 0xFA60).
pub fn parse_vid_pid(s: &str) -> Option<(u16, u16)> {
    let (v, p) = s.trim().split_once(':')?;
    Some((u16::from_str_radix(v, 16).ok()?, u16::from_str_radix(p, 16).ok()?))
}

/// The `--listen` entry point with its two message boxes.
pub fn run(arg: Option<&str>) {
    let Some((vid, pid)) = arg.and_then(parse_vid_pid) else {
        message("Usage: batteryhub-taskbar.exe --listen VID:PID   (e.g. --listen 1D57:FA60)", MB_OK | MB_ICONINFORMATION);
        return;
    };
    let text = format!(
        "BatteryHub will now listen to {}:{} for {} seconds.\n\nClick OK, then move the mouse / use the device the whole time. With a mouse, also try plugging and unplugging its charging cable.",
        hex4(vid),
        hex4(pid),
        LISTEN_MS / 1000
    );
    if !message(&text, MB_OKCANCEL | MB_ICONINFORMATION) {
        return;
    }
    let report = listen(vid, pid, LISTEN_MS);
    let path = desktop_dir().join(format!("BatteryHub-listen-{}-{}.txt", hex4(vid), hex4(pid)));
    let report = report.replace('\n', "\r\n");
    if std::fs::write(&path, report).is_ok() {
        let _ = std::process::Command::new("notepad.exe").arg(&path).spawn();
    } else {
        message("Could not write the result file.", MB_OK | MB_ICONINFORMATION);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ids() {
        assert_eq!(super::parse_vid_pid("1d57:FA60"), Some((0x1D57, 0xFA60)));
        assert_eq!(super::parse_vid_pid("1D57"), None);
    }
}
