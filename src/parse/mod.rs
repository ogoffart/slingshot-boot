// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Parsers for what the firmware and the EFI System Partition hold. Pure
//! functions over bytes and text, so that they are unit-tested on the
//! desktop; the UEFI build reads the data and passes it in.

pub mod battery;
pub mod bls;
// What a file system can start: only the firmware scans them; the desktop
// build has it for the tests.
#[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
pub mod detect;
pub mod fpdt;
pub mod fstype;
// Only the firmware reads HibernateLocation; the desktop build has it for
// the tests.
#[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
pub mod hibernate;
pub mod hid;
pub mod keymap;
pub mod load_option;
// The variables of systemd's Boot Loader Interface: only the firmware sets
// them; the desktop build has it for the tests.
#[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
pub mod loader_interface;
// Firmware only (unlocking encrypted disks); built on the desktop for the
// tests and the simulator.
#[cfg(feature = "luks")]
#[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
pub mod handoff;
#[cfg(feature = "luks")]
#[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
pub mod luks;
// The System screen's hardware details: only the firmware reads SMBIOS; the
// desktop build has it for the tests.
#[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
pub mod smbios;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The architecture in EFI file names: `BOOTX64.EFI`, `systemd-bootaa64.efi`.
#[cfg(target_arch = "aarch64")]
pub const EFI_ARCH: &str = "aa64";
#[cfg(not(target_arch = "aarch64"))]
pub const EFI_ARCH: &str = "x64";

/// What firmware starts from a disk without a boot entry:
/// `\EFI\BOOT\BOOTX64.EFI` (`BOOTAA64.EFI` on ARM).
pub fn default_boot_file() -> String {
    format!("\\EFI\\BOOT\\BOOT{}.EFI", EFI_ARCH.to_ascii_uppercase())
}

/// A firmware variable's bytes as little-endian 16-bit values (BootOrder, a
/// UCS-2 string). A trailing odd byte is ignored.
pub fn le_u16s(data: &[u8]) -> impl Iterator<Item = u16> + '_ {
    data.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c))
}

/// A string as UEFI stores it: UTF-16LE, with the terminating NUL.
pub fn ucs2_bytes(s: &str) -> Vec<u8> {
    s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
}

/// A UCS-2 string variable, up to its terminating NUL if it has one.
pub fn ucs2_string(data: &[u8]) -> String {
    let units: Vec<u16> = le_u16s(data).take_while(|c| *c != 0).collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ucs2() {
        assert_eq!(ucs2_string(b"a\0b\0\0\0c\0"), "ab");
        assert_eq!(ucs2_string(b"a\0b"), "a");
        assert_eq!(le_u16s(&[1, 0, 0, 1]).collect::<Vec<_>>(), [1, 256]);
    }
}
