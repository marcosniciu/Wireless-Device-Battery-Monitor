//! SteelSeries wireless headsets and mice (VID 1038).
//! Headset protocols follow HeadsetControl, mouse protocols rivalcfg (see the original steelseries.cpp).
use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::{HidColl, HidDev};
use crate::util::{clamp100, hex4, map_to_100, Deadline};

fn write_read(d: &HidDev, req: &[u8], timeout_ms: u32) -> Option<Vec<u8>> {
    d.flush();
    if !d.write(req) {
        return None;
    }
    d.read(timeout_ms).filter(|r| !r.is_empty())
}

fn headset(pid: u16, name: &str) -> DeviceStatus {
    DeviceStatus {
        key: format!("steelseries:{}", hex4(pid)),
        name: name.into(),
        kind: DeviceKind::Headset,
        via: "2.4 GHz receiver".into(),
        ..Default::default()
    }
}

fn mouse(pid: u16, name: &str, wired: bool) -> DeviceStatus {
    DeviceStatus {
        key: format!("steelseries:{}", hex4(pid)),
        name: name.into(),
        kind: DeviceKind::Mouse,
        via: if wired { "USB cable".into() } else { "2.4 GHz receiver".into() },
        ..Default::default()
    }
}

// ---- headsets ----------------------------------------------------------------------------------

/// Arctis 1 / 1 Xbox / 7X: 06 12 -> [2]==01 offline, [3] %.
fn arctis1(d: &HidDev, pid: u16) -> Option<DeviceStatus> {
    let r = write_read(d, &[0x06, 0x12], 1000).filter(|r| r.len() >= 4)?;
    let mut s = headset(pid, "SteelSeries Arctis 1 Wireless");
    s.connected = r[2] != 0x01;
    if s.connected {
        s.percent = clamp100(r[3] as i32);
    }
    Some(s)
}

/// Arctis 7 / 7 2019 / Pro 2019: 06 18 -> [2] %.
fn arctis7(d: &HidDev, pid: u16) -> Option<DeviceStatus> {
    let r = write_read(d, &[0x06, 0x18], 1000).filter(|r| r.len() >= 3)?;
    let mut s = headset(pid, if pid == 0x1252 { "SteelSeries Arctis Pro" } else { "SteelSeries Arctis 7" });
    s.percent = clamp100(r[2] as i32);
    s.connected = r[2] != 0;
    Some(s)
}

/// Arctis 9: 00 20 -> [3] raw (0x64..0x9A), [4]==01 charging.
fn arctis9(d: &HidDev, pid: u16) -> Option<DeviceStatus> {
    let r = write_read(d, &[0x00, 0x20], 1000).filter(|r| r.len() >= 5)?;
    let mut s = headset(pid, "SteelSeries Arctis 9");
    s.percent = map_to_100(r[3] as i32, 0x64, 0x9A);
    s.charging = r[4] == 0x01;
    Some(s)
}

/// Arctis Pro Wireless: 41 AA -> [0]==02 offline; 40 AA -> [0] level 0..4.
fn arctis_pro_cmd(d: &HidDev, cmd: u8) -> Option<Vec<u8>> {
    write_read(d, &[0x00, cmd, 0xAA], 1000).or_else(|| write_read(d, &[cmd, 0xAA], 1000))
}

fn arctis_pro_wireless(d: &HidDev, pid: u16) -> Option<DeviceStatus> {
    let r = arctis_pro_cmd(d, 0x41)?;
    let mut s = headset(pid, "SteelSeries Arctis Pro Wireless");
    s.connected = r[0] != 0x02;
    if s.connected {
        if let Some(r) = arctis_pro_cmd(d, 0x40).filter(|r| r[0] <= 4) {
            s.percent = map_to_100(r[0] as i32, 0, 4);
        }
    }
    Some(s)
}

/// Arctis Nova Pro Wireless: 06 B0 -> [15] status (01 offline, 02 charging), [6] level 0..8.
fn nova_pro_wireless(d: &HidDev, pid: u16) -> Option<DeviceStatus> {
    let r = write_read(d, &[0x06, 0xB0], 1000).filter(|r| r.len() >= 16)?;
    let mut s = headset(pid, "SteelSeries Arctis Nova Pro Wireless");
    s.connected = r[15] != 0x01;
    s.charging = r[15] == 0x02;
    if s.connected {
        s.percent = map_to_100(r[6] as i32, 0, 8);
    }
    Some(s)
}

/// Nova family: 00 B0 -> status block; layout depends on the model.
fn nova_family(d: &HidDev, pid: u16) -> Option<DeviceStatus> {
    let r = write_read(d, &[0x00, 0xB0], 1000).filter(|r| r.len() >= 7)?;
    Some(decode_nova(pid, &r))
}

/// Decodes the Nova family's 00 B0 status block (layout depends on the model). `r` has >= 7 bytes.
fn decode_nova(pid: u16, r: &[u8]) -> DeviceStatus {
    let mut s;
    if [0x220E, 0x2212, 0x2216, 0x2236].contains(&pid) {
        s = headset(pid, "SteelSeries Arctis 7+");
        s.connected = r[1] != 0x01;
        s.percent = map_to_100(r[2] as i32, 0, 4);
        s.charging = r[3] == 0x01;
    } else if [0x2269, 0x226D, 0x2232, 0x2253].contains(&pid) {
        let nova5 = [0x2232, 0x2253].contains(&pid);
        s = headset(pid, if nova5 { "SteelSeries Arctis Nova 5" } else { "SteelSeries Arctis Nova 3P Wireless" });
        s.connected = r[1] != 0x02;
        s.percent = clamp100(r[3] as i32);
        s.charging = r[4] == 0x01;
    } else if pid == 0x230A {
        // Arctis GameBuds: lowest of the buds that are out of the case.
        s = headset(pid, "SteelSeries Arctis GameBuds");
        let mut lvl = -1;
        if r[3] == 0x03 {
            lvl = r[5] as i32;
        }
        if r[4] == 0x03 {
            lvl = if lvl < 0 { r[6] as i32 } else { lvl.min(r[6] as i32) };
        }
        s.connected = lvl >= 0;
        s.percent = if lvl >= 0 { clamp100(lvl) } else { -1 };
    } else {
        // Arctis Nova 7 / 7X / 7P and variants.
        let discrete = [0x2202, 0x2206, 0x220A, 0x223A, 0x227A, 0x22A4].contains(&pid);
        s = headset(pid, "SteelSeries Arctis Nova 7");
        s.connected = r[3] != 0x00;
        s.charging = r[3] == 0x01 || r[3] == 0x02;
        s.percent = if discrete { map_to_100(r[2] as i32, 0, 4) } else { clamp100(r[2] as i32) };
    }
    if !s.connected {
        s.percent = -1;
    }
    s
}

// ---- mice --------------------------------------------------------------------------------------

/// Sends `cmd` and returns the first input report that echoes it, resending every 100 ms within 1 s
/// (SteelSeries GG shares the interface). 40 FF = receiver cannot reach the mouse.
fn query_echo(d: &HidDev, cmd: u8, min_len: usize) -> Option<Vec<u8>> {
    d.flush();
    let end = Deadline::in_ms(1000);
    let mut resend = Deadline::in_ms(0);
    loop {
        if end.passed() {
            return None;
        }
        if resend.passed() {
            if !d.write(&[0x00, cmd]) {
                return None;
            }
            resend = Deadline::in_ms(100);
        }
        let wait = resend.left_ms().min(end.left_ms());
        if wait == 0 {
            continue;
        }
        let Some(r) = d.read(wait) else { continue };
        if r.len() >= min_len && r[0] == cmd {
            return Some(r);
        }
        if r.len() >= 2 && r[0] == 0x40 && r[1] == 0xFF {
            return None;
        }
    }
}

struct AeroxModel {
    pid: u16,
    receiver_pid: u16,
    name: &'static str,
    wired: bool,
    percent: bool,
}

const AEROX: &[AeroxModel] = &[
    AeroxModel { pid: 0x1838, receiver_pid: 0x1838, name: "SteelSeries Aerox 3 Wireless", wired: false, percent: false },
    AeroxModel { pid: 0x183A, receiver_pid: 0x1838, name: "SteelSeries Aerox 3 Wireless", wired: true, percent: false },
    AeroxModel { pid: 0x1878, receiver_pid: 0x1878, name: "SteelSeries Aerox 3 Wireless CS2 Dragon Lore Edition", wired: false, percent: false },
    AeroxModel { pid: 0x187A, receiver_pid: 0x1878, name: "SteelSeries Aerox 3 Wireless CS2 Dragon Lore Edition", wired: true, percent: false },
    AeroxModel { pid: 0x1890, receiver_pid: 0x1890, name: "SteelSeries Aerox 3 Wireless Gen 2", wired: false, percent: true },
    AeroxModel { pid: 0x1892, receiver_pid: 0x1890, name: "SteelSeries Aerox 3 Wireless Gen 2", wired: true, percent: true },
    AeroxModel { pid: 0x1852, receiver_pid: 0x1852, name: "SteelSeries Aerox 5 Wireless", wired: false, percent: false },
    AeroxModel { pid: 0x1854, receiver_pid: 0x1852, name: "SteelSeries Aerox 5 Wireless", wired: true, percent: false },
    AeroxModel { pid: 0x185C, receiver_pid: 0x185C, name: "SteelSeries Aerox 5 Wireless Destiny 2 Lightfall Edition", wired: false, percent: false },
    AeroxModel { pid: 0x185E, receiver_pid: 0x185C, name: "SteelSeries Aerox 5 Wireless Destiny 2 Lightfall Edition", wired: true, percent: false },
    AeroxModel { pid: 0x1858, receiver_pid: 0x1858, name: "SteelSeries Aerox 9 Wireless", wired: false, percent: false },
    AeroxModel { pid: 0x185A, receiver_pid: 0x1858, name: "SteelSeries Aerox 9 Wireless", wired: true, percent: false },
    AeroxModel { pid: 0x1840, receiver_pid: 0x1840, name: "SteelSeries Prime Wireless", wired: false, percent: false },
    AeroxModel { pid: 0x1842, receiver_pid: 0x1840, name: "SteelSeries Prime Wireless", wired: true, percent: false },
    AeroxModel { pid: 0x1860, receiver_pid: 0x1860, name: "SteelSeries Aerox 5 Wireless Diablo IV Edition", wired: false, percent: false },
];

fn aerox(d: &HidDev, pid: u16) -> Option<DeviceStatus> {
    let m = AEROX.iter().find(|a| a.pid == pid)?;
    let cmd = if m.wired { 0x92 } else { 0xD2 };
    let r = query_echo(d, cmd, if m.percent { 3 } else { 2 })?;
    if m.percent && r[1] > 0x02 {
        return None;
    }
    let off = if m.percent { !m.wired && r[1] == 0 && r[2] == 0 } else { r[1] & 0x7F == 0 };
    if off {
        return None;
    }
    let mut s = mouse(pid, m.name, m.wired);
    s.key = format!("steelseries:{}", hex4(m.receiver_pid));
    s.grace_sec = 1800;
    if m.percent {
        s.charging = r[1] != 0;
        if r[2] <= 100 {
            s.percent = r[2] as i32;
        }
    } else if r[1] & 0x7F <= 21 {
        s.percent = ((r[1] & 0x7F) as i32 - 1) * 5;
        s.charging = r[1] & 0x80 != 0;
    }
    Some(s)
}

/// Rival 3 Wireless (+ Gen 2), Rival 650: AA 01 -> [0] %, [2] charging.
fn rival(d: &HidDev, pid: u16) -> Option<DeviceStatus> {
    let r = write_read(d, &[0x00, 0xAA, 0x01], 300).filter(|r| r.len() >= 3)?;
    let rival650 = pid == 0x172B || pid == 0x1726;
    let name = if rival650 {
        "SteelSeries Rival 650 Wireless"
    } else if pid == 0x1872 {
        "SteelSeries Rival 3 Wireless Gen 2"
    } else {
        "SteelSeries Rival 3 Wireless"
    };
    let mut s = mouse(pid, name, pid == 0x172B);
    if rival650 {
        s.key = "steelseries:172B".into();
    }
    s.percent = clamp100(r[0] as i32);
    s.charging = r[2] != 0;
    Some(s)
}

/// Rival Pro: 92 -> 92 00 <mouse %> <flag> <spare battery %>.
fn rival_pro(d: &HidDev, pid: u16) -> Option<DeviceStatus> {
    d.flush();
    if !d.write(&[0x00, 0x92]) {
        return None;
    }
    let end = Deadline::in_ms(500);
    let r = loop {
        let left = end.left_ms();
        if left == 0 {
            return None;
        }
        let r = d.read(left).filter(|r| r.len() >= 3)?;
        if r[0] == 0x92 && r[1] == 0x00 {
            break r;
        }
    };
    if r[2] > 100 {
        return None;
    }
    let mut s = mouse(pid, "SteelSeries Rival Pro", false);
    s.via = "UWB base station".into();
    s.percent = r[2] as i32;
    Some(s)
}

type PollFn = fn(&HidDev, u16) -> Option<DeviceStatus>;
type AcceptFn = fn(&HidColl) -> bool;

fn page_ff43(c: &HidColl) -> bool {
    c.page() == 0xFF43 && c.usage() == 0x0202
}
fn page_ffc0(c: &HidColl) -> bool {
    c.page() == 0xFFC0 && c.usage() == 0x0001
}
fn iface0(c: &HidColl) -> bool {
    c.iface == 0 && c.out_len() > 0
}
fn iface2(c: &HidColl) -> bool {
    c.iface == 2 && c.out_len() > 0
}
fn iface3(c: &HidColl) -> bool {
    c.iface == 3 && c.out_len() > 0
}
fn iface4(c: &HidColl) -> bool {
    c.iface == 4 && c.out_len() > 0
}

fn models() -> Vec<(Vec<u16>, AcceptFn, PollFn)> {
    vec![
        (vec![0x12B3, 0x12B6, 0x12D7], page_ff43 as AcceptFn, arctis1 as PollFn),
        (vec![0x1260, 0x12AD, 0x1252], page_ff43, arctis7),
        (vec![0x12C2], iface0, arctis9),
        (vec![0x1290], iface0, arctis_pro_wireless),
        (vec![0x12E0, 0x225D], iface4, nova_pro_wireless),
        (
            vec![
                0x220E, 0x2212, 0x2216, 0x2236, 0x2269, 0x226D, 0x2232, 0x2253, 0x230A, 0x2202, 0x22A1, 0x227E, 0x2206,
                0x2258, 0x229E, 0x22AD, 0x223A, 0x22A9, 0x227A, 0x22A4, 0x22A5, 0x220A, 0x22A7,
            ],
            page_ffc0,
            nova_family,
        ),
        (AEROX.iter().map(|m| m.pid).collect(), iface3, aerox),
        (vec![0x1830, 0x1872], iface3, rival),
        (vec![0x172B, 0x1726], iface0, rival),
        (vec![0x1880], iface2, rival_pro),
    ]
}

pub fn poll(ctx: &mut PollContext) {
    let models = models();
    for (vid, pid) in ctx.products(0x1038) {
        let Some((_, accept, poll_fn)) = models.iter().find(|(pids, _, _)| pids.contains(&pid)) else { continue };
        for c in ctx.find(vid, pid, accept) {
            let Some(d) = HidDev::open(&c) else { continue };
            let Some(s) = poll_fn(&d, pid) else { continue };
            // Aerox/Prime and Rival 650 can answer on both products; keep one entry, preferring the cable.
            if let Some(dup) = ctx.out.iter_mut().find(|o| o.key == s.key) {
                if s.via == "USB cable" {
                    *dup = s;
                    dup.vid = vid;
                    dup.hw = Some((vid, pid));
                }
                ctx.claim(vid, pid);
            } else {
                ctx.add(DeviceStatus { hw: Some((vid, pid)), ..s }, vid, pid);
            }
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 00 B0 replies recorded from an Arctis Nova 3P Wireless (1038:2269).
    #[test]
    fn nova_3p_on_battery() {
        let s = decode_nova(0x2269, &[0xB0, 0x03, 0x00, 27, 0x00, 0x00, 0x00, 0x00]);
        assert!(s.connected && !s.charging);
        assert_eq!(s.percent, 27);
        assert_eq!(s.name, "SteelSeries Arctis Nova 3P Wireless");
    }

    #[test]
    fn nova_3p_charging_and_switched_off() {
        let s = decode_nova(0x2269, &[0xB0, 0x03, 0x00, 64, 0x01, 0x00, 0x00]);
        assert!(s.charging);
        assert_eq!(s.percent, 64);
        let off = decode_nova(0x2269, &[0xB0, 0x02, 0x00, 64, 0x00, 0x00, 0x00]);
        assert!(!off.connected);
        assert_eq!(off.percent, -1);
    }

    #[test]
    fn nova_7_discrete_levels() {
        // Nova 7 (2202) reports 0..4 steps.
        let s = decode_nova(0x2202, &[0xB0, 0x00, 0x03, 0x03, 0x00, 0x00, 0x00]);
        assert_eq!(s.percent, 75);
    }
}
