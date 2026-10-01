//! AF_UNIX sockets: stream, datagram and seqpacket, bound to filesystem
//! paths (a socket node is created where the filesystem allows) or to the
//! abstract namespace, with `socketpair`, `SCM_RIGHTS` file passing,
//! `SCM_CREDENTIALS` and `SO_PEERCRED`.

use super::generic::{Ancillary, Creds, GenericSocket, Received};
use crate::errno::*;
use crate::sched::WaitQueue;
use crate::sync::Mutex;
use crate::vfs::{self, FileLike, FileType, Metadata, POLLHUP, POLLIN, POLLOUT};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};

pub const AF_UNIX: u16 = 1;
pub const SOCK_STREAM: u32 = 1;
pub const SOCK_DGRAM: u32 = 2;
pub const SOCK_SEQPACKET: u32 = 5;

const SOL_SOCKET: u32 = 1;
const SO_PASSCRED: u32 = 16;
const SO_PEERCRED: u32 = 17;

/// Bytes queued for a stream reader before writers block.
const STREAM_LIMIT: usize = 256 * 1024;
/// Datagrams queued before senders block.
const DGRAM_LIMIT: usize = 512;

/// A socket address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnixAddr {
    Unnamed,
    /// An absolute filesystem path.
    Path(String),
    Abstract(Vec<u8>),
}

impl UnixAddr {
    fn key(&self) -> Option<Vec<u8>> {
        match self {
            UnixAddr::Unnamed => None,
            UnixAddr::Path(p) => Some([b"P".as_slice(), p.as_bytes()].concat()),
            UnixAddr::Abstract(n) => Some([b"A".as_slice(), n].concat()),
        }
    }

    /// Parse a `sockaddr_un` of `raw.len()` bytes.
    pub fn parse(raw: &[u8]) -> KResult<UnixAddr> {
        if raw.len() < 2 || u16::from_ne_bytes([raw[0], raw[1]]) != AF_UNIX {
            return Err(EINVAL);
        }
        let path = &raw[2..raw.len().min(2 + 108)];
        if path.is_empty() {
            return Ok(UnixAddr::Unnamed);
        }
        if path[0] == 0 {
            return Ok(UnixAddr::Abstract(path[1..].to_vec()));
        }
        let end = path.iter().position(|&b| b == 0).unwrap_or(path.len());
        let p = core::str::from_utf8(&path[..end]).map_err(|_| EINVAL)?;
        let cwd = crate::process::current()
            .map(|p| p.cwd.lock().clone())
            .unwrap_or_else(|| String::from("/"));
        Ok(UnixAddr::Path(vfs::absolute(&cwd, p)))
    }

    /// Encode as a `sockaddr_un`.
    pub fn encode(&self) -> Vec<u8> {
        let mut v = AF_UNIX.to_ne_bytes().to_vec();
        match self {
            UnixAddr::Unnamed => {}
            UnixAddr::Path(p) => {
                v.extend_from_slice(p.as_bytes());
                v.push(0);
            }
            UnixAddr::Abstract(n) => {
                v.push(0);
                v.extend_from_slice(n);
            }
        }
        v
    }
}

/// Bound sockets by address.
static NAMES: Mutex<BTreeMap<Vec<u8>, Weak<UnixSocket>>> = Mutex::new(BTreeMap::new());
static AUTOBIND: AtomicU64 = AtomicU64::new(1);

struct Msg {
    data: Vec<u8>,
    /// Bytes of `data` already read (streams).
    off: usize,
    from: UnixAddr,
    anc: Ancillary,
}

enum State {
    Unconnected,
    Listening {
        backlog: usize,
        pending: VecDeque<Arc<UnixSocket>>,
    },
    Connected(Weak<UnixSocket>),
}

struct Inner {
    local: UnixAddr,
    state: State,
    /// Default destination of a connected datagram socket.
    dgram_peer: Option<UnixAddr>,
    rx: VecDeque<Msg>,
    rx_bytes: usize,
    read_shutdown: bool,
    write_shutdown: bool,
    /// The connected peer is gone (or shut down writing).
    peer_closed: bool,
    peer_creds: Option<Creds>,
    passcred: bool,
}

pub struct UnixSocket {
    ty: u32,
    me: Weak<UnixSocket>,
    inner: Mutex<Inner>,
    wq: WaitQueue,
    creds: Creds,
}

fn interrupted() -> bool {
    crate::process::signal::has_pending()
}

impl UnixSocket {
    pub fn new(ty: u32) -> Arc<UnixSocket> {
        Arc::new_cyclic(|me| UnixSocket {
            ty,
            me: me.clone(),
            inner: Mutex::new(Inner {
                local: UnixAddr::Unnamed,
                state: State::Unconnected,
                dgram_peer: None,
                rx: VecDeque::new(),
                rx_bytes: 0,
                read_shutdown: false,
                write_shutdown: false,
                peer_closed: false,
                peer_creds: None,
                passcred: false,
            }),
            wq: WaitQueue::new(),
            creds: Creds::current(),
        })
    }

    /// Two connected sockets (`socketpair`).
    pub fn pair(ty: u32) -> (Arc<UnixSocket>, Arc<UnixSocket>) {
        let (a, b) = (UnixSocket::new(ty), UnixSocket::new(ty));
        {
            let mut ia = a.inner.lock();
            ia.state = State::Connected(Arc::downgrade(&b));
            ia.peer_creds = Some(b.creds);
        }
        {
            let mut ib = b.inner.lock();
            ib.state = State::Connected(Arc::downgrade(&a));
            ib.peer_creds = Some(a.creds);
        }
        (a, b)
    }

    fn is_stream_like(&self) -> bool {
        self.ty != SOCK_DGRAM
    }

    fn peer(&self) -> Option<Arc<UnixSocket>> {
        match &self.inner.lock().state {
            State::Connected(p) => p.upgrade(),
            _ => None,
        }
    }

    fn lookup(addr: &UnixAddr) -> KResult<Arc<UnixSocket>> {
        let key = addr.key().ok_or(EINVAL)?;
        if let Some(s) = NAMES.lock().get(&key).and_then(|w| w.upgrade()) {
            return Ok(s);
        }
        match addr {
            UnixAddr::Path(p) if vfs::lookup(p).is_ok() => Err(ECONNREFUSED),
            UnixAddr::Path(_) => Err(ENOENT),
            _ => Err(ECONNREFUSED),
        }
    }

    /// Queue a message for this socket (the receiving side).
    fn deliver(&self, msg: Msg) {
        let mut i = self.inner.lock();
        i.rx_bytes += msg.data.len();
        i.rx.push_back(msg);
        drop(i);
        self.wq.wake_all();
    }

    fn has_space(&self) -> bool {
        let i = self.inner.lock();
        if self.is_stream_like() {
            i.rx_bytes < STREAM_LIMIT
        } else {
            i.rx.len() < DGRAM_LIMIT
        }
    }

    fn add_creds(&self, anc: &mut Ancillary, target: &UnixSocket) {
        if anc.creds.is_none() && target.inner.lock().passcred {
            anc.creds = Some(Creds::current());
        }
    }
}

impl Drop for UnixSocket {
    fn drop(&mut self) {
        let i = self.inner.get_mut();
        if let Some(key) = i.local.key() {
            let mut names = NAMES.lock();
            if names
                .get(&key)
                .is_some_and(|w| w.ptr_eq(&self.me) || w.upgrade().is_none())
            {
                names.remove(&key);
            }
        }
        if let State::Connected(p) = &i.state
            && let Some(peer) = p.upgrade()
        {
            peer.inner.lock().peer_closed = true;
            peer.wq.wake_all();
        }
    }
}

impl GenericSocket for UnixSocket {
    fn sock_type(&self) -> u32 {
        self.ty
    }

    fn family(&self) -> u16 {
        AF_UNIX
    }

    fn bind(&self, raw: &[u8]) -> KResult<()> {
        let mut addr = UnixAddr::parse(raw)?;
        if self.inner.lock().local != UnixAddr::Unnamed {
            return Err(EINVAL);
        }
        if addr == UnixAddr::Unnamed {
            // Autobind: a fresh abstract name, as Linux does.
            let n = AUTOBIND.fetch_add(1, Ordering::Relaxed);
            addr = UnixAddr::Abstract(alloc::format!("{n:05x}").into_bytes());
        }
        let key = addr.key().ok_or(EINVAL)?;
        if let UnixAddr::Path(p) = &addr {
            let (dir, name) = vfs::lookup_parent(p)?;
            match dir.create(&name, FileType::Socket, 0o777) {
                Ok(_) => {}
                Err(e) if e == EEXIST => return Err(EADDRINUSE),
                // Filesystems without socket nodes: the name lives only
                // in the socket table.
                Err(_) => {}
            }
        }
        let mut names = NAMES.lock();
        if !matches!(addr, UnixAddr::Path(_))
            && names.get(&key).is_some_and(|w| w.upgrade().is_some())
        {
            return Err(EADDRINUSE);
        }
        names.insert(key, self.me.clone());
        self.inner.lock().local = addr;
        Ok(())
    }

    fn listen(&self, backlog: usize) -> KResult<()> {
        if !self.is_stream_like() {
            return Err(EOPNOTSUPP);
        }
        let mut i = self.inner.lock();
        match &mut i.state {
            State::Listening { backlog: b, .. } => *b = backlog.clamp(1, 4096),
            State::Unconnected => {
                if i.local == UnixAddr::Unnamed {
                    return Err(EINVAL);
                }
                i.state = State::Listening {
                    backlog: backlog.clamp(1, 4096),
                    pending: VecDeque::new(),
                };
            }
            State::Connected(_) => return Err(EINVAL),
        }
        Ok(())
    }

    fn connect(&self, raw: &[u8], nonblock: bool) -> KResult<()> {
        let addr = UnixAddr::parse(raw)?;
        if !self.is_stream_like() {
            // Datagram: just the default destination.
            Self::lookup(&addr)?;
            self.inner.lock().dgram_peer = Some(addr);
            return Ok(());
        }
        if matches!(self.inner.lock().state, State::Connected(_)) {
            return Err(EISCONN);
        }
        let server = Self::lookup(&addr)?;
        if server.ty != self.ty {
            return Err(EPROTOTYPE);
        }
        loop {
            {
                let mut si = server.inner.lock();
                let State::Listening { backlog, pending } = &mut si.state else {
                    return Err(ECONNREFUSED);
                };
                if pending.len() < *backlog {
                    let child = UnixSocket::new(self.ty);
                    {
                        let mut ci = child.inner.lock();
                        ci.local = addr.clone();
                        ci.state = State::Connected(self.me.clone());
                        ci.peer_creds = Some(self.creds);
                    }
                    {
                        let mut mi = self.inner.lock();
                        mi.state = State::Connected(Arc::downgrade(&child));
                        mi.peer_creds = Some(server.creds);
                    }
                    pending.push_back(child);
                    drop(si);
                    server.wq.wake_all();
                    return Ok(());
                }
            }
            if nonblock {
                return Err(EAGAIN);
            }
            let s = server.clone();
            if !server
                .wq
                .wait_interruptible(|| match &s.inner.lock().state {
                    State::Listening { backlog, pending } => pending.len() < *backlog,
                    _ => true,
                })
                && interrupted()
            {
                return Err(EINTR);
            }
        }
    }

    fn accept(&self, nonblock: bool) -> KResult<(Arc<dyn FileLike>, Vec<u8>)> {
        loop {
            {
                let mut i = self.inner.lock();
                let State::Listening { pending, .. } = &mut i.state else {
                    return Err(EINVAL);
                };
                if let Some(child) = pending.pop_front() {
                    drop(i);
                    // Connecting clients may wait for backlog space.
                    self.wq.wake_all();
                    return Ok((child, UnixAddr::Unnamed.encode()));
                }
            }
            if nonblock {
                return Err(EAGAIN);
            }
            if !self
                .wq
                .wait_interruptible(|| match &self.inner.lock().state {
                    State::Listening { pending, .. } => !pending.is_empty(),
                    _ => true,
                })
                && interrupted()
            {
                return Err(EINTR);
            }
        }
    }

    fn send(
        &self,
        data: &[u8],
        dest: Option<&[u8]>,
        mut anc: Ancillary,
        nonblock: bool,
    ) -> KResult<usize> {
        if self.inner.lock().write_shutdown {
            return Err(EPIPE);
        }
        let target = if self.is_stream_like() {
            if dest.is_some() {
                return Err(EISCONN);
            }
            match &self.inner.lock().state {
                State::Connected(p) => p.upgrade().ok_or(EPIPE)?,
                _ => return Err(ENOTCONN),
            }
        } else {
            let addr = match dest {
                Some(raw) => UnixAddr::parse(raw)?,
                None => self.inner.lock().dgram_peer.clone().ok_or(ENOTCONN)?,
            };
            let t = Self::lookup(&addr)?;
            if t.ty != SOCK_DGRAM {
                return Err(EPROTOTYPE);
            }
            t
        };
        if self.ty == SOCK_DGRAM && data.len() > 212_992 {
            return Err(EMSGSIZE);
        }
        loop {
            if target.inner.lock().read_shutdown {
                return Err(EPIPE);
            }
            if target.has_space() {
                break;
            }
            if nonblock {
                return Err(EAGAIN);
            }
            let t = target.clone();
            if !target
                .wq
                .wait_interruptible(|| t.has_space() || t.inner.lock().read_shutdown)
                && interrupted()
            {
                return Err(EINTR);
            }
        }
        self.add_creds(&mut anc, &target);
        let from = self.inner.lock().local.clone();
        target.deliver(Msg {
            data: data.to_vec(),
            off: 0,
            from,
            anc,
        });
        Ok(data.len())
    }

    fn recv(&self, buf: &mut [u8], nonblock: bool, peek: bool) -> KResult<Received> {
        loop {
            {
                let mut i = self.inner.lock();
                if !i.rx.is_empty() {
                    let r = if self.ty == SOCK_STREAM {
                        stream_take(&mut i, buf, peek)
                    } else {
                        // One message per call (datagram, seqpacket).
                        let m = i.rx.front().unwrap();
                        let n = m.data.len().min(buf.len());
                        buf[..n].copy_from_slice(&m.data[..n]);
                        let r = Received {
                            len: n,
                            full_len: m.data.len(),
                            from: Some(m.from.encode()),
                            ancillary: m.anc.clone(),
                        };
                        if !peek {
                            let m = i.rx.pop_front().unwrap();
                            i.rx_bytes -= m.data.len();
                        }
                        r
                    };
                    drop(i);
                    // Writers may be waiting for space: on this queue
                    // (blocked in send) or on their own (in poll).
                    self.wq.wake_all();
                    if let Some(p) = self.peer() {
                        p.wq.wake_all();
                    }
                    return Ok(r);
                }
                let eof = i.read_shutdown
                    || (self.is_stream_like()
                        && (i.peer_closed
                            || matches!(&i.state, State::Connected(p) if p.upgrade().is_none())));
                if eof {
                    return Ok(Received {
                        len: 0,
                        full_len: 0,
                        from: None,
                        ancillary: Ancillary::default(),
                    });
                }
                if self.is_stream_like()
                    && matches!(i.state, State::Unconnected | State::Listening { .. })
                {
                    return Err(ENOTCONN);
                }
            }
            if nonblock {
                return Err(EAGAIN);
            }
            if !self.wq.wait_interruptible(|| {
                let i = self.inner.lock();
                !i.rx.is_empty() || i.read_shutdown || i.peer_closed
            }) && interrupted()
            {
                return Err(EINTR);
            }
        }
    }

    fn shutdown(&self, how: u32) -> KResult<()> {
        {
            let mut i = self.inner.lock();
            if how == 0 || how == 2 {
                i.read_shutdown = true;
            }
            if how == 1 || how == 2 {
                i.write_shutdown = true;
            }
        }
        if (how == 1 || how == 2)
            && let Some(p) = self.peer()
        {
            p.inner.lock().peer_closed = true;
            p.wq.wake_all();
        }
        self.wq.wake_all();
        Ok(())
    }

    fn sockname(&self) -> Vec<u8> {
        self.inner.lock().local.encode()
    }

    fn peername(&self) -> KResult<Vec<u8>> {
        if let Some(p) = self.peer() {
            return Ok(p.inner.lock().local.encode());
        }
        match &self.inner.lock().dgram_peer {
            Some(a) => Ok(a.encode()),
            None => Err(ENOTCONN),
        }
    }

    fn setsockopt(&self, level: u32, name: u32, val: &[u8]) -> KResult<()> {
        if level == SOL_SOCKET && name == SO_PASSCRED {
            self.inner.lock().passcred = val.first().is_some_and(|&b| b != 0);
        }
        Ok(())
    }

    fn getsockopt(&self, level: u32, name: u32) -> KResult<Vec<u8>> {
        if level == SOL_SOCKET && name == SO_PEERCRED {
            let c = self.peer_creds().unwrap_or(Creds {
                pid: 0,
                uid: u32::MAX,
                gid: u32::MAX,
            });
            return Ok([
                c.pid.to_ne_bytes(),
                c.uid.to_ne_bytes(),
                c.gid.to_ne_bytes(),
            ]
            .concat());
        }
        if level == SOL_SOCKET && name == SO_PASSCRED {
            return Ok((self.inner.lock().passcred as i32).to_ne_bytes().to_vec());
        }
        Err(ENOPROTOOPT)
    }

    fn peer_creds(&self) -> Option<Creds> {
        self.inner.lock().peer_creds
    }
}

/// Read up to `buf.len()` bytes from a stream's queue, stopping before a
/// message with control data unless it is the first one read.
fn stream_take(i: &mut Inner, buf: &mut [u8], peek: bool) -> Received {
    let mut n = 0;
    let mut anc = Ancillary::default();
    let mut idx = 0;
    let mut first = true;
    while n < buf.len() && idx < i.rx.len() {
        let m = &mut i.rx[idx];
        if !first && !m.anc.is_empty() {
            break;
        }
        if first {
            anc = core::mem::take(&mut m.anc);
            if peek {
                m.anc = anc.clone();
            }
        }
        first = false;
        let avail = &m.data[m.off..];
        let take = avail.len().min(buf.len() - n);
        buf[n..n + take].copy_from_slice(&avail[..take]);
        n += take;
        if peek {
            idx += 1;
            continue;
        }
        m.off += take;
        if m.off == m.data.len() {
            let len = m.data.len();
            i.rx.pop_front();
            i.rx_bytes -= len;
        } else {
            break;
        }
    }
    Received {
        len: n,
        full_len: n,
        from: None,
        ancillary: anc,
    }
}

impl FileLike for UnixSocket {
    fn read(&self, buf: &mut [u8], nonblock: bool) -> KResult<usize> {
        Ok(self.recv(buf, nonblock, false)?.len)
    }

    fn write(&self, buf: &[u8], nonblock: bool) -> KResult<usize> {
        self.send(buf, None, Ancillary::default(), nonblock)
    }

    fn poll(&self) -> u16 {
        let i = self.inner.lock();
        let mut ev = 0;
        let peer_gone =
            i.peer_closed || matches!(&i.state, State::Connected(p) if p.upgrade().is_none());
        match &i.state {
            State::Listening { pending, .. } => {
                if !pending.is_empty() {
                    ev |= POLLIN;
                }
            }
            State::Connected(p) => {
                if !i.rx.is_empty() || peer_gone || i.read_shutdown {
                    ev |= POLLIN;
                }
                if peer_gone {
                    ev |= POLLHUP;
                } else if let Some(peer) = p.upgrade() {
                    drop(i);
                    if peer.has_space() {
                        ev |= POLLOUT;
                    }
                    return ev;
                }
            }
            State::Unconnected => {
                if !i.rx.is_empty() {
                    ev |= POLLIN;
                }
                ev |= POLLOUT;
            }
        }
        ev
    }

    fn wait_queue(&self) -> &WaitQueue {
        &self.wq
    }

    /// Interface ioctls work on any socket, as on Linux.
    fn ioctl(&self, cmd: u64, arg: u64) -> KResult<i64> {
        super::syscalls::if_ioctl(cmd, arg)
    }

    fn stat(&self) -> KResult<Metadata> {
        Ok(Metadata::new(FileType::Socket, 0o777))
    }

    fn as_socket(&self) -> Option<&dyn GenericSocket> {
        Some(self)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
