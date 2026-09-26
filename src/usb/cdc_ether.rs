//! USB Ethernet: CDC ECM (Ethernet dongles, QEMU `usb-net`), CDC NCM
//! (newer phones and 2.5G/5G dongles) and RNDIS (Android USB tethering and
//! many older gadgets).

use super::UsbDevice;
use crate::errno::*;
use crate::mm::dma::DmaBuffer;
use crate::net::{self, NetDevice, RxQueue};
use crate::sched::WaitQueue;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use usb_desc::ncm::{self, NtbParams};
use usb_desc::{CLASS_CDC, CLASS_CDC_DATA, Interface, TransferType};

const CS_INTERFACE: u8 = 0x24;
const SUBTYPE_UNION: u8 = 0x06;
const SUBTYPE_ETHERNET: u8 = 0x0F;
const SET_ETHERNET_PACKET_FILTER: u8 = 0x43;
const GET_NTB_PARAMETERS: u8 = 0x80;
const SET_NTB_INPUT_SIZE: u8 = 0x86;
const SUBCLASS_ECM: u8 = 0x06;
const SUBCLASS_NCM: u8 = 0x0D;
const FRAME_MAX: usize = 1600 + 64;
const NCM_IN_MAX: u32 = 16384;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Proto {
    Ecm,
    Ncm(NtbParams),
    Rndis,
}

pub struct CdcEther {
    dev: Arc<UsbDevice>,
    mac: [u8; 6],
    rxq: RxQueue,
    txq: spin::Mutex<VecDeque<Vec<u8>>>,
    tx_wq: WaitQueue,
    link: AtomicBool,
    proto: Proto,
}

impl NetDevice for CdcEther {
    fn mac(&self) -> [u8; 6] {
        self.mac
    }
    fn link_up(&self) -> bool {
        self.link.load(Ordering::SeqCst) && !self.dev.is_gone()
    }
    fn driver(&self) -> &'static str {
        match self.proto {
            Proto::Rndis => "rndis_host",
            Proto::Ncm(_) => "cdc_ncm",
            Proto::Ecm => "cdc_ether",
        }
    }
    fn transmit(&self, frame: &[u8]) -> KResult<()> {
        if self.dev.is_gone() {
            return Err(ENODEV);
        }
        {
            let mut q = self.txq.lock();
            if q.len() >= 64 {
                return Err(EAGAIN);
            }
            q.push_back(frame.to_vec());
        }
        self.tx_wq.wake_all();
        Ok(())
    }
    fn receive(&self) -> Option<Vec<u8>> {
        self.rxq.pop()
    }
}

fn parse_mac(s: &str) -> Option<[u8; 6]> {
    if s.len() != 12 {
        return None;
    }
    let mut m = [0u8; 6];
    for (i, b) in m.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(m)
}

/// Functional descriptors: (union data interface, iMACAddress index).
fn functional(iface: &Interface) -> (Option<u8>, u8) {
    let mut data_if = None;
    let mut mac_idx = 0u8;
    let mut i = 0;
    let e = &iface.extra;
    while i + 3 <= e.len() {
        let len = e[i] as usize;
        if len < 3 || i + len > e.len() {
            break;
        }
        if e[i + 1] == CS_INTERFACE {
            match e[i + 2] {
                SUBTYPE_UNION if len >= 5 => data_if = Some(e[i + 4]),
                SUBTYPE_ETHERNET if len >= 4 => mac_idx = e[i + 3],
                _ => {}
            }
        }
        i += len;
    }
    (data_if, mac_idx)
}

fn random_mac() -> [u8; 6] {
    let mut m = [0u8; 6];
    crate::drivers::random::fill(&mut m);
    m[0] = (m[0] & 0xFE) | 0x02;
    m
}

fn is_rndis(i: &Interface) -> bool {
    matches!(
        (i.class, i.subclass, i.protocol),
        (0xE0, 1, 3) | (CLASS_CDC, 2, 0xFF) | (0xEF, 4, 1)
    )
}

pub fn probe(dev: &Arc<UsbDevice>, iface: &Interface) -> bool {
    let rndis = is_rndis(iface);
    let ncm = iface.class == CLASS_CDC && iface.subclass == SUBCLASS_NCM;
    if !(rndis || ncm || (iface.class == CLASS_CDC && iface.subclass == SUBCLASS_ECM)) {
        return false;
    }
    let (union_if, mac_idx) = functional(iface);
    let data_num = union_if.unwrap_or(iface.number + 1);
    let data = dev.config.lock().as_ref().and_then(|c| {
        c.interfaces
            .iter()
            .find(|d| d.number == data_num && d.class == CLASS_CDC_DATA && d.endpoints.len() >= 2)
            .cloned()
    });
    let Some(data) = data else { return false };
    let (Some(ep_in), Some(ep_out)) = (
        data.find_endpoint(TransferType::Bulk, true),
        data.find_endpoint(TransferType::Bulk, false),
    ) else {
        return false;
    };
    if dev.configure_endpoints(&[ep_in, ep_out]).is_err() {
        return false;
    }
    let mut proto = if rndis { Proto::Rndis } else { Proto::Ecm };
    let mac = if rndis {
        match rndis_init(dev, iface.number) {
            Some(m) => m,
            None => {
                crate::println!("[usb] {}: RNDIS initialisation failed", dev.name());
                return false;
            }
        }
    } else {
        if ncm {
            // Negotiate transfer block sizes while the data interface is
            // still in its idle alternate setting.
            let Some(p) = dev
                .control_in(0xA1, GET_NTB_PARAMETERS, 0, iface.number as u16, 28)
                .ok()
                .and_then(|b| NtbParams::parse(&b))
            else {
                crate::println!("[usb] {}: NCM: no NTB parameters", dev.name());
                return false;
            };
            if p.in_max > NCM_IN_MAX {
                let _ = dev.control_out(
                    0x21,
                    SET_NTB_INPUT_SIZE,
                    0,
                    iface.number as u16,
                    &NCM_IN_MAX.to_le_bytes(),
                );
            }
            proto = Proto::Ncm(p);
        }
        if data.alternate != 0
            && dev
                .control_out(
                    0x01,
                    super::REQ_SET_INTERFACE,
                    data.alternate as u16,
                    data_num as u16,
                    &[],
                )
                .is_err()
        {
            return false;
        }
        // Directed + broadcast + all-multicast.
        let _ = dev.control_out(
            0x21,
            SET_ETHERNET_PACKET_FILTER,
            0x0E,
            iface.number as u16,
            &[],
        );
        dev.string(mac_idx)
            .and_then(|s| parse_mac(&s))
            .unwrap_or_else(random_mac)
    };
    start(dev, mac, ep_in, ep_out, proto)
}

// ---------------------------------------------------------------------------
// RNDIS control channel
// ---------------------------------------------------------------------------

const RNDIS_PACKET: u32 = 0x1;
const RNDIS_INIT: u32 = 0x2;
const RNDIS_QUERY: u32 = 0x4;
const RNDIS_SET: u32 = 0x5;
const OID_802_3_PERMANENT_ADDRESS: u32 = 0x0101_0101;
const OID_GEN_CURRENT_PACKET_FILTER: u32 = 0x0001_010E;
const RNDIS_HDR: usize = 44;

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

/// Send an RNDIS control message and wait for its completion.
fn rndis_cmd(dev: &UsbDevice, ifn: u8, msg: &[u32], payload: &[u8]) -> Option<Vec<u8>> {
    let mut m: Vec<u8> = msg.iter().flat_map(|w| w.to_le_bytes()).collect();
    m.extend_from_slice(payload);
    let len = m.len() as u32;
    m[4..8].copy_from_slice(&len.to_le_bytes());
    dev.control_out(0x21, 0x00, 0, ifn as u16, &m).ok()?;
    for _ in 0..50 {
        if let Ok(r) = dev.control_in(0xA1, 0x01, 0, ifn as u16, 1025)
            && r.len() >= 16
            && le32(&r, 8) == msg[2]
        {
            return (le32(&r, 12) == 0).then_some(r);
        }
        crate::time::sleep_ms(10);
    }
    None
}

fn rndis_init(dev: &UsbDevice, ifn: u8) -> Option<[u8; 6]> {
    rndis_cmd(dev, ifn, &[RNDIS_INIT, 0, 1, 1, 0, 0x4000], &[])?;
    let q = rndis_cmd(
        dev,
        ifn,
        &[RNDIS_QUERY, 0, 2, OID_802_3_PERMANENT_ADDRESS, 8, 20, 0],
        &[0; 8],
    )?;
    let (blen, boff) = (le32(&q, 16) as usize, le32(&q, 20) as usize + 8);
    if blen < 6 || q.len() < boff + 6 {
        return None;
    }
    let mut mac = [0u8; 6];
    mac.copy_from_slice(&q[boff..boff + 6]);
    // Directed | multicast | all-multicast | broadcast.
    rndis_cmd(
        dev,
        ifn,
        &[RNDIS_SET, 0, 3, OID_GEN_CURRENT_PACKET_FILTER, 4, 20, 0],
        &0x0Fu32.to_le_bytes(),
    )?;
    Some(mac)
}

/// Split an RNDIS bulk transfer into Ethernet frames.
fn rndis_unwrap(mut d: &[u8], out: &RxQueue) {
    while d.len() >= RNDIS_HDR && le32(d, 0) == RNDIS_PACKET {
        let msg_len = (le32(d, 4) as usize).min(d.len());
        let off = le32(d, 8) as usize + 8;
        let len = le32(d, 12) as usize;
        if off + len <= msg_len && len >= 14 {
            out.push(d[off..off + len].to_vec());
        }
        if msg_len == 0 {
            break;
        }
        d = &d[msg_len..];
    }
}

fn start(
    dev: &Arc<UsbDevice>,
    mac: [u8; 6],
    ep_in: usb_desc::Endpoint,
    ep_out: usb_desc::Endpoint,
    proto: Proto,
) -> bool {
    let rndis = proto == Proto::Rndis;
    let nic = Arc::new(CdcEther {
        dev: dev.clone(),
        mac,
        rxq: RxQueue::new(),
        txq: spin::Mutex::new(VecDeque::new()),
        tx_wq: WaitQueue::new(),
        link: AtomicBool::new(true),
        proto,
    });
    let name: String = net::register(nic.clone());
    let n = name.clone();
    let wake = nic.clone();
    dev.on_detach(move || {
        wake.tx_wq.wake_all();
        net::unregister(&n);
    });
    // Receive thread: one bulk IN transfer per frame.
    let rx = nic.clone();
    crate::sched::spawn(&alloc::format!("{}-rx", name), move || {
        // RNDIS and NCM devices batch several packets per transfer.
        let rx_len = match proto {
            Proto::Ecm => FRAME_MAX,
            Proto::Ncm(p) => (p.in_max.min(NCM_IN_MAX) as usize).max(2048),
            Proto::Rndis => 16384,
        };
        let Some(buf) = DmaBuffer::new(rx_len) else {
            return;
        };
        while !rx.dev.is_gone() {
            match rx.dev.transfer(&ep_in, &buf, rx_len, None) {
                Ok(n) if rndis => rndis_unwrap(&buf.as_slice()[..n], &rx.rxq),
                Ok(n) if matches!(proto, Proto::Ncm(_)) => {
                    for f in ncm::parse_ntb16(&buf.as_slice()[..n]) {
                        if f.len() >= 14 {
                            rx.rxq.push(f.to_vec());
                        }
                    }
                }
                Ok(n) if n >= 14 => rx.rxq.push(buf.as_slice()[..n].to_vec()),
                Ok(_) => {}
                Err(ENODEV) => break,
                Err(_) => crate::time::sleep_ms(10),
            }
        }
    });
    // Transmit thread.
    let tx = nic;
    crate::sched::spawn(&alloc::format!("{}-tx", name), move || {
        let Some(buf) = DmaBuffer::new(FRAME_MAX) else {
            return;
        };
        let Some(zlp) = DmaBuffer::new(8) else { return };
        let mps = ep_out.packet_size().max(1) as usize;
        let mut seq: u16 = 0;
        loop {
            tx.tx_wq
                .wait_until(|| !tx.txq.lock().is_empty() || tx.dev.is_gone());
            if tx.dev.is_gone() {
                break;
            }
            while let Some(f) = tx.txq.lock().pop_front() {
                let mut frame = f;
                if rndis {
                    let mut h = [0u8; RNDIS_HDR];
                    h[0..4].copy_from_slice(&RNDIS_PACKET.to_le_bytes());
                    h[4..8].copy_from_slice(&((RNDIS_HDR + frame.len()) as u32).to_le_bytes());
                    h[8..12].copy_from_slice(&((RNDIS_HDR - 8) as u32).to_le_bytes());
                    h[12..16].copy_from_slice(&(frame.len() as u32).to_le_bytes());
                    let mut v = h.to_vec();
                    v.extend_from_slice(&frame);
                    frame = v;
                } else if let Proto::Ncm(p) = proto {
                    frame = ncm::build_ntb16(&[&frame], seq, &p).0;
                    seq = seq.wrapping_add(1);
                }
                let n = frame.len().min(FRAME_MAX);
                unsafe {
                    core::ptr::copy_nonoverlapping(frame.as_ptr(), buf.as_ptr::<u8>(), n);
                }
                if tx.dev.transfer(&ep_out, &buf, n, Some(2000)) == Err(ENODEV) {
                    return;
                }
                if n % mps == 0 {
                    let _ = tx.dev.transfer(&ep_out, &zlp, 0, Some(2000));
                }
            }
        }
    });
    crate::println!(
        "[usb] {}: {} Ethernet as {}",
        dev.name(),
        match proto {
            Proto::Rndis => "RNDIS",
            Proto::Ncm(_) => "CDC NCM",
            Proto::Ecm => "CDC ECM",
        },
        name
    );
    true
}
