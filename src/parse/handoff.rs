// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Handing unlocked LUKS volumes to Linux, so that the initrd doesn't ask
//! for the passphrase again:
//!
//! - which volumes a kernel command line needs (`rd.luks.uuid=`,
//!   `rd.luks.name=`, `cryptdevice=`, by LUKS or partition UUID);
//! - a small extra initrd (a "newc" cpio archive, which the kernel unpacks
//!   before the others) with the volume key and a systemd unit that opens the
//!   volume with it (`cryptsetup open --volume-key-file`): no second key
//!   derivation. The passphrase is there too, as a key file
//!   (`/etc/cryptsetup-keys.d/<name>.key`, which systemd-cryptsetup looks
//!   for; `cryptkey=rootfs:` for the `encrypt` hook), for initrds without
//!   the `cryptsetup` program.
//!
//! The initrd lives in memory only; systemd empties it when it switches to
//! the real root.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// How the initrd opens the volume.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Style {
    /// systemd (`rd.luks.uuid=`, `rd.luks.name=`): dracut, mkinitcpio's
    /// `sd-encrypt`, …
    Systemd,
    /// mkinitcpio's `encrypt` hook (`cryptdevice=`).
    Encrypt,
}

/// A volume the command line asks the initrd to open.
#[derive(Debug, Clone, PartialEq)]
pub struct Wanted {
    /// Lowercase.
    pub uuid: String,
    /// `uuid` is the GPT partition's (`cryptdevice=PARTUUID=`), not the
    /// LUKS header's.
    pub partition: bool,
    /// Named by Linux's device name instead (`cryptdevice=/dev/nvme0n1p2`):
    /// the partition's number, `uuid` empty. The firmware doesn't know
    /// Linux's disk names: the partition with that number on the disks
    /// Slingshot trusts (the kernel's, its own).
    pub number: Option<u32>,
    /// The name of the opened volume (`/dev/mapper/<name>`).
    pub name: String,
    pub style: Style,
}

impl Wanted {
    /// Whether it's the volume with this LUKS UUID, on this GPT partition.
    pub fn matches(&self, luks_uuid: &str, partuuid: Option<&str>, number: Option<u32>) -> bool {
        if self.number.is_some() {
            self.number == number
        } else if self.partition {
            partuuid.is_some_and(|p| p.eq_ignore_ascii_case(&self.uuid))
        } else {
            luks_uuid.eq_ignore_ascii_case(&self.uuid)
        }
    }
}

/// A device mapper name that's also a plain file name: letters, digits and
/// `_ . + -`, not starting with a dot.
fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && !name.starts_with('.')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b))
}

fn options(cmdline: &str) -> impl Iterator<Item = (&str, &str)> {
    cmdline.split_ascii_whitespace().map(|o| o.split_once('=').unwrap_or((o, "")))
}

/// The LUKS volumes `cmdline` opens, in order, each once.
pub fn wanted_volumes(cmdline: &str) -> Vec<Wanted> {
    let mut wanted: Vec<Wanted> = Vec::new();
    // `cryptdevice=/dev/…`: by partition number, after the others.
    let (mut by_number, mut numbered): (Vec<Wanted>, Vec<u32>) = (Vec::new(), Vec::new());
    // `name`: given (`rd.luks.name=`, `cryptdevice=`), else `luks-<uuid>`. As
    // in systemd, a given name wins over `rd.luks.uuid=`'s, in any order.
    let mut add = |uuid: &str, partition, name: Option<&str>, style| {
        let uuid = uuid.to_ascii_lowercase();
        // They go into file names and a unit file of the initrd: a name
        // like "../../init" mustn't write elsewhere.
        if uuid.len() != 36 || !uuid.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') || !name.is_none_or(valid_name) {
            return;
        }
        match wanted.iter_mut().find(|w| w.uuid == uuid) {
            Some(w) => {
                if let Some(name) = name {
                    w.name = name.into();
                }
            }
            None => {
                let name = name.map_or_else(|| format!("luks-{uuid}"), String::from);
                wanted.push(Wanted { uuid, partition, number: None, name, style });
            }
        }
    };
    for (key, value) in options(cmdline) {
        match key {
            "rd.luks" | "luks" if value == "0" || value == "no" => return Vec::new(),
            "rd.luks.name" | "luks.name" => {
                if let Some((uuid, name)) = value.split_once('=') {
                    add(uuid.trim_start_matches("UUID="), false, Some(name), Style::Systemd);
                }
            }
            "rd.luks.uuid" | "luks.uuid" => {
                let uuid = value.trim_start_matches("luks-").trim_start_matches("UUID=");
                add(uuid, false, None, Style::Systemd);
            }
            "cryptdevice" => {
                let mut parts = value.split(':');
                let device = parts.next().unwrap_or_default();
                let name = parts.next().unwrap_or("cryptroot");
                let uuid = device.strip_prefix("UUID=").or_else(|| device.strip_prefix("/dev/disk/by-uuid/"));
                let partuuid = device.strip_prefix("PARTUUID=").or_else(|| device.strip_prefix("/dev/disk/by-partuuid/"));
                if let Some(uuid) = uuid {
                    add(uuid, false, Some(name), Style::Encrypt);
                } else if let Some(uuid) = partuuid {
                    add(uuid, true, Some(name), Style::Encrypt);
                } else if let Some(number) = kernel_partition_number(device)
                    && valid_name(name)
                    && !numbered.contains(&number)
                {
                    numbered.push(number);
                    by_number.push(Wanted { uuid: String::new(), partition: false, number: Some(number), name: name.into(), style: Style::Encrypt });
                }
            }
            _ => {}
        }
    }
    wanted.extend(by_number);
    wanted
}

/// The partition number in a Linux device name: `/dev/nvme0n1p2`,
/// `/dev/mmcblk0p2` → 2, `/dev/sda2`, `/dev/vdb10` → 2, 10.
fn kernel_partition_number(device: &str) -> Option<u32> {
    let name = device.strip_prefix("/dev/")?;
    let digits = name.len() - name.bytes().rev().take_while(u8::is_ascii_digit).count();
    let (disk, number) = name.split_at(digits);
    let disk_ok = if let Some(d) = disk.strip_suffix('p') {
        // nvme0n1, mmcblk0: a digit before the p.
        (d.starts_with("nvme") || d.starts_with("mmcblk")) && d.ends_with(|c: char| c.is_ascii_digit())
    } else {
        ["sd", "vd", "hd", "xvd"].iter().any(|p| disk.strip_prefix(p).is_some_and(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_lowercase())))
    };
    number.parse().ok().filter(|n| disk_ok && *n > 0)
}

/// Whether a Linux kernel loads its initrd through the
/// LINUX_EFI_INITRD_MEDIA protocol (5.8 and later), from the start of its
/// file: the PE header's major image version is the EFI stub's version
/// (include/linux/pe.h). 1 and 2 have it; from 3 on, it's a feature mask
/// where 0x1 is this. Older kernels (0) only read `initrd=`.
pub fn takes_initrd_media(image: &[u8]) -> bool {
    let u32_at = |at: usize| Some(u32::from_le_bytes(image.get(at..at + 4)?.try_into().ok()?));
    let Some(pe) = u32_at(0x3c).filter(|_| image.starts_with(b"MZ")) else { return false };
    let pe = pe as usize;
    // The signature, the COFF header (20 bytes), then the optional header
    // with MajorImageVersion at 44 (in both PE32 and PE32+).
    let Some(major) = image.get(pe + 68..pe + 70) else { return false };
    let major = u16::from_le_bytes([major[0], major[1]]);
    image.get(pe..pe + 4) == Some(b"PE\0\0") && (major == 2 || major & 1 == 1)
}

/// `cryptsetup open` flags for the options the initrd would have used
/// (`rd.luks.options=`, for all volumes or `<uuid>=` one), when they change
/// how the volume is opened.
pub fn open_flags(cmdline: &str, uuid: &str) -> Vec<&'static str> {
    let mut flags = Vec::new();
    for (key, value) in options(cmdline) {
        if key != "rd.luks.options" && key != "luks.options" {
            continue;
        }
        // `<uuid>=a,b` for one volume, else `a,b` (which can have `=`s).
        let value = value.trim_start_matches("UUID=");
        let list = match value.split_once('=') {
            Some((for_uuid, list)) if for_uuid.len() == 36 && !for_uuid.contains(',') => {
                if !for_uuid.eq_ignore_ascii_case(uuid) {
                    continue;
                }
                list
            }
            _ => value,
        };
        for option in list.split(',') {
            let flag = match option {
                "discard" => "--allow-discards",
                "no-read-workqueue" => "--perf-no_read_workqueue",
                "no-write-workqueue" => "--perf-no_write_workqueue",
                "same-cpu-crypt" => "--perf-same_cpu_crypt",
                "submit-from-crypt-cpus" => "--perf-submit_from_crypt_cpus",
                "readonly" | "read-only" => "--readonly",
                _ => continue,
            };
            if !flags.contains(&flag) {
                flags.push(flag);
            }
        }
    }
    flags
}

/// Splits the `initrd=` options off: the initrds go through the
/// LINUX_EFI_INITRD_MEDIA protocol instead, with the extra one after them.
pub fn split_initrds(cmdline: &str) -> (Vec<String>, String) {
    let mut initrds = Vec::new();
    let mut rest = Vec::new();
    for option in cmdline.split_ascii_whitespace() {
        match option.strip_prefix("initrd=") {
            Some(path) => initrds.push(path.replace('/', "\\")),
            None => rest.push(option),
        }
    }
    (initrds, rest.join(" "))
}

/// A unit name part, as `systemd-escape` makes it (`path`: as
/// `systemd-escape --path`).
pub fn systemd_escape(s: &str, path: bool) -> String {
    let s = if path { s.trim_matches('/') } else { s };
    let mut out = String::new();
    for (i, c) in s.bytes().enumerate() {
        match c {
            b'/' => out.push('-'),
            b'.' if i == 0 => out.push_str("\\x2e"),
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b':' | b'_' | b'.' => out.push(c as char),
            _ => out.push_str(&format!("\\x{c:02x}")),
        }
    }
    out
}

/// A "newc" cpio archive, as the kernel unpacks an initramfs.
#[derive(Default)]
pub struct Cpio {
    data: Vec<u8>,
    inode: u32,
}

impl Cpio {
    fn entry(&mut self, name: &str, mode: u32, data: &[u8]) {
        // Room first: a reallocation would leave a copy of the keys in the
        // freed buffer.
        self.reserve(110 + name.len() + 4 + data.len() + 3);
        self.inode += 1;
        let fields = [self.inode, mode, 0, 0, 1, 0, data.len() as u32, 0, 0, 0, 0, name.len() as u32 + 1, 0];
        self.data.extend_from_slice(b"070701");
        for field in fields {
            self.data.extend_from_slice(format!("{field:08X}").as_bytes());
        }
        self.data.extend_from_slice(name.as_bytes());
        self.data.push(0);
        self.pad();
        self.data.extend_from_slice(data);
        self.pad();
    }

    /// Makes room for `extra` bytes, wiping the old buffer when it moves.
    fn reserve(&mut self, extra: usize) {
        if self.data.capacity() - self.data.len() >= extra {
            return;
        }
        let mut bigger = Vec::with_capacity((self.data.len() + extra).max(2 * self.data.capacity()));
        bigger.extend_from_slice(&self.data);
        zeroize::Zeroize::zeroize(&mut self.data);
        self.data = bigger;
    }

    fn pad(&mut self) {
        self.data.resize(self.data.len().next_multiple_of(4), 0);
    }

    pub fn dir(&mut self, name: &str) {
        self.entry(name, 0o040755, &[]);
    }

    pub fn file(&mut self, name: &str, mode: u32, data: &[u8]) {
        self.entry(name, 0o100000 | mode, data);
    }

    pub fn symlink(&mut self, name: &str, target: &str) {
        self.entry(name, 0o120777, target.as_bytes());
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.entry("TRAILER!!!", 0, &[]);
        core::mem::take(&mut self.data)
    }
}

impl Drop for Cpio {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.data);
    }
}

/// An unlocked volume to hand over.
pub struct Unlocked<'a> {
    pub wanted: &'a Wanted,
    pub volume_key: &'a [u8],
    pub passphrase: &'a [u8],
}

/// The extra initrd for `volumes`, and what to add to the command line.
pub fn archive(volumes: &[Unlocked], cmdline: &str) -> (Vec<u8>, String) {
    let mut cpio = Cpio::default();
    let mut extra = Vec::new();
    for dir in ["etc", "etc/cryptsetup-keys.d", "etc/slingshot", "etc/systemd", "etc/systemd/system"] {
        cpio.dir(dir);
    }
    for (i, v) in volumes.iter().enumerate() {
        let name = &v.wanted.name;
        let pass = format!("/etc/cryptsetup-keys.d/{name}.key");
        cpio.file(&pass[1..], 0o400, v.passphrase);
        match v.wanted.style {
            Style::Encrypt => extra.push(format!("cryptkey=rootfs:{pass}")),
            Style::Systemd => {
                let key = format!("/etc/slingshot/{i}.key");
                cpio.file(&key[1..], 0o400, v.volume_key);
                let uuid = &v.wanted.uuid;
                let device = systemd_escape(&format!("/dev/disk/by-uuid/{uuid}"), true) + ".device";
                let cryptsetup = format!("systemd-cryptsetup@{}.service", systemd_escape(name, false));
                let mut flags = open_flags(cmdline, uuid).join(" ");
                if !flags.is_empty() {
                    flags.push(' ');
                }
                let unit = format!("slingshot-unlock-{i}.service");
                let text = format!(
                    "# Opens {name} with the volume key Slingshot found from the passphrase.\n\
                     # Without the cryptsetup program, {cryptsetup} uses the\n\
                     # passphrase in {pass}.\n\
                     [Unit]\n\
                     Description=Unlock {name} (Slingshot)\n\
                     DefaultDependencies=no\n\
                     BindsTo={device}\n\
                     After={device}\n\
                     Before={cryptsetup}\n\
                     \n\
                     [Service]\n\
                     Type=oneshot\n\
                     RemainAfterExit=yes\n\
                     ExecStart=-cryptsetup open --type luks2 {flags}--volume-key-file={key} UUID={uuid} {name}\n\
                     ExecStartPost=-rm -f {key}\n"
                );
                cpio.file(&format!("etc/systemd/system/{unit}"), 0o644, text.as_bytes());
                let wants = format!("etc/systemd/system/{cryptsetup}.wants");
                cpio.dir(&wants);
                cpio.symlink(&format!("{wants}/{unit}"), &format!("../{unit}"));
                // The passphrase only stays until the disk is open: else an
                // emergency shell in the initrd could read it.
                let dropin = format!("etc/systemd/system/{cryptsetup}.d");
                cpio.dir(&dropin);
                let text = format!("[Service]\nExecStartPost=-rm -f {pass}\n");
                cpio.file(&format!("{dropin}/slingshot.conf"), 0o644, text.as_bytes());
            }
        }
    }
    (cpio.finish(), extra.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    const U: &str = "0b7e5c0a-8d3f-4b0f-9a52-2f1c6c9d4e11";

    #[test]
    fn kernel_device_names() {
        let w = wanted_volumes("cryptdevice=/dev/nvme0n1p2:crypt root=/dev/mapper/crypt rw");
        assert_eq!(w, [Wanted { uuid: String::new(), partition: false, number: Some(2), name: "crypt".into(), style: Style::Encrypt }]);
        assert!(w[0].matches("any", None, Some(2)) && !w[0].matches("any", None, Some(3)));
        for (device, number) in [("/dev/sda3", Some(3)), ("/dev/mmcblk0p1", Some(1)), ("/dev/vdb10", Some(10))] {
            assert_eq!(kernel_partition_number(device), number);
        }
        for device in ["/dev/sda", "/dev/nvme0n1", "/dev/mapper/vg-root", "/dev/md0", "sda2", "/dev/loop0p1"] {
            assert_eq!(kernel_partition_number(device), None, "{device}");
        }
    }

    #[test]
    fn volumes_from_the_command_line() {
        let w = wanted_volumes(&format!("root=/dev/mapper/root rd.luks.name={U}=root rw"));
        assert_eq!(w, [Wanted { uuid: U.into(), partition: false, number: None, name: "root".into(), style: Style::Systemd }]);
        let w = wanted_volumes(&format!("rd.luks.uuid=luks-{} rd.luks.uuid={U}", U.to_uppercase()));
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].name, format!("luks-{U}"));
        // rd.luks.name= names it, before or after rd.luks.uuid=.
        for line in [format!("rd.luks.uuid={U} rd.luks.name={U}=root"), format!("rd.luks.name={U}=root rd.luks.uuid={U}")] {
            assert_eq!(wanted_volumes(&line)[0].name, "root");
        }
        let w = wanted_volumes(&format!("cryptdevice=UUID={U}:cryptroot:allow-discards root=/dev/mapper/cryptroot"));
        assert_eq!(w, [Wanted { uuid: U.into(), partition: false, number: None, name: "cryptroot".into(), style: Style::Encrypt }]);
        let w = wanted_volumes(&format!("cryptdevice=/dev/disk/by-uuid/{U}:vg"));
        assert_eq!(w[0].name, "vg");
        // The partition's GUID, as the Arch wiki often has it.
        let w = wanted_volumes(&format!("cryptdevice=PARTUUID={}:root", U.to_uppercase()));
        assert_eq!(w, [Wanted { uuid: U.into(), partition: true, number: None, name: "root".into(), style: Style::Encrypt }]);
        assert!(wanted_volumes(&format!("cryptdevice=/dev/disk/by-partuuid/{U}:root"))[0].partition);
        assert!(wanted_volumes(&format!("rd.luks.uuid={U} rd.luks=0")).is_empty());
        assert!(wanted_volumes("root=UUID=1234 rw").is_empty());
        // Names that would be other paths in the initrd.
        for name in ["../../init", ".hidden", "a/b", "a\0b", "a%b"] {
            assert!(wanted_volumes(&format!("rd.luks.name={U}={name}")).is_empty(), "{name}");
        }
        assert!(wanted_volumes("rd.luks.uuid=../../../../../../etc/passwd-0000000000").is_empty());
    }

    #[test]
    fn initrd_media() {
        let image = |major: u8| {
            let mut image = alloc::vec![0u8; 0x200];
            image[..2].copy_from_slice(b"MZ");
            image[0x3c] = 0x80;
            image[0x80..0x84].copy_from_slice(b"PE\0\0");
            image[0x80 + 68] = major;
            image
        };
        assert!(takes_initrd_media(&image(1)));
        assert!(takes_initrd_media(&image(2)));
        assert!(takes_initrd_media(&image(3)), "a feature mask, 0x1 is set");
        assert!(!takes_initrd_media(&image(0)), "before 5.8");
        assert!(!takes_initrd_media(&image(4)), "a feature mask without 0x1");
        assert!(!takes_initrd_media(&image(1)[..0x80]), "cut short");
        assert!(!takes_initrd_media(b"not a kernel"));
    }

    #[test]
    fn flags() {
        let line = format!("rd.luks.options=discard,timeout=0 rd.luks.options={U}=no-read-workqueue");
        assert_eq!(open_flags(&line, U), ["--allow-discards", "--perf-no_read_workqueue"]);
        assert_eq!(open_flags(&line, "other"), ["--allow-discards"]);
    }

    #[test]
    fn initrds() {
        let (initrds, rest) = split_initrds("initrd=\\intel-ucode.img initrd=/initramfs-linux.img root=x rw");
        assert_eq!(initrds, ["\\intel-ucode.img", "\\initramfs-linux.img"]);
        assert_eq!(rest, "root=x rw");
    }

    #[test]
    fn escape() {
        assert_eq!(systemd_escape(&format!("/dev/disk/by-uuid/{U}"), true), format!("dev-disk-by\\x2duuid-{}", U.replace('-', "\\x2d")));
        assert_eq!(systemd_escape("luks-ab", false), "luks\\x2dab");
        assert_eq!(systemd_escape(".x y", false), "\\x2ex\\x20y");
    }

    #[test]
    fn cpio_layout() {
        let mut c = Cpio::default();
        c.file("a", 0o400, b"xyz");
        let data = c.finish();
        assert_eq!(&data[..6], b"070701");
        // Header (110) + "a\0" padded to 112, then 3 bytes padded to 4.
        assert_eq!(&data[112..115], b"xyz");
        assert_eq!(data.len() % 4, 0);
        assert!(data.windows(10).any(|w| w == b"TRAILER!!!"));
    }

    #[test]
    fn archive_contents() {
        let wanted = wanted_volumes(&format!("rd.luks.name={U}=root"));
        let v = [Unlocked { wanted: &wanted[0], volume_key: &[7; 64], passphrase: b"pw" }];
        let (data, extra) = archive(&v, "rd.luks.options=discard");
        assert!(extra.is_empty());
        let text = String::from_utf8_lossy(&data);
        assert!(text.contains("etc/cryptsetup-keys.d/root.key"));
        assert!(text.contains("cryptsetup open --type luks2 --allow-discards --volume-key-file=/etc/slingshot/0.key"));
        assert!(text.contains("systemd-cryptsetup@root.service.wants/slingshot-unlock-0.service"));
        assert!(text.contains("systemd-cryptsetup@root.service.d/slingshot.conf"));
        assert!(text.contains("ExecStartPost=-rm -f /etc/cryptsetup-keys.d/root.key"));
        let wanted = wanted_volumes(&format!("cryptdevice=UUID={U}:cryptroot"));
        let v = [Unlocked { wanted: &wanted[0], volume_key: &[7; 64], passphrase: b"pw" }];
        assert_eq!(archive(&v, "").1, "cryptkey=rootfs:/etc/cryptsetup-keys.d/cryptroot.key");
    }
}
