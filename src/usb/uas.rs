//! USB Attached SCSI (UAS) over SuperSpeed bulk streams.
//!
//! A UAS interface (protocol 0x62, usually alternate setting 1 next to
//! Bulk-Only on alternate 0) has four pipes: command (bulk OUT), status
//! (bulk IN), data-in and data-out. Each command carries a tag; the status
//! and data transfers for it run on the bulk stream with the same ID, so
//! the host queues them before sending the command IU and the device
//! completes them in whatever order it likes. The SCSI command set on top
//! is shared with Bulk-Only (`storage.rs`). Without SuperSpeed streams the
//! device is driven through Bulk-Only instead when it offers it.

use super::UsbDevice;
use super::storage::{self, Dir, Io, Kind, Transport};
use crate::errno::*;
use alloc::sync::Arc;
use usb_desc::{CLASS_MASS_STORAGE, Endpoint, Interface, TransferType};

const PROTO_UAS: u8 = 0x62;
const SET_INTERFACE: u8 = 0x0B;
const IU_COMMAND: u8 = 0x01;
const IU_SENSE: u8 = 0x03;
const IU_RESPONSE: u8 = 0x04;
const SENSE_OFF: usize = 128;
const SENSE_LEN: usize = 128;
const TIMEOUT_MS: u64 = 20_000;
/// Tags in flight at most (stream IDs 1..=MAX_TAGS).
const MAX_TAGS: u32 = 31;

pub(super) struct Pipes {
    cmd: Endpoint,
    status: Endpoint,
    data_in: Endpoint,
    data_out: Endpoint,
    /// Stream IDs available (tags 1..streams).
    streams: u32,
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
    let streams = status
        .max_streams()
        .min(data_in.max_streams())
        .min(data_out.max_streams())
        .min(MAX_TAGS + 1);
    if dev.speed < super::Speed::Super || !dev.hc.supports_streams() || streams < 2 {
        if iface.protocol == PROTO_UAS {
            crate::println!(
                "[usb] {}: UAS device without SuperSpeed bulk streams is not supported",
                dev.name()
            );
        }
        return false; // Bulk-Only (alternate 0) takes it, if offered.
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
    if dev
        .configure_endpoints_streams(&[
            (cmd, 0),
            (status, streams),
            (data_in, streams),
            (data_out, streams),
        ])
        .is_err()
    {
        crate::println!(
            "[usb] {}: UAS: cannot configure stream endpoints",
            dev.name()
        );
        return false;
    }
    crate::println!(
        "[usb] {}: USB Attached SCSI, {} streams",
        dev.name(),
        streams - 1
    );
    storage::start(
        dev,
        alt.number,
        Kind::Uas(Pipes {
            cmd,
            status,
            data_in,
            data_out,
            streams,
        }),
    )
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
    let dev = &t.dev;
    io.tag = io.tag % (p.streams - 1) + 1;
    let tag = io.tag as u16;
    io.last_sense = None;
    // Command IU.
    let c = &io.cbw;
    c.zero();
    c.write::<u8>(0, IU_COMMAND);
    c.write::<u8>(2, (tag >> 8) as u8);
    c.write::<u8>(3, tag as u8);
    c.write::<u8>(9, lun); // single-level LUN
    for (i, b) in cdb.iter().enumerate().take(16) {
        c.write::<u8>(16 + i, *b);
    }
    // Queue status and data on the tag's stream, then send the command.
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
    let status = dev.wait(&st, Some(TIMEOUT_MS));
    let moved = match &data {
        // The data phase completes before the status (or never, when the
        // command failed early).
        Some(d) => dev
            .wait(d, Some(if status.is_ok() { 1000 } else { 1 }))
            .unwrap_or_default(),
        None => 0,
    };
    let n = status?;
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
        IU_RESPONSE => Err(EIO),
        _ => Err(EIO),
    }
}
