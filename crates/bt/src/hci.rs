//! HCI commands (built as `opcode, length, parameters`, without the H4
//! packet-type byte), events, and ACL data packet headers.

use crate::{Addr, AddrType, le16, le32};
use alloc::string::String;
use alloc::vec::Vec;

/// H4 / UART packet types.
pub const H4_CMD: u8 = 0x01;
pub const H4_ACL: u8 = 0x02;
pub const H4_EVENT: u8 = 0x04;

pub mod op {
    pub const INQUIRY: u16 = 0x0401;
    pub const INQUIRY_CANCEL: u16 = 0x0402;
    pub const CREATE_CONNECTION: u16 = 0x0405;
    pub const DISCONNECT: u16 = 0x0406;
    pub const ACCEPT_CONNECTION: u16 = 0x0409;
    pub const REJECT_CONNECTION: u16 = 0x040A;
    pub const LINK_KEY_REPLY: u16 = 0x040B;
    pub const LINK_KEY_NEG_REPLY: u16 = 0x040C;
    pub const PIN_CODE_NEG_REPLY: u16 = 0x040E;
    pub const AUTH_REQUESTED: u16 = 0x0411;
    pub const SET_CONN_ENCRYPTION: u16 = 0x0413;
    pub const REMOTE_NAME_REQUEST: u16 = 0x0419;
    pub const IO_CAP_REPLY: u16 = 0x042B;
    pub const USER_CONFIRM_REPLY: u16 = 0x042C;
    pub const USER_CONFIRM_NEG_REPLY: u16 = 0x042D;
    pub const USER_PASSKEY_REPLY: u16 = 0x042E;
    pub const USER_PASSKEY_NEG_REPLY: u16 = 0x042F;
    pub const IO_CAP_NEG_REPLY: u16 = 0x0434;
    pub const SET_EVENT_MASK: u16 = 0x0C01;
    pub const RESET: u16 = 0x0C03;
    pub const WRITE_LOCAL_NAME: u16 = 0x0C13;
    pub const WRITE_SCAN_ENABLE: u16 = 0x0C1A;
    pub const WRITE_CLASS_OF_DEVICE: u16 = 0x0C24;
    pub const WRITE_INQUIRY_MODE: u16 = 0x0C45;
    pub const WRITE_SIMPLE_PAIRING_MODE: u16 = 0x0C56;
    pub const WRITE_LE_HOST_SUPPORT: u16 = 0x0C6D;
    pub const WRITE_SC_HOST_SUPPORT: u16 = 0x0C7A;
    pub const READ_LOCAL_VERSION: u16 = 0x1001;
    pub const READ_LOCAL_FEATURES: u16 = 0x1003;
    pub const READ_BUFFER_SIZE: u16 = 0x1005;
    pub const READ_BD_ADDR: u16 = 0x1009;
    pub const LE_SET_EVENT_MASK: u16 = 0x2001;
    pub const LE_READ_BUFFER_SIZE: u16 = 0x2002;
    pub const LE_SET_SCAN_PARAMS: u16 = 0x200B;
    pub const LE_SET_SCAN_ENABLE: u16 = 0x200C;
    pub const LE_CREATE_CONN: u16 = 0x200D;
    pub const LE_CREATE_CONN_CANCEL: u16 = 0x200E;
    pub const LE_CONN_UPDATE: u16 = 0x2013;
    pub const LE_RAND: u16 = 0x2018;
    pub const LE_START_ENCRYPTION: u16 = 0x2019;
    pub const LE_LTK_REPLY: u16 = 0x201A;
    pub const LE_LTK_NEG_REPLY: u16 = 0x201B;
    pub const LE_REMOTE_CONN_PARAM_REPLY: u16 = 0x2020;
}

/// A command packet.
pub fn cmd(opcode: u16, params: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(3 + params.len());
    v.extend_from_slice(&opcode.to_le_bytes());
    v.push(params.len() as u8);
    v.extend_from_slice(params);
    v
}

/// Event mask: the BR/EDR events used here plus LE meta events.
pub fn set_event_mask() -> Vec<u8> {
    cmd(
        op::SET_EVENT_MASK,
        &[0xFF, 0xFF, 0xFB, 0xFF, 0x07, 0xF8, 0xBF, 0x3D],
    )
}

/// LE events: connection complete, advertising report, connection update,
/// remote features, LTK request, remote connection parameter request,
/// enhanced connection complete.
pub fn le_set_event_mask() -> Vec<u8> {
    cmd(op::LE_SET_EVENT_MASK, &[0x3F, 0x02, 0, 0, 0, 0, 0, 0])
}

pub fn le_set_scan_params(active: bool) -> Vec<u8> {
    let mut p = alloc::vec![active as u8];
    p.extend_from_slice(&0x0060u16.to_le_bytes()); // 60 ms interval
    p.extend_from_slice(&0x0030u16.to_le_bytes()); // 30 ms window
    p.push(0); // own address: public
    p.push(0); // accept all advertisements
    cmd(op::LE_SET_SCAN_PARAMS, &p)
}

pub fn le_set_scan_enable(on: bool) -> Vec<u8> {
    cmd(op::LE_SET_SCAN_ENABLE, &[on as u8, 0])
}

pub fn le_create_conn(peer: &Addr, t: AddrType) -> Vec<u8> {
    let mut p = Vec::with_capacity(25);
    p.extend_from_slice(&0x0060u16.to_le_bytes());
    p.extend_from_slice(&0x0030u16.to_le_bytes());
    p.push(0); // no filter accept list
    p.push(t as u8);
    p.extend_from_slice(&peer.0);
    p.push(0); // own address: public
    p.extend_from_slice(&0x0018u16.to_le_bytes()); // 30 ms
    p.extend_from_slice(&0x0028u16.to_le_bytes()); // 50 ms
    p.extend_from_slice(&0u16.to_le_bytes()); // latency
    p.extend_from_slice(&0x01F4u16.to_le_bytes()); // 5 s supervision
    p.extend_from_slice(&[0, 0, 0, 0]);
    cmd(op::LE_CREATE_CONN, &p)
}

pub fn le_start_encryption(handle: u16, rand: &[u8; 8], ediv: u16, ltk: &[u8; 16]) -> Vec<u8> {
    let mut p = handle.to_le_bytes().to_vec();
    p.extend_from_slice(rand);
    p.extend_from_slice(&ediv.to_le_bytes());
    p.extend_from_slice(ltk);
    cmd(op::LE_START_ENCRYPTION, &p)
}

pub fn le_ltk_reply(handle: u16, ltk: &[u8; 16]) -> Vec<u8> {
    let mut p = handle.to_le_bytes().to_vec();
    p.extend_from_slice(ltk);
    cmd(op::LE_LTK_REPLY, &p)
}

pub fn le_remote_conn_param_reply(handle: u16, min: u16, max: u16, lat: u16, to: u16) -> Vec<u8> {
    let mut p = handle.to_le_bytes().to_vec();
    for v in [min, max, lat, to, 0, 0] {
        p.extend_from_slice(&v.to_le_bytes());
    }
    cmd(op::LE_REMOTE_CONN_PARAM_REPLY, &p)
}

pub fn disconnect(handle: u16, reason: u8) -> Vec<u8> {
    let mut p = handle.to_le_bytes().to_vec();
    p.push(reason);
    cmd(op::DISCONNECT, &p)
}

/// General inquiry for `secs` seconds (in 1.28 s units, rounded up).
pub fn inquiry(secs: u32) -> Vec<u8> {
    let len = (secs * 100).div_ceil(128).clamp(1, 0x30) as u8;
    cmd(op::INQUIRY, &[0x33, 0x8B, 0x9E, len, 0])
}

pub fn create_connection(peer: &Addr) -> Vec<u8> {
    let mut p = peer.0.to_vec();
    p.extend_from_slice(&0xCC18u16.to_le_bytes()); // DM/DH 1, 3, 5
    p.push(0x02); // page scan repetition mode R2
    p.push(0);
    p.extend_from_slice(&0u16.to_le_bytes());
    p.push(1); // allow role switch
    cmd(op::CREATE_CONNECTION, &p)
}

pub fn addr_cmd(opcode: u16, a: &Addr, extra: &[u8]) -> Vec<u8> {
    let mut p = a.0.to_vec();
    p.extend_from_slice(extra);
    cmd(opcode, &p)
}

pub fn handle_cmd(opcode: u16, handle: u16, extra: &[u8]) -> Vec<u8> {
    let mut p = handle.to_le_bytes().to_vec();
    p.extend_from_slice(extra);
    cmd(opcode, &p)
}

/// An inquiry or extended inquiry result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InquiryResult {
    pub addr: Addr,
    pub class: u32,
    pub rssi: Option<i8>,
    pub eir: Vec<u8>,
}

/// An LE advertising report (legacy or extended).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdvReport {
    pub event_type: u16,
    pub addr_type: AddrType,
    pub addr: Addr,
    pub data: Vec<u8>,
    pub rssi: i8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    CommandComplete {
        opcode: u16,
        ret: Vec<u8>,
    },
    CommandStatus {
        status: u8,
        opcode: u16,
    },
    InquiryComplete(u8),
    Inquiry(Vec<InquiryResult>),
    ConnectionComplete {
        status: u8,
        handle: u16,
        addr: Addr,
        acl: bool,
    },
    ConnectionRequest {
        addr: Addr,
        class: u32,
        acl: bool,
    },
    DisconnectionComplete {
        handle: u16,
        reason: u8,
    },
    AuthComplete {
        status: u8,
        handle: u16,
    },
    RemoteName {
        status: u8,
        addr: Addr,
        name: String,
    },
    EncryptionChange {
        status: u8,
        handle: u16,
        on: bool,
    },
    EncryptionKeyRefresh {
        status: u8,
        handle: u16,
    },
    CompletedPackets(Vec<(u16, u16)>),
    PinCodeRequest(Addr),
    LinkKeyRequest(Addr),
    LinkKeyNotification {
        addr: Addr,
        key: [u8; 16],
        key_type: u8,
    },
    IoCapRequest(Addr),
    IoCapResponse {
        addr: Addr,
        io_cap: u8,
        auth: u8,
    },
    UserConfirmRequest {
        addr: Addr,
        value: u32,
    },
    UserPasskeyRequest(Addr),
    UserPasskeyNotification {
        addr: Addr,
        passkey: u32,
    },
    SimplePairingComplete {
        status: u8,
        addr: Addr,
    },
    LeConnectionComplete {
        status: u8,
        handle: u16,
        central: bool,
        peer_type: AddrType,
        peer: Addr,
        interval: u16,
    },
    LeAdvertising(Vec<AdvReport>),
    LeConnectionUpdate {
        status: u8,
        handle: u16,
        interval: u16,
    },
    LeLtkRequest {
        handle: u16,
        rand: [u8; 8],
        ediv: u16,
    },
    LeRemoteConnParamRequest {
        handle: u16,
        min: u16,
        max: u16,
        latency: u16,
        timeout: u16,
    },
    HardwareError(u8),
    Vendor(Vec<u8>),
    Other(u8, Vec<u8>),
}

fn addr_at(b: &[u8], o: usize) -> Option<Addr> {
    Addr::from_slice(b.get(o..)?)
}

fn class_at(b: &[u8], o: usize) -> Option<u32> {
    let c = b.get(o..o + 3)?;
    Some(u32::from_le_bytes([c[0], c[1], c[2], 0]))
}

impl Event {
    /// Parse an event packet (`code, length, parameters`).
    pub fn parse(pkt: &[u8]) -> Option<Event> {
        let code = *pkt.first()?;
        let len = *pkt.get(1)? as usize;
        let p = pkt.get(2..2 + len)?;
        Some(match code {
            0x01 => Event::InquiryComplete(*p.first()?),
            0x02 | 0x22 => {
                // Fields are grouped per parameter across the responses.
                let n = *p.first()? as usize;
                let rssi = code == 0x22;
                // address, page scan mode, reserved (1 byte with RSSI,
                // 2 without), class, clock offset[, RSSI]
                let cod_off = if rssi { 1 + 8 * n } else { 1 + 9 * n };
                let mut v = Vec::new();
                for i in 0..n {
                    let addr = addr_at(p, 1 + 6 * i)?;
                    let class = class_at(p, cod_off + 3 * i)?;
                    let r = if rssi {
                        Some(*p.get(1 + 13 * n + i)? as i8)
                    } else {
                        None
                    };
                    v.push(InquiryResult {
                        addr,
                        class,
                        rssi: r,
                        eir: Vec::new(),
                    });
                }
                Event::Inquiry(v)
            }
            0x2F => Event::Inquiry(alloc::vec![InquiryResult {
                addr: addr_at(p, 1)?,
                class: class_at(p, 9)?,
                rssi: Some(*p.get(14)? as i8),
                eir: p.get(15..).unwrap_or(&[]).to_vec(),
            }]),
            0x03 => Event::ConnectionComplete {
                status: p[0],
                handle: le16(p, 1)? & 0x0FFF,
                addr: addr_at(p, 3)?,
                acl: *p.get(9)? == 1,
            },
            0x04 => Event::ConnectionRequest {
                addr: addr_at(p, 0)?,
                class: class_at(p, 6)?,
                acl: *p.get(9)? == 1,
            },
            0x05 => Event::DisconnectionComplete {
                handle: le16(p, 1)? & 0x0FFF,
                reason: *p.get(3)?,
            },
            0x06 => Event::AuthComplete {
                status: p[0],
                handle: le16(p, 1)? & 0x0FFF,
            },
            0x07 => {
                let name = p.get(7..).unwrap_or(&[]);
                let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
                Event::RemoteName {
                    status: p[0],
                    addr: addr_at(p, 1)?,
                    name: String::from_utf8_lossy(&name[..end]).into_owned(),
                }
            }
            0x08 => Event::EncryptionChange {
                status: p[0],
                handle: le16(p, 1)? & 0x0FFF,
                on: *p.get(3)? != 0,
            },
            0x0E => Event::CommandComplete {
                opcode: le16(p, 1)?,
                ret: p.get(3..).unwrap_or(&[]).to_vec(),
            },
            0x0F => Event::CommandStatus {
                status: p[0],
                opcode: le16(p, 2)?,
            },
            0x10 => Event::HardwareError(*p.first()?),
            0x13 => {
                let n = *p.first()? as usize;
                let mut v = Vec::new();
                for i in 0..n {
                    v.push((le16(p, 1 + 4 * i)? & 0x0FFF, le16(p, 3 + 4 * i)?));
                }
                Event::CompletedPackets(v)
            }
            0x16 => Event::PinCodeRequest(addr_at(p, 0)?),
            0x17 => Event::LinkKeyRequest(addr_at(p, 0)?),
            0x18 => Event::LinkKeyNotification {
                addr: addr_at(p, 0)?,
                key: p.get(6..22)?.try_into().ok()?,
                key_type: *p.get(22)?,
            },
            0x30 => Event::EncryptionKeyRefresh {
                status: p[0],
                handle: le16(p, 1)? & 0x0FFF,
            },
            0x31 => Event::IoCapRequest(addr_at(p, 0)?),
            0x32 => Event::IoCapResponse {
                addr: addr_at(p, 0)?,
                io_cap: *p.get(6)?,
                auth: *p.get(8)?,
            },
            0x33 => Event::UserConfirmRequest {
                addr: addr_at(p, 0)?,
                value: le32(p, 6)?,
            },
            0x34 => Event::UserPasskeyRequest(addr_at(p, 0)?),
            0x36 => Event::SimplePairingComplete {
                status: p[0],
                addr: addr_at(p, 1)?,
            },
            0x3B => Event::UserPasskeyNotification {
                addr: addr_at(p, 0)?,
                passkey: le32(p, 6)?,
            },
            0x3E => return Self::parse_le(p),
            0xFF => Event::Vendor(p.to_vec()),
            c => Event::Other(c, p.to_vec()),
        })
    }

    fn parse_le(p: &[u8]) -> Option<Event> {
        Some(match *p.first()? {
            0x01 | 0x0A => {
                let enhanced = p[0] == 0x0A;
                let iv = if enhanced { 24 } else { 12 };
                Event::LeConnectionComplete {
                    status: *p.get(1)?,
                    handle: le16(p, 2)? & 0x0FFF,
                    central: *p.get(4)? == 0,
                    peer_type: AddrType::from_u8(*p.get(5)?),
                    peer: addr_at(p, 6)?,
                    interval: le16(p, iv).unwrap_or(0),
                }
            }
            0x02 => {
                let n = *p.get(1)? as usize;
                let mut o = 2;
                let mut v = Vec::new();
                for _ in 0..n {
                    let et = *p.get(o)?;
                    let at = *p.get(o + 1)?;
                    let addr = addr_at(p, o + 2)?;
                    let dl = *p.get(o + 8)? as usize;
                    let data = p.get(o + 9..o + 9 + dl)?.to_vec();
                    let rssi = *p.get(o + 9 + dl)? as i8;
                    o += 10 + dl;
                    v.push(AdvReport {
                        event_type: et as u16,
                        addr_type: AddrType::from_u8(at),
                        addr,
                        data,
                        rssi,
                    });
                }
                Event::LeAdvertising(v)
            }
            0x0D => {
                let n = *p.get(1)? as usize;
                let mut o = 2;
                let mut v = Vec::new();
                for _ in 0..n {
                    let et = le16(p, o)?;
                    let at = *p.get(o + 2)?;
                    let addr = addr_at(p, o + 3)?;
                    let rssi = *p.get(o + 13)? as i8;
                    let dl = *p.get(o + 23)? as usize;
                    let data = p.get(o + 24..o + 24 + dl)?.to_vec();
                    o += 24 + dl;
                    v.push(AdvReport {
                        event_type: et,
                        addr_type: AddrType::from_u8(at),
                        addr,
                        data,
                        rssi,
                    });
                }
                Event::LeAdvertising(v)
            }
            0x03 => Event::LeConnectionUpdate {
                status: *p.get(1)?,
                handle: le16(p, 2)? & 0x0FFF,
                interval: le16(p, 4)?,
            },
            0x05 => Event::LeLtkRequest {
                handle: le16(p, 1)? & 0x0FFF,
                rand: p.get(3..11)?.try_into().ok()?,
                ediv: le16(p, 11)?,
            },
            0x06 => Event::LeRemoteConnParamRequest {
                handle: le16(p, 1)? & 0x0FFF,
                min: le16(p, 3)?,
                max: le16(p, 5)?,
                latency: le16(p, 7)?,
                timeout: le16(p, 9)?,
            },
            _ => Event::Other(0x3E, p.to_vec()),
        })
    }
}

/// ACL packet boundary flags.
pub const PB_START_NO_FLUSH: u8 = 0b00;
pub const PB_CONT: u8 = 0b01;
pub const PB_START: u8 = 0b10;

/// Split an L2CAP frame into ACL packets of at most `mtu` data bytes.
pub fn acl_packets(handle: u16, frame: &[u8], mtu: usize, le: bool) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for (i, chunk) in frame.chunks(mtu.max(1)).enumerate() {
        let pb = if i > 0 {
            PB_CONT
        } else if le {
            PB_START_NO_FLUSH
        } else {
            PB_START
        };
        let h = (handle & 0x0FFF) | (pb as u16) << 12;
        let mut p = Vec::with_capacity(4 + chunk.len());
        p.extend_from_slice(&h.to_le_bytes());
        p.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
        p.extend_from_slice(chunk);
        out.push(p);
    }
    out
}

/// Parse an ACL packet: (handle, packet boundary flag, data).
pub fn parse_acl(pkt: &[u8]) -> Option<(u16, u8, &[u8])> {
    let h = le16(pkt, 0)?;
    let len = le16(pkt, 2)? as usize;
    Some((h & 0x0FFF, ((h >> 12) & 3) as u8, pkt.get(4..4 + len)?))
}

/// Parsed return parameters of the commands the host needs.
pub fn bd_addr(ret: &[u8]) -> Option<Addr> {
    (*ret.first()? == 0).then(|| addr_at(ret, 1))?
}

/// READ_BUFFER_SIZE: (ACL data length, ACL packets).
pub fn buffer_size(ret: &[u8]) -> Option<(u16, u16)> {
    (*ret.first()? == 0).then(|| Some((le16(ret, 1)?, le16(ret, 4)?)))?
}

/// LE_READ_BUFFER_SIZE: (LE ACL data length, packets); zero length means
/// the BR/EDR buffers are shared.
pub fn le_buffer_size(ret: &[u8]) -> Option<(u16, u8)> {
    (*ret.first()? == 0).then(|| Some((le16(ret, 1)?, *ret.get(3)?)))?
}

/// READ_LOCAL_VERSION: (HCI version, manufacturer).
pub fn local_version(ret: &[u8]) -> Option<(u8, u16)> {
    (*ret.first()? == 0).then(|| Some((*ret.get(1)?, le16(ret, 5)?)))?
}

/// Printable HCI/Core version.
pub fn version_name(v: u8) -> &'static str {
    match v {
        6 => "4.0",
        7 => "4.1",
        8 => "4.2",
        9 => "5.0",
        10 => "5.1",
        11 => "5.2",
        12 => "5.3",
        13 => "5.4",
        14 => "6.0",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn command_layouts() {
        assert_eq!(cmd(op::RESET, &[]), [0x03, 0x0C, 0]);
        let a = Addr::parse("C0:11:22:33:44:55").unwrap();
        let c = le_create_conn(&a, AddrType::Random);
        assert_eq!(c.len(), 3 + 25);
        assert_eq!(&c[..3], &[0x0D, 0x20, 25]);
        assert_eq!(c[3 + 5], 1);
        assert_eq!(&c[3 + 6..3 + 12], &[0x55, 0x44, 0x33, 0x22, 0x11, 0xC0]);
        assert_eq!(
            le_start_encryption(0x40, &[0; 8], 0, &[7; 16]).len(),
            3 + 28
        );
        assert_eq!(inquiry(5), [0x01, 0x04, 5, 0x33, 0x8B, 0x9E, 4, 0]);
    }

    #[test]
    fn events() {
        let cc = [
            0x0E, 10, 1, 0x09, 0x10, 0, 0x55, 0x44, 0x33, 0x22, 0x11, 0x00,
        ];
        match Event::parse(&cc).unwrap() {
            Event::CommandComplete { opcode, ret } => {
                assert_eq!(opcode, op::READ_BD_ADDR);
                assert_eq!(bd_addr(&ret).unwrap().to_string(), "00:11:22:33:44:55");
            }
            e => panic!("{:?}", e),
        }
        let adv = [
            0x3E, 15, 0x02, 1, 0x00, 1, 0x55, 0x44, 0x33, 0x22, 0x11, 0xC0, 3, 2, 1, 6, 0xC4,
        ];
        match Event::parse(&adv).unwrap() {
            Event::LeAdvertising(v) => {
                assert_eq!(v[0].addr_type, AddrType::Random);
                assert_eq!(v[0].addr.to_string(), "C0:11:22:33:44:55");
                assert_eq!(v[0].data, [2, 1, 6]);
                assert_eq!(v[0].rssi, -60);
            }
            e => panic!("{:?}", e),
        }
        let conn = [
            0x3E, 19, 0x01, 0, 0x40, 0x00, 0, 1, 0x55, 0x44, 0x33, 0x22, 0x11, 0xC0, 0x28, 0, 0, 0,
            0xF4, 1, 0,
        ];
        assert_eq!(
            Event::parse(&conn).unwrap(),
            Event::LeConnectionComplete {
                status: 0,
                handle: 0x40,
                central: true,
                peer_type: AddrType::Random,
                peer: Addr::parse("C0:11:22:33:44:55").unwrap(),
                interval: 0x28,
            }
        );
        let ncp = [0x13, 5, 1, 0x40, 0x00, 2, 0];
        assert_eq!(
            Event::parse(&ncp).unwrap(),
            Event::CompletedPackets(alloc::vec![(0x40, 2)])
        );
        // Inquiry result with RSSI: one response.
        let mut ir = alloc::vec![0x22, 15, 1];
        ir.extend_from_slice(&[1, 2, 3, 4, 5, 6, 1, 0, 0x40, 0x25, 0x00, 0, 0, 0xC8]);
        match Event::parse(&ir).unwrap() {
            Event::Inquiry(v) => {
                assert_eq!(v[0].class, 0x002540);
                assert_eq!(v[0].rssi, Some(-56));
            }
            e => panic!("{:?}", e),
        }
    }

    #[test]
    fn acl_fragments() {
        let frame: Vec<u8> = (0..60).collect();
        let p = acl_packets(0x41, &frame, 27, true);
        assert_eq!(p.len(), 3);
        assert_eq!(
            parse_acl(&p[0]).unwrap(),
            (0x41, PB_START_NO_FLUSH, &frame[..27])
        );
        assert_eq!(parse_acl(&p[2]).unwrap(), (0x41, PB_CONT, &frame[54..]));
        assert_eq!(
            parse_acl(&acl_packets(1, &frame, 100, false)[0]).unwrap().1,
            PB_START
        );
    }
}
