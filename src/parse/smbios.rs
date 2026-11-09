// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Parsing of the SMBIOS structure table: firmware, system, board, CPU,
//! memory devices and batteries.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Default, Debug)]
pub struct Smbios {
    pub version: Option<(u8, u8)>,
    pub bios_date: String,
    pub system_vendor: String,
    pub system_product: String,
    pub board_product: String,
    pub cpu_name: String,
    pub cpu_cores: u16,
    pub cpu_threads: u16,
    pub cpu_max_mhz: u16,
    /// Size in MB and type name of each populated memory device.
    pub dimms: Vec<(u64, &'static str, u16)>,
    /// The portable batteries' static details (no charge level).
    pub batteries: Vec<Battery>,
}

use super::battery::Battery;

struct Structure<'a> {
    ty: u8,
    formatted: &'a [u8],
    strings: Vec<&'a [u8]>,
}

impl Structure<'_> {
    fn byte(&self, offset: usize) -> u8 {
        self.formatted.get(offset).copied().unwrap_or(0)
    }
    fn word(&self, offset: usize) -> u16 {
        u16::from_le_bytes([self.byte(offset), self.byte(offset + 1)])
    }
    fn dword(&self, offset: usize) -> u32 {
        u32::from_le_bytes([self.byte(offset), self.byte(offset + 1), self.byte(offset + 2), self.byte(offset + 3)])
    }
    fn string(&self, offset: usize) -> String {
        let index = self.byte(offset) as usize;
        if index == 0 {
            return String::new();
        }
        self.strings
            .get(index - 1)
            .map(|s| String::from_utf8_lossy(s).trim().to_string())
            .unwrap_or_default()
    }
}

fn structures(table: &[u8]) -> Vec<Structure<'_>> {
    let mut result = Vec::new();
    let mut rest = table;
    while rest.len() >= 4 {
        let ty = rest[0];
        let len = rest[1] as usize;
        if len < 4 || len > rest.len() {
            break;
        }
        let formatted = &rest[..len];
        // Strings follow, each NUL-terminated, the set ends with a double NUL.
        let mut strings = Vec::new();
        let mut i = len;
        if rest.get(i) == Some(&0) && rest.get(i + 1) == Some(&0) {
            i += 2;
        } else {
            loop {
                let start = i;
                while i < rest.len() && rest[i] != 0 {
                    i += 1;
                }
                if i >= rest.len() {
                    break;
                }
                strings.push(&rest[start..i]);
                i += 1;
                if rest.get(i) == Some(&0) {
                    i += 1;
                    break;
                }
            }
        }
        result.push(Structure { ty, formatted, strings });
        if ty == 127 {
            break;
        }
        rest = rest.get(i..).unwrap_or(&[]);
    }
    result
}

fn memory_type_name(t: u8) -> &'static str {
    match t {
        0x12 => "DDR",
        0x13 => "DDR2",
        0x18 => "DDR3",
        0x1a => "DDR4",
        0x1b => "LPDDR",
        0x1c => "LPDDR2",
        0x1d => "LPDDR3",
        0x1e => "LPDDR4",
        0x22 => "DDR5",
        0x23 => "LPDDR5",
        _ => "RAM",
    }
}

fn chemistry(c: u8) -> &'static str {
    match c {
        3 => "Lead acid",
        4 => "NiCd",
        5 => "NiMH",
        6 => "Li-ion",
        7 => "Zinc air",
        8 => "Li-poly",
        _ => "",
    }
}

/// A portable battery (type 22). The Smart Battery Data Specification
/// fields stand in for the strings the firmware leaves empty.
fn battery(s: &Structure) -> Battery {
    let multiplier = if s.formatted.len() > 0x15 { s.byte(0x15).max(1) as u32 } else { 1 };
    let mut b = Battery {
        manufacturer: s.string(0x05),
        manufacture_date: s.string(0x06),
        serial: s.string(0x07),
        model: s.string(0x08),
        technology: chemistry(s.byte(0x09)).to_string(),
        design: Some(s.word(0x0a) as u32 * multiplier).filter(|d| *d > 0),
        design_voltage_mv: Some(s.word(0x0c) as u32).filter(|v| *v > 0),
        ..Battery::default()
    };
    if s.formatted.len() > 0x14 {
        if b.technology.is_empty() {
            b.technology = s.string(0x14);
        }
        if b.serial.is_empty() && s.word(0x10) != 0 {
            b.serial = format!("{:04X}", s.word(0x10));
        }
        let date = s.word(0x12);
        if b.manufacture_date.is_empty() && date != 0 {
            b.manufacture_date = format!("{}-{:02}-{:02}", 1980 + (date >> 9), (date >> 5) & 0xf, date & 0x1f);
        }
    }
    b
}

/// Reads what the System screen shows from an SMBIOS structure table.
pub fn parse(table: &[u8]) -> Smbios {
    let mut info = Smbios::default();
    for s in structures(table) {
        match s.ty {
            0 => {
                info.bios_date = s.string(8);
            }
            1 => {
                info.system_vendor = s.string(4);
                info.system_product = s.string(5);
            }
            2 => {
                info.board_product = s.string(5);
            }
            4 if info.cpu_name.is_empty() => {
                info.cpu_name = s.string(0x10);
                info.cpu_max_mhz = s.word(0x14);
                info.cpu_cores = s.byte(0x23) as u16;
                info.cpu_threads = s.byte(0x25) as u16;
                if info.cpu_cores == 0xff && s.formatted.len() > 0x2b {
                    info.cpu_cores = s.word(0x2a);
                }
                if info.cpu_threads == 0xff && s.formatted.len() > 0x2f {
                    info.cpu_threads = s.word(0x2e);
                }
            }
            17 => {
                let size = s.word(0x0c);
                let mb: u64 = match size {
                    0 | 0xffff => continue,
                    0x7fff => s.dword(0x1c) as u64 & 0x7fff_ffff,
                    s if s & 0x8000 != 0 => ((s & 0x7fff) as u64) / 1024,
                    s => s as u64,
                };
                info.dimms.push((mb, memory_type_name(s.byte(0x12)), s.word(0x15)));
            }
            22 => info.batteries.push(battery(&s)),
            _ => {}
        }
    }
    info
}


#[cfg(test)]
mod tests {
    use super::*;

    /// A structure: header, `len - 4` formatted bytes set by `set`, strings.
    fn structure(ty: u8, len: u8, set: &[(usize, &[u8])], strings: &[&str]) -> Vec<u8> {
        let mut s = alloc::vec![0u8; len as usize];
        s[0] = ty;
        s[1] = len;
        for (offset, bytes) in set {
            s[*offset..offset + bytes.len()].copy_from_slice(bytes);
        }
        for string in strings {
            s.extend_from_slice(string.as_bytes());
            s.push(0);
        }
        if strings.is_empty() {
            s.push(0);
        }
        s.push(0);
        s
    }

    fn dimm(size: u16, extended: u32, ty: u8, speed: u16) -> Vec<u8> {
        structure(
            17,
            0x28,
            &[(0x0c, &size.to_le_bytes()), (0x12, &[ty]), (0x15, &speed.to_le_bytes()), (0x1c, &extended.to_le_bytes())],
            &[],
        )
    }

    fn table() -> Vec<u8> {
        [
            structure(0, 0x18, &[(4, &[1]), (8, &[2])], &["Firmware Inc.", "03/14/2026 "]),
            structure(1, 0x1b, &[(4, &[1]), (5, &[2])], &["Example Computers", "Model 13"]),
            structure(2, 0x0f, &[(5, &[1])], &["Mainboard X"]),
            structure(
                4,
                0x30,
                &[(0x10, &[1]), (0x14, &4800u16.to_le_bytes()), (0x23, &[8]), (0x25, &[16])],
                &["Example CPU 9000"],
            ),
            // A second CPU socket is ignored.
            structure(4, 0x30, &[(0x10, &[1])], &["Second CPU"]),
            dimm(16384, 0, 0x22, 5600),
            dimm(0, 0, 0x22, 0),         // empty slot
            dimm(0xffff, 0, 0x22, 0),    // unknown size
            dimm(0x8000 | 512, 0, 0x1a, 3200), // size in KB
            dimm(0x7fff, 65536, 0x23, 7500),   // extended size
            structure(
                22,
                0x1a,
                &[(5, &[1]), (8, &[2]), (9, &[2]), (0x0a, &5700u16.to_le_bytes()), (0x0c, &11520u16.to_le_bytes()),
                  (0x10, &0xbeefu16.to_le_bytes()), (0x12, &((41 << 9 | 3 << 5 | 14) as u16).to_le_bytes()), (0x14, &[3]), (0x15, &[10])],
                &["SMP", "5B10W13930", "LiP"],
            ),
            structure(127, 4, &[], &[]),
            structure(1, 0x1b, &[(4, &[1])], &["After the end"]),
        ]
        .concat()
    }

    #[test]
    fn parses_a_table() {
        let info = parse(&table());
        assert_eq!(info.bios_date, "03/14/2026");
        assert_eq!(info.system_vendor, "Example Computers");
        assert_eq!(info.system_product, "Model 13");
        assert_eq!(info.board_product, "Mainboard X");
        assert_eq!(info.cpu_name, "Example CPU 9000");
        assert_eq!((info.cpu_max_mhz, info.cpu_cores, info.cpu_threads), (4800, 8, 16));
        assert_eq!(info.dimms, [(16384, "DDR5", 5600), (0, "DDR4", 3200), (65536, "LPDDR5", 7500)]);
        assert_eq!(info.version, None);
        let b = &info.batteries[0];
        assert_eq!((b.manufacturer.as_str(), b.model.as_str(), b.technology.as_str()), ("SMP", "5B10W13930", "LiP"));
        assert_eq!((b.design, b.design_voltage_mv), (Some(57000), Some(11520)));
        assert_eq!((b.serial.as_str(), b.manufacture_date.as_str()), ("BEEF", "2021-03-14"));
    }

    #[test]
    fn many_cores() {
        let cpu = structure(
            4,
            0x32,
            &[(0x10, &[0]), (0x23, &[0xff]), (0x25, &[0xff]), (0x2a, &300u16.to_le_bytes()), (0x2e, &600u16.to_le_bytes())],
            &[],
        );
        let info = parse(&cpu);
        assert_eq!((info.cpu_cores, info.cpu_threads), (300, 600));
        assert_eq!(info.cpu_name, "");
    }

    #[test]
    fn structures_and_strings() {
        let t = table();
        let s = structures(&t);
        assert_eq!(s.len(), 12, "stops after the end-of-table structure");
        assert_eq!(s[0].string(4), "Firmware Inc.");
        // Index 0 means no string, out-of-range indices give "".
        assert_eq!(s[0].string(5), "");
        assert_eq!(s[0].string(0x30), "");
        assert_eq!(s[1].string(8), "");
        assert_eq!(s[3].word(0x14), 4800);
    }

    #[test]
    fn broken_tables() {
        assert_eq!(structures(&[]).len(), 0);
        assert_eq!(structures(&[1, 2, 0, 0]).len(), 0, "length below the header size");
        assert_eq!(structures(&[1, 40, 0, 0]).len(), 0, "length past the data");
        // Unterminated strings: the structure is kept, the strings are not.
        let s = structures(&[1, 4, 0, 0, b'a', b'b']);
        assert_eq!(s.len(), 1);
        assert!(s[0].strings.is_empty());
        let info = parse(&[1, 0x1b, 0, 0]);
        assert_eq!(info.system_vendor, "");
    }
}
