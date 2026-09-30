//! USB Attached SCSI (UAS).
//!
//! A UAS interface (protocol 0x62, usually alternate setting 1 next to
//! Bulk-Only on alternate 0) has four pipes: command (bulk OUT), status
//! (bulk IN), data-in and data-out. Each command carries a tag.
//!
//! - SuperSpeed with bulk streams: the status and data transfers for a
//!   tag run on the stream with the same ID, so the host queues them
//!   before sending the command IU and the device completes them in
//!   whatever order it likes. Several tags are in flight at once (the
//!   block device splits large requests into commands issued together,
//!   see `storage.rs`).
//! - High speed (no streams): one command at a time; the device announces
//!   each data phase with a READ READY or WRITE READY IU on the status
//!   pipe, then sends the sense IU.
//!
//! A command that times out is aborted (ABORT TASK, then LOGICAL UNIT
//! RESET if that fails). The SCSI command set on top is shared with
//! Bulk-Only (`storage.rs`).

use super::UsbDevice;
use super::storage::{self, Dir, Io, Kind, Transport};
use super::xhci::Td;
use crate::errno::*;
use alloc::sync::Arc;
use usb_desc::{CLASS_MASS_STORAGE, Endpoint, Interface, TransferType};

const PROTO_UAS: u8 = 0x62;
const SET_INTERFACE: u8 = 0x0B;
const IU_COMMAND: u8 = 0x01;
const IU_SENSE: u8 = 0x03;
const IU_RESPONSE: u8 = 0x04;
const IU_TASK_MGMT: u8 = 0x05;
const IU_READ_READY: u8 = 0x06;
const IU_WRITE_READY: u8 = 0x07;
const TMF_ABORT_TASK: u8 = 0x01;
const TMF_LU_RESET: u8 = 0x08;
const SENSE_OFF: usize = 128;
const SENSE_LEN: usize = 128;
const TIMEOUT_MS: u64 = 20_000;
/// Tags in flight at most (stream IDs 1..=MAX_TAGS).
const MAX_TAGS: u32 = 31;
/// Commands the block device keeps in flight (tags 2..).
pub(super) const QUEUE_DEPTH: usize = 8;

pub(super) struct Pipes {
    cmd: Endpoint,
    status: Endpoint,
    data_in: Endpoint,
    data_out: Endpoint,
    /// Stream IDs available (tags 1..streams); 0 = no streams (USB 2).
    pub(super) streams: u32,
}

impl Pipes {
    /// Tag for task management (the highest stream).
    fn tm_tag(&self) -> u16 {
        (self.streams.max(2) - 1) as u16
    }

    /// Commands that can be in flight together.
    pub(super) fn depth(&self) -> usize {
        if self.streams < 4 {
            1
        } else {
            QUEUE_DEPTH.min(self.streams as usize - 3)
        }
    }
}

/// Pick the four pipes: by Pipe Usage descriptor, else by order.
fn pipes(iface: &Interface) -> Option<(Endpoint, Endpoint, Endpoint, Endpoint)> {
    let by_id = |id: u8| iface.endpoints.iter().copied().find(|e| e.pipe_id == id);
    if let (Some(c), Some(s), Some(i), Some(o)) = (by_id(1), by_id(2), by_id(3), by_id(4)) {
        return Some((c, s, i, o));
    }
    let bulk = |dir_in: bool| {
        iface
            .endpoints
            .iter()
            .copied()
            .filter(move |e| e.transfer_type() == TransferType::Bulk && e.is_in() == dir_in)
    };
    let mut outs = bulk(false);
    let mut ins = bulk(true);
    Some((outs.next()?, ins.next()?, ins.next()?, outs.next()?))
}

pub fn probe(dev: &Arc<UsbDevice>, iface: &Interface) -> bool {
    if iface.class != CLASS_MASS_STORAGE || iface.subclass != 6 {
        return false;
    }
    // The UAS alternate setting of this interface, if any.
    let alt = if iface.protocol == PROTO_UAS {
        Some(iface.clone())
    } else {
        dev.config.lock().as_ref().and_then(|c| {
            c.interfaces
                .iter()
                .find(|i| i.number == iface.number && i.protocol == PROTO_UAS)
                .cloned()
        })
    };
    let Some(alt) = alt else { return false };
    let Some((cmd, status, data_in, data_out)) = pipes(&alt) else {
        return false;
    };
    let mut streams = status
        .max_streams()
        .min(data_in.max_streams())
        .min(data_out.max_streams())
        .min(MAX_TAGS + 1);
    if dev.speed < super::Speed::Super || !dev.hc.supports_streams() || streams < 2 {
        streams = 0;
    }
    if alt.alternate != 0
        && dev
            .control_out(
                0x01,
                SET_INTERFACE,
                alt.alternate as u16,
                alt.number as u16,
                &[],
            )
            .is_err()
    {
        return false;
    }
    let configured = if streams > 0 {
        dev.configure_endpoints_streams(&[
            (cmd, 0),
            (status, streams),
            (data_in, streams),
            (data_out, streams),
        ])
    } else {
        dev.configure_endpoints(&[cmd, status, data_in, data_out])
    };
    if configured.is_err() {
        crate::println!("[usb] {}: UAS: cannot configure endpoints", dev.name());
        return false;
    }
    let p = Pipes {
        cmd,
        status,
        data_in,
        data_out,
        streams,
    };
    if streams > 0 {
        crate::println!(
            "[usb] {}: USB Attached SCSI, {} streams, {} commands queued",
            dev.name(),
            streams - 1,
            p.depth()
        );
    } else {
        crate::println!(
            "[usb] {}: USB Attached SCSI (no streams: one command at a time)",
            dev.name()
        );
    }
    storage::start(dev, alt.number, Kind::Uas(p))
}

/// A command whose status (and data) transfers are queued.
pub(super) struct Pending {
    tag: u16,
    st: Td,
    data: Option<Td>,
}

fn command_iu(io: &Io, tag: u16, lun: u8, cdb: &[u8]) {
    let c = &io.cbw;
    c.zero();
    c.write::<u8>(0, IU_COMMAND);
    c.write::<u8>(2, (tag >> 8) as u8);
    c.write::<u8>(3, tag as u8);
    c.write::<u8>(9, lun); // single-level LUN
    for (i, b) in cdb.iter().enumerate().take(16) {
        c.write::<u8>(16 + i, *b);
    }
}

/// Queue the status and data transfers of a command on its tag's
/// stream, then send the command IU (streams only).
pub(super) fn issue(
    t: &Transport,
    p: &Pipes,
    io: &mut Io,
    lun: u8,
    cdb: &[u8],
    dir: Dir,
    len: usize,
) -> KResult<Pending> {
    let dev = &t.dev;
    let tag = io.tag as u16;
    io.last_sense = None;
    command_iu(io, tag, lun, cdb);
    let st = dev.submit(&p.status, tag, &io.cbw, SENSE_OFF, SENSE_LEN)?;
    let data = match dir {
        Dir::In if len > 0 => Some(dev.submit(&p.data_in, tag, &io.data, 0, len)?),
        Dir::Out if len > 0 => Some(dev.submit(&p.data_out, tag, &io.data, 0, len)?),
        _ => None,
    };
    let sent = dev
        .submit(&p.cmd, 0, &io.cbw, 0, 32)
        .and_then(|td| dev.wait(&td, Some(TIMEOUT_MS)));
    if let Err(e) = sent {
        dev.cancel(&st);
        if let Some(d) = &data {
            dev.cancel(d);
        }
        return Err(e);
    }
    Ok(Pending { tag, st, data })
}

/// Wait for a command issued with [`issue`]: bytes moved, or the error
/// (sense kept in `io.last_sense`).
pub(super) fn finish(t: &Transport, p: &Pipes, io: &mut Io, c: Pending, lun: u8) -> KResult<usize> {
    let dev = &t.dev;
    let status = dev.wait(&c.st, Some(TIMEOUT_MS));
    if status == Err(ETIMEDOUT) {
        dev.cancel(&c.st);
        if let Some(d) = &c.data {
            dev.cancel(d);
        }
        abort(t, p, io, c.tag, lun);
        return Err(ETIMEDOUT);
    }
    let moved = match &c.data {
        // The data phase completes before the status (or never, when the
        // command failed early).
        Some(d) => dev
            .wait(d, Some(if status.is_ok() { 1000 } else { 1 }))
            .unwrap_or_default(),
        None => 0,
    };
    let n = status?;
    sense_iu(io, c.tag, n, moved)
}

/// Interpret the IU the status pipe returned (`n` bytes at SENSE_OFF).
fn sense_iu(io: &mut Io, tag: u16, n: usize, moved: usize) -> KResult<usize> {
    let s = io.cbw.as_slice();
    let iu = &s[SENSE_OFF..SENSE_OFF + n.min(SENSE_LEN)];
    if iu.len() < 8 || u16::from_be_bytes([iu[2], iu[3]]) != tag {
        return Err(EIO);
    }
    match iu[0] {
        IU_SENSE => {
            let scsi_status = iu[6];
            if scsi_status == 0 {
                return Ok(moved);
            }
            let slen = if iu.len() >= 16 {
                u16::from_be_bytes([iu[14], iu[15]]) as usize
            } else {
                0
            };
            let sense = &iu[16.min(iu.len())..(16 + slen).min(iu.len())];
            if sense.len() >= 14 {
                io.last_sense = Some((sense[2] & 0x0F, sense[12], sense[13]));
            }
            Err(EIO)
        }
        _ => Err(EIO),
    }
}

/// Send a task management function; true if the device completed it.
fn task_mgmt(t: &Transport, p: &Pipes, io: &mut Io, func: u8, task: u16, lun: u8) -> bool {
    let dev = &t.dev;
    let tag = p.tm_tag();
    let c = &io.cbw;
    c.zero();
    c.write::<u8>(0, IU_TASK_MGMT);
    c.write::<u8>(2, (tag >> 8) as u8);
    c.write::<u8>(3, tag as u8);
    c.write::<u8>(4, func);
    c.write::<u8>(6, (task >> 8) as u8);
    c.write::<u8>(7, task as u8);
    c.write::<u8>(9, lun);
    let sid = if p.streams > 0 { tag } else { 0 };
    let Ok(st) = dev.submit(&p.status, sid, &io.cbw, SENSE_OFF, SENSE_LEN) else {
        return false;
    };
    let sent = dev
        .submit(&p.cmd, 0, &io.cbw, 0, 16)
        .and_then(|td| dev.wait(&td, Some(2000)));
    let r = sent.and_then(|_| dev.wait(&st, Some(5000)));
    if r.is_err() {
        dev.cancel(&st);
        return false;
    }
    let iu = &io.cbw.as_slice()[SENSE_OFF..SENSE_OFF + 8];
    // RESPONSE IU: code 0 (complete) or 8 (succeeded).
    iu[0] == IU_RESPONSE && matches!(iu[7], 0 | 8)
}

/// Abort a timed-out command; reset the logical unit if that fails.
fn abort(t: &Transport, p: &Pipes, io: &mut Io, tag: u16, lun: u8) {
    if task_mgmt(t, p, io, TMF_ABORT_TASK, tag, lun) {
        crate::println!("[usb] {}: UAS: aborted command {}", t.dev.name(), tag);
        return;
    }
    let ok = task_mgmt(t, p, io, TMF_LU_RESET, 0, lun);
    crate::println!(
        "[usb] {}: UAS: command {} timed out; logical unit reset {}",
        t.dev.name(),
        tag,
        if ok { "done" } else { "failed" }
    );
}

/// Run one SCSI command over UAS (see `Transport::command`).
pub(super) fn command(
    t: &Transport,
    p: &Pipes,
    io: &mut Io,
    lun: u8,
    cdb: &[u8],
    dir: Dir,
    len: usize,
) -> KResult<usize> {
    if p.streams == 0 {
        return command_nostream(t, p, io, lun, cdb, dir, len);
    }
    let c = issue(t, p, io, lun, cdb, dir, len)?;
    finish(t, p, io, c, lun)
}

/// USB 2 UAS: command IU, then status IUs announce the data phase
/// (READ/WRITE READY) and finally carry the sense.
fn command_nostream(
    t: &Transport,
    p: &Pipes,
    io: &mut Io,
    lun: u8,
    cdb: &[u8],
    dir: Dir,
    len: usize,
) -> KResult<usize> {
    let dev = &t.dev;
    let tag = io.tag as u16;
    io.last_sense = None;
    command_iu(io, tag, lun, cdb);
    let st = dev.submit(&p.status, 0, &io.cbw, SENSE_OFF, SENSE_LEN)?;
    let sent = dev
        .submit(&p.cmd, 0, &io.cbw, 0, 32)
        .and_then(|td| dev.wait(&td, Some(TIMEOUT_MS)));
    if let Err(e) = sent {
        dev.cancel(&st);
        return Err(e);
    }
    let mut st = st;
    let mut moved = 0;
    for _ in 0..4 {
        let n = match dev.wait(&st, Some(TIMEOUT_MS)) {
            Ok(n) => n,
            Err(ETIMEDOUT) => {
                dev.cancel(&st);
                abort(t, p, io, tag, lun);
                return Err(ETIMEDOUT);
            }
            Err(e) => return Err(e),
        };
        let iu0 = io.cbw.as_slice()[SENSE_OFF];
        match iu0 {
            IU_READ_READY | IU_WRITE_READY => {
                // Queue the next status before moving the data.
                st = dev.submit(&p.status, 0, &io.cbw, SENSE_OFF, SENSE_LEN)?;
                let ep = if iu0 == IU_READ_READY {
                    &p.data_in
                } else {
                    &p.data_out
                };
                if dir != Dir::None && len > 0 {
                    moved = dev.transfer(ep, &io.data, len, Some(TIMEOUT_MS))?;
                }
            }
            _ => return sense_iu(io, tag, n, moved),
        }
    }
    Err(EIO)
}
