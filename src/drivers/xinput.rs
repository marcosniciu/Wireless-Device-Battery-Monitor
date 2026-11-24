//! Xbox controllers.
//!  1. Windows.Gaming.Input: every Xbox controller Windows knows, with a real battery report
//!     (remaining / full mWh -> %).
//!  2. XInput fallback: coarse levels (empty / low / medium / full), also covering headsets
//!     plugged into a controller.
use std::sync::OnceLock;

use windows::Gaming::Input::RawGameController;
use windows::System::Power::BatteryStatus;
use windows::Win32::UI::Input::XboxController::{
    XInputGetBatteryInformation, XInputGetState, BATTERY_DEVTYPE, BATTERY_DEVTYPE_GAMEPAD, BATTERY_DEVTYPE_HEADSET,
    BATTERY_LEVEL_EMPTY, BATTERY_LEVEL_LOW, BATTERY_LEVEL_MEDIUM, BATTERY_TYPE_DISCONNECTED, BATTERY_TYPE_UNKNOWN,
    BATTERY_TYPE_WIRED, XINPUT_BATTERY_INFORMATION, XINPUT_STATE,
};

use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::util::{clamp100, hex4, sleep_ms};

/// Vendors whose controllers report a real battery through Windows.Gaming.Input.
fn wgi_vendor(vid: u16) -> bool {
    vid == 0x045E || vid == 0x3537
}

/// Returns (added, third_party).
fn poll_wgi(ctx: &mut PollContext) -> (usize, usize) {
    static WARMED: OnceLock<()> = OnceLock::new();
    if WARMED.set(()).is_ok() {
        // The controller list fills in asynchronously after first use.
        let _ = RawGameController::RawGameControllers();
        sleep_ms(300);
    }
    let Ok(list) = RawGameController::RawGameControllers() else { return (0, 0) };
    let (mut added, mut third) = (0, 0);
    for i in 0..list.Size().unwrap_or(0) {
        let Ok(raw) = list.GetAt(i) else { continue };
        let vid = raw.HardwareVendorId().unwrap_or(0);
        let pid = raw.HardwareProductId().unwrap_or(0);
        if ctx.is_claimed(vid, pid) || !wgi_vendor(vid) {
            continue;
        }
        let Ok(report) = raw.TryGetBatteryReport() else { continue };
        let rem = report.RemainingCapacityInMilliwattHours().and_then(|r| r.Value()).unwrap_or(0);
        let full = report.FullChargeCapacityInMilliwattHours().and_then(|r| r.Value()).unwrap_or(0);
        let st = report.Status().unwrap_or(BatteryStatus::NotPresent);
        if full <= 0 || st == BatteryStatus::NotPresent {
            continue;
        }
        let mut name = raw.DisplayName().map(|h| h.to_string()).unwrap_or_default();
        if name.is_empty() {
            name = "Xbox controller".into();
        }
        if vid == 0x3537 {
            name = "GameSir controller".into();
        }
        let s = DeviceStatus {
            key: format!("xbox:{}:{}", hex4(pid), i),
            name,
            kind: DeviceKind::Gamepad,
            via: "Xbox Wireless / Bluetooth".into(),
            percent: clamp100(rem * 100 / full),
            charging: st == BatteryStatus::Charging,
            ..Default::default()
        };
        ctx.add(s, vid, pid);
        added += 1;
        if vid != 0x045E {
            third += 1;
        }
    }
    (added, third)
}

fn query(user: u32, ty: BATTERY_DEVTYPE) -> Option<XINPUT_BATTERY_INFORMATION> {
    let mut bi = XINPUT_BATTERY_INFORMATION::default();
    let ok = unsafe { XInputGetBatteryInformation(user, ty, &mut bi) } == 0;
    (ok && bi.BatteryType != BATTERY_TYPE_DISCONNECTED && bi.BatteryType != BATTERY_TYPE_WIRED).then_some(bi)
}

pub fn poll(ctx: &mut PollContext) {
    let bt_xbox = ctx.out.iter().any(|s| s.via == "Bluetooth" && s.name.contains("Xbox"));
    // Windows.Gaming.Input loads a whole input stack into the process, so it is only used when an
    // Xbox-type controller is actually there: an XInput slot in use, or a Microsoft / GameSir gamepad
    // collection (Bluetooth controllers).
    let xinput_pad = (0..4u32).any(|u| {
        let mut st = XINPUT_STATE::default();
        unsafe { XInputGetState(u, &mut st) == 0 }
    });
    let hid_pad = ctx.colls.iter().any(|c| wgi_vendor(c.vid) && c.page() == 0x01 && (c.usage() == 0x04 || c.usage() == 0x05));
    let (precise, third) = if bt_xbox || !(xinput_pad || hid_pad) { (0, 0) } else { poll_wgi(ctx) };
    // GameSir pads answer the headset query with their own battery; skip if all precise pads are third-party.
    let skip_headset = precise > 0 && third == precise;

    for user in 0..4u32 {
        let mut state = XINPUT_STATE::default();
        if unsafe { XInputGetState(user, &mut state) } != 0 {
            continue; // only slots with a controller connected
        }
        let pad = query(user, BATTERY_DEVTYPE_GAMEPAD);
        for ty in [BATTERY_DEVTYPE_GAMEPAD, BATTERY_DEVTYPE_HEADSET] {
            let is_pad = ty == BATTERY_DEVTYPE_GAMEPAD;
            if is_pad && (bt_xbox || precise > 0) {
                continue; // already shown precisely
            }
            if !is_pad && skip_headset {
                continue;
            }
            let bi = if is_pad {
                match pad {
                    Some(p) => p,
                    None => continue,
                }
            } else {
                let Some(bi) = query(user, ty) else { continue };
                if bi.BatteryType == BATTERY_TYPE_UNKNOWN {
                    continue;
                }
                // Xbox One / Series controllers echo their own battery for the headset query.
                if pad.is_some_and(|p| p.BatteryType == bi.BatteryType && p.BatteryLevel == bi.BatteryLevel) {
                    continue;
                }
                bi
            };
            let (percent, text) = match bi.BatteryLevel {
                BATTERY_LEVEL_EMPTY => (5, "Empty"),
                BATTERY_LEVEL_LOW => (25, "Low"),
                BATTERY_LEVEL_MEDIUM => (60, "Medium"),
                _ => (100, "Full"),
            };
            ctx.out.push(DeviceStatus {
                key: format!("xinput:{}:{}", user, ty.0),
                name: if is_pad { format!("Xbox controller {}", user + 1) } else { "Xbox headset".into() },
                kind: if is_pad { DeviceKind::Gamepad } else { DeviceKind::Headset },
                via: "Xbox Wireless".into(),
                percent,
                level_text: text.into(),
                ..Default::default()
            });
        }
    }
}
