//! IEEE 802.11 station logic shared by the kernel's wireless drivers.
//!
//! Everything here is pure data processing (no hardware access) so it can
//! be tested on the host: frame and information-element encoding, RSN
//! negotiation, WPA2-PSK (4-way and group key handshakes) and WPA3-SAE
//! (hunting-and-pecking and hash-to-element), and a station state machine
//! that tells a driver which frames to send and which keys to install.

#![no_std]

extern crate alloc;

pub mod ba;
pub mod caps;
pub mod crypto;
pub mod eapol;
pub mod frame;
pub mod ie;
pub mod sae;
pub mod sta;

pub type Mac = [u8; 6];

pub const BROADCAST: Mac = [0xFF; 6];
