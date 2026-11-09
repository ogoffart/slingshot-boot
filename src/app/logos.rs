// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Which entry gets which operating system's logo. The logos are the PNG
//! files of `assets/os/` (see `SOURCES.md` there), kept compressed in the
//! program and decoded when it starts: Slint would embed them as raw pixels,
//! 1.3 MB instead of 172 KB.

use alloc::string::String;

use super::backend::Target;
use super::cards::{Action, Card};

/// The logos, in the order of `config::ICONS` (the Customize dialog's icon
/// picker): name, PNG file.
pub const FILES: [(&str, &[u8]); 23] = [
    ("windows", include_bytes!("../../assets/os/windows.png")),
    ("macos", include_bytes!("../../assets/os/mac.png")),
    ("tux", include_bytes!("../../assets/os/tux.png")),
    ("arch", include_bytes!("../../assets/os/arch.png")),
    ("fedora", include_bytes!("../../assets/os/fedora.png")),
    ("ubuntu", include_bytes!("../../assets/os/ubuntu.png")),
    ("opensuse", include_bytes!("../../assets/os/opensuse.png")),
    ("debian", include_bytes!("../../assets/os/debian.png")),
    ("mint", include_bytes!("../../assets/os/mint.png")),
    ("popos", include_bytes!("../../assets/os/popos.png")),
    ("elementary", include_bytes!("../../assets/os/elementary.png")),
    ("zorin", include_bytes!("../../assets/os/zorin.png")),
    ("bazzite", include_bytes!("../../assets/os/bazzite.png")),
    ("rocky", include_bytes!("../../assets/os/rocky.png")),
    ("almalinux", include_bytes!("../../assets/os/almalinux.png")),
    ("manjaro", include_bytes!("../../assets/os/manjaro.png")),
    ("endeavouros", include_bytes!("../../assets/os/endeavouros.png")),
    ("cachyos", include_bytes!("../../assets/os/cachyos.png")),
    ("kali", include_bytes!("../../assets/os/kali.png")),
    ("nixos", include_bytes!("../../assets/os/nixos.png")),
    ("gentoo", include_bytes!("../../assets/os/gentoo.png")),
    ("void", include_bytes!("../../assets/os/void.png")),
    ("alpine", include_bytes!("../../assets/os/alpine.png")),
];

/// Which logo suits an entry, from its title, its systemd-boot entry and
/// the program it starts. Distributions first, then Tux for other Linux
/// entries.
pub fn logo_for(card: &Card) -> Option<&'static str> {
    let mut text = String::new();
    for part in [card.title.as_str(), card.match_title.as_str()] {
        text.push_str(part);
        text.push(' ');
    }
    match &card.action {
        Action::Start(Target::LoaderEntry { id, program, .. }) => {
            text.push_str(id);
            text.push(' ');
            text.push_str(program);
        }
        Action::Start(Target::Program { path, .. } | Target::Found { program: path, .. }) => text.push_str(path),
        Action::Start(_) => {}
        _ => return None,
    }
    let text = text.to_ascii_lowercase().replace('/', "\\");
    let has = |s: &str| text.contains(s);
    // Derivatives before what they derive from: their entries often say
    // "ubuntu" or "fedora", or start an Arch kernel (vmlinuz-linux-cachyos).
    let names: &[(&str, &[&str])] = &[
        ("windows", &["windows", "\\efi\\microsoft"]),
        ("macos", &["macos", "mac os", "os x", "apple"]),
        ("mint", &["linux mint", "linuxmint", "lmde"]),
        ("popos", &["pop!_os", "pop_os", "pop-os", "\\efi\\pop"]),
        ("elementary", &["elementary"]),
        ("zorin", &["zorin"]),
        ("ubuntu", &["ubuntu", "kubuntu", "xubuntu"]),
        ("bazzite", &["bazzite"]),
        ("rocky", &["rocky"]),
        ("almalinux", &["almalinux", "alma linux"]),
        ("fedora", &["fedora"]),
        ("opensuse", &["suse"]),
        ("manjaro", &["manjaro"]),
        ("endeavouros", &["endeavouros", "endeavour"]),
        ("cachyos", &["cachyos", "cachy"]),
        ("kali", &["kali"]),
        ("debian", &["debian"]),
        ("nixos", &["nixos"]),
        ("gentoo", &["gentoo"]),
        ("void", &["void linux", "voidlinux", "\\efi\\void"]),
        ("alpine", &["alpine"]),
        ("freebsd", &["freebsd"]),
    ];
    if let Some((name, _)) = names.iter().find(|(_, keys)| keys.iter().any(|k| has(k))) {
        return Some(name);
    }
    let logo = if has("arch linux") || has("archlinux") || has("\\efi\\arch") || has("vmlinuz-linux") || text.starts_with("arch") {
        "arch"
    } else if card.kind == "linux" || has("linux") || has("vmlinuz") {
        "tux"
    } else {
        return None;
    };
    Some(logo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    fn card(title: &str, kind: &'static str, action: Action) -> Card {
        Card {
            title: title.to_string(),
            subtitle: String::new(),
            kind,
            image: None,
            action,
            more: false,
            whole_device: false,
            match_title: title.to_string(),
            custom: None,
            icon_chosen: false,
        }
    }

    fn entry(id: &str, program: &str) -> Action {
        Action::Start(Target::LoaderEntry { systemd_boot: None, id: id.into(), program: program.into(), options: String::new() })
    }

    #[test]
    fn by_title() {
        let firmware = || Action::Start(Target::Firmware(1));
        assert_eq!(logo_for(&card("Windows Boot Manager", "windows", firmware())), Some("windows"));
        assert_eq!(logo_for(&card("ubuntu", "linux", firmware())), Some("ubuntu"));
        assert_eq!(logo_for(&card("Fedora", "linux", firmware())), Some("fedora"));
        assert_eq!(logo_for(&card("opensuse-secureboot", "linux", firmware())), Some("opensuse"));
        assert_eq!(logo_for(&card("Debian GNU/Linux 13 (trixie)", "linux", firmware())), Some("debian"));
        assert_eq!(logo_for(&card("macOS Sequoia", "app", firmware())), Some("macos"));
        assert_eq!(logo_for(&card("Linux Boot Manager", "linux", firmware())), Some("tux"));
        assert_eq!(logo_for(&card("UEFI Shell", "shell", firmware())), None);
        assert_eq!(logo_for(&card("Pop!_OS", "linux", firmware())), Some("popos"));
        assert_eq!(logo_for(&card("Linux Mint 22 Wilma", "linux", firmware())), Some("mint"));
        assert_eq!(logo_for(&card("FreeBSD", "app", firmware())), Some("freebsd"));
    }

    #[test]
    fn by_entry_and_program() {
        // Arch's kernel and entry names, under a custom title.
        assert_eq!(logo_for(&card("My system", "linux", entry("arch.conf", "\\vmlinuz-linux"))), Some("arch"));
        assert_eq!(logo_for(&card("Arch Linux (fallback initramfs)", "linux", entry("x.conf", ""))), Some("arch"));
        assert_eq!(logo_for(&card("Kernel", "linux", entry("x.conf", "/vmlinuz-6.8"))), Some("tux"));
        // An Arch derivative, with Arch's kernel names.
        assert_eq!(logo_for(&card("Linux", "linux", entry("linux-cachyos.conf", "\\vmlinuz-linux-cachyos"))), Some("cachyos"));
        assert_eq!(logo_for(&card("Bazzite", "linux", entry("fedora-x.conf", ""))), Some("bazzite"));
        assert_eq!(logo_for(&card("NixOS", "linux", entry("nixos-generation-12.conf", ""))), Some("nixos"));
        let windows = Action::Start(Target::Program { path: "\\EFI\\Microsoft\\Boot\\bootmgfw.efi".into(), options: String::new() });
        assert_eq!(logo_for(&card("Work", "app", windows)), Some("windows"));
    }

    #[test]
    fn not_for_other_actions() {
        assert_eq!(logo_for(&card("Linux stuff", "linux", Action::FirmwareSetup)), None);
    }
}
