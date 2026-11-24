//! Generic HID fallback: any device exposing a standard battery usage (Generic Device Controls
//! "Battery Strength" 06:20, Battery System "Absolute State Of Charge" 85:65) in a feature or
//! input report.
use std::ffi::c_void;

use windows::core::HSTRING;
use windows::Win32::Devices::HumanInterfaceDevice::{
    HidD_FreePreparsedData, HidD_GetFeature, HidD_GetInputReport, HidD_GetPreparsedData, HidP_Feature, HidP_GetValueCaps,
    HidP_GetUsageValue, HidP_Input, HIDP_REPORT_TYPE, HIDP_STATUS_SUCCESS, HIDP_VALUE_CAPS, PHIDP_PREPARSED_DATA,
};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};

use super::sdi::container_of_interface;
use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::{self, HidColl};
use crate::util::{clamp100, guess_kind, hex4};

fn is_battery_usage(page: u16, usage: u16) -> bool {
    (page == 0x06 && usage == 0x20) || (page == 0x85 && usage == 0x65)
}

fn read_battery(c: &HidColl) -> Option<i32> {
    unsafe {
        let h = CreateFileW(
            &HSTRING::from(c.path.as_str()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
        .ok()?;
        let mut pp = PHIDP_PREPARSED_DATA::default();
        if !HidD_GetPreparsedData(h, &mut pp) {
            let _ = CloseHandle(h);
            return None;
        }
        let mut pct = None;
        let kinds: [(HIDP_REPORT_TYPE, u16, u16); 2] = [
            (HidP_Feature, c.n_feature_value_caps, c.feat_len()),
            (HidP_Input, c.n_input_value_caps, c.in_len()),
        ];
        'outer: for (ty, n, len) in kinds {
            if n == 0 || len == 0 {
                continue;
            }
            let mut n = n;
            let mut vc = vec![HIDP_VALUE_CAPS::default(); n as usize];
            if HidP_GetValueCaps(ty, vc.as_mut_ptr(), &mut n, pp) != HIDP_STATUS_SUCCESS {
                continue;
            }
            for v in &vc[..n as usize] {
                let u = if v.IsRange { v.Anonymous.Range.UsageMin } else { v.Anonymous.NotRange.Usage };
                if !is_battery_usage(v.UsagePage, u) {
                    continue;
                }
                let mut buf = vec![0u8; len as usize];
                buf[0] = v.ReportID;
                let got = if ty == HidP_Feature {
                    HidD_GetFeature(h, buf.as_mut_ptr() as *mut c_void, len as u32)
                } else {
                    HidD_GetInputReport(h, buf.as_mut_ptr() as *mut c_void, len as u32)
                };
                if !got {
                    continue;
                }
                let mut val = 0u32;
                if HidP_GetUsageValue(ty, v.UsagePage, Some(v.LinkCollection), u, &mut val, pp, &buf) != HIDP_STATUS_SUCCESS {
                    continue;
                }
                // Apple devices report a plain percentage while declaring 0-255 (HID_BATTERY_QUIRK_PERCENT).
                let p = if c.vid == 0x05AC && val <= 100 {
                    val as i32
                } else if v.LogicalMax > v.LogicalMin {
                    clamp100(((val as i32) - v.LogicalMin) * 100 / (v.LogicalMax - v.LogicalMin))
                } else if val <= 100 {
                    val as i32
                } else {
                    continue;
                };
                pct = Some(p);
                break 'outer;
            }
        }
        let _ = HidD_FreePreparsedData(pp);
        let _ = CloseHandle(h);
        pct
    }
}

pub fn poll(ctx: &mut PollContext) {
    // Drawing tablets (Digitizers page 0D) are never read: polling one flipped an XP-Pen's Y axis.
    let digitizers: Vec<(u16, u16)> = ctx.colls.iter().filter(|c| c.page() == 0x0D).map(|c| (c.vid, c.pid)).collect();
    let colls = ctx.colls.clone();
    for c in &colls {
        if ctx.is_claimed(c.vid, c.pid) || c.page() == 0x84 || c.page() == 0x85 {
            continue; // claimed, or UPS / power supplies
        }
        if digitizers.contains(&(c.vid, c.pid)) || (c.n_feature_value_caps == 0 && c.n_input_value_caps == 0) {
            continue;
        }
        if !ctx.bt_containers.is_empty() {
            if let Some(cont) = container_of_interface(&c.path) {
                if ctx.bt_containers.contains(&cont) {
                    continue; // already shown with Windows' Bluetooth level
                }
            }
        }
        let Some(pct) = read_battery(c) else { continue };
        let mut name = hid::product_string(c);
        if name.is_empty() {
            name = "Wireless device".into();
        }
        let mut kind = hid::kind_from_collections(&ctx.colls, c.vid, c.pid);
        if kind == DeviceKind::Other {
            kind = guess_kind(&name);
        }
        let s = DeviceStatus {
            key: format!("hid:{}:{}", hex4(c.vid), hex4(c.pid)),
            name,
            via: "HID".into(),
            kind,
            percent: pct,
            ..Default::default()
        };
        ctx.add(DeviceStatus { hw: Some((c.vid, c.pid)), ..s }, c.vid, c.pid);
    }
}
