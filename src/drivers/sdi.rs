//! SetupAPI helpers for device-node properties (used by the Bluetooth fallback and AirPods).
use windows::core::{GUID, HSTRING, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiCreateDeviceInfoList, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
    SetupDiGetDeviceInstanceIdW, SetupDiGetDeviceInterfaceDetailW, SetupDiGetDevicePropertyW,
    SetupDiOpenDeviceInterfaceW, DIGCF_ALLCLASSES, DIGCF_PRESENT, HDEVINFO, SP_DEVICE_INTERFACE_DATA, SP_DEVINFO_DATA,
};
use windows::Win32::Devices::Properties::{
    DEVPKEY_Device_ContainerId, DEVPROPTYPE, DEVPROP_TYPE_BOOLEAN, DEVPROP_TYPE_BYTE, DEVPROP_TYPE_GUID,
    DEVPROP_TYPE_STRING, DEVPROP_TYPE_STRING_LIST, DEVPROP_TYPE_UINT32,
};
use windows::Win32::Foundation::DEVPROPKEY;
use windows::Win32::System::Com::StringFromGUID2;

use crate::util::from_wide;

pub const fn key(d1: u32, d2: u16, d3: u16, d4: [u8; 8], pid: u32) -> DEVPROPKEY {
    DEVPROPKEY { fmtid: GUID { data1: d1, data2: d2, data3: d3, data4: d4 }, pid }
}

/// Bluetooth battery level Windows reads over HFP / GATT Battery Service.
pub const BT_BATTERY: DEVPROPKEY = key(0x104EA319, 0x6EE2, 0x4701, [0xBD, 0x47, 0x8D, 0xDB, 0xF4, 0x25, 0xBB, 0xE5], 2);
pub const IS_CONNECTED: DEVPROPKEY = key(0x83DA6326, 0x97A6, 0x4088, [0x94, 0x53, 0xA1, 0x92, 0x3F, 0x57, 0x3B, 0x29], 15);
pub const CLASS_OF_DEVICE: DEVPROPKEY = key(0x2BD67D8B, 0x8BEB, 0x48D5, [0x87, 0xE0, 0x6C, 0xDA, 0x34, 0x28, 0x04, 0x0A], 10);

pub fn guid_string(g: &GUID) -> String {
    let mut buf = [0u16; 64];
    unsafe { StringFromGUID2(g, &mut buf) };
    from_wide(&buf)
}

pub struct DevSet(HDEVINFO);

pub struct DevNode<'a> {
    set: &'a DevSet,
    pub data: SP_DEVINFO_DATA,
    pub instance_id: String,
}

impl DevSet {
    /// All present device nodes, optionally limited to one enumerator ("BTHENUM").
    pub fn present(enumerator: Option<&str>) -> Option<Self> {
        let e = enumerator.map(HSTRING::from);
        let p = e.as_ref().map(|h| PCWSTR(h.as_ptr())).unwrap_or(PCWSTR::null());
        unsafe { SetupDiGetClassDevsW(None, p, None, DIGCF_ALLCLASSES | DIGCF_PRESENT).ok().map(DevSet) }
    }

    pub fn nodes(&self) -> Vec<DevNode<'_>> {
        let mut out = Vec::new();
        let mut i = 0;
        loop {
            let mut dd = SP_DEVINFO_DATA { cbSize: size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
            if unsafe { SetupDiEnumDeviceInfo(self.0, i, &mut dd) }.is_err() {
                break;
            }
            i += 1;
            let mut inst = [0u16; 512];
            if unsafe { SetupDiGetDeviceInstanceIdW(self.0, &dd, Some(&mut inst), None) }.is_err() {
                continue;
            }
            out.push(DevNode { set: self, data: dd, instance_id: from_wide(&inst) });
        }
        out
    }
}

impl Drop for DevSet {
    fn drop(&mut self) {
        unsafe {
            let _ = SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

impl DevNode<'_> {
    fn raw(&self, key: &DEVPROPKEY, buf: &mut [u8]) -> Option<DEVPROPTYPE> {
        let mut t = DEVPROPTYPE::default();
        unsafe { SetupDiGetDevicePropertyW(self.set.0, &self.data, key, &mut t, Some(buf), None, 0) }
            .ok()
            .map(|_| t)
    }

    pub fn string(&self, key: &DEVPROPKEY) -> Option<String> {
        let mut buf = [0u8; 512];
        let t = self.raw(key, &mut buf[..510])?;
        if t != DEVPROP_TYPE_STRING && t != DEVPROP_TYPE_STRING_LIST {
            return None;
        }
        let w: Vec<u16> = buf.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        Some(from_wide(&w)).filter(|s| !s.is_empty())
    }

    pub fn guid(&self, key: &DEVPROPKEY) -> Option<GUID> {
        let mut buf = [0u8; 16];
        // GUID is #[repr(C)] and 16 bytes: read it straight from the property buffer.
        (self.raw(key, &mut buf)? == DEVPROP_TYPE_GUID).then(|| unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const GUID) })
    }

    pub fn container(&self) -> Option<String> {
        self.guid(&DEVPKEY_Device_ContainerId).map(|g| guid_string(&g))
    }

    pub fn boolean(&self, key: &DEVPROPKEY) -> Option<bool> {
        let mut buf = [0u8; 1];
        (self.raw(key, &mut buf)? == DEVPROP_TYPE_BOOLEAN).then_some(buf[0] != 0)
    }

    pub fn byte(&self, key: &DEVPROPKEY) -> Option<u8> {
        let mut buf = [0u8; 1];
        (self.raw(key, &mut buf)? == DEVPROP_TYPE_BYTE).then_some(buf[0])
    }

    pub fn u32(&self, key: &DEVPROPKEY) -> Option<u32> {
        let mut buf = [0u8; 4];
        (self.raw(key, &mut buf)? == DEVPROP_TYPE_UINT32).then_some(u32::from_le_bytes(buf))
    }
}

/// Container ids by interface path. A path names one device instance, and its container never
/// changes, so the SetupAPI lookup is done once per path (cleared on every HID rescan).
static CONTAINERS: std::sync::Mutex<Option<std::collections::HashMap<String, Option<String>>>> = std::sync::Mutex::new(None);

pub fn forget_containers() {
    if let Ok(mut c) = CONTAINERS.lock() {
        *c = None;
    }
}

/// Container id of the physical device a device interface (e.g. a HID path) belongs to (cached).
pub fn container_of_interface(path: &str) -> Option<String> {
    if let Ok(c) = CONTAINERS.lock() {
        if let Some(hit) = c.as_ref().and_then(|m| m.get(path)) {
            return hit.clone();
        }
    }
    let found = lookup_container(path);
    if let Ok(mut c) = CONTAINERS.lock() {
        c.get_or_insert_with(Default::default).insert(path.to_string(), found.clone());
    }
    found
}

fn lookup_container(path: &str) -> Option<String> {
    unsafe {
        let di = SetupDiCreateDeviceInfoList(None, None).ok()?;
        let set = DevSet(di);
        let mut ifd = SP_DEVICE_INTERFACE_DATA { cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32, ..Default::default() };
        SetupDiOpenDeviceInterfaceW(set.0, &HSTRING::from(path), 0, Some(&mut ifd)).ok()?;
        let mut dd = SP_DEVINFO_DATA { cbSize: size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
        let _ = SetupDiGetDeviceInterfaceDetailW(set.0, &ifd, None, 0, None, Some(&mut dd));
        if dd.DevInst == 0 {
            return None;
        }
        let node = DevNode { set: &set, data: dd, instance_id: String::new() };
        node.container()
    }
}
