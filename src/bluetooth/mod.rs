//! Bluetooth: HCI controllers (USB and H4 UART transports), the host
//! stack glue around `crates/bt` (L2CAP, SMP, GATT, SDP), HID over GATT
//! and BR/EDR HID input devices, bonding keys, and the `/dev/bluetooth`
//! control device used by `bt`.
//!
//! Each controller (`Hci`) receives packets from its transport's reader
//! thread in `on_packet`, which never blocks: replies the stack must send
//! from there (LTK/link key/IO capability replies, ATT confirmations) are
//! queued and sent as the controller grants command and buffer credits.
//! Commands that wait for their completion, connection set-up, pairing
//! and HID run in other threads.

pub mod bredr;
pub mod ctl;
pub mod h4;
pub mod keys;
pub mod le;

use crate::errno::*;
use crate::sched::WaitQueue;
use crate::sync::Mutex;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use bt::adv::{self, AdInfo};
use bt::hci::{self, Event, op};
use bt::l2cap::{self, Signal};
use bt::{Addr, AddrType, att, smp};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// Sends HCI packets to a controller.
pub trait Transport: Send + Sync {
    /// Send one packet of H4 type `kind` (command or ACL data).
    fn send(&self, kind: u8, pkt: &[u8]) -> KResult<()>;
    fn name(&self) -> String;
    /// Vendor set-up before the standard initialisation (firmware).
    fn setup(&self, _hci: &Arc<Hci>) -> KResult<()> {
        Ok(())
    }
    fn is_gone(&self) -> bool {
        false
    }
}

/// A device seen while scanning.
#[derive(Clone, Debug, Default)]
pub struct Found {
    pub addr_type: AddrType,
    pub le: bool,
    pub info: AdInfo,
    pub class: u32,
    pub rssi: i8,
    pub seen: u64,
}

impl Found {
    pub fn kind(&self) -> &'static str {
        if self.le {
            self.info.appearance.map_or("", adv::appearance_name)
        } else {
            adv::class_name(self.class)
        }
    }
}

/// A logical channel on a BR/EDR link.
#[derive(Default)]
pub struct Chan {
    pub remote: u16,
    pub psm: u16,
    /// Our configuration request was accepted.
    pub conf_out: bool,
    /// We accepted the peer's configuration request.
    pub conf_in: bool,
    pub rx: VecDeque<Vec<u8>>,
}

/// An ACL connection.
pub struct Conn {
    pub handle: u16,
    pub peer: Addr,
    pub peer_type: AddrType,
    pub le: bool,
    pub encrypted: AtomicBool,
    pub gone: AtomicBool,
    /// ATT responses (to our requests).
    pub att_rsp: Mutex<VecDeque<Vec<u8>>>,
    /// ATT notifications and indications: (handle, value).
    pub att_ntf: Mutex<VecDeque<(u16, Vec<u8>)>>,
    pub smp: Mutex<VecDeque<Vec<u8>>>,
    /// Signaling responses (BR/EDR).
    pub sig: Mutex<VecDeque<Signal>>,
    pub chans: Mutex<BTreeMap<u16, Chan>>,
    /// Woken when any queue above changes or the link drops.
    pub wq: WaitQueue,
    pub security_requested: AtomicBool,
    next_id: AtomicUsize,
    next_cid: AtomicUsize,
}

impl Conn {
    fn new(handle: u16, peer: Addr, peer_type: AddrType, le: bool) -> Conn {
        Conn {
            handle,
            peer,
            peer_type,
            le,
            encrypted: AtomicBool::new(false),
            gone: AtomicBool::new(false),
            att_rsp: Mutex::new(VecDeque::new()),
            att_ntf: Mutex::new(VecDeque::new()),
            smp: Mutex::new(VecDeque::new()),
            sig: Mutex::new(VecDeque::new()),
            chans: Mutex::new(BTreeMap::new()),
            wq: WaitQueue::new(),
            security_requested: AtomicBool::new(false),
            next_id: AtomicUsize::new(1),
            next_cid: AtomicUsize::new(l2cap::CID_DYN_START as usize),
        }
    }

    pub fn is_gone(&self) -> bool {
        self.gone.load(Ordering::Relaxed)
    }

    /// Next signaling identifier (1..=255).
    pub fn sig_id(&self) -> u8 {
        (self.next_id.fetch_add(1, Ordering::Relaxed) % 255 + 1) as u8
    }

    pub fn alloc_cid(&self) -> u16 {
        self.next_cid.fetch_add(1, Ordering::Relaxed) as u16
    }

    /// Wait until `f` yields something, the link drops, or the time runs out.
    pub fn wait_for<T>(&self, ms: u64, mut f: impl FnMut(&Conn) -> Option<T>) -> Option<T> {
        let deadline = crate::time::Deadline::after_ms(ms);
        loop {
            if let Some(v) = f(self) {
                return Some(v);
            }
            if self.is_gone() || deadline.expired() {
                return None;
            }
            self.wq.wait_timeout(20, || self.is_gone());
        }
    }
}

/// Controller facts from initialisation.
#[derive(Clone, Debug, Default)]
pub struct Info {
    pub addr: Addr,
    pub version: u8,
    pub manufacturer: u16,
    pub le: bool,
    pub bredr: bool,
    pub acl_mtu: usize,
    pub le_mtu: usize,
    pub up: bool,
}

pub struct Hci {
    pub index: usize,
    pub tr: Arc<dyn Transport>,
    pub info: Mutex<Info>,
    /// One synchronous command at a time.
    cmd_lock: Mutex<()>,
    /// The synchronous command's result: (opcode, event).
    cmd_wait: Mutex<Option<(u16, Option<Event>)>>,
    /// Commands waiting for a command credit.
    cmd_queue: Mutex<VecDeque<Vec<u8>>>,
    cmd_credits: AtomicUsize,
    cmd_sent_at: AtomicU64,
    /// ACL packets waiting for buffer credits: (le, packet).
    acl_queue: Mutex<VecDeque<(bool, Vec<u8>)>>,
    acl_credits: AtomicUsize,
    le_credits: AtomicUsize,
    /// Asynchronous events for waiters, numbered.
    events: Mutex<VecDeque<(u64, Event)>>,
    ev_seq: AtomicU64,
    pub wq: WaitQueue,
    reasm: Mutex<l2cap::Reassembler>,
    pub conns: Mutex<BTreeMap<u16, Arc<Conn>>>,
    pub found: Mutex<BTreeMap<Addr, Found>>,
    /// Link keys reported during BR/EDR pairing, until saved.
    pub new_link_keys: Mutex<Vec<(Addr, [u8; 16], u8)>>,
    /// The `bt` session pairing right now (prompts go to it).
    pub session: Mutex<Option<Arc<ctl::Session>>>,
    /// Background (passive) scanning to reconnect bonded LE devices.
    pub background: AtomicBool,
    /// Devices a (re)connection is in progress for.
    pub connecting: Mutex<Vec<Addr>>,
    pub gone: AtomicBool,
}

static HCIS: Mutex<Vec<Arc<Hci>>> = Mutex::new(Vec::new());
static NEXT_INDEX: AtomicUsize = AtomicUsize::new(0);

/// The controllers (powered or not).
pub fn controllers() -> Vec<Arc<Hci>> {
    HCIS.lock().clone()
}

pub fn first() -> Option<Arc<Hci>> {
    HCIS.lock()
        .iter()
        .find(|h| !h.gone.load(Ordering::Relaxed))
        .cloned()
}

/// Register a controller and bring it up in the background.
pub fn register(tr: Arc<dyn Transport>) -> Arc<Hci> {
    let hci = Arc::new(Hci {
        index: NEXT_INDEX.fetch_add(1, Ordering::Relaxed),
        tr,
        info: Mutex::new(Info::default()),
        cmd_lock: Mutex::new(()),
        cmd_wait: Mutex::new(None),
        cmd_queue: Mutex::new(VecDeque::new()),
        cmd_credits: AtomicUsize::new(1),
        cmd_sent_at: AtomicU64::new(0),
        acl_queue: Mutex::new(VecDeque::new()),
        acl_credits: AtomicUsize::new(0),
        le_credits: AtomicUsize::new(0),
        events: Mutex::new(VecDeque::new()),
        ev_seq: AtomicU64::new(0),
        wq: WaitQueue::new(),
        reasm: Mutex::new(l2cap::Reassembler::default()),
        conns: Mutex::new(BTreeMap::new()),
        found: Mutex::new(BTreeMap::new()),
        new_link_keys: Mutex::new(Vec::new()),
        session: Mutex::new(None),
        background: AtomicBool::new(false),
        connecting: Mutex::new(Vec::new()),
        gone: AtomicBool::new(false),
    });
    HCIS.lock().push(hci.clone());
    ctl::register_node();
    let h = hci.clone();
    crate::sched::spawn(&alloc::format!("hci{}-init", hci.index), move || {
        if let Err(e) = h.power_on() {
            crate::println!("[bt] hci{}: initialisation failed: {:?}", h.index, e);
        }
    });
    hci
}

/// The transport went away (USB unplug).
pub fn unregister(hci: &Arc<Hci>) {
    hci.gone.store(true, Ordering::SeqCst);
    for c in hci.conns.lock().values() {
        c.gone.store(true, Ordering::SeqCst);
        c.wq.wake_all();
    }
    hci.wq.wake_all();
    HCIS.lock().retain(|h| !Arc::ptr_eq(h, hci));
    crate::println!("[bt] hci{}: removed", hci.index);
}

/// Create /dev/bluetooth and start H4 controllers named by
/// `bt.h4=com2[,...]` (kernel.conf).
pub fn late_init() {
    ctl::register_node();
    if let Some(v) = crate::params::get("bt.h4") {
        for port in v.split(',') {
            if let Err(e) = h4::attach(port.trim()) {
                crate::println!("[bt] h4 {}: {:?}", port, e);
            }
        }
    }
}

impl Hci {
    pub fn name(&self) -> String {
        alloc::format!("hci{}", self.index)
    }

    // ---- sending ----

    fn pump_cmds(&self) {
        let mut q = self.cmd_queue.lock();
        // A controller that never answered: assume the credit came back.
        let now = crate::time::millis();
        if self.cmd_credits.load(Ordering::Relaxed) == 0
            && now - self.cmd_sent_at.load(Ordering::Relaxed) > 2000
        {
            self.cmd_credits.store(1, Ordering::Relaxed);
        }
        while self.cmd_credits.load(Ordering::Relaxed) > 0 {
            let Some(c) = q.pop_front() else { break };
            self.cmd_credits.fetch_sub(1, Ordering::Relaxed);
            self.cmd_sent_at.store(now, Ordering::Relaxed);
            let _ = self.tr.send(hci::H4_CMD, &c);
        }
    }

    /// Queue a command whose completion nobody waits for.
    pub fn send_cmd(&self, c: Vec<u8>) {
        self.cmd_queue.lock().push_back(c);
        self.pump_cmds();
    }

    /// Run a command and return its Command Complete parameters (or, for
    /// commands answered by Command Status, an empty vector on success).
    pub fn command(&self, c: Vec<u8>) -> KResult<Vec<u8>> {
        self.command_timeout(c, 3000)
    }

    pub fn command_timeout(&self, c: Vec<u8>, ms: u64) -> KResult<Vec<u8>> {
        if self.gone.load(Ordering::Relaxed) {
            return Err(ENODEV);
        }
        let opcode = u16::from_le_bytes([c[0], c[1]]);
        let _g = self.cmd_lock.lock();
        *self.cmd_wait.lock() = Some((opcode, None));
        self.send_cmd(c);
        let deadline = crate::time::Deadline::after_ms(ms);
        let ev = loop {
            if let Some((_, Some(e))) = self.cmd_wait.lock().as_ref() {
                break Some(e.clone());
            }
            if deadline.expired() || self.gone.load(Ordering::Relaxed) {
                break None;
            }
            self.pump_cmds();
            self.wq.wait_timeout(10, || {
                matches!(self.cmd_wait.lock().as_ref(), Some((_, Some(_))))
            });
        };
        *self.cmd_wait.lock() = None;
        match ev {
            Some(Event::CommandComplete { ret, .. }) => Ok(ret),
            Some(Event::CommandStatus { status: 0, .. }) => Ok(Vec::new()),
            Some(Event::CommandStatus { status, .. }) => Err(hci_err(status)),
            _ => Err(ETIMEDOUT),
        }
    }

    /// A command whose Command Complete starts with a status byte.
    pub fn command_ok(&self, c: Vec<u8>) -> KResult<Vec<u8>> {
        let r = self.command(c)?;
        match r.first() {
            Some(0) | None => Ok(r),
            Some(&s) => Err(hci_err(s)),
        }
    }

    fn pump_acl(&self) {
        let mut q = self.acl_queue.lock();
        while let Some((le, _)) = q.front() {
            let credits = if *le && self.info.lock().le_mtu > 0 {
                &self.le_credits
            } else {
                &self.acl_credits
            };
            if credits.load(Ordering::Relaxed) == 0 {
                break;
            }
            credits.fetch_sub(1, Ordering::Relaxed);
            let (_, p) = q.pop_front().unwrap();
            let _ = self.tr.send(hci::H4_ACL, &p);
        }
    }

    /// Queue an L2CAP frame on a connection.
    pub fn send_frame(&self, conn: &Conn, cid: u16, payload: &[u8]) {
        let f = l2cap::frame(cid, payload);
        let mtu = {
            let i = self.info.lock();
            if conn.le && i.le_mtu > 0 {
                i.le_mtu
            } else {
                i.acl_mtu
            }
        };
        {
            let mut q = self.acl_queue.lock();
            for p in hci::acl_packets(conn.handle, &f, mtu.max(23), conn.le) {
                q.push_back((conn.le, p));
            }
        }
        self.pump_acl();
    }

    pub fn send_signal(&self, conn: &Conn, s: &Signal) {
        let cid = if conn.le {
            l2cap::CID_LE_SIGNALING
        } else {
            l2cap::CID_SIGNALING
        };
        self.send_frame(conn, cid, &s.encode());
    }

    // ---- events for waiters ----

    pub fn ev_seq(&self) -> u64 {
        self.ev_seq.load(Ordering::SeqCst)
    }

    fn post(&self, e: Event) {
        let n = self.ev_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let mut q = self.events.lock();
        q.push_back((n, e));
        while q.len() > 64 {
            q.pop_front();
        }
        drop(q);
        self.wq.wake_all();
    }

    /// Wait for an event posted after `since` that `f` accepts.
    pub fn wait_event<T>(
        &self,
        since: u64,
        ms: u64,
        f: impl FnMut(&Event) -> Option<T>,
    ) -> Option<T> {
        self.wait_event_seq(since, ms, f).map(|(_, v)| v)
    }

    /// Like `wait_event`, also returning the event's number (to continue
    /// from).
    pub fn wait_event_seq<T>(
        &self,
        since: u64,
        ms: u64,
        mut f: impl FnMut(&Event) -> Option<T>,
    ) -> Option<(u64, T)> {
        let deadline = crate::time::Deadline::after_ms(ms);
        let mut seen = since;
        loop {
            {
                let q = self.events.lock();
                for (n, e) in q.iter() {
                    if *n > seen {
                        seen = *n;
                        if let Some(v) = f(e) {
                            return Some((seen, v));
                        }
                    }
                }
            }
            if deadline.expired() || self.gone.load(Ordering::Relaxed) {
                return None;
            }
            self.wq.wait_timeout(20, || self.ev_seq() > seen);
        }
    }

    pub fn conn(&self, handle: u16) -> Option<Arc<Conn>> {
        self.conns.lock().get(&handle).cloned()
    }

    pub fn conn_to(&self, a: &Addr) -> Option<Arc<Conn>> {
        self.conns.lock().values().find(|c| c.peer == *a).cloned()
    }

    // ---- receiving ----

    /// A packet from the controller (called by the transport's reader).
    pub fn on_packet(self: &Arc<Self>, kind: u8, pkt: &[u8]) {
        match kind {
            hci::H4_EVENT => {
                if let Some(e) = Event::parse(pkt) {
                    self.on_event(e);
                }
                if pkt.first() == Some(&0x0E) || pkt.first() == Some(&0x0F) {
                    let ncmd = if pkt[0] == 0x0E {
                        pkt.get(2)
                    } else {
                        pkt.get(3)
                    };
                    self.cmd_credits
                        .store(ncmd.copied().unwrap_or(1) as usize, Ordering::Relaxed);
                }
                self.pump_cmds();
            }
            hci::H4_ACL => {
                if let Some((handle, pb, data)) = hci::parse_acl(pkt) {
                    let f = self.reasm.lock().feed(handle, pb, data);
                    if let Some((cid, payload)) = f
                        && let Some(c) = self.conn(handle)
                    {
                        self.on_frame(&c, cid, payload);
                    }
                }
            }
            _ => {}
        }
    }

    fn on_event(self: &Arc<Self>, e: Event) {
        match &e {
            Event::CommandComplete { opcode, .. } | Event::CommandStatus { opcode, .. } => {
                let mut w = self.cmd_wait.lock();
                if let Some((op, slot @ None)) = w.as_mut()
                    && op == opcode
                {
                    *slot = Some(e.clone());
                    drop(w);
                    self.wq.wake_all();
                }
                return;
            }
            Event::CompletedPackets(v) => {
                let le = self.info.lock().le_mtu > 0;
                for (h, n) in v {
                    let is_le = self.conn(*h).is_some_and(|c| c.le);
                    let c = if is_le && le {
                        &self.le_credits
                    } else {
                        &self.acl_credits
                    };
                    c.fetch_add(*n as usize, Ordering::Relaxed);
                }
                self.pump_acl();
                return;
            }
            Event::LeAdvertising(reports) => {
                for r in reports {
                    self.on_adv(r);
                }
                return;
            }
            Event::Inquiry(v) => {
                let now = crate::time::millis();
                let mut f = self.found.lock();
                for r in v {
                    let e = f.entry(r.addr).or_default();
                    e.le = false;
                    e.addr_type = AddrType::Public;
                    e.class = r.class;
                    e.rssi = r.rssi.unwrap_or(0);
                    e.seen = now;
                    if !r.eir.is_empty() {
                        e.info.merge(adv::parse(&r.eir));
                    }
                }
                return;
            }
            Event::RemoteName {
                status: 0,
                addr,
                name,
            } => {
                if let Some(f) = self.found.lock().get_mut(addr) {
                    f.info.name = Some(name.clone());
                }
            }
            Event::LeConnectionComplete {
                status: 0,
                handle,
                peer_type,
                peer,
                ..
            } => {
                self.conns.lock().insert(
                    *handle,
                    Arc::new(Conn::new(*handle, *peer, *peer_type, true)),
                );
            }
            Event::ConnectionComplete {
                status: 0,
                handle,
                addr,
                acl: true,
            } => {
                let c = Arc::new(Conn::new(*handle, *addr, AddrType::Public, false));
                self.conns.lock().insert(*handle, c.clone());
                // A bonded HID device reconnecting to us.
                if !self.connecting.lock().contains(addr) && keys::get(addr).is_some() {
                    let h = self.clone();
                    crate::sched::spawn("bt-hidp", move || bredr::incoming(&h, &c));
                }
            }
            Event::DisconnectionComplete { handle, reason } => {
                if let Some(c) = self.conns.lock().remove(handle) {
                    c.gone.store(true, Ordering::SeqCst);
                    c.wq.wake_all();
                    crate::println!(
                        "[bt] {}: {} disconnected (reason {:#04x})",
                        self.name(),
                        c.peer,
                        reason
                    );
                }
                self.reasm.lock().drop_handle(*handle);
            }
            Event::EncryptionChange { status, handle, on } => {
                if let Some(c) = self.conn(*handle) {
                    c.encrypted.store(*status == 0 && *on, Ordering::SeqCst);
                    c.wq.wake_all();
                }
            }
            Event::LeLtkRequest { handle, .. } => {
                // We are always the central: never asked for keys.
                self.send_cmd(hci::handle_cmd(op::LE_LTK_NEG_REPLY, *handle, &[]));
            }
            Event::LeRemoteConnParamRequest {
                handle,
                min,
                max,
                latency,
                timeout,
            } => {
                self.send_cmd(hci::le_remote_conn_param_reply(
                    *handle, *min, *max, *latency, *timeout,
                ));
            }
            Event::ConnectionRequest { addr, acl, .. } => {
                if *acl && keys::get(addr).is_some() {
                    self.send_cmd(hci::addr_cmd(op::ACCEPT_CONNECTION, addr, &[0x00]));
                } else {
                    self.send_cmd(hci::addr_cmd(op::REJECT_CONNECTION, addr, &[0x0F]));
                }
            }
            Event::LinkKeyRequest(a) => match keys::get(a).and_then(|b| b.link_key) {
                Some(k) => self.send_cmd(hci::addr_cmd(op::LINK_KEY_REPLY, a, &k)),
                None => self.send_cmd(hci::addr_cmd(op::LINK_KEY_NEG_REPLY, a, &[])),
            },
            Event::LinkKeyNotification {
                addr,
                key,
                key_type,
            } => {
                self.new_link_keys.lock().push((*addr, *key, *key_type));
            }
            Event::PinCodeRequest(a) => {
                self.send_cmd(hci::addr_cmd(op::PIN_CODE_NEG_REPLY, a, &[]));
            }
            Event::IoCapRequest(a) => {
                // Display with yes/no; MITM, general bonding.
                self.send_cmd(hci::addr_cmd(op::IO_CAP_REPLY, a, &[0x01, 0x00, 0x05]));
            }
            // Otherwise handled by the pairing thread (it may need the
            // user); outside an interactive pairing, refuse.
            Event::UserConfirmRequest { addr, .. } if self.session.lock().is_none() => {
                self.send_cmd(hci::addr_cmd(op::USER_CONFIRM_NEG_REPLY, addr, &[]));
            }
            Event::UserPasskeyRequest(addr) if self.session.lock().is_none() => {
                self.send_cmd(hci::addr_cmd(op::USER_PASSKEY_NEG_REPLY, addr, &[]));
            }
            Event::HardwareError(code) => {
                crate::println!("[bt] {}: hardware error {:#04x}", self.name(), code);
            }
            _ => {}
        }
        self.post(e);
    }

    fn on_adv(self: &Arc<Self>, r: &hci::AdvReport) {
        let now = crate::time::millis();
        {
            let mut f = self.found.lock();
            let e = f.entry(r.addr).or_default();
            e.le = true;
            e.addr_type = r.addr_type;
            e.rssi = r.rssi;
            e.seen = now;
            e.info.merge(adv::parse(&r.data));
        }
        // Connectable advertising from a bonded device: reconnect.
        let connectable = matches!(r.event_type, 0x00 | 0x01) || r.event_type & 0x01 != 0;
        if connectable
            && self.background.load(Ordering::Relaxed)
            && let Some(b) = keys::resolve(&r.addr)
            && b.le
            && self.conn_to(&r.addr).is_none()
            && !self.connecting.lock().contains(&r.addr)
        {
            self.connecting.lock().push(r.addr);
            let h = self.clone();
            let (a, t) = (r.addr, r.addr_type);
            crate::sched::spawn("bt-reconnect", move || {
                le::reconnect(&h, a, t);
                h.connecting.lock().retain(|x| *x != a);
            });
        }
    }

    fn on_frame(self: &Arc<Self>, c: &Arc<Conn>, cid: u16, p: Vec<u8>) {
        match cid {
            l2cap::CID_ATT if c.le => self.on_att(c, p),
            l2cap::CID_SMP if c.le => {
                if p.first() == Some(&smp::SECURITY_REQUEST) {
                    c.security_requested.store(true, Ordering::SeqCst);
                }
                c.smp.lock().push_back(p);
                c.wq.wake_all();
            }
            l2cap::CID_LE_SIGNALING if c.le => {
                for s in l2cap::parse_signals(&p) {
                    match s.code {
                        l2cap::CONN_PARAM_UPDATE_REQ if s.data.len() >= 8 => {
                            self.send_signal(c, &l2cap::conn_param_update_rsp(s.id, true));
                            let mut prm = c.handle.to_le_bytes().to_vec();
                            prm.extend_from_slice(&s.data[..8]);
                            prm.extend_from_slice(&[0, 0, 0, 0]);
                            self.send_cmd(hci::cmd(op::LE_CONN_UPDATE, &prm));
                        }
                        l2cap::COMMAND_REJECT | l2cap::CONN_PARAM_UPDATE_RSP => {}
                        _ => self.send_signal(c, &l2cap::reject(s.id)),
                    }
                }
            }
            l2cap::CID_SIGNALING if !c.le => bredr::on_signal(self, c, &p),
            cid if cid >= l2cap::CID_DYN_START => {
                if let Some(ch) = c.chans.lock().get_mut(&cid) {
                    ch.rx.push_back(p);
                }
                c.wq.wake_all();
            }
            _ => {}
        }
    }

    fn on_att(self: &Arc<Self>, c: &Arc<Conn>, p: Vec<u8>) {
        let Some(&opc) = p.first() else { return };
        match opc {
            att::NOTIFY | att::INDICATE => {
                if opc == att::INDICATE {
                    self.send_frame(c, l2cap::CID_ATT, &[att::CONFIRM]);
                }
                if let Some((h, v)) = bt::gatt::notification(&p) {
                    let mut q = c.att_ntf.lock();
                    if q.len() < 256 {
                        q.push_back((h, v.to_vec()));
                    }
                }
            }
            // Requests from the peer to our (empty) GATT server.
            att::MTU_REQ => {
                let mut r = alloc::vec![att::MTU_RSP];
                r.extend_from_slice(&att::DEFAULT_MTU.to_le_bytes());
                self.send_frame(c, l2cap::CID_ATT, &r);
                return;
            }
            att::FIND_INFO_REQ
            | 0x06
            | att::READ_BY_TYPE_REQ
            | att::READ_BY_GROUP_REQ
            | att::READ_REQ
            | att::READ_BLOB_REQ => {
                let h = u16::from_le_bytes([*p.get(1).unwrap_or(&0), *p.get(2).unwrap_or(&0)]);
                let r = att::error_rsp(opc, h, att::ERR_ATTRIBUTE_NOT_FOUND);
                self.send_frame(c, l2cap::CID_ATT, &r);
                return;
            }
            att::WRITE_REQ | 0x16 | 0x18 => {
                let r = att::error_rsp(opc, 0, att::ERR_REQUEST_NOT_SUPPORTED);
                self.send_frame(c, l2cap::CID_ATT, &r);
                return;
            }
            att::WRITE_CMD | 0xD2 | att::CONFIRM => return,
            _ => c.att_rsp.lock().push_back(p),
        }
        c.wq.wake_all();
    }

    // ---- bring-up ----

    /// Initialise the controller.
    pub fn power_on(self: &Arc<Self>) -> KResult<()> {
        self.tr.setup(self)?;
        self.command_ok(hci::cmd(op::RESET, &[]))?;
        let ver = self.command_ok(hci::cmd(op::READ_LOCAL_VERSION, &[]))?;
        let addr = hci::bd_addr(&self.command_ok(hci::cmd(op::READ_BD_ADDR, &[]))?).ok_or(EIO)?;
        let feat = self.command_ok(hci::cmd(op::READ_LOCAL_FEATURES, &[]))?;
        let (version, manufacturer) = hci::local_version(&ver).unwrap_or((0, 0));
        // Features byte 4: bit 6 LE supported, bit 5 BR/EDR not supported.
        let f4 = feat.get(5).copied().unwrap_or(0);
        let (le, bredr) = (f4 & 0x40 != 0, f4 & 0x20 == 0);
        let (acl_mtu, acl_pkts) =
            hci::buffer_size(&self.command_ok(hci::cmd(op::READ_BUFFER_SIZE, &[]))?)
                .unwrap_or((27, 1));
        let (le_mtu, le_pkts) = if le {
            self.command_ok(hci::cmd(op::LE_READ_BUFFER_SIZE, &[]))
                .ok()
                .and_then(|r| hci::le_buffer_size(&r))
                .unwrap_or((0, 0))
        } else {
            (0, 0)
        };
        self.acl_credits
            .store(acl_pkts.max(1) as usize, Ordering::Relaxed);
        self.le_credits.store(le_pkts as usize, Ordering::Relaxed);
        *self.info.lock() = Info {
            addr,
            version,
            manufacturer,
            le,
            bredr,
            acl_mtu: acl_mtu as usize,
            le_mtu: le_mtu as usize,
            up: false,
        };
        let _ = self.command_ok(hci::set_event_mask());
        if le {
            let _ = self.command_ok(hci::le_set_event_mask());
        }
        if bredr {
            let _ = self.command_ok(hci::cmd(op::WRITE_SIMPLE_PAIRING_MODE, &[1]));
            let _ = self.command_ok(hci::cmd(op::WRITE_SC_HOST_SUPPORT, &[1]));
            let _ = self.command_ok(hci::cmd(op::WRITE_INQUIRY_MODE, &[2]));
            let _ = self.command_ok(hci::cmd(op::WRITE_CLASS_OF_DEVICE, &[0x0C, 0x01, 0x00]));
            let mut name = [0u8; 248];
            name[..6].copy_from_slice(b"RustOS");
            let _ = self.command_ok(hci::cmd(op::WRITE_LOCAL_NAME, &name));
            if le {
                let _ = self.command_ok(hci::cmd(op::WRITE_LE_HOST_SUPPORT, &[1, 0]));
            }
            // Page scan: bonded keyboards and mice reconnect to us.
            let _ = self.command_ok(hci::cmd(op::WRITE_SCAN_ENABLE, &[0x02]));
        }
        self.info.lock().up = true;
        crate::println!(
            "[bt] {}: {} ({}), Bluetooth {}, manufacturer {:#06x}{}{}",
            self.name(),
            addr,
            self.tr.name(),
            hci::version_name(version),
            manufacturer,
            if le { ", LE" } else { "" },
            if bredr { ", BR/EDR" } else { "" },
        );
        keys::load();
        self.update_background();
        Ok(())
    }

    /// Power off: disconnect everything and stop scanning.
    pub fn power_off(&self) {
        self.set_scan(false, false);
        self.background.store(false, Ordering::Relaxed);
        let handles: Vec<u16> = self.conns.lock().keys().copied().collect();
        for h in handles {
            let _ = self.command(hci::disconnect(h, 0x15));
        }
        let _ = self.command_ok(hci::cmd(op::WRITE_SCAN_ENABLE, &[0]));
        self.info.lock().up = false;
    }

    /// LE scanning on or off (active scanning asks for scan responses).
    pub fn set_scan(&self, on: bool, active: bool) {
        if !self.info.lock().le {
            return;
        }
        let _ = self.command_ok(hci::le_set_scan_enable(false));
        if on {
            let _ = self.command_ok(hci::le_set_scan_params(active));
            let _ = self.command_ok(hci::le_set_scan_enable(true));
        }
    }

    /// Passive background scanning while bonded LE devices are not
    /// connected.
    pub fn update_background(&self) {
        let want = self.info.lock().up
            && keys::all()
                .iter()
                .any(|b| b.le && self.conn_to(&b.addr).is_none());
        let was = self.background.swap(want, Ordering::SeqCst);
        if want || was {
            self.set_scan(want, false);
        }
    }
}

/// Map an HCI status code to an errno.
pub fn hci_err(status: u8) -> Errno {
    match status {
        0x02 => ENOENT,        // unknown connection
        0x04 => EHOSTDOWN,     // page timeout
        0x05 | 0x06 => EACCES, // authentication failure / key missing
        0x08 | 0x22 => ETIMEDOUT,
        0x0C => EBUSY, // command disallowed
        0x01 | 0x11 => EOPNOTSUPP,
        _ => EIO,
    }
}
