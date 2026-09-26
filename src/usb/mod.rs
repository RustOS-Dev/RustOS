//! USB core: device model, enumeration, class-driver binding, hotplug.
//!
//! Host controllers (xHCI) report root-port changes; hubs poll their ports.
//! Both call [`attach`] for new devices and [`detach`] for removed ones.
//! Class drivers (hub, HID, mass storage, and whatever registers through
//! [`register_driver`], e.g. USB networking) bind per interface.

pub mod cdc_ether;
pub mod hid;
pub mod hub;
pub mod storage;
pub mod xhci;

use crate::errno::*;
use crate::mm::dma::DmaBuffer;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;
pub use usb_desc::{Configuration, DeviceDescriptor, Endpoint, Interface, TransferType};
use xhci::{EpConfig, HubConfig, SlotInfo, Xhci};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Speed {
    Low,
    #[default]
    Full,
    High,
    Super,
    SuperPlus,
}

impl Speed {
    pub fn from_xhci(v: u32) -> Speed {
        match v {
            2 => Speed::Low,
            3 => Speed::High,
            4 => Speed::Super,
            5.. => Speed::SuperPlus,
            _ => Speed::Full,
        }
    }
    pub fn xhci(self) -> u32 {
        match self {
            Speed::Full => 1,
            Speed::Low => 2,
            Speed::High => 3,
            Speed::Super => 4,
            Speed::SuperPlus => 5,
        }
    }
    pub fn mbps(self) -> &'static str {
        match self {
            Speed::Low => "1.5M",
            Speed::Full => "12M",
            Speed::High => "480M",
            Speed::Super => "5000M",
            Speed::SuperPlus => "10000M",
        }
    }
    fn default_mps0(self) -> u16 {
        match self {
            Speed::Low | Speed::Full => 8,
            Speed::High => 64,
            _ => 512,
        }
    }
}

// Standard requests.
pub const REQ_GET_STATUS: u8 = 0;
pub const REQ_CLEAR_FEATURE: u8 = 1;
pub const REQ_SET_FEATURE: u8 = 3;
pub const REQ_GET_DESCRIPTOR: u8 = 6;
pub const REQ_SET_CONFIGURATION: u8 = 9;
pub const REQ_SET_INTERFACE: u8 = 11;

pub struct UsbDevice {
    pub hc: Arc<Xhci>,
    pub slot: u8,
    pub speed: Speed,
    pub info: SlotInfo,
    /// Port on the parent hub (root port for tier 1).
    pub port: u8,
    /// 1 = attached to a root port.
    pub tier: u8,
    pub parent_slot: u8,
    pub desc: Mutex<DeviceDescriptor>,
    pub config: Mutex<Option<Configuration>>,
    pub manufacturer: Mutex<String>,
    pub product: Mutex<String>,
    pub serial: Mutex<String>,
    pub drivers: Mutex<Vec<&'static str>>,
    pub children: Mutex<BTreeMap<u8, Arc<UsbDevice>>>,
    gone: AtomicBool,
    on_detach: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
}

impl UsbDevice {
    pub fn is_gone(&self) -> bool {
        self.gone.load(Ordering::SeqCst)
    }

    pub fn name(&self) -> String {
        format!("usb{}-{}", self.hc.index + 1, self.slot)
    }

    /// Run `f` when the device is unplugged.
    pub fn on_detach(&self, f: impl FnOnce() + Send + 'static) {
        self.on_detach.lock().push(Box::new(f));
    }

    fn setup(rt: u8, req: u8, value: u16, index: u16, len: u16) -> [u8; 8] {
        let mut s = [0u8; 8];
        s[0] = rt;
        s[1] = req;
        s[2..4].copy_from_slice(&value.to_le_bytes());
        s[4..6].copy_from_slice(&index.to_le_bytes());
        s[6..8].copy_from_slice(&len.to_le_bytes());
        s
    }

    pub fn control_in(
        &self,
        rt: u8,
        req: u8,
        value: u16,
        index: u16,
        len: usize,
    ) -> KResult<Vec<u8>> {
        let buf = DmaBuffer::new(len.max(1)).ok_or(ENOMEM)?;
        let n = self.hc.control(
            self,
            Self::setup(rt | 0x80, req, value, index, len as u16),
            Some((&buf, len)),
            5000,
        )?;
        Ok(buf.as_slice()[..n.min(len)].to_vec())
    }

    pub fn control_out(&self, rt: u8, req: u8, value: u16, index: u16, data: &[u8]) -> KResult<()> {
        let setup = Self::setup(rt & 0x7F, req, value, index, data.len() as u16);
        if data.is_empty() {
            self.hc.control(self, setup, None, 5000)?;
        } else {
            let mut buf = DmaBuffer::new(data.len()).ok_or(ENOMEM)?;
            buf.as_mut_slice()[..data.len()].copy_from_slice(data);
            self.hc
                .control(self, setup, Some((&buf, data.len())), 5000)?;
        }
        Ok(())
    }

    pub fn get_descriptor(&self, ty: u8, idx: u8, lang: u16, len: usize) -> KResult<Vec<u8>> {
        self.control_in(
            0x00,
            REQ_GET_DESCRIPTOR,
            (ty as u16) << 8 | idx as u16,
            lang,
            len,
        )
    }

    pub fn string(&self, idx: u8) -> Option<String> {
        if idx == 0 {
            return None;
        }
        let langs = self.get_descriptor(usb_desc::DT_STRING, 0, 0, 255).ok()?;
        let lang = if langs.len() >= 4 {
            u16::from_le_bytes([langs[2], langs[3]])
        } else {
            0x0409
        };
        let b = self
            .get_descriptor(usb_desc::DT_STRING, idx, lang, 255)
            .ok()?;
        let mut s = String::new();
        usb_desc::parse_string(&b, &mut s).then(|| String::from(s.trim()))
    }

    fn ep_config(&self, ep: &Endpoint) -> EpConfig {
        let tt = ep.transfer_type();
        let ep_type = match (tt, ep.is_in()) {
            (TransferType::Control, _) => 4,
            (TransferType::Isochronous, false) => 1,
            (TransferType::Bulk, false) => 2,
            (TransferType::Interrupt, false) => 3,
            (TransferType::Isochronous, true) => 5,
            (TransferType::Bulk, true) => 6,
            (TransferType::Interrupt, true) => 7,
        };
        let periodic = matches!(tt, TransferType::Interrupt | TransferType::Isochronous);
        let interval = if !periodic {
            0
        } else if self.speed >= Speed::High {
            ep.interval.clamp(1, 16) - 1
        } else if tt == TransferType::Isochronous {
            ep.interval.clamp(1, 16) + 2
        } else {
            // Frames (ms) -> 125 µs units, log2.
            let uf = (ep.interval.max(1) as u32) * 8;
            (31 - uf.leading_zeros()).clamp(3, 10) as u8
        };
        let (max_burst, mult) = if self.speed >= Speed::Super {
            let m = if tt == TransferType::Isochronous {
                ep.ss_attributes & 3
            } else {
                0
            };
            (ep.max_burst, m)
        } else if self.speed == Speed::High && periodic {
            (ep.hs_mult(), 0)
        } else {
            (0, 0)
        };
        let mut mps = ep.packet_size();
        if self.speed >= Speed::Super && tt == TransferType::Bulk {
            mps = mps.max(1024);
        }
        EpConfig {
            dci: ep.dci(),
            ep_type,
            max_packet: mps,
            max_burst,
            mult,
            interval,
        }
    }

    /// Enable endpoints on the controller (after SET_CONFIGURATION).
    pub fn configure_endpoints(&self, eps: &[Endpoint]) -> KResult<()> {
        let cfgs: Vec<EpConfig> = eps.iter().map(|e| self.ep_config(e)).collect();
        self.hc.configure_endpoints(self.slot, &cfgs, None)
    }

    pub fn configure_hub(&self, eps: &[Endpoint], hub: HubConfig) -> KResult<()> {
        let cfgs: Vec<EpConfig> = eps.iter().map(|e| self.ep_config(e)).collect();
        self.hc.configure_endpoints(self.slot, &cfgs, Some(hub))
    }

    /// Bulk/interrupt transfer using a caller-provided DMA buffer.
    /// A stalled endpoint is recovered (host side and CLEAR_FEATURE on the
    /// device) and reported as EPIPE.
    pub fn transfer(
        &self,
        ep: &Endpoint,
        buf: &DmaBuffer,
        len: usize,
        timeout_ms: Option<u64>,
    ) -> KResult<usize> {
        let r = self
            .hc
            .transfer(self, ep.dci(), buf.phys(), len.min(buf.len()), timeout_ms);
        if r == Err(EPIPE) {
            self.clear_halt(ep);
        }
        r
    }

    pub fn clear_halt(&self, ep: &Endpoint) {
        let _ = self.control_out(0x02, REQ_CLEAR_FEATURE, 0, ep.address as u16, &[]);
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

pub type DriverProbe = fn(&Arc<UsbDevice>, &Interface) -> bool;

static CONTROLLERS: Mutex<Vec<Arc<Xhci>>> = Mutex::new(Vec::new());
static DEVICES: Mutex<Vec<Arc<UsbDevice>>> = Mutex::new(Vec::new());
static DRIVERS: Mutex<Vec<(&'static str, DriverProbe)>> = Mutex::new(Vec::new());

/// Register an additional class driver (e.g. USB networking). Existing
/// devices are offered to it immediately.
pub fn register_driver(name: &'static str, probe: DriverProbe) {
    DRIVERS.lock().push((name, probe));
    for d in devices() {
        let ifaces: Vec<Interface> = d
            .config
            .lock()
            .as_ref()
            .map(|c| c.default_interfaces().cloned().collect())
            .unwrap_or_default();
        for i in ifaces {
            if d.drivers.lock().is_empty() && probe(&d, &i) {
                d.drivers.lock().push(name);
            }
        }
    }
}

pub fn devices() -> Vec<Arc<UsbDevice>> {
    DEVICES.lock().clone()
}

pub fn controllers() -> Vec<Arc<Xhci>> {
    CONTROLLERS.lock().clone()
}

fn bind(dev: &Arc<UsbDevice>) {
    let ifaces: Vec<Interface> = dev
        .config
        .lock()
        .as_ref()
        .map(|c| c.default_interfaces().cloned().collect())
        .unwrap_or_default();
    for iface in &ifaces {
        let builtin: [(&'static str, DriverProbe); 4] = [
            ("hub", hub::probe),
            ("usbhid", hid::probe),
            ("usb-storage", storage::probe),
            ("cdc_ether", cdc_ether::probe),
        ];
        let extra = DRIVERS.lock().clone();
        for (name, probe) in builtin.iter().chain(extra.iter()) {
            if probe(dev, iface) {
                dev.drivers.lock().push(name);
                break;
            }
        }
    }
}

/// Enumerate a newly connected (and reset) device.
pub fn attach(
    hc: &Arc<Xhci>,
    parent: Option<&Arc<UsbDevice>>,
    port: u8,
    speed: Speed,
) -> KResult<Arc<UsbDevice>> {
    let (tier, info) = match parent {
        None => (
            1,
            SlotInfo {
                speed,
                route: 0,
                root_port: port,
                ..Default::default()
            },
        ),
        Some(p) => {
            let shift = 4 * (p.tier as u32 - 1);
            let mut info = SlotInfo {
                speed,
                route: p.info.route | ((port.min(15) as u32) << shift),
                root_port: p.info.root_port,
                ..Default::default()
            };
            if speed <= Speed::Full {
                if p.speed == Speed::High {
                    info.tt_slot = p.slot;
                    info.tt_port = port;
                    info.mtt = p.desc.lock().protocol == 2;
                } else {
                    info.tt_slot = p.info.tt_slot;
                    info.tt_port = p.info.tt_port;
                    info.mtt = p.info.mtt;
                }
            }
            (p.tier + 1, info)
        }
    };
    if tier > 6 {
        return Err(EINVAL);
    }
    let slot = hc.address_device(&info, speed.default_mps0())?;
    let dev = Arc::new(UsbDevice {
        hc: hc.clone(),
        slot,
        speed,
        info,
        port,
        tier,
        parent_slot: parent.map_or(0, |p| p.slot),
        desc: Mutex::new(DeviceDescriptor::default()),
        config: Mutex::new(None),
        manufacturer: Mutex::new(String::new()),
        product: Mutex::new(String::new()),
        serial: Mutex::new(String::new()),
        drivers: Mutex::new(Vec::new()),
        children: Mutex::new(BTreeMap::new()),
        gone: AtomicBool::new(false),
        on_detach: Mutex::new(Vec::new()),
    });
    if let Err(e) = enumerate(&dev) {
        crate::println!("[usb] {}: enumeration failed: {}", dev.name(), e);
        dev.gone.store(true, Ordering::SeqCst);
        hc.free_slot(slot);
        return Err(e);
    }
    DEVICES.lock().push(dev.clone());
    let d = *dev.desc.lock();
    crate::println!(
        "[usb] {}: {:04x}:{:04x} {} {} ({}, port {}{})",
        dev.name(),
        d.vendor,
        d.product,
        dev.manufacturer.lock(),
        dev.product.lock(),
        speed.mbps(),
        port,
        if tier > 1 {
            format!(", tier {}", tier)
        } else {
            String::new()
        }
    );
    bind(&dev);
    Ok(dev)
}

fn enumerate(dev: &Arc<UsbDevice>) -> KResult<()> {
    crate::time::sleep_ms(2);
    let head = dev.get_descriptor(usb_desc::DT_DEVICE, 0, 0, 8)?;
    if head.len() < 8 {
        return Err(EIO);
    }
    let mps = if dev.speed >= Speed::Super {
        1u16 << head[7].min(9)
    } else {
        head[7] as u16
    };
    if mps != dev.speed.default_mps0() && mps >= 8 {
        dev.hc.set_max_packet0(dev.slot, mps)?;
    }
    let full = dev.get_descriptor(usb_desc::DT_DEVICE, 0, 0, 18)?;
    let desc = DeviceDescriptor::parse(&full).ok_or(EIO)?;
    *dev.desc.lock() = desc;
    *dev.manufacturer.lock() = dev.string(desc.manufacturer_idx).unwrap_or_default();
    *dev.product.lock() = dev.string(desc.product_idx).unwrap_or_default();
    *dev.serial.lock() = dev.string(desc.serial_idx).unwrap_or_default();
    // Read every configuration; prefer one with a class we drive natively
    // over vendor/RNDIS alternatives (e.g. CDC ECM over RNDIS).
    let mut configs = Vec::new();
    for idx in 0..desc.num_configs.clamp(1, 8) {
        let head = dev.get_descriptor(usb_desc::DT_CONFIG, idx, 0, 9)?;
        if head.len() < 4 {
            return Err(EIO);
        }
        let total = u16::from_le_bytes([head[2], head[3]]) as usize;
        let raw = dev.get_descriptor(usb_desc::DT_CONFIG, idx, 0, total)?;
        if let Some(c) = Configuration::parse(&raw) {
            configs.push(c);
        }
    }
    // RUSTOS_USB_PREFER_RNDIS (build-time) exercises the RNDIS path in tests.
    let want_rndis = option_env!("RUSTOS_USB_PREFER_RNDIS").is_some();
    let preferred = configs
        .iter()
        .position(|c| {
            c.interfaces.iter().any(|i| {
                if want_rndis {
                    i.class == 0xE0 || (i.class == usb_desc::CLASS_CDC && i.subclass == 2)
                } else {
                    i.class == usb_desc::CLASS_CDC && i.subclass == 6
                }
            })
        })
        .unwrap_or(0);
    if configs.is_empty() {
        return Err(EIO);
    }
    let cfg = configs.swap_remove(preferred);
    dev.control_out(0x00, REQ_SET_CONFIGURATION, cfg.value as u16, 0, &[])?;
    *dev.config.lock() = Some(cfg);
    Ok(())
}

/// Tear down a device (and everything behind it, for hubs).
pub fn detach(dev: &Arc<UsbDevice>) {
    if dev.gone.swap(true, Ordering::SeqCst) {
        return;
    }
    let children: Vec<Arc<UsbDevice>> = dev.children.lock().values().cloned().collect();
    for c in &children {
        detach(c);
    }
    dev.children.lock().clear();
    dev.hc.wq.wake_all();
    let hooks: Vec<Box<dyn FnOnce() + Send>> = core::mem::take(&mut *dev.on_detach.lock());
    for h in hooks {
        h();
    }
    DEVICES.lock().retain(|d| !Arc::ptr_eq(d, dev));
    dev.hc.free_slot(dev.slot);
    crate::println!("[usb] {}: disconnected", dev.name());
}

// ---------------------------------------------------------------------------
// Root hub handling
// ---------------------------------------------------------------------------

fn root_port_changed(hc: &Arc<Xhci>, port: u8) {
    let sc = hc.portsc(port);
    let connect_change = sc & (1 << 17) != 0;
    hc.ack_port(port);
    let connected = sc & xhci::PORT_CCS != 0;
    let existing = hc.roots.lock().get(&port).cloned();
    if let Some(d) = existing
        && (!connected || connect_change || d.is_gone())
    {
        hc.roots.lock().remove(&port);
        detach(&d);
    }
    if connected && !hc.roots.lock().contains_key(&port) {
        crate::time::sleep_ms(100); // debounce
        if hc.portsc(port) & xhci::PORT_CCS == 0 {
            return;
        }
        match hc.reset_port(port) {
            Ok(speed) => match attach(hc, None, port, speed) {
                Ok(d) => {
                    hc.roots.lock().insert(port, d);
                }
                Err(e) => {
                    crate::println!("[usb] xhci{} port {}: attach failed: {}", hc.index, port, e)
                }
            },
            Err(e) => crate::println!("[usb] xhci{} port {}: reset failed: {}", hc.index, port, e),
        }
        hc.ack_port(port);
    }
}

fn root_hub_thread(hc: Arc<Xhci>) {
    loop {
        hc.process_events();
        let changes = hc.take_port_changes();
        for p in 1..=hc.max_ports.min(64) {
            if changes & (1 << (p - 1)) != 0 {
                root_port_changed(&hc, p);
            }
        }
        let h = hc.clone();
        hc.wq.wait_timeout(1000, move || h.has_port_changes());
    }
}

/// Probe every xHCI controller and start hotplug handling.
pub fn init() {
    crate::vfs::procfs::register("bus/usb/devices", gen_devices);
    let ctrls = crate::pci::find_by_class(0x0C, 0x03, Some(0x30));
    for dev in ctrls {
        let idx = CONTROLLERS.lock().len();
        if let Some(hc) = xhci::probe(&dev, idx) {
            CONTROLLERS.lock().push(hc.clone());
            crate::sched::spawn(&format!("xhci{}", idx), move || root_hub_thread(hc));
        }
    }
}

/// Stop controllers before reboot.
pub fn shutdown() {
    for hc in controllers() {
        xhci::halt(&hc);
    }
}

fn gen_devices() -> String {
    let mut s = String::new();
    for hc in controllers() {
        let _ = writeln!(
            s,
            "Bus {:03} Device 000: ID 1d6b:0003 xHCI root hub ({} ports, {})",
            hc.index + 1,
            hc.max_ports,
            hc.irq_mode
        );
    }
    for d in devices() {
        let desc = *d.desc.lock();
        let _ = writeln!(
            s,
            "Bus {:03} Device {:03}: ID {:04x}:{:04x} {} {} [{} port {} class {:02x}] {}",
            d.hc.index + 1,
            d.slot,
            desc.vendor,
            desc.product,
            d.manufacturer.lock(),
            d.product.lock(),
            d.speed.mbps(),
            d.port,
            desc.class,
            d.drivers.lock().join(",")
        );
    }
    s
}
