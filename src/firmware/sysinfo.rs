// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! System information: SMBIOS tables, the UEFI memory map and Secure Boot.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use uefi::boot::MemoryType;
use uefi::mem::memory_map::MemoryMap;
use uefi::runtime::VariableVendor;
use uefi::table::cfg::ConfigTableEntry;
use uefi::cstr16;

use crate::parse::smbios::{self, Smbios};

/// Finds the SMBIOS structure table through the configuration table.
fn smbios_table() -> Option<(&'static [u8], (u8, u8))> {
    let entries: Vec<ConfigTableEntry> = uefi::system::with_config_table(|t| t.to_vec());
    // SAFETY: The firmware guarantees that the entry points and the tables
    // they reference stay mapped while boot services run.
    unsafe {
        if let Some(e) = entries.iter().find(|e| e.guid == ConfigTableEntry::SMBIOS3_GUID) {
            let ep = core::slice::from_raw_parts(e.address as *const u8, 0x18);
            if &ep[0..5] == b"_SM3_" {
                let max = u32::from_le_bytes(ep[0x0c..0x10].try_into().ok()?) as usize;
                let addr = u64::from_le_bytes(ep[0x10..0x18].try_into().ok()?) as usize;
                return Some((core::slice::from_raw_parts(addr as *const u8, max), (ep[7], ep[8])));
            }
        }
        if let Some(e) = entries.iter().find(|e| e.guid == ConfigTableEntry::SMBIOS_GUID) {
            let ep = core::slice::from_raw_parts(e.address as *const u8, 0x1f);
            if &ep[0..4] == b"_SM_" {
                let len = u16::from_le_bytes([ep[0x16], ep[0x17]]) as usize;
                let addr = u32::from_le_bytes(ep[0x18..0x1c].try_into().ok()?) as usize;
                return Some((core::slice::from_raw_parts(addr as *const u8, len), (ep[6], ep[7])));
            }
        }
    }
    None
}

pub fn smbios() -> Smbios {
    let Some((table, version)) = smbios_table() else { return Smbios::default() };
    Smbios { version: Some(version), ..smbios::parse(table) }
}

use crate::app::MemoryUsage;

/// Sums the memory map by what the memory will be once the OS runs. MMIO
/// ranges are not RAM and are left out.
pub fn memory_usage() -> Option<MemoryUsage> {
    let map = uefi::boot::memory_map(MemoryType::LOADER_DATA).ok()?;
    let mut usage = MemoryUsage::default();
    for d in map.entries() {
        let bytes = d.page_count * uefi::boot::PAGE_SIZE as u64;
        match d.ty {
            MemoryType::CONVENTIONAL
            | MemoryType::BOOT_SERVICES_CODE
            | MemoryType::BOOT_SERVICES_DATA
            | MemoryType::LOADER_CODE
            | MemoryType::LOADER_DATA => usage.usable += bytes,
            MemoryType::RUNTIME_SERVICES_CODE | MemoryType::RUNTIME_SERVICES_DATA => {
                usage.runtime += bytes
            }
            MemoryType::ACPI_RECLAIM | MemoryType::ACPI_NON_VOLATILE => usage.acpi += bytes,
            MemoryType::MMIO | MemoryType::MMIO_PORT_SPACE => continue,
            // Reserved ranges that aren't cacheable RAM (e.g. PCI holes) are
            // not memory either.
            _ if !d.att.contains(uefi::boot::MemoryAttribute::WRITE_BACK) => continue,
            _ => usage.reserved += bytes,
        }
    }
    Some(usage)
}

pub enum SecureBoot {
    On,
    Off,
    SetupMode,
    Unsupported,
}

pub fn secure_boot() -> SecureBoot {
    let vendor = VariableVendor::GLOBAL_VARIABLE;
    let mut buf = [0u8; 1];
    let sb = uefi::runtime::get_variable(cstr16!("SecureBoot"), &vendor, &mut buf).map(|(d, _)| d.first().copied().unwrap_or(0));
    let mut buf = [0u8; 1];
    let setup = uefi::runtime::get_variable(cstr16!("SetupMode"), &vendor, &mut buf).map(|(d, _)| d.first().copied().unwrap_or(0));
    match (sb, setup) {
        (Ok(1), _) => SecureBoot::On,
        (_, Ok(1)) => SecureBoot::SetupMode,
        (Ok(_), _) => SecureBoot::Off,
        _ => SecureBoot::Unsupported,
    }
}

/// The firmware's clock.
pub fn date_time() -> Option<crate::app::DateTime> {
    let t = uefi::runtime::get_time().ok()?;
    Some(crate::app::DateTime { year: t.year(), month: t.month(), day: t.day(), hour: t.hour(), minute: t.minute() })
}

/// Sets the firmware's clock, keeping its time zone and daylight saving
/// flags as they are.
pub fn set_date_time(dt: crate::app::DateTime) -> Result<(), String> {
    use uefi::runtime::{Daylight, Time, TimeParams};
    let current = uefi::runtime::get_time().ok();
    let time = Time::new(TimeParams {
        year: dt.year,
        month: dt.month,
        day: dt.day,
        hour: dt.hour,
        minute: dt.minute,
        second: 0,
        nanosecond: 0,
        time_zone: current.and_then(|t| t.time_zone()),
        daylight: current.map(|t| t.daylight()).unwrap_or(Daylight::empty()),
    })
    .map_err(|e| format!("{e:?}"))?;
    // SAFETY: Called while boot services run, from the only thread.
    unsafe { uefi::runtime::set_time(&time) }.map_err(|e| format!("{:?}", e.status()))
}
