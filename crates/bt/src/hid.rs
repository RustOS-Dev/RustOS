//! HID over GATT (HOGP) discovery for LE keyboards and mice, and the
//! BR/EDR HID protocol (HIDP) headers and boot-protocol descriptors.

use crate::att::Uuid;
use crate::gatt::{self, Bearer, Client, Error};
use alloc::string::String;
use alloc::vec::Vec;

pub const SVC_GAP: u16 = 0x1800;
pub const SVC_DEVICE_INFO: u16 = 0x180A;
pub const SVC_BATTERY: u16 = 0x180F;
pub const SVC_HID: u16 = 0x1812;
pub const CHR_DEVICE_NAME: u16 = 0x2A00;
pub const CHR_BATTERY_LEVEL: u16 = 0x2A19;
pub const CHR_PNP_ID: u16 = 0x2A50;
pub const CHR_HID_INFO: u16 = 0x2A4A;
pub const CHR_REPORT_MAP: u16 = 0x2A4B;
pub const CHR_CONTROL_POINT: u16 = 0x2A4C;
pub const CHR_REPORT: u16 = 0x2A4D;
pub const CHR_PROTOCOL_MODE: u16 = 0x2A4E;
pub const DESC_REPORT_REFERENCE: u16 = 0x2908;

/// Report types in a Report Reference descriptor.
pub const REPORT_INPUT: u8 = 1;
pub const REPORT_OUTPUT: u8 = 2;

/// An input report characteristic: notifications on `handle` carry
/// report `id` (without the ID byte).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputReport {
    pub handle: u16,
    pub id: u8,
}

/// What HOGP discovery found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HogpDevice {
    pub name: Option<String>,
    /// (vendor ID source, vendor, product, version) from the PnP ID.
    pub pnp: Option<(u8, u16, u16, u16)>,
    pub report_map: Vec<u8>,
    pub inputs: Vec<InputReport>,
    /// Output report for keyboard LEDs: (value handle, report ID).
    pub led_output: Option<(u16, u8)>,
    pub battery: Option<u8>,
}

impl HogpDevice {
    /// The report as the HID parser expects it: prefixed with the report
    /// ID when the descriptor uses IDs.
    pub fn report(&self, handle: u16, value: &[u8]) -> Option<Vec<u8>> {
        let r = self.inputs.iter().find(|r| r.handle == handle)?;
        let mut v = Vec::with_capacity(value.len() + 1);
        if r.id != 0 {
            v.push(r.id);
        }
        v.extend_from_slice(value);
        Some(v)
    }
}

/// Discover the HID service, read the report map, enable notifications
/// on the input reports and select report protocol.
pub fn discover<B: Bearer>(c: &mut Client<B>) -> Result<HogpDevice, Error> {
    let svcs = c.services()?;
    let mut dev = HogpDevice::default();
    let find = |u: u16| svcs.iter().filter(move |s| s.uuid == Uuid::U16(u));
    for s in find(SVC_GAP) {
        for ch in c.characteristics(s)? {
            if ch.uuid == Uuid::U16(CHR_DEVICE_NAME)
                && let Ok(v) = c.read(ch.value)
            {
                dev.name = Some(String::from_utf8_lossy(&v).into_owned());
            }
        }
    }
    for s in find(SVC_DEVICE_INFO) {
        for ch in c.characteristics(s)? {
            if ch.uuid == Uuid::U16(CHR_PNP_ID)
                && let Ok(v) = c.read(ch.value)
                && v.len() >= 7
            {
                let w = |o: usize| u16::from_le_bytes([v[o], v[o + 1]]);
                dev.pnp = Some((v[0], w(1), w(3), w(5)));
            }
        }
    }
    for s in find(SVC_BATTERY) {
        for ch in c.characteristics(s)? {
            if ch.uuid == Uuid::U16(CHR_BATTERY_LEVEL)
                && let Ok(v) = c.read(ch.value)
            {
                dev.battery = v.first().copied();
            }
        }
    }
    let mut found = false;
    for s in find(SVC_HID) {
        found = true;
        for ch in c.characteristics(s)? {
            match ch.uuid {
                Uuid::U16(CHR_REPORT_MAP) if dev.report_map.is_empty() => {
                    dev.report_map = c.read(ch.value)?;
                }
                Uuid::U16(CHR_PROTOCOL_MODE) if ch.props & gatt::PROP_WRITE_NO_RSP != 0 => {
                    c.write_cmd(ch.value, &[1])?;
                }
                Uuid::U16(CHR_REPORT) => {
                    let descs = c.descriptors(&ch)?;
                    let mut id = 0;
                    let mut ty = REPORT_INPUT;
                    let mut cccd = None;
                    for (h, u) in descs {
                        match u {
                            Uuid::U16(DESC_REPORT_REFERENCE) => {
                                let v = c.read(h)?;
                                id = v.first().copied().unwrap_or(0);
                                ty = v.get(1).copied().unwrap_or(REPORT_INPUT);
                            }
                            Uuid::U16(gatt::CCCD) => cccd = Some(h),
                            _ => {}
                        }
                    }
                    match ty {
                        REPORT_INPUT if ch.props & gatt::PROP_NOTIFY != 0 => {
                            if let Some(h) = cccd {
                                c.subscribe(h, false)?;
                            }
                            dev.inputs.push(InputReport {
                                handle: ch.value,
                                id,
                            });
                        }
                        REPORT_OUTPUT if dev.led_output.is_none() => {
                            dev.led_output = Some((ch.value, id));
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
    if !found || dev.report_map.is_empty() {
        return Err(Error::Protocol);
    }
    Ok(dev)
}

// ---- BR/EDR HID (HIDP) ----

/// HIDP transaction types (high nibble of the header byte).
pub const HIDP_HANDSHAKE: u8 = 0x00;
pub const HIDP_CONTROL: u8 = 0x10;
pub const HIDP_SET_PROTOCOL: u8 = 0x70;
pub const HIDP_DATA: u8 = 0xA0;
/// DATA parameter: input and output reports.
pub const HIDP_INPUT: u8 = 0x01;
pub const HIDP_OUTPUT: u8 = 0x02;
/// HID_CONTROL parameter: virtual cable unplug.
pub const HIDP_UNPLUG: u8 = 0x05;

/// An input report from the interrupt channel (header stripped).
pub fn hidp_input(p: &[u8]) -> Option<&[u8]> {
    (*p.first()? == HIDP_DATA | HIDP_INPUT).then(|| &p[1..])
}

/// SET_PROTOCOL on the control channel (true: report protocol).
pub fn hidp_set_protocol(report: bool) -> [u8; 1] {
    [HIDP_SET_PROTOCOL | report as u8]
}

/// Keyboard LEDs as an output report on the interrupt channel.
pub fn hidp_output(id: u8, data: &[u8]) -> Vec<u8> {
    let mut v = alloc::vec![HIDP_DATA | HIDP_OUTPUT];
    if id != 0 {
        v.push(id);
    }
    v.extend_from_slice(data);
    v
}

/// Report descriptor for boot protocol over HIDP: the boot keyboard as
/// report 1 and the boot mouse as report 2 (the IDs boot-protocol
/// devices send).
pub const BOOT_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x85, 0x01, // keyboard, ID 1
    0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81,
    0x02, // modifiers
    0x95, 0x01, 0x75, 0x08, 0x81, 0x01, // reserved
    0x95, 0x05, 0x75, 0x01, 0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x91, 0x02, // LEDs
    0x95, 0x01, 0x75, 0x03, 0x91, 0x01, // padding
    0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19, 0x00, 0x29, 0x65, 0x81,
    0x00, // keys
    0xC0, //
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x02, 0x09, 0x01, 0xA1, 0x00, // mouse, ID 2
    0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81,
    0x02, // buttons
    0x95, 0x01, 0x75, 0x05, 0x81, 0x01, // padding
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x02, 0x81,
    0x06, // X, Y
    0xC0, 0xC0,
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gatt::testserver::Server;
    use crate::gatt::{CCCD, PROP_NOTIFY, PROP_READ, PROP_WRITE, PROP_WRITE_NO_RSP};

    pub fn keyboard_server() -> (Server, u16, u16) {
        let mut s = Server::default();
        s.service(SVC_GAP);
        s.characteristic(CHR_DEVICE_NAME, PROP_READ, b"BLE Keys");
        s.service(SVC_DEVICE_INFO);
        s.characteristic(
            CHR_PNP_ID,
            PROP_READ,
            &[2, 0x6D, 0x04, 0x01, 0xB3, 0x11, 0x01],
        );
        s.service(SVC_HID);
        s.characteristic(CHR_HID_INFO, PROP_READ, &[0x11, 0x01, 0, 2]);
        s.characteristic(CHR_REPORT_MAP, PROP_READ, BOOT_DESCRIPTOR);
        s.characteristic(CHR_PROTOCOL_MODE, PROP_READ | PROP_WRITE_NO_RSP, &[1]);
        let kb = s.characteristic(CHR_REPORT, PROP_READ | PROP_NOTIFY, &[0; 8]);
        let cccd = s.add(CCCD, &[0, 0]);
        s.add(DESC_REPORT_REFERENCE, &[1, REPORT_INPUT]);
        s.characteristic(CHR_REPORT, PROP_READ | PROP_NOTIFY, &[0; 3]);
        s.add(CCCD, &[0, 0]);
        s.add(DESC_REPORT_REFERENCE, &[2, REPORT_INPUT]);
        s.characteristic(CHR_REPORT, PROP_READ | PROP_WRITE, &[0]);
        s.add(DESC_REPORT_REFERENCE, &[1, REPORT_OUTPUT]);
        s.characteristic(CHR_CONTROL_POINT, PROP_WRITE_NO_RSP, &[0]);
        (s, kb, cccd)
    }

    #[test]
    fn hogp() {
        let (mut s, kb, cccd) = keyboard_server();
        s.secure = alloc::vec![cccd];
        {
            // The CCCD needs encryption: discovery reports it.
            let mut c = Client::new(&mut s);
            assert!(discover(&mut c).unwrap_err().needs_security());
        }
        s.encrypted = true;
        let mut c = Client::new(&mut s);
        let d = discover(&mut c).unwrap();
        assert_eq!(d.name.as_deref(), Some("BLE Keys"));
        assert_eq!(d.pnp, Some((2, 0x046D, 0xB301, 0x0111)));
        assert_eq!(d.report_map, BOOT_DESCRIPTOR);
        assert_eq!(d.inputs.len(), 2);
        assert_eq!(d.inputs[0], InputReport { handle: kb, id: 1 });
        assert!(d.led_output.is_some());
        assert!(s.written.contains(&(cccd, alloc::vec![1, 0])));
        assert_eq!(
            d.report(kb, &[0, 0, 4, 0, 0, 0, 0, 0]).unwrap()[..3],
            [1, 0, 0]
        );
    }

    #[test]
    fn hidp() {
        assert_eq!(hidp_input(&[0xA1, 1, 2]), Some(&[1, 2][..]));
        assert_eq!(hidp_input(&[0xA2, 1]), None);
        assert_eq!(hidp_set_protocol(false), [0x70]);
        assert_eq!(hidp_output(1, &[2]), [0xA2, 1, 2]);
    }
}
