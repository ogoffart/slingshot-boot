// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! What an EFI program is, from its PE headers and sections: the Files
//! screen's details of an `.efi` file.

use alloc::string::String;
use alloc::vec::Vec;

use object::LittleEndian as LE;
use object::pe;
use object::read::pe::{ImageNtHeaders, ImageOptionalHeader, PeFile, PeFile32, PeFile64};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Program {
    /// The processor: "x86_64", "AArch64"… (or the PE machine number).
    pub machine: String,
    /// An EFI application, a driver, or something else (a Windows program).
    pub subsystem: Subsystem,
    /// It has an Authenticode signature (not checked here).
    pub signed: bool,
    /// The components of its `.sbat` section (shim's revocation data).
    pub sbat: Vec<String>,
    /// A Unified Kernel Image: its `.osrel`'s PRETTY_NAME, its `.uname`,
    /// its `.cmdline`.
    pub uki: bool,
    pub os: Option<String>,
    pub kernel: Option<String>,
    pub cmdline: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Subsystem {
    #[default]
    Application,
    BootServiceDriver,
    RuntimeDriver,
    Other,
}

/// `data`'s details, if it's a PE file.
pub fn program(data: &[u8]) -> Option<Program> {
    match PeFile64::parse(data) {
        Ok(file) => Some(details(&file, data)),
        Err(_) => PeFile32::parse(data).ok().map(|file| details(&file, data)),
    }
}

fn details<Pe: ImageNtHeaders>(file: &PeFile<'_, Pe>, data: &[u8]) -> Program {
    let machine = file.nt_headers().file_header().machine.get(LE);
    let machine = match machine {
        pe::IMAGE_FILE_MACHINE_AMD64 => String::from("x86_64"),
        pe::IMAGE_FILE_MACHINE_ARM64 => String::from("AArch64"),
        pe::IMAGE_FILE_MACHINE_I386 => String::from("x86 (32-bit)"),
        pe::IMAGE_FILE_MACHINE_ARMNT => String::from("ARM (32-bit)"),
        pe::IMAGE_FILE_MACHINE_RISCV64 => String::from("RISC-V 64"),
        other => alloc::format!("{:#06x}", other.0),
    };
    let subsystem = match file.nt_headers().optional_header().subsystem() {
        pe::IMAGE_SUBSYSTEM_EFI_APPLICATION => Subsystem::Application,
        pe::IMAGE_SUBSYSTEM_EFI_BOOT_SERVICE_DRIVER => Subsystem::BootServiceDriver,
        pe::IMAGE_SUBSYSTEM_EFI_RUNTIME_DRIVER => Subsystem::RuntimeDriver,
        _ => Subsystem::Other,
    };
    let signed = file.data_directory(pe::IMAGE_DIRECTORY_ENTRY_SECURITY).is_some_and(|d| d.size.get(LE) > 0);
    let section = |name: &[u8]| {
        let s = file.section_table().iter().find(|s| s.raw_name() == name)?;
        let bytes = s.pe_data(data).ok()?;
        // Sections are padded with zeros.
        let end = bytes.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
        Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
    };
    let sbat = section(b".sbat")
        .map(|csv| csv.lines().filter_map(|l| l.split(',').next()).filter(|c| !c.is_empty() && *c != "sbat").map(String::from).collect())
        .unwrap_or_default();
    let os = section(b".osrel").and_then(|t| super::detect::os_release(&t, "PRETTY_NAME").or_else(|| super::detect::os_release(&t, "NAME")));
    Program {
        machine,
        subsystem,
        signed,
        sbat,
        // Not read: the kernel is most of the file.
        uki: file.section_table().iter().any(|s| s.raw_name() == b".linux"),
        os,
        kernel: section(b".uname").map(|u| String::from(u.trim())),
        cmdline: section(b".cmdline").map(|c| String::from(c.trim())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal PE32+ file: the headers and `sections` (name, contents),
    /// with a signature directory of `signature` bytes.
    fn pe(machine: u16, subsystem: u16, sections: &[(&[u8], &[u8])], signature: u32) -> Vec<u8> {
        const HEADERS: usize = 0x400;
        let mut out = alloc::vec![0u8; HEADERS];
        out[0..2].copy_from_slice(b"MZ");
        out[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        let h = 0x40;
        out[h..h + 4].copy_from_slice(b"PE\0\0");
        // The file header.
        let f = h + 4;
        out[f..f + 2].copy_from_slice(&machine.to_le_bytes());
        out[f + 2..f + 4].copy_from_slice(&(sections.len() as u16).to_le_bytes());
        out[f + 16..f + 18].copy_from_slice(&240u16.to_le_bytes());
        out[f + 18..f + 20].copy_from_slice(&0x0022u16.to_le_bytes());
        // The optional header (PE32+), with 16 data directories.
        let o = f + 20;
        out[o..o + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        out[o + 32..o + 36].copy_from_slice(&0x1000u32.to_le_bytes());
        out[o + 36..o + 40].copy_from_slice(&0x200u32.to_le_bytes());
        out[o + 68..o + 70].copy_from_slice(&subsystem.to_le_bytes());
        out[o + 108..o + 112].copy_from_slice(&16u32.to_le_bytes());
        out[o + 56..o + 60].copy_from_slice(&((sections.len() as u32 + 1) * 0x1000).to_le_bytes());
        out[o + 60..o + 64].copy_from_slice(&(HEADERS as u32).to_le_bytes());
        let security = o + 112 + 4 * 8;
        // The signature sits after the sections (a file offset, not an RVA).
        let sig_offset = (HEADERS + sections.len() * 0x200) as u32;
        if signature > 0 {
            out[security..security + 4].copy_from_slice(&sig_offset.to_le_bytes());
            out[security + 4..security + 8].copy_from_slice(&signature.to_le_bytes());
        }
        // The section table, then each section's data in 0x200 bytes.
        let mut t = o + 240;
        for (i, (name, data)) in sections.iter().enumerate() {
            let mut n = [0u8; 8];
            n[..name.len()].copy_from_slice(name);
            out[t..t + 8].copy_from_slice(&n);
            out[t + 8..t + 12].copy_from_slice(&(data.len() as u32).to_le_bytes());
            out[t + 12..t + 16].copy_from_slice(&((i as u32 + 1) * 0x1000).to_le_bytes());
            out[t + 16..t + 20].copy_from_slice(&0x200u32.to_le_bytes());
            out[t + 20..t + 24].copy_from_slice(&((HEADERS + i * 0x200) as u32).to_le_bytes());
            out[t + 36..t + 40].copy_from_slice(&0x4000_0040u32.to_le_bytes());
            t += 40;
        }
        for (_, data) in sections {
            let mut block = alloc::vec![0u8; 0x200];
            block[..data.len()].copy_from_slice(data);
            out.extend_from_slice(&block);
        }
        out.extend(core::iter::repeat_n(0, signature as usize));
        out
    }

    #[test]
    fn a_unified_kernel_image() {
        let data = pe(
            0x8664,
            10,
            &[
                (b".osrel", b"NAME=\"Arch Linux\"\nPRETTY_NAME=\"Arch Linux\"\nID=arch\n"),
                (b".cmdline", b"root=UUID=1234 rw quiet\n"),
                (b".uname", b"6.17.1-arch1-1\n"),
                (b".sbat", b"sbat,1,SBAT Version,sbat,1,https://example\nsystemd-stub,1,The systemd Developers,systemd,258,https://example\n"),
                (b".linux", b"MZ kernel"),
            ],
            64,
        );
        let p = program(&data).unwrap();
        assert_eq!((p.machine.as_str(), p.subsystem, p.signed, p.uki), ("x86_64", Subsystem::Application, true, true));
        assert_eq!(p.os.as_deref(), Some("Arch Linux"));
        assert_eq!(p.kernel.as_deref(), Some("6.17.1-arch1-1"));
        assert_eq!(p.cmdline.as_deref(), Some("root=UUID=1234 rw quiet"));
        assert_eq!(p.sbat, ["systemd-stub"]);
    }

    #[test]
    fn a_plain_driver() {
        let p = program(&pe(0xaa64, 11, &[(b".text", b"\xc3")], 0)).unwrap();
        assert_eq!((p.machine.as_str(), p.subsystem, p.signed, p.uki), ("AArch64", Subsystem::BootServiceDriver, false, false));
        assert_eq!((p.os, p.kernel, p.sbat.len()), (None, None, 0));
    }

    #[test]
    fn not_a_program() {
        assert_eq!(program(b"MZ but nothing else"), None);
        assert_eq!(program(b"timeout 3\n"), None);
    }
}
