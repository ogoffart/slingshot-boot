// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Secure Boot with shim. With Secure Boot on, the firmware only starts
//! programs signed by a key in its database (db): Microsoft's, usually.
//! Distributions sign their kernels with their own key, which only shim
//! knows (built in, or enrolled as a MOK). When shim started Slingshot,
//! Slingshot asks shim (its SHIM_LOCK protocol) to verify a kernel before
//! loading it, and approves exactly that verified buffer in the firmware's
//! security check while it loads it, like systemd-boot does. Anything shim
//! doesn't accept is refused.

use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};

use uefi::boot::{self, LoadImageSource};
use uefi::proto::device_path::DevicePath;
use uefi::proto::unsafe_protocol;
use uefi::{Handle, Identify, Status};

/// shim's protocol: `verify` checks a PE image against shim's keys, the
/// MOKs, db and dbx. Unlike UEFI's protocols, shim's functions use the
/// System V calling convention on x86_64 (systemd-boot declares them
/// `__sysv_abi__` too): with "efiapi", shim reads garbage and answers
/// UNSUPPORTED.
#[repr(C)]
#[unsafe_protocol("605dab50-e046-4300-abb6-3dd810dd8b23")]
struct ShimLock {
    #[cfg(target_arch = "x86_64")]
    verify: unsafe extern "sysv64" fn(buffer: *const c_void, size: u32) -> Status,
    #[cfg(not(target_arch = "x86_64"))]
    verify: unsafe extern "C" fn(buffer: *const c_void, size: u32) -> Status,
    // hash and context follow; not used.
}

type Authenticate2 = unsafe extern "efiapi" fn(
    this: *const Security2,
    device_path: *const c_void,
    buffer: *const c_void,
    size: usize,
    boot_policy: bool,
) -> Status;
type Authenticate = unsafe extern "efiapi" fn(this: *const Security, status: u32, device_path: *const c_void) -> Status;

/// The firmware's security check for images loaded from a buffer
/// (EFI_SECURITY2_ARCH_PROTOCOL).
#[repr(C)]
#[unsafe_protocol("94ab2f58-1438-4ef1-9152-18941a3a0e68")]
struct Security2 {
    file_authentication: Authenticate2,
}

/// The firmware's older security check, by device path
/// (EFI_SECURITY_ARCH_PROTOCOL).
#[repr(C)]
#[unsafe_protocol("a46423e3-4617-49f1-b9ff-d1bfa9115839")]
struct Security {
    file_authentication_state: Authenticate,
}

/// SBAT metadata (https://github.com/rhboot/shim/blob/main/SBAT.md): shim
/// refuses to start a program without it, so that a vulnerable generation
/// can be revoked. Bump the `slingshot` generation (the second field) when
/// a security fix must revoke the older ones.
#[used]
#[unsafe(link_section = ".sbat")]
static SBAT: [u8; SBAT_CSV.len()] = {
    let mut sbat = [0; SBAT_CSV.len()];
    let mut i = 0;
    while i < sbat.len() {
        sbat[i] = SBAT_CSV[i];
        i += 1;
    }
    sbat
};
const SBAT_CSV: &[u8] = concat!(
    "sbat,1,SBAT Version,sbat,1,https://github.com/rhboot/shim/blob/main/SBAT.md\n",
    "slingshot,1,Slingshot,slingshot,",
    env!("CARGO_PKG_VERSION"),
    ",https://github.com/ogoffart/slingshot-boot\n",
)
.as_bytes();

/// Secure Boot is enforced.
pub fn enforced() -> bool {
    matches!(super::sysinfo::secure_boot(), super::sysinfo::SecureBoot::On)
}

/// shim is there (it started Slingshot) and Secure Boot is enforced: kernels
/// go through shim's verification.
pub fn shim_active() -> bool {
    enforced() && boot::locate_handle_buffer(boot::SearchType::ByProtocol(&ShimLock::GUID)).is_ok_and(|h| !h.is_empty())
}

/// Asks shim whether `data` is signed by a key it trusts.
pub fn shim_verify(data: &[u8]) -> bool {
    let Ok(handle) = boot::get_handle_for_protocol::<ShimLock>() else { return false };
    let Some(shim) = crate::uefi_platform::open_shared::<ShimLock>(handle) else { return false };
    let Ok(size) = u32::try_from(data.len()) else { return false };
    // SAFETY: shim reads `size` bytes of `data`.
    let status = unsafe { (shim.verify)(data.as_ptr().cast(), size) };
    log::info!("shim verify: {status:?}");
    status == Status::SUCCESS
}

// The approved buffer, and the firmware's functions the hooks replace,
// while `load_verified` runs (single-threaded: boot services).
static APPROVED_PTR: AtomicUsize = AtomicUsize::new(0);
static APPROVED_LEN: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL2: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL1: AtomicUsize = AtomicUsize::new(0);

unsafe extern "efiapi" fn security2_hook(
    this: *const Security2,
    device_path: *const c_void,
    buffer: *const c_void,
    size: usize,
    boot_policy: bool,
) -> Status {
    let approved = APPROVED_PTR.load(Ordering::Relaxed);
    if approved != 0 && buffer as usize == approved && size == APPROVED_LEN.load(Ordering::Relaxed) {
        return Status::SUCCESS;
    }
    // SAFETY: the firmware's own function, saved by `load_verified`.
    let original = unsafe { core::mem::transmute::<usize, Authenticate2>(ORIGINAL2.load(Ordering::Relaxed)) };
    unsafe { original(this, device_path, buffer, size, boot_policy) }
}

unsafe extern "efiapi" fn security_hook(this: *const Security, status: u32, device_path: *const c_void) -> Status {
    // Only called by the firmware inside our LoadImage of the verified
    // buffer (the hook is removed right after).
    if APPROVED_PTR.load(Ordering::Relaxed) != 0 {
        return Status::SUCCESS;
    }
    // SAFETY: the firmware's own function, saved by `load_verified`.
    let original = unsafe { core::mem::transmute::<usize, Authenticate>(ORIGINAL1.load(Ordering::Relaxed)) };
    unsafe { original(this, status, device_path) }
}

/// Loads `data` (the file at `device_path`), which shim verified, with the
/// firmware's security check approving exactly this buffer.
pub fn load_verified(device_path: &DevicePath, data: &[u8]) -> uefi::Result<Handle> {
    let security2 = boot::get_handle_for_protocol::<Security2>().ok().and_then(crate::uefi_platform::open_shared::<Security2>);
    let security = boot::get_handle_for_protocol::<Security>().ok().and_then(crate::uefi_platform::open_shared::<Security>);
    APPROVED_PTR.store(data.as_ptr() as usize, Ordering::Relaxed);
    APPROVED_LEN.store(data.len(), Ordering::Relaxed);
    // SAFETY: the protocols are the firmware's, alive during boot services;
    // their functions are put back below, before returning.
    unsafe {
        if let Some(s) = &security2 {
            let p = &**s as *const Security2 as *mut Security2;
            ORIGINAL2.store((*p).file_authentication as usize, Ordering::Relaxed);
            (*p).file_authentication = security2_hook;
        }
        if let Some(s) = &security {
            let p = &**s as *const Security as *mut Security;
            ORIGINAL1.store((*p).file_authentication_state as usize, Ordering::Relaxed);
            (*p).file_authentication_state = security_hook;
        }
    }
    let result = boot::load_image(
        boot::image_handle(),
        LoadImageSource::FromBuffer { buffer: data, file_path: Some(device_path) },
    );
    unsafe {
        if let Some(s) = &security2 {
            let p = &**s as *const Security2 as *mut Security2;
            (*p).file_authentication = core::mem::transmute::<usize, Authenticate2>(ORIGINAL2.load(Ordering::Relaxed));
        }
        if let Some(s) = &security {
            let p = &**s as *const Security as *mut Security;
            (*p).file_authentication_state = core::mem::transmute::<usize, Authenticate>(ORIGINAL1.load(Ordering::Relaxed));
        }
    }
    APPROVED_PTR.store(0, Ordering::Relaxed);
    APPROVED_LEN.store(0, Ordering::Relaxed);
    result
}
