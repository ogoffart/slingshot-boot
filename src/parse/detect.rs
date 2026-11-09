// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Finds what a file system can start, like rEFInd does: macOS, Windows,
//! and Linux kernels with their initrd and command line. The firmware calls
//! [`scan`] on each file system it can read (FAT, and others with drivers).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::bls::version_cmp;

/// A file system, as [`scan`] reads it. Paths use backslashes, from its root.
pub trait Disk {
    /// The entries of a folder: name, and whether it's a folder.
    fn list(&self, dir: &str) -> Option<Vec<(String, bool)>>;
    fn read(&self, path: &str) -> Option<Vec<u8>>;
}

/// Something found to start.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub title: String,
    pub subtitle: String,
    /// The icon: "apple", "windows" or "linux".
    pub kind: &'static str,
    /// The program, from the file system's root.
    pub program: String,
    /// Its command line (a kernel's: initrds, then options).
    pub options: String,
    /// A kernel's version, for its subtitle (translated by the caller).
    pub version: String,
}

/// The entry of `dir` named `name`, ignoring case (FAT ignores it, ext4
/// doesn't): its real name.
fn find(entries: &[(String, bool)], name: &str, is_dir: bool) -> Option<String> {
    entries.iter().find(|(n, d)| *d == is_dir && n.eq_ignore_ascii_case(name)).map(|(n, _)| n.clone())
}

/// The path `parts` under the root, with each part's real case, if it all
/// exists (the last part is a file).
fn resolve(disk: &dyn Disk, parts: &[&str]) -> Option<String> {
    let mut path = String::new();
    for (i, part) in parts.iter().enumerate() {
        let entries = disk.list(if path.is_empty() { "\\" } else { &path })?;
        let name = find(&entries, part, i + 1 < parts.len())?;
        path = format!("{path}\\{name}");
    }
    Some(path)
}

/// The folder `parts` under the root, with each part's real case, if it
/// exists.
fn resolve_dir(disk: &dyn Disk, parts: &[&str]) -> Option<String> {
    let mut path = String::new();
    for part in parts {
        let entries = disk.list(if path.is_empty() { "\\" } else { &path })?;
        path = format!("{path}\\{}", find(&entries, part, true)?);
    }
    Some(path)
}

/// `<key>name</key><string>value</string>` in a property list.
fn plist_string(plist: &str, key: &str) -> Option<String> {
    let after = &plist[plist.find(&format!("<key>{key}</key>"))?..];
    let start = after.find("<string>")? + "<string>".len();
    let end = after[start..].find("</string>")?;
    Some(after[start..start + end].trim().to_string())
}

/// macOS's boot loader in `prefix` (the volume's root, or a system's folder
/// on an APFS Preboot volume), with its version.
fn macos(disk: &dyn Disk, prefix: &[&str]) -> Option<Found> {
    let core = [prefix, &["System", "Library", "CoreServices"]].concat();
    let program = resolve(disk, &[core.as_slice(), &["boot.efi"]].concat())?;
    let version = resolve(disk, &[core.as_slice(), &["SystemVersion.plist"]].concat())
        .and_then(|p| disk.read(&p))
        .and_then(|data| plist_string(&String::from_utf8_lossy(&data), "ProductUserVisibleVersion")
            .or_else(|| plist_string(&String::from_utf8_lossy(&data), "ProductVersion")));
    Some(Found {
        title: match &version {
            Some(v) => format!("macOS {v}"),
            None => String::from("macOS"),
        },
        subtitle: String::from("macOS"),
        kind: "apple",
        program,
        options: String::new(),
        version: String::new(),
    })
}

/// An APFS system's folder on the Preboot volume: a UUID.
fn is_uuid(name: &str) -> bool {
    name.len() == 36 && name.bytes().enumerate().all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == b'-' } else { c.is_ascii_hexdigit() })
}

/// `KEY="value"` in os-release.
fn os_release(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let value = l.strip_prefix(key)?.strip_prefix('=')?;
        Some(value.trim().trim_matches('"').trim_matches('\'').to_string()).filter(|v| !v.is_empty())
    })
}

/// How a distribution's own boot configuration starts a kernel: its
/// arguments (without the initrds) and initrds, and the menu's title.
#[derive(Clone, Debug, Default, PartialEq)]
struct Configured {
    title: String,
    args: String,
    /// From the file system's root, with backslashes.
    initrds: Vec<String>,
}

/// Arguments the boot loader fills in itself (`$kernelopts`, `$vt_handoff`,
/// `BOOT_IMAGE=`): left out.
fn clean_args<'a>(args: impl Iterator<Item = &'a str>) -> String {
    args.filter(|a| !a.starts_with('$') && !a.starts_with("BOOT_IMAGE=")).collect::<Vec<_>>().join(" ")
}

/// A kernel's file name, from a path in a boot loader's configuration.
fn base_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// GRUB's `grub.cfg`: for each kernel (by file name), how the first menu
/// entry that starts it does. Paths in it are from the partition's root.
fn grub_entries(cfg: &str) -> Vec<(String, Configured)> {
    let mut found: Vec<(String, Configured)> = Vec::new();
    let mut title = String::new();
    let mut current: Option<(String, Configured)> = None;
    for line in cfg.lines().map(str::trim) {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("menuentry") => {
                // menuentry 'Ubuntu' --class ubuntu … {
                let rest = line["menuentry".len()..].trim_start();
                let quote = rest.chars().next().filter(|c| *c == '\'' || *c == '"');
                title = match quote {
                    Some(q) => rest[1..].split(q).next().unwrap_or("").to_string(),
                    None => rest.split_whitespace().next().unwrap_or("").to_string(),
                };
            }
            Some("linux" | "linuxefi") => {
                if let Some(path) = words.next() {
                    let kernel = base_name(path).to_string();
                    current = Some((kernel, Configured { title: title.clone(), args: clean_args(words), initrds: Vec::new() }));
                }
            }
            Some("initrd" | "initrdefi") => {
                if let Some((_, c)) = &mut current {
                    c.initrds = words.map(|p| p.replace('/', "\\")).collect();
                }
            }
            Some("}") => {
                if let Some((kernel, c)) = current.take()
                    && !found.iter().any(|(k, _)| *k == kernel)
                {
                    found.push((kernel, c));
                }
            }
            _ => {}
        }
    }
    found
}

/// A Boot Loader Specification entry in a root file system's
/// `boot/loader/entries` (Fedora without a separate `/boot`): its kernel's
/// file name and how it starts it. Its paths are from `/boot`, under `dir`.
fn bls_entry(text: &str, dir: &str) -> Option<(String, Configured)> {
    let (entry, _) = super::bls::parse_entry("entry.conf", text);
    let kernel = base_name(&entry.program).to_string();
    let mut initrds = Vec::new();
    let mut args = Vec::new();
    for part in entry.options.split_whitespace() {
        match part.strip_prefix("initrd=") {
            Some(i) => initrds.push(format!("{dir}{i}")),
            None => args.push(part),
        }
    }
    (!kernel.is_empty()).then(|| (kernel, Configured { title: entry.title, args: clean_args(args.into_iter()), initrds }))
}

/// The Linux kernels in `\boot` (a root file system) or at the root (a
/// separate `/boot` partition), newest first, with their initramfs. The
/// command line, in this order: `\etc\kernel\cmdline`; how the
/// distribution's GRUB (`grub.cfg`) or Boot Loader Specification entries
/// start that kernel; else `root=PARTUUID=` of this partition when it's the
/// root file system. A kernel none of these covers is left out: its root
/// file system is unknown.
fn linux(disk: &dyn Disk, partuuid: Option<&str>) -> Vec<Found> {
    let root = disk.list("\\").unwrap_or_default();
    let boot_dir = find(&root, "boot", true);
    let (dir, entries) = match &boot_dir {
        Some(b) => (format!("\\{b}"), disk.list(&format!("\\{b}")).unwrap_or_default()),
        None => (String::new(), root.clone()),
    };
    let is_root_fs = boot_dir.is_some() && find(&root, "etc", true).is_some();
    let text = |parts: &[&str]| resolve(disk, parts).and_then(|p| disk.read(&p)).map(|d| String::from_utf8_lossy(&d).into_owned());
    let cmdline = text(&["etc", "kernel", "cmdline"])
        .map(|c| c.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|c| !c.is_empty());
    let fallback = partuuid.filter(|_| is_root_fs).map(|u| format!("root=PARTUUID={u} ro"));
    let os_name = text(&["etc", "os-release"])
        .or_else(|| text(&["usr", "lib", "os-release"]))
        .and_then(|t| os_release(&t, "PRETTY_NAME").or_else(|| os_release(&t, "NAME")));

    // What the distribution's boot loader configuration says, per kernel.
    let boot_parts: Vec<&str> = boot_dir.iter().map(String::as_str).collect();
    let mut configured: Vec<(String, Configured)> = Vec::new();
    for grub in ["grub", "grub2"] {
        if let Some(cfg) = text(&[boot_parts.as_slice(), &[grub, "grub.cfg"]].concat()) {
            configured.extend(grub_entries(&cfg));
        }
    }
    if is_root_fs {
        let dir_path = resolve_dir(disk, &[boot_parts.as_slice(), &["loader", "entries"]].concat()).unwrap_or_default();
        for (name, is_dir) in disk.list(&dir_path).into_iter().flatten().filter(|_| !dir_path.is_empty()) {
            if !is_dir && super::bls::ends_with_ignore_case(&name, ".conf")
                && let Some(text) = disk.read(&format!("{dir_path}\\{name}"))
                && let Some(entry) = bls_entry(&String::from_utf8_lossy(&text), &dir)
            {
                configured.push(entry);
            }
        }
    }

    let file = |name: &str| find(&entries, name, false);
    let mut kernels: Vec<(String, String)> = entries
        .iter()
        .filter(|(_, d)| !d)
        .filter_map(|(n, _)| {
            let version = n.strip_prefix("vmlinuz-")?;
            let skip = [".old", ".bak", ".sig", ".signed"].iter().any(|s| version.ends_with(s));
            (!version.is_empty() && !skip).then(|| (n.clone(), version.to_string()))
        })
        .collect();
    kernels.sort_by(|a, b| version_cmp(&b.1, &a.1));
    // Microcode first (Arch), then the kernel's initramfs.
    let microcode: Vec<String> = ["intel-ucode.img", "amd-ucode.img"].iter().filter_map(|m| file(m)).collect();
    kernels
        .into_iter()
        .filter_map(|(kernel, version)| {
            let config = configured.iter().find(|(k, _)| *k == kernel).map(|(_, c)| c);
            let args = cmdline.clone().or_else(|| config.map(|c| c.args.clone())).or_else(|| fallback.clone())?;
            // The configuration's initrds when they're all there, else the
            // kernel's initramfs by its usual names.
            let exists = |path: &str| {
                let (folder, name) = path.rsplit_once('\\').unwrap_or(("", path));
                disk.list(if folder.is_empty() { "\\" } else { folder }).is_some_and(|e| e.iter().any(|(n, d)| !d && n == name))
            };
            let initrds = match config.filter(|c| !c.initrds.is_empty() && c.initrds.iter().all(|i| exists(i))) {
                Some(c) => c.initrds.clone(),
                None => {
                    let initrd = [
                        format!("initramfs-{version}.img"),
                        format!("initrd.img-{version}"),
                        format!("initrd-{version}.img"),
                        format!("initrd-{version}"),
                        format!("initramfs-{version}"),
                    ]
                    .iter()
                    .find_map(|n| file(n))?;
                    microcode.iter().chain(core::iter::once(&initrd)).map(|i| format!("{dir}\\{i}")).collect()
                }
            };
            // The system's name; without os-release (a /boot partition),
            // the boot menu's ("Ubuntu, with Linux 6.8…" → "Ubuntu").
            let title = os_name
                .clone()
                .or_else(|| config.map(|c| c.title.split(", with Linux").next().unwrap_or("").trim().to_string()))
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| String::from("Linux"));
            Some(Found {
                title,
                subtitle: format!("Kernel {version}"),
                kind: "linux",
                program: format!("{dir}\\{kernel}"),
                options: super::bls::command_line(&initrds, &args),
                version,
            })
        })
        .collect()
}

/// What `disk` can start. `partuuid` is the partition's GUID, for a kernel's
/// `root=`.
pub fn scan(disk: &dyn Disk, partuuid: Option<&str>) -> Vec<Found> {
    let mut found = Vec::new();
    // macOS: on its volume (HFS+, older APFS), or on the APFS Preboot
    // volume, in a folder per system.
    if let Some(m) = macos(disk, &[]) {
        found.push(m);
    }
    for (name, is_dir) in disk.list("\\").unwrap_or_default() {
        if is_dir && is_uuid(&name)
            && let Some(m) = macos(disk, &[name.as_str()])
        {
            found.push(m);
        }
    }
    if let Some(program) = resolve(disk, &["EFI", "Microsoft", "Boot", "bootmgfw.efi"]) {
        found.push(Found {
            title: String::from("Windows"),
            subtitle: String::from("Windows Boot Manager"),
            kind: "windows",
            program,
            options: String::new(),
            version: String::new(),
        });
    }
    found.extend(linux(disk, partuuid));
    found
}

/// Whether an entry already starts `program` on the file system `fs` (its
/// device path): `started` holds each entry's device path (empty when it
/// names no disk) and file. Paths compare as [`normalize_path`] gives them,
/// and the disk too, so a second Windows on another disk isn't a duplicate.
///
/// [`normalize_path`]: super::load_option::normalize_path
pub fn already_started(started: &[(Vec<u8>, String)], fs: &[u8], program: &str) -> bool {
    let file = super::load_option::normalize_path(program);
    let file = file.trim_start_matches('\\');
    started.iter().any(|(device_path, f)| {
        f.trim_start_matches('\\') == file && super::load_option::is_on_file_system(device_path, fs)
    })
}

/// The partition number in a device path (its hard drive node).
pub fn partition_number(device_path: &[u8]) -> Option<u32> {
    let (_, _, node) = super::load_option::nodes(device_path).into_iter().find(|(t, s, n)| (*t, *s) == (0x04, 0x01) && n.len() >= 8)?;
    Some(u32::from_le_bytes(node[4..8].try_into().ok()?))
}

/// The GUID of a GPT partition, from its device path's hard drive node, as
/// Linux writes it (`root=PARTUUID=`).
pub fn partition_uuid(device_path: &[u8]) -> Option<String> {
    // Hard drive: signature at 24, signature type (2: GUID) at 41.
    let (_, _, node) = super::load_option::nodes(device_path)
        .into_iter()
        .find(|(t, s, n)| (*t, *s) == (0x04, 0x01) && n.len() >= 42 && n[41] == 2)?;
    let g = &node[24..40];
    Some(format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        u32::from_le_bytes([g[0], g[1], g[2], g[3]]),
        u16::from_le_bytes([g[4], g[5]]),
        u16::from_le_bytes([g[6], g[7]]),
        g[8], g[9], g[10], g[11], g[12], g[13], g[14], g[15]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;
    use alloc::vec;

    /// A file system in memory: paths to contents ("" for a folder).
    struct Fake(BTreeMap<String, String>);

    impl Fake {
        fn new(files: &[(&str, &str)]) -> Self {
            Fake(files.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect())
        }
    }

    impl Disk for Fake {
        fn list(&self, dir: &str) -> Option<Vec<(String, bool)>> {
            let prefix = if dir == "\\" { String::from("\\") } else { format!("{dir}\\") };
            let mut out: Vec<(String, bool)> = Vec::new();
            for path in self.0.keys() {
                if let Some(rest) = path.strip_prefix(&prefix) {
                    let (name, is_dir) = match rest.split_once('\\') {
                        Some((n, _)) => (n, true),
                        None => (rest, false),
                    };
                    if !name.is_empty() && !out.iter().any(|(n, _)| n == name) {
                        out.push((name.to_string(), is_dir));
                    }
                }
            }
            (!out.is_empty() || dir == "\\").then_some(out)
        }
        fn read(&self, path: &str) -> Option<Vec<u8>> {
            self.0.get(path).map(|c| c.as_bytes().to_vec())
        }
    }

    const PLIST: &str = "<dict><key>ProductName</key><string>macOS</string>\
        <key>ProductUserVisibleVersion</key><string>14.5</string></dict>";

    #[test]
    fn macos_volumes() {
        let hfs = Fake::new(&[
            ("\\System\\Library\\CoreServices\\boot.efi", "x"),
            ("\\System\\Library\\CoreServices\\SystemVersion.plist", PLIST),
        ]);
        let found = scan(&hfs, None);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].title.as_str(), found[0].kind), ("macOS 14.5", "apple"));
        assert_eq!(found[0].program, "\\System\\Library\\CoreServices\\boot.efi");

        let uuid = "0A1B2C3D-0000-1111-2222-333344445555";
        let preboot = Fake::new(&[(&*format!("\\{uuid}\\System\\Library\\CoreServices\\boot.efi"), "x")]);
        let found = scan(&preboot, None);
        assert_eq!(found[0].title, "macOS");
        assert_eq!(found[0].program, format!("\\{uuid}\\System\\Library\\CoreServices\\boot.efi"));
        assert!(!is_uuid("not-a-uuid"));
    }

    #[test]
    fn windows_any_case() {
        let esp = Fake::new(&[("\\EFI\\MICROSOFT\\Boot\\BOOTMGFW.EFI", "x")]);
        let found = scan(&esp, None);
        assert_eq!((found[0].title.as_str(), found[0].program.as_str()), ("Windows", "\\EFI\\MICROSOFT\\Boot\\BOOTMGFW.EFI"));
    }

    #[test]
    fn linux_root_file_system() {
        let root = Fake::new(&[
            ("\\etc\\os-release", "NAME=\"Arch Linux\"\nPRETTY_NAME=\"Arch Linux\"\n"),
            ("\\boot\\vmlinuz-linux", "k"),
            ("\\boot\\initramfs-linux.img", "i"),
            ("\\boot\\initramfs-linux-fallback.img", "i"),
            ("\\boot\\intel-ucode.img", "u"),
            ("\\boot\\vmlinuz-linux-lts", "k"),
            ("\\boot\\initramfs-linux-lts.img", "i"),
            ("\\boot\\vmlinuz-nope", "no initramfs: left out"),
            ("\\vmlinuz", "a symlink on Debian: left out"),
        ]);
        let found = scan(&root, Some("1234"));
        let summary: Vec<(&str, &str)> = found.iter().map(|f| (f.program.as_str(), f.subtitle.as_str())).collect();
        assert_eq!(summary, [("\\boot\\vmlinuz-linux-lts", "Kernel linux-lts"), ("\\boot\\vmlinuz-linux", "Kernel linux")]);
        assert_eq!(found[1].title, "Arch Linux");
        assert_eq!(found[1].options, "initrd=\\boot\\intel-ucode.img initrd=\\boot\\initramfs-linux.img root=PARTUUID=1234 ro");
        // No partition GUID and no cmdline: nothing (the root is unknown).
        assert!(scan(&root, None).is_empty());
    }

    #[test]
    fn linux_cmdline_and_versions() {
        let root = Fake::new(&[
            ("\\etc\\kernel\\cmdline", "root=UUID=abcd rw quiet\n"),
            ("\\usr\\lib\\os-release", "NAME=Debian\n"),
            ("\\boot\\vmlinuz-6.9.0-1-amd64", "k"),
            ("\\boot\\initrd.img-6.9.0-1-amd64", "i"),
            ("\\boot\\vmlinuz-6.10.0-1-amd64", "k"),
            ("\\boot\\initrd.img-6.10.0-1-amd64", "i"),
            ("\\boot\\vmlinuz-6.8.0-1-amd64.old", "k"),
        ]);
        let found = scan(&root, Some("1234"));
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].subtitle, "Kernel 6.10.0-1-amd64");
        assert_eq!(found[0].title, "Debian");
        assert_eq!(found[0].options, "initrd=\\boot\\initrd.img-6.10.0-1-amd64 root=UUID=abcd rw quiet");

        // A separate /boot partition: kernels at its root, but the root file
        // system is unknown.
        let boot = Fake::new(&[("\\vmlinuz-6.10", "k"), ("\\initramfs-6.10.img", "i")]);
        assert!(scan(&boot, Some("1234")).is_empty());
    }

    #[test]
    fn partition_guids() {
        // PCI, then a GPT hard drive node, then the end.
        let mut hd = vec![0x04, 0x01, 42, 0, 1, 0, 0, 0];
        hd.extend_from_slice(&[0; 16]);
        hd.extend_from_slice(&[0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x77, 0x66, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        hd.extend_from_slice(&[2, 2]);
        let dp = [vec![0x01, 0x01, 6, 0, 0, 0x1f], hd, vec![0x7f, 0xff, 4, 0]].concat();
        assert_eq!(partition_uuid(&dp).as_deref(), Some("00112233-4455-6677-8899-aabbccddeeff"));
        assert_eq!(partition_uuid(&[0x7f, 0xff, 4, 0]), None);
        assert_eq!(partition_number(&dp), Some(1));
        assert_eq!(partition_number(&[0x7f, 0xff, 4, 0]), None);
    }

    #[test]
    fn duplicates_on_the_same_disk_only() {
        // A partition (HD node: number, start, size, GUID) and the end node.
        let partition = |n: u8| {
            let mut hd = vec![0x04, 0x01, 42, 0, n, 0, 0, 0];
            hd.extend_from_slice(&[n; 34]);
            [vec![0x01, 0x01, 6, 0, 0, 0x1f], hd, vec![0x7f, 0xff, 4, 0]].concat()
        };
        let file_node = |p: &str| {
            let name = crate::parse::ucs2_bytes(p);
            [vec![0x04, 0x04, (4 + name.len()) as u8, 0], name].concat()
        };
        let (disk1, disk2) = (partition(1), partition(2));
        // The firmware's Windows entry, short form: the HD node, then the file.
        let entry = [disk1[6..48].to_vec(), file_node("\\EFI\\Microsoft\\Boot\\bootmgfw.efi"), vec![0x7f, 0xff, 4, 0]].concat();
        let started = [(entry, String::from("\\efi\\microsoft\\boot\\bootmgfw.efi"))];
        let windows = "\\EFI\\Microsoft\\Boot\\bootmgfw.efi";
        assert!(already_started(&started, &disk1, windows));
        assert!(!already_started(&started, &disk2, windows), "another Windows, on another disk");
        // An entry that names no disk matches on any.
        let anywhere = [(Vec::new(), String::from("\\efi\\microsoft\\boot\\bootmgfw.efi"))];
        assert!(already_started(&anywhere, &disk2, windows));
        assert!(!already_started(&started, &disk1, "\\EFI\\other.efi"));
    }

    const UBUNTU_GRUB: &str = "\
menuentry 'Ubuntu' --class ubuntu --class gnu-linux $menuentry_id_option 'gnulinux-simple-abcd' {
	recordfail
	search --no-floppy --fs-uuid --set=root abcd
	linux	/boot/vmlinuz-6.8.0-45-generic root=UUID=abcd ro  quiet splash $vt_handoff
	initrd	/boot/initrd.img-6.8.0-45-generic
}
submenu 'Advanced options for Ubuntu' $menuentry_id_option 'gnulinux-advanced-abcd' {
	menuentry 'Ubuntu, with Linux 6.8.0-45-generic (recovery mode)' {
		linux	/boot/vmlinuz-6.8.0-45-generic root=UUID=abcd ro recovery nomodeset
		initrd	/boot/initrd.img-6.8.0-45-generic
	}
	menuentry 'Ubuntu, with Linux 6.8.0-40-generic' {
		linux	/boot/vmlinuz-6.8.0-40-generic root=UUID=abcd ro  quiet splash $vt_handoff
		initrd	/boot/initrd.img-6.8.0-40-generic
	}
}
";

    #[test]
    fn grub_configuration() {
        let root = Fake::new(&[
            ("\\etc\\fstab", ""),
            ("\\boot\\grub\\grub.cfg", UBUNTU_GRUB),
            ("\\boot\\vmlinuz-6.8.0-45-generic", "k"),
            ("\\boot\\initrd.img-6.8.0-45-generic", "i"),
            ("\\boot\\vmlinuz-6.8.0-40-generic", "k"),
            ("\\boot\\initrd.img-6.8.0-40-generic", "i"),
        ]);
        // GRUB's arguments win over the partition's GUID; the first entry
        // per kernel, not the recovery one.
        let found = scan(&root, Some("1234"));
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].options, "initrd=\\boot\\initrd.img-6.8.0-45-generic root=UUID=abcd ro quiet splash");
        assert_eq!(found[1].options, "initrd=\\boot\\initrd.img-6.8.0-40-generic root=UUID=abcd ro quiet splash");
        // No os-release here: GRUB's menu names the system.
        assert_eq!((found[0].title.as_str(), found[1].title.as_str()), ("Ubuntu", "Ubuntu"));

        // A separate /boot partition: only GRUB knows the root.
        let boot = Fake::new(&[
            ("\\grub\\grub.cfg", &UBUNTU_GRUB.replace("/boot/", "/")),
            ("\\vmlinuz-6.8.0-45-generic", "k"),
            ("\\initrd.img-6.8.0-45-generic", "i"),
        ]);
        let found = scan(&boot, None);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].title, "Ubuntu");
        assert_eq!(found[0].program, "\\vmlinuz-6.8.0-45-generic");
        assert_eq!(found[0].options, "initrd=\\initrd.img-6.8.0-45-generic root=UUID=abcd ro quiet splash");
    }

    #[test]
    fn grub_entries_parse() {
        let entries = grub_entries(UBUNTU_GRUB);
        let kernels: Vec<(&str, &str)> = entries.iter().map(|(k, c)| (k.as_str(), c.title.as_str())).collect();
        assert_eq!(kernels, [("vmlinuz-6.8.0-45-generic", "Ubuntu"), ("vmlinuz-6.8.0-40-generic", "Ubuntu, with Linux 6.8.0-40-generic")]);
        assert_eq!(entries[0].1.initrds, ["\\boot\\initrd.img-6.8.0-45-generic"]);
    }

    #[test]
    fn fedora_entries_on_the_root() {
        let root = Fake::new(&[
            ("\\etc\\os-release", "PRETTY_NAME=\"Fedora Linux 43\"\n"),
            ("\\boot\\loader\\entries\\abc-6.17.2.conf", "title Fedora Linux (6.17.2)\nversion 6.17.2\nlinux /vmlinuz-6.17.2\ninitrd /initramfs-6.17.2.img\noptions root=UUID=f00d ro rhgb quiet\n"),
            ("\\boot\\vmlinuz-6.17.2", "k"),
            ("\\boot\\initramfs-6.17.2.img", "i"),
        ]);
        let found = scan(&root, Some("1234"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].options, "initrd=\\boot\\initramfs-6.17.2.img root=UUID=f00d ro rhgb quiet");
        assert_eq!(found[0].title, "Fedora Linux 43");
    }
}
