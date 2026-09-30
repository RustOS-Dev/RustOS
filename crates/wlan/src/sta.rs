//! Station (client) connection state machine.
//!
//! Drivers feed received management and EAPOL frames plus a periodic tick;
//! the station answers with frames to transmit and keys to install. It
//! handles open, WPA2-PSK (incl. PSK-SHA256) and WPA3-SAE (H2E or
//! hunting-and-pecking) networks, PMF negotiation, retries and timeouts.

use crate::eapol::{self, Supplicant};
use crate::frame::{self, BssInfo};
use crate::ie::{self, Akm, Cipher, Rsn, Security};
use crate::sae::{self, Sae};
use crate::{Mac, caps, crypto};
use alloc::vec::Vec;

const RETRY_MS: u64 = 1000;
const MAX_RETRIES: u32 = 3;
const HANDSHAKE_MS: u64 = 8000;

/// RSNXE capability bit: SAE hash-to-element.
const RSNX_SAE_H2E: u8 = 1 << 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    /// Transmit an 802.11 management frame.
    TxMgmt(Vec<u8>),
    /// Transmit an EAPOL frame (ethertype 0x888E) to the AP, unencrypted
    /// until the pairwise key is installed.
    TxEapol(Vec<u8>),
    InstallPairwise {
        cipher: Cipher,
        key: Vec<u8>,
    },
    InstallGroup {
        idx: u8,
        cipher: Cipher,
        key: Vec<u8>,
        rsc: [u8; 8],
    },
    InstallIgtk {
        idx: u16,
        key: Vec<u8>,
        ipn: [u8; 6],
    },
    /// Association succeeded (the driver adds the station to firmware).
    /// `ies` are the AP's elements from the association response.
    Associated {
        aid: u16,
        qos: bool,
        ies: Vec<u8>,
    },
    /// Data traffic may flow.
    Connected,
    /// Lost or failed; `reason` is for the log.
    Disconnected(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Authenticating,
    SaeConfirming,
    Associating,
    Handshake,
    Connected,
    Failed,
}

pub struct Station {
    pub own: Mac,
    pub bss: BssInfo,
    pub security: Security,
    pub state: State,
    akm: Akm,
    pairwise: Cipher,
    group: Cipher,
    pmf: bool,
    pmf_required: bool,
    h2e: bool,
    passphrase: Vec<u8>,
    pmk: Vec<u8>,
    sae: Option<Sae>,
    supplicant: Option<Supplicant>,
    own_rsne: Vec<u8>,
    ap_rsne: Vec<u8>,
    last_tx: Option<Vec<u8>>,
    deadline: u64,
    retries: u32,
    pub aid: u16,
    /// Capabilities advertised in the association request.
    pub profile: caps::Profile,
}

fn band_2g(bss: &BssInfo) -> bool {
    bss.band == crate::chan::Band::B2G
}

impl Station {
    /// Start connecting to `bss`. `passphrase` is ignored for open networks.
    pub fn connect(
        own: Mac,
        bss: BssInfo,
        passphrase: &[u8],
        now: u64,
        rng: &mut dyn FnMut(&mut [u8]),
    ) -> Result<(Station, Vec<Output>), &'static str> {
        let security = ie::security(bss.capability, &bss.ies);
        let rsn = ie::find(&bss.ies, ie::RSN).and_then(Rsn::parse);
        let ap_rsne = ie::find(&bss.ies, ie::RSN)
            .map(|b| {
                let mut e = Vec::new();
                ie::push(&mut e, ie::RSN, b);
                e
            })
            .unwrap_or_default();
        // 6 GHz allows only hash-to-element.
        let h2e = ie::find(&bss.ies, ie::RSNX)
            .is_some_and(|x| x.first().is_some_and(|b| b & RSNX_SAE_H2E != 0))
            || bss.band == crate::chan::Band::B6G;
        let (akm, pairwise, group, pmf, pmf_required) = match (security, &rsn) {
            (Security::Open, _) => (
                Akm::Unknown(0),
                Cipher::Ccmp128,
                Cipher::Ccmp128,
                false,
                false,
            ),
            (Security::Wpa2Psk | Security::Wpa3Sae, Some(r)) => {
                if passphrase.is_empty() {
                    return Err("this network needs a passphrase");
                }
                if !r.pairwise.contains(&Cipher::Ccmp128) {
                    return Err("AP does not offer CCMP");
                }
                let akm = if security == Security::Wpa3Sae {
                    Akm::Sae
                } else if r.akms.contains(&Akm::PskSha256) {
                    Akm::PskSha256
                } else {
                    Akm::Psk
                };
                let pmf_capable = r.caps & ie::RSN_CAP_MFPC != 0;
                let pmf_required = r.caps & ie::RSN_CAP_MFPR != 0 || akm == Akm::Sae;
                if akm == Akm::Sae && !pmf_capable {
                    return Err("WPA3 network without PMF");
                }
                (akm, Cipher::Ccmp128, r.group, pmf_capable, pmf_required)
            }
            (Security::Wpa2Psk | Security::Wpa3Sae, None) => return Err("missing RSN element"),
            (s, _) => {
                let _ = s;
                return Err("unsupported security (WEP/WPA1/Enterprise)");
            }
        };
        let own_rsne = if security == Security::Open {
            Vec::new()
        } else {
            let mut caps = 0;
            if pmf {
                caps |= ie::RSN_CAP_MFPC;
            }
            if pmf_required {
                caps |= ie::RSN_CAP_MFPR;
            }
            Rsn {
                group,
                pairwise: alloc::vec![pairwise],
                akms: alloc::vec![akm],
                caps,
                pmkids: Vec::new(),
                group_mgmt: if pmf {
                    rsn.as_ref()
                        .and_then(|r| r.group_mgmt)
                        .or(Some(Cipher::BipCmac128))
                } else {
                    None
                },
            }
            .element()
        };
        let pmk = if matches!(akm, Akm::Psk | Akm::PskSha256) {
            if passphrase.len() == 64 && passphrase.iter().all(|c| c.is_ascii_hexdigit()) {
                (0..32)
                    .map(|i| {
                        u8::from_str_radix(
                            core::str::from_utf8(&passphrase[i * 2..i * 2 + 2]).unwrap(),
                            16,
                        )
                        .unwrap()
                    })
                    .collect()
            } else if (8..=63).contains(&passphrase.len()) {
                crypto::wpa_psk(passphrase, &bss.ssid).to_vec()
            } else {
                return Err("WPA2 passphrase must be 8-63 characters");
            }
        } else {
            Vec::new()
        };
        let mut sta = Station {
            own,
            security,
            state: State::Authenticating,
            akm,
            pairwise,
            group,
            pmf,
            pmf_required,
            h2e: h2e && akm == Akm::Sae,
            passphrase: passphrase.to_vec(),
            pmk,
            sae: None,
            supplicant: None,
            own_rsne,
            ap_rsne,
            last_tx: None,
            deadline: now + RETRY_MS,
            retries: 0,
            aid: 0,
            profile: caps::Profile::LEGACY,
            bss,
        };
        let out = sta.start_auth(now, rng)?;
        Ok((sta, out))
    }

    fn send(&mut self, f: Vec<u8>, now: u64) -> Output {
        self.last_tx = Some(f.clone());
        self.deadline = now + RETRY_MS;
        Output::TxMgmt(f)
    }

    fn start_auth(
        &mut self,
        now: u64,
        rng: &mut dyn FnMut(&mut [u8]),
    ) -> Result<Vec<Output>, &'static str> {
        let bssid = self.bss.bssid;
        if self.akm == Akm::Sae {
            let s = if self.h2e {
                let pt = sae::h2e_pt(&self.bss.ssid, &self.passphrase, None);
                Sae::new_h2e(&pt, self.own, bssid, rng)
            } else {
                Sae::new(&self.passphrase, self.own, bssid, rng)
                    .map_err(|_| "SAE: no password element")?
            };
            let f = frame::auth(
                self.own,
                bssid,
                frame::AUTH_SAE,
                1,
                s.commit_status(),
                &s.commit(),
            );
            self.sae = Some(s);
            Ok(alloc::vec![self.send(f, now)])
        } else {
            let f = frame::auth(self.own, bssid, frame::AUTH_OPEN, 1, 0, &[]);
            Ok(alloc::vec![self.send(f, now)])
        }
    }

    fn assoc(&mut self, now: u64) -> Output {
        let mut extra = Vec::new();
        if self.h2e {
            ie::push(&mut extra, ie::RSNX, &[RSNX_SAE_H2E | 1]);
        }
        // Advertise WMM so QoS data is used (required for 802.11n+ rates).
        let wmm_ap = ie::iter(&self.bss.ies)
            .any(|(id, b)| id == ie::VENDOR && b.starts_with(&[0x00, 0x50, 0xF2, 0x02]));
        if wmm_ap {
            ie::push(
                &mut extra,
                ie::VENDOR,
                &[0x00, 0x50, 0xF2, 0x02, 0x00, 0x01, 0x00],
            );
        }
        extra.extend_from_slice(&caps::assoc_elements_band(
            &self.profile,
            &self.bss.ies,
            self.bss.band,
        ));
        let f = frame::assoc_request(
            self.own,
            self.bss.bssid,
            self.bss.capability & 0x0431,
            &self.bss.ssid,
            band_2g(&self.bss),
            &self.own_rsne,
            &extra,
        );
        self.state = State::Associating;
        self.retries = 0;
        self.send(f, now)
    }

    fn fail(&mut self, why: &'static str) -> Vec<Output> {
        self.state = State::Failed;
        alloc::vec![Output::Disconnected(why)]
    }

    /// Process a received management frame addressed to us.
    pub fn rx_mgmt(&mut self, f: &[u8], now: u64, rng: &mut dyn FnMut(&mut [u8])) -> Vec<Output> {
        let Some(h) = frame::Header::parse(f) else {
            return Vec::new();
        };
        if h.addr2 != self.bss.bssid || h.addr1 != self.own {
            return Vec::new();
        }
        if let Some(reason) = frame::parse_deauth(f) {
            let _ = reason;
            return self.fail("deauthenticated or disassociated by the AP");
        }
        match self.state {
            State::Authenticating | State::SaeConfirming => {
                let Some((algo, seq, status, body)) = frame::parse_auth(f) else {
                    return Vec::new();
                };
                if algo == frame::AUTH_OPEN && self.akm != Akm::Sae {
                    if status != 0 {
                        return self.fail("open authentication rejected");
                    }
                    return alloc::vec![self.assoc(now)];
                }
                if algo != frame::AUTH_SAE {
                    return Vec::new();
                }
                let Some(sae) = self.sae.as_mut() else {
                    return Vec::new();
                };
                match (seq, status) {
                    (1, frame::STATUS_ANTI_CLOGGING) => {
                        if sae.set_token_from(body).is_err() {
                            return self.fail("SAE: bad anti-clogging request");
                        }
                        let c = frame::auth(
                            self.own,
                            self.bss.bssid,
                            frame::AUTH_SAE,
                            1,
                            sae.commit_status(),
                            &sae.commit(),
                        );
                        alloc::vec![self.send(c, now)]
                    }
                    (1, 0) | (1, frame::STATUS_SAE_HASH_TO_ELEMENT) => {
                        if sae.process_commit(body).is_err() {
                            return self.fail("SAE: invalid commit from AP");
                        }
                        let conf = sae.confirm();
                        let c = frame::auth(self.own, self.bss.bssid, frame::AUTH_SAE, 2, 0, &conf);
                        self.state = State::SaeConfirming;
                        self.retries = 0;
                        alloc::vec![self.send(c, now)]
                    }
                    (2, 0) => {
                        if sae.process_confirm(body).is_err() {
                            return self.fail("SAE: confirm mismatch (wrong password?)");
                        }
                        self.pmk = sae.pmk().to_vec();
                        alloc::vec![self.assoc(now)]
                    }
                    (_, frame::STATUS_UNSUPPORTED_GROUP) => {
                        self.fail("SAE: AP does not support group 19")
                    }
                    _ => self.fail("SAE authentication rejected"),
                }
            }
            State::Associating => {
                let Some((status, aid, ies)) = frame::parse_assoc_response(f) else {
                    return Vec::new();
                };
                if status != 0 {
                    return self.fail("association rejected");
                }
                self.aid = aid;
                let qos = ie::iter(ies)
                    .any(|(id, b)| id == ie::VENDOR && b.starts_with(&[0x00, 0x50, 0xF2, 0x02]))
                    || ie::find(ies, ie::HT_CAPS).is_some();
                let mut out = alloc::vec![Output::Associated {
                    aid,
                    qos,
                    ies: ies.to_vec()
                }];
                self.last_tx = None;
                if self.security == Security::Open {
                    self.state = State::Connected;
                    out.push(Output::Connected);
                } else {
                    self.state = State::Handshake;
                    self.deadline = now + HANDSHAKE_MS;
                    self.supplicant = Some(Supplicant::new(
                        self.akm,
                        self.pairwise,
                        self.group,
                        &self.pmk,
                        self.bss.bssid,
                        self.own,
                        self.own_rsne.clone(),
                        self.ap_rsne.clone(),
                        rng,
                    ));
                }
                out
            }
            _ => Vec::new(),
        }
    }

    /// Process an EAPOL frame from the AP.
    pub fn rx_eapol(&mut self, body: &[u8], now: u64) -> Vec<Output> {
        let Some(s) = self.supplicant.as_mut() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for a in s.rx(body) {
            match a {
                eapol::Action::Send(f) => out.push(Output::TxEapol(f)),
                eapol::Action::InstallPairwise { cipher, key } => {
                    out.push(Output::InstallPairwise { cipher, key })
                }
                eapol::Action::InstallGroup {
                    idx,
                    cipher,
                    key,
                    rsc,
                } => out.push(Output::InstallGroup {
                    idx,
                    cipher,
                    key,
                    rsc,
                }),
                eapol::Action::InstallIgtk { idx, key, ipn } => {
                    if self.pmf {
                        out.push(Output::InstallIgtk { idx, key, ipn })
                    }
                }
                eapol::Action::Connected => {
                    self.state = State::Connected;
                    out.push(Output::Connected);
                }
                eapol::Action::Failed(why) => {
                    out.extend(self.fail(why));
                    let _ = now;
                    return out;
                }
            }
        }
        out
    }

    /// Retransmissions and timeouts; call every ~100 ms.
    pub fn tick(&mut self, now: u64) -> Vec<Output> {
        match self.state {
            State::Authenticating | State::SaeConfirming | State::Associating
                if now >= self.deadline =>
            {
                self.retries += 1;
                if self.retries > MAX_RETRIES {
                    return self.fail(match self.state {
                        State::Associating => "association timed out",
                        _ => "authentication timed out",
                    });
                }
                match self.last_tx.clone() {
                    Some(f) => alloc::vec![self.send(f, now)],
                    None => Vec::new(),
                }
            }
            State::Handshake if now >= self.deadline => {
                self.fail("4-way handshake timed out (wrong password?)")
            }
            _ => Vec::new(),
        }
    }

    /// Deauthenticate (user disconnect).
    pub fn disconnect(&mut self) -> Vec<Output> {
        self.state = State::Failed;
        alloc::vec![
            Output::TxMgmt(frame::deauth(self.own, self.bss.bssid, 3)),
            Output::Disconnected("disconnected by user")
        ]
    }

    /// Management frame protection negotiated with the AP.
    pub fn pmf(&self) -> bool {
        self.pmf
    }
    pub fn pmf_required(&self) -> bool {
        self.pmf_required
    }
    pub fn akm(&self) -> Akm {
        self.akm
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eapol::tests::{Authenticator, rng};
    use crate::sae::Sae;

    const ME: Mac = [0x02, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
    const AP: Mac = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];

    fn bss(security: Security, h2e: bool) -> BssInfo {
        let mut ies = Vec::new();
        ie::push(&mut ies, ie::SSID, b"home");
        ie::push(&mut ies, ie::DS_PARAMS, &[6]);
        let akms = match security {
            Security::Wpa2Psk => alloc::vec![Akm::Psk],
            Security::Wpa3Sae => alloc::vec![Akm::Psk, Akm::Sae],
            _ => Vec::new(),
        };
        if !akms.is_empty() {
            let r = Rsn {
                group: Cipher::Ccmp128,
                pairwise: alloc::vec![Cipher::Ccmp128],
                akms,
                caps: ie::RSN_CAP_MFPC,
                pmkids: Vec::new(),
                group_mgmt: Some(Cipher::BipCmac128),
            };
            ies.extend_from_slice(&r.element());
        }
        if h2e {
            ie::push(&mut ies, ie::RSNX, &[RSNX_SAE_H2E | 1]);
        }
        BssInfo {
            bssid: AP,
            ssid: b"home".to_vec(),
            capability: if akms_present(security) {
                0x0411
            } else {
                0x0401
            },
            beacon_interval: 100,
            channel: Some(6),
            band: crate::chan::Band::B2G,
            ies,
        }
    }

    fn akms_present(s: Security) -> bool {
        !matches!(s, Security::Open)
    }

    /// Rewrite a frame we sent as the AP's reply header (AP -> ME).
    fn from_ap(mut f: Vec<u8>) -> Vec<u8> {
        f[4..10].copy_from_slice(&ME);
        f[10..16].copy_from_slice(&AP);
        f[16..22].copy_from_slice(&AP);
        f
    }

    fn assoc_resp() -> Vec<u8> {
        let mut f = frame::auth(AP, AP, 0, 0, 0, &[]);
        f[0..2].copy_from_slice(&frame::fc(frame::TYPE_MGMT, frame::ST_ASSOC_RESP).to_le_bytes());
        f.truncate(24);
        f.extend_from_slice(&[0x11, 0x04, 0, 0, 0x01, 0xC0]);
        from_ap(f)
    }

    fn tx(out: &[Output]) -> Vec<u8> {
        out.iter()
            .find_map(|o| {
                if let Output::TxMgmt(f) = o {
                    Some(f.clone())
                } else {
                    None
                }
            })
            .expect("no frame sent")
    }

    #[test]
    fn open_network() {
        let mut r = rng();
        let (mut s, out) =
            Station::connect(ME, bss(Security::Open, false), b"", 0, &mut r).unwrap();
        let auth = tx(&out);
        assert_eq!(frame::parse_auth(&auth).unwrap().0, frame::AUTH_OPEN);
        let out = s.rx_mgmt(&from_ap(frame::auth(ME, AP, 0, 2, 0, &[])), 10, &mut r);
        assert_eq!(
            frame::Header::parse(&tx(&out)).unwrap().subtype(),
            frame::ST_ASSOC_REQ
        );
        let out = s.rx_mgmt(&assoc_resp(), 20, &mut r);
        assert!(out.contains(&Output::Connected));
        assert_eq!(s.aid, 1);
    }

    #[test]
    fn assoc_request_carries_ht_caps() {
        let mut r = rng();
        let mut b = bss(Security::Open, false);
        ie::push(
            &mut b.ies,
            caps::HT_CAPS,
            &caps::ht_caps(&caps::Profile::AX210),
        );
        let (mut s, _) = Station::connect(ME, b, b"", 0, &mut r).unwrap();
        s.profile = caps::Profile::AX210;
        let out = s.rx_mgmt(&from_ap(frame::auth(ME, AP, 0, 2, 0, &[])), 10, &mut r);
        let req = tx(&out);
        assert!(ie::find(&req[28..], caps::HT_CAPS).is_some());
        assert!(ie::find(&req[28..], caps::VHT_CAPS).is_none()); // 2.4 GHz
    }

    /// Full 4-way handshake against the test authenticator (whose
    /// addresses AA/SPA equal AP/ME here).
    fn handshake(s: &mut Station, pmk: &[u8], rsne: Vec<u8>) {
        let mut ap = Authenticator::new(s.akm, pmk, rsne);
        let out = s.rx_eapol(&ap.msg1(), 30);
        let Some(Output::TxEapol(m2)) = out.first() else {
            panic!("no msg2: {:?}", out)
        };
        let m3 = ap.msg3(m2).expect("AP rejected msg2");
        let out = s.rx_eapol(&m3, 40);
        let Some(Output::TxEapol(m4)) = out.first() else {
            panic!("no msg4: {:?}", out)
        };
        assert!(ap.check_msg4(m4));
        assert!(out.contains(&Output::InstallPairwise {
            cipher: Cipher::Ccmp128,
            key: ap.tk()
        }));
        assert!(out.iter().any(|o| matches!(o, Output::InstallGroup { .. })));
        assert!(out.contains(&Output::Connected));
        assert_eq!(s.state, State::Connected);
    }

    #[test]
    fn wpa2_network() {
        let mut r = rng();
        let b = bss(Security::Wpa2Psk, false);
        let (mut s, _) = Station::connect(ME, b.clone(), b"correct horse", 0, &mut r).unwrap();
        s.rx_mgmt(&from_ap(frame::auth(ME, AP, 0, 2, 0, &[])), 10, &mut r);
        let out = s.rx_mgmt(&assoc_resp(), 20, &mut r);
        assert!(matches!(out[0], Output::Associated { aid: 1, .. }));
        assert_eq!(s.state, State::Handshake);
        // Addresses here differ from the eapol tests' AA/SPA, so run the
        // authenticator directly against the supplicant keys.
        let pmk = crypto::wpa_psk(b"correct horse", b"home");
        assert_eq!(s.pmk, pmk.to_vec());
        let rsne = ie::find(&b.ies, ie::RSN).map(|x| {
            let mut e = Vec::new();
            ie::push(&mut e, ie::RSN, x);
            e
        });
        handshake(&mut s, &pmk, rsne.unwrap());
        assert!(s.tick(20 + HANDSHAKE_MS + 1).is_empty());
    }

    #[test]
    fn wpa3_sae_network() {
        for h2e in [false, true] {
            let mut r = rng();
            let b = bss(Security::Wpa3Sae, h2e);
            let (mut s, out) = Station::connect(ME, b, b"sae password", 0, &mut r).unwrap();
            let commit = tx(&out);
            let (algo, seq, status, body) = frame::parse_auth(&commit).unwrap();
            assert_eq!((algo, seq), (frame::AUTH_SAE, 1));
            assert_eq!(status == frame::STATUS_SAE_HASH_TO_ELEMENT, h2e);
            // The AP side.
            let mut ap = if h2e {
                Sae::new_h2e(&sae::h2e_pt(b"home", b"sae password", None), AP, ME, &mut r)
            } else {
                Sae::new(b"sae password", AP, ME, &mut r).unwrap()
            };
            ap.process_commit(body).unwrap();
            let out = s.rx_mgmt(
                &from_ap(frame::auth(
                    ME,
                    AP,
                    frame::AUTH_SAE,
                    1,
                    ap.commit_status(),
                    &ap.commit(),
                )),
                10,
                &mut r,
            );
            let conf = tx(&out);
            let (_, seq, _, cbody) = frame::parse_auth(&conf).unwrap();
            assert_eq!(seq, 2);
            ap.process_confirm(cbody).unwrap();
            let out = s.rx_mgmt(
                &from_ap(frame::auth(ME, AP, frame::AUTH_SAE, 2, 0, &ap.confirm())),
                20,
                &mut r,
            );
            let areq = tx(&out);
            assert_eq!(
                frame::Header::parse(&areq).unwrap().subtype(),
                frame::ST_ASSOC_REQ
            );
            let rsn = Rsn::parse(ie::find(&areq[28..], ie::RSN).unwrap()).unwrap();
            assert_eq!(rsn.akms, alloc::vec![Akm::Sae]);
            assert!(rsn.caps & ie::RSN_CAP_MFPR != 0);
            assert_eq!(ie::find(&areq[28..], ie::RSNX).is_some(), h2e);
            assert_eq!(s.pmk, ap.pmk().to_vec());
            s.rx_mgmt(&assoc_resp(), 30, &mut r);
            let mut e = Vec::new();
            ie::push(&mut e, ie::RSN, ie::find(&s.bss.ies, ie::RSN).unwrap());
            let pmk = ap.pmk();
            handshake(&mut s, &pmk, e);
        }
    }

    #[test]
    fn retries_then_timeout() {
        let mut r = rng();
        let (mut s, _) = Station::connect(ME, bss(Security::Open, false), b"", 0, &mut r).unwrap();
        for i in 1..=MAX_RETRIES as u64 {
            assert!(matches!(s.tick(i * RETRY_MS + 1)[0], Output::TxMgmt(_)));
        }
        assert_eq!(
            s.tick(10 * RETRY_MS),
            alloc::vec![Output::Disconnected("authentication timed out")]
        );
    }

    #[test]
    fn rejects_short_passphrase_and_enterprise() {
        let mut r = rng();
        assert!(Station::connect(ME, bss(Security::Wpa2Psk, false), b"short", 0, &mut r).is_err());
        assert!(Station::connect(ME, bss(Security::Wpa2Psk, false), b"", 0, &mut r).is_err());
    }
}
