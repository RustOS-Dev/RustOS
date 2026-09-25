//! USB 2 and USB 3 hub class driver.
//!
//! Each hub gets a kernel thread that polls its port status every 250 ms
//! and attaches or detaches downstream devices.

use super::xhci::HubConfig;
use super::{
    REQ_CLEAR_FEATURE, REQ_GET_DESCRIPTOR, REQ_GET_STATUS, REQ_SET_FEATURE, Speed, UsbDevice,
};
use crate::errno::*;
use alloc::sync::Arc;
use usb_desc::{CLASS_HUB, HubDescriptor, Interface, TransferType};

const PORT_RESET: u16 = 4;
const PORT_POWER: u16 = 8;
const C_PORT_CONNECTION: u16 = 16;
const C_PORT_RESET: u16 = 20;
const BH_PORT_RESET: u16 = 28;
const SET_HUB_DEPTH: u8 = 12;

/// Feature selector that clears change bit `bit` of wPortChange.
fn change_feature(bit: u16, ss: bool) -> u16 {
    match (bit, ss) {
        (5, true) => 29, // C_BH_PORT_RESET
        (6, true) => 25, // C_PORT_LINK_STATE
        (7, true) => 26, // C_PORT_CONFIG_ERROR
        _ => 16 + bit,
    }
}

/// (status, change) for a hub port.
fn port_status(hub: &UsbDevice, port: u8) -> KResult<(u16, u16)> {
    let b = hub.control_in(0x23, REQ_GET_STATUS, 0, port as u16, 4)?;
    if b.len() < 4 {
        return Err(EIO);
    }
    Ok((
        u16::from_le_bytes([b[0], b[1]]),
        u16::from_le_bytes([b[2], b[3]]),
    ))
}

fn clear(hub: &UsbDevice, port: u8, feature: u16) {
    let _ = hub.control_out(0x23, REQ_CLEAR_FEATURE, feature, port as u16, &[]);
}

fn set(hub: &UsbDevice, port: u8, feature: u16) -> KResult<()> {
    hub.control_out(0x23, REQ_SET_FEATURE, feature, port as u16, &[])
}

pub fn probe(dev: &Arc<UsbDevice>, iface: &Interface) -> bool {
    if iface.class != CLASS_HUB {
        return false;
    }
    let ss = dev.speed >= Speed::Super;
    let dt = if ss {
        usb_desc::DT_SS_HUB
    } else {
        usb_desc::DT_HUB
    };
    let Ok(raw) = dev.control_in(0x20, REQ_GET_DESCRIPTOR, (dt as u16) << 8, 0, 12) else {
        return false;
    };
    let Some(hd) = HubDescriptor::parse(&raw) else {
        return false;
    };
    let eps: alloc::vec::Vec<_> = iface
        .find_endpoint(TransferType::Interrupt, true)
        .into_iter()
        .collect();
    let cfg = HubConfig {
        ports: hd.ports,
        ttt: if ss { 0 } else { hd.tt_think_time() },
        mtt: dev.desc.lock().protocol == 2,
    };
    if let Err(e) = dev.configure_hub(&eps, cfg) {
        crate::println!("[usb] {}: hub configure failed: {}", dev.name(), e);
        return false;
    }
    if ss {
        let _ = dev.control_out(0x20, SET_HUB_DEPTH, (dev.tier - 1) as u16, 0, &[]);
    }
    for p in 1..=hd.ports {
        let _ = set(dev, p, PORT_POWER);
    }
    crate::println!(
        "[usb] {}: {}-port {} hub",
        dev.name(),
        hd.ports,
        if ss { "USB 3" } else { "USB 2" }
    );
    let hub = dev.clone();
    let settle = hd.power_on_ms.max(100) as u64;
    crate::sched::spawn(&alloc::format!("usb-hub{}", dev.slot), move || {
        crate::time::sleep_ms(settle);
        hub_thread(hub, hd.ports, ss)
    });
    true
}

fn hub_thread(hub: Arc<UsbDevice>, ports: u8, ss: bool) {
    let mut first = true;
    while !hub.is_gone() {
        for port in 1..=ports {
            if hub.is_gone() {
                return;
            }
            let Ok((status, change)) = port_status(&hub, port) else {
                continue;
            };
            let conn_change = change & 1 != 0;
            // Acknowledge every change bit we might see.
            for bit in 0..8u16 {
                if change & (1 << bit) != 0 {
                    clear(&hub, port, change_feature(bit, ss));
                }
            }
            if !(conn_change || first) {
                continue;
            }
            let connected = status & 1 != 0;
            let existing = hub.children.lock().get(&port).cloned();
            if let Some(d) = existing
                && (!connected || conn_change)
            {
                hub.children.lock().remove(&port);
                super::detach(&d);
            }
            if connected && !hub.children.lock().contains_key(&port) {
                crate::time::sleep_ms(100);
                match reset_port(&hub, port, ss) {
                    Ok(speed) => match super::attach(&hub.hc, Some(&hub), port, speed) {
                        Ok(d) => {
                            hub.children.lock().insert(port, d);
                        }
                        Err(e) => crate::println!(
                            "[usb] {} port {}: attach failed: {}",
                            hub.name(),
                            port,
                            e
                        ),
                    },
                    Err(e) => {
                        crate::println!("[usb] {} port {}: reset failed: {}", hub.name(), port, e)
                    }
                }
            }
        }
        first = false;
        crate::time::sleep_ms(250);
    }
}

fn reset_port(hub: &UsbDevice, port: u8, ss: bool) -> KResult<Speed> {
    set(hub, port, PORT_RESET)?;
    let deadline = crate::time::Deadline::after_ms(1000);
    loop {
        let (status, change) = port_status(hub, port)?;
        if change & (1 << 4) != 0 || (ss && change & (1 << 5) != 0) {
            clear(hub, port, C_PORT_RESET);
            if ss {
                clear(hub, port, change_feature(5, true));
                clear(hub, port, change_feature(6, true));
            }
            crate::time::sleep_ms(10);
            let (status, _) = port_status(hub, port)?;
            if status & 2 == 0 {
                return Err(EIO);
            }
            return Ok(if ss {
                Speed::Super
            } else if status & (1 << 9) != 0 {
                Speed::Low
            } else if status & (1 << 10) != 0 {
                Speed::High
            } else {
                Speed::Full
            });
        }
        if status & 1 == 0 {
            return Err(ENODEV);
        }
        if deadline.expired() {
            if ss {
                let _ = set(hub, port, BH_PORT_RESET);
            }
            return Err(ETIMEDOUT);
        }
        crate::time::sleep_ms(10);
    }
}

#[allow(dead_code)]
const _: u16 = C_PORT_CONNECTION;
