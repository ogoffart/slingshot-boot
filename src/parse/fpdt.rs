// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! The firmware's boot timings, from ACPI's Firmware Performance Data Table
//! (FPDT, ACPI 6.5 section 5.2.24): when the firmware finished its reset,
//! and when it loaded and started the boot loader (Slingshot), in
//! nanoseconds of the same clock. Logged, to tell the firmware's time from
//! Slingshot's.

// The firmware's: the desktop program has none.
#![cfg_attr(not(target_os = "uefi"), allow(dead_code))]

/// The Firmware Basic Boot Performance Data record (type 2), in ns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootTimes {
    /// The end of the firmware's reset (0 when unknown).
    pub reset_end: u64,
    /// The boot loader's image loaded, then started.
    pub loader_load: u64,
    pub loader_start: u64,
}

fn u64_at(data: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(data.get(at..at + 8)?.try_into().ok()?))
}

/// Where the FPDT's Basic Boot Performance Table (FBPT) is: the address in
/// its pointer record (type 0). `fpdt` is the whole table, header included.
pub fn fbpt_address(fpdt: &[u8]) -> Option<u64> {
    if fpdt.get(..4)? != b"FPDT" {
        return None;
    }
    let mut at = 36;
    while at + 4 <= fpdt.len() {
        let kind = u16::from_le_bytes([fpdt[at], fpdt[at + 1]]);
        let len = fpdt[at + 2] as usize;
        if len < 4 {
            return None;
        }
        if kind == 0 && len >= 16 {
            return u64_at(fpdt, at + 8).filter(|a| *a != 0);
        }
        at += len;
    }
    None
}

/// The basic boot record of an FBPT (its 8-byte header first).
pub fn boot_times(fbpt: &[u8]) -> Option<BootTimes> {
    if fbpt.get(..4)? != b"FBPT" {
        return None;
    }
    let length = u32::from_le_bytes(fbpt.get(4..8)?.try_into().ok()?) as usize;
    let fbpt = fbpt.get(..length.min(fbpt.len()))?;
    let mut at = 8;
    while at + 4 <= fbpt.len() {
        let kind = u16::from_le_bytes([fbpt[at], fbpt[at + 1]]);
        let len = fbpt[at + 2] as usize;
        if len < 4 {
            return None;
        }
        if kind == 2 && len >= 48 {
            return Some(BootTimes {
                reset_end: u64_at(fbpt, at + 8)?,
                loader_load: u64_at(fbpt, at + 16)?,
                loader_start: u64_at(fbpt, at + 24)?,
            });
        }
        at += len;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn reads_the_boot_record() {
        let mut fpdt = Vec::from(*b"FPDT");
        fpdt.resize(36, 0);
        // An S3 pointer record (type 1) first, then the basic boot one.
        fpdt.extend_from_slice(&[1, 0, 16, 1, 0, 0, 0, 0]);
        fpdt.extend_from_slice(&0x1111u64.to_le_bytes());
        fpdt.extend_from_slice(&[0, 0, 16, 1, 0, 0, 0, 0]);
        fpdt.extend_from_slice(&0x7654_3000u64.to_le_bytes());
        assert_eq!(fbpt_address(&fpdt), Some(0x7654_3000));
        assert_eq!(fbpt_address(b"APIC"), None);

        let mut fbpt = Vec::from(*b"FBPT");
        fbpt.extend_from_slice(&56u32.to_le_bytes());
        fbpt.extend_from_slice(&[2, 0, 48, 2, 0, 0, 0, 0]);
        for ns in [1_500_000_000u64, 4_200_000_000, 4_250_000_000, 0, 0] {
            fbpt.extend_from_slice(&ns.to_le_bytes());
        }
        let t = boot_times(&fbpt).unwrap();
        assert_eq!((t.reset_end, t.loader_load, t.loader_start), (1_500_000_000, 4_200_000_000, 4_250_000_000));
        assert_eq!(boot_times(&fbpt[..20]), None);
    }
}
