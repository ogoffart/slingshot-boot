// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Tells the operating system about Slingshot and the boot, as systemd-boot
//! does: the variables of systemd's Boot Loader Interface, read by `bootctl
//! status`, `systemd-analyze` (the time spent in the firmware and the boot
//! loader) and systemd-gpt-auto-generator (which partition is the ESP).

use alloc::format;
use core::sync::atomic::{AtomicU64, Ordering};

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

/// Microseconds since the CPU started (its time stamp counter), as
/// systemd-boot measures them: `LoaderTimeInitUSec` and
/// `LoaderTimeExecUSec`; 0 when unknown.
fn time_usec() -> u64 {
    let (ticks, hz) = counter();
    if hz == 0 { 0 } else { (u128::from(ticks) * 1_000_000 / u128::from(hz)) as u64 }
}

#[cfg(target_arch = "x86_64")]
fn counter() -> (u64, u64) {
    use core::arch::x86_64::{__cpuid, _rdtsc};
    static HZ: AtomicU64 = AtomicU64::new(0);
    // SAFETY: every x86-64 processor has RDTSC and CPUID.
    let ticks = unsafe { _rdtsc() };
    let mut hz = HZ.load(Ordering::Relaxed);
    if hz == 0 {
        // The TSC frequency from CPUID leaf 0x15 when the processor says,
        // else measured over a millisecond.
        // Leaf 0 says which leaves exist.
        let max_leaf = __cpuid(0).eax;
        if max_leaf >= 0x15 {
            let r = __cpuid(0x15);
            if r.eax != 0 && r.ebx != 0 && r.ecx != 0 {
                hz = u64::from(r.ecx) * u64::from(r.ebx) / u64::from(r.eax);
            }
        }
        if hz == 0 {
            // SAFETY: as above.
            let start = unsafe { _rdtsc() };
            uefi::boot::stall(core::time::Duration::from_millis(1));
            // SAFETY: as above.
            hz = (unsafe { _rdtsc() } - start) * 1000;
        }
        HZ.store(hz, Ordering::Relaxed);
    }
    (ticks, hz)
}

#[cfg(target_arch = "aarch64")]
fn counter() -> (u64, u64) {
    let (ticks, hz): (u64, u64);
    // SAFETY: the generic timer's registers, readable at EL1 and below.
    unsafe {
        core::arch::asm!("mrs {}, cntvct_el0", out(reg) ticks);
        core::arch::asm!("mrs {}, cntfrq_el0", out(reg) hz);
    }
    (ticks, hz)
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn counter() -> (u64, u64) {
    (0, 0)
}

/// When Slingshot started, for `LoaderTimeInitUSec`.
static INIT: AtomicU64 = AtomicU64::new(0);

/// First thing at startup: the time, before anything else takes some.
pub fn started() {
    INIT.store(time_usec(), Ordering::Relaxed);
}

/// Once the firmware is ready (the image's device path can be read):
/// what Slingshot is, where it was started from, what it supports.
pub fn announce(own_fs_device_path: &[u8], own_file: &str) {
    let init = INIT.load(Ordering::Relaxed);
    if init != 0 {
        set_string(cstr16!("LoaderTimeInitUSec"), &format!("{init}"));
    }
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
    let now = time_usec();
    if now != 0 {
        set_string(cstr16!("LoaderTimeExecUSec"), &format!("{now}"));
    }
}

/// `LoaderEntryOneShot` (`bootctl reboot --boot-loader-entry`, `systemctl
/// reboot --boot-loader-entry`): read once and removed, as systemd-boot
/// does; it's the default for this boot.
pub fn take_one_shot() -> Option<alloc::string::String> {
    static TAKEN: spin_once::Once = spin_once::Once::new();
    TAKEN.get_or_init(|| {
        let name = cstr16!("LoaderEntryOneShot");
        let (data, _) = runtime::get_variable_boxed(name, &LOADER).ok()?;
        let _ = runtime::delete_variable(name, &LOADER);
        let id = crate::parse::ucs2_string(&data);
        log::info!("LoaderEntryOneShot: {id}");
        (!id.is_empty()).then_some(id)
    })
}

/// A value computed once (the firmware is single-threaded).
mod spin_once {
    use alloc::string::String;
    use core::cell::OnceCell;

    pub struct Once(OnceCell<Option<String>>);

    // SAFETY: UEFI boot services run on one processor; nothing else
    // touches it.
    unsafe impl Sync for Once {}

    impl Once {
        pub const fn new() -> Self {
            Once(OnceCell::new())
        }

        pub fn get_or_init(&self, f: impl FnOnce() -> Option<String>) -> Option<String> {
            self.0.get_or_init(f).clone()
        }
    }
}
