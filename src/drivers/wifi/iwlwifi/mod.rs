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
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use fw::Firmware;
use mvm::*;
use spin::Mutex;
use trans::{Packet, Trans, TxQueue};
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

#[repr(C)]
#[derive(Clone, Copy)]
struct WifiReq {
    buf: u64,
    len: u64,
    arg: u64,
    arg_len: u64,
}

const DATA_QUEUE_SIZE: usize = 256;
const MGMT_QUEUE_SIZE: usize = 64;
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
    data_q: Option<TxQueue>,
    mgmt_q: Option<TxQueue>,
    tx_errors: u64,
}

#[derive(Default)]
struct Requests {
    scan: bool,
    connect: Option<(Vec<u8>, Vec<u8>)>,
    disconnect: bool,
}

struct Status {
    phase: Phase,
    ssid: Vec<u8>,
    bssid: [u8; 6],
    channel: u8,
    signal: i8,
    security: &'static str,
    message: String,
    firmware: String,
    results: Vec<ScanEntry>,
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
            data_q: None,
            mgmt_q: None,
            tx_errors: 0,
        }),
        rxq: RxQueue::new(),
        req: Mutex::new(Requests::default()),
        status: Mutex::new(Status {
            phase: Phase::Starting,
            ssid: Vec::new(),
            bssid: [0; 6],
            channel: 0,
            signal: 0,
            security: "",
            message: String::new(),
            firmware: String::new(),
            results: Vec::new(),
        }),
        wq: WaitQueue::new(),
        irq: AtomicBool::new(false),
        done_wq: WaitQueue::new(),
        scan_epoch: AtomicU64::new(0),
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
                e.bss.channel.unwrap_or(0),
                e.signal,
                sec.name(),
                ssid_str(&e.bss.ssid)
            );
        }
        out
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
        let Some(frame) = wlan::frame::data_from_ethernet(eth, l.bssid, l.qos) else {
            return Err(EINVAL);
        };
        let hdrlen = hdr_len(&frame);
        let encrypt = l.ptk;
        let (cmd, tb1) = tx_command(&frame, hdrlen, None, encrypt, false);
        let mmio = self.mmio;
        let q = l.data_q.as_mut().ok_or(ENETDOWN)?;
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
                s.channel,
                s.signal,
                s.security
            );
        }
        if !s.firmware.is_empty() {
            let _ = write!(out, " firmware={}", s.firmware);
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
                    r.scan || r.connect.is_some() || r.disconnect
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
            Cmd::new().u32(1).u32((1 << 2) | (1 << 4)).0.as_slice(),
        )?;
        self.cmd(
            LEGACY,
            PHY_CONTEXT,
            &phy_context(FW_CTXT_ACTION_ADD, 1, rx_ant),
        )?;
        self.channel = 1;
        let _ = self.cmd(LEGACY, POWER_TABLE, &[0, 0, 0, 0]);
        if fw.has_capa(1) {
            let mcc = Cmd::new()
                .u16(0x5A5A)
                .u8(0x10)
                .u8(0)
                .u32(0)
                .zeros(20)
                .done();
            if self.cmd(LEGACY, MCC_UPDATE, &mcc).is_err() {
                crate::println!("[iwlwifi] regulatory update failed; using firmware defaults");
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
        mac_context(&MacParams {
            action,
            own: self.dev.mac,
            bssid,
            assoc,
            qos,
            band_5g: self.channel > 14,
            short_slot,
            short_preamble: short_pre,
            _p: core::marker::PhantomData,
        })
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

        let (scan, connect, disconnect) = {
            let mut r = self.dev.req.lock();
            (
                core::mem::take(&mut r.scan),
                r.connect.take(),
                core::mem::take(&mut r.disconnect),
            )
        };
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

    fn packet(&mut self, p: Packet) -> KResult<()> {
        match (p.group, p.cmd) {
            (0 | 1, RX_MPDU) => self.rx_mpdu(&p.data)?,
            (0 | 1, TX_CMD) => self.tx_status(&p),
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
        // iwl_mvm_tx_resp: frame_count at 0, tx_queue at 36, status at 40.
        let d = &p.data;
        if d.len() < 44 {
            return;
        }
        let queue = u16::from_le_bytes([d[36], d[37]]);
        let status = u16::from_le_bytes([d[40], d[41]]);
        let idx = (p.seq & 0xFF) as u32;
        let mut l = self.dev.link.lock();
        if status & 0xFF != 1 && status & 0xFF != 2 {
            l.tx_errors += 1;
        }
        let Link { data_q, mgmt_q, .. } = &mut *l;
        for q in [data_q.as_mut(), mgmt_q.as_mut()].into_iter().flatten() {
            if q.id == queue {
                q.reclaim_to(idx + 1);
            }
        }
    }

    fn rx_mpdu(&mut self, d: &[u8]) -> KResult<()> {
        const DESC: usize = 64;
        if d.len() < DESC + 24 {
            return Ok(());
        }
        let mpdu_len = u16::from_le_bytes([d[0], d[1]]) as usize;
        let flags1 = d[2];
        let flags2 = d[3];
        let status = u32::from_le_bytes(d[12..16].try_into().unwrap());
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
        if decrypted {
            f.drain(hdrlen..hdrlen + 8);
            f[1] &= !0x40;
        }
        if f.len() < hdrlen {
            return Ok(());
        }
        let fc = u16::from_le_bytes([f[0], f[1]]);
        match (fc >> 2) & 3 {
            0 => self.rx_mgmt(&f, channel, energy.clamp(-127, 0) as i8),
            2 => self.rx_data(&f, protected),
            _ => Ok(()),
        }
    }

    fn rx_mgmt(&mut self, f: &[u8], channel: u8, signal: i8) -> KResult<()> {
        let sub = (f[0] >> 4) & 0xF;
        if sub == 8 || sub == 5 {
            // Beacon / probe response.
            if let Some(mut bss) = wlan::frame::parse_beacon(f) {
                if channel != 0 && channel <= 196 {
                    bss.channel = Some(channel);
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
        let Some(eth) = wlan::frame::ethernet_from_data(f) else {
            return Ok(());
        };
        if eth.len() >= 14 && eth[12..14] == [0x88, 0x8E] {
            let out = self.sta.as_mut().unwrap().rx_eapol(&eth[14..], now());
            return self.outputs(out);
        }
        let secured = sta.security == Security::Open || was_protected;
        if self.dev.link.lock().connected && secured {
            self.dev.rxq.push(eth);
            net::kick();
        }
        Ok(())
    }

    fn start_scan(&mut self, for_connect: bool) -> KResult<()> {
        if self.fw.is_none() {
            return Ok(());
        }
        let connected = self.dev.status.lock().phase == Phase::Connected;
        let mut chans: Vec<u8> = SCAN_CHANNELS_24.to_vec();
        chans.extend_from_slice(SCAN_CHANNELS_5);
        let ssid = if for_connect {
            self.want.as_ref().map(|w| w.0.clone()).unwrap_or_default()
        } else {
            Vec::new()
        };
        let req = scan_request(self.dev.mac, &chans, &ssid, connected);
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
                .filter(|e| e.bss.ssid == ssid && e.bss.channel.is_some_and(|c| c <= 165))
                .max_by_key(|e| {
                    e.signal as i32
                        + if e.bss.channel.unwrap_or(0) > 14 {
                            8
                        } else {
                            0
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
            channel,
            security.name(),
            best.signal
        );
        {
            let mut st = self.dev.status.lock();
            st.phase = Phase::Connecting;
            st.ssid = ssid.clone();
            st.bssid = bss.bssid;
            st.channel = channel;
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
        self.pmf = sta.pmf();

        // Tune to the channel, bind the MAC to it, add the AP station.
        self.channel = channel;
        let rx_ant = self.rx_ant();
        self.cmd(
            LEGACY,
            PHY_CONTEXT,
            &phy_context(FW_CTXT_ACTION_MODIFY, channel, rx_ant),
        )?;
        if !self.bound {
            self.cmd(LEGACY, BINDING, &binding(FW_CTXT_ACTION_ADD))?;
            self.bound = true;
        }
        self.sta = Some(sta);
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
            l.band_5g = channel > 14;
            l.ptk = false;
            l.qos = false;
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
        let r = self.cmd(LEGACY, ADD_STA, &add_sta(false, bssid, 0))?;
        if r.first().is_some_and(|&s| s != 1) {
            crate::println!("[iwlwifi] ADD_STA failed ({:#x})", r[0]);
            return Err(EIO);
        }
        self.sta_added = true;
        let data = self.alloc_queue(0, DATA_QUEUE_SIZE)?;
        let mgmt = self.alloc_queue(MGMT_TID, MGMT_QUEUE_SIZE)?;
        let mut l = self.dev.link.lock();
        l.data_q = Some(data);
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
    fn send_mgmt(&mut self, f: &[u8]) -> KResult<()> {
        let mut l = self.dev.link.lock();
        let rate = basic_rate(l.band_5g, l.tx_ant);
        let (cmd, tb1) = tx_command(f, 24, Some(rate), false, true);
        let q = l.mgmt_q.as_mut().ok_or(ENETDOWN)?;
        Trans::tx(q, self.dev.mmio, &cmd, tb1, f.len() as u16)
    }

    fn send_eapol(&mut self, body: &[u8]) -> KResult<()> {
        let mut l = self.dev.link.lock();
        let mut eth = Vec::with_capacity(14 + body.len());
        eth.extend_from_slice(&l.bssid);
        eth.extend_from_slice(&self.dev.mac);
        eth.extend_from_slice(&[0x88, 0x8E]);
        eth.extend_from_slice(body);
        let frame = wlan::frame::data_from_ethernet(&eth, l.bssid, l.qos).ok_or(EINVAL)?;
        let rate = basic_rate(l.band_5g, l.tx_ant);
        let encrypt = l.ptk;
        let (cmd, tb1) = tx_command(&frame, hdr_len(&frame), Some(rate), encrypt, true);
        let q = l.data_q.as_mut().ok_or(ENETDOWN)?;
        Trans::tx(q, self.dev.mmio, &cmd, tb1, frame.len() as u16)
    }

    fn outputs(&mut self, out: Vec<Output>) -> KResult<()> {
        for o in out {
            match o {
                Output::TxMgmt(f) => {
                    if let Err(e) = self.send_mgmt(&f) {
                        crate::println!("[iwlwifi] management TX failed: {}", e);
                    }
                }
                Output::TxEapol(b) => {
                    if let Err(e) = self.send_eapol(&b) {
                        crate::println!("[iwlwifi] EAPOL TX failed: {}", e);
                    }
                }
                Output::Associated { aid, qos } => self.associated(aid, qos)?,
                Output::InstallPairwise { cipher, key } => {
                    let gcmp = matches!(cipher, Cipher::Gcmp128 | Cipher::Gcmp256);
                    self.cmd(
                        LEGACY,
                        ADD_STA_KEY,
                        &add_sta_key(&key, 0, 0, gcmp, false, self.pmf, [0; 6]),
                    )?;
                    self.dev.link.lock().ptk = true;
                }
                Output::InstallGroup {
                    idx,
                    cipher,
                    key,
                    rsc: _,
                } => {
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

    fn associated(&mut self, aid: u16, qos: bool) -> KResult<()> {
        let (bssid, bi) = {
            let s = self.sta.as_ref().ok_or(EIO)?;
            (s.bss.bssid, s.bss.beacon_interval)
        };
        let dtim = self
            .sta
            .as_ref()
            .and_then(|s| ie::find(&s.bss.ies, 5).and_then(|t| t.get(1).copied()))
            .unwrap_or(1);
        self.cmd(
            LEGACY,
            MAC_CONTEXT,
            &self.mac_cmd(FW_CTXT_ACTION_MODIFY, bssid, Some((aid, bi, dtim)), qos),
        )?;
        self.cmd(LEGACY, ADD_STA, &add_sta(true, bssid, aid))?;
        let chains = (self.fw.as_ref().map_or(3, |f| f.valid_tx_ant()) & 3) as u8;
        self.cmd(
            DATA_PATH,
            TLC_MNG_CONFIG,
            &tlc_config(self.channel > 14, chains),
        )?;
        let mut l = self.dev.link.lock();
        l.qos = qos;
        drop(l);
        if self
            .sta
            .as_ref()
            .is_some_and(|s| s.security == Security::Open)
        {
            // No handshake: the station reports Connected itself.
        }
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
                        let _ = self.send_mgmt(&f);
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
        let mut l = self.dev.link.lock();
        let was = l.connected;
        l.connected = false;
        l.ptk = false;
        l.data_q = None;
        l.mgmt_q = None;
        drop(l);
        if was {
            crate::println!("[iwlwifi] link down: {}", why);
            net::kick();
        }
    }

    /// Remove the AP station, its queues and the binding.
    fn teardown(&mut self) {
        self.sta = None;
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
        {
            let mut l = self.dev.link.lock();
            l.data_q = None;
            l.mgmt_q = None;
        }
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
