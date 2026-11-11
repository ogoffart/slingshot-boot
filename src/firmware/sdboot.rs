// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! systemd-boot integration: reads the boot loader entries from the EFI
//! System Partition (`\loader\entries\*.conf` and unified kernel images in
//! `\EFI\Linux\`), and boots one of them through systemd-boot's one-shot
//! variables, so that systemd-boot starts it without showing its menu.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use uefi::boot;
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::runtime;
use uefi::{CString16, cstr16};

/// An entry with a boot counter: its id and its file.
pub type Counted = (String, String);

use super::loader_interface::LOADER;

use super::bootvars::NV_BS_RT;
use crate::app::{LoaderConfig, LoaderEntry};
use crate::parse::bls::{self, ends_with_ignore_case};

// Through `dir`, which also reads file systems of drivers like rEFInd's
// (a Linux `/boot` partition with entries, as Fedora has).
fn list_dir(handle: uefi::Handle, dir: &str) -> Vec<String> {
    super::dir::read_dir(handle, dir)
        .map(|entries| entries.into_iter().filter(|e| !e.is_dir).map(|e| e.name).collect())
        .unwrap_or_default()
}

fn read_file(handle: uefi::Handle, path: &str) -> Option<String> {
    String::from_utf8(super::dir::read_at_most(handle, path, super::dir::SMALL)?).ok()
}

/// Whether a file named in an entry exists ("/vmlinuz-linux" style paths).
fn file_exists(handle: uefi::Handle, path: &str) -> bool {
    super::dir::exists(handle, &bls::efi_path(path))
}

fn get_string_var(name: &uefi::CStr16) -> Option<String> {
    let (data, _) = runtime::get_variable_boxed(name, &LOADER).ok()?;
    Some(crate::parse::ucs2_string(&data))
}

/// An Extended Boot Loader partition (the Boot Loader Specification's
/// XBOOTLDR, where some systems put the kernels and the entries).
fn is_xbootldr(handle: uefi::Handle) -> bool {
    use uefi::proto::media::partition::PartitionInfo;
    let Some(info) = super::platform::open_shared::<PartitionInfo>(handle) else { return false };
    info.gpt_partition_entry().is_some_and(|e| {
        let kind = { e.partition_type_guid };
        kind.0 == uefi::guid!("bc13c2ff-59e6-4262-a352-b275fd6f7172")
    })
}

/// What `read_config` reads or does once per boot, though it runs again
/// (`boot_data`, when the language changes).
#[derive(Default)]
pub struct OncePerBoot {
    one_shot: core::cell::OnceCell<Option<String>>,
    measured: core::cell::Cell<bool>,
}

/// Finds the file system with Boot Loader Specification entries (systemd-boot's
/// configuration, on the ESP) and reads them. Leaves out entries that start
/// `own_file` (Slingshot itself). Also returns that file system's device path,
/// to start the entries from, and the files of the entries with a boot
/// counter, for [`count_try`].
pub fn read_config(own_file: &str, once: &OncePerBoot) -> Option<(LoaderConfig, Vec<u8>, Vec<Counted>)> {
    let handles = boot::find_handles::<SimpleFileSystem>().ok()?;
    // Slingshot's own partition, then an XBOOTLDR one, then the others
    // (Fedora's /boot), all on Slingshot's disk. Never a USB stick's or
    // another disk's: their entries would become the default, and start
    // by themselves.
    let own = super::own_fs_device_path();
    let own_disk = crate::parse::load_option::disk_part(&own);
    let mut handles: Vec<(u8, uefi::Handle)> = handles
        .into_iter()
        .filter_map(|h| {
            let dp = super::platform::open_shared::<uefi::proto::device_path::DevicePath>(h)?.as_bytes().to_vec();
            if dp == own {
                Some((0, h))
            } else if !own_disk.is_empty() && crate::parse::load_option::disk_part(&dp) == own_disk {
                Some((if is_xbootldr(h) { 1 } else { 2 }, h))
            } else {
                None
            }
        })
        .collect();
    handles.sort_by_key(|(rank, _)| *rank);
    for (_, handle) in handles {
        let conf_files: Vec<String> = list_dir(handle, "\\loader\\entries")
            .into_iter()
            .filter(|n| ends_with_ignore_case(n, ".conf"))
            .collect();
        let ukis: Vec<String> = list_dir(handle, "\\EFI\\Linux")
            .into_iter()
            .filter(|n| ends_with_ignore_case(n, ".efi"))
            .collect();
        if conf_files.is_empty() && ukis.is_empty() {
            continue;
        }

        let confs: Vec<(String, String)> = conf_files
            .iter()
            .filter_map(|name| Some((name.clone(), read_file(handle, &format!("\\loader\\entries\\{name}"))?)))
            .collect();
        // Default: `bootctl reboot --boot-loader-entry` for this boot
        // (LoaderEntryOneShot), else `bootctl set-default`
        // (LoaderEntryDefault), else the `default` pattern of loader.conf,
        // else the first entry.
        let one_shot = once.one_shot.get_or_init(super::loader_interface::take_one_shot).clone();
        let default_var = one_shot.clone().or_else(|| get_string_var(cstr16!("LoaderEntryDefault")));
        let loader_conf = read_file(handle, "\\loader\\loader.conf");
        // Measured once, like systemd-boot does at its start.
        if let Some(conf) = &loader_conf
            && !once.measured.replace(true)
        {
            super::measure::loader_conf(conf.as_bytes());
        }
        let (entries, default) = bls::menu(
            &confs,
            &ukis,
            &mut |f| file_exists(handle, f),
            own_file,
            default_var.as_deref(),
            loader_conf.as_deref(),
        );

        log::info!("systemd-boot: {} entries, default {}", entries.len(), default);
        super::loader_interface::entries(&entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>());
        let counted = entries.iter().filter(|e| e.tries.is_some()).map(|e| (e.id.clone(), e.path.clone())).collect();
        let entries = entries
            .into_iter()
            .map(LoaderEntry::from)
            .collect();
        let device_path = super::platform::open_shared::<uefi::proto::device_path::DevicePath>(handle)
            .map(|dp| dp.as_bytes().to_vec())
            .unwrap_or_default();
        let pattern = default_var.or_else(|| loader_conf.as_deref().and_then(bls::loader_conf_default));
        let saved = pattern.as_deref() == Some("@saved");
        let last_booted = get_string_var(cstr16!("LoaderEntryLastBooted"));
        let editor_off = !loader_conf.as_deref().is_none_or(bls::loader_conf_editor);
        return Some((LoaderConfig { entries, default, one_shot, saved, last_booted, editor_off }, device_path, counted));
    }
    None
}

fn set_string_var(name: &uefi::CStr16, value: &str) -> uefi::Result {
    let data = crate::parse::ucs2_bytes(value);
    runtime::set_variable(
        name,
        &LOADER,
        NV_BS_RT,
        &data,
    )
}

/// The entry started last, for systemd-boot's `@saved` default (as
/// systemd-boot keeps it); written only when it changes.
pub fn set_last_booted(id: &str) {
    if get_string_var(cstr16!("LoaderEntryLastBooted")).as_deref() == Some(id) {
        return;
    }
    if let Err(e) = set_string_var(cstr16!("LoaderEntryLastBooted"), id) {
        log::info!("LoaderEntryLastBooted: {:?}", e.status());
    }
}

/// Asks systemd-boot to boot `id` on its next start (systemd-boot deletes
/// the variable when it reads it). `LoaderConfigTimeoutOneShot` must not be
/// set as well: it forces systemd-boot's menu to show.
pub fn select_one_shot(id: &str) -> uefi::Result {
    set_string_var(cstr16!("LoaderEntryOneShot"), id)
}

/// Counts a try of a boot-counted entry (systemd's Automatic Boot
/// Assessment), before starting it: renames its file `path` on the file
/// system `fs_device_path` from `+3-0` to `+2-1`, and tells
/// `systemd-bless-boot` where it is now (`LoaderBootCountPath`), so that it
/// can remove the counter once the system has started. Returns the new
/// path, or `None` when there's nothing to count or the rename failed.
pub fn count_try(fs_device_path: &[u8], path: &str) -> Option<String> {
    use uefi::proto::device_path::DevicePath;
    use uefi::proto::media::file::{File, FileAttribute, FileInfo, FileMode};

    let next = bls::next_try_path(path)?;
    let new_name = next.rsplit('\\').next()?;
    let fs_dp: &DevicePath = fs_device_path.try_into().ok()?;
    let handle = boot::locate_device_path::<SimpleFileSystem>(&mut &*fs_dp).ok()?;
    let mut sfs = super::platform::open_shared::<SimpleFileSystem>(handle)?;
    let mut root = sfs.open_volume().ok()?;
    let mut file = root.open(&CString16::try_from(path).ok()?, FileMode::ReadWrite, FileAttribute::empty()).ok()?;
    let info = file.get_boxed_info::<FileInfo>().ok()?;
    // A rename is a SetInfo with the new name (in the same directory): the
    // file isn't copied, which matters for a 50 MB unified kernel image.
    let name = CString16::try_from(new_name).ok()?;
    let mut buffer = alloc::boxed::Box::new(super::dir::InfoBuffer([0; 4096]));
    let renamed = FileInfo::new(
        &mut buffer.0,
        info.file_size(),
        info.physical_size(),
        *info.create_time(),
        *info.last_access_time(),
        *info.modification_time(),
        info.attribute(),
        &name,
    )
    .ok()?;
    if let Err(e) = file.set_info(renamed) {
        log::warn!("boot counting: cannot rename {path}: {:?}", e.status());
        return None;
    }
    let _ = file.flush();
    log::info!("boot counting: {path} → {next}");
    // Volatile, like systemd-boot sets it.
    let data = crate::parse::ucs2_bytes(&next);
    let attributes = uefi::runtime::VariableAttributes::BOOTSERVICE_ACCESS | uefi::runtime::VariableAttributes::RUNTIME_ACCESS;
    if let Err(e) = runtime::set_variable(cstr16!("LoaderBootCountPath"), &LOADER, attributes, &data) {
        log::warn!("boot counting: cannot set LoaderBootCountPath: {:?}", e.status());
    }
    Some(next)
}
