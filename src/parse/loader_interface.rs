// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! systemd's Boot Loader Interface (<https://systemd.io/BOOT_LOADER_INTERFACE/>):
//! the EFI variables a boot loader sets to tell the operating system about
//! itself and the boot (`bootctl status`, `systemd-analyze`, gpt-auto).
//! Their contents, built here; the firmware build sets them.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// `LoaderFeatures`: what Slingshot does of what systemd-boot does.
pub mod features {
    pub const ENTRY_DEFAULT: u64 = 1 << 2;
    pub const ENTRY_ONESHOT: u64 = 1 << 3;
    pub const BOOT_COUNTING: u64 = 1 << 4;
    pub const SORT_KEY: u64 = 1 << 8;
}

/// The features Slingshot reports.
pub const FEATURES: u64 = features::ENTRY_DEFAULT | features::ENTRY_ONESHOT | features::BOOT_COUNTING | features::SORT_KEY;

/// `LoaderEntries`: the entry ids, each nul-terminated.
pub fn entries(ids: &[&str]) -> Vec<u8> {
    ids.iter().flat_map(|id| super::ucs2_bytes(id)).collect()
}

/// The GPT partition GUID of the partition a device path is on, as
/// `LoaderDevicePartUUID` has it: upper case.
pub fn partition_uuid(device_path: &[u8]) -> Option<String> {
    super::detect::partition_uuid(device_path).map(|u| u.to_ascii_uppercase())
}

/// `LoaderFirmwareInfo`: the vendor and its revision, as systemd-boot
/// writes it ("EDK II 1.00").
pub fn firmware_info(vendor: &str, revision: u32) -> String {
    format!("{vendor} {}", firmware_revision(revision))
}

/// The firmware's revision as systemd-boot writes it: "5.26".
pub fn firmware_revision(revision: u32) -> String {
    format!("{}.{:02}", revision >> 16, revision & 0xffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_and_entries() {
        assert_eq!(crate::parse::ucs2_bytes("ab"), [b'a', 0, b'b', 0, 0, 0]);
        assert_eq!(entries(&["a", "b"]), [b'a', 0, 0, 0, b'b', 0, 0, 0]);
        assert_eq!(FEATURES, 0x11c);
    }

    #[test]
    fn partition_uuid_from_a_device_path() {
        // PCI, then a GPT partition, then a file path, then the end.
        let mut dp = alloc::vec![1, 1, 6, 0, 0, 0];
        let mut hd = alloc::vec![4, 1, 42, 0];
        hd.extend_from_slice(&1u32.to_le_bytes());
        hd.extend_from_slice(&2048u64.to_le_bytes());
        hd.extend_from_slice(&1024u64.to_le_bytes());
        // c12a7328-f81f-11d2-ba4b-00a0c93ec93b, mixed-endian.
        hd.extend_from_slice(&[0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b]);
        hd.extend_from_slice(&[2, 2]);
        dp.extend_from_slice(&hd);
        dp.extend_from_slice(&[4, 4, 6, 0, b'a', 0]);
        dp.extend_from_slice(&[0x7f, 0xff, 4, 0]);
        assert_eq!(partition_uuid(&dp).as_deref(), Some("C12A7328-F81F-11D2-BA4B-00A0C93EC93B"));
        // An MBR partition has no GUID.
        hd[41] = 1;
        assert_eq!(partition_uuid(&hd), None);
    }

    #[test]
    fn firmware_info_like_systemd_boot() {
        assert_eq!(firmware_info("EDK II", 0x10000), "EDK II 1.00");
        assert_eq!(firmware_info("American Megatrends", 0x5001a), "American Megatrends 5.26");
    }
}
