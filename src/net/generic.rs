//! Sockets of families other than IP (AF_UNIX, AF_NETLINK, AF_PACKET): a
//! common interface the socket system calls use, with addresses as raw
//! `sockaddr` bytes and ancillary data (`SCM_RIGHTS`, `SCM_CREDENTIALS`).

use crate::errno::*;
use crate::vfs::File;
use alloc::sync::Arc;
use alloc::vec::Vec;

/// Process credentials as `struct ucred` carries them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Creds {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
}

impl Creds {
    /// The calling process's.
    pub fn current() -> Creds {
        use core::sync::atomic::Ordering;
        crate::process::current()
            .map(|p| Creds {
                pid: p.pid,
                uid: p.uid.load(Ordering::Relaxed),
                gid: p.gid.load(Ordering::Relaxed),
            })
            .unwrap_or_default()
    }
}

/// Control messages travelling with data.
#[derive(Clone, Default)]
pub struct Ancillary {
    /// `SCM_RIGHTS`: open files passed along.
    pub files: Vec<Arc<File>>,
    /// `SCM_CREDENTIALS`: the sender's credentials.
    pub creds: Option<Creds>,
}

impl Ancillary {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.creds.is_none()
    }
}

/// What a receive returns.
pub struct Received {
    /// Bytes copied into the buffer.
    pub len: usize,
    /// The full length of the message (datagram sockets; for MSG_TRUNC).
    pub full_len: usize,
    /// Source address (raw sockaddr), if the family reports one.
    pub from: Option<Vec<u8>>,
    pub ancillary: Ancillary,
}

/// A socket of a non-IP family.
pub trait GenericSocket: Send + Sync {
    /// `SOCK_STREAM`, `SOCK_DGRAM`, ...
    fn sock_type(&self) -> u32;
    fn family(&self) -> u16;
    fn bind(&self, addr: &[u8]) -> KResult<()>;
    fn listen(&self, _backlog: usize) -> KResult<()> {
        Err(EOPNOTSUPP)
    }
    fn connect(&self, addr: &[u8], nonblock: bool) -> KResult<()>;
    /// A new connection and its peer address.
    fn accept(&self, _nonblock: bool) -> KResult<(Arc<dyn crate::vfs::FileLike>, Vec<u8>)> {
        Err(EOPNOTSUPP)
    }
    fn send(
        &self,
        data: &[u8],
        dest: Option<&[u8]>,
        anc: Ancillary,
        nonblock: bool,
    ) -> KResult<usize>;
    fn recv(&self, buf: &mut [u8], nonblock: bool, peek: bool) -> KResult<Received>;
    fn shutdown(&self, _how: u32) -> KResult<()> {
        Ok(())
    }
    fn sockname(&self) -> Vec<u8>;
    fn peername(&self) -> KResult<Vec<u8>> {
        Err(ENOTCONN)
    }
    fn setsockopt(&self, _level: u32, _name: u32, _val: &[u8]) -> KResult<()> {
        Ok(())
    }
    fn getsockopt(&self, _level: u32, _name: u32) -> KResult<Vec<u8>> {
        Err(ENOPROTOOPT)
    }
    /// `SO_PEERCRED`.
    fn peer_creds(&self) -> Option<Creds> {
        None
    }
}
