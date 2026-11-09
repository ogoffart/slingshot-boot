// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Parsing of UEFI load options (the data of `Boot####` variables) and their
//! device paths, and turning them into what the boot picker shows.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::app::BootOption;

const LOAD_OPTION_ACTIVE: u32 = 0x1;
const LOAD_OPTION_HIDDEN: u32 = 0x8;

/// A `Boot####` entry with the loader's file path in its device path
/// (lowercase, "" if none).
#[derive(Debug)]
pub struct Described {
    pub option: BootOption,
    pub file: String,
}

/// A UCS-2 string up to its NUL, and how many bytes it took (the NUL
/// included, when there is one).
pub fn ucs2_until_nul(data: &[u8]) -> (String, usize) {
    let units: Vec<u16> = super::le_u16s(data).take_while(|c| *c != 0).collect();
    let used = (2 * units.len() + 2).min(data.len() & !1);
    (String::from_utf16_lossy(&units), used)
}

/// What the device path of a load option says about where it boots from.
#[derive(Default, Debug)]
pub struct PathInfo {
    pub usb: bool,
    pub nvme: bool,
    pub sata: bool,
    pub ata: bool,
    pub scsi: bool,
    pub mac: bool,
    pub ipv4: bool,
    pub ipv6: bool,
    pub uri: bool,
    pub cdrom: bool,
    pub firmware_volume: bool,
    pub partition: Option<u32>,
    pub file: String,
}

pub fn parse_device_path(mut dp: &[u8]) -> PathInfo {
    let mut info = PathInfo::default();
    while dp.len() >= 4 {
        let (ty, sub) = (dp[0], dp[1]);
        let len = u16::from_le_bytes([dp[2], dp[3]]) as usize;
        if len < 4 || len > dp.len() {
            break;
        }
        let node = &dp[..len];
        match (ty, sub) {
            (0x7f, 0xff) => break,
            (0x03, 0x05) | (0x03, 0x0f) | (0x03, 0x10) => info.usb = true,
            (0x03, 0x17) => info.nvme = true,
            (0x03, 0x12) => info.sata = true,
            (0x03, 0x01) => info.ata = true,
            (0x03, 0x02) => info.scsi = true,
            (0x03, 0x0b) => info.mac = true,
            (0x03, 0x0c) => info.ipv4 = true,
            (0x03, 0x0d) => info.ipv6 = true,
            (0x03, 0x18) => info.uri = true,
            (0x04, 0x01) if len >= 8 => {
                info.partition = Some(u32::from_le_bytes([node[4], node[5], node[6], node[7]]))
            }
            (0x04, 0x02) => info.cdrom = true,
            (0x04, 0x04) => info.file = ucs2_until_nul(&node[4..]).0,
            (0x04, 0x06) | (0x04, 0x07) => info.firmware_volume = true,
            _ => {}
        }
        dp = &dp[len..];
    }
    info
}

/// The part of a partition's device path that names its disk: the nodes
/// before the hard drive (or CD-ROM) node. Empty when there is none.
// The firmware's (systemd-boot entries, LUKS): the desktop program has none.
#[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
pub fn disk_part(dp: &[u8]) -> &[u8] {
    let mut at = 0;
    while dp.len() >= at + 4 {
        let (ty, sub) = (dp[at], dp[at + 1]);
        let len = u16::from_le_bytes([dp[at + 2], dp[at + 3]]) as usize;
        if len < 4 || at + len > dp.len() || ty == 0x7f {
            break;
        }
        if ty == 0x04 && matches!(sub, 0x01 | 0x02) {
            return &dp[..at];
        }
        at += len;
    }
    &[]
}

/// Firmware-generated names carry noise, e.g. "QEMU QEMU USB HARDDRIVE
/// 1-0000:00:02.0-1" or "QEMU NVMe Ctrl SLING01 1": drop location tokens,
/// a trailing namespace number and repeated words.
fn clean_title(title: &str) -> String {
    let mut words: Vec<&str> = Vec::new();
    for w in title.split_whitespace() {
        if w.contains(':') || words.last() == Some(&w) {
            continue;
        }
        words.push(w);
    }
    while words.len() > 1 && words.last().is_some_and(|w| w.chars().all(|c| c.is_ascii_digit())) {
        words.pop();
    }
    let mut result = String::new();
    for w in words {
        if !result.is_empty() {
            result.push(' ');
        }
        // "HARDDRIVE" → "Harddrive": all-caps words longer than 4 letters.
        if w.len() > 4 && w.chars().all(|c| c.is_ascii_uppercase()) {
            let mut chars = w.chars();
            result.extend(chars.next());
            result.extend(chars.flat_map(|c| c.to_lowercase()));
        } else {
            result.push_str(w);
        }
    }
    result
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

/// Parses the data of `Boot{number:04X}`. Returns `None` for malformed,
/// inactive or hidden entries.
pub fn parse_load_option(number: u16, data: &[u8]) -> Option<Described> {
    if data.len() < 6 {
        return None;
    }
    let attributes = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let (description, desc_len) = ucs2_until_nul(&data[6..]);
    log::info!("Boot{number:04X}: attributes={attributes:#x} {description:?}");
    if attributes & LOAD_OPTION_ACTIVE == 0 || attributes & LOAD_OPTION_HIDDEN != 0 {
        return None;
    }
    let path_len = u16::from_le_bytes([data[4], data[5]]) as usize;
    let path_start = 6 + desc_len;
    let path = data.get(path_start..(path_start + path_len).min(data.len())).unwrap_or(&[]);
    let info = parse_device_path(path);
    Some(describe(number, &description, &info))
}

// Chainloading (firmware only); built on the desktop for the tests.
#[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
/// The device path and optional data of a load option (the data of a
/// `Boot####` variable).
pub fn load_option_parts(data: &[u8]) -> Option<(&[u8], &[u8])> {
    let path_len = u16::from_le_bytes([*data.get(4)?, *data.get(5)?]) as usize;
    let (_, desc_len) = ucs2_until_nul(data.get(6..)?);
    let path_start = 6 + desc_len;
    let path = data.get(path_start..path_start + path_len)?;
    Some((path, &data[path_start + path_len..]))
}

// Chainloading (firmware only); built on the desktop for the tests.
#[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
/// The nodes of a device path as (type, subtype, node bytes), up to the end
/// node.
pub(crate) fn nodes(mut dp: &[u8]) -> Vec<(u8, u8, &[u8])> {
    let mut result = Vec::new();
    while dp.len() >= 4 {
        let len = u16::from_le_bytes([dp[2], dp[3]]) as usize;
        if len < 4 || len > dp.len() || (dp[0], dp[1]) == (0x7f, 0xff) {
            break;
        }
        result.push((dp[0], dp[1], &dp[..len]));
        dp = &dp[len..];
    }
    result
}

// Chainloading (firmware only); built on the desktop for the tests.
#[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
/// The full device paths to try for a load option's device path, as the
/// firmware's boot manager would expand it. `file_systems` are the device
/// paths of the file systems (SimpleFileSystem handles).
///
/// - A full path (from the root of the device tree) is used as it is.
/// - A short path that starts with a hard drive node `HD(…)/\EFI\…`, as
///   efibootmgr, bootctl and Windows write them, goes on the file system
///   with the same partition.
/// - A short path that starts with a file path is tried on every file
///   system.
/// - Other short forms (USB class or WWID, vendor paths) give nothing: only
///   the firmware knows what they mean.
pub fn expand_device_path(path: &[u8], file_systems: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let short = nodes(path);
    let Some(&(ty, sub, first)) = short.first() else { return Vec::new() };
    let without_end = |p: &[u8]| -> Vec<u8> { nodes(p).iter().flat_map(|n| n.2.iter().copied()).collect() };
    let rest = &path[first.len()..];
    match (ty, sub) {
        // Hard drive: match the partition (number, start, size and signature).
        (0x04, 0x01) => file_systems
            .iter()
            .filter(|fs| nodes(fs).iter().any(|n| (n.0, n.1) == (0x04, 0x01) && n.2 == first))
            .map(|fs| [without_end(fs), rest.to_vec()].concat())
            .collect(),
        (0x04, 0x04) => file_systems.iter().map(|fs| [without_end(fs), path.to_vec()].concat()).collect(),
        // USB WWID, USB class, vendor-defined messaging and media nodes.
        (0x03, 0x0f) | (0x03, 0x10) | (0x03, 0x0a) | (0x04, 0x03) => Vec::new(),
        // A whole device, as the firmware creates for disks and removable
        // media: as is (network boot loads through LoadFile), then the
        // removable-media default file on each of its file systems.
        _ if !short.iter().any(|n| (n.0, n.1) == (0x04, 0x04)) => {
            let device = without_end(path);
            let mut paths = alloc::vec![path.to_vec()];
            for fs in file_systems.iter().filter(|fs| fs.starts_with(&device)) {
                paths.push([without_end(fs), default_file_node()].concat());
            }
            paths
        }
        _ => alloc::vec![path.to_vec()],
    }
}

// Chainloading (firmware only); built on the desktop for the tests.
#[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
/// `\EFI\BOOT\BOOTX64.EFI` (or BOOTAA64.EFI) and an end node.
fn default_file_node() -> Vec<u8> {
    file_device_path(&[], &super::default_boot_file())
}

pub fn describe(number: u16, description: &str, info: &PathInfo) -> Described {
    let lower = description.to_lowercase();
    let file = normalize_path(&info.file);
    let network = info.uri || info.mac || info.ipv4 || info.ipv6;

    let kind = if lower.contains("shell") {
        "shell"
    } else if lower.contains("windows") || file.contains("\\microsoft\\") {
        "windows"
    } else if contains_any(
        &lower,
        &["linux", "ubuntu", "debian", "fedora", "arch", "suse", "mint", "grub", "systemd", "pop!_os", "elementary", "manjaro", "centos", "red hat"],
    ) || contains_any(&file, &["\\ubuntu\\", "\\debian\\", "\\fedora\\", "\\grub", "\\systemd\\", "\\linux"])
    {
        "linux"
    } else if network {
        "network"
    } else if info.usb {
        "usb"
    } else if info.cdrom || contains_any(&lower, &["dvd", "cd-rom", "cdrom"]) {
        "disc"
    } else if info.firmware_volume {
        "app"
    } else {
        "disk"
    };

    // Firmware descriptions often look like "UEFI PXEv4 (MAC:525400123456)".
    let mut title = description.trim();
    if let Some(rest) = title.strip_prefix("UEFI ") {
        title = rest.trim();
    }
    if let Some(pos) = title.find(" (MAC:") {
        title = title[..pos].trim();
    }
    let mut title = clean_title(title);
    if title.is_empty() {
        title = format!("Boot{number:04X}");
    }

    use crate::tr::{tr, trf};
    let bus = if info.nvme {
        tr("NVMe disk")
    } else if info.sata {
        tr("SATA disk")
    } else if info.ata {
        tr("ATA disk")
    } else if info.scsi {
        tr("SCSI disk")
    } else if info.usb {
        String::from("USB")
    } else {
        tr("Disk")
    };

    let subtitle = if kind == "network" {
        let proto = if info.uri || lower.contains("http") { "HTTP" } else { "PXE" };
        let ip = if info.ipv6 || lower.contains("v6") { "IPv6" } else { "IPv4" };
        title = trf("{} boot", &[&proto]);
        trf("Network · {}", &[&ip])
    } else if info.usb {
        tr("USB · removable")
    } else if kind == "disc" {
        String::from("CD / DVD")
    } else if info.firmware_volume && info.partition.is_none() {
        tr("Built into firmware")
    } else if let Some(p) = info.partition {
        trf("{} · partition {}", &[&bus, &p])
    } else {
        bus
    };

    let is_systemd_boot =
        file.ends_with(&format!("\\systemd\\systemd-boot{}.efi", super::EFI_ARCH)) || title == "Linux Boot Manager";
    let whole_device = info.partition.is_none() && info.file.is_empty();
    Described { option: BootOption { number, title, subtitle, kind, is_systemd_boot, whole_device, absent: false }, file }
}

/// The device path of `file` (e.g. `\EFI\BOOT\BOOTX64.EFI`) on the file
/// system at `fs_device_path`: its end node replaced by a file path node.
// Chainloading and new boot entries (firmware only); built on the desktop
// for the tests.
#[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
pub fn file_device_path(fs_device_path: &[u8], file: &str) -> Vec<u8> {
    let mut path: Vec<u8> = fs_device_path.to_vec();
    if path.ends_with(&END_NODE) {
        path.truncate(path.len() - 4);
    }
    let name = super::ucs2_bytes(file);
    path.extend_from_slice(&[0x04, 0x04]);
    path.extend_from_slice(&((4 + name.len()) as u16).to_le_bytes());
    path.extend_from_slice(&name);
    path.extend_from_slice(&END_NODE);
    path
}

const END_NODE: [u8; 4] = [0x7f, 0xff, 4, 0];

/// Whether a load option's device path (`entry`) is on the file system at
/// `fs`: the same partition (HD node), or the same device for a full path.
/// A path with only a file can't tell: true.
#[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
pub fn is_on_file_system(entry: &[u8], fs: &[u8]) -> bool {
    let entry = nodes(entry);
    let device: Vec<&(u8, u8, &[u8])> = entry.iter().take_while(|n| (n.0, n.1) != (0x04, 0x04)).collect();
    let fs = nodes(fs);
    match device.first() {
        None => true,
        Some(&&(0x04, 0x01, hd)) => fs.iter().any(|n| (n.0, n.1) == (0x04, 0x01) && n.2 == hd),
        Some(_) => device.len() == fs.len() && device.iter().zip(&fs).all(|(a, b)| a.2 == b.2),
    }
}

/// Whether a whole-device load option (no file, as firmware creates for
/// removable disks) is the device `path` (a file system's or a disk's) is
/// on: its nodes start `path`'s.
#[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
pub fn is_on_device(option: &[u8], path: &[u8]) -> bool {
    let device = nodes(option);
    let path = nodes(path);
    !device.is_empty() && device.len() <= path.len() && device.iter().zip(&path).all(|(a, b)| a.2 == b.2)
}

/// A whole USB device (no file): a removable disk the firmware listed at
/// startup, which can go away.
#[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
pub fn is_usb_device(option: &[u8]) -> bool {
    let n = nodes(option);
    n.iter().any(|n| (n.0, n.1) == (0x03, 0x05)) && !n.iter().any(|n| (n.0, n.1) == (0x04, 0x04))
}

/// The data of a new, active `Boot####` variable that starts `device_path`.
#[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
pub fn new_load_option(description: &str, device_path: &[u8]) -> Vec<u8> {
    let mut data = LOAD_OPTION_ACTIVE.to_le_bytes().to_vec();
    data.extend_from_slice(&(device_path.len() as u16).to_le_bytes());
    data.extend(super::ucs2_bytes(description));
    data.extend_from_slice(device_path);
    data
}

/// What a file system's device path says it is, for the file browser: an
/// icon, and a line such as "NVMe disk · partition 1".
#[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
pub fn describe_volume(device_path: &[u8]) -> (&'static str, String) {
    let info = parse_device_path(device_path);
    let d = describe(0, "", &info);
    (if d.option.kind == "disc" || d.option.kind == "usb" { d.option.kind } else { "disk" }, d.option.subtitle)
}

pub fn normalize_path(path: &str) -> String {
    path.replace('/', "\\").to_lowercase()
}

#[cfg(test)]
mod tests {
    #[test]
    fn disk_of_a_partition() {
        let nvme = [0x03, 0x17, 0x10, 0x00, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let partition = |n: u8| {
            let mut dp = nvme.to_vec();
            let mut hd = alloc::vec![0x04, 0x01, 42, 0, n, 0, 0, 0];
            hd.resize(42, 0);
            dp.extend(hd);
            dp.extend(END_NODE);
            dp
        };
        assert_eq!(super::disk_part(&partition(1)), nvme);
        assert_eq!(super::disk_part(&partition(1)), super::disk_part(&partition(3)));
        // A whole disk, or no disk at all.
        let mut whole = nvme.to_vec();
        whole.extend(END_NODE);
        assert!(super::disk_part(&whole).is_empty());
        assert!(super::disk_part(&[]).is_empty());
    }

    #[test]
    fn new_boot_entry() {
        // An NVMe namespace, partition 1 (HD node), then the end node.
        let mut fs = alloc::vec![0x03, 0x17, 0x10, 0x00, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut hd = alloc::vec![0x04, 0x01, 42, 0, 1, 0, 0, 0];
        hd.resize(42, 0);
        fs.extend(hd);
        fs.extend(END_NODE);
        let path = file_device_path(&fs, "\\EFI\\tools\\memtest.efi");
        let data = new_load_option("Memtest", &path);
        let d = parse_load_option(7, &data).unwrap();
        assert_eq!((d.option.title.as_str(), d.file.as_str()), ("Memtest", "\\efi\\tools\\memtest.efi"));
        assert_eq!(d.option.subtitle, "NVMe disk · partition 1");
        assert_eq!(load_option_parts(&data).unwrap().0, path.as_slice());
        assert_eq!(describe_volume(&fs), ("disk", String::from("NVMe disk · partition 1")));

        // Is it on this file system? The same full path, an HD short form,
        // a file alone; not another disk.
        assert!(is_on_file_system(&path, &fs));
        // A whole-device option is on the disk, not on others.
        let disk: Vec<u8> = [&fs[..16], &END_NODE[..]].concat();
        assert!(is_on_device(&disk, &fs));
        assert!(!is_on_device(&fs, &disk));
        assert!(!is_on_device(&END_NODE, &fs));
        assert!(is_on_file_system(&file_device_path(&fs[16..], "\\x.efi"), &fs));
        assert!(is_on_file_system(&file_device_path(&END_NODE, "\\x.efi"), &fs));
        let mut other = fs.clone();
        other[4] = 2;
        assert!(!is_on_file_system(&file_device_path(&other, "\\x.efi"), &fs));
    }

    use super::*;

    use crate::parse::ucs2_bytes as ucs2;

    fn node(ty: u8, sub: u8, data: &[u8]) -> Vec<u8> {
        let len = (4 + data.len()) as u16;
        [&[ty, sub], &len.to_le_bytes()[..], data].concat()
    }

    const END: [u8; 4] = [0x7f, 0xff, 4, 0];

    fn hard_drive(partition: u32) -> Vec<u8> {
        let mut data = partition.to_le_bytes().to_vec();
        data.resize(38, 0);
        node(0x04, 0x01, &data)
    }

    fn file(path: &str) -> Vec<u8> {
        node(0x04, 0x04, &ucs2(path))
    }

    fn load_option(attributes: u32, description: &str, path: &[u8]) -> Vec<u8> {
        [&attributes.to_le_bytes()[..], &(path.len() as u16).to_le_bytes(), &ucs2(description), path, b"optional data"]
            .concat()
    }

    #[test]
    fn ucs2_string() {
        let data = ucs2("Hé");
        assert_eq!(ucs2_until_nul(&data), (String::from("Hé"), 6));
        // Outside the BMP: a surrogate pair.
        assert_eq!(ucs2_until_nul(&ucs2("a😀")), (String::from("a😀"), 8));
        // No terminator, odd trailing byte.
        assert_eq!(ucs2_until_nul(&[b'A', 0, b'B']), (String::from("A"), 2));
        assert_eq!(ucs2_until_nul(&[]), (String::new(), 0));
    }

    #[test]
    fn device_path_nvme_partition_file() {
        let dp = [
            node(0x02, 0x01, &[0; 8]), // ACPI
            node(0x01, 0x01, &[0; 2]), // PCI
            node(0x03, 0x17, &[0; 12]), // NVMe
            hard_drive(1),
            file("\\EFI\\systemd\\systemd-bootx64.efi"),
            END.to_vec(),
        ]
        .concat();
        let info = parse_device_path(&dp);
        assert!(info.nvme && !info.usb && !info.sata);
        assert_eq!(info.partition, Some(1));
        assert_eq!(info.file, "\\EFI\\systemd\\systemd-bootx64.efi");
    }

    #[test]
    fn device_path_stops_at_end_and_on_bad_lengths() {
        let after_end = [END.to_vec(), node(0x03, 0x05, &[0; 2])].concat();
        assert!(!parse_device_path(&after_end).usb);
        // A length shorter than a node header, or past the data.
        assert!(!parse_device_path(&[0x03, 0x05, 2, 0, 0, 0]).usb);
        assert!(!parse_device_path(&[0x03, 0x05, 40, 0]).usb);
        assert!(!parse_device_path(&[0x03]).usb);
    }

    #[test]
    fn device_path_network_and_media() {
        let dp = [node(0x03, 0x0b, &[0; 33]), node(0x03, 0x0d, &[0; 56])].concat();
        let info = parse_device_path(&dp);
        assert!(info.mac && info.ipv6 && !info.ipv4);
        assert!(parse_device_path(&node(0x04, 0x02, &[0; 20])).cdrom);
        assert!(parse_device_path(&node(0x04, 0x07, &[0; 16])).firmware_volume);
        assert!(parse_device_path(&node(0x03, 0x18, b"http://x\0")).uri);
    }

    #[test]
    fn titles_are_cleaned() {
        assert_eq!(clean_title("QEMU QEMU USB HARDDRIVE 1-0000:00:02.0-1"), "QEMU USB Harddrive");
        assert_eq!(clean_title("QEMU NVMe Ctrl SLING01 1"), "QEMU NVMe Ctrl SLING01");
        assert_eq!(clean_title("  Windows   Boot Manager "), "Windows Boot Manager");
        // A title made of a number only stays.
        assert_eq!(clean_title("2"), "2");
        assert_eq!(clean_title(""), "");
    }

    #[test]
    fn load_option_systemd_boot() {
        let path = [hard_drive(1), file("\\EFI\\systemd\\systemd-bootx64.efi"), END.to_vec()].concat();
        let d = parse_load_option(3, &load_option(1, "Linux Boot Manager", &path)).unwrap();
        assert_eq!(d.option.number, 3);
        assert_eq!(d.option.title, "Linux Boot Manager");
        assert_eq!(d.option.subtitle, "Disk · partition 1");
        assert_eq!(d.option.kind, "linux");
        assert!(d.option.is_systemd_boot);
        assert_eq!(d.file, "\\efi\\systemd\\systemd-bootx64.efi");
    }

    #[test]
    fn load_option_inactive_hidden_or_short() {
        let path = END.to_vec();
        assert!(parse_load_option(0, &load_option(0, "Off", &path)).is_none());
        assert!(parse_load_option(0, &load_option(1 | 8, "Hidden", &path)).is_none());
        assert!(parse_load_option(0, &[1, 0, 0, 0, 0]).is_none());
        // A path length past the data is cut, not a panic.
        let mut data = load_option(1, "Truncated", &file("\\a.efi"));
        data[4] = 0xff;
        assert!(parse_load_option(0, &data).is_some());
    }

    #[test]
    fn load_option_empty_description() {
        let d = parse_load_option(0x1a, &load_option(1, "", &END)).unwrap();
        assert_eq!(d.option.title, "Boot001A");
        assert_eq!(d.option.kind, "disk");
    }

    fn describe_with(description: &str, info: PathInfo) -> BootOption {
        describe(1, description, &info).option
    }

    #[test]
    fn kinds() {
        let file = |f: &str| PathInfo { file: f.into(), ..Default::default() };
        assert_eq!(describe_with("Windows Boot Manager", file("")).kind, "windows");
        assert_eq!(describe_with("Something", file("\\EFI\\Microsoft\\Boot\\bootmgfw.efi")).kind, "windows");
        assert_eq!(describe_with("ubuntu", file("\\EFI\\ubuntu\\shimx64.efi")).kind, "linux");
        assert_eq!(describe_with("Mystery", file("/EFI/fedora/shim.efi")).kind, "linux");
        assert_eq!(describe_with("EFI Internal Shell", file("")).kind, "shell");
        assert_eq!(describe_with("UEFI DVD", PathInfo::default()).kind, "disc");
        assert_eq!(describe_with("x", PathInfo { cdrom: true, ..Default::default() }).kind, "disc");
        assert_eq!(describe_with("x", PathInfo { firmware_volume: true, ..Default::default() }).kind, "app");
        assert_eq!(describe_with("x", PathInfo::default()).kind, "disk");
    }

    #[test]
    fn network_entries() {
        let o = describe_with("UEFI PXEv4 (MAC:525400123456)", PathInfo { mac: true, ipv4: true, ..Default::default() });
        assert_eq!((o.kind, o.title.as_str(), o.subtitle.as_str()), ("network", "PXE boot", "Network · IPv4"));
        let o = describe_with("UEFI HTTPv6 (MAC:525400123456)", PathInfo { mac: true, ipv6: true, uri: true, ..Default::default() });
        assert_eq!((o.title.as_str(), o.subtitle.as_str()), ("HTTP boot", "Network · IPv6"));
    }

    #[test]
    fn subtitles() {
        let o = describe_with("UEFI QEMU USB HARDDRIVE", PathInfo { usb: true, partition: Some(1), ..Default::default() });
        assert_eq!((o.kind, o.title.as_str(), o.subtitle.as_str()), ("usb", "QEMU USB Harddrive", "USB · removable"));
        let o = describe_with("Disk", PathInfo { sata: true, partition: Some(2), ..Default::default() });
        assert_eq!(o.subtitle, "SATA disk · partition 2");
        let o = describe_with("Disk", PathInfo { nvme: true, ..Default::default() });
        assert_eq!(o.subtitle, "NVMe disk");
        let o = describe_with("UiApp", PathInfo { firmware_volume: true, ..Default::default() });
        assert_eq!(o.subtitle, "Built into firmware");
    }

    #[test]
    fn load_option_parts_split() {
        let path = [hard_drive(1), file("\\a.efi"), END.to_vec()].concat();
        let data = load_option(1, "A", &path);
        let (p, optional) = load_option_parts(&data).unwrap();
        assert_eq!(p, &path[..]);
        assert_eq!(optional, b"optional data");
        assert!(load_option_parts(&[1, 0, 0, 0]).is_none());
        let mut data = load_option(1, "A", &path);
        data[4] = 0xff;
        assert!(load_option_parts(&data).is_none(), "path past the data");
    }

    #[test]
    fn device_path_expansion() {
        let pci = [node(0x02, 0x01, &[0; 8]), node(0x01, 0x01, &[0; 2]), node(0x03, 0x17, &[0; 12])].concat();
        let fs1 = [pci.clone(), hard_drive(1), END.to_vec()].concat();
        let fs2 = [pci.clone(), hard_drive(2), END.to_vec()].concat();
        let file_systems = [fs1.clone(), fs2.clone()];
        let tail = [file("\\EFI\\x.efi"), END.to_vec()].concat();

        // HD(2)/file goes on the second partition.
        let short = [hard_drive(2), tail.clone()].concat();
        let expanded = expand_device_path(&short, &file_systems);
        assert_eq!(expanded, [[pci.clone(), hard_drive(2), tail.clone()].concat()]);

        // No such partition.
        assert!(expand_device_path(&[hard_drive(3), tail.clone()].concat(), &file_systems).is_empty());

        // A file alone: every file system.
        let expanded = expand_device_path(&tail, &file_systems);
        assert_eq!(expanded.len(), 2);
        assert_eq!(expanded[1], [pci.clone(), hard_drive(2), tail.clone()].concat());

        // Full paths stay as they are.
        let full = [pci.clone(), hard_drive(1), tail.clone()].concat();
        assert_eq!(expand_device_path(&full, &file_systems), [full]);

        // A whole disk: as is, then BOOTX64.EFI on each of its partitions.
        let disk = [pci.clone(), END.to_vec()].concat();
        let default = [file(&crate::parse::default_boot_file()), END.to_vec()].concat();
        let expanded = expand_device_path(&disk, &file_systems);
        assert_eq!(expanded.len(), 3);
        assert_eq!(expanded[0], disk);
        assert_eq!(expanded[2], [pci.clone(), hard_drive(2), default].concat());
        // Another disk's file systems aren't used.
        let other = [node(0x02, 0x01, &[1; 8]), END.to_vec()].concat();
        assert_eq!(expand_device_path(&other, &file_systems), [other]);

        // USB class short form, and nothing.
        assert!(expand_device_path(&[node(0x03, 0x0f, &[0; 7]), END.to_vec()].concat(), &file_systems).is_empty());
        assert!(expand_device_path(&END, &file_systems).is_empty());
        assert!(expand_device_path(&[], &file_systems).is_empty());
    }

    #[test]
    fn paths_are_normalized() {
        assert_eq!(normalize_path("/EFI/Slingshot/SLINGSHOT.efi"), "\\efi\\slingshot\\slingshot.efi");
    }
}
