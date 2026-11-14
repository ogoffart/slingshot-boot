// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Reading and writing the firmware's boot variables: `BootOrder`,
//! `Boot####`, `BootNext`, `BootCurrent`, `Timeout` and `OsIndications`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use uefi::runtime::{self, ResetType, VariableAttributes, VariableVendor};
use uefi::{CStr16, CString16, Status, cstr16};

const GLOBAL: VariableVendor = VariableVendor::GLOBAL_VARIABLE;

const OS_INDICATIONS_BOOT_TO_FW_UI: u64 = 0x1;
/// Non-volatile, with boot services and runtime access.
pub const NV_BS_RT: VariableAttributes = VariableAttributes::NON_VOLATILE
    .union(VariableAttributes::BOOTSERVICE_ACCESS)
    .union(VariableAttributes::RUNTIME_ACCESS);

use crate::app::BootOption;
use crate::parse::load_option::{parse_device_path, parse_load_option};
pub use crate::parse::load_option::normalize_path;

fn get_var(name: &CStr16) -> Option<(alloc::boxed::Box<[u8]>, VariableAttributes)> {
    runtime::get_variable_boxed(name, &GLOBAL).ok()
}

fn get_u16(name: &CStr16) -> Option<u16> {
    let (data, _) = get_var(name)?;
    crate::parse::le_u16s(&data).next()
}

fn get_u64(name: &CStr16) -> Option<u64> {
    let (data, _) = get_var(name)?;
    let bytes: [u8; 8] = data.get(..8)?.try_into().ok()?;
    Some(u64::from_le_bytes(bytes))
}

pub fn boot_current() -> Option<u16> {
    get_u16(cstr16!("BootCurrent"))
}

/// The firmware's `Timeout` in seconds (0xFFFF means wait forever).
pub fn firmware_timeout() -> Option<u16> {
    get_u16(cstr16!("Timeout"))
}

pub fn boot_order() -> Option<(Vec<u16>, VariableAttributes)> {
    let (data, attrs) = get_var(cstr16!("BootOrder"))?;
    let order = crate::parse::le_u16s(&data).collect();
    Some((order, attrs))
}

pub fn set_boot_order(order: &[u16], attrs: VariableAttributes) -> uefi::Result {
    let data: Vec<u8> = order.iter().flat_map(|n| n.to_le_bytes()).collect();
    runtime::set_variable(cstr16!("BootOrder"), &GLOBAL, attrs, &data)
}

/// Adds `Boot####` starting `device_path`, at the end of the boot order (so
/// what boots by default doesn't change). Returns its number.
pub fn add_boot_option(description: &str, device_path: &[u8]) -> Result<u16, String> {
    let number = (0..u16::MAX).find(|n| boot_option_data(*n).is_none()).ok_or("no free Boot#### number")?;
    let name = CString16::try_from(format!("Boot{number:04X}").as_str()).map_err(|_| String::from("bad name"))?;
    let data = crate::parse::load_option::new_load_option(description, device_path);
    runtime::set_variable(&name, &GLOBAL, NV_BS_RT, &data).map_err(|e| format!("{:?}", e.status()))?;
    let (mut order, attrs) = boot_order().unwrap_or((Vec::new(), NV_BS_RT));
    order.push(number);
    set_boot_order(&order, attrs).map_err(|e| format!("{:?}", e.status()))?;
    Ok(number)
}

/// The raw data of `Boot{number:04X}`.
pub fn boot_option_data(number: u16) -> Option<alloc::boxed::Box<[u8]>> {
    let name = CString16::try_from(format!("Boot{number:04X}").as_str()).ok()?;
    Some(get_var(&name)?.0)
}

/// The file path this app was loaded from (lowercase), e.g.
/// `\efi\slingshot\slingshot.efi`.
pub fn own_file_path() -> String {
    use uefi::proto::loaded_image::LoadedImage;
    let Ok(image) = uefi::boot::open_protocol_exclusive::<LoadedImage>(uefi::boot::image_handle())
    else {
        return String::new();
    };
    let file = image.file_path().map(|p| parse_device_path(p.as_bytes()).file).unwrap_or_default();
    normalize_path(&file)
}

/// A `Boot####` entry, read and parsed once for what's made of it.
pub struct OrderEntry {
    pub described: crate::parse::load_option::Described,
    /// Its device path, if it has one.
    pub path: Option<Vec<u8>>,
}

/// The `Boot####` variables of `order` that can be read: number and data.
pub fn read_entries(order: &[u16]) -> Vec<(u16, alloc::boxed::Box<[u8]>)> {
    order.iter().filter_map(|n| Some((*n, boot_option_data(*n)?))).collect()
}

/// Parses what [`read_entries`] read (again when the language changes: the
/// descriptions are translated).
pub fn parse_entries(raw: &[(u16, alloc::boxed::Box<[u8]>)]) -> Vec<OrderEntry> {
    raw.iter()
        .filter_map(|(n, data)| {
            let path = crate::parse::load_option::load_option_parts(data).map(|p| p.0.to_vec());
            Some(OrderEntry { described: parse_load_option(*n, data)?, path })
        })
        .collect()
}

/// The visible entries of `BootOrder` (`entries`), leaving out Slingshot itself:
/// entries pointing to our own file, and `BootCurrent` when it is a
/// removable-media entry (no file path, as when started as BOOTX64.EFI).
/// When started from another boot manager, `BootCurrent` is that boot
/// manager's entry, which stays in the list.
/// A USB disk's entry is `absent` when none of `devices` (the disks and
/// partitions there are) is on it: unplugged since the firmware listed it.
pub fn visible_boot_options(entries: &[OrderEntry], current: Option<u16>, own_file: &str, own_fs: &[u8], devices: &[Vec<u8>]) -> Vec<BootOption> {
    entries
        .iter()
        .map(|e| (&e.described, e.path.as_deref().unwrap_or_default()))
        .filter(|(d, path)| {
            // The same file on another disk (a USB stick's \EFI\BOOT\BOOTX64.EFI
            // when Slingshot is the ESP's) isn't Slingshot.
            let is_self = !own_file.is_empty()
                && d.file == own_file
                && crate::parse::load_option::is_on_file_system(path, own_fs);
            let is_current_removable = Some(d.option.number) == current && d.file.is_empty();
            !is_self && !is_current_removable
        })
        .map(|(d, path)| {
            use crate::parse::load_option::{is_on_device, is_usb_device};
            let absent = is_usb_device(path) && !devices.iter().any(|dev| is_on_device(path, dev));
            if absent {
                log::info!("Boot{:04X}: its USB disk isn't there", d.option.number);
            }
            BootOption { absent, ..d.option.clone() }
        })
        .collect()
}

/// The device paths of the whole devices the entries numbered `shown`
/// start (no file: removable disks, network cards).
pub fn option_devices(entries: &[OrderEntry], shown: &[u16]) -> Vec<Vec<u8>> {
    entries
        .iter()
        .filter(|e| shown.contains(&e.described.option.number) && e.described.file.is_empty())
        .filter_map(|e| e.path.clone())
        .collect()
}

/// What the boot entries start: each one's device path and file (from
/// [`normalize_path`]).
///
/// [`normalize_path`]: crate::parse::load_option::normalize_path
pub fn option_files(entries: &[OrderEntry]) -> Vec<(Vec<u8>, String)> {
    entries
        .iter()
        .filter(|e| !e.described.file.is_empty())
        .map(|e| (e.path.clone().unwrap_or_default(), e.described.file.clone()))
        .collect()
}

/// Boots `number` on the next start, then resets the machine.
pub fn boot_next_and_reset(number: u16) -> uefi::Result {
    runtime::set_variable(
        cstr16!("BootNext"),
        &GLOBAL,
        NV_BS_RT,
        &number.to_le_bytes(),
    )?;
    runtime::reset(ResetType::COLD, Status::SUCCESS, None)
}

pub fn firmware_setup_supported() -> bool {
    get_u64(cstr16!("OsIndicationsSupported"))
        .is_some_and(|v| v & OS_INDICATIONS_BOOT_TO_FW_UI != 0)
}

/// Asks the firmware to open its setup screen on the next start, then resets.
pub fn reboot_to_firmware_setup() -> uefi::Result {
    let current = get_u64(cstr16!("OsIndications")).unwrap_or(0);
    runtime::set_variable(
        cstr16!("OsIndications"),
        &GLOBAL,
        NV_BS_RT,
        &(current | OS_INDICATIONS_BOOT_TO_FW_UI).to_le_bytes(),
    )?;
    runtime::reset(ResetType::COLD, Status::SUCCESS, None)
}
