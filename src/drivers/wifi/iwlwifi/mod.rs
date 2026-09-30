//! Intel Wi-Fi 6/6E (AX210 family, "iwlwifi" MVM firmware) station driver.
//!
//! The transport (`trans`) boots the firmware and moves packets; the
//! firmware does the 802.11 MAC work (scanning, channel switching, ACKs,
//! CCMP/GCMP encryption, rate scaling). Authentication, association and
//! the WPA2/WPA3 handshakes run in the host on top of `wlan::sta`.
//!
//! One kernel thread owns the device: it services interrupts, runs the
//! station state machine and executes requests from the `wifi` tool
//! (SIOCRWIFI ioctls). Data frames go straight to the firmware TX queue
//! from the network stack.

mod fw;
mod mvm;
mod trans;

use crate::errno::*;
use crate::net::{self, IfKind, NetDevice, RxQueue};
use crate::pci::PciDevice;
use crate::sched::WaitQueue;
use crate::sync::Mutex;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use fw::Firmware;
use mvm::*;
use trans::{Packet, Trans, TxQueue};
use wlan::ba::{self, Action, Reorder, Replay, Verdict};
use wlan::caps::{self, Ac, Mode, Profile, Width};
use wlan::chan::{self, Band};
use wlan::frame::BssInfo;
use wlan::ie::{self, Cipher, Security};
use wlan::sta::{Output, State, Station};

/// ioctl numbers (SIOCRWIFI range). The argument is an ifreq whose data
/// area (offset 16) holds `WifiReq`.
pub const WIFI_STATUS: u64 = 0x89F8;
pub const WIFI_SCAN: u64 = 0x89F9;
pub const WIFI_CONNECT: u64 = 0x89FA;
pub const WIFI_DISCONNECT: u64 = 0x89FB;
pub const WIFI_RESULTS: u64 = 0x89FC;
pub const WIFI_POWER: u64 = 0x89FD;

/// Power-save policy (`wifi power on|off|auto`).
const PS_AUTO: u8 = 0;
const PS_ON: u8 = 1;
const PS_OFF: u8 = 2;

#[repr(C)]
#[derive(Clone, Copy)]
struct WifiReq {
    buf: u64,
    len: u64,
    arg: u64,
    arg_len: u64,
}

/// TX queue sizes: best effort carries almost everything.
const BE_QUEUE_SIZE: usize = 256;
const AC_QUEUE_SIZE: usize = 64;
const MGMT_QUEUE_SIZE: usize = 64;
/// Frames on a TID before a TX block-ack session is requested.
const BA_TRIGGER_FRAMES: u32 = 16;
/// The firmware aggregates only with a peer buffer of at least 64 frames.
const BA_MIN_BUF: u16 = 64;
const TX_SLOT: usize = 2560;
const SCAN_TIMEOUT_MS: u64 = 12_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    NoFirmware,
    Starting,
    Idle,
    Scanning,
    Connecting,
    Connected,
    Failed,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::NoFirmware => "no-firmware",
            Phase::Starting => "starting",
            Phase::Idle => "disconnected",
            Phase::Scanning => "scanning",
            Phase::Connecting => "connecting",
            Phase::Connected => "connected",
            Phase::Failed => "failed",
        }
    }
}

#[derive(Clone)]
struct ScanEntry {
    bss: BssInfo,
    signal: i8,
    seen: u64,
}

/// State shared with the transmit path.
struct Link {
    connected: bool,
    bssid: [u8; 6],
    qos: bool,
    band_5g: bool,
    ptk: bool,
    tx_ant: u32,
    /// TX rate_n_flags format v2 (TX_CMD version 9+).
    rate_v2: bool,
    /// Data queues per access category (index `caps::Ac`).
    txq: [Option<TxQueue>; 4],
    mgmt_q: Option<TxQueue>,
    /// Next sequence number per TID (8 = non-QoS data).
    seq: [u16; 9],
    mgmt_seq: u16,
    /// Frames sent per TID (to start aggregation on busy TIDs).
    tid_frames: [u32; 8],
    tx_errors: u64,
}

impl Link {
    fn clear_queues(&mut self) {
        self.txq = [None, None, None, None];
        self.mgmt_q = None;
    }
    fn queues(&mut self) -> impl Iterator<Item = &mut TxQueue> {
        self.txq
            .iter_mut()
            .chain(core::iter::once(&mut self.mgmt_q))
            .flatten()
    }
    /// Build the data frame for `eth` on the queue of its access category:
    /// (frame, access category index).
    fn data_frame(&mut self, eth: &[u8]) -> Option<(Vec<u8>, usize)> {
        let (tid, ac) = if self.qos {
            let ac = Ac::from_tid(wlan::frame::tid_for_ethernet(eth));
            let ac = if self.txq[ac as usize].is_some() {
                ac
            } else {
                Ac::Be
            };
            (Some(ac.tid()), ac as usize)
        } else {
            (None, Ac::Be as usize)
        };
        let si = tid.map_or(8, |t| t as usize);
        let seq = self.seq[si];
        let mut frame = wlan::frame::data_frame(eth, self.bssid, tid, seq)?;
        self.seq[si] = wlan::ba::sn_inc(seq);
        if let Some(t) = tid {
            self.tid_frames[t as usize] = self.tid_frames[t as usize].saturating_add(1);
        }
        if self.ptk {
            // The firmware inserts the CCMP/GCMP header and encrypts frames
            // marked Protected (as mac80211 marks them).
            frame[1] |= 0x40;
        }
        Some((frame, ac))
    }
}

#[derive(Default)]
struct Requests {
    scan: bool,
    connect: Option<(Vec<u8>, Vec<u8>)>,
    disconnect: bool,
    /// The power-save policy changed.
    power: bool,
}

struct Status {
    phase: Phase,
    ssid: Vec<u8>,
    bssid: [u8; 6],
    channel: u8,
    band: Band,
    signal: i8,
    security: &'static str,
    message: String,
    firmware: String,
    results: Vec<ScanEntry>,
    /// Negotiated mode ("802.11ax 80 MHz 2x2") and last TX rate.
    link_info: String,
    rate: String,
    /// Regulatory domain from the firmware.
    country: String,
    /// Power save as applied ("on", "off"), with the reason.
    power: String,
}

pub struct Iwl {
    mac: [u8; 6],
    mmio: u64,
    link: Mutex<Link>,
    rxq: RxQueue,
    req: Mutex<Requests>,
    status: Mutex<Status>,
    /// Wakes the driver thread (interrupts, requests).
    wq: WaitQueue,
    irq: AtomicBool,
    /// Wakes ioctl callers waiting for a scan.
    done_wq: WaitQueue,
    scan_epoch: AtomicU64,
    /// PS_AUTO / PS_ON / PS_OFF.
    power_mode: AtomicU8,
}

/// Check firmware command encodings against the Linux structure sizes.
pub fn self_test() {
    mvm::layout_checks();
}

pub fn matches(dev: &PciDevice) -> bool {
    dev.vendor_id == 0x8086
        && matches!(
            dev.device_id,
            0x2725 | 0x51F0 | 0x51F1 | 0x54F0 | 0x7A70 | 0x7AF0 | 0x7F70
        )
}

pub fn probe(dev: &PciDevice) {
    dev.set_power_d0();
    dev.enable();
    let Some(mmio) = dev.map_bar(0) else {
        crate::println!("[iwlwifi] cannot map BAR0");
        return;
    };
    let integrated = dev.device_id != 0x2725;
    let Some(t) = Trans::new(mmio, integrated) else {
        crate::println!("[iwlwifi] out of DMA memory");
        return;
    };
    let rf_type = (t.hw_rf_id >> 12) & 0xFFF;
    let family = if integrated {
        if rf_type == 0x10A || rf_type == 0x10C {
            "so-a0-hr-b0"
        } else {
            "so-a0-gf-a0"
        }
    } else {
        "ty-a0-gf-a0"
    };
    let mac = t.mac_address();
    crate::println!(
        "[iwlwifi] {:04x}:{:04x} hw_rev {:#x} rf_id {:#x} ({}) mac {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        dev.vendor_id,
        dev.device_id,
        t.hw_rev,
        t.hw_rf_id,
        family,
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5]
    );
    let iwl = Arc::new(Iwl {
        mac,
        mmio,
        link: Mutex::new(Link {
            connected: false,
            bssid: [0; 6],
            qos: false,
            band_5g: false,
            ptk: false,
            tx_ant: 3,
            rate_v2: true,
            txq: [None, None, None, None],
            mgmt_q: None,
            seq: [0; 9],
            mgmt_seq: 0,
            tid_frames: [0; 8],
            tx_errors: 0,
        }),
        rxq: RxQueue::new(),
        req: Mutex::new(Requests::default()),
        status: Mutex::new(Status {
            phase: Phase::Starting,
            ssid: Vec::new(),
            bssid: [0; 6],
            channel: 0,
            band: Band::B2G,
            signal: 0,
            security: "",
            message: String::new(),
            firmware: String::new(),
            results: Vec::new(),
            link_info: String::new(),
            rate: String::new(),
            country: String::new(),
            power: String::new(),
        }),
        wq: WaitQueue::new(),
        irq: AtomicBool::new(false),
        done_wq: WaitQueue::new(),
        scan_epoch: AtomicU64::new(0),
        power_mode: AtomicU8::new(match crate::params::get("iwlwifi.power").as_deref() {
            Some("on" | "1") => PS_ON,
            Some("off" | "0") => PS_OFF,
            _ => PS_AUTO,
        }),
    });
    let h = iwl.clone();
    let handler: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        // Mask until the thread has serviced the device.
        unsafe { core::ptr::write_volatile((h.mmio + trans::CSR_INT_MASK) as *mut u32, 0) };
        h.irq.store(true, Ordering::SeqCst);
        h.wq.wake_all();
    });
    let have_irq = dev.enable_msi_or_intx(handler).is_some();
    net::register(iwl.clone());
    let family = String::from(family);
    crate::sched::spawn("iwlwifi", move || {
        Driver {
            dev: iwl,
            trans: t,
            fw: None,
            family,
            have_irq,
            sta: None,
            channel: 0,
            bound: false,
            sta_added: false,
            pmf: false,
            want: None,
            scan_started: 0,
            scan_for_connect: false,
            last_tick: 0,
            fw_attempts: 0,
            err_tables: (0, 0),
            profile: Profile::AX210,
            agg: true,
            link: None,
            tid_disable: 0xFFFF,
            tx_ba: [TxBa::Off; 8],
            rx_ba: Vec::new(),
            dialog: 0,
            replay: Replay::new(0),
            group_replay: Replay::new(0),
            channels: Vec::new(),
            band: Band::B2G,
            he_ctxt_ver: 0,
            tlc_v2: false,
            baid_ver: 0,
            ps_applied: None,
            on_battery: None,
            battery_checked: 0,
            bss_timing: (100, 1),
        }
        .run()
    });
}

fn mac_str(m: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        m[0], m[1], m[2], m[3], m[4], m[5]
    )
}

fn ssid_str(s: &[u8]) -> String {
    String::from_utf8_lossy(s).into()
}

impl Iwl {
    fn set_phase(&self, p: Phase, msg: &str) {
        let mut s = self.status.lock();
        s.phase = p;
        if !msg.is_empty() {
            s.message = msg.into();
        }
    }

    fn scan_results_text(&self) -> String {
        let s = self.status.lock();
        let mut out = String::new();
        let mut v = s.results.clone();
        v.sort_by_key(|e| -(e.signal as i32));
        for e in v {
            let sec = ie::security(e.bss.capability, &e.bss.ies);
            let _ = writeln!(
                out,
                "{}\t{}\t{}\t{}\t{}",
                mac_str(&e.bss.bssid),
                chan_str(e.bss.band, e.bss.channel.unwrap_or(0)),
                e.signal,
                sec.name(),
                ssid_str(&e.bss.ssid)
            );
        }
        out
    }
}

/// Channel for display: "36", or "37/6G" on 6 GHz.
fn chan_str(band: Band, ch: u8) -> String {
    if band == Band::B6G {
        format!("{}/6G", ch)
    } else {
        format!("{}", ch)
    }
}

fn user_bytes(ptr: u64, len: u64, max: usize) -> KResult<Vec<u8>> {
    if len as usize > max {
        return Err(EINVAL);
    }
    let mut v = alloc::vec![0u8; len as usize];
    if len > 0 {
        crate::process::uaccess::copy_from_user(&mut v, ptr)?;
    }
    Ok(v)
}

fn copy_out(r: &WifiReq, s: &str) -> KResult<i64> {
    let n = s.len().min(r.len as usize);
    crate::process::uaccess::copy_to_user(r.buf, &s.as_bytes()[..n])?;
    Ok(n as i64)
}

impl NetDevice for Iwl {
    fn mac(&self) -> [u8; 6] {
        self.mac
    }
    fn mtu(&self) -> usize {
        1500
    }
    fn link_up(&self) -> bool {
        self.link.lock().connected
    }
    fn kind(&self) -> IfKind {
        IfKind::Wireless
    }
    fn driver(&self) -> &'static str {
        "iwlwifi"
    }
    fn shutdown(&self) {
        // Stop interrupts and DMA; the firmware dies with the reset.
        let w = |reg: u64, v: u32| unsafe {
            core::ptr::write_volatile((self.mmio + reg) as *mut u32, v)
        };
        w(trans::CSR_INT_MASK, 0);
        w(trans::CSR_RESET, 0x80);
    }

    fn transmit(&self, eth: &[u8]) -> KResult<()> {
        let mut l = self.link.lock();
        if !l.connected {
            return Err(ENETDOWN);
        }
        let Some((frame, ac)) = l.data_frame(eth) else {
            return Err(EINVAL);
        };
        let encrypt = l.ptk;
        let (cmd, tb1) = tx_command(&frame, hdr_len(&frame), None, encrypt, false);
        let mmio = self.mmio;
        let q = l.txq[ac].as_mut().ok_or(ENETDOWN)?;
        match Trans::tx(q, mmio, &cmd, tb1, frame.len() as u16) {
            Ok(()) => Ok(()),
            Err(e) => {
                l.tx_errors += 1;
                Err(e)
            }
        }
    }

    fn receive(&self) -> Option<Vec<u8>> {
        self.rxq.pop()
    }

    fn ioctl(&self, cmd: u64, arg: u64) -> KResult<i64> {
        let r: WifiReq = crate::process::uaccess::read_user(arg + 16)?;
        match cmd {
            WIFI_STATUS => copy_out(&r, &self.status()),
            WIFI_RESULTS => copy_out(&r, &self.scan_results_text()),
            WIFI_SCAN => {
                if self.status.lock().phase == Phase::NoFirmware {
                    self.req.lock().scan = true; // retry firmware load
                    self.wq.wake_all();
                    return Err(ENOENT);
                }
                let epoch = self.scan_epoch.load(Ordering::SeqCst);
                self.req.lock().scan = true;
                self.wq.wake_all();
                let done = self.done_wq.wait_timeout(SCAN_TIMEOUT_MS, || {
                    self.scan_epoch.load(Ordering::SeqCst) != epoch
                });
                if !done {
                    return Err(ETIMEDOUT);
                }
                copy_out(&r, &self.scan_results_text())
            }
            WIFI_CONNECT => {
                let ssid = user_bytes(r.buf, r.len, 32)?;
                let pass = user_bytes(r.arg, r.arg_len, 64)?;
                if ssid.is_empty() {
                    return Err(EINVAL);
                }
                if !pass.is_empty() && !(8..=63).contains(&pass.len()) && pass.len() != 64 {
                    return Err(EINVAL);
                }
                self.req.lock().connect = Some((ssid, pass));
                self.wq.wake_all();
                Ok(0)
            }
            WIFI_DISCONNECT => {
                self.req.lock().disconnect = true;
                self.wq.wake_all();
                Ok(0)
            }
            WIFI_POWER => {
                let mode = match user_bytes(r.buf, r.len, 8)?.as_slice() {
                    b"on" => PS_ON,
                    b"off" => PS_OFF,
                    b"auto" => PS_AUTO,
                    _ => return Err(EINVAL),
                };
                self.power_mode.store(mode, Ordering::SeqCst);
                self.req.lock().power = true;
                self.wq.wake_all();
                Ok(0)
            }
            _ => Err(ENOTTY),
        }
    }

    fn status(&self) -> String {
        let s = self.status.lock();
        let mut out = format!("state={}", s.phase.name());
        if matches!(s.phase, Phase::Connecting | Phase::Connected) {
            let _ = write!(
                out,
                " ssid=\"{}\" bssid={} channel={} signal={}dBm security={}",
                ssid_str(&s.ssid),
                mac_str(&s.bssid),
                chan_str(s.band, s.channel),
                s.signal,
                s.security
            );
        }
        if s.phase == Phase::Connected && !s.link_info.is_empty() {
            let _ = write!(out, " mode=\"{}\"", s.link_info);
            if !s.rate.is_empty() {
                let _ = write!(out, " rate=\"{}\"", s.rate);
            }
        }
        if !s.country.is_empty() {
            let _ = write!(out, " country={}", s.country);
        }
        if !s.firmware.is_empty() {
            let _ = write!(out, " firmware={}", s.firmware);
        }
        if !s.power.is_empty() {
            let _ = write!(out, " power=\"{}\"", s.power);
        }
        if !s.message.is_empty() {
            let _ = write!(out, " msg=\"{}\"", s.message);
        }
        out
    }
}

struct Driver {
    dev: Arc<Iwl>,
    trans: Trans,
    fw: Option<Firmware>,
    family: String,
    have_irq: bool,
    sta: Option<Station>,
    channel: u8,
    bound: bool,
    sta_added: bool,
    pmf: bool,
    /// Network we are trying to join (SSID, passphrase).
    want: Option<(Vec<u8>, Vec<u8>)>,
    scan_started: u64,
    scan_for_connect: bool,
    last_tick: u64,
    fw_attempts: u32,
    /// SRAM addresses of the LMAC and UMAC error tables (from ALIVE).
    err_tables: (u32, u32),
    /// What we advertise (limited by kernel.conf and firmware support).
    profile: Profile,
    /// Aggregation (block ack) enabled.
    agg: bool,
    /// Link parameters negotiated with the current AP.
    link: Option<caps::Link>,
    /// TIDs without TX aggregation (ADD_STA tid_disable_tx).
    tid_disable: u16,
    tx_ba: [TxBa; 8],
    rx_ba: Vec<RxBa>,
    dialog: u8,
    /// Replay counters: unicast per TID, group per key index.
    replay: Replay,
    group_replay: Replay,
    /// Regulatory channel flags from MCC_UPDATE (empty: unknown).
    channels: Vec<(Band, u8, u32)>,
    /// STA_HE_CTXT_CMD version (0: HE unsupported by the driver).
    he_ctxt_ver: u8,
    /// TLC notifications use rate_n_flags v2.
    tlc_v2: bool,
    /// RX_BAID_ALLOCATION_CONFIG version (0: RX block ack via ADD_STA).
    baid_ver: u8,
    /// Power save as last sent to the firmware (None: resend).
    ps_applied: Option<(bool, bool)>,
    /// ACPI power source, polled every 30 s in auto mode.
    on_battery: Option<bool>,
    battery_checked: u64,
    /// Beacon interval and DTIM period of the current BSS.
    bss_timing: (u16, u8),
    /// Band of `channel`.
    band: Band,
}

/// Block-ack session we originate (TX aggregation) on one TID.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TxBa {
    Off,
    Pending {
        token: u8,
        since: u64,
    },
    On,
    /// Refused or failed; try again after `until`.
    Refused {
        until: u64,
    },
}

/// Receive block-ack session (the AP aggregates frames to us).
struct RxBa {
    tid: u8,
    baid: u8,
    reorder: Reorder<RxFrame>,
}

/// A received data frame on its way through reordering and replay
/// checks: the 802.11 frame (decrypted, security header removed).
struct RxFrame {
    f: Vec<u8>,
    protected: bool,
    pn: Option<u64>,
    keyidx: u8,
    /// Later subframe of a hardware-split A-MSDU (same PN allowed).
    same_pn_ok: bool,
}

fn now() -> u64 {
    crate::time::millis()
}

impl Driver {
    fn run(mut self) {
        loop {
            if self.fw.is_none() {
                match self.boot() {
                    Ok(()) => {}
                    Err(e) => {
                        self.trans.stop();
                        self.fw = None;
                        self.fw_attempts += 1;
                        if e != ENOENT {
                            self.dev.set_phase(
                                Phase::Failed,
                                &format!("firmware start failed ({})", e),
                            );
                        }
                        // Firmware files may appear once storage is
                        // mounted: retry quickly at first, then on request.
                        let wait = if self.fw_attempts < 20 { 3000 } else { 60_000 };
                        let d = self.dev.clone();
                        d.wq.wait_timeout(wait, || {
                            let r = d.req.lock();
                            r.scan || r.connect.is_some()
                        });
                        continue;
                    }
                }
            }
            let d = self.dev.clone();
            let timeout = if self.have_irq { 100 } else { 10 };
            d.wq.wait_timeout(timeout, || {
                d.irq.load(Ordering::SeqCst) || {
                    let r = d.req.lock();
                    r.scan || r.connect.is_some() || r.disconnect || r.power
                }
            });
            if self.service().is_err() {
                self.restart();
            }
        }
    }

    fn restart(&mut self) {
        crate::println!("[iwlwifi] restarting firmware");
        self.drop_link("firmware restarted");
        self.trans.stop();
        self.fw = None;
        self.bound = false;
        self.sta_added = false;
        self.dev.set_phase(Phase::Starting, "");
    }

    fn cmd(&mut self, group: u8, id: u8, data: &[u8]) -> KResult<Vec<u8>> {
        self.trans.send_cmd(group, id, data, 2000)
    }

    fn boot(&mut self) -> KResult<()> {
        self.dev.set_phase(Phase::Starting, "");
        let names: Vec<String> = [72, 71, 68, 66, 63, 59]
            .iter()
            .map(|v| format!("iwlwifi-{}-{}.ucode", self.family, v))
            .collect();
        let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let (i, data) = match crate::firmware::load_any(&refs) {
            Ok(x) => x,
            Err(_) => {
                self.dev.set_phase(
                    Phase::NoFirmware,
                    &format!("install {} (and .pnvm) into /lib/firmware", names[0]),
                );
                return Err(ENOENT);
            }
        };
        let fw = Firmware::parse(&data).map_err(|e| {
            crate::println!("[iwlwifi] {}: {}", names[i], e);
            EINVAL
        })?;
        drop(data);
        crate::println!(
            "[iwlwifi] firmware {} ({} lmac, {} umac, {} paging sections)",
            fw.version,
            fw.lmac.len(),
            fw.umac.len(),
            fw.paging.len()
        );
        for (g, c, want) in [
            (LEGACY, ADD_STA, 12),
            (LEGACY, SCAN_REQ_UMAC, 15),
            (LEGACY, TX_CMD, 10),
            (LEGACY, SCD_QUEUE_CFG, 2),
            (LEGACY, PHY_CONTEXT, 3),
        ] {
            if let Some(v) = fw.cmd_version(g, c)
                && v != want
            {
                crate::println!(
                    "[iwlwifi] warning: command {:02x}.{:02x} is v{} (driver speaks v{})",
                    g,
                    c,
                    v,
                    want
                );
            }
        }
        self.dev.link.lock().rate_v2 = fw.cmd_version(LEGACY, TX_CMD).is_none_or(|v| v > 8);
        self.tlc_v2 = fw
            .notif_version(DATA_PATH, TLC_MNG_UPDATE_NOTIF)
            .is_some_and(|v| v >= 3);
        self.baid_ver = match crate::params::get("iwlwifi.baid").as_deref() {
            Some("sta" | "0") => 0,
            _ => fw
                .cmd_version(DATA_PATH, RX_BAID_ALLOCATION_CONFIG)
                .filter(|v| (1..=2).contains(v))
                .unwrap_or(0),
        };
        self.he_ctxt_ver = match fw.cmd_version(DATA_PATH, STA_HE_CTXT).unwrap_or(2) {
            v @ (2 | 3) => v,
            v => {
                crate::println!(
                    "[iwlwifi] STA_HE_CTXT v{} unsupported: 802.11ax disabled",
                    v
                );
                0
            }
        };
        self.dev.status.lock().firmware =
            format!("{} ({})", names[i].trim_end_matches(".ucode"), fw.version);
        if self.trans.rfkill() {
            crate::println!("[iwlwifi] radio is disabled by the hardware RF-kill switch");
        }

        self.trans.start_fw(&fw)?;
        let alive = self
            .trans
            .wait_notif(LEGACY, ALIVE, 1000)
            .inspect_err(|_| {
                crate::println!("[iwlwifi] no ALIVE notification");
            })?;
        let status = u16::from_le_bytes([alive[0], alive[1]]);
        if status != 0xCAFE {
            crate::println!("[iwlwifi] firmware ALIVE status {:#x}", status);
            return Err(EIO);
        }
        let le = |o: usize| {
            alive
                .get(o..o + 4)
                .map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()))
        };
        // iwl_alive_ntf_v6: status, flags, lmac_data[2] (48 bytes each,
        // error_event_table_ptr at +16), umac_data (error_info_addr at +8).
        self.err_tables = (le(20), le(108));
        let sku = [le(116), le(120), le(124)];
        if sku != [0; 3] {
            self.load_pnvm(sku)?;
        }
        self.trans.fw_alive();

        // Unified init: NVM access window, then INIT_COMPLETE.
        self.cmd(SYSTEM, INIT_EXTENDED_CFG, &2u32.to_le_bytes())?;
        self.cmd(REGULATORY_NVM, NVM_ACCESS_COMPLETE, &0u32.to_le_bytes())?;
        self.trans
            .wait_notif(LEGACY, INIT_COMPLETE, 2000)
            .inspect_err(|_| {
                crate::println!("[iwlwifi] no INIT_COMPLETE");
            })?;

        let tx_ant = fw.valid_tx_ant();
        let rx_ant = fw.valid_rx_ant();
        self.dev.link.lock().tx_ant = tx_ant;
        self.cmd(LEGACY, TX_ANT_CONFIG, &tx_ant.to_le_bytes())?;
        if fw.has_capa(37) {
            let (flags, latency) = if self.trans.integrated {
                (2u32, 12000u32)
            } else {
                (1, 0)
            };
            let _ = self.cmd(
                SYSTEM,
                SOC_CONFIGURATION,
                Cmd::new().u32(flags).u32(latency).0.as_slice(),
            );
        }
        self.cmd(
            LEGACY,
            BT_CONFIG,
            &bt_coex(!crate::params::get("iwlwifi.btcoex").is_some_and(|v| v == "0" || v == "off")),
        )?;
        self.cmd(
            LEGACY,
            PHY_CONTEXT,
            &phy_context(FW_CTXT_ACTION_ADD, Band::B2G, 1, 0, 0, rx_ant),
        )?;
        self.channel = 1;
        self.band = Band::B2G;
        let _ = self.cmd(LEGACY, POWER_TABLE, &[0, 0, 0, 0]);
        if fw.has_capa(1) {
            let mcc = Cmd::new()
                .u16(0x5A5A)
                .u8(0x10)
                .u8(0)
                .u32(0)
                .zeros(20)
                .done();
            match self.cmd(LEGACY, MCC_UPDATE, &mcc) {
                Ok(r) => match parse_mcc_response(&r) {
                    Some((mcc, chans)) => {
                        let valid: Vec<(Band, u8, u32)> = chans
                            .into_iter()
                            .filter(|(_, _, f)| f & NVM_CHANNEL_VALID != 0)
                            .collect();
                        let country = String::from_utf8_lossy(&mcc).into_owned();
                        crate::println!(
                            "[iwlwifi] regulatory domain {}: {} channels ({} passive-only)",
                            country,
                            valid.len(),
                            valid
                                .iter()
                                .filter(|(_, _, f)| f & NVM_CHANNEL_ACTIVE == 0)
                                .count()
                        );
                        let n6 = valid.iter().filter(|c| c.0 == Band::B6G).count();
                        if n6 > 0 {
                            crate::println!("[iwlwifi] 6 GHz: {} channels allowed", n6);
                        }
                        self.dev.status.lock().country = country;
                        if !valid.is_empty() {
                            self.channels = valid;
                        }
                    }
                    None => crate::println!(
                        "[iwlwifi] unrecognised MCC_UPDATE response ({} bytes)",
                        r.len()
                    ),
                },
                Err(_) => {
                    crate::println!("[iwlwifi] regulatory update failed; using firmware defaults")
                }
            }
        }
        self.cmd(LEGACY, SCAN_CFG, &scan_config(tx_ant, rx_ant))?;
        self.cmd(
            LEGACY,
            MAC_CONTEXT,
            &self.mac_cmd(FW_CTXT_ACTION_ADD, [0; 6], None, false),
        )?;
        self.fw = Some(fw);
        self.fw_attempts = 0;
        self.dev.set_phase(Phase::Idle, "");
        crate::println!("[iwlwifi] ready");
        Ok(())
    }

    fn load_pnvm(&mut self, sku: [u32; 3]) -> KResult<()> {
        let name = format!("iwlwifi-{}.pnvm", self.family);
        let Ok(data) = crate::firmware::load(&name) else {
            crate::println!(
                "[iwlwifi] warning: {} not found; the radio may not work",
                name
            );
            return Ok(());
        };
        let mac_type = ((self.trans.hw_rev & 0xFFF0) >> 4) as u16;
        let rf = ((self.trans.hw_rf_id & 0x0FF_F000) >> 12) as u16;
        let Some((ver, payload)) = fw::pnvm_payload(&data, sku, mac_type, rf) else {
            crate::println!("[iwlwifi] warning: {} has no section for this SKU", name);
            return Ok(());
        };
        self.trans.load_pnvm(&payload)?;
        self.trans
            .wait_notif(REGULATORY_NVM, PNVM_INIT_COMPLETE, 1000)
            .inspect_err(|_| {
                crate::println!("[iwlwifi] PNVM not accepted");
            })?;
        crate::println!("[iwlwifi] loaded PNVM version {:#x}", ver);
        Ok(())
    }

    fn mac_cmd(
        &self,
        action: u32,
        bssid: [u8; 6],
        assoc: Option<(u16, u16, u8)>,
        qos: bool,
    ) -> Vec<u8> {
        let (short_slot, short_pre) = self
            .sta
            .as_ref()
            .map(|s| {
                (
                    s.bss.capability & (1 << 10) != 0,
                    s.bss.capability & (1 << 5) != 0,
                )
            })
            .unwrap_or((false, false));
        // HT/HE settings and the AP's EDCA parameters apply once associated.
        let link = self.link.as_ref().filter(|_| assoc.is_some());
        let edca = link.map_or(caps::DEFAULT_EDCA, |l| l.edca);
        mac_context(&MacParams {
            action,
            own: self.dev.mac,
            bssid,
            assoc,
            qos,
            band_5g: self.band != Band::B2G,
            short_slot,
            short_preamble: short_pre,
            ht: link.filter(|l| l.mode >= Mode::Ht).map(|l| l.ht_protection),
            wide: link.is_some_and(|l| l.width > Width::W20),
            erp_protection: link.is_some_and(|l| l.erp_protection),
            he: link.is_some_and(|l| l.mode == Mode::He),
            edca: &edca,
        })
    }

    /// Capabilities to advertise: the AX210 profile limited by the
    /// firmware and by `iwlwifi.mode` / `iwlwifi.width` in kernel.conf.
    fn profile(&mut self) -> Profile {
        let mut p = Profile::AX210;
        if trans::rb_size() < 12288 {
            p.max_mpdu = 3895;
        }
        p.nss = (self
            .fw
            .as_ref()
            .map_or(3, |f| f.valid_tx_ant() & f.valid_rx_ant())
            & 3)
        .count_ones() as u8;
        p.nss = p.nss.max(1);
        if self.he_ctxt_ver == 0 {
            p.max_mode = Mode::Vht;
        }
        match crate::params::get("iwlwifi.mode").as_deref() {
            Some("legacy" | "abg") => p.max_mode = Mode::Legacy,
            Some("ht" | "n") => p.max_mode = p.max_mode.min(Mode::Ht),
            Some("vht" | "ac") => p.max_mode = p.max_mode.min(Mode::Vht),
            _ => {}
        }
        if let Some(w) = crate::params::get("iwlwifi.width").and_then(|w| w.parse::<u32>().ok()) {
            p.max_width = p.max_width.min(Width::from_mhz(w));
        }
        if p.max_mode == Mode::Legacy {
            p.max_width = Width::W20;
        }
        self.agg = p.max_mode >= Mode::Ht
            && !matches!(
                crate::params::get("iwlwifi.agg").as_deref(),
                Some("0" | "off" | "no")
            );
        self.profile = p;
        p
    }

    fn rx_ant(&self) -> u32 {
        self.fw.as_ref().map_or(3, |f| f.valid_rx_ant())
    }

    /// One round of work: interrupts, received packets, requests, timers.
    fn service(&mut self) -> KResult<()> {
        if self.dev.irq.swap(false, Ordering::SeqCst) || !self.have_irq {
            let ints = self.trans.ack_interrupts();
            if ints & (trans::INT_SW_ERR | trans::INT_HW_ERR) != 0 {
                crate::println!("[iwlwifi] firmware error (CSR_INT {:#x})", ints);
                self.dump_error_tables();
                return Err(EIO);
            }
            if ints & trans::INT_RF_KILL != 0 {
                let off = self.trans.rfkill();
                crate::println!(
                    "[iwlwifi] RF-kill switch {}",
                    if off { "on" } else { "off" }
                );
                if off {
                    self.drop_link("radio disabled by RF-kill switch");
                }
            }
            self.trans.unmask();
        }
        self.trans.rx();
        self.drain()?;

        let (scan, connect, disconnect, power) = {
            let mut r = self.dev.req.lock();
            (
                core::mem::take(&mut r.scan),
                r.connect.take(),
                core::mem::take(&mut r.disconnect),
                core::mem::take(&mut r.power),
            )
        };
        if power {
            self.battery_checked = 0;
            self.power_tick(now());
        }
        if disconnect {
            self.want = None;
            self.disconnect("disconnected by user");
            self.dev.set_phase(Phase::Idle, "disconnected by user");
        }
        if let Some(w) = connect {
            self.disconnect("switching networks");
            self.want = Some(w);
            self.start_scan(true)?;
        }
        if scan && self.dev.status.lock().phase != Phase::Scanning {
            self.start_scan(false)?;
        }

        let t = now();
        if t - self.last_tick >= 100 {
            self.last_tick = t;
            self.power_tick(t);
            if self.dev.status.lock().phase == Phase::Scanning
                && t - self.scan_started > SCAN_TIMEOUT_MS
            {
                crate::println!("[iwlwifi] scan timed out");
                let _ = self.cmd(
                    LEGACY,
                    SCAN_ABORT_UMAC,
                    Cmd::new().u32(0).u32(0).0.as_slice(),
                );
                self.scan_done();
            }
            if let Some(s) = self.sta.as_mut() {
                let out = s.tick(t);
                self.outputs(out)?;
            }
            self.tx_ba_tick(t);
            self.expire_results();
        }
        Ok(())
    }

    fn drain(&mut self) -> KResult<()> {
        while let Some(p) = self.trans.pending.pop_front() {
            self.packet(p)?;
        }
        Ok(())
    }

    /// Print the firmware's LMAC/UMAC error tables after an assert.
    fn dump_error_tables(&self) {
        let (lmac, umac) = self.err_tables;
        if lmac != 0 {
            let t = self.trans.read_mem(lmac, 32);
            crate::println!(
                "[iwlwifi] LMAC error {:#010x} blink2 {:#x} ilink1 {:#x} ilink2 {:#x} data {:#x} {:#x} {:#x} fw {}.{} hcmd {:#x}",
                t[1],
                t[4],
                t[5],
                t[6],
                t[7],
                t[8],
                t[9],
                t[16],
                t[17],
                t[23]
            );
            crate::println!("[iwlwifi] LMAC table {:08x?}", t);
        }
        if umac != 0 {
            let t = self.trans.read_mem(umac, 16);
            crate::println!(
                "[iwlwifi] UMAC error {:#010x} blink1 {:#x} blink2 {:#x} ilink1 {:#x} ilink2 {:#x} data {:#x} {:#x} {:#x} cmd {:#x}",
                t[1],
                t[2],
                t[3],
                t[4],
                t[5],
                t[6],
                t[7],
                t[8],
                t[13]
            );
        }
    }

    fn packet(&mut self, p: Packet) -> KResult<()> {
        if crate::params::IWL_DEBUG.load(Ordering::Relaxed) && p.cmd != RX_MPDU {
            crate::println!(
                "[iwlwifi] notif {:#04x}:{:#04x} len {} {:02x?}",
                p.group,
                p.cmd,
                p.data.len(),
                &p.data[..p.data.len().min(32)]
            );
        }
        match (p.group, p.cmd) {
            (0 | 1, RX_MPDU) => self.rx_mpdu(&p.data)?,
            (0 | 1, TX_CMD) => self.tx_status(&p),
            (0 | 1, BA_NOTIF) => self.ba_notif(&p.data),
            (0 | 1, FRAME_RELEASE) if p.data.len() >= 4 => {
                let nssn = u16::from_le_bytes([p.data[2], p.data[3]]);
                self.release_frames(p.data[0], nssn)?;
            }
            (0 | 1, BAR_FRAME_RELEASE) if p.data.len() >= 8 => {
                let info = u32::from_le_bytes(p.data[4..8].try_into().unwrap());
                self.release_frames(((info >> 24) & 0x3F) as u8, (info & 0xFFF) as u16)?;
            }
            (DATA_PATH, TLC_MNG_UPDATE_NOTIF) if p.data.len() >= 12 => {
                let flags = u32::from_le_bytes(p.data[4..8].try_into().unwrap());
                if flags & 1 != 0 {
                    let rate = u32::from_le_bytes(p.data[8..12].try_into().unwrap());
                    self.dev.status.lock().rate = describe_rate(rate, self.tlc_v2);
                }
            }
            (0 | 1, SCAN_COMPLETE_UMAC) => self.scan_done(),
            (0 | 1, MISSED_BEACONS) => {
                let missed = p
                    .data
                    .get(8..12)
                    .map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()));
                if missed >= 16 && self.sta.is_some() {
                    self.disconnect("lost the access point (missed beacons)");
                    self.reconnect_later();
                }
            }
            (0, REPLY_ERROR) => {
                crate::println!("[iwlwifi] firmware reported a command error");
            }
            _ => {}
        }
        Ok(())
    }

    fn tx_status(&mut self, p: &Packet) {
        // iwl_mvm_tx_resp: frame_count at 0, tx_queue at 36, per-frame
        // status from 40, then the scheduler SSN (next index to reclaim).
        let d = &p.data;
        if d.len() < 44 {
            return;
        }
        let count = d[0] as usize;
        let queue = u16::from_le_bytes([d[36], d[37]]);
        let status = u16::from_le_bytes([d[40], d[41]]);
        let mut l = self.dev.link.lock();
        if count == 1 && status & 0xFF != 1 && status & 0xFF != 2 {
            l.tx_errors += 1;
        }
        if count > 1 {
            return; // aggregated: reclaimed by the block-ack notification
        }
        let ssn = d
            .get(40 + 4 * count..44 + 4 * count)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()) & 0xFFFF)
            .unwrap_or((p.seq as u32 & 0xFF) + 1);
        for q in l.queues() {
            if q.id == queue {
                q.reclaim_to(ssn);
            }
        }
    }

    /// Compressed block-ack notification: reclaim the acknowledged frames
    /// (iwl_mvm_compressed_ba_notif, TFD entries from offset 32).
    fn ba_notif(&mut self, d: &[u8]) {
        if d.len() < 32 {
            return;
        }
        let n = u16::from_le_bytes([d[28], d[29]]) as usize;
        let mut l = self.dev.link.lock();
        for i in 0..n {
            let Some(e) = d.get(32 + 8 * i..40 + 8 * i) else {
                break;
            };
            let queue = u16::from_le_bytes([e[0], e[1]]);
            let index = u16::from_le_bytes([e[2], e[3]]) as u32;
            for q in l.queues() {
                if q.id == queue {
                    q.reclaim_to(index);
                }
            }
        }
    }

    fn rx_mpdu(&mut self, d: &[u8]) -> KResult<()> {
        // iwl_rx_mpdu_desc (v3, 64 bytes): mpdu_len 0, mac_flags1 2,
        // mac_flags2 3, amsdu_info 4, status 12, reorder_data 16,
        // energy 40/41, channel 42.
        const DESC: usize = 64;
        if d.len() < DESC + 24 {
            return Ok(());
        }
        let mpdu_len = u16::from_le_bytes([d[0], d[1]]) as usize;
        let flags1 = d[2];
        let flags2 = d[3];
        let amsdu_info = d[4];
        let status = u32::from_le_bytes(d[12..16].try_into().unwrap());
        let reorder = u32::from_le_bytes(d[16..20].try_into().unwrap());
        // CRC and overrun OK.
        if status & 3 != 3 {
            return Ok(());
        }
        let energy = [d[40], d[41]]
            .into_iter()
            .filter(|&e| e != 0)
            .min()
            .map_or(-100, |e| -(e as i32));
        let channel = d[42];
        // mac_phy_band: bits 6-7 are the PHY band (0 = 5, 1 = 2.4, 2 = 6 GHz).
        let band = match d[43] >> 6 {
            PHY_BAND_6 => Band::B6G,
            PHY_BAND_5 => Band::B5G,
            _ if channel <= 14 => Band::B2G,
            _ => Band::B5G,
        };
        let mut f = d[DESC..(DESC + mpdu_len).min(d.len())].to_vec();
        let mut len = f.len();
        let hdrlen = hdr_len(&f);
        let sec = status & (7 << 8);
        let protected = u16::from_le_bytes([f[0], f[1]]) & 0x4000 != 0;
        let decrypted = protected && (sec == 2 << 8 || sec == 5 << 8);
        if protected && sec != 0 && !decrypted {
            return Ok(()); // unsupported cipher
        }
        if decrypted && status & (1 << 6) == 0 {
            return Ok(()); // MIC failure
        }
        let crypt = if decrypted { 8 } else { 0 };
        if flags2 & 0x20 != 0 && len >= hdrlen + crypt + 2 {
            f.drain(hdrlen + crypt..hdrlen + crypt + 2);
            len -= 2;
        }
        let mic = (((flags1 & 0xF0) >> 4) as usize) * 2;
        if len > mic + hdrlen {
            len -= mic;
        }
        f.truncate(len);
        let (mut pn, mut keyidx) = (None, 0);
        if decrypted && f.len() >= hdrlen + 8 {
            pn = ba::ccmp_pn(&f[hdrlen..hdrlen + 8]);
            keyidx = f[hdrlen + 3] >> 6;
            f.drain(hdrlen..hdrlen + 8);
            f[1] &= !0x40;
        }
        if f.len() < hdrlen {
            return Ok(());
        }
        let fc = u16::from_le_bytes([f[0], f[1]]);
        match (fc >> 2) & 3 {
            0 => self.rx_mgmt(&f, band, channel, energy.clamp(-127, 0) as i8),
            2 => {
                // The hardware splits A-MSDUs: each subframe arrives as an
                // MPDU of its own with the A-MSDU flag still set.
                let hw_amsdu = flags2 & 0x40 != 0;
                if hw_amsdu {
                    wlan::frame::clear_amsdu(&mut f);
                }
                let sub = (fc >> 4) & 0xF;
                let reorderable = sub & 0x8 != 0 && sub & 0x4 == 0 && f[4] & 1 == 0;
                let frame = RxFrame {
                    f,
                    protected,
                    pn,
                    keyidx,
                    same_pn_ok: hw_amsdu && amsdu_info & 0x7F != 0,
                };
                let baid = ((reorder >> 24) & 0x7F) as u8;
                if reorderable
                    && baid != INVALID_BAID
                    && let Some(b) = self.rx_ba.iter_mut().find(|b| b.baid == baid)
                {
                    let sn = ((reorder >> 12) & 0xFFF) as u16;
                    let nssn = (reorder & 0xFFF) as u16;
                    let old = reorder & 0x8000_0000 != 0;
                    let last = amsdu_info & 0x80 != 0;
                    match b.reorder.rx(frame, sn, nssn, old, false, hw_amsdu, last) {
                        Verdict::Pass(fr) => self.deliver(fr)?,
                        Verdict::Held(v) => {
                            for fr in v {
                                self.deliver(fr)?;
                            }
                        }
                    }
                    return Ok(());
                }
                self.deliver(frame)
            }
            _ => Ok(()),
        }
    }

    /// Frames the reorder buffer may now release (firmware frame release
    /// and block-ack-request notifications).
    fn release_frames(&mut self, baid: u8, nssn: u16) -> KResult<()> {
        let Some(b) = self.rx_ba.iter_mut().find(|b| b.baid == baid) else {
            return Ok(());
        };
        for fr in b.reorder.release(nssn) {
            self.deliver(fr)?;
        }
        Ok(())
    }

    /// Replay check, then hand the frame to the network stack.
    fn deliver(&mut self, fr: RxFrame) -> KResult<()> {
        if let Some(pn) = fr.pn {
            let ok = if fr.f[4] & 1 != 0 {
                self.group_replay
                    .check(fr.keyidx as usize, pn, fr.same_pn_ok)
            } else {
                let tid = wlan::frame::qos_tid(&fr.f).map_or(16, |t| t as usize);
                self.replay.check(tid, pn, fr.same_pn_ok)
            };
            if !ok {
                return Ok(()); // replayed
            }
        }
        self.rx_data(&fr.f, fr.protected)
    }

    fn rx_mgmt(&mut self, f: &[u8], band: Band, channel: u8, signal: i8) -> KResult<()> {
        let sub = (f[0] >> 4) & 0xF;
        if sub == wlan::frame::ST_ACTION && f.len() > 24 {
            let from_ap = self
                .sta
                .as_ref()
                .is_some_and(|s| f[10..16] == s.bss.bssid && f[4..10] == self.dev.mac);
            if from_ap && let Some(a) = Action::parse(&f[24..]) {
                return self.rx_block_ack(a);
            }
        }
        if sub == 8 || sub == 5 {
            // Beacon / probe response.
            if let Some(mut bss) = wlan::frame::parse_beacon(f) {
                if channel != 0 && channel <= 233 {
                    bss.channel = Some(channel);
                    bss.band = band;
                }
                let mut st = self.dev.status.lock();
                if st.bssid == bss.bssid && st.phase == Phase::Connected {
                    st.signal = signal;
                }
                let t = now();
                let full = st.results.len() >= 128;
                match st.results.iter_mut().find(|e| e.bss.bssid == bss.bssid) {
                    Some(e) => {
                        if !bss.ssid.is_empty() || e.bss.ssid.is_empty() {
                            e.bss = bss;
                        }
                        e.signal = signal;
                        e.seen = t;
                    }
                    None if !full => st.results.push(ScanEntry {
                        bss,
                        signal,
                        seen: t,
                    }),
                    None => {}
                }
            }
            return Ok(());
        }
        if let Some(s) = self.sta.as_mut() {
            let mut rng = |b: &mut [u8]| crate::drivers::random::fill(b);
            let out = s.rx_mgmt(f, now(), &mut rng);
            self.outputs(out)?;
        }
        Ok(())
    }

    fn rx_data(&mut self, f: &[u8], was_protected: bool) -> KResult<()> {
        let Some(sta) = self.sta.as_ref() else {
            return Ok(());
        };
        let fc = u16::from_le_bytes([f[0], f[1]]);
        let addr2: [u8; 6] = f[10..16].try_into().unwrap();
        if fc & 0x0300 != 0x0200 || addr2 != sta.bss.bssid {
            return Ok(()); // only FromDS frames from our AP
        }
        let secured = sta.security == Security::Open || was_protected;
        let frames = if wlan::frame::is_amsdu(f) {
            wlan::frame::amsdu_to_ethernet(f)
        } else {
            wlan::frame::ethernet_from_data(f).into_iter().collect()
        };
        for eth in frames {
            if eth.len() >= 14 && eth[12..14] == [0x88, 0x8E] {
                if let Some(s) = self.sta.as_mut() {
                    let out = s.rx_eapol(&eth[14..], now());
                    self.outputs(out)?;
                }
                continue;
            }
            if self.dev.link.lock().connected && secured {
                self.dev.rxq.push(eth);
                net::kick();
            }
        }
        Ok(())
    }

    /// Block Ack action frames from the AP.
    fn rx_block_ack(&mut self, a: Action) -> KResult<()> {
        match a {
            Action::AddbaRequest {
                token,
                params,
                timeout,
                ssn,
            } => {
                let tid = params.tid & 7;
                self.stop_rx_ba(tid)?;
                let he = self.link.as_ref().is_some_and(|l| l.mode == Mode::He);
                let max = if he { 256 } else { 64 };
                let win = if params.buf_size == 0 {
                    max
                } else {
                    params.buf_size.min(max)
                };
                let usable = self.agg
                    && params.immediate
                    && self.link.as_ref().is_some_and(|l| l.mode >= Mode::Ht);
                let mut status = ba::STATUS_DECLINED;
                if usable {
                    let (got, st) = self.alloc_rx_ba(tid, ssn, win)?;
                    match got {
                        Some(baid) => {
                            self.rx_ba.push(RxBa {
                                tid,
                                baid,
                                reorder: Reorder::new(ssn, win),
                            });
                            status = ba::STATUS_SUCCESS;
                        }
                        None => {
                            crate::println!(
                                "[iwlwifi] RX block ack for TID {} refused ({:#x})",
                                tid,
                                st
                            );
                            if self.baid_ver == 0 && st & 0xFF == 1 {
                                let _ = self.cmd(LEGACY, ADD_STA, &add_sta_rx_ba(false, tid, 0, 0));
                            }
                        }
                    }
                }
                self.send_action(Action::AddbaResponse {
                    token,
                    status,
                    params: ba::Params {
                        amsdu: params.amsdu,
                        immediate: true,
                        tid,
                        buf_size: if status == ba::STATUS_SUCCESS {
                            win
                        } else {
                            params.buf_size
                        },
                    },
                    timeout,
                });
            }
            Action::AddbaResponse {
                token,
                status,
                params,
                ..
            } => {
                let tid = (params.tid & 7) as usize;
                let TxBa::Pending { token: t, .. } = self.tx_ba[tid] else {
                    return Ok(());
                };
                if t != token {
                    return Ok(());
                }
                if status != ba::STATUS_SUCCESS || params.buf_size < BA_MIN_BUF {
                    if status == ba::STATUS_SUCCESS {
                        // A window below 64 frames: the firmware cannot use it.
                        self.send_action(Action::Delba {
                            tid: tid as u8,
                            initiator: true,
                            reason: ba::REASON_END_BA,
                        });
                    }
                    self.tx_ba[tid] = TxBa::Refused {
                        until: now() + 60_000,
                    };
                    return Ok(());
                }
                self.tid_disable &= !(1 << tid);
                self.cmd(LEGACY, ADD_STA, &add_sta_tid_disable(self.tid_disable))?;
                self.tx_ba[tid] = TxBa::On;
                crate::println!(
                    "[iwlwifi] TX aggregation on TID {} (window {})",
                    tid,
                    params.buf_size
                );
            }
            Action::Delba { tid, initiator, .. } => {
                if initiator {
                    self.stop_rx_ba(tid & 7)?;
                } else {
                    let tid = (tid & 7) as usize;
                    if self.tx_ba[tid] == TxBa::On {
                        self.tid_disable |= 1 << tid;
                        self.cmd(LEGACY, ADD_STA, &add_sta_tid_disable(self.tid_disable))?;
                    }
                    self.tx_ba[tid] = TxBa::Refused {
                        until: now() + 60_000,
                    };
                }
            }
        }
        Ok(())
    }

    /// End the receive block-ack session on `tid` (delivering what the
    /// reorder buffer still holds).
    fn stop_rx_ba(&mut self, tid: u8) -> KResult<()> {
        let Some(i) = self.rx_ba.iter().position(|b| b.tid == tid) else {
            return Ok(());
        };
        let mut b = self.rx_ba.remove(i);
        for fr in b.reorder.flush() {
            self.deliver(fr)?;
        }
        if self.sta_added {
            if self.baid_ver > 0 {
                let c = rx_baid_remove(self.baid_ver, b.baid, tid);
                let _ = self.cmd(DATA_PATH, RX_BAID_ALLOCATION_CONFIG, &c);
            } else {
                let _ = self.cmd(LEGACY, ADD_STA, &add_sta_rx_ba(false, tid, 0, 0));
            }
        }
        Ok(())
    }

    /// Ask the firmware for a receive BA session: RX_BAID_ALLOCATION_CONFIG
    /// when the firmware has it, ADD_STA otherwise (and as a fallback if
    /// the new command is rejected). Returns the BAID and the raw status.
    fn alloc_rx_ba(&mut self, tid: u8, ssn: u16, win: u16) -> KResult<(Option<u8>, u32)> {
        if self.baid_ver > 0 {
            match self.cmd(
                DATA_PATH,
                RX_BAID_ALLOCATION_CONFIG,
                &rx_baid_alloc(tid, ssn, win),
            ) {
                Ok(r) => {
                    if let Some(b) = rx_baid_resp(&r) {
                        return Ok((Some(b), 0));
                    }
                    crate::println!("[iwlwifi] BAID allocation refused: {:02x?}", r);
                    return Ok((None, 0));
                }
                Err(e) => {
                    crate::println!(
                        "[iwlwifi] RX_BAID_ALLOCATION_CONFIG failed ({:?}); using ADD_STA",
                        e
                    );
                    self.baid_ver = 0;
                }
            }
        }
        let r = self.cmd(LEGACY, ADD_STA, &add_sta_rx_ba(true, tid, ssn, win))?;
        let st = r
            .get(0..4)
            .map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()));
        Ok((add_sta_baid(st), st))
    }

    /// Start TX block-ack sessions on busy TIDs; expire stale requests.
    fn tx_ba_tick(&mut self, t: u64) {
        let (connected, qos, frames) = {
            let l = self.dev.link.lock();
            (l.connected, l.qos, l.tid_frames)
        };
        let ht = self.link.as_ref().is_some_and(|l| l.mode >= Mode::Ht);
        if !self.agg || !connected || !qos || !ht {
            return;
        }
        let he = self.link.as_ref().is_some_and(|l| l.mode == Mode::He);
        for ac in Ac::ALL {
            let tid = ac.tid() as usize;
            match self.tx_ba[tid] {
                TxBa::Off if frames[tid] >= BA_TRIGGER_FRAMES => {
                    self.dialog = self.dialog.wrapping_add(1).max(1);
                    let token = self.dialog;
                    let ssn = self.dev.link.lock().seq[tid];
                    self.send_action(Action::AddbaRequest {
                        token,
                        params: ba::Params {
                            amsdu: false,
                            immediate: true,
                            tid: tid as u8,
                            buf_size: if he { 256 } else { 64 },
                        },
                        timeout: 0,
                        ssn,
                    });
                    self.tx_ba[tid] = TxBa::Pending { token, since: t };
                }
                TxBa::Pending { since, .. } if t - since > 1000 => {
                    self.tx_ba[tid] = TxBa::Refused { until: t + 30_000 };
                }
                TxBa::Refused { until } if t >= until => {
                    self.dev.link.lock().tid_frames[tid] = 0;
                    self.tx_ba[tid] = TxBa::Off;
                }
                _ => {}
            }
        }
    }

    /// Forget the link parameters and block-ack sessions.
    fn reset_link_state(&mut self) {
        self.link = None;
        self.tid_disable = 0xFFFF;
        self.tx_ba = [TxBa::Off; 8];
        self.rx_ba.clear();
        self.replay = Replay::new(0);
        self.group_replay = Replay::new(0);
        let mut st = self.dev.status.lock();
        st.link_info.clear();
        st.rate.clear();
    }

    fn start_scan(&mut self, for_connect: bool) -> KResult<()> {
        if self.fw.is_none() {
            return Ok(());
        }
        let connected = self.dev.status.lock().phase == Phase::Connected;
        let mut chans: Vec<(Band, u8)> = if self.channels.is_empty() {
            SCAN_CHANNELS_24
                .iter()
                .chain(SCAN_CHANNELS_5)
                .map(|&c| (Band::of_legacy(c), c))
                .collect()
        } else {
            self.channels
                .iter()
                .filter(|c| c.0 != Band::B6G)
                .map(|&(b, c, _)| (b, c))
                .collect()
        };
        // 6 GHz: the preferred scanning channels plus channels that
        // 2.4/5 GHz APs reported in Reduced Neighbor Reports, probing for
        // the reported BSSIDs and short SSIDs.
        let p = self.profile();
        let mut bssids6: Vec<(u8, [u8; 6])> = Vec::new();
        let mut shorts6: Vec<(u8, u32)> = Vec::new();
        let six_ok = p.max_mode >= Mode::He
            && self.channels.iter().any(|c| c.0 == Band::B6G)
            && !matches!(
                crate::params::get("iwlwifi.6ghz").as_deref(),
                Some("0" | "off")
            );
        if six_ok {
            let reported: Vec<chan::Neighbor> = {
                let st = self.dev.status.lock();
                st.results
                    .iter()
                    .flat_map(|e| chan::neighbors(&e.bss.ies))
                    .collect()
            };
            let allowed = |c: u8| {
                self.channels
                    .iter()
                    .any(|&(b, ch, _)| b == Band::B6G && ch == c)
            };
            for n in reported.iter().filter(|n| n.band == Band::B6G) {
                if let Some(b) = n.bssid
                    && bssids6.len() < 16
                    && !bssids6.iter().any(|x| x.1 == b)
                {
                    bssids6.push((n.channel, b));
                }
                if let Some(s) = n.short_ssid
                    && shorts6.len() < 8
                    && !shorts6.contains(&(n.channel, s))
                {
                    shorts6.push((n.channel, s));
                }
            }
            for c in chan::scan_list_6g(&reported) {
                if allowed(c) && chans.len() < 67 {
                    chans.push((Band::B6G, c));
                }
            }
        }
        let ssid = if for_connect {
            self.want.as_ref().map(|w| w.0.clone()).unwrap_or_default()
        } else {
            Vec::new()
        };
        let caps6 = caps::probe_elements_6g(&p);
        let req = scan_request(
            self.dev.mac,
            &chans,
            &ssid,
            connected,
            &caps::probe_elements(&p, true),
            &caps::probe_elements(&p, false),
            &Scan6g {
                bssids: &bssids6,
                short_ssids: &shorts6,
                caps: if six_ok { &caps6 } else { &[] },
            },
        );
        self.cmd(LEGACY, SCAN_REQ_UMAC, &req)?;
        self.scan_started = now();
        self.scan_for_connect = for_connect;
        if !connected {
            self.dev.set_phase(Phase::Scanning, "");
        }
        Ok(())
    }

    fn scan_done(&mut self) {
        let phase = self.dev.status.lock().phase;
        if phase == Phase::Scanning {
            self.dev.set_phase(Phase::Idle, "");
        }
        self.dev.scan_epoch.fetch_add(1, Ordering::SeqCst);
        self.dev.done_wq.wake_all();
        if self.scan_for_connect {
            self.scan_for_connect = false;
            if let Err(e) = self.join() {
                crate::println!("[iwlwifi] connect failed: {}", e);
            }
        }
    }

    fn expire_results(&mut self) {
        let t = now();
        self.dev
            .status
            .lock()
            .results
            .retain(|e| t - e.seen < 120_000);
    }

    fn reconnect_later(&mut self) {
        if self.want.is_some() {
            let _ = self.start_scan(true);
        }
    }

    /// Pick the best BSS for the wanted SSID and start joining it.
    fn join(&mut self) -> KResult<()> {
        let Some((ssid, pass)) = self.want.clone() else {
            return Ok(());
        };
        let best = {
            let st = self.dev.status.lock();
            st.results
                .iter()
                .filter(|e| e.bss.ssid == ssid && e.bss.channel.is_some())
                .max_by_key(|e| {
                    // Prefer the wider, less crowded bands a little.
                    e.signal as i32
                        + match e.bss.band {
                            Band::B2G => 0,
                            Band::B5G => 8,
                            Band::B6G => 12,
                        }
                })
                .cloned()
        };
        let Some(best) = best else {
            self.dev.set_phase(
                Phase::Failed,
                &format!("network \"{}\" not found", ssid_str(&ssid)),
            );
            return Ok(());
        };
        let bss = best.bss.clone();
        let channel = bss.channel.unwrap_or(1);
        let band = bss.band;
        if !self.channels.is_empty()
            && !self
                .channels
                .iter()
                .any(|&(b, c, _)| b == band && c == channel)
        {
            self.dev.set_phase(
                Phase::Failed,
                &format!(
                    "channel {} is not allowed in this regulatory domain",
                    chan_str(band, channel)
                ),
            );
            return Ok(());
        }
        let security = ie::security(bss.capability, &bss.ies);
        if matches!(security, Security::Wep | Security::Enterprise) {
            self.dev.set_phase(
                Phase::Failed,
                &format!("{} networks are not supported", security.name()),
            );
            return Ok(());
        }
        crate::println!(
            "[iwlwifi] joining \"{}\" {} channel {} ({}, {} dBm)",
            ssid_str(&ssid),
            mac_str(&bss.bssid),
            chan_str(band, channel),
            security.name(),
            best.signal
        );
        {
            let mut st = self.dev.status.lock();
            st.phase = Phase::Connecting;
            st.ssid = ssid.clone();
            st.bssid = bss.bssid;
            st.channel = channel;
            st.band = band;
            st.signal = best.signal;
            st.security = security.name();
            st.message.clear();
        }
        let mut rng = |b: &mut [u8]| crate::drivers::random::fill(b);
        let (sta, out) = match Station::connect(self.dev.mac, bss.clone(), &pass, now(), &mut rng) {
            Ok(x) => x,
            Err(e) => {
                self.dev.set_phase(Phase::Failed, e);
                return Ok(());
            }
        };
        let mut sta = sta;
        self.pmf = sta.pmf();
        sta.profile = self.profile();
        let link = caps::negotiate_band(&sta.profile, &bss.ies, band, channel);

        // Tune to the channel (at the AP's width), bind the MAC to it, add
        // the AP station.
        self.channel = channel;
        self.band = band;
        let rx_ant = self.rx_ant();
        self.cmd(
            LEGACY,
            PHY_CONTEXT,
            &phy_context(
                FW_CTXT_ACTION_MODIFY,
                band,
                channel,
                link.width.code(),
                caps::ctrl_pos(&link),
                rx_ant,
            ),
        )?;
        if !self.bound {
            self.cmd(LEGACY, BINDING, &binding(FW_CTXT_ACTION_ADD))?;
            self.bound = true;
        }
        self.sta = Some(sta);
        self.link = Some(link);
        let bssid = bss.bssid;
        self.cmd(
            LEGACY,
            MAC_CONTEXT,
            &self.mac_cmd(FW_CTXT_ACTION_MODIFY, bssid, None, false),
        )?;
        self.add_station(bssid)?;
        {
            let mut l = self.dev.link.lock();
            l.bssid = bssid;
            l.band_5g = band != Band::B2G;
            l.ptk = false;
            l.qos = false;
            l.seq = [0; 9];
            l.tid_frames = [0; 8];
        }
        let _ = self.cmd(
            MAC_CONF,
            SESSION_PROTECTION,
            &session_protection(FW_CTXT_ACTION_ADD, 900),
        );
        // Give the firmware a moment to get on channel.
        let _ = self
            .trans
            .wait_notif(MAC_CONF, SESSION_PROTECTION_NOTIF, 300);
        self.outputs(out)
    }

    fn add_station(&mut self, bssid: [u8; 6]) -> KResult<()> {
        self.tid_disable = 0xFFFF;
        let legacy = sta_flags(&caps::negotiate(&Profile::LEGACY, &[], self.channel), 1);
        let r = self.cmd(
            LEGACY,
            ADD_STA,
            &add_sta(false, bssid, 0, legacy, self.tid_disable, false),
        )?;
        if r.first().is_some_and(|&s| s != 1) {
            crate::println!("[iwlwifi] ADD_STA failed ({:#x})", r[0]);
            return Err(EIO);
        }
        self.sta_added = true;
        let mut queues = [None, None, None, None];
        queues[Ac::Be as usize] = Some(self.alloc_queue(Ac::Be.tid(), BE_QUEUE_SIZE)?);
        for ac in [Ac::Bk, Ac::Vi, Ac::Vo] {
            // Other categories fall back to best effort without a queue.
            match self.alloc_queue(ac.tid(), AC_QUEUE_SIZE) {
                Ok(q) => queues[ac as usize] = Some(q),
                Err(e) => crate::println!("[iwlwifi] no TX queue for {:?}: {}", ac, e),
            }
        }
        let mgmt = self.alloc_queue(MGMT_TID, MGMT_QUEUE_SIZE)?;
        let mut l = self.dev.link.lock();
        l.txq = queues;
        l.mgmt_q = Some(mgmt);
        Ok(())
    }

    fn alloc_queue(&mut self, tid: u8, size: usize) -> KResult<TxQueue> {
        let mut q = TxQueue::new(size, TX_SLOT).ok_or(ENOMEM)?;
        let r = self.cmd(
            LEGACY,
            SCD_QUEUE_CFG,
            &tx_queue_cfg(AP_STA_ID, tid, q.cb_size(), q.bc_phys(), q.tfd_phys()),
        )?;
        if r.len() < 6 {
            return Err(EIO);
        }
        q.id = u16::from_le_bytes([r[0], r[1]]);
        q.set_start(u16::from_le_bytes([r[4], r[5]]) as u32);
        Ok(q)
    }

    /// Send a management frame through the AP station's management queue.
    /// `protect`: encrypt it (robust action frames with PMF).
    fn send_mgmt(&mut self, f: &[u8], protect: bool) -> KResult<()> {
        let mut l = self.dev.link.lock();
        let rate = basic_rate(l.band_5g, l.tx_ant, l.rate_v2);
        let mut f = f.to_vec();
        wlan::frame::set_seq(&mut f, l.mgmt_seq);
        l.mgmt_seq = ba::sn_inc(l.mgmt_seq);
        let encrypt = protect && l.ptk;
        if encrypt {
            f[1] |= 0x40;
        }
        let (cmd, tb1) = tx_command(&f, 24, Some(rate), encrypt, true);
        let q = l.mgmt_q.as_mut().ok_or(ENETDOWN)?;
        Trans::tx(q, self.dev.mmio, &cmd, tb1, f.len() as u16)
    }

    /// Send a Block Ack action frame to the AP.
    fn send_action(&mut self, a: Action) {
        let bssid = self.dev.link.lock().bssid;
        let f = a.frame(self.dev.mac, bssid);
        if let Err(e) = self.send_mgmt(&f, self.pmf) {
            crate::println!("[iwlwifi] action frame TX failed: {}", e);
        }
    }

    fn send_eapol(&mut self, body: &[u8]) -> KResult<()> {
        let mut l = self.dev.link.lock();
        let mut eth = Vec::with_capacity(14 + body.len());
        eth.extend_from_slice(&l.bssid);
        eth.extend_from_slice(&self.dev.mac);
        eth.extend_from_slice(&[0x88, 0x8E]);
        eth.extend_from_slice(body);
        let (frame, ac) = l.data_frame(&eth).ok_or(EINVAL)?;
        let rate = basic_rate(l.band_5g, l.tx_ant, l.rate_v2);
        let encrypt = l.ptk;
        let (cmd, tb1) = tx_command(&frame, hdr_len(&frame), Some(rate), encrypt, true);
        let q = l.txq[ac].as_mut().ok_or(ENETDOWN)?;
        Trans::tx(q, self.dev.mmio, &cmd, tb1, frame.len() as u16)
    }

    fn outputs(&mut self, out: Vec<Output>) -> KResult<()> {
        for o in out {
            match o {
                Output::TxMgmt(f) => {
                    if let Err(e) = self.send_mgmt(&f, false) {
                        crate::println!("[iwlwifi] management TX failed: {}", e);
                    }
                }
                Output::TxEapol(b) => {
                    if let Err(e) = self.send_eapol(&b) {
                        crate::println!("[iwlwifi] EAPOL TX failed: {}", e);
                    }
                }
                Output::Associated { aid, qos, ies } => self.associated(aid, qos, &ies)?,
                Output::InstallPairwise { cipher, key } => {
                    let gcmp = matches!(cipher, Cipher::Gcmp128 | Cipher::Gcmp256);
                    self.cmd(
                        LEGACY,
                        ADD_STA_KEY,
                        &add_sta_key(&key, 0, 0, gcmp, false, self.pmf, [0; 6]),
                    )?;
                    self.dev.link.lock().ptk = true;
                    self.replay = Replay::new(0);
                }
                Output::InstallGroup {
                    idx,
                    cipher,
                    key,
                    rsc,
                } => {
                    let mut pn = [0u8; 8];
                    pn[..6].copy_from_slice(&rsc[..6]);
                    let start = u64::from_le_bytes(pn);
                    // Accept the counter itself (RSC is the last one used).
                    let _ = self.group_replay.check(idx as usize, start, false);
                    let gcmp = matches!(cipher, Cipher::Gcmp128 | Cipher::Gcmp256);
                    let offset = idx.clamp(1, 3);
                    self.cmd(
                        LEGACY,
                        ADD_STA_KEY,
                        &add_sta_key(&key, idx, offset, gcmp, true, self.pmf, [0; 6]),
                    )?;
                }
                Output::InstallIgtk { idx, key, ipn } => {
                    if let Err(e) = self.cmd(LEGACY, MGMT_MCAST_KEY, &igtk(&key, idx, ipn)) {
                        crate::println!("[iwlwifi] IGTK install failed: {}", e);
                    }
                }
                Output::Connected => {
                    let _ = self.cmd(
                        MAC_CONF,
                        SESSION_PROTECTION,
                        &session_protection(FW_CTXT_ACTION_REMOVE, 0),
                    );
                    self.dev.link.lock().connected = true;
                    self.dev.set_phase(Phase::Connected, "");
                    if let Some(l) = &self.link {
                        self.dev.status.lock().link_info =
                            caps::describe(l, self.band == Band::B2G);
                    }
                    let st = self.dev.status.lock();
                    crate::println!(
                        "[iwlwifi] connected to \"{}\" ({})",
                        ssid_str(&st.ssid),
                        mac_str(&st.bssid)
                    );
                    drop(st);
                    net::kick();
                }
                Output::Disconnected(why) => {
                    crate::println!("[iwlwifi] {}", why);
                    let was_connected = self.dev.link.lock().connected;
                    self.teardown();
                    self.dev.set_phase(Phase::Failed, why);
                    if was_connected {
                        self.reconnect_later();
                    }
                }
            }
        }
        Ok(())
    }

    fn associated(&mut self, aid: u16, qos: bool, resp_ies: &[u8]) -> KResult<()> {
        let (bssid, bi, beacon_ies) = {
            let s = self.sta.as_ref().ok_or(EIO)?;
            (s.bss.bssid, s.bss.beacon_interval, s.bss.ies.clone())
        };
        let dtim = ie::find(&beacon_ies, 5)
            .and_then(|t| t.get(1).copied())
            .unwrap_or(1);
        self.bss_timing = (bi, dtim);
        // The association response carries the AP's capabilities for us;
        // the beacon fills in what it leaves out (e.g. WMM parameters).
        let mut ies = resp_ies.to_vec();
        ies.extend_from_slice(&beacon_ies);
        let link = caps::negotiate_band(&self.profile, &ies, self.band, self.channel);
        let qos = qos || link.qos;
        crate::println!(
            "[iwlwifi] associated (aid {}): {}{}",
            aid,
            caps::describe(&link, self.band == Band::B2G),
            if link.mode >= Mode::Ht {
                format!(", A-MPDU up to {} KiB", 8u32 << link.ampdu_exp)
            } else {
                String::new()
            }
        );
        let rx_ant = self.rx_ant();
        if self
            .link
            .as_ref()
            .is_none_or(|l| l.width != link.width || l.center != link.center)
        {
            self.cmd(
                LEGACY,
                PHY_CONTEXT,
                &phy_context(
                    FW_CTXT_ACTION_MODIFY,
                    self.band,
                    self.channel,
                    link.width.code(),
                    caps::ctrl_pos(&link),
                    rx_ant,
                ),
            )?;
        }
        self.link = Some(link.clone());
        self.cmd(
            LEGACY,
            MAC_CONTEXT,
            &self.mac_cmd(FW_CTXT_ACTION_MODIFY, bssid, Some((aid, bi, dtim)), qos),
        )?;
        let chains = (self.fw.as_ref().map_or(3, |f| f.valid_tx_ant()) & 3) as u8;
        let flags = sta_flags(&link, chains.count_ones() as u8);
        self.cmd(
            LEGACY,
            ADD_STA,
            &add_sta(true, bssid, aid, flags, self.tid_disable, false),
        )?;
        if let Some(he) = link.he.as_ref().filter(|_| self.he_ctxt_ver != 0)
            && let Err(e) = self.cmd(
                DATA_PATH,
                STA_HE_CTXT,
                &he_sta_context(self.he_ctxt_ver, he),
            )
        {
            crate::println!("[iwlwifi] HE station context failed: {}", e);
        }
        self.cmd(
            DATA_PATH,
            TLC_MNG_CONFIG,
            &tlc_config(&link, self.band != Band::B2G, chains),
        )?;
        self.dev.link.lock().qos = qos;
        Ok(())
    }

    /// Leave the current network (sending deauthentication if associated).
    fn disconnect(&mut self, why: &'static str) {
        if let Some(mut s) = self.sta.take() {
            if matches!(
                s.state,
                State::Connected | State::Handshake | State::Associating
            ) {
                for o in s.disconnect() {
                    if let Output::TxMgmt(f) = o {
                        let _ = self.send_mgmt(&f, self.pmf);
                        crate::time::sleep_ms(20);
                    }
                }
            }
            crate::println!("[iwlwifi] {}", why);
        }
        self.teardown();
    }

    fn drop_link(&mut self, why: &'static str) {
        self.sta = None;
        self.reset_link_state();
        let mut l = self.dev.link.lock();
        let was = l.connected;
        l.connected = false;
        l.ptk = false;
        l.clear_queues();
        drop(l);
        if was {
            crate::println!("[iwlwifi] link down: {}", why);
            net::kick();
        }
    }

    /// Remove the AP station, its queues and the binding.
    /// Apply the power-save policy: device power save follows the policy;
    /// MAC power save and beacon filtering only while associated. The
    /// firmware leaves power save by itself on traffic (100 ms timeouts).
    fn power_tick(&mut self, t: u64) {
        if self.fw.is_none() {
            return;
        }
        let mode = self.dev.power_mode.load(Ordering::Relaxed);
        if mode == PS_AUTO && (self.battery_checked == 0 || t - self.battery_checked > 30_000) {
            self.battery_checked = t.max(1);
            self.on_battery = crate::acpi::on_battery();
        }
        let want = match mode {
            PS_ON => true,
            PS_OFF => false,
            _ => self.on_battery == Some(true),
        };
        let assoc = self.dev.link.lock().connected;
        if self.ps_applied == Some((want, assoc)) {
            return;
        }
        let _ = self.cmd(LEGACY, POWER_TABLE, &device_power(want));
        if assoc {
            let (bi, dtim) = self.bss_timing;
            if let Err(e) = self.cmd(LEGACY, MAC_PM_POWER_TABLE, &mac_power(want, bi, dtim)) {
                crate::println!("[iwlwifi] MAC power table failed: {}", e);
            }
            let ver = self
                .fw
                .as_ref()
                .and_then(|f| f.cmd_version(LEGACY, BEACON_FILTER_CONFIG))
                .unwrap_or(3);
            let _ = self.cmd(LEGACY, BEACON_FILTER_CONFIG, &beacon_filter(ver, want));
        }
        self.ps_applied = Some((want, assoc));
        let why = match mode {
            PS_ON => "set by user",
            PS_OFF => "set by user",
            _ => match self.on_battery {
                Some(true) => "auto: on battery",
                Some(false) => "auto: on AC power",
                None => "auto: no battery",
            },
        };
        self.dev.status.lock().power = format!("{} ({})", if want { "on" } else { "off" }, why);
    }

    fn teardown(&mut self) {
        self.ps_applied = None;
        self.sta = None;
        self.reset_link_state();
        {
            let mut l = self.dev.link.lock();
            l.connected = false;
            l.ptk = false;
        }
        net::kick();
        if self.fw.is_none() {
            return;
        }
        if self.sta_added {
            let _ = self.cmd(LEGACY, REMOVE_STA, &remove_sta(AP_STA_ID));
            self.sta_added = false;
        }
        self.dev.link.lock().clear_queues();
        let _ = self.cmd(
            LEGACY,
            MAC_CONTEXT,
            &self.mac_cmd(FW_CTXT_ACTION_MODIFY, [0; 6], None, false),
        );
        if self.bound {
            let _ = self.cmd(LEGACY, BINDING, &binding(FW_CTXT_ACTION_REMOVE));
            self.bound = false;
        }
    }
}
