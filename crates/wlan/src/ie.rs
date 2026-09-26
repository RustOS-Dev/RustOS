//! Information elements, including RSN negotiation.

use alloc::vec::Vec;

pub const SSID: u8 = 0;
pub const SUPP_RATES: u8 = 1;
pub const DS_PARAMS: u8 = 3;
pub const TIM: u8 = 5;
pub const COUNTRY: u8 = 7;
pub const HT_CAPS: u8 = 45;
pub const RSN: u8 = 48;
pub const EXT_RATES: u8 = 50;
pub const HT_OP: u8 = 61;
pub const EXT_CAPS: u8 = 127;
pub const VHT_CAPS: u8 = 191;
pub const VENDOR: u8 = 221;
pub const RSNX: u8 = 244;
pub const EXTENSION: u8 = 255;

/// Iterate over (id, body) pairs; stops at the first malformed element.
pub fn iter(mut ies: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    core::iter::from_fn(move || {
        if ies.len() < 2 {
            return None;
        }
        let (id, len) = (ies[0], ies[1] as usize);
        if ies.len() < 2 + len {
            return None;
        }
        let body = &ies[2..2 + len];
        ies = &ies[2 + len..];
        Some((id, body))
    })
}

pub fn find(ies: &[u8], id: u8) -> Option<&[u8]> {
    iter(ies).find(|(i, _)| *i == id).map(|(_, b)| b)
}

pub fn push(out: &mut Vec<u8>, id: u8, body: &[u8]) {
    out.push(id);
    out.push(body.len() as u8);
    out.extend_from_slice(body);
}

/// Cipher suites (OUI 00-0F-AC).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cipher {
    Wep40,
    Tkip,
    Ccmp128,
    Wep104,
    BipCmac128,
    Gcmp128,
    Gcmp256,
    Ccmp256,
    BipGmac128,
    BipGmac256,
    BipCmac256,
    Unknown(u32),
}

impl Cipher {
    pub fn from_suite(s: u32) -> Cipher {
        match s {
            0x000FAC01 => Cipher::Wep40,
            0x000FAC02 => Cipher::Tkip,
            0x000FAC04 => Cipher::Ccmp128,
            0x000FAC05 => Cipher::Wep104,
            0x000FAC06 => Cipher::BipCmac128,
            0x000FAC08 => Cipher::Gcmp128,
            0x000FAC09 => Cipher::Gcmp256,
            0x000FAC0A => Cipher::Ccmp256,
            0x000FAC0B => Cipher::BipGmac128,
            0x000FAC0C => Cipher::BipGmac256,
            0x000FAC0D => Cipher::BipCmac256,
            s => Cipher::Unknown(s),
        }
    }
    pub fn suite(self) -> u32 {
        match self {
            Cipher::Wep40 => 0x000FAC01,
            Cipher::Tkip => 0x000FAC02,
            Cipher::Ccmp128 => 0x000FAC04,
            Cipher::Wep104 => 0x000FAC05,
            Cipher::BipCmac128 => 0x000FAC06,
            Cipher::Gcmp128 => 0x000FAC08,
            Cipher::Gcmp256 => 0x000FAC09,
            Cipher::Ccmp256 => 0x000FAC0A,
            Cipher::BipGmac128 => 0x000FAC0B,
            Cipher::BipGmac256 => 0x000FAC0C,
            Cipher::BipCmac256 => 0x000FAC0D,
            Cipher::Unknown(s) => s,
        }
    }
    /// Temporal key length in bytes.
    pub fn key_len(self) -> usize {
        match self {
            Cipher::Gcmp256 | Cipher::Ccmp256 | Cipher::BipGmac256 | Cipher::BipCmac256 => 32,
            Cipher::Tkip => 32,
            Cipher::Wep40 => 5,
            Cipher::Wep104 => 13,
            _ => 16,
        }
    }
}

/// Authentication and key management suites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Akm {
    Ieee8021x,
    Psk,
    FtPsk,
    PskSha256,
    Sae,
    FtSae,
    SaeExt,
    Owe,
    Unknown(u32),
}

impl Akm {
    pub fn from_suite(s: u32) -> Akm {
        match s {
            0x000FAC01 => Akm::Ieee8021x,
            0x000FAC02 => Akm::Psk,
            0x000FAC04 => Akm::FtPsk,
            0x000FAC06 => Akm::PskSha256,
            0x000FAC08 => Akm::Sae,
            0x000FAC09 => Akm::FtSae,
            0x000FAC18 => Akm::SaeExt,
            0x000FAC12 => Akm::Owe,
            s => Akm::Unknown(s),
        }
    }
    pub fn suite(self) -> u32 {
        match self {
            Akm::Ieee8021x => 0x000FAC01,
            Akm::Psk => 0x000FAC02,
            Akm::FtPsk => 0x000FAC04,
            Akm::PskSha256 => 0x000FAC06,
            Akm::Sae => 0x000FAC08,
            Akm::FtSae => 0x000FAC09,
            Akm::SaeExt => 0x000FAC18,
            Akm::Owe => 0x000FAC12,
            Akm::Unknown(s) => s,
        }
    }
    /// Uses the SHA-256 KDF and AES-CMAC MIC (key descriptor version 3 or
    /// AKM-defined) instead of PRF-SHA1 / HMAC-SHA1.
    pub fn sha256(self) -> bool {
        matches!(self, Akm::PskSha256 | Akm::Sae | Akm::FtSae | Akm::FtPsk)
    }
}

pub const RSN_CAP_MFPR: u16 = 1 << 6;
pub const RSN_CAP_MFPC: u16 = 1 << 7;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rsn {
    pub group: Cipher,
    pub pairwise: Vec<Cipher>,
    pub akms: Vec<Akm>,
    pub caps: u16,
    pub pmkids: Vec<[u8; 16]>,
    pub group_mgmt: Option<Cipher>,
}

fn suite_at(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

impl Rsn {
    /// Parse an RSN element body.
    pub fn parse(b: &[u8]) -> Option<Rsn> {
        if b.len() < 2 || u16::from_le_bytes([b[0], b[1]]) != 1 {
            return None;
        }
        let mut rsn = Rsn {
            group: Cipher::Ccmp128,
            pairwise: alloc::vec![Cipher::Ccmp128],
            akms: alloc::vec![Akm::Ieee8021x],
            caps: 0,
            pmkids: Vec::new(),
            group_mgmt: None,
        };
        let mut o = 2;
        if let Some(s) = suite_at(b, o) {
            rsn.group = Cipher::from_suite(s);
            o += 4;
        } else {
            return Some(rsn);
        }
        let list = |o: &mut usize| -> Option<Vec<u32>> {
            let n = u16::from_le_bytes([*b.get(*o)?, *b.get(*o + 1)?]) as usize;
            *o += 2;
            let mut v = Vec::new();
            for _ in 0..n {
                v.push(suite_at(b, *o)?);
                *o += 4;
            }
            Some(v)
        };
        if o >= b.len() {
            return Some(rsn);
        }
        rsn.pairwise = list(&mut o)?.into_iter().map(Cipher::from_suite).collect();
        if o >= b.len() {
            return Some(rsn);
        }
        rsn.akms = list(&mut o)?.into_iter().map(Akm::from_suite).collect();
        if o + 2 <= b.len() {
            rsn.caps = u16::from_le_bytes([b[o], b[o + 1]]);
            o += 2;
        }
        if o + 2 <= b.len() {
            let n = u16::from_le_bytes([b[o], b[o + 1]]) as usize;
            o += 2;
            for _ in 0..n {
                let id: [u8; 16] = b.get(o..o + 16)?.try_into().ok()?;
                rsn.pmkids.push(id);
                o += 16;
            }
        }
        if let Some(s) = suite_at(b, o) {
            rsn.group_mgmt = Some(Cipher::from_suite(s));
        }
        Some(rsn)
    }

    /// Encode as an RSN element body (for the association request).
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&self.group.suite().to_be_bytes());
        b.extend_from_slice(&(self.pairwise.len() as u16).to_le_bytes());
        for p in &self.pairwise {
            b.extend_from_slice(&p.suite().to_be_bytes());
        }
        b.extend_from_slice(&(self.akms.len() as u16).to_le_bytes());
        for a in &self.akms {
            b.extend_from_slice(&a.suite().to_be_bytes());
        }
        b.extend_from_slice(&self.caps.to_le_bytes());
        if !self.pmkids.is_empty() || self.group_mgmt.is_some() {
            b.extend_from_slice(&(self.pmkids.len() as u16).to_le_bytes());
            for p in &self.pmkids {
                b.extend_from_slice(p);
            }
        }
        if let Some(g) = self.group_mgmt {
            b.extend_from_slice(&g.suite().to_be_bytes());
        }
        b
    }

    /// Full element (id + length + body).
    pub fn element(&self) -> Vec<u8> {
        let body = self.encode();
        let mut e = Vec::with_capacity(body.len() + 2);
        push(&mut e, RSN, &body);
        e
    }
}

/// Security offered by a BSS, in order of our preference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Security {
    Open,
    Wep,
    Wpa1,
    Wpa2Psk,
    Wpa3Sae,
    Enterprise,
}

impl Security {
    pub fn name(self) -> &'static str {
        match self {
            Security::Open => "open",
            Security::Wep => "WEP",
            Security::Wpa1 => "WPA1",
            Security::Wpa2Psk => "WPA2-PSK",
            Security::Wpa3Sae => "WPA3-SAE",
            Security::Enterprise => "WPA-Enterprise",
        }
    }
}

/// Classify a BSS from its capability field privacy bit and elements.
/// Transition-mode networks (PSK + SAE) report WPA3.
pub fn security(capability: u16, ies: &[u8]) -> Security {
    if let Some(rsn) = find(ies, RSN).and_then(Rsn::parse) {
        if rsn.akms.iter().any(|a| matches!(a, Akm::Sae | Akm::FtSae | Akm::SaeExt)) {
            return Security::Wpa3Sae;
        }
        if rsn.akms.iter().any(|a| matches!(a, Akm::Psk | Akm::PskSha256 | Akm::FtPsk)) {
            return Security::Wpa2Psk;
        }
        return Security::Enterprise;
    }
    let wpa1 = iter(ies).any(|(id, b)| id == VENDOR && b.starts_with(&[0x00, 0x50, 0xF2, 0x01]));
    if wpa1 {
        Security::Wpa1
    } else if capability & 0x10 != 0 {
        Security::Wep
    } else {
        Security::Open
    }
}

/// Supported rates element bodies for 2.4 GHz (11b + 11g) or 5/6 GHz.
pub fn rates(band_2g: bool) -> (Vec<u8>, Vec<u8>) {
    if band_2g {
        (
            alloc::vec![0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24],
            alloc::vec![0x30, 0x48, 0x60, 0x6c],
        )
    } else {
        (alloc::vec![0x8c, 0x12, 0x98, 0x24, 0xb0, 0x48, 0x60, 0x6c], Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rsn_round_trip() {
        // WPA2/WPA3 transition mode AP, PMF capable.
        let r = Rsn {
            group: Cipher::Ccmp128,
            pairwise: alloc::vec![Cipher::Ccmp128],
            akms: alloc::vec![Akm::Psk, Akm::Sae],
            caps: RSN_CAP_MFPC,
            pmkids: Vec::new(),
            group_mgmt: Some(Cipher::BipCmac128),
        };
        let e = r.element();
        assert_eq!(e[0], RSN);
        let p = Rsn::parse(&e[2..]).unwrap();
        assert_eq!(p, r);
        assert_eq!(security(0x11, &e), Security::Wpa3Sae);
    }

    #[test]
    fn classic_wpa2_ie() {
        // Common WPA2-PSK CCMP element as broadcast by most APs.
        let body = [
            1, 0, 0x00, 0x0f, 0xac, 4, 1, 0, 0x00, 0x0f, 0xac, 4, 1, 0, 0x00, 0x0f, 0xac, 2, 0x0c, 0,
        ];
        let r = Rsn::parse(&body).unwrap();
        assert_eq!(r.akms, alloc::vec![Akm::Psk]);
        assert_eq!(r.caps, 0x000c);
        assert_eq!(r.group_mgmt, None);
        let mut ies = Vec::new();
        push(&mut ies, SSID, b"home");
        push(&mut ies, RSN, &body);
        assert_eq!(security(0x411, &ies), Security::Wpa2Psk);
        assert_eq!(find(&ies, SSID), Some(&b"home"[..]));
    }

    #[test]
    fn open_and_wep() {
        assert_eq!(security(0x01, &[]), Security::Open);
        assert_eq!(security(0x11, &[]), Security::Wep);
    }
}
