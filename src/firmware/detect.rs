// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Finds what the disks can start that no entry starts yet: macOS, Windows,
//! Linux kernels (`parse::detect`), on every file system Slingshot can read,
//! including those of file system drivers.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use uefi::proto::device_path::DevicePath;
use uefi::proto::media::fs::SimpleFileSystem;

use crate::app::FoundEntry;
use crate::parse::detect::{self, Disk};
use crate::tr::{tr, trf};

/// A folder's entries: name, and whether it's a folder.
type Listing = Vec<(String, bool)>;

/// A file system, for one scan: `parse::detect` lists the root and `\boot`
/// for each thing it looks for, each time real disk I/O with a driver.
struct FileSystem {
    handle: uefi::Handle,
    lists: RefCell<BTreeMap<String, Option<Listing>>>,
}

impl Disk for FileSystem {
    fn list(&self, dir: &str) -> Option<Listing> {
        let read = || super::dir::read_dir(self.handle, dir).ok().map(|l| l.into_iter().map(|e| (e.name, e.is_dir)).collect());
        self.lists.borrow_mut().entry(dir.into()).or_insert_with(read).clone()
    }
    // Text files only (os-release, entries, plists…).
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        super::dir::read_at_most(self.handle, path, super::dir::SMALL)
    }
}

/// The program a removable disk starts (`\EFI\BOOT\BOOTX64.EFI`).
#[cfg(target_arch = "aarch64")]
const REMOVABLE_PROGRAM: &str = "\\EFI\\BOOT\\BOOTAA64.EFI";
#[cfg(not(target_arch = "aarch64"))]
const REMOVABLE_PROGRAM: &str = "\\EFI\\BOOT\\BOOTX64.EFI";

/// What a found entry's texts are made from: made again in another language
/// without reading the disks again.
pub enum Texts {
    /// A removable disk: its label, if it has one.
    Usb(Option<String>),
    /// A system found on a disk: its kind and version.
    Found(&'static str, String),
}

impl Texts {
    /// Sets `entry`'s texts that depend on the language.
    pub fn apply(&self, entry: &mut FoundEntry) {
        match self {
            Texts::Usb(label) => {
                entry.title = label.clone().unwrap_or_else(|| tr("USB stick"));
                entry.subtitle = crate::parse::load_option::describe_volume(&entry.fs).1;
            }
            Texts::Found(kind, version) => {
                entry.subtitle = match *kind {
                    "apple" => tr("macOS"),
                    "windows" => tr("Windows Boot Manager"),
                    _ => trf("Kernel {}", &[version]),
                };
            }
        }
    }
}

/// A removable disk's own program, when no firmware entry starts that disk
/// (`devices`: what the whole-device entries start): a USB stick plugged in
/// after the firmware listed the disks.
fn removable(handle: uefi::Handle, fs: &[u8], devices: &[Vec<u8>]) -> Option<(FoundEntry, Texts)> {
    use uefi::proto::media::block::BlockIO;
    let block = super::platform::open_shared::<BlockIO>(handle)?;
    if !block.media().is_removable_media() && !crate::parse::load_option::is_usb_device(&usb_part(fs)) {
        return None;
    }
    if devices.iter().any(|d| crate::parse::load_option::is_on_device(d, fs)) || !super::dir::exists(handle, REMOVABLE_PROGRAM) {
        return None;
    }
    let label = super::volume_label(handle).map(|l| String::from(l.trim())).filter(|l| !l.is_empty());
    let texts = Texts::Usb(label);
    let mut entry = FoundEntry { kind: "usb", fs: fs.to_vec(), program: String::from(REMOVABLE_PROGRAM), ..Default::default() };
    texts.apply(&mut entry);
    log::info!("found a removable disk: {}", entry.subtitle);
    Some((entry, texts))
}

/// The nodes of `fs` up to its USB node, for [`is_usb_device`].
///
/// [`is_usb_device`]: crate::parse::load_option::is_usb_device
fn usb_part(fs: &[u8]) -> Vec<u8> {
    let mut dp = fs;
    let mut out = Vec::new();
    while dp.len() >= 4 {
        let len = usize::from(u16::from_le_bytes([dp[2], dp[3]]));
        if len < 4 || len > dp.len() || (dp[0], dp[1]) == (0x7f, 0xff) {
            break;
        }
        out.extend_from_slice(&dp[..len]);
        if (dp[0], dp[1]) == (0x03, 0x05) {
            break;
        }
        dp = &dp[len..];
    }
    out.extend_from_slice(&[0x7f, 0xff, 4, 0]);
    out
}

/// What the file systems can start, except what the firmware's and
/// systemd-boot's entries start (`started`: device path and file, see
/// [`detect::already_started`]; `devices`: the whole devices they start)
/// and the kernels on `entries_fs`, the file system with systemd-boot's
/// entries.
pub fn scan(started: &[(Vec<u8>, String)], devices: &[Vec<u8>], entries_fs: &[u8]) -> Vec<(FoundEntry, Texts)> {
    let Ok(handles) = uefi::boot::find_handles::<SimpleFileSystem>() else { return Vec::new() };
    let mut all = Vec::new();
    for handle in handles {
        let fs: Vec<u8> = super::platform::open_shared::<DevicePath>(handle).map(|dp| dp.as_bytes().to_vec()).unwrap_or_default();
        if let Some(f) = removable(handle, &fs, devices)
            && !detect::already_started(started, &fs, &f.0.program)
        {
            all.push(f);
            continue;
        }
        let partuuid = detect::partition_uuid(&fs);
        for f in detect::scan(&FileSystem { handle, lists: RefCell::default() }, partuuid.as_deref()) {
            if detect::already_started(started, &fs, &f.program)
                || (f.kind == "linux" && !entries_fs.is_empty() && fs == entries_fs)
            {
                continue;
            }
            log::info!("found: {} ({})", f.title, f.program);
            let texts = Texts::Found(f.kind, f.version);
            let mut entry = FoundEntry { title: f.title, kind: f.kind, fs: fs.clone(), program: f.program, options: f.options, ..Default::default() };
            texts.apply(&mut entry);
            all.push((entry, texts));
        }
    }
    all
}
