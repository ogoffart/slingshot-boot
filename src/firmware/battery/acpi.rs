// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! The batteries' state through ACPI: the firmware's AML code (the DSDT and
//! SSDTs) runs in the `acpi` crate's interpreter, which calls each battery's
//! `_BIX` or `_BIF` and `_BST`, and the adapter's `_PSR`.
//!
//! On laptops, those methods read the embedded controller (EC). The `acpi`
//! crate doesn't handle EC regions, so after loading the tables, each EC
//! region is turned into a memory region at `EC_BASE`, an address no memory
//! has, and the handler talks to the EC when the AML reads or writes there.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::ptr::NonNull;
use core::str::FromStr;
use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use core::time::Duration;

use acpi::aml::namespace::{AmlName, NamespaceLevelKind};
use acpi::aml::object::{DeviceStatus, Object, WrappedObject};
use acpi::aml::op_region::RegionSpace;
use acpi::aml::{AmlError, Interpreter};
use acpi::registers::FixedRegisters;
use acpi::sdt::fadt::Fadt;
use acpi::{AcpiTables, Handle, Handler, PciAddress, PhysicalMapping, RawPhysicalMapping};
use uefi::runtime;
use uefi::table::cfg::ConfigTableEntry;

use super::super::nvram::VENDOR;
use crate::uefi_platform::{timer_freq, timer_tick};
use super::{NO_PROBE, PROBING};
use crate::parse::battery::Power;
use crate::parse::battery::acpi::{self as battery, AmlValue};

/// Where EC regions are moved to: above any physical address.
const EC_BASE: usize = 0xec00_0000_0000_0000;
const EC_SIZE: usize = 0x100;

// x86 port I/O.
fn inb(port: u16) -> u8 {
    let v: u8;
    // SAFETY: Port reads the AML asks for; the firmware owns the hardware.
    unsafe { core::arch::asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack)) };
    v
}
fn inw(port: u16) -> u16 {
    let v: u16;
    // SAFETY: As above.
    unsafe { core::arch::asm!("in ax, dx", out("ax") v, in("dx") port, options(nomem, nostack)) };
    v
}
fn inl(port: u16) -> u32 {
    let v: u32;
    // SAFETY: As above.
    unsafe { core::arch::asm!("in eax, dx", out("eax") v, in("dx") port, options(nomem, nostack)) };
    v
}
fn outb(port: u16, v: u8) {
    // SAFETY: Port writes the AML asks for.
    unsafe { core::arch::asm!("out dx, al", in("dx") port, in("al") v, options(nomem, nostack)) };
}
fn outw(port: u16, v: u16) {
    // SAFETY: As above.
    unsafe { core::arch::asm!("out dx, ax", in("dx") port, in("ax") v, options(nomem, nostack)) };
}
fn outl(port: u16, v: u32) {
    // SAFETY: As above.
    unsafe { core::arch::asm!("out dx, eax", in("dx") port, in("eax") v, options(nomem, nostack)) };
}

/// The embedded controller's ports (ACPI spec, section 12).
#[derive(Clone, Copy)]
struct Ec {
    data: u16,
    command: u16,
}

/// The EC's ports, and whether it stopped answering: then every access fails
/// at once instead of waiting each time.
static EC_DATA: AtomicU16 = AtomicU16::new(0x62);
static EC_COMMAND: AtomicU16 = AtomicU16::new(0x66);
/// Whether the first reading was logged.
static LOGGED: AtomicBool = AtomicBool::new(false);
static EC_DEAD: AtomicBool = AtomicBool::new(false);

impl Ec {
    fn get() -> Option<Ec> {
        if EC_DEAD.load(Ordering::Relaxed) {
            return None;
        }
        Some(Ec { data: EC_DATA.load(Ordering::Relaxed), command: EC_COMMAND.load(Ordering::Relaxed) })
    }

    /// Waits until the status has `bit` set (or clear), for up to 50 ms.
    fn wait(&self, bit: u8, set: bool) -> Option<()> {
        for _ in 0..5000 {
            if (inb(self.command) & bit != 0) == set {
                return Some(());
            }
            uefi::boot::stall(Duration::from_micros(10));
        }
        log::warn!("battery: the embedded controller doesn't answer");
        EC_DEAD.store(true, Ordering::Relaxed);
        None
    }

    const OBF: u8 = 1;
    const IBF: u8 = 2;

    fn read(&self, address: u8) -> Option<u8> {
        if inb(self.command) & Self::OBF != 0 {
            // A stale byte.
            inb(self.data);
        }
        self.wait(Self::IBF, false)?;
        outb(self.command, 0x80);
        self.wait(Self::IBF, false)?;
        outb(self.data, address);
        self.wait(Self::OBF, true)?;
        Some(inb(self.data))
    }

    fn write(&self, address: u8, value: u8) -> Option<()> {
        self.wait(Self::IBF, false)?;
        outb(self.command, 0x81);
        self.wait(Self::IBF, false)?;
        outb(self.data, address);
        self.wait(Self::IBF, false)?;
        outb(self.data, value);
        self.wait(Self::IBF, false)
    }
}

fn ec_read(address: usize, bytes: usize) -> u64 {
    let mut v = 0u64;
    for i in 0..bytes {
        let b = Ec::get().and_then(|ec| ec.read((address + i - EC_BASE) as u8)).unwrap_or(0xff);
        v |= (b as u64) << (8 * i);
    }
    v
}

fn ec_write(address: usize, bytes: usize, value: u64) {
    for i in 0..bytes {
        if let Some(ec) = Ec::get() {
            ec.write((address + i - EC_BASE) as u8, (value >> (8 * i)) as u8);
        }
    }
}

fn is_ec(address: usize) -> bool {
    (EC_BASE..EC_BASE + EC_SIZE).contains(&address)
}

/// Runs `f` with `PROBING` set, for the panic handler.
fn probing<T>(f: impl FnOnce() -> T) -> T {
    PROBING.store(true, Ordering::Relaxed);
    let result = f();
    PROBING.store(false, Ordering::Relaxed);
    result
}

/// What the interpreter needs from the machine. UEFI maps physical memory
/// 1:1, and runs one thread: mutexes are always free.
#[derive(Clone, Copy)]
struct Host;

impl Handler for Host {
    unsafe fn map_physical_region<T>(&self, physical_address: usize, size: usize) -> RawPhysicalMapping<T> {
        let ptr = NonNull::new(physical_address as *mut T).unwrap_or(NonNull::dangling());
        // SAFETY: Physical memory is identity-mapped.
        unsafe { RawPhysicalMapping::new(physical_address, ptr, size, size) }
    }
    unsafe fn unmap_physical_region<T>(&self, _region: RawPhysicalMapping<T>) {}

    fn read_u8(&self, address: usize) -> u8 {
        if is_ec(address) {
            return ec_read(address, 1) as u8;
        }
        // SAFETY: An address of an AML memory region.
        unsafe { (address as *const u8).read_volatile() }
    }
    fn read_u16(&self, address: usize) -> u16 {
        if is_ec(address) {
            return ec_read(address, 2) as u16;
        }
        // SAFETY: As above.
        unsafe { (address as *const u16).read_unaligned() }
    }
    fn read_u32(&self, address: usize) -> u32 {
        if is_ec(address) {
            return ec_read(address, 4) as u32;
        }
        // SAFETY: As above.
        unsafe { (address as *const u32).read_unaligned() }
    }
    fn read_u64(&self, address: usize) -> u64 {
        if is_ec(address) {
            return ec_read(address, 8);
        }
        // SAFETY: As above.
        unsafe { (address as *const u64).read_unaligned() }
    }
    fn write_u8(&self, address: usize, value: u8) {
        if is_ec(address) {
            return ec_write(address, 1, value as u64);
        }
        // SAFETY: As above.
        unsafe { (address as *mut u8).write_volatile(value) }
    }
    fn write_u16(&self, address: usize, value: u16) {
        if is_ec(address) {
            return ec_write(address, 2, value as u64);
        }
        // SAFETY: As above.
        unsafe { (address as *mut u16).write_unaligned(value) }
    }
    fn write_u32(&self, address: usize, value: u32) {
        if is_ec(address) {
            return ec_write(address, 4, value as u64);
        }
        // SAFETY: As above.
        unsafe { (address as *mut u32).write_unaligned(value) }
    }
    fn write_u64(&self, address: usize, value: u64) {
        if is_ec(address) {
            return ec_write(address, 8, value);
        }
        // SAFETY: As above.
        unsafe { (address as *mut u64).write_unaligned(value) }
    }

    fn read_io_u8(&self, port: u16) -> u8 {
        inb(port)
    }
    fn read_io_u16(&self, port: u16) -> u16 {
        inw(port)
    }
    fn read_io_u32(&self, port: u16) -> u32 {
        inl(port)
    }
    fn write_io_u8(&self, port: u16, value: u8) {
        outb(port, value)
    }
    fn write_io_u16(&self, port: u16, value: u16) {
        outw(port, value)
    }
    fn write_io_u32(&self, port: u16, value: u32) {
        outl(port, value)
    }

    // PCI configuration space through the legacy ports (segment 0).
    fn read_pci_u8(&self, address: PciAddress, offset: u16) -> u8 {
        (self.read_pci_u32(address, offset & !3) >> ((offset & 3) * 8)) as u8
    }
    fn read_pci_u16(&self, address: PciAddress, offset: u16) -> u16 {
        (self.read_pci_u32(address, offset & !3) >> ((offset & 2) * 8)) as u16
    }
    fn read_pci_u32(&self, address: PciAddress, offset: u16) -> u32 {
        if pci_select(address, offset) { inl(0xcfc) } else { 0xffff_ffff }
    }
    fn write_pci_u8(&self, address: PciAddress, offset: u16, value: u8) {
        if pci_select(address, offset) {
            outb(0xcfc + (offset & 3), value);
        }
    }
    fn write_pci_u16(&self, address: PciAddress, offset: u16, value: u16) {
        if pci_select(address, offset) {
            outw(0xcfc + (offset & 2), value);
        }
    }
    fn write_pci_u32(&self, address: PciAddress, offset: u16, value: u32) {
        if pci_select(address, offset) {
            outl(0xcfc, value);
        }
    }

    fn nanos_since_boot(&self) -> u64 {
        (timer_tick() as u128 * 1_000_000_000 / timer_freq() as u128) as u64
    }
    fn stall(&self, microseconds: u64) {
        uefi::boot::stall(Duration::from_micros(microseconds));
    }
    fn sleep(&self, milliseconds: u64) {
        uefi::boot::stall(Duration::from_millis(milliseconds));
    }

    fn create_mutex(&self) -> Handle {
        Handle(0)
    }
    fn acquire(&self, _mutex: Handle, _timeout: u16) -> Result<(), AmlError> {
        Ok(())
    }
    fn release(&self, _mutex: Handle) {}

    fn handle_fatal_error(&self, fatal_type: u8, fatal_code: u32, fatal_arg: u64) {
        log::warn!("battery: AML Fatal({fatal_type}, {fatal_code:#x}, {fatal_arg:#x})");
    }
}

/// Selects `offset` in `address`'s configuration space, through the legacy
/// ports: segment 0, the first 256 bytes. False when it can't.
fn pci_select(address: PciAddress, offset: u16) -> bool {
    if address.segment() != 0 || offset >= 256 {
        return false;
    }
    let config_address = 0x8000_0000
        | (address.bus() as u32) << 16
        | (address.device() as u32) << 11
        | (address.function() as u32) << 8
        | (offset as u32 & 0xfc);
    outl(0xcf8, config_address);
    true
}

/// The interpreter with the machine's tables loaded, and where the devices
/// are in its namespace.
pub struct Acpi {
    interpreter: Interpreter<Host>,
    batteries: Vec<AmlName>,
    adapters: Vec<AmlName>,
    /// Every device, with its ids (`_HID`, `_CID`).
    devices: Vec<(AmlName, Vec<String>)>,
    /// The methods that failed, logged once (they're read every second).
    failed: core::cell::RefCell<Vec<String>>,
}

fn rsdp() -> Option<usize> {
    uefi::system::with_config_table(|t| {
        let find = |guid| t.iter().find(|e| e.guid == guid).map(|e| e.address as usize);
        find(ConfigTableEntry::ACPI2_GUID).or_else(|| find(ConfigTableEntry::ACPI_GUID))
    })
}

/// `device.method`, e.g. `\_SB.BAT0._BST`.
fn child(device: &AmlName, method: &str) -> Option<AmlName> {
    AmlName::from_str(method).ok()?.resolve(device).ok()
}

/// A device's ids: `_HID`, and `_CID` (one id or a package of them).
fn ids(interpreter: &Interpreter<Host>, device: &AmlName) -> Vec<String> {
    let mut ids = Vec::new();
    for method in ["_HID", "_CID"] {
        let Some(path) = child(device, method) else { continue };
        let Ok(Some(value)) = interpreter.evaluate_if_present(path, vec![]) else { continue };
        let value = value.unwrap_transparent_reference();
        let mut push = |o: &Object| match o {
            Object::Integer(v) => ids.push(battery::eisa_id(*v)),
            Object::String(s) => ids.push(s.clone()),
            _ => {}
        };
        match &*value {
            Object::Package(elements) => elements.iter().for_each(|e| push(&e.clone().unwrap_transparent_reference())),
            other => push(other),
        }
    }
    ids
}

impl Acpi {
    pub fn load() -> Option<Acpi> {
        let mut buf = [0u8; 32];
        if let Ok((version, _)) = runtime::get_variable(NO_PROBE, &VENDOR, &mut buf)
            && version == env!("CARGO_PKG_VERSION").as_bytes()
        {
            log::info!("battery: not reading ACPI: it crashed this version before");
            return None;
        }
        let Some(rsdp) = rsdp() else {
            log::warn!("battery: no ACPI tables (no RSDP in the configuration table)");
            return None;
        };
        probing(|| {
            // SAFETY: The firmware's RSDP, in identity-mapped memory.
            let tables = unsafe { AcpiTables::from_rsdp(Host, rsdp) }
                .map_err(|e| log::warn!("battery: reading the ACPI tables: {e:?}"))
                .ok()?;
            let Some(fadt) = tables.find_table::<Fadt>() else {
                log::warn!("battery: no FADT");
                return None;
            };
            let registers = Arc::new(
                FixedRegisters::new(&fadt, Host).map_err(|e| log::warn!("battery: the FADT's registers: {e:?}")).ok()?,
            );
            let facs = fadt.facs_address().ok().map(|a| {
                // SAFETY: The FACS the FADT points to.
                unsafe { PhysicalMapping::new(a, core::mem::size_of::<acpi::sdt::facs::Facs>(), Host) }
            });
            let dsdt = tables.dsdt().map_err(|e| log::warn!("battery: no DSDT: {e:?}")).ok()?;
            let interpreter = Interpreter::new(Host, dsdt.revision, registers, facs);
            for (i, table) in core::iter::once(dsdt).chain(tables.ssdts()).enumerate() {
                let header = core::mem::size_of::<acpi::sdt::SdtHeader>();
                // SAFETY: The table's AML, after its header, in memory the
                // firmware keeps.
                let aml = unsafe {
                    core::slice::from_raw_parts(
                        (table.phys_address + header) as *const u8,
                        (table.length as usize).saturating_sub(header),
                    )
                };
                // The DSDT, then the SSDTs.
                match interpreter.load_table(aml) {
                    Ok(()) => log::info!("battery: loaded ACPI table {i} ({} bytes of AML)", aml.len()),
                    Err(e) => log::warn!("battery: loading ACPI table {i}: {e:?}"),
                }
            }
            let mut acpi = Acpi { interpreter, batteries: Vec::new(), adapters: Vec::new(), devices: Vec::new(), failed: Default::default() };
            acpi.find_devices();
            log::info!("battery: {} batteries, {} AC adapters", acpi.batteries.len(), acpi.adapters.len());
            Some(acpi)
        })
    }

    /// Moves the EC regions, finds the EC, the batteries and the adapters,
    /// and tells the AML that the EC can be used.
    fn find_devices(&mut self) {
        let token = self.interpreter.object_token.lock();
        let mut namespace = self.interpreter.namespace.lock().clone();
        let mut devices = Vec::new();
        let _ = namespace.traverse(|path, level| {
            for (_, object) in level.values.values() {
                if let Object::OpRegion(region) = &**object
                    && region.space == RegionSpace::EmbeddedControl
                {
                    // SAFETY: Nothing else holds a reference into the
                    // object: the interpreter isn't running.
                    if let Object::OpRegion(region) = unsafe { object.gain_mut(&token) } {
                        region.space = RegionSpace::SystemMemory;
                        region.base = (EC_BASE as u64).wrapping_add(region.base.min(EC_SIZE as u64 - 1));
                    }
                }
            }
            if level.kind == NamespaceLevelKind::Device {
                devices.push(path.clone());
            }
            Ok(true)
        });
        drop(token);
        drop(namespace);

        log::info!("battery: {} devices in the namespace", devices.len());
        let mut ec = None;
        for device in devices {
            let ids = ids(&self.interpreter, &device);
            self.devices.push((device.clone(), ids.clone()));
            let has = |id: &str| ids.iter().any(|i| i == id);
            if has("PNP0C0A") {
                log::info!("battery: battery {device} {ids:?}");
                self.batteries.push(device);
            } else if has("ACPI0003") {
                log::info!("battery: AC adapter {device} {ids:?}");
                self.adapters.push(device);
            } else if has("PNP0C09") && ec.is_none() {
                log::info!("battery: embedded controller {device} {ids:?}");
                ec = Some(device);
            } else if ids.iter().any(|i| i == "ACPI0002" || i == "PNP0C0B") {
                // A Smart Battery System or a fan: not read, but worth knowing.
                log::info!("battery: also {device} {ids:?}");
            }
        }
        let Some(ec) = ec else {
            log::info!("battery: no embedded controller");
            return;
        };
        // The EC's ports: its data port, then its command and status port.
        if let Some(crs) = child(&ec, "_CRS")
            && let Ok(Some(resources)) = self.interpreter.evaluate_if_present(crs, vec![])
            && let Ok(list) = acpi::aml::resource::resource_descriptor_list(resources)
        {
            let ports: Vec<u16> = list
                .iter()
                .filter_map(|r| match r {
                    acpi::aml::resource::Resource::IOPort(p) => Some(p.memory_range.0),
                    _ => None,
                })
                .collect();
            if let [data, command, ..] = ports[..] {
                EC_DATA.store(data, Ordering::Relaxed);
                EC_COMMAND.store(command, Ordering::Relaxed);
            }
        }
        log::info!(
            "battery: EC ports data {:#x}, command {:#x}",
            EC_DATA.load(Ordering::Relaxed),
            EC_COMMAND.load(Ordering::Relaxed)
        );
        // `_REG(EmbeddedControl, connected)`: many DSDTs only read the EC
        // once they've been told.
        if let Some(reg) = child(&ec, "_REG") {
            let args = vec![Object::Integer(3).wrap(), Object::Integer(1).wrap()];
            match self.interpreter.evaluate_if_present(reg, args) {
                Ok(r) => log::info!("battery: EC _REG: {}", if r.is_some() { "done" } else { "none" }),
                Err(e) => log::warn!("battery: EC _REG: {e:?}"),
            }
        }
    }

    fn evaluate(&self, device: &AmlName, method: &str) -> Option<WrappedObject> {
        let path = child(device, method)?;
        match self.interpreter.evaluate_if_present(path, vec![]) {
            // A value, even when the method returns a reference to it (a
            // Dell's _STA returns an Index reference).
            Ok(v) => Some(v?.unwrap_reference()),
            Err(e) => {
                let what = alloc::format!("{device}.{method}");
                let mut failed = self.failed.borrow_mut();
                if !failed.contains(&what) {
                    log::warn!("battery: {what}: {e:?}");
                    failed.push(what);
                }
                None
            }
        }
    }

    /// The devices with this id (`_HID` or `_CID`).
    #[cfg_attr(not(feature = "touchpad"), allow(dead_code))] // the touchpad's
    pub fn devices_with(&self, id: &str) -> Vec<AmlName> {
        self.devices.iter().filter(|(_, ids)| ids.iter().any(|i| i == id)).map(|(d, _)| d.clone()).collect()
    }

    /// Calls `method` of `device` with `args`, inside the panic guard.
    #[cfg_attr(not(feature = "touchpad"), allow(dead_code))] // the touchpad's
    pub fn call(&self, device: &AmlName, method: &str, args: Vec<WrappedObject>) -> Option<WrappedObject> {
        let path = child(device, method)?;
        probing(|| match self.interpreter.evaluate_if_present(path, args) {
            Ok(v) => Some(v?.unwrap_transparent_reference()),
            Err(e) => {
                log::warn!("acpi: {device}.{method}: {e:?}");
                None
            }
        })
    }

    /// A device's resources (`_CRS`).
    #[cfg_attr(not(feature = "touchpad"), allow(dead_code))] // the touchpad's
    pub fn resources(&self, device: &AmlName) -> Vec<acpi::aml::resource::Resource> {
        self.call(device, "_CRS", vec![])
            .and_then(|r| probing(|| acpi::aml::resource::resource_descriptor_list(r).ok()))
            .unwrap_or_default()
    }

    pub fn integer(&self, device: &AmlName, method: &str) -> Option<u64> {
        self.evaluate(device, method)?.as_integer().ok()
    }

    /// A package's elements as plain values.
    pub fn package(&self, device: &AmlName, method: &str) -> Option<Vec<AmlValue>> {
        let package = self.evaluate(device, method)?;
        let Object::Package(elements) = &*package else { return None };
        Some(elements.iter().map(|e| self.value(e.clone())).collect())
    }

    fn value(&self, object: WrappedObject) -> AmlValue {
        let object = object.unwrap_transparent_reference();
        match &*object {
            Object::Integer(v) => AmlValue::Integer(*v),
            Object::String(s) => AmlValue::String(s.clone()),
            Object::Buffer(b) => {
                let end = b.iter().position(|c| *c == 0).unwrap_or(b.len());
                AmlValue::String(String::from_utf8_lossy(&b[..end]).into())
            }
            // A name in a package literal: what it names.
            Object::NamePath { name, scope } => match name.resolve(scope) {
                Ok(path) => match self.interpreter.evaluate(path, vec![]) {
                    Ok(v) if !matches!(&*v, Object::NamePath { .. }) => self.value(v),
                    _ => AmlValue::Other,
                },
                Err(_) => AmlValue::Other,
            },
            _ => AmlValue::Other,
        }
    }

    pub fn power(&self) -> Power {
        // Give the EC another chance if it was busy last time.
        EC_DEAD.store(false, Ordering::Relaxed);
        // The first reading is logged in full, for bug reports.
        let first = !LOGGED.swap(true, Ordering::Relaxed);
        probing(|| {
            let mut power = Power::default();
            for adapter in &self.adapters {
                let psr = self.integer(adapter, "_PSR");
                if first {
                    log::info!("battery: {adapter}._PSR = {psr:?}");
                }
                if let Some(online) = psr {
                    // 1, or Ones from a logical expression.
                    power.ac_online = Some(power.ac_online.unwrap_or(false) || online != 0);
                }
            }
            for device in &self.batteries {
                let sta = self.integer(device, "_STA");
                if first {
                    log::info!("battery: {device}._STA = {sta:x?}");
                }
                if sta.is_some_and(|s| !DeviceStatus(s).battery_present()) {
                    continue;
                }
                let bix = self.package(device, "_BIX");
                let bif = if bix.as_deref().and_then(battery::from_bix).is_none() { self.package(device, "_BIF") } else { None };
                if first {
                    log::info!("battery: {device}._BIX = {bix:?}");
                    if bif.is_some() {
                        log::info!("battery: {device}._BIF = {bif:?}");
                    }
                }
                let info = bix.as_deref().and_then(battery::from_bix);
                let Some(mut b) = info.or_else(|| bif.as_deref().and_then(battery::from_bif)) else {
                    continue;
                };
                let bst = self.package(device, "_BST");
                if first {
                    log::info!("battery: {device}._BST = {bst:?}");
                }
                if let Some(bst) = bst {
                    battery::apply_bst(&mut b, &bst);
                }
                power.batteries.push(b);
            }
            power
        })
    }
}

