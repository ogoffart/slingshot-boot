// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! The UEFI side: the Slint platform (display, input, event loop) and the
//! [`Backend`] that reads and writes firmware variables.

mod battery;
mod bootvars;
mod chainload;
mod dir;
#[cfg(all(target_arch = "x86_64", feature = "touchpad"))]
mod i2c;
#[cfg(all(target_arch = "x86_64", feature = "touchpad"))]
mod touchpad;
mod debuglog;
mod detect;
mod loader_interface;
mod measure;
#[cfg(feature = "luks")]
mod luks;
mod nvram;
mod platform;
mod sdboot;
mod secureboot;
mod sysinfo;
mod usbmouse;

use crate::tr::{tr, trf};
use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::time::Duration;

use crate::app::{Backend, BootData, DirEntry, FoundEntry, InfoCard, Saved, StartResult, SystemInfo, Target, Volume, format_bytes};
use uefi::prelude::*;
use uefi::runtime::VariableAttributes;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    log::error!("panic: {info}");
    if battery::panicked_in_aml() {
        // The firmware's battery code: start again without it.
        uefi::runtime::reset(uefi::runtime::ResetType::WARM, Status::ABORTED, None)
    }
    uefi::println!("[PANIC]: {info}");
    uefi::boot::stall(Duration::from_secs(10));
    uefi::runtime::reset(uefi::runtime::ResetType::SHUTDOWN, Status::ABORTED, None)
}


/// Slingshot's directory on its file system, e.g. `\\efi\\slingshot`.
fn own_dir() -> String {
    let own = bootvars::own_file_path();
    own.rfind('\\').map(|i| String::from(&own[..i])).unwrap_or_default()
}

/// Loads and starts the UEFI drivers in the `drivers` directory next to
/// Slingshot, like rEFInd does. This adds what the firmware lacks, such as a
/// USB mouse driver (OVMF, the firmware used with QEMU, has none). Returns
/// how many started.
fn load_drivers() -> usize {
    let Ok(fs) = uefi::boot::get_image_file_system(uefi::boot::image_handle()) else { return 0 };
    let mut fs = uefi::fs::FileSystem::new(fs);
    let dir = format!("{}\\drivers", own_dir());
    let Ok(dir_path) = uefi::CString16::try_from(dir.as_str()) else { return 0 };
    let Ok(entries) = fs.read_dir(dir_path.as_ref()) else { return 0 };
    let names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| !e.is_directory())
        .map(|e| e.file_name().to_string())
        .filter(|n| crate::parse::bls::ends_with_ignore_case(n, ".efi"))
        .collect();
    let mut started = 0;
    for name in names {
        let Ok(path) = uefi::CString16::try_from(format!("{dir}\\{name}").as_str()) else { continue };
        let Ok(data) = fs.read(path.as_ref()) else { continue };
        let source = uefi::boot::LoadImageSource::FromBuffer { buffer: &data, file_path: None };
        match uefi::boot::load_image(uefi::boot::image_handle(), source) {
            Ok(handle) => match uefi::boot::start_image(handle) {
                Ok(()) => {
                    log::info!("driver {name}: started");
                    started += 1;
                }
                Err(e) => log::warn!("driver {name}: cannot start: {:?}", e.status()),
            },
            Err(e) => log::warn!("driver {name}: cannot load: {:?}", e.status()),
        }
    }
    started
}

/// The extra drivers, then a "connect all": firmware often only connects
/// the devices it needs for its own console and its boot disk, so mice,
/// touch screens and the other disks get their drivers too. Done after the
/// first frame (`platform::Slingshot`): it can take seconds, and the menu
/// shows first. Returns whether drivers were started (they can make file
/// systems readable, and boot entries appear).
///
/// A slice at a time ([`Connecting::step`]): the event loop draws and reads
/// the keys in between, so the menu and its countdown don't freeze.
pub(super) struct Connecting {
    handles: Vec<uefi::Handle>,
    next: usize,
    /// Drivers were started.
    pub drivers: bool,
}

impl Connecting {
    /// Starts the drivers, then lists the handles to connect.
    pub(super) fn start() -> Self {
        let drivers = load_drivers() > 0;
        let handles = uefi::boot::locate_handle_buffer(uefi::boot::SearchType::AllHandles)
            .map(|h| h.iter().copied().collect())
            .unwrap_or_default();
        Connecting { handles, next: 0, drivers }
    }

    /// Connects handles until `enough` says so; whether all are done.
    pub(super) fn step(&mut self, enough: impl Fn() -> bool) -> bool {
        while let Some(handle) = self.handles.get(self.next) {
            let _ = uefi::boot::connect_controller(*handle, &[], None, true);
            self.next += 1;
            if enough() {
                break;
            }
        }
        self.next >= self.handles.len()
    }
}

/// The firmware's own boot timings (ACPI's FPDT), if it records them: how
/// long it took before starting Slingshot, as a log line.
fn log_firmware_times() {
    // SAFETY: The firmware's ACPI tables, in memory it keeps (identity
    // mapped); each read stays within the length the table gives.
    let table = |address: usize| unsafe {
        let length = core::ptr::read_unaligned((address + 4) as *const u32) as usize;
        core::slice::from_raw_parts(address as *const u8, length.max(8))
    };
    let rsdp = uefi::system::with_config_table(|t| {
        let find = |guid| t.iter().find(|e| e.guid == guid).map(|e| e.address as usize);
        find(uefi::table::cfg::ConfigTableEntry::ACPI2_GUID)
    });
    let Some(rsdp) = rsdp else { return };
    // SAFETY: The RSDP's XSDT address (ACPI 2.0 and later: offset 24).
    let xsdt = unsafe { core::ptr::read_unaligned((rsdp + 24) as *const u64) } as usize;
    if xsdt == 0 {
        return;
    }
    let xsdt = table(xsdt);
    let fpdt = xsdt.get(36..).unwrap_or_default().as_chunks::<8>().0.iter().map(|e| u64::from_le_bytes(*e) as usize).find(|a| {
        // SAFETY: An XSDT entry: a table's address.
        *a != 0 && unsafe { core::slice::from_raw_parts(*a as *const u8, 4) } == b"FPDT"
    });
    let Some(fpdt) = fpdt else {
        log::info!("firmware: no boot timings (no FPDT)");
        return;
    };
    let times = crate::parse::fpdt::fbpt_address(table(fpdt)).and_then(|a| crate::parse::fpdt::boot_times(table(a as usize)));
    match times {
        Some(t) => log::info!(
            "firmware: reset done at {} ms, Slingshot loaded at {} ms and started at {} ms (since the firmware's clock started)",
            t.reset_end / 1_000_000,
            t.loader_load / 1_000_000,
            t.loader_start / 1_000_000
        ),
        None => log::info!("firmware: no boot timings in its FPDT"),
    }
}

/// Why `path` couldn't be loaded (not Secure Boot).
fn not_loaded(path: &str) -> String {
    trf("The firmware couldn't load {}. It may not be an EFI program for this computer, or the file is damaged or missing.", &[&path])
}

/// Why Secure Boot refused a program.
fn refused() -> String {
    tr("Secure Boot is on, and this program isn't signed by a key this computer trusts. Sign it, or turn Secure Boot off in the firmware setup.")
}

/// What starting `path` came to, when it couldn't take over.
fn started(outcome: Option<chainload::Outcome>, path: &str) -> StartResult {
    match outcome {
        Some(chainload::Outcome::Returned(status)) => StartResult::Returned(chainload::describe_status(status)),
        Some(chainload::Outcome::Refused) => StartResult::Failed(refused()),
        _ => StartResult::Failed(not_loaded(path)),
    }
}

fn status_text(e: uefi::Error) -> String {
    format!("{:?}", e.status())
}

/// Joins the parts that aren't empty.
fn join_non_empty(parts: &[&str], separator: &str) -> String {
    parts.iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join(separator)
}

fn firmware_version() -> String {
    crate::parse::loader_interface::firmware_revision(uefi::system::firmware_revision())
}

/// The device path of the file system Slingshot was loaded from.
fn own_fs_device_path() -> Vec<u8> {
    use uefi::proto::loaded_image::LoadedImage;
    let Ok(image) = uefi::boot::open_protocol_exclusive::<LoadedImage>(uefi::boot::image_handle()) else {
        return Vec::new();
    };
    image
        .device()
        .and_then(crate::uefi_platform::open_shared::<uefi::proto::device_path::DevicePath>)
        .map(|dp| dp.as_bytes().to_vec())
        .unwrap_or_default()
}

struct UefiBackend {
    order_attributes: VariableAttributes,
    /// The device path of the file system with the Boot Loader Specification
    /// entries, to start them from.
    entries_fs: core::cell::RefCell<Vec<u8>>,
    /// The entries there with a boot counter: id and file.
    counted_entries: core::cell::RefCell<Vec<sdboot::Counted>>,
    /// The file systems `volumes()` listed: handle and device path.
    volumes: core::cell::RefCell<Vec<(uefi::Handle, Vec<u8>)>>,
    /// Read the first time the System screen asks.
    batteries: core::cell::OnceCell<battery::Batteries>,
    /// The disks there were at the last scan.
    media: core::cell::RefCell<Vec<usize>>,
    /// What `boot_data` read, for `rescan`.
    last: core::cell::RefCell<LastRead>,
    boot_once: sdboot::OncePerBoot,
    #[cfg(feature = "luks")]
    luks: Rc<luks::Luks>,
}

/// What `boot_data` read that a rescan uses again.
#[derive(Default)]
struct LastRead {
    current: Option<u16>,
    own_file: String,
    /// The `Boot####` of `BootOrder`.
    entries: Vec<(u16, Box<[u8]>)>,
    /// What systemd-boot's entries start (on `entries_fs`).
    loader_files: Vec<(Vec<u8>, String)>,
    /// What the last scan found, for `retranslate`.
    found: Vec<(FoundEntry, detect::Texts)>,
}

impl UefiBackend {
    /// Starts a kernel (or an EFI program) with `options`, from the file
    /// system `fs`; with the unlocked disks when it needs them.
    fn start_kernel(&self, fs: &[u8], program: &str, options: &str) -> Option<chainload::Outcome> {
        #[cfg(feature = "luks")]
        if let Some((options, _handoff)) = self.luks.handoff(fs, program, options) {
            return chainload::start_file(fs, program, &options);
        }
        chainload::start_file(fs, program, options)
    }
}

/// A file system's label.
fn volume_label(handle: uefi::Handle) -> Option<String> {
    volume_info(handle).map(|(label, _)| label)
}

/// The device paths of the disks and partitions there are.
fn block_device_paths() -> Vec<Vec<u8>> {
    use uefi::proto::media::block::BlockIO;
    uefi::boot::find_handles::<BlockIO>()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|h| crate::uefi_platform::open_shared::<uefi::proto::device_path::DevicePath>(h).map(|d| d.as_bytes().to_vec()))
        .collect()
}

/// The disks and partitions there are, to see when one comes or goes.
fn media_signature() -> Vec<usize> {
    use uefi::proto::media::block::BlockIO;
    let mut handles: Vec<usize> =
        uefi::boot::find_handles::<BlockIO>().unwrap_or_default().into_iter().map(|h| h.as_ptr() as usize).collect();
    handles.sort_unstable();
    handles
}

/// A file system's label and size.
fn volume_info(handle: uefi::Handle) -> Option<(String, u64)> {
    use uefi::proto::media::file::{File, FileSystemInfo};
    let mut fs = crate::uefi_platform::open_shared::<uefi::proto::media::fs::SimpleFileSystem>(handle)?;
    let mut root = fs.open_volume().ok()?;
    let info = root.get_boxed_info::<FileSystemInfo>().ok()?;
    Some((format!("{}", info.volume_label()), info.volume_size()))
}

impl Backend for UefiBackend {
    fn boot_data(&self) -> BootData {
        // Encrypted disks connected since the first look (as `rescan`).
        #[cfg(feature = "luks")]
        self.luks.refresh();
        let current = bootvars::boot_current();
        let (order, _) = bootvars::boot_order().unwrap_or_default();
        let own_file = bootvars::own_file_path();
        // Boot Loader Specification entries, with or without systemd-boot:
        // Slingshot starts them itself.
        let loader_entries = sdboot::read_config(&own_file, &self.boot_once).map(|(config, fs, counted)| {
            *self.entries_fs.borrow_mut() = fs;
            *self.counted_entries.borrow_mut() = counted;
            config
        });
        let fs = self.entries_fs.borrow().clone();
        let loader_files = loader_entries.iter().flat_map(|c| &c.entries).map(|e| (fs.clone(), crate::parse::load_option::normalize_path(&e.program))).collect();
        // Each Boot#### read once.
        *self.last.borrow_mut() = LastRead { current, own_file, entries: bootvars::read_entries(&order), loader_files, ..Default::default() };
        let BootData { options, found, .. } = self.rescan();
        log::info!(
            "BootCurrent={current:?} BootOrder={order:x?} own file={:?}, {} visible entries",
            self.last.borrow().own_file,
            options.len()
        );
        BootData {
            options,
            order,
            loader_entries,
            found,
            firmware_setup_supported: bootvars::firmware_setup_supported(),
            firmware_timeout: bootvars::firmware_timeout(),
            secure_boot: secureboot::enforced(),
        }
    }

    fn system_info(&self, _size: (usize, usize), _modes: &[(usize, usize)]) -> SystemInfo {
        let smbios = sysinfo::smbios();
        let vendor = String::from_utf16_lossy(uefi::system::firmware_vendor().to_u16_slice());
        let uefi_rev = uefi::system::uefi_revision();
        let uefi_text = format!("UEFI {}.{}", uefi_rev.major(), uefi_rev.minor());

        let cpu_title = if smbios.cpu_name.is_empty() { tr("Unknown processor") } else { smbios.cpu_name.clone() };
        let mut cpu_detail = Vec::new();
        if smbios.cpu_cores > 0 {
            cpu_detail.push(trf("{} cores", &[&smbios.cpu_cores]));
        }
        if smbios.cpu_threads > 0 {
            cpu_detail.push(trf("{} threads", &[&smbios.cpu_threads]));
        }
        if smbios.cpu_max_mhz > 0 {
            cpu_detail.push(trf("up to {} GHz", &[&format!("{:.1}", smbios.cpu_max_mhz as f32 / 1000.)]));
        }

        let memory = sysinfo::memory_usage();
        let dimm_total: u64 = smbios.dimms.iter().map(|d| d.0).sum();
        let mem_title = if dimm_total > 0 {
            let ty = smbios.dimms.first().map(|d| d.1).unwrap_or("RAM");
            format!("{} {}", format_bytes(dimm_total << 20), if ty == "RAM" { "" } else { ty })
        } else {
            memory
                .as_ref()
                .map(|u| format_bytes(u.total()))
                .unwrap_or_default()
        };
        let mem_detail = if smbios.dimms.len() > 1 && smbios.dimms.iter().all(|d| d.0 == smbios.dimms[0].0) {
            format!("{} × {}", smbios.dimms.len(), format_bytes(smbios.dimms[0].0 << 20))
        } else {
            if smbios.dimms.len() == 1 { tr("1 memory device") } else { trf("{} memory devices", &[&smbios.dimms.len()]) }
        };
        let mem_detail = match smbios.dimms.first().map(|d| d.2) {
            Some(speed) if speed > 0 && speed != 0xffff => format!("{mem_detail} · {speed} MT/s"),
            _ => mem_detail,
        };

        // The vendor, unless the product name starts with it ("QEMU Virtual Machine").
        let vendor_in_product = !smbios.system_vendor.is_empty()
            && smbios.system_product.to_lowercase().starts_with(&smbios.system_vendor.to_lowercase());
        let system_title = if vendor_in_product {
            smbios.system_product.clone()
        } else {
            join_non_empty(&[&smbios.system_vendor, &smbios.system_product], " ")
        };
        let system_detail =
            if smbios.board_product.is_empty() { String::new() } else { trf("Board {}", &[&smbios.board_product]) };

        let fw_title = format!("{} {}", vendor, firmware_version());
        let smbios_text = smbios.version.map(|(a, b)| format!("SMBIOS {a}.{b}")).unwrap_or_default();
        let fw_detail = join_non_empty(&[&uefi_text, &smbios_text, &smbios.bios_date], " · ");


        let secure_boot = sysinfo::secure_boot();
        let (sb_title, sb_detail, sb_icon, sb_good) = match secure_boot {
            sysinfo::SecureBoot::On => (tr("On"), tr("User mode · keys enrolled"), "shield-on", true),
            sysinfo::SecureBoot::Off => (tr("Off"), tr("Keys enrolled, not enforced"), "shield-off", false),
            sysinfo::SecureBoot::SetupMode => (tr("Off"), tr("Setup mode · no keys enrolled"), "shield-off", false),
            sysinfo::SecureBoot::Unsupported => (tr("Not supported"), tr("No Secure Boot variables"), "shield-off", false),
        };
        let sb_short = if matches!(secure_boot, sysinfo::SecureBoot::On) { tr("Secure Boot on") } else { tr("Secure Boot off") };

        SystemInfo {
            cards: alloc::vec![
                InfoCard::new(&tr("Processor"), cpu_title, cpu_detail.join(" · "), "cpu", false),
                InfoCard::new(&tr("Memory"), mem_title.trim().to_string(), mem_detail, "memory", false),
                InfoCard::new(
                    &tr("System"),
                    if system_title.is_empty() { tr("Unknown system") } else { system_title },
                    system_detail,
                    "computer",
                    false
                ),
                InfoCard::new(&tr("Firmware"), fw_title, fw_detail, "firmware", false),
                InfoCard::new(&tr("Secure Boot"), sb_title, sb_detail, sb_icon, sb_good),
            ],
            memory,
            firmware_line: trf("{} firmware {} · {} · {}", &[&vendor, &firmware_version(), &uefi_text, &sb_short]),
        }
    }

    fn power_off(&self, shut_down: bool) -> StartResult {
        use uefi::runtime::ResetType;
        log::info!("{}", if shut_down { "shutting down" } else { "restarting" });
        let kind = if shut_down { ResetType::SHUTDOWN } else { ResetType::COLD };
        uefi::runtime::reset(kind, Status::SUCCESS, None)
    }

    fn power(&self) -> crate::parse::battery::Power {
        self.batteries.get_or_init(|| battery::Batteries::new(sysinfo::smbios().batteries)).power()
    }

    fn date_time(&self) -> Option<crate::app::DateTime> {
        sysinfo::date_time()
    }

    fn set_date_time(&self, time: crate::app::DateTime) -> Result<(), String> {
        sysinfo::set_date_time(time)
    }

    fn load_settings(&self) -> Option<Saved> {
        nvram::load()
    }

    fn save_settings(&self, settings: &Saved) -> Result<(), String> {
        nvram::save(settings).map_err(status_text)
    }

    fn reset_settings(&self) -> Result<(), String> {
        nvram::reset().map_err(status_text)
    }

    fn set_boot_order(&self, order: &[u16]) -> Result<(), String> {
        bootvars::set_boot_order(order, self.order_attributes).map_err(status_text)
    }

    fn read_file(&self, name: &str) -> Option<Vec<u8>> {
        // Next to our own binary, on the same file system.
        let dir = own_dir();
        let path = uefi::CString16::try_from(format!("{dir}\\{name}").as_str()).ok()?;
        let fs = uefi::boot::get_image_file_system(uefi::boot::image_handle()).ok()?;
        uefi::fs::FileSystem::new(fs).read(path.as_ref()).ok()
    }

    fn volumes(&self) -> Vec<Volume> {
        let handles = uefi::boot::find_handles::<uefi::proto::media::fs::SimpleFileSystem>().unwrap_or_default();
        log::info!("file browser: {} file system(s)", handles.len());
        let own = own_fs_device_path();
        let mut found = Vec::new();
        let mut volumes = Vec::new();
        for handle in handles {
            let Some(dp) = crate::uefi_platform::open_shared::<uefi::proto::device_path::DevicePath>(handle).map(|d| d.as_bytes().to_vec())
            else {
                continue;
            };
            let (kind, what) = crate::parse::load_option::describe_volume(&dp);
            let (label, size) = volume_info(handle).unwrap_or_default();
            let mut detail = what.clone();
            if size > 0 {
                detail = format!("{detail} · {}", format_bytes(size));
            }
            if dp == own {
                detail = format!("{detail} · Slingshot is here");
            }
            #[cfg(feature = "luks")]
            if self.luks.is_mapped(&dp) {
                detail = format!("{detail} · {}", tr("unlocked"));
            }
            let label = label.trim();
            volumes.push(Volume { name: if label.is_empty() { what } else { label.into() }, detail, kind, format: "", unlock: None });
            found.push((handle, dp));
        }
        *self.volumes.borrow_mut() = found;
        volumes
    }

    fn unreadable_volumes(&self) -> Vec<Volume> {
        #[cfg(feature = "luks")]
        let mapped = |p: &dir::Unreadable| self.luks.is_mapped(&p.device_path);
        #[cfg(not(feature = "luks"))]
        let mapped = |_: &dir::Unreadable| false;
        let encrypted = |p: &dir::Unreadable| crate::parse::fstype::is_encrypted(p.kind);
        dir::unreadable_partitions()
            .into_iter()
            // An unlocked encrypted partition: its decrypted disk is listed.
            .filter(|p| !(mapped(p) && encrypted(p)))
            .map(|p| {
                let (kind, what) = crate::parse::load_option::describe_volume(&p.device_path);
                let detail = format!("{} · {}", p.kind, format_bytes(p.size));
                let detail = if mapped(&p) { format!("{detail} · {}", tr("unlocked")) } else { detail };
                #[cfg(feature = "luks")]
                let unlock = if encrypted(&p) && !mapped(&p) { self.luks.disk_at(&p.device_path) } else { None };
                #[cfg(not(feature = "luks"))]
                let unlock = None;
                Volume { name: what, detail, kind, format: p.kind, unlock }
            })
            .collect()
    }

    fn list_dir(&self, volume: usize, path: &str) -> Result<Vec<DirEntry>, String> {
        let handle = self.volumes.borrow().get(volume).map(|v| v.0).ok_or("no such disk")?;
        Ok(dir::read_dir(handle, path)?
            .into_iter()
            .map(|e| DirEntry { name: e.name, is_dir: e.is_dir, size: e.size })
            .collect())
    }

    fn add_boot_entry(&self, volume: usize, path: &str, title: &str) -> Result<u16, String> {
        let fs = self.volumes.borrow().get(volume).map(|v| v.1.clone()).ok_or("no such disk")?;
        bootvars::add_boot_option(title, &crate::parse::load_option::file_device_path(&fs, path))
    }

    fn read_volume_file(&self, volume: usize, path: &str) -> Result<Vec<u8>, String> {
        let handle = self.volumes.borrow().get(volume).map(|v| v.0).ok_or("no such disk")?;
        // Like `list_dir`: rEFInd's drivers too.
        dir::read_file(handle, path).ok_or_else(|| String::from("cannot read the file"))
    }

    fn list_pictures(&self) -> Vec<String> {
        let Ok(dir) = uefi::CString16::try_from(own_dir().as_str()) else { return Vec::new() };
        let Ok(fs) = uefi::boot::get_image_file_system(uefi::boot::image_handle()) else { return Vec::new() };
        let Ok(entries) = uefi::fs::FileSystem::new(fs).read_dir(dir.as_ref()) else { return Vec::new() };
        let files = entries.filter_map(Result::ok).filter(|i| !i.is_directory());
        crate::app::picture_names(files.map(|i| format!("{}", i.file_name())))
    }

    fn save_log(&self) -> Result<String, String> {
        // What the machine is first, for a bug report.
        let smbios = sysinfo::smbios();
        let vendor = String::from_utf16_lossy(uefi::system::firmware_vendor().to_u16_slice());
        let mut log = format!(
            "Slingshot {}\nMachine: {} {} (board {})\nFirmware: {vendor} {} ({}), UEFI {}\n\n",
            env!("CARGO_PKG_VERSION"),
            smbios.system_vendor,
            smbios.system_product,
            smbios.board_product,
            firmware_version(),
            smbios.bios_date,
            uefi::system::uefi_revision(),
        )
        .into_bytes();
        log.extend_from_slice(&debuglog::contents());
        self.write_file("slingshot.log", &log)?;
        Ok(format!("{}\\slingshot.log", own_dir()))
    }

    fn write_file(&self, name: &str, data: &[u8]) -> Result<(), String> {
        let dir = own_dir();
        let path = uefi::CString16::try_from(format!("{dir}\\{name}").as_str()).map_err(|e| format!("{e:?}"))?;
        let fs = uefi::boot::get_image_file_system(uefi::boot::image_handle()).map_err(status_text)?;
        uefi::fs::FileSystem::new(fs).write(path.as_ref(), data).map_err(|e| format!("{e}"))
    }

    #[cfg(feature = "luks")]
    fn locked_volumes(&self, target: &Target) -> Vec<crate::app::LockedVolume> {
        // The kernel and its file system, when Slingshot starts it itself.
        let (fs, program, options) = match target {
            Target::LoaderEntry { program, options, .. } if !program.is_empty() => {
                (self.entries_fs.borrow().clone(), program, options)
            }
            Target::Program { path, options } => (own_fs_device_path(), path, options),
            Target::Found { fs, program, options } => (fs.clone(), program, options),
            _ => return Vec::new(),
        };
        self.luks.locked_volumes(&fs, program, options)
    }

    #[cfg(feature = "luks")]
    fn forget_unlocked(&self) {
        self.luks.cancel();
        self.luks.forget();
    }

    #[cfg(feature = "luks")]
    fn unlock(&self, volume: usize, passphrase: crate::app::Secret, done: crate::app::UnlockDone) {
        self.luks.unlock(volume, passphrase, done);
    }

    fn hibernated(&self) -> Option<crate::parse::hibernate::Hibernated> {
        nvram::hibernated()
    }

    fn last_started(&self) -> Option<String> {
        nvram::last_entry()
    }

    fn set_last_started(&self, key: &str, loader_entry: Option<&str>) {
        nvram::set_last_entry(key);
        if let Some(id) = loader_entry {
            sdboot::set_last_booted(id);
        }
    }

    #[cfg(feature = "luks")]
    fn open_unlocked(&self, id: usize) -> Result<(), String> {
        self.luks.open(id)
    }

    fn language(&self) -> Option<String> {
        // PlatformLang: an RFC 4646 tag ("fr-FR"), NUL-terminated ASCII.
        let (data, _) = uefi::runtime::get_variable_boxed(uefi::cstr16!("PlatformLang"), &uefi::runtime::VariableVendor::GLOBAL_VARIABLE).ok()?;
        let tag: String = data.iter().take_while(|b| **b != 0).map(|b| *b as char).collect();
        (!tag.is_empty()).then_some(tag)
    }

    fn start(&self, target: &Target) -> StartResult {
        // Nothing starts while other processors still run a (cancelled)
        // passphrase check: what starts may take their memory.
        #[cfg(feature = "luks")]
        {
            if !luks::wait_for_processors() {
                return StartResult::Failed(tr("A passphrase check is still running. Try again in a moment."));
            }
            // Only a kernel started here gets the unlocked disks' keys (it
            // takes them, `handoff`): anything else starts without them,
            // and without the decrypted disks unless it's a file, which
            // may be on one.
            let kernel = match target {
                Target::Found { .. } | Target::Program { .. } => true,
                Target::LoaderEntry { program, .. } => !program.is_empty(),
                Target::Firmware(_) | Target::File { .. } => false,
            };
            if !kernel {
                self.luks.forget_all(!matches!(target, Target::File { .. }));
            }
        }
        loader_interface::starting(match target {
            Target::LoaderEntry { id, .. } => Some(id.as_str()),
            _ => None,
        });
        let boot = match target {
            Target::Firmware(n) => *n,
            Target::File { volume, path } => {
                let fs = self.volumes.borrow().get(*volume).map(|v| v.1.clone()).unwrap_or_default();
                return started(chainload::start_file(&fs, path, ""), path);
            }
            Target::Found { fs, program, options } => return started(self.start_kernel(fs, program, options), program),
            Target::Program { path, options } => {
                return started(self.start_kernel(&own_fs_device_path(), path, options), path);
            }
            Target::LoaderEntry { systemd_boot, id, program, options } => {
                // The kernel (or EFI program) directly, like systemd-boot does.
                if !program.is_empty() {
                    let fs = self.entries_fs.borrow().clone();
                    // Boot counting: a try counts before the start, as with
                    // systemd-boot. A unified kernel image is itself the
                    // counted file, so it's started under its new name.
                    let mut program = program.clone();
                    let counted = self.counted_entries.borrow().iter().find(|(i, _)| i == id).map(|(_, p)| p.clone());
                    if let Some(path) = counted
                        && let Some(next) = sdboot::count_try(&fs, &path)
                        && program.eq_ignore_ascii_case(&path)
                    {
                        program = next;
                    }
                    match self.start_kernel(&fs, &program, options) {
                        Some(chainload::Outcome::Returned(status)) => {
                            return StartResult::Returned(chainload::describe_status(status));
                        }
                        // systemd-boot would be refused the same kernel.
                        Some(chainload::Outcome::Refused) => return StartResult::Failed(refused()),
                        _ => log::info!("{id}: cannot load {program} directly"),
                    }
                }
                // Else through systemd-boot, which boots it without its menu.
                let Some(boot) = systemd_boot else {
                    return StartResult::Failed(not_loaded(program));
                };
                if let Err(e) = sdboot::select_one_shot(id) {
                    return StartResult::Failed(status_text(e));
                }
                *boot
            }
        };
        // Start it directly; if it can't be loaded here, let the firmware
        // start it after a reset.
        match chainload::start_boot_option(boot) {
            chainload::Outcome::Returned(status) => StartResult::Returned(chainload::describe_status(status)),
            chainload::Outcome::Refused => StartResult::Failed(refused()),
            chainload::Outcome::NotLoaded => {
                log::info!("Boot{boot:04X}: not loadable here, BootNext and reset");
                // Only returns on failure: the machine resets otherwise.
                StartResult::Failed(bootvars::boot_next_and_reset(boot).err().map(status_text).unwrap_or_default())
            }
        }
    }

    /// From what `boot_data` read: only the entries' descriptions and what the
    /// disks can start that they don't are made again.
    fn rescan(&self) -> BootData {
        #[cfg(feature = "luks")]
        self.luks.refresh();
        let mut last = self.last.borrow_mut();
        let entries = bootvars::parse_entries(&last.entries);
        let own_fs = own_fs_device_path();
        let options = bootvars::visible_boot_options(&entries, last.current, &last.own_file, &own_fs, &block_device_paths());
        *self.media.borrow_mut() = media_signature();
        // What else the disks can start: not what the entries start already,
        // nor Slingshot itself.
        let mut started = bootvars::option_files(&entries);
        started.push((own_fs, last.own_file.clone()));
        started.extend(last.loader_files.iter().cloned());
        // The whole devices the visible entries start (a USB disk listed at
        // startup and still there).
        let shown: Vec<u16> = options.iter().filter(|o| !o.absent).map(|o| o.number).collect();
        last.found = detect::scan(&started, &bootvars::option_devices(&entries, &shown), &self.entries_fs.borrow());
        BootData { options, found: last.found.iter().map(|(f, _)| f.clone()).collect(), ..Default::default() }
    }

    fn retranslate(&self) -> BootData {
        let mut last = self.last.borrow_mut();
        let entries = bootvars::parse_entries(&last.entries);
        let options = bootvars::visible_boot_options(&entries, last.current, &last.own_file, &own_fs_device_path(), &block_device_paths());
        for (entry, texts) in &mut last.found {
            texts.apply(entry);
        }
        BootData { options, found: last.found.iter().map(|(f, _)| f.clone()).collect(), ..Default::default() }
    }

    fn media_changed(&self) -> bool {
        *self.media.borrow() != media_signature()
    }

    fn reboot_to_firmware_setup(&self) -> StartResult {
        StartResult::Failed(bootvars::reboot_to_firmware_setup().err().map(status_text).unwrap_or_default())
    }
}

pub fn main() -> Status {
    // Before anything else takes some time.
    let started = crate::uefi_platform::timer_tick();
    // The firmware resets the machine after 5 minutes unless the watchdog
    // is disabled.
    let _ = uefi::boot::set_watchdog_timer(0, 0x10000, None);
    debuglog::init();
    // Which build: the x86 one without SSE draws about twice as slowly.
    let build = if cfg!(target_arch = "aarch64") {
        "ARM"
    } else if cfg!(target_feature = "sse2") {
        "x86, SSE2"
    } else {
        "x86, software floating point (slower)"
    };
    log::info!("Slingshot {} starting ({build})", env!("CARGO_PKG_VERSION"));
    log_firmware_times();
    loader_interface::announce(started, &own_fs_device_path(), &bootvars::own_file_path());

    // The drivers and the other devices: after the first frame
    // (`Connecting`, a slice at a time: `platform::Slingshot::first_frame`).

    let hub = Rc::new(crate::app::Hub::default());
    let platform = crate::uefi_platform::UefiPlatform::new(platform::Slingshot::new(hub.clone()));
    slint::platform::set_platform(Box::new(platform))
        .expect("platform already set");

    let order_attributes = bootvars::boot_order().map(|(_, a)| a).unwrap_or(bootvars::NV_BS_RT);
    let backend = Rc::new(UefiBackend {
        entries_fs: Default::default(),
        counted_entries: Default::default(),
        volumes: Default::default(),
        batteries: Default::default(),
        media: Default::default(),
        last: Default::default(),
        boot_once: Default::default(),
        order_attributes,
        #[cfg(feature = "luks")]
        luks: Default::default(),
    });

    // Without a display, let the firmware try the next boot entry.
    match crate::app::run(backend, hub) {
        Ok(()) => Status::SUCCESS,
        Err(e) => {
            log::error!("event loop failed: {e}");
            Status::UNSUPPORTED
        }
    }
}
