//! USB mass storage with the SCSI transparent command set, over Bulk-Only
//! Transport or USB Attached SCSI (`uas.rs`). Each LUN becomes a block
//! device (sdX) with partitions and automount.

use super::UsbDevice;
use crate::block::{self, BlockDevice, DiskKind};
use crate::errno::*;
use crate::mm::dma::DmaBuffer;
use crate::sched::mutex::Mutex;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use usb_desc::{CLASS_MASS_STORAGE, Endpoint, Interface, TransferType};

const CBW_SIG: u32 = 0x4342_5355;
const CSW_SIG: u32 = 0x5342_5355;
const BUF: usize = 64 * 1024;
const TIMEOUT_MS: u64 = 20_000;

pub(super) struct Io {
    /// BOT: CBW/CSW. UAS: command IU at 0, sense IU at 128.
    pub(super) cbw: DmaBuffer,
    pub(super) data: DmaBuffer,
    pub(super) tag: u32,
    /// UAS: sense data (key, ASC, ASCQ) from the last failed command.
    pub(super) last_sense: Option<(u8, u8, u8)>,
}

pub(super) enum Kind {
    Bot { ep_in: Endpoint, ep_out: Endpoint },
    Uas(super::uas::Pipes),
}

pub(super) struct Transport {
    pub(super) dev: Arc<UsbDevice>,
    pub(super) iface: u8,
    pub(super) kind: Kind,
    pub(super) io: Mutex<Io>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Dir {
    None,
    In,
    Out,
}

impl Transport {
    fn reset_recovery(&self) {
        let Kind::Bot { ep_in, ep_out } = &self.kind else {
            return;
        };
        let _ = self.dev.control_out(0x21, 0xFF, 0, self.iface as u16, &[]);
        self.dev.clear_halt(ep_in);
        self.dev.clear_halt(ep_out);
    }

    /// Run one SCSI command. For writes, the data is taken from
    /// `io.data[..len]`; for reads it is left there. Returns bytes moved.
    pub(super) fn command(
        &self,
        io: &mut Io,
        lun: u8,
        cdb: &[u8],
        dir: Dir,
        len: usize,
    ) -> KResult<usize> {
        if self.dev.is_gone() {
            return Err(ENODEV);
        }
        let (ep_in, ep_out) = match &self.kind {
            Kind::Bot { ep_in, ep_out } => (ep_in, ep_out),
            Kind::Uas(p) => return super::uas::command(self, p, io, lun, cdb, dir, len),
        };
        io.tag = io.tag.wrapping_add(1);
        let tag = io.tag;
        let c = &io.cbw;
        c.zero();
        c.write::<u32>(0, CBW_SIG);
        c.write::<u32>(4, tag);
        c.write::<u32>(8, len as u32);
        c.write::<u8>(12, if dir == Dir::In { 0x80 } else { 0 });
        c.write::<u8>(13, lun);
        c.write::<u8>(14, cdb.len() as u8);
        for (i, b) in cdb.iter().enumerate() {
            c.write::<u8>(15 + i, *b);
        }
        if let Err(e) = self.dev.transfer(ep_out, c, 31, Some(TIMEOUT_MS)) {
            if e != ENODEV {
                self.reset_recovery();
            }
            return Err(e);
        }
        let mut moved = 0;
        if dir != Dir::None && len > 0 {
            let ep = if dir == Dir::In { ep_in } else { ep_out };
            match self.dev.transfer(ep, &io.data, len, Some(TIMEOUT_MS)) {
                Ok(n) => moved = n,
                Err(EPIPE) => {} // stalled data phase: read the CSW anyway
                Err(e) => {
                    if e != ENODEV {
                        self.reset_recovery();
                    }
                    return Err(e);
                }
            }
        }
        // CSW (retry once after a stall).
        let mut csw = Err(EIO);
        for _ in 0..2 {
            csw = self.dev.transfer(ep_in, c, 13, Some(TIMEOUT_MS));
            if csw != Err(EPIPE) {
                break;
            }
        }
        match csw {
            Ok(13) => {}
            Ok(_) | Err(_) => {
                if !self.dev.is_gone() {
                    self.reset_recovery();
                }
                return Err(if self.dev.is_gone() { ENODEV } else { EIO });
            }
        }
        if c.read::<u32>(0) != CSW_SIG || c.read::<u32>(4) != tag {
            self.reset_recovery();
            return Err(EIO);
        }
        match c.read::<u8>(12) {
            0 => Ok(moved),
            1 => Err(EIO),
            _ => {
                self.reset_recovery();
                Err(EIO)
            }
        }
    }

    /// REQUEST SENSE: (sense key, ASC, ASCQ).
    fn sense(&self, io: &mut Io, lun: u8) -> Option<(u8, u8, u8)> {
        if let Kind::Uas(_) = self.kind {
            // UAS returns sense data with the failed command.
            return io.last_sense.take();
        }
        let n = self
            .command(io, lun, &[0x03, 0, 0, 0, 18, 0], Dir::In, 18)
            .ok()?;
        let d = io.data.as_slice();
        (n >= 14).then(|| (d[2] & 0x0F, d[12], d[13]))
    }
}

pub struct UsbDisk {
    t: Arc<Transport>,
    lun: u8,
    sectors: u64,
    sector_size: usize,
    read_only: bool,
    model: String,
}

impl UsbDisk {
    fn rw(
        &self,
        lba: u64,
        count: usize,
        dir: Dir,
        buf_in: Option<&mut [u8]>,
        buf_out: Option<&[u8]>,
    ) -> KResult<()> {
        let mut io = self.t.io.lock();
        let len = count * self.sector_size;
        if let Some(src) = buf_out {
            io.data.as_mut_slice()[..len].copy_from_slice(&src[..len]);
        }
        let cdb: alloc::vec::Vec<u8> = if lba + count as u64 > u32::MAX as u64 {
            let mut c = alloc::vec![if dir == Dir::In { 0x88 } else { 0x8A }; 1];
            c.push(0);
            c.extend_from_slice(&lba.to_be_bytes());
            c.extend_from_slice(&(count as u32).to_be_bytes());
            c.extend_from_slice(&[0, 0]);
            c
        } else {
            let mut c = alloc::vec![if dir == Dir::In { 0x28 } else { 0x2A }, 0];
            c.extend_from_slice(&(lba as u32).to_be_bytes());
            c.push(0);
            c.extend_from_slice(&(count as u16).to_be_bytes());
            c.push(0);
            c
        };
        let mut last = Err(EIO);
        for _attempt in 0..3 {
            last = self.t.command(&mut io, self.lun, &cdb, dir, len);
            match last {
                Ok(n) if n == len => break,
                Ok(_) => last = Err(EIO),
                Err(ENODEV) => return Err(ENODEV),
                Err(_) => {
                    let _ = self.t.sense(&mut io, self.lun);
                }
            }
        }
        last?;
        if let Some(dst) = buf_in {
            dst[..len].copy_from_slice(&io.data.as_slice()[..len]);
        }
        Ok(())
    }
}

impl BlockDevice for UsbDisk {
    fn sector_size(&self) -> usize {
        self.sector_size
    }
    fn sector_count(&self) -> u64 {
        self.sectors
    }
    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> KResult<()> {
        let per = BUF / self.sector_size;
        let total = buf.len() / self.sector_size;
        let mut done = 0;
        while done < total {
            let n = per.min(total - done);
            let off = done * self.sector_size;
            self.rw(
                lba + done as u64,
                n,
                Dir::In,
                Some(&mut buf[off..off + n * self.sector_size]),
                None,
            )?;
            done += n;
        }
        Ok(())
    }
    fn write_sectors(&self, lba: u64, buf: &[u8]) -> KResult<()> {
        if self.read_only {
            return Err(EROFS);
        }
        let per = BUF / self.sector_size;
        let total = buf.len() / self.sector_size;
        let mut done = 0;
        while done < total {
            let n = per.min(total - done);
            let off = done * self.sector_size;
            self.rw(
                lba + done as u64,
                n,
                Dir::Out,
                None,
                Some(&buf[off..off + n * self.sector_size]),
            )?;
            done += n;
        }
        Ok(())
    }
    fn flush(&self) -> KResult<()> {
        if self.t.dev.is_gone() {
            return Err(ENODEV);
        }
        let mut io = self.t.io.lock();
        // SYNCHRONIZE CACHE(10); many devices don't implement it.
        let _ = self.t.command(
            &mut io,
            self.lun,
            &[0x35, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            Dir::None,
            0,
        );
        Ok(())
    }
    fn read_only(&self) -> bool {
        self.read_only
    }
    fn model(&self) -> String {
        self.model.clone()
    }
    fn removed(&self) -> bool {
        self.t.dev.is_gone()
    }
}

pub fn probe(dev: &Arc<UsbDevice>, iface: &Interface) -> bool {
    // SCSI transparent command set over Bulk-Only Transport.
    if iface.class != CLASS_MASS_STORAGE || iface.subclass != 6 || iface.protocol != 0x50 {
        return false;
    }
    let (Some(ep_in), Some(ep_out)) = (
        iface.find_endpoint(TransferType::Bulk, true),
        iface.find_endpoint(TransferType::Bulk, false),
    ) else {
        return false;
    };
    if dev.configure_endpoints(&[ep_in, ep_out]).is_err() {
        return false;
    }
    start(dev, iface.number, Kind::Bot { ep_in, ep_out })
}

/// Bring up the LUNs behind a configured transport.
pub(super) fn start(dev: &Arc<UsbDevice>, iface: u8, kind: Kind) -> bool {
    let (Some(cbw), Some(data)) = (DmaBuffer::new(256), DmaBuffer::new(BUF)) else {
        return false;
    };
    let uas = matches!(kind, Kind::Uas(_));
    let t = Arc::new(Transport {
        dev: dev.clone(),
        iface,
        kind,
        io: Mutex::new(Io {
            cbw,
            data,
            tag: 0,
            last_sense: None,
        }),
    });
    // Bring the LUNs up in a thread: spin-up can take seconds.
    let d = dev.clone();
    crate::sched::spawn(&format!("usb-storage{}", dev.slot), move || {
        let max_lun = if uas {
            0
        } else {
            d.control_in(0x21, 0xFE, 0, t.iface as u16, 1)
                .ok()
                .and_then(|b| b.first().copied())
                .unwrap_or(0)
                .min(7)
        };
        for lun in 0..=max_lun {
            match init_lun(&t, lun) {
                Ok(disk) => {
                    let name = block::register_disk(Arc::new(disk), DiskKind::Scsi, 0);
                    let n = name.clone();
                    d.on_detach(move || block::unregister_disk(&n));
                    block::automount();
                }
                Err(e) => {
                    if e != ENOMEDIUM {
                        crate::println!("[usb] {}: LUN {}: {}", d.name(), lun, e);
                    }
                }
            }
        }
    });
    true
}

fn init_lun(t: &Arc<Transport>, lun: u8) -> KResult<UsbDisk> {
    let mut io = t.io.lock();
    let n = t.command(&mut io, lun, &[0x12, 0, 0, 0, 36, 0], Dir::In, 36)?;
    let inq = io.data.as_slice();
    if n < 36 || inq[0] & 0x1F != 0 {
        return Err(ENODEV); // not a direct-access block device
    }
    let vendor: String = String::from_utf8_lossy(&inq[8..16]).trim().into();
    let product: String = String::from_utf8_lossy(&inq[16..32]).trim().into();
    let model = format!("{} {}", vendor_str(&vendor), product);
    // Wait for the medium (TEST UNIT READY).
    let deadline = crate::time::Deadline::after_ms(10_000);
    loop {
        match t.command(&mut io, lun, &[0; 6], Dir::None, 0) {
            Ok(_) => break,
            Err(ENODEV) => return Err(ENODEV),
            Err(_) => {
                let s = t.sense(&mut io, lun);
                if s.is_some_and(|(k, asc, _)| k == 2 && asc == 0x3A) {
                    return Err(ENOMEDIUM);
                }
                if deadline.expired() {
                    return Err(ETIMEDOUT);
                }
                drop(io);
                crate::time::sleep_ms(100);
                io = t.io.lock();
            }
        }
    }
    let n = t.command(&mut io, lun, &[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0], Dir::In, 8)?;
    if n < 8 {
        return Err(EIO);
    }
    let d = io.data.as_slice();
    let mut last = u32::from_be_bytes(d[0..4].try_into().unwrap()) as u64;
    let mut bs = u32::from_be_bytes(d[4..8].try_into().unwrap()) as usize;
    if last == 0xFFFF_FFFF {
        let mut cdb = [0u8; 16];
        cdb[0] = 0x9E;
        cdb[1] = 0x10;
        cdb[13] = 32;
        let n = t.command(&mut io, lun, &cdb, Dir::In, 32)?;
        if n < 12 {
            return Err(EIO);
        }
        let d = io.data.as_slice();
        last = u64::from_be_bytes(d[0..8].try_into().unwrap());
        bs = u32::from_be_bytes(d[8..12].try_into().unwrap()) as usize;
    }
    if !matches!(bs, 512 | 1024 | 2048 | 4096) {
        return Err(EINVAL);
    }
    // Write protect from MODE SENSE(6).
    let read_only = t
        .command(&mut io, lun, &[0x1A, 0, 0x3F, 0, 192, 0], Dir::In, 192)
        .ok()
        .filter(|&n| n >= 4)
        .is_some_and(|_| io.data.as_slice()[2] & 0x80 != 0);
    drop(io);
    Ok(UsbDisk {
        t: t.clone(),
        lun,
        sectors: last + 1,
        sector_size: bs,
        read_only,
        model,
    })
}

fn vendor_str(v: &str) -> &str {
    if v.is_empty() { "USB" } else { v }
}
