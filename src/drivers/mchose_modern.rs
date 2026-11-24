//! MCHOSE's newer (Nordic) mice on VID 3837: the K7 V2 Ultra+ on its receiver 3837:1014 or USB cable
//! 3837:4028, and the other receivers M HUB groups with it (1018, e.g. the A7 V3 Ultra+; 1016, 102C,
//! 102D). The older Realtek receivers on this VID (1001, 100A..100C, 1012) are mchose.rs's.
//!
//! Protocol from MCHOSE's own M HUB web driver (readDeviceInfo, command 0x0900) as documented by
//! MchoseBatteryTray (MIT, github.com/HifumiDaisuki01/MchoseBatteryTray, PROTOCOL.md) and the original
//! C++ BatteryHub (GPL-3.0, src/drivers/mchose_modern.cpp: verified on the K7 V2 Ultra+ receiver and
//! cable; the other receiver ids are M HUB's newMouseWirelessPids); implemented here from those facts.
//! Vendor collection FF01:0001 (64-byte input / output report 4D):
//!
//!   request  4D 01 01 00 00 09 00 00 XX           read (01), length 0, command 0x0900 (LE),
//!                                                 XX = xor of bytes 2..7
//!   reply    4D 01 FF LL 00 09 BB QQ <LL data> XX XX = xor of bytes 2..7+LL
//!            data[0..1] = 3837 (LE), [10] link (0 none, 1 cable, 2 receiver), [11] 1 = charging,
//!            [12] level %
//!
//! With the mouse off the receiver answers with a short frame (LL = 1, FF): shown as disconnected.
//! Only this read is sent; nothing is written to the mouse.
use crate::device::{DeviceKind, DeviceStatus, PollContext};
use crate::hid::{self, HidColl, HidDev};
use crate::util::{hex4, sleep_ms, Deadline};

const VID: u16 = 0x3837;
const RECEIVERS: [u16; 5] = [0x1014, 0x1018, 0x1016, 0x102C, 0x102D];
const CABLE: u16 = 0x4028;
const REPORT: u8 = 0x4D;
const CMD_INFO: u16 = 0x0900;
const DATA: usize = 8;

fn xor(b: &[u8], from: usize, to: usize) -> u8 {
    b[from..=to].iter().fold(0, |a, &x| a ^ x)
}

pub fn request() -> [u8; 64] {
    let mut r = [0u8; 64];
    r[..6].copy_from_slice(&[REPORT, 0x01, 0x01, 0x00, (CMD_INFO & 0xFF) as u8, (CMD_INFO >> 8) as u8]);
    r[DATA] = xor(&r, 2, 7);
    r
}

#[derive(Debug, PartialEq, Eq)]
pub enum Info {
    /// Level, charging.
    Battery(i32, bool),
    /// The receiver answered, but no mouse is linked.
    NotLinked,
}

/// Decodes one input report (report ID included) as the reply to the device-info read.
pub fn decode(r: &[u8]) -> Option<Info> {
    if r.len() < DATA + 2 || r[0] != REPORT || u16::from_le_bytes([r[4], r[5]]) != CMD_INFO {
        return None;
    }
    let len = r[3] as usize;
    if DATA + len >= r.len() {
        return None;
    }
    if len < 14 {
        // Header only, FF in place of the model id: the mouse is off or asleep. Our own request
        // read back (length 0) is not an answer.
        return (len == 1 && r[DATA] == 0xFF).then_some(Info::NotLinked);
    }
    if r[DATA + len] != xor(r, 2, 7 + len) || u16::from_le_bytes([r[DATA], r[DATA + 1]]) != VID {
        return None;
    }
    if r[DATA + 10] == 0 {
        return Some(Info::NotLinked);
    }
    (r[DATA + 12] <= 100).then(|| Info::Battery(r[DATA + 12] as i32, r[DATA + 11] == 1))
}

fn read(c: &HidColl) -> Option<Info> {
    let d = HidDev::open(c)?;
    d.flush();
    let end = Deadline::in_ms(1000);
    for attempt in 0..3 {
        if attempt > 0 {
            if end.left_ms() <= 150 {
                break;
            }
            sleep_ms(150);
        }
        if !d.write_timeout(&request(), 250) {
            continue;
        }
        let reply = Deadline::in_ms(250.min(end.left_ms() as u64));
        while !reply.passed() {
            if let Some(info) = d.read(reply.left_ms()).and_then(|r| decode(&r)) {
                return Some(info);
            }
        }
    }
    None
}

pub fn poll(ctx: &mut PollContext) {
    for pid in RECEIVERS.into_iter().chain([CABLE]) {
        if ctx.is_claimed(VID, pid) {
            continue;
        }
        let colls = ctx.find(VID, pid, |c| c.page() == 0xFF01 && c.usage() == 0x0001 && c.in_len() == 64 && c.out_len() == 64);
        let Some((c, info)) = colls.iter().find_map(|c| read(c).map(|i| (c, i))) else { continue };
        let name = hid::product_string(c);
        let name = name.trim();
        let cable = pid == CABLE;
        let base = DeviceStatus {
            key: format!("mchose:3837:{}", hex4(pid)),
            name: if name.is_empty() { "MCHOSE mouse".into() } else { name.to_string() },
            kind: DeviceKind::Mouse,
            via: if cable { "USB cable" } else { "2.4 GHz receiver" }.into(),
            hw: Some((VID, pid)),
            grace_sec: 1800,
            ..Default::default()
        };
        let s = match info {
            Info::Battery(level, charging) => DeviceStatus { percent: level, charging: charging || cable, ..base },
            Info::NotLinked => DeviceStatus { connected: false, ..base },
        };
        ctx.add(s, VID, pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A device-info reply with the given data block, checksum filled in.
    fn reply(data: &[u8]) -> Vec<u8> {
        let mut f = vec![0u8; 64];
        f[..8].copy_from_slice(&[REPORT, 0x01, 0x01, data.len() as u8, 0x00, 0x09, 0x00, 0x00]);
        f[DATA..DATA + data.len()].copy_from_slice(data);
        f[DATA + data.len()] = xor(&f, 2, 7 + data.len());
        f
    }

    fn info(link: u8, charging: u8, level: u8) -> Vec<u8> {
        let mut d = vec![0u8; 14];
        d[..4].copy_from_slice(&[0x37, 0x38, 0x23, 0x20]);
        d[10] = link;
        d[11] = charging;
        d[12] = level;
        reply(&d)
    }

    #[test]
    fn request_frame() {
        // xor(01 00 00 09 00 00) = 08
        assert_eq!(request()[..9], [0x4D, 0x01, 0x01, 0x00, 0x00, 0x09, 0x00, 0x00, 0x08]);
    }

    #[test]
    fn battery() {
        // The original's K7 V2 Ultra+ readings: 29 % on the receiver, 25 % charging on the cable.
        assert_eq!(decode(&info(2, 0, 29)), Some(Info::Battery(29, false)));
        assert_eq!(decode(&info(1, 1, 25)), Some(Info::Battery(25, true)));
    }

    #[test]
    fn not_linked() {
        assert_eq!(decode(&reply(&[0xFF])), Some(Info::NotLinked));
        assert_eq!(decode(&info(0, 0, 0)), Some(Info::NotLinked));
    }

    #[test]
    fn rejected() {
        assert_eq!(decode(&request()), None); // echo
        let mut bad = info(2, 0, 50);
        bad[DATA + 14] ^= 1;
        assert_eq!(decode(&bad), None); // checksum
        assert_eq!(decode(&info(2, 0, 101)), None);
        let mut other = info(2, 0, 50);
        other[5] = 0x0A;
        assert_eq!(decode(&other), None); // other command
    }
}
