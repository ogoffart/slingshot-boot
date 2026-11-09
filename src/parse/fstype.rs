// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! What a partition holds, from its signatures: to say why the firmware
//! can't read a disk (ext4, btrfs, NTFS… have no driver in most firmware).

// The desktop program only uses `is_encrypted`.
#![cfg_attr(not(target_os = "uefi"), allow(dead_code))]

/// Where `identify` wants to look: the start of the partition, and 64 KiB
/// in (btrfs's superblock).
pub const HEAD: usize = 4096;
pub const BTRFS_AT: u64 = 0x10000;

/// The file system or container of a partition, from its first `HEAD`
/// bytes and the `HEAD` bytes at `BTRFS_AT` (empty if unread).
pub fn identify(head: &[u8], at_64k: &[u8]) -> Option<&'static str> {
    let at = |offset: usize, magic: &[u8]| head.get(offset..offset + magic.len()) == Some(magic);
    let u16_at = |offset: usize| head.get(offset..offset + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |offset: usize| head.get(offset..offset + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    if at(0, b"LUKS\xba\xbe") {
        return Some("LUKS");
    }
    if at(3, b"-FVE-FS-") {
        return Some("BitLocker");
    }
    if at(3, b"NTFS    ") {
        return Some("NTFS");
    }
    if at(3, b"EXFAT   ") {
        return Some("exFAT");
    }
    if at(510, b"\x55\xaa") && (at(54, b"FAT1") || at(82, b"FAT32")) {
        return Some("FAT");
    }
    if at(0, b"XFSB") {
        return Some("XFS");
    }
    if at(32, b"NXSB") {
        return Some("APFS");
    }
    if at(1024, b"H+") || at(1024, b"HX") {
        return Some("HFS+");
    }
    if at(512, b"LABELONE") {
        return Some("LVM");
    }
    if at(4086, b"SWAPSPACE2") {
        return Some("swap");
    }
    if u32_at(1024) == Some(0xf2f5_2010) {
        return Some("F2FS");
    }
    if u16_at(1024 + 0x38) == Some(0xef53) {
        // The superblock's feature flags: extents (or 64-bit, flex_bg)
        // make it ext4, a journal ext3.
        let compat = u32_at(1024 + 0x5c).unwrap_or(0);
        let incompat = u32_at(1024 + 0x60).unwrap_or(0);
        return Some(if incompat & (0x40 | 0x80 | 0x200) != 0 {
            "ext4"
        } else if compat & 4 != 0 {
            "ext3"
        } else {
            "ext2"
        });
    }
    if at_64k.get(64..72) == Some(b"_BHRfS_M") {
        return Some("btrfs");
    }
    None
}

/// Whether `kind` (from `identify`) is encrypted: no driver reads it.
pub fn is_encrypted(kind: &str) -> bool {
    matches!(kind, "LUKS" | "BitLocker")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head() -> Vec<u8> {
        alloc::vec![0; HEAD]
    }

    #[test]
    fn signatures() {
        assert_eq!(identify(&head(), &[]), None);
        let mut h = head();
        h[..6].copy_from_slice(b"LUKS\xba\xbe");
        assert_eq!(identify(&h, &[]), Some("LUKS"));
        let mut h = head();
        h[3..11].copy_from_slice(b"NTFS    ");
        assert_eq!(identify(&h, &[]), Some("NTFS"));
        let mut h = head();
        h[82..87].copy_from_slice(b"FAT32");
        h[510..512].copy_from_slice(&[0x55, 0xaa]);
        assert_eq!(identify(&h, &[]), Some("FAT"));
        let mut b = head();
        b[64..72].copy_from_slice(b"_BHRfS_M");
        assert_eq!(identify(&head(), &b), Some("btrfs"));
    }

    #[test]
    fn ext_versions() {
        let mut h = head();
        h[1024 + 0x38..1024 + 0x3a].copy_from_slice(&0xef53u16.to_le_bytes());
        assert_eq!(identify(&h, &[]), Some("ext2"));
        h[1024 + 0x5c] = 4;
        assert_eq!(identify(&h, &[]), Some("ext3"));
        h[1024 + 0x60] = 0x40;
        assert_eq!(identify(&h, &[]), Some("ext4"));
    }
}
