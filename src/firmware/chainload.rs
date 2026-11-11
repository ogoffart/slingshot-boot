// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Starting a boot entry directly, like the firmware's boot manager does:
//! `LoadImage` on the entry's device path, then `StartImage`. Much faster
//! than setting `BootNext` and resetting, which runs the whole firmware
//! start-up (POST) again.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use uefi::boot::{self, LoadImageSource};
use uefi::proto::BootPolicy;
use uefi::proto::device_path::DevicePath;
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::Status;

use crate::parse::load_option::{expand_device_path, load_option_parts};

pub enum Outcome {
    /// The program ran and returned to Slingshot (e.g. the shell's `exit`).
    Returned(Status),
    /// It could not be loaded here: the caller falls back to `BootNext` and a
    /// reset, so that the firmware starts it.
    NotLoaded,
    /// Secure Boot refused it: not signed by a key the firmware (or shim)
    /// trusts. Not worth a reset: the firmware would refuse it too.
    Refused,
}

/// The device paths of all file systems.
fn file_system_paths() -> Vec<Vec<u8>> {
    let Ok(handles) = boot::find_handles::<SimpleFileSystem>() else { return Vec::new() };
    handles
        .iter()
        .filter_map(|h| super::platform::open_shared::<DevicePath>(*h))
        .map(|dp| dp.as_bytes().to_vec())
        .collect()
}

/// Starts `Boot{number:04X}`.
pub fn start_boot_option(number: u16) -> Outcome {
    let Some(data) = super::bootvars::boot_option_data(number) else { return Outcome::NotLoaded };
    let Some((path, optional)) = load_option_parts(&data) else { return Outcome::NotLoaded };
    let candidates = expand_device_path(path, &file_system_paths());
    log::info!("Boot{number:04X}: {} device path(s) to try", candidates.len());
    for candidate in &candidates {
        if let Some(outcome) = load_and_start(candidate, optional) {
            return outcome;
        }
    }
    Outcome::NotLoaded
}

/// Loads and starts the program at `device_path`, passing `options` as its
/// load options. `None` if it can't be loaded.
pub fn load_and_start(device_path: &[u8], options: &[u8]) -> Option<Outcome> {
    let dp: &DevicePath = device_path.try_into().ok()?;
    let image = match boot::load_image(
        boot::image_handle(),
        LoadImageSource::FromDevicePath { device_path: dp, boot_policy: BootPolicy::BootSelection },
    ) {
        Ok(image) => image,
        Err(e) if matches!(e.status(), Status::SECURITY_VIOLATION | Status::ACCESS_DENIED) => {
            log::info!("LoadImage: {:?} (Secure Boot)", e.status());
            return Some(Outcome::Refused);
        }
        Err(e) => {
            log::info!("LoadImage: {:?}", e.status());
            return None;
        }
    };
    run_image(image, options)
}

/// Sets `options` as the image's load options, starts it, and unloads it
/// when it returns.
fn run_image(image: uefi::Handle, options: &[u8]) -> Option<Outcome> {
    if !options.is_empty()
        && let Ok(mut loaded) = boot::open_protocol_exclusive::<LoadedImage>(image)
    {
        // SAFETY: `options` outlives the image's run: start_image returns
        // before this function does.
        unsafe { loaded.set_load_options(options.as_ptr(), options.len() as u32) };
    }
    forget_keys();
    let status = match boot::start_image(image) {
        Ok(()) => Status::SUCCESS,
        Err(e) => e.status(),
    };
    log::info!("StartImage returned {status:?}");
    // A program that returns is unloaded by the firmware when it called
    // Exit(); unload in case it just returned from its entry point.
    let _ = boot::unload_image(image);
    Some(Outcome::Returned(status))
}

/// Empties the firmware's keyboard buffer as the simple text input reads
/// it: Slingshot reads keys with the extended one, and some firmware
/// (Dell's) keeps a queue for each, so every key pressed in Slingshot would
/// still be waiting there, and a program that shows its menu when a key is
/// pressed (Ubuntu's GRUB, hidden otherwise) would show it every time.
fn forget_keys() {
    uefi::system::with_stdin(|stdin| {
        let _ = stdin.reset(false);
        let mut left = 0;
        while let Ok(Some(_)) = stdin.read_key() {
            left += 1;
            if left > 256 {
                break;
            }
        }
    });
}

/// Starts `file` (e.g. `\vmlinuz-linux`) from the file system of
/// `fs_device_path`, with `command_line` as its load options (UCS-2, as the
/// Linux EFI stub and the UEFI shell read them). `None` if it can't be
/// loaded.
pub fn start_file(fs_device_path: &[u8], file: &str, command_line: &str) -> Option<Outcome> {
    let path = crate::parse::load_option::file_device_path(fs_device_path, file);
    let options: Vec<u8> = if command_line.is_empty() {
        Vec::new()
    } else {
        crate::parse::ucs2_bytes(command_line)
    };
    log::info!("starting {file} {command_line}");
    // As systemd-boot does: a command line passed explicitly is measured;
    // a unified kernel image's own is measured with the image.
    super::measure::kernel_command_line(command_line);
    if super::secureboot::shim_active() {
        return start_with_shim(fs_device_path, file, &path, &options);
    }
    load_and_start(&path, &options)
}

/// Secure Boot with shim: shim verifies the file (a distribution's kernel
/// is signed with a key only shim knows), then it's loaded from that buffer.
fn start_with_shim(fs_device_path: &[u8], file: &str, path: &[u8], options: &[u8]) -> Option<Outcome> {
    let data = super::dir::read_file(super::dir::handle_of(fs_device_path)?, file)?;
    if !super::secureboot::shim_verify(&data) {
        return Some(Outcome::Refused);
    }
    let dp: &DevicePath = path.try_into().ok()?;
    match super::secureboot::load_verified(dp, &data) {
        Ok(image) => run_image(image, options),
        Err(e) => {
            log::info!("LoadImage (verified by shim): {:?}", e.status());
            Some(Outcome::Refused)
        }
    }
}

/// "exited", or "exited with an error: LOAD_ERROR".
pub fn describe_status(status: Status) -> String {
    if status == Status::SUCCESS { String::from("exited") } else { format!("exited with an error: {status:?}") }
}
