//! BR/EDR (classic) links: connecting, Secure Simple Pairing (the
//! controller runs it; we answer its questions, asking the user when
//! needed), L2CAP basic-mode channels, the SDP lookup of a HID
//! descriptor, and HID keyboards and mice over HIDP. Bonded devices
//! reconnect by themselves: `incoming` accepts their HID channels.

use super::ctl::Session;
use super::keys::{self, BondEntry};
use super::{Chan, Conn, Hci};
use crate::drivers::input;
use crate::errno::*;
use crate::usb::hid::HidSink;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use bt::hci::{self, Event, op};
use bt::hid as bthid;
use bt::l2cap::{self, Signal};
use bt::{Addr, sdp};
use core::sync::atomic::Ordering;
use usb_desc::hid::ReportDescriptor;

/// Handle the signaling channel (called from the controller's reader).
pub fn on_signal(h: &Arc<Hci>, c: &Arc<Conn>, p: &[u8]) {
    for s in l2cap::parse_signals(p) {
        match s.code {
            l2cap::CONN_REQ => {
                let (psm, scid) = (s.u16_at(0), s.u16_at(2));
                let hid = matches!(psm, l2cap::PSM_HID_CONTROL | l2cap::PSM_HID_INTERRUPT);
                if hid && keys::get(&c.peer).is_some() {
                    let cid = c.alloc_cid();
                    c.chans.lock().insert(
                        cid,
                        Chan {
                            remote: scid,
                            psm,
                            ..Default::default()
                        },
                    );
                    h.send_signal(c, &l2cap::conn_rsp(s.id, cid, scid, l2cap::CONN_SUCCESS));
                    h.send_signal(c, &l2cap::conf_req(c.sig_id(), scid, 672));
                } else {
                    // PSM not supported.
                    h.send_signal(c, &l2cap::conn_rsp(s.id, 0, scid, 0x0002));
                }
            }
            l2cap::CONF_REQ => {
                let dcid = s.u16_at(0);
                let remote = {
                    let mut ch = c.chans.lock();
                    ch.get_mut(&dcid).map(|x| {
                        x.conf_in = true;
                        x.remote
                    })
                };
                match remote {
                    Some(r) => h.send_signal(c, &l2cap::conf_rsp(s.id, r)),
                    None => h.send_signal(c, &l2cap::reject(s.id)),
                }
            }
            l2cap::CONF_RSP => {
                let (scid, result) = (s.u16_at(0), s.u16_at(4));
                if result == 0
                    && let Some(x) = c.chans.lock().get_mut(&scid)
                {
                    x.conf_out = true;
                }
            }
            l2cap::DISCONN_REQ => {
                let (dcid, scid) = (s.u16_at(0), s.u16_at(2));
                c.chans.lock().remove(&dcid);
                h.send_signal(c, &l2cap::disconn_rsp(s.id, dcid, scid));
            }
            l2cap::INFO_REQ => h.send_signal(c, &l2cap::info_rsp(s.id, s.u16_at(0))),
            l2cap::ECHO_REQ => {
                h.send_signal(c, &Signal::new(l2cap::ECHO_RSP, s.id, s.data.clone()))
            }
            _ => {}
        }
        if matches!(
            s.code,
            l2cap::CONN_RSP | l2cap::CONF_RSP | l2cap::DISCONN_RSP | l2cap::COMMAND_REJECT
        ) {
            c.sig.lock().push_back(s);
        }
    }
    c.wq.wake_all();
}

/// Open a channel to `psm`; returns our channel ID once configured.
pub fn open_channel(h: &Hci, c: &Conn, psm: u16) -> KResult<u16> {
    let cid = c.alloc_cid();
    c.chans.lock().insert(
        cid,
        Chan {
            psm,
            ..Default::default()
        },
    );
    let id = c.sig_id();
    h.send_signal(c, &l2cap::conn_req(id, psm, cid));
    // Wait for success (a "pending" response keeps us waiting).
    let dcid = c
        .wait_for(10_000, |c| {
            let mut q = c.sig.lock();
            let i = q.iter().position(|s| s.id == id)?;
            let s = q.remove(i)?;
            match (s.code, s.u16_at(4)) {
                (l2cap::CONN_RSP, l2cap::CONN_PENDING) => None,
                (l2cap::CONN_RSP, l2cap::CONN_SUCCESS) => Some(Ok(s.u16_at(0))),
                _ => Some(Err(ECONNREFUSED)),
            }
        })
        .ok_or(ETIMEDOUT)??;
    if let Some(x) = c.chans.lock().get_mut(&cid) {
        x.remote = dcid;
    }
    h.send_signal(c, &l2cap::conf_req(c.sig_id(), dcid, 672));
    c.wait_for(5000, |c| {
        c.chans
            .lock()
            .get(&cid)
            .is_some_and(|x| x.conf_in && x.conf_out)
            .then_some(())
    })
    .ok_or(ETIMEDOUT)?;
    Ok(cid)
}

pub fn close_channel(h: &Hci, c: &Conn, cid: u16) {
    if let Some(x) = c.chans.lock().remove(&cid) {
        h.send_signal(c, &l2cap::disconn_req(c.sig_id(), x.remote, cid));
    }
}

fn send_on(h: &Hci, c: &Conn, cid: u16, data: &[u8]) {
    let remote = c.chans.lock().get(&cid).map(|x| x.remote);
    if let Some(r) = remote {
        h.send_frame(c, r, data);
    }
}

fn recv_on(c: &Conn, cid: u16, ms: u64) -> Option<Vec<u8>> {
    c.wait_for(ms, |c| c.chans.lock().get_mut(&cid)?.rx.pop_front())
}

/// Connect to a BR/EDR device (or return the existing link).
pub fn connect(h: &Arc<Hci>, a: Addr) -> KResult<Arc<Conn>> {
    if let Some(c) = h.conn_to(&a) {
        return Ok(c);
    }
    let since = h.ev_seq();
    h.command(hci::create_connection(&a))?;
    match h.wait_event(since, 20_000, |e| match e {
        Event::ConnectionComplete {
            status,
            addr,
            handle,
            ..
        } if *addr == a => Some((*status, *handle)),
        _ => None,
    }) {
        Some((0, handle)) => h.conn(handle).ok_or(EIO),
        Some((st, _)) => Err(super::hci_err(st)),
        None => Err(ETIMEDOUT),
    }
}

/// Authenticate the link: Secure Simple Pairing (or the stored link key),
/// answering the controller's questions.
fn authenticate(h: &Arc<Hci>, c: &Conn, s: &Session) -> KResult<()> {
    let a = c.peer;
    let mut since = h.ev_seq();
    h.command(hci::handle_cmd(op::AUTH_REQUESTED, c.handle, &[]))?;
    let mut peer_io = 0xFF;
    enum Step {
        Done(u8),
        Io(u8),
        Confirm(u32),
        Enter,
        Show(u32),
    }
    loop {
        let (seq, step) = h
            .wait_event_seq(since, 60_000, |e| match e {
                Event::AuthComplete { status, handle } if *handle == c.handle => {
                    Some(Step::Done(*status))
                }
                Event::DisconnectionComplete { handle, .. } if *handle == c.handle => {
                    Some(Step::Done(0x08))
                }
                Event::IoCapResponse { addr, io_cap, .. } if *addr == a => Some(Step::Io(*io_cap)),
                Event::UserConfirmRequest { addr, value } if *addr == a => {
                    Some(Step::Confirm(*value))
                }
                Event::UserPasskeyRequest(addr) if *addr == a => Some(Step::Enter),
                Event::UserPasskeyNotification { addr, passkey } if *addr == a => {
                    Some(Step::Show(*passkey))
                }
                _ => None,
            })
            .ok_or(ETIMEDOUT)?;
        since = seq;
        match step {
            Step::Done(0) => return Ok(()),
            Step::Done(st) => return Err(super::hci_err(st)),
            Step::Io(io) => peer_io = io,
            Step::Confirm(v) => {
                // No display or keyboard on the device: Just Works.
                let ok = peer_io == 3
                    || s.ask(&alloc::format!("Does the device show {:06}? [yes/no]", v))
                        .is_some_and(|x| x.trim().eq_ignore_ascii_case("yes") || x.trim() == "y");
                let o = if ok {
                    op::USER_CONFIRM_REPLY
                } else {
                    op::USER_CONFIRM_NEG_REPLY
                };
                h.send_cmd(hci::addr_cmd(o, &a, &[]));
            }
            Step::Enter => match s
                .ask("Passkey shown on the device:")
                .and_then(|x| x.trim().parse::<u32>().ok())
            {
                Some(p) => h.send_cmd(hci::addr_cmd(op::USER_PASSKEY_REPLY, &a, &p.to_le_bytes())),
                None => h.send_cmd(hci::addr_cmd(op::USER_PASSKEY_NEG_REPLY, &a, &[])),
            },
            Step::Show(p) => s.say(&alloc::format!("Type {:06} on the device, then Enter", p)),
        }
    }
}

fn set_encryption(h: &Hci, c: &Conn) -> KResult<()> {
    if c.encrypted.load(Ordering::SeqCst) {
        return Ok(());
    }
    let since = h.ev_seq();
    h.command(hci::handle_cmd(op::SET_CONN_ENCRYPTION, c.handle, &[1]))?;
    match h.wait_event(since, 5000, |e| match e {
        Event::EncryptionChange { status, handle, on } if *handle == c.handle => {
            Some(*status == 0 && *on)
        }
        _ => None,
    }) {
        Some(true) => Ok(()),
        _ => Err(EACCES),
    }
}

/// Pair with (or reconnect to) a BR/EDR device and start its HID.
pub fn pair(h: &Arc<Hci>, a: Addr, s: &Session, pair: bool) -> KResult<()> {
    if pair {
        // Forget the old key so the controller pairs afresh.
        keys::remove(&a);
    }
    s.say(&alloc::format!("connecting to {}...", a));
    let c = connect(h, a)?;
    h.new_link_keys.lock().retain(|(x, _, _)| *x != a);
    if let Err(e) = authenticate(h, &c, s) {
        let _ = h.command(hci::disconnect(c.handle, 0x13));
        s.say("authentication failed");
        return Err(e);
    }
    let name = h
        .found
        .lock()
        .get(&a)
        .and_then(|f| f.info.name.clone())
        .unwrap_or_default();
    if let Some((_, key, kt)) = h
        .new_link_keys
        .lock()
        .iter()
        .rev()
        .find(|(x, _, _)| *x == a)
        .copied()
    {
        keys::put(BondEntry {
            addr: a,
            le: false,
            name: name.clone(),
            link_key: Some(key),
            link_key_type: kt,
            ..Default::default()
        });
        s.say("paired");
    }
    set_encryption(h, &c)?;
    let node = start_hid(h, &c, true, &name)?;
    s.say(&alloc::format!("connected: {} ({})", node, a));
    Ok(())
}

/// A bonded device connected to us: wait for it to open its HID channels.
pub fn incoming(h: &Arc<Hci>, c: &Arc<Conn>) {
    let name = keys::get(&c.peer).map(|b| b.name).unwrap_or_default();
    if let Err(e) = start_hid(h, c, false, &name) {
        crate::println!(
            "[bt] {}: {}: HID reconnect failed: {:?}",
            h.name(),
            c.peer,
            e
        );
    }
}

/// The HID descriptor from the device's SDP record.
fn sdp_descriptor(h: &Hci, c: &Conn) -> Option<Vec<u8>> {
    let cid = open_channel(h, c, l2cap::PSM_SDP).ok()?;
    let mut lists = Vec::new();
    let mut cont: Vec<u8> = Vec::new();
    let mut ok = false;
    for tid in 1..16u16 {
        send_on(h, c, cid, &sdp::search_attr_req(tid, sdp::UUID_HID, &cont));
        let Some(r) = recv_on(c, cid, 5000) else {
            break;
        };
        let Some((part, next)) = sdp::search_attr_rsp(&r) else {
            break;
        };
        lists.extend_from_slice(part);
        if next.is_empty() {
            ok = true;
            break;
        }
        cont = next.to_vec();
    }
    close_channel(h, c, cid);
    if ok {
        sdp::hid_descriptor(&lists)
    } else {
        None
    }
}

/// Wait for (or open) the HID channels, register the input device and
/// start feeding it. Returns the /dev/input node.
fn start_hid(h: &Arc<Hci>, c: &Arc<Conn>, initiate: bool, name: &str) -> KResult<String> {
    let desc = sdp_descriptor(h, c);
    let (ctrl, intr) = if initiate {
        let ctrl = open_channel(h, c, l2cap::PSM_HID_CONTROL)?;
        (ctrl, open_channel(h, c, l2cap::PSM_HID_INTERRUPT)?)
    } else {
        let find = |psm| {
            c.wait_for(10_000, |c| {
                c.chans
                    .lock()
                    .iter()
                    .find(|(_, x)| x.psm == psm && x.conf_in && x.conf_out)
                    .map(|(k, _)| *k)
            })
        };
        (
            find(l2cap::PSM_HID_CONTROL).ok_or(ETIMEDOUT)?,
            find(l2cap::PSM_HID_INTERRUPT).ok_or(ETIMEDOUT)?,
        )
    };
    let boot = desc.is_none();
    let rd = ReportDescriptor::parse(desc.as_deref().unwrap_or(bthid::BOOT_DESCRIPTOR))
        .map_err(|_| EPROTO)?;
    send_on(h, c, ctrl, &bthid::hidp_set_protocol(!boot));
    let info = input::Info::new(
        if name.is_empty() {
            "Bluetooth HID"
        } else {
            name
        },
        &alloc::format!("{}/{}", h.name(), c.peer),
        [input::BUS_BLUETOOTH, 0, 0, 0],
    );
    let info = crate::usb::hid::report_info(info, &rd);
    let what = crate::usb::hid::describe(&rd);
    let ev = input::register(info);
    let node = alloc::format!("/dev/input/event{}", ev.idx);
    crate::println!(
        "[bt] {}: {}: HID {}{} ({})",
        h.name(),
        c.peer,
        what,
        if boot { " (boot protocol)" } else { "" },
        node
    );
    let mut sink = HidSink::new(rd, ev.clone());
    if sink.is_keyboard() {
        let (hw, cw) = (Arc::downgrade(h), Arc::downgrade(c));
        let id = if boot { 1 } else { 0 };
        ev.set_led_sink(alloc::boxed::Box::new(move |bits| {
            if let (Some(h), Some(c)) = (hw.upgrade(), cw.upgrade()) {
                send_on(&h, &c, intr, &bthid::hidp_output(id, &[bits]));
            }
        }));
    }
    let c2 = c.clone();
    crate::sched::spawn("bt-hidp", move || {
        while !c2.is_gone() && c2.chans.lock().contains_key(&intr) {
            let batch: Vec<Vec<u8>> = c2
                .chans
                .lock()
                .get_mut(&intr)
                .map(|x| x.rx.drain(..).collect())
                .unwrap_or_default();
            for p in batch {
                if let Some(r) = bthid::hidp_input(&p) {
                    sink.feed(r);
                }
            }
            sink.tick();
            c2.wq.wait_timeout(30, || c2.is_gone());
        }
        sink.finish();
        input::unregister(&sink.ev);
    });
    Ok(node)
}
