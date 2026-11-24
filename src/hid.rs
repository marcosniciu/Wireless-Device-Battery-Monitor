//! Thin Win32 HID layer shared by all drivers (port of hid.h / hid.cpp).
//!
//! Report buffers follow hidapi conventions:
//!  * write / set_feature: data[0] is the report ID (0 when the device does not number its reports);
//!    the buffer is zero-padded to the report length.
//!  * read: a leading 0 report ID is stripped, so data[0] is the first payload byte for
//!    unnumbered reports and the report ID for numbered ones.
use std::ffi::c_void;

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, SP_DEVICE_INTERFACE_DATA,
    SP_DEVICE_INTERFACE_DETAIL_DATA_W,
};
use windows::Win32::Devices::HumanInterfaceDevice::{
    HidD_FlushQueue, HidD_FreePreparsedData, HidD_GetAttributes, HidD_GetFeature, HidD_GetHidGuid,
    HidD_GetInputReport, HidD_GetPreparsedData, HidD_GetProductString, HidD_SetFeature, HidD_SetOutputReport,
    HidP_GetCaps, HIDD_ATTRIBUTES, HIDP_CAPS, PHIDP_PREPARSED_DATA,
};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::System::IO::{CancelIo, GetOverlappedResult, OVERLAPPED};

use crate::device::DeviceKind;

/// One HID top-level collection.
#[derive(Clone, Debug, Default)]
pub struct HidColl {
    pub path: String,
    pub vid: u16,
    pub pid: u16,
    /// USB interface number (MI_xx in the path), -1 when unknown.
    pub iface: i32,
    pub usage_page: u16,
    pub usage_id: u16,
    pub input_len: u16,
    pub output_len: u16,
    pub feature_len: u16,
    pub n_input_value_caps: u16,
    pub n_feature_value_caps: u16,
}

impl HidColl {
    pub fn page(&self) -> u16 {
        self.usage_page
    }
    pub fn usage(&self) -> u16 {
        self.usage_id
    }
    pub fn in_len(&self) -> u16 {
        self.input_len
    }
    pub fn out_len(&self) -> u16 {
        self.output_len
    }
    pub fn feat_len(&self) -> u16 {
        self.feature_len
    }
}

fn open_raw(path: &str, access: u32, overlapped: bool) -> Option<HANDLE> {
    let flags = if overlapped { FILE_FLAG_OVERLAPPED } else { FILE_FLAGS_AND_ATTRIBUTES(0) };
    unsafe {
        CreateFileW(
            &HSTRING::from(path),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            flags,
            None,
        )
        .ok()
        .filter(|h| !h.is_invalid())
    }
}

/// Paths of all present HID interfaces (sorted). Cheap: SetupAPI only, no device is opened - used
/// to notice that a device was plugged in or removed between two reads.
pub fn interface_paths() -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        let guid = HidD_GetHidGuid();
        let Ok(di) = SetupDiGetClassDevsW(Some(&guid), PCWSTR::null(), None, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE) else {
            return out;
        };
        let mut ifd = SP_DEVICE_INTERFACE_DATA { cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32, ..Default::default() };
        let mut i = 0;
        while SetupDiEnumDeviceInterfaces(di, None, &guid, i, &mut ifd).is_ok() {
            i += 1;
            let mut need = 0u32;
            let _ = SetupDiGetDeviceInterfaceDetailW(di, &ifd, None, 0, Some(&mut need), None);
            if need == 0 {
                continue;
            }
            let mut buf = vec![0u32; (need as usize).div_ceil(4)];
            let det = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*det).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(di, &ifd, Some(det), need, None, None).is_ok() {
                out.push(PCWSTR((*det).DevicePath.as_ptr()).to_string().unwrap_or_default());
            }
        }
        let _ = SetupDiDestroyDeviceInfoList(di);
    }
    out.sort();
    out
}

pub fn enum_hid() -> Vec<HidColl> {
    let mut out = Vec::new();
    unsafe {
        let guid = HidD_GetHidGuid();
        let Ok(di) = SetupDiGetClassDevsW(Some(&guid), PCWSTR::null(), None, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE) else {
            return out;
        };
        let mut ifd = SP_DEVICE_INTERFACE_DATA { cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32, ..Default::default() };
        let mut i = 0;
        while SetupDiEnumDeviceInterfaces(di, None, &guid, i, &mut ifd).is_ok() {
            i += 1;
            let mut need = 0u32;
            let _ = SetupDiGetDeviceInterfaceDetailW(di, &ifd, None, 0, Some(&mut need), None);
            if need == 0 {
                continue;
            }
            // u32-aligned buffer for the variable-length detail struct.
            let mut buf = vec![0u32; (need as usize).div_ceil(4)];
            let det = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*det).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(di, &ifd, Some(det), need, None, None).is_err() {
                continue;
            }
            let path_ptr = (*det).DevicePath.as_ptr();
            let path = PCWSTR(path_ptr).to_string().unwrap_or_default();
            let Some(h) = open_raw(&path, 0, false) else { continue };
            let mut c = HidColl { path, iface: -1, ..Default::default() };
            let mut a = HIDD_ATTRIBUTES { Size: size_of::<HIDD_ATTRIBUTES>() as u32, ..Default::default() };
            if HidD_GetAttributes(h, &mut a) {
                c.vid = a.VendorID;
                c.pid = a.ProductID;
            }
            let mut pp = PHIDP_PREPARSED_DATA::default();
            if HidD_GetPreparsedData(h, &mut pp) {
                let mut caps = HIDP_CAPS::default();
                let _ = HidP_GetCaps(pp, &mut caps);
                c.usage_page = caps.UsagePage;
                c.usage_id = caps.Usage;
                c.input_len = caps.InputReportByteLength;
                c.output_len = caps.OutputReportByteLength;
                c.feature_len = caps.FeatureReportByteLength;
                c.n_input_value_caps = caps.NumberInputValueCaps;
                c.n_feature_value_caps = caps.NumberFeatureValueCaps;
                let _ = HidD_FreePreparsedData(pp);
            }
            let _ = CloseHandle(h);
            let lower = c.path.to_lowercase();
            if let Some(mi) = lower.find("&mi_") {
                let hex: String = lower[mi + 4..].chars().take_while(|c| c.is_ascii_hexdigit()).collect();
                c.iface = i32::from_str_radix(&hex, 16).unwrap_or(-1);
            }
            out.push(c);
        }
        let _ = SetupDiDestroyDeviceInfoList(di);
    }
    out
}

/// An open HID collection. Closed on drop.
pub struct HidDev {
    h: HANDLE,
    overlapped: bool,
    c: HidColl,
}

// HANDLEs are plain kernel handles; the device is only used from one thread at a time.
unsafe impl Send for HidDev {}

struct Event(HANDLE);
impl Event {
    fn new() -> Option<Self> {
        unsafe { CreateEventW(None, true, false, PCWSTR::null()).ok().map(Event) }
    }
}
impl Drop for Event {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

impl HidDev {
    /// Read/write, overlapped.
    pub fn open(c: &HidColl) -> Option<Self> {
        open_raw(&c.path, GENERIC_READ.0 | GENERIC_WRITE.0, true).map(|h| Self { h, overlapped: true, c: c.clone() })
    }
    /// Read only, overlapped: for push-only collections another app may hold.
    pub fn open_read_only(c: &HidColl) -> Option<Self> {
        open_raw(&c.path, GENERIC_READ.0, true).map(|h| Self { h, overlapped: true, c: c.clone() })
    }
    /// Zero access: enough for feature reports, works on mice/keyboards Windows holds exclusively.
    pub fn open_feature_only(c: &HidColl) -> Option<Self> {
        open_raw(&c.path, 0, false).map(|h| Self { h, overlapped: false, c: c.clone() })
    }

    pub fn coll(&self) -> &HidColl {
        &self.c
    }

    fn wait_io(&self, ov: &mut OVERLAPPED, ev: &Event, timeout_ms: u32) -> Option<u32> {
        let mut n = 0u32;
        unsafe {
            if timeout_ms > 0 && WaitForSingleObject(ev.0, timeout_ms) == WAIT_OBJECT_0 {
                GetOverlappedResult(self.h, ov, &mut n, false).ok().map(|_| n)
            } else {
                let _ = CancelIo(self.h);
                // It may have completed meanwhile.
                GetOverlappedResult(self.h, ov, &mut n, true).ok().map(|_| n)
            }
        }
    }

    pub fn write(&self, data: &[u8]) -> bool {
        self.write_timeout(data, 500)
    }

    pub fn write_timeout(&self, data: &[u8], timeout_ms: u32) -> bool {
        let len = self.c.out_len() as usize;
        if !self.overlapped || len == 0 || data.is_empty() {
            return false;
        }
        let mut buf = vec![0u8; len];
        let n = data.len().min(len);
        buf[..n].copy_from_slice(&data[..n]);
        let Some(ev) = Event::new() else { return false };
        let mut ov = OVERLAPPED { hEvent: ev.0, ..Default::default() };
        unsafe {
            let r = WriteFile(self.h, Some(&buf), None, Some(&mut ov));
            if r.is_err() && GetLastError() != ERROR_IO_PENDING {
                return false;
            }
        }
        self.wait_io(&mut ov, &ev, timeout_ms).is_some()
    }

    fn strip(tmp: &[u8], got: u32) -> Option<Vec<u8>> {
        if got == 0 {
            return None;
        }
        let off = if tmp[0] == 0 { 1 } else { 0 };
        Some(tmp[off..got as usize].to_vec())
    }

    /// Reads one input report. None on timeout/error.
    pub fn read(&self, timeout_ms: u32) -> Option<Vec<u8>> {
        let len = self.c.in_len() as usize;
        if !self.overlapped || len == 0 {
            return None;
        }
        let mut tmp = vec![0u8; len];
        let ev = Event::new()?;
        let mut ov = OVERLAPPED { hEvent: ev.0, ..Default::default() };
        unsafe {
            let r = ReadFile(self.h, Some(&mut tmp), None, Some(&mut ov));
            if r.is_err() && GetLastError() != ERROR_IO_PENDING {
                return None;
            }
        }
        let got = self.wait_io(&mut ov, &ev, timeout_ms)?;
        Self::strip(&tmp, got)
    }

    /// For collections without an interrupt OUT pipe: arms a read, sends `data` with
    /// HidD_SetOutputReport and returns the first input report.
    pub fn set_output_and_read(&self, data: &[u8], timeout_ms: u32) -> Option<Vec<u8>> {
        let (il, ol) = (self.c.in_len() as usize, self.c.out_len() as usize);
        if !self.overlapped || il == 0 || ol == 0 || data.is_empty() {
            return None;
        }
        let mut out = vec![0u8; ol];
        let n = data.len().min(ol);
        out[..n].copy_from_slice(&data[..n]);
        let mut tmp = vec![0u8; il];
        let ev = Event::new()?;
        let mut ov = OVERLAPPED { hEvent: ev.0, ..Default::default() };
        unsafe {
            let r = ReadFile(self.h, Some(&mut tmp), None, Some(&mut ov));
            if r.is_err() && GetLastError() != ERROR_IO_PENDING {
                return None;
            }
            let sent = HidD_SetOutputReport(self.h, out.as_ptr() as *const c_void, out.len() as u32);
            let got = self.wait_io(&mut ov, &ev, if sent { timeout_ms } else { 0 })?;
            if !sent {
                return None;
            }
            Self::strip(&tmp, got)
        }
    }

    /// Discards queued input reports.
    pub fn flush(&self) {
        unsafe {
            let _ = HidD_FlushQueue(self.h);
        }
    }

    pub fn set_feature(&self, data: &[u8]) -> bool {
        let len = self.c.feat_len() as usize;
        if len == 0 {
            return false;
        }
        let mut buf = vec![0u8; len];
        let n = data.len().min(len);
        buf[..n].copy_from_slice(&data[..n]);
        unsafe { HidD_SetFeature(self.h, buf.as_ptr() as *const c_void, buf.len() as u32) }
    }

    /// out[0] = report ID.
    pub fn get_feature(&self, report_id: u8) -> Option<Vec<u8>> {
        let len = self.c.feat_len() as usize;
        if len == 0 {
            return None;
        }
        let mut out = vec![0u8; len];
        out[0] = report_id;
        unsafe { HidD_GetFeature(self.h, out.as_mut_ptr() as *mut c_void, len as u32) }.then_some(out)
    }

    /// Current input report via the control pipe; out[0] = report ID.
    pub fn get_input_report(&self, report_id: u8) -> Option<Vec<u8>> {
        let len = self.c.in_len() as usize;
        if len == 0 {
            return None;
        }
        let mut out = vec![0u8; len];
        out[0] = report_id;
        unsafe { HidD_GetInputReport(self.h, out.as_mut_ptr() as *mut c_void, len as u32) }.then_some(out)
    }
}

impl Drop for HidDev {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.h);
        }
    }
}

pub fn product_string(c: &HidColl) -> String {
    let Some(h) = open_raw(&c.path, 0, false) else { return String::new() };
    let mut name = [0u16; 128];
    let ok = unsafe { HidD_GetProductString(h, name.as_mut_ptr() as *mut c_void, (name.len() * 2 - 2) as u32) };
    unsafe {
        let _ = CloseHandle(h);
    }
    if !ok {
        return String::new();
    }
    crate::util::from_wide(&name).trim_end().to_string()
}

/// Device type from the top-level collections a device exposes (mouse > gamepad > keyboard).
pub fn kind_from_collections(all: &[HidColl], vid: u16, pid: u16) -> DeviceKind {
    let (mut mouse, mut pad, mut kbd) = (false, false, false);
    for c in all.iter().filter(|c| c.vid == vid && c.pid == pid && c.page() == 0x01) {
        mouse |= c.usage() == 0x02;
        pad |= c.usage() == 0x04 || c.usage() == 0x05;
        kbd |= c.usage() == 0x06;
    }
    if mouse {
        DeviceKind::Mouse
    } else if pad {
        DeviceKind::Gamepad
    } else if kbd {
        DeviceKind::Keyboard
    } else {
        DeviceKind::Other
    }
}
