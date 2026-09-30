//! A GATT client: service, characteristic and descriptor discovery, reads
//! (long values included), writes and notifications, over any `Bearer`
//! that can carry one ATT request and return its response.

use crate::att::{self, Uuid};
use crate::le16;
use alloc::vec::Vec;

pub const PRIMARY_SERVICE: u16 = 0x2800;
pub const CHARACTERISTIC: u16 = 0x2803;
pub const CCCD: u16 = 0x2902;

pub const PROP_READ: u8 = 0x02;
pub const PROP_WRITE_NO_RSP: u8 = 0x04;
pub const PROP_WRITE: u8 = 0x08;
pub const PROP_NOTIFY: u8 = 0x10;
pub const PROP_INDICATE: u8 = 0x20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// ATT error response: (request opcode, handle, error code).
    Att(u8, u16, u8),
    /// No response, or the link went away.
    Timeout,
    /// A response that does not parse.
    Protocol,
}

impl Error {
    /// The peer wants an encrypted (or authenticated) link first.
    pub fn needs_security(&self) -> bool {
        matches!(
            self,
            Error::Att(
                _,
                _,
                att::ERR_INSUFFICIENT_AUTHENTICATION | att::ERR_INSUFFICIENT_ENCRYPTION
            )
        )
    }
}

/// Carries ATT PDUs to the server.
pub trait Bearer {
    /// Send a request and return its response PDU (not a notification).
    fn request(&mut self, pdu: &[u8]) -> Result<Vec<u8>, Error>;
    /// Send a PDU that has no response (Write Command).
    fn send(&mut self, pdu: &[u8]) -> Result<(), Error>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Service {
    pub start: u16,
    pub end: u16,
    pub uuid: Uuid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Characteristic {
    pub decl: u16,
    pub props: u8,
    pub value: u16,
    pub uuid: Uuid,
    /// Last handle belonging to it (before the next declaration).
    pub end: u16,
}

pub struct Client<'a, B: Bearer> {
    pub bearer: &'a mut B,
    pub mtu: u16,
}

fn check(rsp: Vec<u8>, want: u8) -> Result<Vec<u8>, Error> {
    match rsp.first() {
        Some(&op) if op == want => Ok(rsp),
        Some(&att::ERROR_RSP) if rsp.len() >= 5 => {
            Err(Error::Att(rsp[1], le16(&rsp, 2).unwrap_or(0), rsp[4]))
        }
        _ => Err(Error::Protocol),
    }
}

impl<'a, B: Bearer> Client<'a, B> {
    pub fn new(bearer: &'a mut B) -> Self {
        Client {
            bearer,
            mtu: att::DEFAULT_MTU,
        }
    }

    fn req(&mut self, pdu: &[u8], want: u8) -> Result<Vec<u8>, Error> {
        let r = self.bearer.request(pdu)?;
        check(r, want)
    }

    /// Exchange MTU: returns the MTU in use.
    pub fn exchange_mtu(&mut self, ours: u16) -> Result<u16, Error> {
        match self.req(&att::mtu_req(ours), att::MTU_RSP) {
            Ok(r) => {
                let theirs = le16(&r, 1).ok_or(Error::Protocol)?;
                self.mtu = ours.min(theirs).max(att::DEFAULT_MTU);
            }
            Err(Error::Att(_, _, att::ERR_REQUEST_NOT_SUPPORTED)) => {}
            Err(e) => return Err(e),
        }
        Ok(self.mtu)
    }

    /// Run a ranged discovery until the server reports no more
    /// attributes; `f` returns the last handle each response covered.
    fn ranged(
        &mut self,
        start: u16,
        end: u16,
        mut make: impl FnMut(u16, u16) -> Vec<u8>,
        want: u8,
        mut f: impl FnMut(&[u8]) -> Option<u16>,
    ) -> Result<(), Error> {
        let mut s = start;
        while s <= end && s != 0 {
            let r = match self.req(&make(s, end), want) {
                Ok(r) => r,
                Err(Error::Att(_, _, att::ERR_ATTRIBUTE_NOT_FOUND)) => break,
                Err(e) => return Err(e),
            };
            let last = f(&r).ok_or(Error::Protocol)?;
            if last >= end || last < s {
                break;
            }
            s = last + 1;
        }
        Ok(())
    }

    pub fn services(&mut self) -> Result<Vec<Service>, Error> {
        let mut out = Vec::new();
        self.ranged(
            1,
            0xFFFF,
            |s, e| att::read_by_group_req(s, e, Uuid::U16(PRIMARY_SERVICE)),
            att::READ_BY_GROUP_RSP,
            |r| {
                let mut last = None;
                for (h, rest) in att::entries(r) {
                    let end = le16(rest, 0)?;
                    out.push(Service {
                        start: h,
                        end,
                        uuid: Uuid::from_bytes(&rest[2..])?,
                    });
                    last = Some(end);
                }
                last
            },
        )?;
        Ok(out)
    }

    pub fn characteristics(&mut self, svc: &Service) -> Result<Vec<Characteristic>, Error> {
        let mut out: Vec<Characteristic> = Vec::new();
        self.ranged(
            svc.start,
            svc.end,
            |s, e| att::read_by_type_req(s, e, Uuid::U16(CHARACTERISTIC)),
            att::READ_BY_TYPE_RSP,
            |r| {
                let mut last = None;
                for (h, rest) in att::entries(r) {
                    out.push(Characteristic {
                        decl: h,
                        props: *rest.first()?,
                        value: le16(rest, 1)?,
                        uuid: Uuid::from_bytes(rest.get(3..)?)?,
                        end: svc.end,
                    });
                    last = Some(h);
                }
                last
            },
        )?;
        for i in 1..out.len() {
            out[i - 1].end = out[i].decl - 1;
        }
        Ok(out)
    }

    /// Descriptors of a characteristic: (handle, type).
    pub fn descriptors(&mut self, c: &Characteristic) -> Result<Vec<(u16, Uuid)>, Error> {
        let mut out = Vec::new();
        if c.value >= c.end {
            return Ok(out);
        }
        self.ranged(
            c.value + 1,
            c.end,
            att::find_info_req,
            att::FIND_INFO_RSP,
            |r| {
                let mut last = None;
                for (h, rest) in att::entries(r) {
                    out.push((h, Uuid::from_bytes(rest)?));
                    last = Some(h);
                }
                last
            },
        )?;
        Ok(out)
    }

    /// Read a value, following up with Read Blob for long ones.
    pub fn read(&mut self, h: u16) -> Result<Vec<u8>, Error> {
        let r = self.req(&att::read_req(h), att::READ_RSP)?;
        let mut v = r[1..].to_vec();
        let chunk = self.mtu as usize - 1;
        let mut last = v.len();
        while last == chunk && v.len() < 4096 {
            match self.req(&att::read_blob_req(h, v.len() as u16), att::READ_BLOB_RSP) {
                Ok(r) => {
                    last = r.len() - 1;
                    v.extend_from_slice(&r[1..]);
                }
                Err(Error::Att(_, _, att::ERR_ATTRIBUTE_NOT_LONG)) => break,
                Err(e) => return Err(e),
            }
        }
        Ok(v)
    }

    pub fn write(&mut self, h: u16, value: &[u8]) -> Result<(), Error> {
        self.req(&att::write(att::WRITE_REQ, h, value), att::WRITE_RSP)
            .map(|_| ())
    }

    pub fn write_cmd(&mut self, h: u16, value: &[u8]) -> Result<(), Error> {
        self.bearer.send(&att::write(att::WRITE_CMD, h, value))
    }

    /// Turn on notifications (or indications) through a CCCD.
    pub fn subscribe(&mut self, cccd: u16, indicate: bool) -> Result<(), Error> {
        self.write(cccd, &(if indicate { 2u16 } else { 1u16 }).to_le_bytes())
    }
}

/// A notification or indication: (handle, value).
pub fn notification(pdu: &[u8]) -> Option<(u16, &[u8])> {
    match pdu.first()? {
        &att::NOTIFY | &att::INDICATE => Some((le16(pdu, 1)?, pdu.get(3..)?)),
        _ => None,
    }
}

/// A small in-memory GATT server for tests.
#[cfg(test)]
pub mod testserver {
    use super::*;

    pub struct Attr {
        pub handle: u16,
        pub ty: Uuid,
        pub value: Vec<u8>,
    }

    #[derive(Default)]
    pub struct Server {
        pub attrs: Vec<Attr>,
        pub mtu: u16,
        pub written: Vec<(u16, Vec<u8>)>,
        /// Handles that need an encrypted link.
        pub secure: Vec<u16>,
        pub encrypted: bool,
    }

    impl Server {
        pub fn add(&mut self, ty: u16, value: &[u8]) -> u16 {
            let h = self.attrs.len() as u16 + 1;
            self.attrs.push(Attr {
                handle: h,
                ty: Uuid::U16(ty),
                value: value.to_vec(),
            });
            h
        }

        pub fn service(&mut self, uuid: u16) -> u16 {
            self.add(PRIMARY_SERVICE, &uuid.to_le_bytes())
        }

        /// Adds declaration + value; returns the value handle.
        pub fn characteristic(&mut self, uuid: u16, props: u8, value: &[u8]) -> u16 {
            let h = self.attrs.len() as u16 + 1;
            let mut d = alloc::vec![props];
            d.extend_from_slice(&(h + 1).to_le_bytes());
            d.extend_from_slice(&uuid.to_le_bytes());
            self.add(CHARACTERISTIC, &d);
            self.add(uuid, value)
        }

        fn group_end(&self, i: usize) -> u16 {
            self.attrs[i + 1..]
                .iter()
                .find(|a| a.ty == Uuid::U16(PRIMARY_SERVICE))
                .map_or(self.attrs.len() as u16, |a| a.handle - 1)
        }

        pub fn handle(&mut self, p: &[u8]) -> Vec<u8> {
            let op = p[0];
            let h = le16(p, 1).unwrap_or(0);
            let mtu = if self.mtu == 0 { 23 } else { self.mtu } as usize;
            match op {
                att::MTU_REQ => {
                    self.mtu = le16(p, 1).unwrap().min(100);
                    let mut v = alloc::vec![att::MTU_RSP];
                    v.extend_from_slice(&100u16.to_le_bytes());
                    v
                }
                att::READ_BY_GROUP_REQ | att::READ_BY_TYPE_REQ | att::FIND_INFO_REQ => {
                    let end = le16(p, 3).unwrap();
                    let want = Uuid::from_bytes(&p[5..]);
                    let mut out = Vec::new();
                    let mut elen = 0;
                    for (i, a) in self.attrs.iter().enumerate() {
                        if a.handle < h || a.handle > end {
                            continue;
                        }
                        if op != att::FIND_INFO_REQ && Some(a.ty) != want {
                            continue;
                        }
                        let mut e = a.handle.to_le_bytes().to_vec();
                        match op {
                            att::READ_BY_GROUP_REQ => {
                                e.extend_from_slice(&self.group_end(i).to_le_bytes());
                                e.extend_from_slice(&a.value);
                            }
                            att::READ_BY_TYPE_REQ => e.extend_from_slice(&a.value),
                            _ => e.extend_from_slice(&a.ty.bytes()),
                        }
                        if elen == 0 {
                            elen = e.len();
                        }
                        if e.len() != elen || 2 + out.len() + e.len() > mtu {
                            break;
                        }
                        out.extend_from_slice(&e);
                    }
                    if out.is_empty() {
                        return att::error_rsp(op, h, att::ERR_ATTRIBUTE_NOT_FOUND);
                    }
                    let rsp = op + 1;
                    let second = if op == att::FIND_INFO_REQ {
                        1
                    } else {
                        elen as u8
                    };
                    let mut v = alloc::vec![rsp, second];
                    v.extend_from_slice(&out);
                    v
                }
                att::READ_REQ | att::READ_BLOB_REQ => {
                    if self.secure.contains(&h) && !self.encrypted {
                        return att::error_rsp(op, h, att::ERR_INSUFFICIENT_ENCRYPTION);
                    }
                    let off = if op == att::READ_BLOB_REQ {
                        le16(p, 3).unwrap() as usize
                    } else {
                        0
                    };
                    let Some(a) = self.attrs.iter().find(|a| a.handle == h) else {
                        return att::error_rsp(op, h, att::ERR_INVALID_HANDLE);
                    };
                    let v = &a.value[off.min(a.value.len())..];
                    let mut r = alloc::vec![op + 1];
                    r.extend_from_slice(&v[..v.len().min(mtu - 1)]);
                    r
                }
                att::WRITE_REQ | att::WRITE_CMD => {
                    if self.secure.contains(&h) && !self.encrypted {
                        return att::error_rsp(op, h, att::ERR_INSUFFICIENT_ENCRYPTION);
                    }
                    self.written.push((h, p[3..].to_vec()));
                    alloc::vec![att::WRITE_RSP]
                }
                _ => att::error_rsp(op, h, att::ERR_REQUEST_NOT_SUPPORTED),
            }
        }
    }

    impl Bearer for Server {
        fn request(&mut self, pdu: &[u8]) -> Result<Vec<u8>, Error> {
            Ok(self.handle(pdu))
        }
        fn send(&mut self, pdu: &[u8]) -> Result<(), Error> {
            self.handle(pdu);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testserver::Server;
    use super::*;

    #[test]
    fn discovery_and_long_read() {
        let mut s = Server::default();
        s.service(0x1800);
        s.characteristic(0x2A00, PROP_READ, b"Test Keyboard");
        s.service(0x1812);
        let map: Vec<u8> = (0..70).collect();
        let map_h = s.characteristic(0x2A4B, PROP_READ, &map);
        let rep = s.characteristic(0x2A4D, PROP_READ | PROP_NOTIFY, &[0; 8]);
        let cccd = s.add(CCCD, &[0, 0]);
        let mut c = Client::new(&mut s);
        let svcs = c.services().unwrap();
        assert_eq!(svcs.len(), 2);
        assert_eq!(svcs[1].uuid, Uuid::U16(0x1812));
        let chars = c.characteristics(&svcs[1]).unwrap();
        assert_eq!(chars.len(), 2);
        assert_eq!(chars[1].value, rep);
        assert_eq!(c.descriptors(&chars[1]).unwrap(), [(cccd, Uuid::U16(CCCD))]);
        assert_eq!(c.descriptors(&chars[0]).unwrap(), []);
        // 23-byte MTU: 22 bytes per read, then blobs.
        assert_eq!(c.read(map_h).unwrap(), map);
        assert_eq!(c.exchange_mtu(247).unwrap(), 100);
        assert_eq!(c.read(map_h).unwrap(), map);
        c.subscribe(cccd, false).unwrap();
        assert_eq!(s.written, [(cccd, alloc::vec![1, 0])]);
        let n = [att::NOTIFY, rep as u8, 0, 1, 2];
        assert_eq!(notification(&n), Some((rep, &[1, 2][..])));
    }
}
