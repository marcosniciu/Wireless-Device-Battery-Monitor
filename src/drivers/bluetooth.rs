//! Bluetooth fallback: Windows keeps the level it reads over HFP (classic) or the GATT Battery
//! Service (LE) in a device property, the same value Settings > Bluetooth & devices shows.
//! Fast Pair earbuds get their left / right / case split from BLE broadcasts.
use std::collections::{BTreeMap, BTreeSet};

use windows::Win32::Devices::DeviceAndDriverInstallation::{CM_Get_DevNode_Status, CM_DEVNODE_STATUS_FLAGS, CM_PROB, CR_SUCCESS, DN_HAS_PROBLEM};
use windows::Win32::Devices::Properties::{DEVPKEY_DeviceContainer_Category, DEVPKEY_Device_DeviceDesc, DEVPKEY_Device_FriendlyName};

use super::sdi::{container_of_interface, DevSet, BT_BATTERY, CLASS_OF_DEVICE, IS_CONNECTED};
use crate::ble;
use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid;
use crate::util::{guess_kind, sleep_ms};

/// Bluetooth address from a main device node id ("BTHENUM\DEV_6CFBED129847\...").
fn bt_address(inst: &str) -> Option<u64> {
    let p = inst.find("\\DEV_")? + 5;
    let hex = inst.get(p..p + 12)?;
    u64::from_str_radix(hex, 16).ok()
}

fn kind_from_class_of_device(cod: u32) -> DeviceKind {
    let (major, minor) = ((cod >> 8) & 0x1F, (cod >> 2) & 0x3F);
    match major {
        0x02 => DeviceKind::Phone,
        0x04 => DeviceKind::Headset,
        0x05 if minor & 0x0F == 0x01 || minor & 0x0F == 0x02 => DeviceKind::Gamepad,
        0x05 if minor >> 4 == 1 => DeviceKind::Keyboard,
        0x05 if minor >> 4 == 2 => DeviceKind::Mouse,
        _ => DeviceKind::Other,
    }
}

fn kind_from_category(c: &str) -> DeviceKind {
    if c == "Communication.Phone" || c.starts_with("Communication.Phone.") {
        DeviceKind::Phone
    } else if c == "Input.Keyboard" {
        DeviceKind::Keyboard
    } else if c == "Input.Mouse" {
        DeviceKind::Mouse
    } else if c == "Input.Gaming" {
        DeviceKind::Gamepad
    } else if c.starts_with("Communication.Headset") || c == "Audio.Headphones" {
        DeviceKind::Headset
    } else {
        DeviceKind::Other
    }
}

fn strip_service_suffix(n: String) -> String {
    for suf in [" Hands-Free AG", " Hands-Free", " Avrcp Transport", " Stereo", " Audio Gateway"] {
        if n.len() > suf.len() && n.ends_with(suf) {
            return n[..n.len() - suf.len()].to_string();
        }
    }
    n
}

/// Vendor drivers (AirPods, ...) also say "Bluetooth"; dedupe them by name, our own entries by key.
fn is_duplicate(candidate: &DeviceStatus, existing: &[DeviceStatus]) -> bool {
    existing
        .iter()
        .any(|o| o.key == candidate.key || (!o.key.starts_with("bt:") && o.name == candidate.name))
}

fn apply_fast_pair(s: &mut DeviceStatus, b: &ble::FastPairBattery) {
    let (mut dis, mut chg) = (-1, -1);
    for (p, c) in [(b.left, b.left_chg), (b.right, b.right_chg)] {
        if p < 0 {
            continue;
        }
        let dst = if c { &mut chg } else { &mut dis };
        if *dst < 0 || p < *dst {
            *dst = p;
        }
    }
    if dis >= 0 || chg >= 0 {
        s.percent = if dis >= 0 { dis } else { chg };
        s.charging = dis < 0;
    }
    let mut parts = Vec::new();
    for (label, p, c) in [("L", b.left, b.left_chg), ("R", b.right, b.right_chg), ("case", b.case, b.case_chg)] {
        if p >= 0 {
            parts.push(format!("{label} {p}%{}", if c { " ⚡" } else { "" }));
        }
    }
    s.detail = parts.join(", ");
}

struct Found {
    pct: u8,
    name: String,
    connected: bool,
}

/// A hash of which Bluetooth devices are connected right now. Connecting or disconnecting a
/// Bluetooth device (e.g. AirPods) does not add or remove device nodes, so Windows sends no
/// device-change message; comparing this signature every few seconds catches it instead.
pub fn connection_signature() -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for e in ["BTHENUM", "BTHLE"] {
        let Some(set) = DevSet::present(Some(e)) else { continue };
        for n in set.nodes() {
            if n.instance_id.starts_with("BTHENUM\\DEV_") || n.instance_id.starts_with("BTHLE\\DEV_") {
                n.instance_id.hash(&mut h);
                n.boolean(&IS_CONNECTED).hash(&mut h);
            }
        }
    }
    h.finish()
}

pub fn poll(ctx: &mut PollContext) {
    // Only the Bluetooth enumerators: listing every device node on the system (all classes) was the
    // most expensive part of a periodic read.
    let sets: Vec<DevSet> = ["BTHENUM", "BTHLE", "BTHLEDEVICE"].iter().filter_map(|e| DevSet::present(Some(e))).collect();
    if sets.is_empty() {
        return;
    }
    let mut container_name: BTreeMap<String, String> = BTreeMap::new();
    let mut container_cod: BTreeMap<String, u32> = BTreeMap::new();
    let mut container_cat: BTreeMap<String, String> = BTreeMap::new();
    let mut container_conn: BTreeMap<String, bool> = BTreeMap::new();
    let mut container_addr: BTreeMap<String, u64> = BTreeMap::new();
    let mut battery: BTreeMap<String, Found> = BTreeMap::new();

    for n in sets.iter().flat_map(|set| set.nodes()) {
        let inst = &n.instance_id;
        if !(inst.starts_with("BTHENUM\\") || inst.starts_with("BTHLE\\") || inst.starts_with("BTHLEDEVICE\\")) {
            continue;
        }
        let cid = n.container().unwrap_or_else(|| inst.clone());
        let name = n
            .string(&DEVPKEY_Device_FriendlyName)
            .or_else(|| n.string(&DEVPKEY_Device_DeviceDesc))
            .unwrap_or_default();
        if inst.starts_with("BTHENUM\\DEV_") || inst.starts_with("BTHLE\\DEV_") {
            container_name.insert(cid.clone(), name.clone());
            if let Some(a) = bt_address(inst) {
                container_addr.insert(cid.clone(), a);
            }
            if let Some(cod) = n.u32(&CLASS_OF_DEVICE) {
                container_cod.insert(cid.clone(), cod);
            }
            if let Some(cat) = n.string(&DEVPKEY_DeviceContainer_Category) {
                container_cat.insert(cid.clone(), cat);
            }
            if let Some(conn) = n.boolean(&IS_CONNECTED) {
                // Classic and LE nodes can share one container: connected if either is.
                *container_conn.entry(cid.clone()).or_insert(false) |= conn;
            }
        }
        let Some(pct) = n.byte(&BT_BATTERY) else { continue };
        let conn = n.boolean(&IS_CONNECTED).unwrap_or(true);
        let (mut st, mut prob) = (CM_DEVNODE_STATUS_FLAGS(0), CM_PROB(0));
        if unsafe { CM_Get_DevNode_Status(&mut st, &mut prob, n.data.DevInst, 0) } == CR_SUCCESS && st.0 & DN_HAS_PROBLEM.0 != 0 {
            continue;
        }
        if pct > 100 {
            continue;
        }
        if let Some(prev) = battery.get(&cid) {
            if prev.connected && !conn {
                continue; // prefer a live node
            }
        }
        battery.insert(cid, Found { pct, name: strip_service_suffix(name), connected: conn });
    }
    drop(sets);

    // Physical devices a driver above already reported over a HID interface on the BT link itself.
    let mut reported = BTreeSet::new();
    let mut hid_ids: BTreeMap<String, (u16, u16)> = BTreeMap::new();
    for c in &ctx.colls {
        let Some(cont) = container_of_interface(&c.path) else { continue };
        if ctx.is_claimed(c.vid, c.pid) {
            reported.insert(cont.clone());
        }
        hid_ids.insert(cont, (c.vid, c.pid));
    }

    let (mut watching, mut waited) = (false, false);
    for (cid, f) in &battery {
        if reported.contains(cid) {
            continue;
        }
        let connected = container_conn.get(cid).copied().unwrap_or(f.connected);
        if !connected {
            continue;
        }
        let mut s = DeviceStatus { key: format!("bt:{cid}"), ..Default::default() };
        s.name = container_name.get(cid).filter(|n| !n.is_empty()).cloned().unwrap_or_else(|| f.name.clone());
        if s.name.is_empty() {
            s.name = "Bluetooth device".into();
        }
        if is_duplicate(&s, &ctx.out) {
            continue;
        }
        s.via = "Bluetooth".into();
        s.percent = f.pct as i32;
        s.kind = container_cod.get(cid).map(|&c| kind_from_class_of_device(c)).unwrap_or_default();
        if s.kind == DeviceKind::Other {
            if let Some(cat) = container_cat.get(cid) {
                s.kind = kind_from_category(cat);
            }
        }
        if let Some(&(vid, pid)) = hid_ids.get(cid) {
            s.vid = vid;
            if s.kind == DeviceKind::Other {
                s.kind = hid::kind_from_collections(&ctx.colls, vid, pid);
            }
        }
        if s.kind == DeviceKind::Other {
            s.kind = guess_kind(&s.name);
        }
        if s.kind == DeviceKind::Headset {
            if let Some(&addr) = container_addr.get(cid) {
                let mut just_started = false;
                if !watching {
                    watching = true;
                    just_started = ble::ensure_watcher();
                }
                let mut got = ble::fast_pair_lookup(addr);
                if got.is_none() && just_started && !waited {
                    for _ in 0..15 {
                        sleep_ms(200);
                        got = ble::fast_pair_lookup(addr);
                        if got.is_some() {
                            break;
                        }
                    }
                }
                waited |= just_started;
                if let Some(b) = got {
                    apply_fast_pair(&mut s, &b);
                }
            }
        }
        ctx.out.push(s);
        ctx.bt_containers.insert(cid.clone());
    }
}
