// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Laptop touchpads on I2C ("HID over I2C"), such as the Framework's, which
//! firmware rarely has a driver for. The `touchpad` feature, on by default.
//!
//! - ACPI describes the touchpad (`PNP0C50`): its I2C address and
//!   controller (`_CRS`), and where its HID descriptor is (`_DSM`).
//! - The controller is a DesignWare I2C controller (`i2c.rs`): on Intel, an
//!   LPSS PCI device (its registers at BAR 0, taken out of reset); on AMD,
//!   at the memory range of its `_CRS`.
//! - HID over I2C: power on, reset, read the report descriptor, then poll
//!   the input reports, read with `parse::hid`. Touchpads should start in
//!   mouse mode (relative movement and buttons), but some report fingers
//!   anyway (the Framework 13's): the first finger's moves move the
//!   pointer, its press clicks (with two fingers, a right click).
//!
//! Everything is logged: F12 on the System screen saves the log.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::str::FromStr;
use core::time::Duration;

use acpi::aml::namespace::AmlName;
use acpi::aml::object::Object;
use acpi::aml::resource::{MemoryRangeDescriptor, Resource, SerialBus};

use super::battery::acpi::Acpi;
use super::i2c::{self, I2c, Timing};
use crate::parse::battery::acpi::AmlValue;
use crate::parse::hid::{self, MouseLayout, MouseReport, TouchLayout};

/// HID over I2C's `_DSM` (its function 1 gives the HID descriptor's
/// register), in ACPI's byte order.
const HID_I2C_DSM: [u8; 16] = [
    0xf7, 0xf6, 0xdf, 0x3c, 0x67, 0x42, 0x55, 0x45, 0xad, 0x05, 0xb3, 0x0a, 0x3d, 0x89, 0x38, 0xde,
];

pub struct Touchpad {
    i2c: I2c,
    address: u16,
    max_input: usize,
    layout: Option<MouseLayout>,
    touch: Option<TouchLayout>,
    /// The finger down at the last touch report: its contact, where.
    finger: Option<(i32, i32, i32)>,
    /// What's left of the movement, under a pixel.
    carry: (f32, f32),
    /// Reads so far, to log the first ones.
    reads: u32,
    /// Errors in a row: the touchpad is given up after too many.
    errors: u32,
    /// The pad pressed, with how many fingers and the pointer's movement
    /// since: logged on release.
    press: Option<(i32, i32, i32)>,
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}

// PCI configuration space, segment 0, through the legacy ports.
fn pci_address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000 | (bus as u32) << 16 | (device as u32) << 11 | (function as u32) << 8 | (offset as u32 & 0xfc)
}
fn pci_read(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let v: u32;
    // SAFETY: The PCI configuration ports.
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") 0xcf8u16, in("eax") pci_address(bus, device, function, offset), options(nomem, nostack));
        core::arch::asm!("in eax, dx", out("eax") v, in("dx") 0xcfcu16, options(nomem, nostack));
    }
    v
}
fn pci_write(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    // SAFETY: As above.
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") 0xcf8u16, in("eax") pci_address(bus, device, function, offset), options(nomem, nostack));
        core::arch::asm!("out dx, eax", in("dx") 0xcfcu16, in("eax") value, options(nomem, nostack));
    }
}

/// The controller's registers: a memory range in its `_CRS` (AMD), else
/// its PCI device's BAR 0 (Intel's LPSS), powered on and out of reset.
fn controller_base(acpi: &Acpi, controller: &AmlName) -> Result<usize, String> {
    for r in acpi.resources(controller) {
        if let Resource::MemoryRange(MemoryRangeDescriptor::FixedLocation { base_address, .. }) = r {
            log::info!("touchpad: controller registers at {base_address:#x} (_CRS)");
            return Ok(base_address as usize);
        }
    }
    let adr = acpi.integer(controller, "_ADR").ok_or("no memory range and no PCI address")?;
    let (device, function) = ((adr >> 16) as u8, (adr & 0xffff) as u8);
    let id = pci_read(0, device, function, 0);
    log::info!("touchpad: controller is PCI 00:{device:02x}.{function} ({:04x}:{:04x})", id & 0xffff, id >> 16);
    if id == 0xffff_ffff {
        return Err(String::from("its PCI device isn't there (hidden by the firmware?)"));
    }
    // Power state D0, through the power management capability.
    let mut cap = (pci_read(0, device, function, 0x34) & 0xfc) as u8;
    while cap != 0 {
        let header = pci_read(0, device, function, cap);
        if header & 0xff == 1 {
            let pmcsr = pci_read(0, device, function, cap + 4);
            if pmcsr & 3 != 0 {
                log::info!("touchpad: controller in D{}, to D0", pmcsr & 3);
                pci_write(0, device, function, cap + 4, pmcsr & !3);
                uefi::boot::stall(Duration::from_millis(10));
            }
            break;
        }
        cap = ((header >> 8) & 0xfc) as u8;
    }
    let bar0 = pci_read(0, device, function, 0x10);
    let mut base = (bar0 & !0xf) as u64;
    if (bar0 >> 1) & 3 == 2 {
        base |= (pci_read(0, device, function, 0x14) as u64) << 32;
    }
    if base == 0 || bar0 & 1 != 0 {
        return Err(format!("its BAR 0 isn't usable ({bar0:#x})"));
    }
    // Memory decoding on.
    let command = pci_read(0, device, function, 4);
    if command & 2 == 0 {
        pci_write(0, device, function, 4, command | 2);
    }
    let base = base as usize;
    if i2c::read_register(base, 0xfc) != i2c::DESIGNWARE {
        // Intel LPSS: out of reset, and the remap address its private
        // registers want (as Linux's intel-lpss does).
        log::info!("touchpad: taking the LPSS controller out of reset");
        // SAFETY: The LPSS private registers, after the controller's.
        unsafe {
            let private = (base + 0x200) as *mut u32;
            private.add(1).write_volatile(0);
            private.add(1).write_volatile(0x7);
            private.add(0x10).write_volatile(base as u32);
            private.add(0x11).write_volatile((base as u64 >> 32) as u32);
        }
        uefi::boot::stall(Duration::from_millis(1));
    }
    log::info!("touchpad: controller registers at {base:#x} (BAR 0)");
    Ok(base)
}

/// The bus timing the controller's ACPI methods give, else a safe one.
fn timing(acpi: &Acpi, controller: &AmlName, speed: u32) -> Timing {
    let counts = |method: &str| {
        let p = acpi.package(controller, method)?;
        let n = |i: usize| match p.get(i) {
            Some(AmlValue::Integer(v)) => Some(*v as u16),
            _ => None,
        };
        Some((n(0)?, n(1)?, n(2)))
    };
    if speed >= 400_000
        && let Some((high, low, hold)) = counts("FMCN")
    {
        return Timing { fast: true, high, low, hold };
    }
    if let Some((high, low, hold)) = counts("SSCN") {
        return Timing { fast: false, high, low, hold };
    }
    Timing::SAFE
}

fn open(acpi: &Acpi, device: &AmlName) -> Result<Touchpad, String> {
    let sta = acpi.integer(device, "_STA");
    log::info!("touchpad: {device} _STA={sta:x?}");
    if sta.is_some_and(|s| s & 1 == 0) {
        return Err(String::from("not present"));
    }
    let (address, speed, source) = acpi
        .resources(device)
        .into_iter()
        .find_map(|r| match r {
            Resource::SerialBus(s) => match s.bus {
                SerialBus::I2C { address, speed, .. } => Some((address, speed, s.source)),
            },
            _ => None,
        })
        .ok_or("no I2C connection in its _CRS")?;
    log::info!("touchpad: I2C address {address:#x}, {speed} Hz, on {source}");
    let dsm = vec![
        Object::Buffer(HID_I2C_DSM.to_vec()).wrap(),
        Object::Integer(1).wrap(),
        Object::Integer(1).wrap(),
        Object::Package(Vec::new()).wrap(),
    ];
    let register = acpi.call(device, "_DSM", dsm).and_then(|r| r.as_integer().ok()).ok_or("no HID descriptor register (_DSM)")? as u16;
    log::info!("touchpad: HID descriptor at register {register:#x}");

    let controller = AmlName::from_str(source.trim()).map_err(|e| format!("controller {source}: {e:?}"))?;
    // Power on, as Linux does when it binds a driver.
    acpi.call(&controller, "_PS0", Vec::new());
    acpi.call(device, "_PS0", Vec::new());
    let base = controller_base(acpi, &controller)?;
    let mut i2c = I2c::new(base, timing(acpi, &controller, speed))?;

    let descriptor = i2c.transfer(address, &register.to_le_bytes(), 30).map_err(|e| format!("HID descriptor: {e}"))?;
    log::info!("touchpad: HID descriptor {}", hex(&descriptor));
    let desc = hid::parse_i2c_hid_descriptor(&descriptor).ok_or("not a HID over I2C descriptor")?;
    log::info!("touchpad: {desc:x?}");
    let command = desc.command_register.to_le_bytes();
    // SET_POWER (on), then RESET: the device then has an empty report (a
    // length of 0) to read.
    i2c.transfer(address, &[command[0], command[1], 0x00, 0x08], 0).map_err(|e| format!("power on: {e}"))?;
    uefi::boot::stall(Duration::from_millis(1));
    i2c.transfer(address, &[command[0], command[1], 0x00, 0x01], 0).map_err(|e| format!("reset: {e}"))?;
    uefi::boot::stall(Duration::from_millis(100));
    let max_input = (desc.max_input_length as usize).clamp(4, 512);
    match i2c.transfer(address, &[], max_input) {
        Ok(r) => log::info!("touchpad: after reset: {}", hex(&r[..r.len().min(8)])),
        Err(e) => log::info!("touchpad: after reset: {e}"),
    }
    let report_descriptor = i2c
        .transfer(address, &desc.report_descriptor_register.to_le_bytes(), desc.report_descriptor_length as usize)
        .map_err(|e| format!("report descriptor: {e}"))?;
    log::info!("touchpad: report descriptor ({} bytes) {}", report_descriptor.len(), hex(&report_descriptor));
    let layout = hid::mouse_layout(&report_descriptor);
    let touch = hid::touchpad_layout(&report_descriptor);
    log::info!("touchpad: mouse reports: {layout:?}, finger reports: {touch:?}");
    if layout.is_none() && touch.is_none() {
        return Err(String::from("neither a mouse nor a touch pad in the report descriptor"));
    }
    Ok(Touchpad { i2c, address, max_input, layout, touch, finger: None, carry: (0., 0.), reads: 0, errors: 0, press: None })
}

/// The touchpad, if there's one Slingshot can drive. Loads the machine's
/// ACPI tables (again, they're the battery's too).
pub fn find() -> Option<Touchpad> {
    let acpi = Acpi::load()?;
    let devices = acpi.devices_with("PNP0C50");
    log::info!("touchpad: {} HID over I2C devices", devices.len());
    for device in devices {
        match open(&acpi, &device) {
            Ok(t) => {
                log::info!("touchpad: {device} ready");
                return Some(t);
            }
            Err(e) => log::warn!("touchpad: {device}: {e}"),
        }
    }
    None
}

impl Touchpad {
    /// A report, if the touchpad has one (`None` when it's given up).
    pub fn poll(&mut self) -> Option<MouseReport> {
        if self.errors > 50 {
            return None;
        }
        let data = match self.i2c.transfer(self.address, &[], self.max_input) {
            Ok(d) => {
                self.errors = 0;
                d
            }
            Err(e) => {
                self.errors += 1;
                if self.errors == 1 || self.errors > 50 {
                    log::warn!("touchpad: reading: {e}{}", if self.errors > 50 { " (giving up)" } else { "" });
                }
                return None;
            }
        };
        let length = (u16::from_le_bytes([data[0], data[1]]) as usize).min(data.len());
        if length <= 2 {
            return None;
        }
        self.reads += 1;
        if self.reads <= 20 {
            log::info!("touchpad: report {}", hex(&data[2..length]));
        }
        let report = &data[2..length];
        if let Some(t) = self.touch.as_ref().and_then(|l| hid::parse_touch_report(l, report)) {
            return Some(self.touch_to_mouse(t));
        }
        hid::parse_mouse_report(self.layout.as_ref()?, report)
    }

    /// Pixels per touch pad unit, at scale 1: the Framework's pad is 1337
    /// units for 11 cm (2.5 on its screen, scaled ×1.5). The platform
    /// accelerates fast moves.
    const SPEED: f32 = 1.7;

    /// A finger report as a mouse's: the movement of the finger since the
    /// last report (none when it just came down, or with more fingers), and
    /// the button, a right click with two fingers.
    fn touch_to_mouse(&mut self, t: hid::Touch) -> MouseReport {
        let (mut dx, mut dy) = (0, 0);
        if t.touching {
            if let Some((contact, x, y)) = self.finger
                && contact == t.contact
                && t.fingers <= 1
            {
                let (mx, my) = ((t.x - x) as f32, (t.y - y) as f32);
                let fx = mx * Self::SPEED + self.carry.0;
                let fy = my * Self::SPEED + self.carry.1;
                (dx, dy) = (fx as i32, fy as i32);
                self.carry = (fx - dx as f32, fy - dy as f32);
            } else {
                self.carry = (0., 0.);
            }
            self.finger = Some((t.contact, t.x, t.y));
        } else {
            self.finger = None;
        }
        match (&mut self.press, t.button) {
            (None, true) => {
                log::info!("touchpad: pressed, {} finger(s)", t.fingers);
                self.press = Some((t.fingers, dx, dy));
            }
            (Some((_, x, y)), true) => (*x, *y) = (*x + dx, *y + dy),
            (Some((fingers, x, y)), false) => {
                log::info!("touchpad: released ({fingers} finger(s) down), pointer moved by {x},{y} while pressed");
                self.press = None;
            }
            (None, false) => {}
        }
        MouseReport { dx, dy, left: t.button && t.fingers < 2, right: t.button && t.fingers >= 2 }
    }
}
