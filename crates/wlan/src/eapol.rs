//! EAPOL-Key frames and the supplicant side of the 4-way and group key
//! handshakes (IEEE 802.11-2020 §12.7.6-7) for PSK, PSK-SHA256 and SAE.

use crate::Mac;
use crate::crypto::{self, min_max};
use crate::ie::{self, Akm, Cipher};
use alloc::vec::Vec;

pub const KI_TYPE_MASK: u16 = 0x0007;
pub const KI_PAIRWISE: u16 = 1 << 3;
pub const KI_INSTALL: u16 = 1 << 6;
pub const KI_ACK: u16 = 1 << 7;
pub const KI_MIC: u16 = 1 << 8;
pub const KI_SECURE: u16 = 1 << 9;
pub const KI_ERROR: u16 = 1 << 10;
pub const KI_REQUEST: u16 = 1 << 11;
pub const KI_ENC_DATA: u16 = 1 << 12;

const MIC_OFF: usize = 81;
const MIC_LEN: usize = 16;

/// An EAPOL-Key frame (with its 802.1X header).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyFrame {
    pub eapol_version: u8,
    pub info: u16,
    pub key_len: u16,
    pub replay: [u8; 8],
    pub nonce: [u8; 32],
    pub iv: [u8; 16],
    pub rsc: [u8; 8],
    pub mic: [u8; 16],
    pub data: Vec<u8>,
}

impl KeyFrame {
    pub fn parse(b: &[u8]) -> Option<KeyFrame> {
        // 802.1X: version, type 3 (EAPOL-Key), length; descriptor type 2 (RSN).
        if b.len() < MIC_OFF + MIC_LEN + 2 || b[1] != 3 || b[4] != 2 {
            return None;
        }
        let body_len = u16::from_be_bytes([b[2], b[3]]) as usize;
        let dlen = u16::from_be_bytes([b[97], b[98]]) as usize;
        if b.len() < 4 + body_len || 99 + dlen > b.len() {
            return None;
        }
        Some(KeyFrame {
            eapol_version: b[0],
            info: u16::from_be_bytes([b[5], b[6]]),
            key_len: u16::from_be_bytes([b[7], b[8]]),
            replay: b[9..17].try_into().unwrap(),
            nonce: b[17..49].try_into().unwrap(),
            iv: b[49..65].try_into().unwrap(),
            rsc: b[65..73].try_into().unwrap(),
            mic: b[MIC_OFF..MIC_OFF + MIC_LEN].try_into().unwrap(),
            data: b[99..99 + dlen].to_vec(),
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let body = 95 + self.data.len();
        let mut b = Vec::with_capacity(4 + body);
        b.push(self.eapol_version.max(1));
        b.push(3);
        b.extend_from_slice(&(body as u16).to_be_bytes());
        b.push(2);
        b.extend_from_slice(&self.info.to_be_bytes());
        b.extend_from_slice(&self.key_len.to_be_bytes());
        b.extend_from_slice(&self.replay);
        b.extend_from_slice(&self.nonce);
        b.extend_from_slice(&self.iv);
        b.extend_from_slice(&self.rsc);
        b.extend_from_slice(&[0; 8]);
        b.extend_from_slice(&self.mic);
        b.extend_from_slice(&(self.data.len() as u16).to_be_bytes());
        b.extend_from_slice(&self.data);
        b
    }
}

/// Pairwise transient key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ptk {
    pub kck: Vec<u8>,
    pub kek: Vec<u8>,
    pub tk: Vec<u8>,
}

/// Key descriptor version used for an AKM.
pub fn descriptor_version(akm: Akm) -> u16 {
    match akm {
        Akm::Psk | Akm::Ieee8021x => 2,
        Akm::PskSha256 => 3,
        _ => 0, // AKM-defined (SAE, OWE, ...)
    }
}

/// Raw PTK bytes (KCK || KEK || TK || extra) of `len` bytes.
pub fn ptk_bytes(akm: Akm, pmk: &[u8], aa: &Mac, spa: &Mac, anonce: &[u8; 32], snonce: &[u8; 32], len: usize) -> Vec<u8> {
    let (a1, a2) = min_max(aa, spa);
    let (n1, n2) = min_max(anonce, snonce);
    let mut ctx = Vec::with_capacity(76);
    ctx.extend_from_slice(a1);
    ctx.extend_from_slice(a2);
    ctx.extend_from_slice(n1);
    ctx.extend_from_slice(n2);
    if akm.sha256() {
        crypto::kdf_sha256(pmk, b"Pairwise key expansion", &ctx, len * 8)
    } else {
        crypto::prf_sha1(pmk, b"Pairwise key expansion", &ctx, len)
    }
}

pub fn derive_ptk(akm: Akm, cipher: Cipher, pmk: &[u8], aa: &Mac, spa: &Mac, anonce: &[u8; 32], snonce: &[u8; 32]) -> Ptk {
    let raw = ptk_bytes(akm, pmk, aa, spa, anonce, snonce, 32 + cipher.key_len());
    Ptk {
        kck: raw[..16].to_vec(),
        kek: raw[16..32].to_vec(),
        tk: raw[32..].to_vec(),
    }
}

/// MIC over a whole EAPOL frame whose MIC field is zero.
pub fn mic(akm: Akm, kck: &[u8], frame: &[u8]) -> [u8; 16] {
    let mut out = [0u8; 16];
    if descriptor_version(akm) == 2 {
        out.copy_from_slice(&crypto::hmac_sha1(kck, &[frame])[..16]);
    } else {
        let k: [u8; 16] = kck[..16].try_into().unwrap();
        out = crypto::aes_cmac(&k, frame);
    }
    out
}

fn sign(akm: Akm, kck: &[u8], k: &KeyFrame) -> Vec<u8> {
    let mut k = k.clone();
    k.mic = [0; 16];
    let mut b = k.encode();
    let m = mic(akm, kck, &b);
    b[MIC_OFF..MIC_OFF + MIC_LEN].copy_from_slice(&m);
    b
}

fn verify(akm: Akm, kck: &[u8], raw: &[u8]) -> bool {
    if raw.len() < MIC_OFF + MIC_LEN {
        return false;
    }
    let mut z = raw.to_vec();
    z[MIC_OFF..MIC_OFF + MIC_LEN].fill(0);
    let expect = mic(akm, kck, &z);
    crypto::ct_eq(&expect, &raw[MIC_OFF..MIC_OFF + MIC_LEN])
}

/// Key data encapsulation (KDE) types.
pub const KDE_GTK: u8 = 1;
pub const KDE_PMKID: u8 = 4;
pub const KDE_IGTK: u8 = 9;

/// Pad and wrap key data with the KEK.
pub fn wrap_key_data(kek: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    let mut d = data.to_vec();
    if d.len() < 16 || d.len() % 8 != 0 {
        d.push(0xDD);
        while d.len() < 16 || d.len() % 8 != 0 {
            d.push(0);
        }
    }
    crypto::aes_wrap(&kek[..16].try_into().ok()?, &d)
}

pub fn unwrap_key_data(kek: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    crypto::aes_unwrap(&kek[..16].try_into().ok()?, data)
}

/// Find a KDE of `kind` in (decrypted) key data.
pub fn find_kde(data: &[u8], kind: u8) -> Option<&[u8]> {
    ie::iter(data).find_map(|(id, b)| {
        (id == ie::VENDOR && b.len() >= 4 && b[..3] == [0x00, 0x0F, 0xAC] && b[3] == kind).then(|| &b[4..])
    })
}

pub fn kde(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut v = alloc::vec![0xDD, (4 + body.len()) as u8, 0x00, 0x0F, 0xAC, kind];
    v.extend_from_slice(body);
    v
}

/// What the supplicant wants the driver to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Send this EAPOL frame to the AP.
    Send(Vec<u8>),
    InstallPairwise { cipher: Cipher, key: Vec<u8> },
    InstallGroup { idx: u8, cipher: Cipher, key: Vec<u8>, rsc: [u8; 8] },
    InstallIgtk { idx: u16, key: Vec<u8>, ipn: [u8; 6] },
    /// The 4-way handshake finished; the port is open.
    Connected,
    Failed(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    WaitMsg1,
    WaitMsg3,
    Done,
}

pub struct Supplicant {
    pub akm: Akm,
    pub pairwise: Cipher,
    pub group: Cipher,
    pmk: Vec<u8>,
    aa: Mac,
    spa: Mac,
    /// Our RSN element as sent in the association request.
    own_rsne: Vec<u8>,
    /// The AP's RSN element from its beacon/probe response.
    ap_rsne: Vec<u8>,
    snonce: [u8; 32],
    ptk: Option<Ptk>,
    last_replay: Option<[u8; 8]>,
    state: State,
}

impl Supplicant {
    #[allow(clippy::too_many_arguments)]
    pub fn new(akm: Akm, pairwise: Cipher, group: Cipher, pmk: &[u8], aa: Mac, spa: Mac, own_rsne: Vec<u8>, ap_rsne: Vec<u8>, rng: &mut dyn FnMut(&mut [u8])) -> Supplicant {
        let mut snonce = [0u8; 32];
        rng(&mut snonce);
        Supplicant {
            akm,
            pairwise,
            group,
            pmk: pmk.to_vec(),
            aa,
            spa,
            own_rsne,
            ap_rsne,
            snonce,
            ptk: None,
            last_replay: None,
            state: State::WaitMsg1,
        }
    }

    pub fn is_done(&self) -> bool {
        self.state == State::Done
    }

    fn version(&self) -> u16 {
        descriptor_version(self.akm)
    }

    fn reply(&self, k: &KeyFrame, info: u16, nonce: [u8; 32], data: Vec<u8>) -> Vec<u8> {
        let f = KeyFrame {
            eapol_version: k.eapol_version,
            info: self.version() | info | KI_MIC,
            key_len: 0,
            replay: k.replay,
            nonce,
            data,
            ..Default::default()
        };
        sign(self.akm, &self.ptk.as_ref().unwrap().kck, &f)
    }

    fn replay_ok(&self, r: &[u8; 8]) -> bool {
        self.last_replay.is_none_or(|l| u64::from_be_bytes(*r) > u64::from_be_bytes(l))
    }

    fn install_group(&self, data: &[u8], out: &mut Vec<Action>, rsc: [u8; 8]) -> bool {
        let Some(g) = find_kde(data, KDE_GTK) else { return false };
        if g.len() < 2 + 5 {
            return false;
        }
        out.push(Action::InstallGroup {
            idx: g[0] & 3,
            cipher: self.group,
            key: g[2..].to_vec(),
            rsc,
        });
        if let Some(i) = find_kde(data, KDE_IGTK)
            && i.len() >= 8 + 16
        {
            let mut ipn = [0u8; 6];
            ipn.copy_from_slice(&i[2..8]);
            out.push(Action::InstallIgtk {
                idx: u16::from_le_bytes([i[0], i[1]]),
                key: i[8..].to_vec(),
                ipn,
            });
        }
        true
    }

    /// Process a received EAPOL frame.
    pub fn rx(&mut self, raw: &[u8]) -> Vec<Action> {
        let mut out = Vec::new();
        let Some(k) = KeyFrame::parse(raw) else { return out };
        if k.info & KI_REQUEST != 0 || k.info & KI_ACK == 0 {
            return out;
        }
        if k.info & KI_PAIRWISE != 0 {
            if k.info & KI_MIC == 0 {
                // Message 1 (may be retransmitted; always answer the latest).
                if !self.replay_ok(&k.replay) && self.state != State::WaitMsg1 {
                    return out;
                }
                if self.state == State::Done {
                    return out; // rekeying via a new 4-way is not supported
                }
                self.ptk = Some(derive_ptk(self.akm, self.pairwise, &self.pmk, &self.aa, &self.spa, &k.nonce, &self.snonce));
                self.last_replay = Some(k.replay);
                let msg2 = self.reply(&k, KI_PAIRWISE, self.snonce, self.own_rsne.clone());
                out.push(Action::Send(msg2));
                self.state = State::WaitMsg3;
                return out;
            }
            // Message 3.
            let Some(ptk) = self.ptk.clone() else { return out };
            if self.state != State::WaitMsg3 || !self.replay_ok(&k.replay) {
                return out;
            }
            if !verify(self.akm, &ptk.kck, raw) {
                out.push(Action::Failed("message 3 MIC mismatch (wrong password?)"));
                return out;
            }
            let data = if k.info & KI_ENC_DATA != 0 {
                match unwrap_key_data(&ptk.kek, &k.data) {
                    Some(d) => d,
                    None => {
                        out.push(Action::Failed("cannot decrypt message 3 key data"));
                        return out;
                    }
                }
            } else {
                k.data.clone()
            };
            // The RSNE in message 3 must match the beacon's (downgrade check).
            if let Some(r) = ie::find(&data, ie::RSN)
                && self.ap_rsne.len() > 2
                && r != &self.ap_rsne[2..]
            {
                out.push(Action::Failed("RSN element mismatch in message 3"));
                return out;
            }
            self.last_replay = Some(k.replay);
            let msg4 = self.reply(&k, KI_PAIRWISE | KI_SECURE, [0; 32], Vec::new());
            out.push(Action::Send(msg4));
            out.push(Action::InstallPairwise {
                cipher: self.pairwise,
                key: ptk.tk.clone(),
            });
            self.install_group(&data, &mut out, k.rsc);
            self.state = State::Done;
            out.push(Action::Connected);
            return out;
        }
        // Group key handshake message 1.
        let Some(ptk) = self.ptk.clone() else { return out };
        if self.state != State::Done || !self.replay_ok(&k.replay) || !verify(self.akm, &ptk.kck, raw) {
            return out;
        }
        let Some(data) = unwrap_key_data(&ptk.kek, &k.data) else {
            return out;
        };
        self.last_replay = Some(k.replay);
        if self.install_group(&data, &mut out, k.rsc) {
            let reply = self.reply(&k, KI_SECURE, [0; 32], Vec::new());
            out.insert(0, Action::Send(reply));
        }
        out
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ie::Rsn;

    pub const AA: Mac = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
    pub const SPA: Mac = [0x02, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE];

    pub fn rng() -> impl FnMut(&mut [u8]) {
        let mut x = 0x1234_5678_9abc_def0u64;
        move |b: &mut [u8]| {
            for v in b.iter_mut() {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                *v = x as u8;
            }
        }
    }

    /// Minimal authenticator (AP side) for the tests.
    pub struct Authenticator {
        akm: Akm,
        pmk: Vec<u8>,
        anonce: [u8; 32],
        replay: u64,
        ptk: Option<Ptk>,
        pub rsne: Vec<u8>,
        pub gtk: [u8; 16],
        pub igtk: [u8; 16],
    }

    impl Authenticator {
        pub fn new(akm: Akm, pmk: &[u8], rsne: Vec<u8>) -> Authenticator {
            Authenticator {
                akm,
                pmk: pmk.to_vec(),
                anonce: [0x55; 32],
                replay: 1,
                ptk: None,
                rsne,
                gtk: [0x77; 16],
                igtk: [0x99; 16],
            }
        }

        pub fn msg1(&mut self) -> Vec<u8> {
            self.replay += 1;
            KeyFrame {
                eapol_version: 2,
                info: descriptor_version(self.akm) | KI_PAIRWISE | KI_ACK,
                key_len: 16,
                replay: self.replay.to_be_bytes(),
                nonce: self.anonce,
                ..Default::default()
            }
            .encode()
        }

        /// Returns message 3 if message 2 checks out.
        pub fn msg3(&mut self, msg2: &[u8]) -> Option<Vec<u8>> {
            let k = KeyFrame::parse(msg2)?;
            let ptk = derive_ptk(self.akm, Cipher::Ccmp128, &self.pmk, &AA, &SPA, &self.anonce, &k.nonce);
            if !verify(self.akm, &ptk.kck, msg2) {
                return None;
            }
            let mut data = self.rsne.clone();
            let mut g = alloc::vec![1u8, 0];
            g.extend_from_slice(&self.gtk);
            data.extend_from_slice(&kde(KDE_GTK, &g));
            let mut i = alloc::vec![4u8, 0, 0, 0, 0, 0, 0, 0];
            i.extend_from_slice(&self.igtk);
            data.extend_from_slice(&kde(KDE_IGTK, &i));
            self.replay += 1;
            let f = KeyFrame {
                eapol_version: 2,
                info: descriptor_version(self.akm) | KI_PAIRWISE | KI_ACK | KI_MIC | KI_INSTALL | KI_SECURE | KI_ENC_DATA,
                key_len: 16,
                replay: self.replay.to_be_bytes(),
                nonce: self.anonce,
                data: wrap_key_data(&ptk.kek, &data)?,
                ..Default::default()
            };
            let out = sign(self.akm, &ptk.kck, &f);
            self.ptk = Some(ptk);
            Some(out)
        }

        pub fn check_msg4(&self, msg4: &[u8]) -> bool {
            verify(self.akm, &self.ptk.as_ref().unwrap().kck, msg4)
        }

        pub fn group_msg1(&mut self, gtk: [u8; 16]) -> Vec<u8> {
            let ptk = self.ptk.clone().unwrap();
            let mut g = alloc::vec![2u8, 0];
            g.extend_from_slice(&gtk);
            self.replay += 1;
            let f = KeyFrame {
                eapol_version: 2,
                info: descriptor_version(self.akm) | KI_ACK | KI_MIC | KI_SECURE | KI_ENC_DATA,
                key_len: 16,
                replay: self.replay.to_be_bytes(),
                data: wrap_key_data(&ptk.kek, &kde(KDE_GTK, &g)).unwrap(),
                ..Default::default()
            };
            sign(self.akm, &ptk.kck, &f)
        }

        pub fn tk(&self) -> Vec<u8> {
            self.ptk.as_ref().unwrap().tk.clone()
        }
    }

    fn rsne(akm: Akm) -> Vec<u8> {
        Rsn {
            group: Cipher::Ccmp128,
            pairwise: alloc::vec![Cipher::Ccmp128],
            akms: alloc::vec![akm],
            caps: ie::RSN_CAP_MFPC,
            pmkids: Vec::new(),
            group_mgmt: Some(Cipher::BipCmac128),
        }
        .element()
    }

    pub fn run(akm: Akm, sta_pmk: &[u8], ap_pmk: &[u8]) -> Result<Vec<Action>, &'static str> {
        let ap_rsne = rsne(akm);
        let mut ap = Authenticator::new(akm, ap_pmk, ap_rsne.clone());
        let mut r = rng();
        let mut sta = Supplicant::new(akm, Cipher::Ccmp128, Cipher::Ccmp128, sta_pmk, AA, SPA, rsne(akm), ap_rsne, &mut r);
        let a1 = sta.rx(&ap.msg1());
        let Some(Action::Send(m2)) = a1.first() else { return Err("no msg2") };
        let m3 = ap.msg3(m2).ok_or("AP rejected msg2")?;
        let a3 = sta.rx(&m3);
        let Some(Action::Send(m4)) = a3.first() else {
            return Err("no msg4");
        };
        if !ap.check_msg4(m4) {
            return Err("AP rejected msg4");
        }
        assert!(a3.contains(&Action::InstallPairwise { cipher: Cipher::Ccmp128, key: ap.tk() }));
        assert!(a3.contains(&Action::Connected));
        // Rekey the group key.
        let g = sta.rx(&ap.group_msg1([0x42; 16]));
        assert!(matches!(g.first(), Some(Action::Send(_))));
        assert!(g.iter().any(|a| matches!(a, Action::InstallGroup { idx: 2, key, .. } if key == &[0x42; 16])));
        Ok(a3)
    }

    #[test]
    fn wpa2_psk_handshake() {
        let pmk = crypto::wpa_psk(b"correct horse", b"home");
        let a = run(Akm::Psk, &pmk, &pmk).unwrap();
        assert!(a.iter().any(|x| matches!(x, Action::InstallGroup { idx: 1, key, .. } if key == &[0x77; 16])));
        assert!(a.iter().any(|x| matches!(x, Action::InstallIgtk { idx: 4, .. })));
    }

    #[test]
    fn psk_sha256_and_sae_akms() {
        let pmk = [0x3c; 32];
        run(Akm::PskSha256, &pmk, &pmk).unwrap();
        run(Akm::Sae, &pmk, &pmk).unwrap();
    }

    #[test]
    fn wrong_password_detected() {
        let a = crypto::wpa_psk(b"right", b"home");
        let b = crypto::wpa_psk(b"wrong", b"home");
        assert_eq!(run(Akm::Psk, &b, &a).unwrap_err(), "AP rejected msg2");
    }

    /// IEEE 802.11-2024 J.13: SAE PTK with a 256-bit KDK appended.
    #[test]
    fn ptk_ieee_j13() {
        let h = |s: &str| -> Vec<u8> { (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect() };
        let pmk = h("def43e5567e01ca6649265f19a290eeff8bd888f6c1d9cc9d10f04bd378f3cad");
        let aa: Mac = h("c0ffd4a8dbc1").try_into().unwrap();
        let spa: Mac = h("00904c01c107").try_into().unwrap();
        let an: [u8; 32] = h("be7a1ca284347b5bd67dbd2dfdb4d99f1afae0b88ba18e008718417e4b27ef5f").try_into().unwrap();
        let sn: [u8; 32] = h("404b012ffb43ed0fb43ea1f287c91f2506d21b4a92d74b5ea50c943350ce8671").try_into().unwrap();
        let raw = ptk_bytes(Akm::Sae, &pmk, &aa, &spa, &an, &sn, 16 + 16 + 16 + 32);
        assert_eq!(raw[..16].to_vec(), h("cd7b9e7555362df0b63568484a8112f5"));
        assert_eq!(raw[16..32].to_vec(), h("99cad3588da0f1e63fd190191039bb4b"));
        assert_eq!(raw[32..48].to_vec(), h("9e2e9377e7532e737a1bc250fe194a03"));
        assert_eq!(raw[48..].to_vec(), h("6c7fb97ceb55b01acff00f070942bdf5291feb4bee38e0365b25a250bb2ac9ff"));
    }

    #[test]
    fn frame_round_trip() {
        let k = KeyFrame {
            eapol_version: 2,
            info: 0x008a,
            key_len: 16,
            replay: [0, 0, 0, 0, 0, 0, 0, 1],
            nonce: [7; 32],
            data: alloc::vec![1, 2, 3],
            ..Default::default()
        };
        assert_eq!(KeyFrame::parse(&k.encode()).unwrap(), k);
    }
}
