// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! A USB mouse driver, for firmware that has none (OVMF, and many PCs
//! outside their own setup screens): USB mice support the HID "boot
//! protocol", 3-byte reports (buttons, dx, dy) on an interrupt endpoint,
//! which the firmware's USB stack lets us read with the USB I/O protocol.
//!
//! Mice that a firmware driver already handles (they have a pointer
//! protocol on their handle) are left to it.

use alloc::vec::Vec;
use uefi::boot::{self, OpenProtocolParams, ScopedProtocol};
use uefi::proto::console::pointer::Pointer;
use uefi::proto::usb::io::{ControlTransfer, UsbIo};

const HID_CLASS: u8 = 3;
const BOOT_SUBCLASS: u8 = 1;
const MOUSE_PROTOCOL: u8 = 2;
/// HID class requests (host to device, class, interface).
const CLASS_INTERFACE_OUT: u8 = 0x21;
const SET_IDLE: u8 = 0x0a;
const SET_PROTOCOL: u8 = 0x0b;

pub struct UsbMouse {
    io: ScopedProtocol<UsbIo>,
    /// The interrupt IN endpoint, without the direction bit.
    endpoint: u8,
    packet_size: usize,
}

/// One report: movement in counts, the wheel in notches (up: positive),
/// and the buttons.
pub struct Report {
    pub dx: i32,
    pub dy: i32,
    pub wheel: i32,
    pub left: bool,
    pub right: bool,
}

/// Parses a boot protocol report. Its three bytes are all the protocol
/// promises, but most mice send the wheel as a fourth (Linux's boot
/// protocol driver reads it too).
pub fn parse_report(data: &[u8]) -> Option<Report> {
    let [buttons, dx, dy, ..] = *data else { return None };
    let wheel = data.get(3).map_or(0, |w| *w as i8 as i32);
    Some(Report { dx: dx as i8 as i32, dy: dy as i8 as i32, wheel, left: buttons & 1 != 0, right: buttons & 2 != 0 })
}

fn has_pointer(handle: uefi::Handle) -> bool {
    // SAFETY: Only tests whether the protocol is there.
    boot::test_protocol::<Pointer>(OpenProtocolParams { handle, agent: boot::image_handle(), controller: None })
        .unwrap_or(false)
}

fn open(handle: uefi::Handle) -> Option<UsbMouse> {
    if has_pointer(handle) {
        return None;
    }
    let mut io = crate::uefi_platform::open_shared::<UsbIo>(handle)?;
    let interface = io.interface_descriptor().ok()?;
    if (interface.interface_class, interface.interface_subclass, interface.interface_protocol)
        != (HID_CLASS, BOOT_SUBCLASS, MOUSE_PROTOCOL)
    {
        return None;
    }
    let endpoint = (0..interface.num_endpoints).find_map(|i| {
        let e = io.endpoint_descriptor(i).ok()?;
        (e.attributes & 3 == 3 && e.endpoint_address & 0x80 != 0).then_some(e)
    })?;
    let index = interface.interface_number as u16;
    // The boot protocol (0), and reports only when something changes.
    let _ = io.control_transfer(CLASS_INTERFACE_OUT, SET_PROTOCOL, 0, index, ControlTransfer::None, 100);
    let _ = io.control_transfer(CLASS_INTERFACE_OUT, SET_IDLE, 0, index, ControlTransfer::None, 100);
    // Copies: the descriptor is a packed struct.
    let (address, packet) = (endpoint.endpoint_address, { endpoint.max_packet_size });
    log::info!("USB mouse: endpoint {address:#x}, {packet} byte reports");
    Some(UsbMouse { io, endpoint: address & 0x7f, packet_size: (packet as usize).clamp(3, 64) })
}

/// The USB mice that no firmware driver handles.
pub fn find() -> Vec<UsbMouse> {
    let Ok(handles) = boot::find_handles::<UsbIo>() else { return Vec::new() };
    handles.iter().filter_map(|h| open(*h)).collect()
}

impl UsbMouse {
    /// A report, if the mouse sent one; waits at most a millisecond.
    pub fn poll(&mut self) -> Option<Report> {
        let mut buffer = [0u8; 64];
        let n = self.io.sync_interrupt_receive(self.endpoint, &mut buffer[..self.packet_size], 1).ok()?;
        parse_report(&buffer[..n])
    }
}
