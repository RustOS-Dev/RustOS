//! The LE Security Manager, central (initiator) side: LE Secure
//! Connections (Just Works, numeric comparison, passkey entry) with legacy
//! pairing as the fallback, followed by key distribution. The pairing
//! runs synchronously over an `SmpIo`, which carries SMP PDUs, supplies
//! randomness and user interaction, and starts link encryption.

use crate::crypto::{self, KeyPair};
use crate::{Addr, AddrType};
use alloc::vec::Vec;

pub const PAIRING_REQUEST: u8 = 0x01;
pub const PAIRING_RESPONSE: u8 = 0x02;
pub const PAIRING_CONFIRM: u8 = 0x03;
pub const PAIRING_RANDOM: u8 = 0x04;
pub const PAIRING_FAILED: u8 = 0x05;
pub const ENCRYPTION_INFO: u8 = 0x06;
pub const CENTRAL_IDENT: u8 = 0x07;
pub const IDENTITY_INFO: u8 = 0x08;
pub const IDENTITY_ADDR_INFO: u8 = 0x09;
pub const SIGNING_INFO: u8 = 0x0A;
pub const SECURITY_REQUEST: u8 = 0x0B;
pub const PUBLIC_KEY: u8 = 0x0C;
pub const DHKEY_CHECK: u8 = 0x0D;
pub const KEYPRESS: u8 = 0x0E;

/// IO capabilities.
pub const IO_DISPLAY_ONLY: u8 = 0;
pub const IO_DISPLAY_YES_NO: u8 = 1;
pub const IO_KEYBOARD_ONLY: u8 = 2;
pub const IO_NONE: u8 = 3;
pub const IO_KEYBOARD_DISPLAY: u8 = 4;

/// AuthReq bits.
pub const AUTH_BONDING: u8 = 0x01;
pub const AUTH_MITM: u8 = 0x04;
pub const AUTH_SC: u8 = 0x08;

/// Key distribution bits.
pub const KEY_ENC: u8 = 0x01;
pub const KEY_ID: u8 = 0x02;
pub const KEY_SIGN: u8 = 0x04;

/// Pairing Failed reasons.
pub const ERR_PASSKEY_ENTRY: u8 = 0x01;
pub const ERR_AUTH_REQUIREMENTS: u8 = 0x03;
pub const ERR_CONFIRM_VALUE: u8 = 0x04;
pub const ERR_PAIRING_NOT_SUPPORTED: u8 = 0x05;
pub const ERR_ENC_KEY_SIZE: u8 = 0x06;
pub const ERR_UNSPECIFIED: u8 = 0x08;
pub const ERR_INVALID_PARAMETERS: u8 = 0x0A;
pub const ERR_DHKEY_CHECK: u8 = 0x0B;
pub const ERR_NUMERIC_COMPARISON: u8 = 0x0C;

/// How long each step may wait for the peer (the SMP timeout is 30 s).
pub const TIMEOUT_MS: u64 = 30_000;

/// The association model a pairing uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    JustWorks,
    NumericComparison,
    /// We show a passkey that the user types on the peer.
    PasskeyWeDisplay,
    /// The peer shows a passkey that the user types here.
    PasskeyWeEnter,
    /// Both sides have only keyboards: the user types the same passkey on
    /// both.
    PasskeyBothEnter,
}

/// Choose the association model (Core Vol 3 Part H 2.3.5.1) for us as
/// initiator.
pub fn method(ours: u8, theirs: u8, mitm: bool, sc: bool) -> Method {
    use Method::*;
    if !mitm || ours == IO_NONE || theirs == IO_NONE || ours > 4 || theirs > 4 {
        return JustWorks;
    }
    let nc = |legacy: Method| if sc { NumericComparison } else { legacy };
    match (ours, theirs) {
        (IO_DISPLAY_ONLY, IO_KEYBOARD_ONLY | IO_KEYBOARD_DISPLAY) => PasskeyWeDisplay,
        (IO_DISPLAY_ONLY, _) => JustWorks,
        (IO_DISPLAY_YES_NO, IO_DISPLAY_ONLY) => JustWorks,
        (IO_DISPLAY_YES_NO, IO_DISPLAY_YES_NO) => nc(JustWorks),
        (IO_DISPLAY_YES_NO, IO_KEYBOARD_ONLY) => PasskeyWeDisplay,
        (IO_DISPLAY_YES_NO, _) => nc(PasskeyWeDisplay),
        (IO_KEYBOARD_ONLY, IO_KEYBOARD_ONLY) => PasskeyBothEnter,
        (IO_KEYBOARD_ONLY, _) => PasskeyWeEnter,
        (IO_KEYBOARD_DISPLAY, IO_DISPLAY_ONLY) => PasskeyWeEnter,
        (IO_KEYBOARD_DISPLAY, IO_DISPLAY_YES_NO) => nc(PasskeyWeEnter),
        (IO_KEYBOARD_DISPLAY, IO_KEYBOARD_ONLY) => PasskeyWeDisplay,
        (IO_KEYBOARD_DISPLAY, _) => nc(PasskeyWeDisplay),
        _ => JustWorks,
    }
}

/// What the pairing needs from the system.
pub trait SmpIo {
    fn send(&mut self, pdu: &[u8]);
    /// The next SMP PDU from the peer, or None on timeout/disconnect.
    fn recv(&mut self, timeout_ms: u64) -> Option<Vec<u8>>;
    fn random(&mut self, out: &mut [u8]);
    /// Show a passkey the user must type on the peer.
    fn show_passkey(&mut self, passkey: u32);
    /// Ask the user for the passkey the peer shows.
    fn enter_passkey(&mut self) -> Option<u32>;
    /// Ask the user whether both devices show `value`.
    fn confirm(&mut self, value: u32) -> bool;
    /// Encrypt the link with `key`; true once encryption is on.
    fn encrypt(&mut self, key: &[u8; 16], ediv: u16, rand: &[u8; 8]) -> bool;
}

/// Our side of the pairing.
#[derive(Clone, Debug)]
pub struct Local {
    pub addr: Addr,
    pub addr_type: AddrType,
    pub io_cap: u8,
    /// Our identity resolving key (distributed to the peer).
    pub irk: [u8; 16],
}

/// The keys a pairing leaves: enough to re-encrypt the link and to
/// recognise the peer behind resolvable private addresses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bond {
    pub ltk: [u8; 16],
    pub ediv: u16,
    pub rand: [u8; 8],
    pub key_size: u8,
    /// The pairing protected against man-in-the-middle attacks.
    pub authenticated: bool,
    /// LE Secure Connections.
    pub secure: bool,
    pub irk: Option<[u8; 16]>,
    pub identity: Option<(Addr, AddrType)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// We failed the pairing (the reason was sent to the peer).
    Local(u8),
    /// The peer sent Pairing Failed.
    Remote(u8),
    Timeout,
    /// The link would not encrypt with the new key.
    Encryption,
}

pub struct Pairing<'a, I: SmpIo> {
    io: &'a mut I,
    local: &'a Local,
    peer: Addr,
    peer_type: AddrType,
    preq: [u8; 7],
    pres: [u8; 7],
}

fn arr16(b: &[u8]) -> Option<[u8; 16]> {
    b.get(..16)?.try_into().ok()
}

impl<'a, I: SmpIo> Pairing<'a, I> {
    pub fn new(io: &'a mut I, local: &'a Local, peer: Addr, peer_type: AddrType) -> Self {
        Pairing {
            io,
            local,
            peer,
            peer_type,
            preq: [0; 7],
            pres: [0; 7],
        }
    }

    fn fail(&mut self, reason: u8) -> Failure {
        self.io.send(&[PAIRING_FAILED, reason]);
        Failure::Local(reason)
    }

    /// Wait for a PDU with the given code; Pairing Failed ends the pairing.
    fn expect(&mut self, code: u8, len: usize) -> Result<Vec<u8>, Failure> {
        loop {
            let p = self.io.recv(TIMEOUT_MS).ok_or(Failure::Timeout)?;
            match p.first() {
                Some(&PAIRING_FAILED) => {
                    return Err(Failure::Remote(p.get(1).copied().unwrap_or(0)));
                }
                // A peripheral may repeat its Security Request, and
                // keypress notifications are informational.
                Some(&SECURITY_REQUEST) | Some(&KEYPRESS) => continue,
                Some(&c) if c == code && p.len() > len => return Ok(p),
                _ => return Err(self.fail(ERR_INVALID_PARAMETERS)),
            }
        }
    }

    fn random16(&mut self) -> [u8; 16] {
        let mut r = [0u8; 16];
        self.io.random(&mut r);
        r
    }

    fn a(&self) -> [u8; 7] {
        crypto::addr56(&self.local.addr, self.local.addr_type)
    }

    fn b(&self) -> [u8; 7] {
        crypto::addr56(&self.peer, self.peer_type)
    }

    /// Pair and return the bond.
    pub fn run(mut self) -> Result<Bond, Failure> {
        let auth = AUTH_BONDING | AUTH_MITM | AUTH_SC;
        self.preq = [
            PAIRING_REQUEST,
            self.local.io_cap,
            0, // no OOB data
            auth,
            16,
            KEY_ID,
            KEY_ENC | KEY_ID,
        ];
        let preq = self.preq;
        self.io.send(&preq);
        let rsp = self.expect(PAIRING_RESPONSE, 6)?;
        self.pres.copy_from_slice(&rsp[..7]);
        let (their_io, their_auth, key_size) = (rsp[1], rsp[3], rsp[4]);
        if !(7..=16).contains(&key_size) {
            return Err(self.fail(ERR_ENC_KEY_SIZE));
        }
        let sc = their_auth & AUTH_SC != 0;
        let mitm = their_auth & AUTH_MITM != 0 || auth & AUTH_MITM != 0;
        let m = method(self.local.io_cap, their_io, mitm, sc);
        let init_dist = rsp[5] & preq[5];
        let resp_dist = rsp[6] & preq[6];
        let mut bond = if sc {
            self.secure_connections(m)?
        } else {
            self.legacy(m, key_size)?
        };
        bond.key_size = key_size;
        bond.authenticated = m != Method::JustWorks;
        self.distribute(&mut bond, sc, init_dist, resp_dist)?;
        Ok(bond)
    }

    fn secure_connections(&mut self, m: Method) -> Result<Bond, Failure> {
        let kp = loop {
            let mut seed = [0u8; 32];
            self.io.random(&mut seed);
            if let Some(k) = KeyPair::from_random(&seed) {
                break k;
            }
        };
        let mut pk = alloc::vec![PUBLIC_KEY];
        pk.extend_from_slice(&kp.x);
        pk.extend_from_slice(&kp.y);
        self.io.send(&pk);
        let p = self.expect(PUBLIC_KEY, 64)?;
        let px: [u8; 32] = p[1..33].try_into().unwrap();
        let py: [u8; 32] = p[33..65].try_into().unwrap();
        if px == kp.x && py == kp.y {
            // A reflected key: pairing with ourselves.
            return Err(self.fail(ERR_INVALID_PARAMETERS));
        }
        let Some(dhkey) = kp.dhkey(&px, &py) else {
            return Err(self.fail(ERR_DHKEY_CHECK));
        };
        let (na, nb, r) = match m {
            Method::JustWorks | Method::NumericComparison => {
                let cb = self.expect(PAIRING_CONFIRM, 16)?;
                let na = self.random16();
                self.send16(PAIRING_RANDOM, &na);
                let nb = arr16(&self.expect(PAIRING_RANDOM, 16)?[1..]).unwrap();
                if crypto::f4(&px, &kp.x, &nb, 0)[..] != cb[1..17] {
                    return Err(self.fail(ERR_CONFIRM_VALUE));
                }
                if m == Method::NumericComparison {
                    let v = crypto::g2(&kp.x, &px, &na, &nb);
                    if !self.io.confirm(v) {
                        return Err(self.fail(ERR_NUMERIC_COMPARISON));
                    }
                }
                (na, nb, [0u8; 16])
            }
            _ => {
                let passkey = self.passkey(m)?;
                let mut na = [0u8; 16];
                let mut nb = [0u8; 16];
                for i in 0..20 {
                    let ri = 0x80 | ((passkey >> i) & 1) as u8;
                    na = self.random16();
                    let ca = crypto::f4(&kp.x, &px, &na, ri);
                    self.send16(PAIRING_CONFIRM, &ca);
                    let cb = self.expect(PAIRING_CONFIRM, 16)?;
                    self.send16(PAIRING_RANDOM, &na);
                    nb = arr16(&self.expect(PAIRING_RANDOM, 16)?[1..]).unwrap();
                    if crypto::f4(&px, &kp.x, &nb, ri)[..] != cb[1..17] {
                        return Err(self.fail(ERR_CONFIRM_VALUE));
                    }
                }
                let mut r = [0u8; 16];
                r[..4].copy_from_slice(&passkey.to_le_bytes());
                (na, nb, r)
            }
        };
        let (mac, ltk) = crypto::f5(&dhkey, &na, &nb, &self.a(), &self.b());
        let io_a = [self.preq[1], self.preq[2], self.preq[3]];
        let io_b = [self.pres[1], self.pres[2], self.pres[3]];
        let ea = crypto::f6(&mac, &na, &nb, &r, &io_a, &self.a(), &self.b());
        self.send16(DHKEY_CHECK, &ea);
        let eb = self.expect(DHKEY_CHECK, 16)?;
        if crypto::f6(&mac, &nb, &na, &r, &io_b, &self.b(), &self.a())[..] != eb[1..17] {
            return Err(self.fail(ERR_DHKEY_CHECK));
        }
        if !self.io.encrypt(&ltk, 0, &[0; 8]) {
            return Err(Failure::Encryption);
        }
        Ok(Bond {
            ltk,
            secure: true,
            ..Default::default()
        })
    }

    fn passkey(&mut self, m: Method) -> Result<u32, Failure> {
        match m {
            Method::PasskeyWeDisplay => {
                let mut b = [0u8; 4];
                self.io.random(&mut b);
                let p = u32::from_le_bytes(b) % 1_000_000;
                self.io.show_passkey(p);
                Ok(p)
            }
            _ => match self.io.enter_passkey() {
                Some(p) if p < 1_000_000 => Ok(p),
                _ => Err(self.fail(ERR_PASSKEY_ENTRY)),
            },
        }
    }

    fn legacy(&mut self, m: Method, key_size: u8) -> Result<Bond, Failure> {
        let mut tk = [0u8; 16];
        match m {
            Method::JustWorks => {}
            Method::NumericComparison => return Err(self.fail(ERR_AUTH_REQUIREMENTS)),
            _ => tk[..4].copy_from_slice(&self.passkey(m)?.to_le_bytes()),
        }
        let (ia, ra) = (self.local.addr, self.peer);
        let (iat, rat) = (self.local.addr_type as u8, self.peer_type as u8);
        let mrand = self.random16();
        let mconfirm = crypto::c1(&tk, &mrand, &self.preq, &self.pres, iat, rat, &ia, &ra);
        self.send16(PAIRING_CONFIRM, &mconfirm);
        let sconfirm = self.expect(PAIRING_CONFIRM, 16)?;
        self.send16(PAIRING_RANDOM, &mrand);
        let srand = arr16(&self.expect(PAIRING_RANDOM, 16)?[1..]).unwrap();
        let check = crypto::c1(&tk, &srand, &self.preq, &self.pres, iat, rat, &ia, &ra);
        if check[..] != sconfirm[1..17] {
            return Err(self.fail(ERR_CONFIRM_VALUE));
        }
        let mut stk = crypto::s1(&tk, &srand, &mrand);
        stk[key_size as usize..].fill(0);
        if !self.io.encrypt(&stk, 0, &[0; 8]) {
            return Err(Failure::Encryption);
        }
        Ok(Bond::default())
    }

    fn send16(&mut self, code: u8, v: &[u8; 16]) {
        let mut p = alloc::vec![code];
        p.extend_from_slice(v);
        self.io.send(&p);
    }

    /// Key distribution: the responder's keys first, then ours.
    fn distribute(
        &mut self,
        bond: &mut Bond,
        sc: bool,
        init_dist: u8,
        resp_dist: u8,
    ) -> Result<(), Failure> {
        if !sc && resp_dist & KEY_ENC != 0 {
            let e = self.expect(ENCRYPTION_INFO, 16)?;
            bond.ltk = arr16(&e[1..]).unwrap();
            bond.ltk[bond.key_size.max(7) as usize..].fill(0);
            let c = self.expect(CENTRAL_IDENT, 10)?;
            bond.ediv = u16::from_le_bytes([c[1], c[2]]);
            bond.rand.copy_from_slice(&c[3..11]);
        }
        if resp_dist & KEY_ID != 0 {
            let i = self.expect(IDENTITY_INFO, 16)?;
            bond.irk = arr16(&i[1..]);
            let a = self.expect(IDENTITY_ADDR_INFO, 7)?;
            bond.identity = Some((Addr::from_slice(&a[2..8]).unwrap(), AddrType::from_u8(a[1])));
        }
        if resp_dist & KEY_SIGN != 0 {
            self.expect(SIGNING_INFO, 16)?;
        }
        if init_dist & KEY_ID != 0 {
            let irk = self.local.irk;
            self.send16(IDENTITY_INFO, &irk);
            let mut a = alloc::vec![IDENTITY_ADDR_INFO, self.local.addr_type as u8];
            a.extend_from_slice(&self.local.addr.0);
            self.io.send(&a);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::VecDeque;

    /// A peripheral's side of pairing, written independently of the
    /// initiator, driven by the PDUs the initiator sends.
    struct Responder {
        io_cap: u8,
        auth: u8,
        addr: Addr,
        central: Addr,
        passkey: u32,
        seed: u8,
        preq: [u8; 7],
        pres: [u8; 7],
        kp: Option<KeyPair>,
        pka: ([u8; 32], [u8; 32]),
        round: usize,
        ca: [u8; 16],
        nb: [u8; 16],
        na: [u8; 16],
        ltk: [u8; 16],
        srand: [u8; 16],
        mconfirm: [u8; 16],
        out: VecDeque<Vec<u8>>,
        irk: [u8; 16],
    }

    impl Responder {
        fn new(io_cap: u8, auth: u8) -> Self {
            Responder {
                io_cap,
                auth,
                addr: Addr::parse("C0:11:22:33:44:55").unwrap(),
                central: Addr::parse("00:1A:7D:DA:71:13").unwrap(),
                passkey: 123456,
                seed: 7,
                preq: [0; 7],
                pres: [0; 7],
                kp: None,
                pka: ([0; 32], [0; 32]),
                round: 0,
                ca: [0; 16],
                nb: [0; 16],
                na: [0; 16],
                ltk: [0; 16],
                srand: [0; 16],
                mconfirm: [0; 16],
                out: VecDeque::new(),
                irk: [0x5A; 16],
            }
        }

        fn rnd(&mut self) -> [u8; 16] {
            self.seed = self.seed.wrapping_mul(31).wrapping_add(17);
            [self.seed; 16]
        }

        fn sc(&self) -> bool {
            self.auth & AUTH_SC != 0 && self.preq[3] & AUTH_SC != 0
        }

        fn a(&self) -> [u8; 7] {
            crypto::addr56(&self.central, AddrType::Public)
        }

        fn b(&self) -> [u8; 7] {
            crypto::addr56(&self.addr, AddrType::Random)
        }

        fn passkey_mode(&self) -> bool {
            let m = method(self.preq[1], self.io_cap, true, self.sc());
            !matches!(m, Method::JustWorks | Method::NumericComparison)
        }

        fn push16(&mut self, code: u8, v: [u8; 16]) {
            let mut p = alloc::vec![code];
            p.extend_from_slice(&v);
            self.out.push_back(p);
        }

        fn handle(&mut self, p: &[u8]) {
            match p[0] {
                PAIRING_REQUEST => {
                    self.preq.copy_from_slice(p);
                    self.pres = [
                        PAIRING_RESPONSE,
                        self.io_cap,
                        0,
                        self.auth,
                        16,
                        p[5] & KEY_ID,
                        p[6] & (KEY_ENC | KEY_ID),
                    ];
                    self.out.push_back(self.pres.to_vec());
                }
                PUBLIC_KEY => {
                    self.pka = (p[1..33].try_into().unwrap(), p[33..65].try_into().unwrap());
                    let mut seed = [0u8; 32];
                    seed[31] = 99;
                    let kp = KeyPair::from_random(&seed).unwrap();
                    let mut pk = alloc::vec![PUBLIC_KEY];
                    pk.extend_from_slice(&kp.x);
                    pk.extend_from_slice(&kp.y);
                    self.out.push_back(pk);
                    self.kp = Some(kp);
                    if !self.passkey_mode() {
                        self.nb = self.rnd();
                        let kp = self.kp.as_ref().unwrap();
                        let cb = crypto::f4(&kp.x, &self.pka.0, &self.nb, 0);
                        self.push16(PAIRING_CONFIRM, cb);
                    }
                }
                PAIRING_CONFIRM if self.sc() => {
                    // Passkey round i.
                    self.ca = p[1..17].try_into().unwrap();
                    let ri = 0x80 | ((self.passkey >> self.round) & 1) as u8;
                    self.nb = self.rnd();
                    let kp = self.kp.as_ref().unwrap();
                    let cb = crypto::f4(&kp.x, &self.pka.0, &self.nb, ri);
                    self.push16(PAIRING_CONFIRM, cb);
                }
                PAIRING_CONFIRM => {
                    self.mconfirm = p[1..17].try_into().unwrap();
                    self.srand = self.rnd();
                    let tk = self.tk();
                    let c = crypto::c1(
                        &tk,
                        &self.srand,
                        &self.preq,
                        &self.pres,
                        0,
                        1,
                        &self.central,
                        &self.addr,
                    );
                    self.push16(PAIRING_CONFIRM, c);
                }
                PAIRING_RANDOM if self.sc() => {
                    self.na = p[1..17].try_into().unwrap();
                    if self.passkey_mode() {
                        let ri = 0x80 | ((self.passkey >> self.round) & 1) as u8;
                        let ok =
                            crypto::f4(&self.pka.0, &self.kp.as_ref().unwrap().x, &self.na, ri)
                                == self.ca;
                        if !ok {
                            self.out
                                .push_back(alloc::vec![PAIRING_FAILED, ERR_CONFIRM_VALUE]);
                            return;
                        }
                        self.round += 1;
                    }
                    let nb = self.nb;
                    self.push16(PAIRING_RANDOM, nb);
                }
                PAIRING_RANDOM => {
                    let mrand: [u8; 16] = p[1..17].try_into().unwrap();
                    let tk = self.tk();
                    let c = crypto::c1(
                        &tk,
                        &mrand,
                        &self.preq,
                        &self.pres,
                        0,
                        1,
                        &self.central,
                        &self.addr,
                    );
                    if c != self.mconfirm {
                        self.out
                            .push_back(alloc::vec![PAIRING_FAILED, ERR_CONFIRM_VALUE]);
                        return;
                    }
                    let srand = self.srand;
                    self.push16(PAIRING_RANDOM, srand);
                    self.ltk = crypto::s1(&tk, &self.srand, &mrand);
                }
                DHKEY_CHECK => {
                    let kp = self.kp.as_ref().unwrap();
                    let dh = kp.dhkey(&self.pka.0, &self.pka.1).unwrap();
                    let (mac, ltk) = crypto::f5(&dh, &self.na, &self.nb, &self.a(), &self.b());
                    let mut r = [0u8; 16];
                    if self.passkey_mode() {
                        r[..4].copy_from_slice(&self.passkey.to_le_bytes());
                    }
                    let io_a = [self.preq[1], self.preq[2], self.preq[3]];
                    let io_b = [self.pres[1], self.pres[2], self.pres[3]];
                    let ea = crypto::f6(&mac, &self.na, &self.nb, &r, &io_a, &self.a(), &self.b());
                    if ea[..] != p[1..17] {
                        self.out
                            .push_back(alloc::vec![PAIRING_FAILED, ERR_DHKEY_CHECK]);
                        return;
                    }
                    let eb = crypto::f6(&mac, &self.nb, &self.na, &r, &io_b, &self.b(), &self.a());
                    self.push16(DHKEY_CHECK, eb);
                    self.ltk = ltk;
                }
                _ => {}
            }
        }

        fn tk(&self) -> [u8; 16] {
            let mut tk = [0u8; 16];
            if self.passkey_mode() {
                tk[..4].copy_from_slice(&self.passkey.to_le_bytes());
            }
            tk
        }

        /// After encryption: the keys the responder distributes.
        fn keys(&mut self) {
            if !self.sc() && self.pres[6] & KEY_ENC != 0 {
                self.push16(ENCRYPTION_INFO, [0x11; 16]);
                let mut c = alloc::vec![CENTRAL_IDENT, 0x34, 0x12];
                c.extend_from_slice(&[9; 8]);
                self.out.push_back(c);
            }
            if self.pres[6] & KEY_ID != 0 {
                let irk = self.irk;
                self.push16(IDENTITY_INFO, irk);
                let mut a = alloc::vec![IDENTITY_ADDR_INFO, 1];
                a.extend_from_slice(&self.addr.0);
                self.out.push_back(a);
            }
        }
    }

    struct Link {
        r: Responder,
        seed: u8,
        shown: Option<u32>,
        typed: Option<u32>,
        encrypted_with: Option<[u8; 16]>,
        sent: Vec<Vec<u8>>,
    }

    impl SmpIo for Link {
        fn send(&mut self, pdu: &[u8]) {
            self.sent.push(pdu.to_vec());
            self.r.handle(pdu);
        }
        fn recv(&mut self, _t: u64) -> Option<Vec<u8>> {
            self.r.out.pop_front()
        }
        fn random(&mut self, out: &mut [u8]) {
            for b in out {
                self.seed = self.seed.wrapping_mul(13).wrapping_add(5);
                *b = self.seed;
            }
        }
        fn show_passkey(&mut self, p: u32) {
            // The "user" types it on the peripheral.
            self.shown = Some(p);
            self.r.passkey = p;
        }
        fn enter_passkey(&mut self) -> Option<u32> {
            self.typed
        }
        fn confirm(&mut self, _v: u32) -> bool {
            true
        }
        fn encrypt(&mut self, key: &[u8; 16], _ediv: u16, _rand: &[u8; 8]) -> bool {
            self.encrypted_with = Some(*key);
            let ok = *key == self.r.ltk;
            if ok {
                self.r.keys();
            }
            ok
        }
    }

    fn pair(io_cap: u8, auth: u8, ours: u8, typed: Option<u32>) -> (Result<Bond, Failure>, Link) {
        let mut link = Link {
            r: Responder::new(io_cap, auth),
            seed: 1,
            shown: None,
            typed,
            encrypted_with: None,
            sent: Vec::new(),
        };
        let local = Local {
            addr: Addr::parse("00:1A:7D:DA:71:13").unwrap(),
            addr_type: AddrType::Public,
            io_cap: ours,
            irk: [0xA5; 16],
        };
        let peer = link.r.addr;
        let res = Pairing::new(&mut link, &local, peer, AddrType::Random).run();
        (res, link)
    }

    #[test]
    fn methods() {
        use Method::*;
        assert_eq!(method(IO_KEYBOARD_DISPLAY, IO_NONE, true, true), JustWorks);
        assert_eq!(
            method(IO_KEYBOARD_DISPLAY, IO_KEYBOARD_ONLY, true, true),
            PasskeyWeDisplay
        );
        assert_eq!(
            method(IO_KEYBOARD_DISPLAY, IO_DISPLAY_YES_NO, true, true),
            NumericComparison
        );
        assert_eq!(
            method(IO_KEYBOARD_DISPLAY, IO_DISPLAY_YES_NO, true, false),
            PasskeyWeEnter
        );
        assert_eq!(
            method(IO_KEYBOARD_ONLY, IO_KEYBOARD_ONLY, true, true),
            PasskeyBothEnter
        );
        assert_eq!(
            method(IO_KEYBOARD_DISPLAY, IO_KEYBOARD_ONLY, false, true),
            JustWorks
        );
    }

    #[test]
    fn sc_just_works() {
        let (res, link) = pair(IO_NONE, AUTH_BONDING | AUTH_SC, IO_KEYBOARD_DISPLAY, None);
        let b = res.unwrap();
        assert!(b.secure && !b.authenticated);
        assert_eq!(Some(b.ltk), link.encrypted_with);
        assert_eq!(b.irk, Some([0x5A; 16]));
        assert_eq!(b.identity, Some((link.r.addr, AddrType::Random)));
        // Our identity went out last.
        assert_eq!(link.sent.last().unwrap()[0], IDENTITY_ADDR_INFO);
    }

    #[test]
    fn sc_passkey_keyboard() {
        // A keyboard: we display, the user types it on the keyboard.
        let (res, link) = pair(
            IO_KEYBOARD_ONLY,
            AUTH_BONDING | AUTH_MITM | AUTH_SC,
            IO_KEYBOARD_DISPLAY,
            None,
        );
        let b = res.unwrap();
        assert!(b.secure && b.authenticated);
        assert!(link.shown.is_some());
        assert_eq!(link.r.round, 20);
    }

    #[test]
    fn sc_passkey_wrong() {
        // The peer displays; the user types the wrong number here.
        let (res, _) = pair(
            IO_DISPLAY_ONLY,
            AUTH_BONDING | AUTH_MITM | AUTH_SC,
            IO_KEYBOARD_DISPLAY,
            Some(654321),
        );
        assert!(matches!(
            res,
            Err(Failure::Remote(ERR_CONFIRM_VALUE)) | Err(Failure::Local(ERR_CONFIRM_VALUE))
        ));
        let (res, _) = pair(
            IO_DISPLAY_ONLY,
            AUTH_BONDING | AUTH_MITM | AUTH_SC,
            IO_KEYBOARD_DISPLAY,
            Some(123456),
        );
        assert!(res.unwrap().authenticated);
    }

    #[test]
    fn numeric_comparison() {
        let (res, _) = pair(
            IO_DISPLAY_YES_NO,
            AUTH_BONDING | AUTH_MITM | AUTH_SC,
            IO_KEYBOARD_DISPLAY,
            None,
        );
        assert!(res.unwrap().authenticated);
    }

    #[test]
    fn legacy_just_works_and_passkey() {
        let (res, link) = pair(IO_NONE, AUTH_BONDING, IO_KEYBOARD_DISPLAY, None);
        let b = res.unwrap();
        assert!(!b.secure);
        assert_eq!(b.ltk, [0x11; 16]);
        assert_eq!(b.ediv, 0x1234);
        assert_eq!(b.rand, [9; 8]);
        assert!(link.encrypted_with.is_some());
        let (res, link) = pair(
            IO_KEYBOARD_ONLY,
            AUTH_BONDING | AUTH_MITM,
            IO_KEYBOARD_DISPLAY,
            None,
        );
        assert!(res.unwrap().authenticated);
        assert!(link.shown.is_some());
    }
}
