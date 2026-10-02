//! USB Bluetooth controllers (class E0/01/01): HCI commands go out as
//! class requests on the control pipe, events arrive on the interrupt IN
//! endpoint and ACL data on the bulk pipes (SCO is not used). Intel
//! controllers with TLV version information (AX200/AX210/...) start in a
//! bootloader and get their firmware (`intel/ibt-*.sfi`, then `.ddc`)
//! here before the standard HCI initialisation. MediaTek controllers
//! (MT7921/MT7922/MT7925, including the MT7921AU combo adapter) get their
//! patch (`mediatek/BT_RAM_CODE_*_hdr.bin`) over WMT commands, answered
//! through a vendor control request.

use super::UsbDevice;
use crate::bluetooth::{self, Hci, Transport};
use crate::errno::*;
use crate::mm::dma::DmaBuffer;
use crate::sync::Mutex;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use bt::hci::{self, Event};
use bt::intel;
use bt::mtk;
use usb_desc::{Endpoint, Interface, TransferType};

struct BtUsb {
    dev: Weak<UsbDevice>,
    iface: u8,
    bulk_out: Endpoint,
    out_buf: Mutex<DmaBuffer>,
    vendor: u16,
    product: u16,
}

impl Transport for BtUsb {
    fn send(&self, kind: u8, pkt: &[u8]) -> KResult<()> {
        let dev = self.dev.upgrade().ok_or(ENODEV)?;
        match kind {
            hci::H4_CMD => dev.control_out(0x20, 0, 0, self.iface as u16, pkt),
            hci::H4_ACL => {
                let mut b = self.out_buf.lock();
                if pkt.len() > b.len() {
                    return Err(EMSGSIZE);
                }
                b.as_mut_slice()[..pkt.len()].copy_from_slice(pkt);
                dev.transfer(&self.bulk_out, &b, pkt.len(), Some(2000))
                    .map(|_| ())
            }
            _ => Err(EINVAL),
        }
    }

    fn name(&self) -> String {
        let name = self.dev.upgrade().map(|d| d.name()).unwrap_or_default();
        alloc::format!("USB {} {:04x}:{:04x}", name, self.vendor, self.product)
    }

    fn setup(&self, h: &Arc<Hci>) -> KResult<()> {
        if self.vendor == 0x8087 {
            intel_setup(h)?;
        } else if let Some(dev) = self.dev.upgrade()
            && let Some(id) = mtk_chip(&dev)
        {
            self.mtk_setup(h, &dev, id)?;
        }
        Ok(())
    }

    fn is_gone(&self) -> bool {
        self.dev.upgrade().is_none_or(|d| d.is_gone())
    }
}

pub fn probe(dev: &Arc<UsbDevice>, iface: &Interface) -> bool {
    if (iface.class, iface.subclass, iface.protocol) != (0xE0, 0x01, 0x01) {
        return false;
    }
    let (Some(evt), Some(bin), Some(bout)) = (
        iface.find_endpoint(TransferType::Interrupt, true),
        iface.find_endpoint(TransferType::Bulk, true),
        iface.find_endpoint(TransferType::Bulk, false),
    ) else {
        return false;
    };
    if dev.configure_endpoints(&[evt, bin, bout]).is_err() {
        return false;
    }
    let (Some(out_buf), Some(ev_buf), Some(acl_buf)) = (
        DmaBuffer::new(4096),
        DmaBuffer::new(1024),
        DmaBuffer::new(4096),
    ) else {
        return false;
    };
    let desc = *dev.desc.lock();
    let tr = Arc::new(BtUsb {
        dev: Arc::downgrade(dev),
        iface: iface.number,
        bulk_out: bout,
        out_buf: Mutex::new(out_buf),
        vendor: desc.vendor,
        product: desc.product,
    });
    // The readers run before the controller is registered: the first
    // commands' events must get through.
    let slot: Arc<Mutex<Option<Arc<Hci>>>> = Arc::new(Mutex::new(None));
    let (d, s) = (dev.clone(), slot.clone());
    crate::sched::spawn("btusb-evt", move || {
        let mut buf = Vec::new();
        let len = (evt.packet_size() as usize).max(16);
        while !d.is_gone() {
            match d.transfer(&evt, &ev_buf, len, None) {
                Ok(n) => {
                    buf.extend_from_slice(&ev_buf.as_slice()[..n]);
                    while buf.len() >= 2 && buf.len() >= 2 + buf[1] as usize {
                        let pkt: Vec<u8> = buf.drain(..2 + buf[1] as usize).collect();
                        if let Some(h) = s.lock().clone() {
                            h.on_packet(hci::H4_EVENT, &pkt);
                        }
                    }
                }
                Err(ENODEV) => break,
                Err(_) => {
                    buf.clear();
                    crate::time::sleep_ms(20);
                }
            }
        }
    });
    let (d, s) = (dev.clone(), slot.clone());
    crate::sched::spawn("btusb-acl", move || {
        let mut buf = Vec::new();
        while !d.is_gone() {
            match d.transfer(&bin, &acl_buf, acl_buf.len(), None) {
                Ok(n) => {
                    buf.extend_from_slice(&acl_buf.as_slice()[..n]);
                    while buf.len() >= 4 {
                        let need = 4 + u16::from_le_bytes([buf[2], buf[3]]) as usize;
                        if buf.len() < need {
                            break;
                        }
                        let pkt: Vec<u8> = buf.drain(..need).collect();
                        if let Some(h) = s.lock().clone() {
                            h.on_packet(hci::H4_ACL, &pkt);
                        }
                    }
                }
                Err(ENODEV) => break,
                Err(_) => {
                    buf.clear();
                    crate::time::sleep_ms(20);
                }
            }
        }
    });
    let hci = bluetooth::register(tr);
    *slot.lock() = Some(hci.clone());
    crate::println!(
        "[usb] {}: Bluetooth controller {:04x}:{:04x} (hci{})",
        dev.name(),
        desc.vendor,
        desc.product,
        hci.index
    );
    dev.on_detach(move || bluetooth::unregister(&hci));
    true
}

/// Intel: load the operational firmware if the controller is in its
/// bootloader, then its configuration (DDC).
fn intel_setup(h: &Arc<Hci>) -> KResult<()> {
    let ret = h.command_ok(hci::cmd(intel::OP_READ_VERSION, &[0xFF]))?;
    let Some(v) = intel::parse_version(&ret) else {
        crate::println!(
            "[bt] {}: Intel controller without TLV version: no firmware loading",
            h.name()
        );
        return Ok(());
    };
    if v.image_type == intel::IMAGE_BOOTLOADER {
        let name = intel::firmware_name(&v, "sfi");
        let fw = crate::firmware::load(&name).inspect_err(|_| {
            crate::println!(
                "[bt] {}: firmware {} not found (install it with write_to_drive.sh --ax210-firmware)",
                h.name(),
                name
            );
        })?;
        let (cmds, boot) = intel::secure_send_plan(&fw, v.sbe_type).ok_or(EINVAL)?;
        crate::println!(
            "[bt] {}: downloading {} ({} fragments)",
            h.name(),
            name,
            cmds.len()
        );
        for p in cmds {
            h.command_ok(hci::cmd(intel::OP_SECURE_SEND, &p))?;
        }
        // The bootloader resets without completing the command, then
        // reports that the new image booted.
        let since = h.ev_seq();
        h.send_cmd(hci::cmd(intel::OP_RESET, &intel::reset_params(boot)));
        let booted = h.wait_event(since, 5000, |e| match e {
            Event::Vendor(p) if p.first() == Some(&intel::EVT_BOOTUP) => Some(()),
            _ => None,
        });
        if booted.is_none() {
            crate::println!("[bt] {}: no boot notification after the download", h.name());
        }
        crate::time::sleep_ms(100);
    }
    let v = h
        .command_ok(hci::cmd(intel::OP_READ_VERSION, &[0xFF]))
        .ok()
        .and_then(|r| intel::parse_version(&r))
        .unwrap_or(v);
    if v.image_type != intel::IMAGE_OPERATIONAL {
        crate::println!(
            "[bt] {}: firmware did not start (image type {})",
            h.name(),
            v.image_type
        );
        return Err(EIO);
    }
    if let Ok(ddc) = crate::firmware::load(&intel::firmware_name(&v, "ddc")) {
        for rec in intel::ddc_commands(&ddc) {
            let _ = h.command_ok(hci::cmd(intel::OP_WRITE_DDC, &rec));
        }
    }
    crate::println!(
        "[bt] {}: Intel firmware build {} running",
        h.name(),
        v.build_num
    );
    Ok(())
}

/// MediaTek chip ID (0x7961, 0x7922, ...), read from the chip's registers
/// by the vendor request btmtk uses; None for other controllers.
fn mtk_chip(dev: &UsbDevice) -> Option<u32> {
    let d = *dev.desc.lock();
    if !mtk::is_mediatek(d.vendor, d.product) {
        return None;
    }
    let id = mtk_reg(dev, mtk::REG_DEV_ID).ok()?;
    mtk::supported(id).then_some(id)
}

fn mtk_reg(dev: &UsbDevice, reg: u32) -> KResult<u32> {
    let v = dev.control_in(0x40, 0x63, (reg >> 16) as u16, reg as u16, 4)?;
    let b: [u8; 4] = v.get(..4).ok_or(EIO)?.try_into().unwrap();
    Ok(u32::from_le_bytes(b))
}

impl BtUsb {
    /// One WMT command and its answer: the command goes out as HCI command
    /// 0xFC6F (outside the HCI command queue: no Command Complete follows),
    /// the answer is polled with vendor control-IN requests.
    fn wmt(&self, dev: &UsbDevice, op: u8, flag: u8, data: &[u8]) -> KResult<mtk::WmtEvent> {
        self.send(
            hci::H4_CMD,
            &hci::cmd(mtk::OP_WMT, &mtk::wmt_cmd(op, flag, data)),
        )?;
        let deadline = crate::time::Deadline::after_ms(5000);
        while !deadline.expired() {
            if dev.is_gone() {
                return Err(ENODEV);
            }
            let r = dev.control_in(0x40, 0x01, 0x30, 0, 64)?;
            if !r.is_empty() {
                if let Some(e) = mtk::parse_event(&r)
                    && e.op == op
                {
                    return Ok(e);
                }
                return Err(EIO);
            }
            crate::time::delay_us(500);
        }
        Err(ETIMEDOUT)
    }

    /// MediaTek 79xx: download the patch, reset the endpoints, turn the
    /// Bluetooth function on (btmtk_usb_setup()).
    fn mtk_setup(&self, h: &Arc<Hci>, dev: &UsbDevice, id: u32) -> KResult<()> {
        let version = mtk_reg(dev, mtk::REG_FW_VERSION).unwrap_or(0);
        let flavor = (mtk_reg(dev, mtk::REG_FW_FLAVOR).unwrap_or(0) & 0x80) >> 7;
        let name = mtk::firmware_name(id, version, flavor);
        let fw = crate::firmware::load(&name).inspect_err(|_| {
            crate::println!(
                "[bt] {}: MT{:04x}: firmware {} not found",
                h.name(),
                id,
                name
            );
        })?;
        let sections = mtk::sections(&fw, id).ok_or(EINVAL)?;
        crate::println!(
            "[bt] {}: MT{:04x}: downloading {} ({}; {} sections)",
            h.name(),
            id,
            name,
            mtk::describe(&fw).unwrap_or_default(),
            sections.len()
        );
        for sec in &sections {
            let mut tries = 20;
            let send = loop {
                let e = self.wmt(dev, mtk::WMT_PATCH_DWNLD, 0, &sec.announce)?;
                match mtk::status(&e) {
                    Some(mtk::Status::PatchUndone) => break true,
                    Some(mtk::Status::PatchDone) => break false,
                    Some(mtk::Status::PatchProgress) if tries > 0 => {
                        tries -= 1;
                        crate::time::sleep_ms(100);
                    }
                    s => {
                        crate::println!("[bt] {}: MT{:04x}: patch status {:?}", h.name(), id, s);
                        return Err(EIO);
                    }
                }
            };
            if send {
                for (flag, block) in &sec.blocks {
                    self.wmt(dev, mtk::WMT_PATCH_DWNLD, *flag, block)?;
                }
            }
        }
        // Let the patch activate.
        crate::time::sleep_ms(110);
        let reg = mtk::REG_EP_RST_OPT;
        dev.control_out(
            0x5E,
            0x02,
            (reg >> 16) as u16,
            reg as u16,
            &mtk::EP_RST_IN_OUT_OPT.to_le_bytes(),
        )?;
        self.wmt(dev, mtk::WMT_FUNC_CTRL, 0, &[1])?;
        crate::println!("[bt] {}: MT{:04x}: firmware running", h.name(), id);
        Ok(())
    }
}
