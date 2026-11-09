// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! This computer's boot configuration, read on Linux: the firmware's boot
//! entries from efivarfs, and systemd-boot's entries from the EFI System
//! Partition, for the preview and `slingshot config`; and what `slingshot
//! config` writes (files on the ESP, the boot order).

use std::path::{Path, PathBuf};

use crate::app::{BootData, LoaderConfig, LoaderEntry};
use crate::parse::{bls, load_option};

const EFIVARS: &str = "/sys/firmware/efi/efivars";
const GLOBAL: &str = "8be4df61-93ca-11d2-aa0d-00e098032b8c";
const LOADER: &str = "4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";
/// Where the setup script installs Slingshot, on the ESP.
pub const SLINGSHOT_DIR: &str = "EFI/slingshot";

pub struct Machine {
    pub boot: BootData,
    /// The EFI System Partition's mount point.
    pub esp: Option<PathBuf>,
    /// What couldn't be read, to tell the user.
    pub notes: Vec<String>,
}

/// A firmware variable's data (efivarfs files start with 4 bytes of
/// attributes).
fn var(name: &str, guid: &str) -> Option<Vec<u8>> {
    let data = std::fs::read(format!("{EFIVARS}/{name}-{guid}")).ok()?;
    Some(data.get(4..)?.to_vec())
}

fn var_u16(name: &str) -> Option<u16> {
    crate::parse::le_u16s(&var(name, GLOBAL)?).next()
}

fn var_string(name: &str, guid: &str) -> Option<String> {
    Some(crate::parse::ucs2_string(&var(name, guid)?))
}

/// Where the EFI System Partition is mounted.
pub fn find_esp() -> Option<PathBuf> {
    if let Ok(out) = std::process::Command::new("bootctl").arg("--print-esp-path").output()
        && out.status.success()
    {
        let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        if path.is_dir() {
            return Some(path);
        }
    }
    ["/efi", "/boot", "/boot/efi"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.join("EFI").is_dir() || p.join("efi").is_dir())
}

/// A file name from a boot entry ("/vmlinuz-linux", "\EFI\x.efi") on the ESP.
fn esp_path(esp: &Path, name: &str) -> PathBuf {
    esp.join(name.replace('\\', "/").trim_start_matches('/'))
}

fn list(dir: &Path, suffix: &str, notes: &mut Vec<String>) -> Vec<String> {
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            let mut names: Vec<String> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| bls::ends_with_ignore_case(n, suffix))
                .collect();
            names.sort();
            names
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            notes.push(format!("{} is only readable by root: run with sudo to see systemd-boot's entries", dir.display()));
            Vec::new()
        }
        Err(_) => Vec::new(),
    }
}

fn systemd_boot(esp: &Path, own_file: &str, notes: &mut Vec<String>) -> Option<LoaderConfig> {
    let names = list(&esp.join("loader/entries"), ".conf", notes);
    let ukis = list(&esp.join("EFI/Linux"), ".efi", notes);
    let confs: Vec<(String, String)> = names
        .into_iter()
        .filter_map(|n| Some((n.clone(), std::fs::read_to_string(esp.join("loader/entries").join(&n)).ok()?)))
        .collect();
    let loader_conf = std::fs::read_to_string(esp.join("loader/loader.conf")).ok();
    let default_var = var_string("LoaderEntryDefault", LOADER);
    let (entries, default) = bls::menu(
        &confs,
        &ukis,
        &mut |f| esp_path(esp, f).exists(),
        own_file,
        default_var.as_deref(),
        loader_conf.as_deref(),
    );
    let entries: Vec<LoaderEntry> = entries
        .into_iter()
        .map(LoaderEntry::from)
        .collect();
    (!entries.is_empty()).then_some(LoaderConfig {
        entries,
        default,
        editor_off: !loader_conf.as_deref().is_none_or(bls::loader_conf_editor),
        ..Default::default()
    })
}

/// Reads this computer's boot configuration. `None` when it isn't a Linux
/// system booted with UEFI.
pub fn read(esp: Option<PathBuf>) -> Option<Machine> {
    let order_data = var("BootOrder", GLOBAL)?;
    let order: Vec<u16> = crate::parse::le_u16s(&order_data).collect();
    let own_file = load_option::normalize_path(&format!("\\{SLINGSHOT_DIR}\\slingshot.efi"));
    let options = order
        .iter()
        .filter_map(|n| load_option::parse_load_option(*n, &var(&format!("Boot{n:04X}"), GLOBAL)?))
        .filter(|d| d.file != own_file)
        .map(|d| d.option)
        .collect::<Vec<_>>();
    let mut notes = Vec::new();
    let esp = esp.or_else(find_esp);
    // Like the firmware: the Boot Loader Specification entries, with or
    // without systemd-boot.
    let systemd_boot = esp.as_deref().and_then(|esp| systemd_boot(esp, &own_file, &mut notes));
    if esp.is_none() {
        notes.push(String::from("The EFI System Partition wasn't found (use --esp DIR)"));
    }
    let firmware_setup_supported =
        var("OsIndicationsSupported", GLOBAL).and_then(|d| d.first().copied()).is_some_and(|b| b & 1 != 0);
    Some(Machine {
        boot: BootData {
            options,
            order,
            loader_entries: systemd_boot,
            // The firmware scans the disks; the desktop shows its entries.
            found: Vec::new(),
            firmware_setup_supported,
            firmware_timeout: var_u16("Timeout"),
            secure_boot: var("SecureBoot", GLOBAL).and_then(|d| d.first().copied()) == Some(1),
        },
        esp,
        notes,
    })
}

pub fn is_root() -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0)
}

/// A program's full path (pkexec wants one).
pub fn find_in_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(program)).find(|p| p.is_file())
}

/// Writes `data` to `path` (making its folder), with pkexec if only root may.
pub fn write_file(path: &Path, data: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let direct = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(path, data));
    match direct {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && !is_root() => {
            let mut child = std::process::Command::new("pkexec")
                .args(["sh", "-c", "mkdir -p \"$(dirname \"$1\")\" && cat > \"$1\"", "sh"])
                .arg(path)
                .stdin(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| format!("{} is only writable by root, and pkexec failed: {e}", path.display()))?;
            child.stdin.take().map(|mut s| s.write_all(data)).transpose().map_err(|e| e.to_string())?;
            let status = child.wait().map_err(|e| e.to_string())?;
            if status.success() { Ok(()) } else { Err(String::from("not saved (no permission)")) }
        }
        Err(e) => Err(format!("cannot write {}: {e}", path.display())),
    }
}

/// Sets the firmware's BootOrder, with efibootmgr (through pkexec unless
/// root).
pub fn set_boot_order(order: &[u16]) -> Result<(), String> {
    let efibootmgr = find_in_path("efibootmgr").ok_or("efibootmgr is needed to change the boot order")?;
    let list: Vec<String> = order.iter().map(|n| format!("{n:04X}")).collect();
    let mut command = if is_root() {
        std::process::Command::new(efibootmgr)
    } else {
        let mut c = std::process::Command::new("pkexec");
        c.arg(efibootmgr);
        c
    };
    let out = command.args(["--quiet", "--bootorder", &list.join(",")]).output().map_err(|e| format!("cannot run efibootmgr: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { String::from("not saved (no permission)") } else { err })
    }
}

/// The batteries and the AC adapter, from /sys/class/power_supply. Devices'
/// batteries (a mouse's, a headset's) are left out.
pub fn power() -> crate::parse::battery::Power {
    let mut power = crate::parse::battery::Power::default();
    let Ok(dir) = std::fs::read_dir("/sys/class/power_supply") else { return power };
    let mut supplies: Vec<PathBuf> = dir.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    supplies.sort();
    for supply in supplies {
        let file = |name: &str| std::fs::read_to_string(supply.join(name)).ok();
        let ty = file("type").unwrap_or_default();
        if file("scope").is_some_and(|s| s.trim() == "Device") {
            continue;
        }
        match ty.trim() {
            "Battery" if file("present").is_none_or(|p| p.trim() == "1") => {
                power.batteries.push(crate::parse::battery::from_sysfs(file));
            }
            "Mains" | "USB" => {
                let online = file("online").is_some_and(|o| o.trim() == "1");
                power.ac_online = Some(power.ac_online.unwrap_or(false) || online);
            }
            _ => {}
        }
    }
    power
}
