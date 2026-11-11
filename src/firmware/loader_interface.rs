// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Tells the operating system about Slingshot and the boot, as systemd-boot
//! does: the variables of systemd's Boot Loader Interface, read by `bootctl
//! status`, `systemd-analyze` (the time spent in the firmware and the boot
//! loader) and systemd-gpt-auto-generator (which partition is the ESP).

use alloc::format;

use uefi::runtime::{self, VariableAttributes, VariableVendor};
use uefi::{CStr16, cstr16, guid};

use crate::parse::loader_interface::{self as li, FEATURES};

/// systemd's boot loader interface vendor GUID.
pub(super) const LOADER: VariableVendor = VariableVendor(guid!("4a67b082-0a4c-41cf-b6c7-440b29bb8c4f"));

/// Volatile: for this boot only.
const VOLATILE: VariableAttributes = VariableAttributes::BOOTSERVICE_ACCESS.union(VariableAttributes::RUNTIME_ACCESS);

fn set(name: &CStr16, data: &[u8]) {
    if let Err(e) = runtime::set_variable(name, &LOADER, VOLATILE, data) {
        log::warn!("cannot set {name}: {:?}", e.status());
    }
}

fn set_string(name: &CStr16, value: &str) {
    set(name, &crate::parse::ucs2_bytes(value));
}

/// `ticks` of the platform's timer (the time stamp counter on x86) in
/// microseconds since the processor started, as systemd-boot measures
/// them: `LoaderTimeInitUSec` and `LoaderTimeExecUSec`.
fn usec(ticks: u64) -> u64 {
    (u128::from(ticks) * 1_000_000 / u128::from(super::platform::timer_freq())) as u64
}

/// Once the firmware is ready (the image's device path can be read):
/// what Slingshot is, where it was started from, what it supports.
/// `started` is the timer when Slingshot started.
pub fn announce(started: u64, own_fs_device_path: &[u8], own_file: &str) {
    set_string(cstr16!("LoaderTimeInitUSec"), &format!("{}", usec(started)));
    set_string(cstr16!("LoaderInfo"), concat!("Slingshot ", env!("CARGO_PKG_VERSION")));
    let vendor = alloc::string::String::from_utf16_lossy(uefi::system::firmware_vendor().to_u16_slice());
    set_string(cstr16!("LoaderFirmwareInfo"), &li::firmware_info(&vendor, uefi::system::firmware_revision()));
    let rev = uefi::system::uefi_revision();
    set_string(cstr16!("LoaderFirmwareType"), &format!("UEFI {}.{:02}", rev.major(), rev.minor()));
    if let Some(uuid) = li::partition_uuid(own_fs_device_path) {
        set_string(cstr16!("LoaderDevicePartUUID"), &uuid);
    }
    if !own_file.is_empty() {
        set_string(cstr16!("LoaderImageIdentifier"), own_file);
    }
    set(cstr16!("LoaderFeatures"), &FEATURES.to_le_bytes());
}

/// The ids of the Boot Loader Specification entries shown.
pub fn entries(ids: &[&str]) {
    set(cstr16!("LoaderEntries"), &li::entries(ids));
}

/// Just before starting something: the time, and the entry when it's one
/// of the Boot Loader Specification's.
pub fn starting(entry: Option<&str>) {
    if let Some(id) = entry {
        set_string(cstr16!("LoaderEntrySelected"), id);
    }
    set_string(cstr16!("LoaderTimeExecUSec"), &format!("{}", usec(super::platform::timer_tick())));
}

/// `LoaderEntryOneShot` (`bootctl reboot --boot-loader-entry`, `systemctl
/// reboot --boot-loader-entry`): read once and removed, as systemd-boot
/// does; it's the default for this boot.
pub fn take_one_shot() -> Option<alloc::string::String> {
    let name = cstr16!("LoaderEntryOneShot");
    let (data, _) = runtime::get_variable_boxed(name, &LOADER).ok()?;
    let _ = runtime::delete_variable(name, &LOADER);
    let id = crate::parse::ucs2_string(&data);
    log::info!("LoaderEntryOneShot: {id}");
    (!id.is_empty()).then_some(id)
}
