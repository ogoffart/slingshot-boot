// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! The boot picker's cards: built from the firmware's entries and the Boot
//! Loader Specification entries, changed by slingshot.toml's rules, and
//! split between the main row and "More…".

use crate::tr::{tr, trf};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::config::{self, Config};
use super::{BootOption, FoundEntry, LoaderConfig, LoaderEntry, Target};

/// What a card of the boot picker does.
#[derive(Clone, Debug, Default)]
pub enum Action {
    Start(Target),
    /// Restart into the firmware's setup screen (not a boot entry).
    FirmwareSetup,
    /// "Start a file…": the file browser.
    BrowseFiles,
    /// Show the entries under "More…".
    ShowMore,
    /// Back from "More…" to the main row.
    #[default]
    ShowMain,
}

#[derive(Clone, Debug, Default)]
pub struct Card {
    pub title: String,
    pub subtitle: String,
    pub kind: &'static str,
    /// An image file (from slingshot.toml) shown instead of the icon.
    pub image: Option<String>,
    pub action: Action,
    /// Under "More…" rather than on the main row.
    pub more: bool,
    /// A whole device (the firmware's generic entries).
    pub whole_device: bool,
    /// The title slingshot.toml's rules match (before renaming).
    pub match_title: String,
    /// For an entry of slingshot.toml's own: its index in the rules.
    pub custom: Option<usize>,
    /// slingshot.toml chose its icon (`kind`): no logo of its own then.
    pub icon_chosen: bool,
}

/// What the boot picker shows: the main row or "More…".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Level {
    #[default]
    Main,
    More,
}

impl Card {
    /// Where Slingshot puts a card when slingshot.toml doesn't say: the
    /// firmware's generic entries, tools and fallback kernels go under
    /// "More…".
    pub fn more_by_default(&self) -> bool {
        matches!(self.action, Action::FirmwareSetup | Action::BrowseFiles)
            || matches!(self.kind, "network" | "shell" | "app" | "disc")
            || (self.kind == "usb" && self.whole_device)
            || self.title.to_lowercase().contains("fallback")
            // Kernels found on a disk: the distribution's own entry (GRUB,
            // shim) is the usual way to start them.
            || (self.kind == "linux" && matches!(self.action, Action::Start(Target::Found { .. })))
    }

    /// The systemd-boot id slingshot.toml's rules match.
    /// What identifies the card from one start to the next, to remember
    /// the one started last: its systemd-boot entry, its program, else its
    /// title (Boot#### numbers change when entries are added).
    pub fn key(&self) -> Option<String> {
        match &self.action {
            Action::Start(Target::LoaderEntry { id, .. }) => Some(format!("entry:{id}")),
            Action::Start(Target::Program { path, .. }) => Some(format!("program:{path}")),
            // Tied to its partition: a subtitle like "USB · removable" and
            // \EFI\BOOT\BOOTX64.EFI would match any stick. And not a
            // USB disk's at all: whatever stick is plugged in next mustn't
            // start by itself.
            Action::Start(Target::Found { fs, program, .. }) if !on_usb(fs) => {
                let partition = crate::parse::detect::partition_uuid(fs)
                    .unwrap_or_else(|| fs.iter().map(|b| format!("{b:02x}")).collect());
                Some(format!("found:{partition}:{program}"))
            }
            Action::Start(Target::Firmware(_)) if self.kind == "usb" => None,
            Action::Start(Target::Firmware(_)) => {
                // The title before slingshot.toml renames it, when it does.
                let title = if self.match_title.is_empty() { &self.title } else { &self.match_title };
                Some(format!("firmware:{title}"))
            }
            _ => None,
        }
    }

    pub fn id(&self) -> Option<&str> {
        match &self.action {
            Action::Start(Target::LoaderEntry { id, .. }) => Some(id),
            _ => None,
        }
    }

    /// The kernel options without the initrds, for Linux entries.
    pub fn kernel_options(&self) -> Option<String> {
        self.options().map(|o| o.split(' ').filter(|p| !p.starts_with("initrd=")).collect::<Vec<_>>().join(" "))
    }

    /// The kernel options this card starts with, if they can be edited.
    pub fn options(&self) -> Option<&str> {
        match &self.action {
            Action::Start(
                Target::LoaderEntry { options, .. } | Target::Program { options, .. } | Target::Found { options, .. },
            ) => Some(options),
            _ => None,
        }
    }
}

/// A device path through USB.
fn on_usb(device_path: &[u8]) -> bool {
    crate::parse::load_option::nodes(device_path).iter().any(|n| n.0 == 0x03 && matches!(n.1, 0x05 | 0x0f | 0x10))
}

/// A card for a Boot Loader Specification entry.
fn loader_card(e: &LoaderEntry, boot: Option<u16>) -> Card {
    Card {
        title: e.title.clone(),
        subtitle: if e.failed {
            tr("Failed to start (no tries left)")
        } else if !e.version.is_empty() {
            e.version.clone()
        } else if e.kind == "linux" {
            tr("Linux")
        } else {
            tr("Boot entry")
        },
        kind: e.kind,
        action: Action::Start(Target::LoaderEntry {
            systemd_boot: boot,
            id: e.id.clone(),
            program: e.program.clone(),
            options: e.options.clone(),
        }),
        ..Default::default()
    }
}

/// The boot picker's cards: the firmware entries in order, with the
/// systemd-boot entry replaced by the Boot Loader Specification entries
/// (systemd-boot's menu). Without a systemd-boot entry, those entries come
/// first: Slingshot starts them itself. Returns the cards and the index of
/// the default one. The firmware setup comes last.
pub fn build_cards(
    options: &[BootOption],
    sd: Option<&LoaderConfig>,
    found: &[FoundEntry],
    firmware_setup: bool,
) -> (Vec<Card>, usize) {
    let mut cards = Vec::new();
    let mut default = 0;
    let sd = sd.filter(|c| !c.entries.is_empty());
    if let Some(config) = sd.filter(|_| !options.iter().any(|o| o.is_systemd_boot)) {
        default = config.default;
        cards.extend(config.entries.iter().map(|e| loader_card(e, None)));
    }
    for (i, o) in options.iter().enumerate() {
        if o.absent {
            continue;
        }
        match sd {
            Some(config) if o.is_systemd_boot => {
                if i == 0 {
                    default = cards.len() + config.default;
                }
                cards.extend(config.entries.iter().map(|e| loader_card(e, Some(o.number))));
            }
            _ => cards.push(Card {
                title: o.title.clone(),
                subtitle: o.subtitle.clone(),
                kind: o.kind,
                action: Action::Start(Target::Firmware(o.number)),
                whole_device: o.whole_device,
                ..Default::default()
            }),
        }
    }
    // What was found on the disks, after the firmware's entries.
    cards.extend(found.iter().map(|f| Card {
        title: f.title.clone(),
        subtitle: f.subtitle.clone(),
        kind: f.kind,
        action: Action::Start(Target::Found { fs: f.fs.clone(), program: f.program.clone(), options: f.options.clone() }),
        ..Default::default()
    }));
    if firmware_setup {
        cards.push(Card {
            title: tr("Firmware setup"),
            // slingshot.toml's rules name it in English.
            match_title: String::from("Firmware setup"),
            subtitle: tr("The firmware's own settings"),
            kind: "sliders",
            action: Action::FirmwareSetup,
            ..Default::default()
        });
    }
    (cards, default)
}

/// The "Start a file…" card, under "More…".
pub fn browse_card() -> Card {
    Card {
        title: tr("Start a file…"),
        subtitle: tr("An EFI program on any disk"),
        kind: "folder",
        action: Action::BrowseFiles,
        more: true,
        ..Default::default()
    }
}

/// A card for an entry of slingshot.toml's own, `index` in its rules.
fn custom_card(rule: &config::EntryRule, index: usize) -> Option<Card> {
    let custom = rule.custom.as_ref()?;
    Some(Card {
        match_title: rule.title.clone().unwrap_or_default(),
        custom: Some(index),
        title: rule.title.clone().unwrap_or_default(),
        subtitle: if custom.is_linux { tr("Linux") } else { tr("EFI program") },
        kind: rule.icon.unwrap_or(if custom.is_linux { "linux" } else { "app" }),
        image: rule.image.clone(),
        action: Action::Start(Target::Program { path: custom.program.clone(), options: rule.command_line() }),
        more: rule.more.unwrap_or(false),
        whole_device: false,
        icon_chosen: rule.icon.is_some(),
    })
}

/// Applies the `[[entry]]` rules of slingshot.toml to the cards: renames,
/// changes icons and kernel options, sorts them between the main row and
/// "More…", adds the entries of the file's own (before the firmware setup),
/// and picks the default: the systemd-boot entry `one_shot` (`bootctl
/// reboot --boot-loader-entry`) this time, else the rules'. Returns the new
/// default; it's always on the main row.
pub fn apply_rules(cards: &mut Vec<Card>, default: usize, config: &Config, one_shot: Option<&str>) -> usize {
    let mut kept = Vec::with_capacity(cards.len());
    let mut new_default = None;
    let mut rule_default = None;
    let mut one_shot_default = None;
    for (i, mut card) in core::mem::take(cards).into_iter().enumerate() {
        let id = match &card.action {
            Action::Start(Target::LoaderEntry { id, .. }) => Some(id.as_str()),
            _ => None,
        };
        if one_shot.is_some() && id == one_shot && one_shot_default.is_none() {
            one_shot_default = Some(kept.len());
        }
        card.more = card.more_by_default();
        if card.match_title.is_empty() {
            card.match_title = card.title.clone();
        }
        if let Some(rule) = config.rule_for(&card.match_title, id) {
            if rule.default && rule_default.is_none() {
                rule_default = Some(kept.len());
            }
            if let Some(title) = &rule.title {
                card.title = title.clone();
            }
            if let Some(icon) = rule.icon {
                card.kind = icon;
                card.icon_chosen = true;
            }
            if rule.image.is_some() {
                card.image = rule.image.clone();
            }
            if let Some(more) = rule.more {
                card.more = more;
            }
            if let (Some(new), Action::Start(Target::LoaderEntry { options, program, .. })) =
                (&rule.options, &mut card.action)
            {
                // The initrds stay: only the kernel options change.
                let initrds: Vec<&str> = options.split(' ').filter(|o| o.starts_with("initrd=")).collect();
                *options = if program.is_empty() { new.clone() } else { [initrds.join(" "), new.clone()].join(" ").trim().into() };
            }
        }
        // The firmware setup is only the default when there's nothing else
        // (the boot order has no other entry): then the first card is.
        if i == default && !matches!(card.action, Action::FirmwareSetup) {
            new_default = Some(kept.len());
        }
        kept.push(card);
    }
    // The entries of the file's own go before the firmware setup.
    let setup = kept.iter().position(|c| matches!(c.action, Action::FirmwareSetup | Action::BrowseFiles)).unwrap_or(kept.len());
    let mut customs: Vec<(bool, Card)> = Vec::new();
    for (i, rule) in config.entries.iter().enumerate() {
        if let Some(card) = custom_card(rule, i) {
            customs.push((rule.default, card));
        }
    }
    let added = customs.len();
    let moved = |i: usize| if i >= setup { i + added } else { i };
    rule_default = rule_default.map(moved);
    new_default = new_default.map(moved);
    one_shot_default = one_shot_default.map(moved);
    for (n, (is_default, card)) in customs.into_iter().enumerate() {
        if is_default && rule_default.is_none() {
            rule_default = Some(setup + n);
        }
        kept.insert(setup + n, card);
    }
    *cards = kept;
    // The one-shot entry, else a `default = true` rule, else the firmware's
    // default, else the first card.
    let default = one_shot_default.or(rule_default).or(new_default).unwrap_or(0);
    if let Some(card) = cards.get_mut(default) {
        card.more = false;
    }
    default
}

/// The cards shown: the main row with a "More…" card when there are other
/// entries, or the "More…" level with a "Back" card first. Also returns the
/// index of the default card in that view, if it's there.
pub fn view(cards: &[Card], default: usize, level: Level) -> (Vec<Card>, Option<usize>) {
    let mut view = Vec::new();
    let mut view_default = None;
    match level {
        Level::Main => {}
        Level::More => view.push(Card {
            title: tr("Back"),
            subtitle: tr("The main entries"),
            kind: "back",
            action: Action::ShowMain,
            ..Default::default()
        }),
    }
    let more = level == Level::More;
    for (i, c) in cards.iter().enumerate() {
        if c.more == more {
            if i == default {
                view_default = Some(view.len());
            }
            view.push(c.clone());
        }
    }
    let others = cards.iter().filter(|c| c.more).count();
    if !more && others > 0 {
        view.push(Card {
            title: tr("More…"),
            subtitle: if others == 1 { tr("1 other entry") } else { trf("{} other entries", &[&others]) },
            kind: "more",
            action: Action::ShowMore,
            ..Default::default()
        });
    }
    (view, view_default)
}

/// The card of the system that hibernated (`hibernated`), and whether
/// that's certain enough to start it without asking: a card with its
/// kernel version, else the Linux card started last, else the only Linux
/// card on the main row. Otherwise a guess, for the focus.
pub fn hibernated_card(cards: &[Card], hibernated: &crate::parse::hibernate::Hibernated, last: Option<&str>) -> Option<(usize, bool)> {
    // Not a USB stick's kernel: one named like the hibernated one would
    // start by itself.
    let startable = |c: &Card| match &c.action {
        Action::Start(Target::Found { fs, .. }) => !on_usb(fs),
        Action::Start(_) => true,
        _ => false,
    };
    let linux = |c: &Card| startable(c) && c.kind == "linux";
    let version = hibernated.kernel_version.as_str();
    // "6.17.1-arch1-1": a few characters with a digit and a dot, not
    // something that's in every title.
    if version.len() >= 3 && version.contains('.') && version.bytes().any(|b| b.is_ascii_digit()) {
        let has_version = |c: &Card| {
            let (id, program, options) = match &c.action {
                Action::Start(Target::LoaderEntry { id, program, options, .. }) => (id.as_str(), program.as_str(), options.as_str()),
                Action::Start(Target::Found { program, options, .. } | Target::Program { path: program, options }) => ("", program.as_str(), options.as_str()),
                _ => ("", "", ""),
            };
            [c.title.as_str(), &c.subtitle, id, program, options].iter().any(|t| t.contains(version))
        };
        if let Some(i) = cards.iter().position(|c| startable(c) && has_version(c)) {
            return Some((i, true));
        }
    }
    if let Some(i) = last.and_then(|key| cards.iter().position(|c| linux(c) && c.key().as_deref() == Some(key))) {
        return Some((i, true));
    }
    let main: Vec<usize> = (0..cards.len()).filter(|&i| linux(&cards[i]) && !cards[i].more).collect();
    match main[..] {
        [only] => Some((only, true)),
        [first, ..] => Some((first, false)),
        [] => cards.iter().position(linux).map(|i| (i, false)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::LoaderEntry;
    use alloc::vec;

    fn option(number: u16, title: &str, is_systemd_boot: bool) -> BootOption {
        BootOption { number, title: title.into(), subtitle: String::from("Disk"), kind: "disk", is_systemd_boot, ..Default::default() }
    }

    fn loader(default: usize) -> LoaderConfig {
        let entry = |id: &str, version: &str| LoaderEntry {
            id: id.into(),
            title: id.trim_end_matches(".conf").into(),
            version: version.into(),
            kind: "linux",
            program: String::from("\\vmlinuz"),
            ..Default::default()
        };
        LoaderConfig { entries: vec![entry("first.conf", "1.2"), entry("second.conf", "")], default, ..Default::default() }
    }

    fn titles(cards: &[Card]) -> Vec<&str> {
        cards.iter().map(|c| c.title.as_str()).collect()
    }

    #[test]
    fn the_system_that_hibernated() {
        use crate::parse::hibernate::Hibernated;
        let options = [option(1, "Windows", false), option(4, "Linux Boot Manager", true)];
        let (cards, _) = build_cards(&options, Some(&loader(0)), &[], false);
        let h = |v: &str| Hibernated { kernel_version: v.into(), os_id: String::from("arch") };
        // Its kernel version: certain.
        assert_eq!(hibernated_card(&cards, &h("1.2"), None), Some((1, true)));
        // The Linux entry started last: certain too.
        assert_eq!(hibernated_card(&cards, &h("6.0"), Some("entry:second.conf")), Some((2, true)));
        // Two Linux entries, no clue: the first, to choose from.
        assert_eq!(hibernated_card(&cards, &h(""), Some("firmware:Windows")), Some((1, false)));
        assert_eq!(cards[0].key().as_deref(), Some("firmware:Windows"));
        assert_eq!(cards[2].key().as_deref(), Some("entry:second.conf"));
    }

    #[test]
    fn cards_without_systemd_boot() {
        let options = [option(1, "Disk A", false), option(2, "Disk B", false)];
        let (cards, default) = build_cards(&options, None, &[], true);
        assert_eq!(titles(&cards), ["Disk A", "Disk B", "Firmware setup"]);
        assert_eq!(default, 0);
        assert!(matches!(cards[1].action, Action::Start(Target::Firmware(2))));
        assert!(matches!(cards[2].action, Action::FirmwareSetup));
        let (cards, _) = build_cards(&options, None, &[], false);
        assert_eq!(cards.len(), 2);
    }

    #[test]
    fn systemd_boot_entries_replace_its_card() {
        let options = [option(1, "Other", false), option(4, "Linux Boot Manager", true), option(5, "Last", false)];
        let config = loader(1);
        let (cards, default) = build_cards(&options, Some(&config), &[], false);
        assert_eq!(titles(&cards), ["Other", "first", "second", "Last"]);
        assert_eq!(cards[1].subtitle, "1.2");
        assert_eq!(cards[2].subtitle, "Linux");
        match &cards[2].action {
            Action::Start(Target::LoaderEntry { systemd_boot, id, .. }) => assert_eq!((*systemd_boot, id.as_str()), (Some(4), "second.conf")),
            other => panic!("{other:?}"),
        }
        // systemd-boot isn't first in BootOrder: the firmware's first entry is the default.
        assert_eq!(default, 0);
    }

    #[test]
    fn systemd_boot_first_gives_its_default() {
        let options = [option(4, "Linux Boot Manager", true), option(5, "Other", false)];
        let config = loader(1);
        let (cards, default) = build_cards(&options, Some(&config), &[], true);
        assert_eq!(titles(&cards), ["first", "second", "Other", "Firmware setup"]);
        assert_eq!(default, 1);
    }

    #[test]
    fn systemd_boot_without_entries_stays_a_card() {
        let options = [option(4, "Linux Boot Manager", true)];
        let config = LoaderConfig::default();
        let (cards, default) = build_cards(&options, Some(&config), &[], false);
        assert_eq!(titles(&cards), ["Linux Boot Manager"]);
        assert_eq!(default, 0);
    }

    #[test]
    fn entries_without_systemd_boot_come_first() {
        let options = [option(1, "Windows Boot Manager", false), option(2, "USB", false)];
        let config = loader(1);
        let (cards, default) = build_cards(&options, Some(&config), &[], false);
        assert_eq!(titles(&cards), ["first", "second", "Windows Boot Manager", "USB"]);
        assert_eq!(default, 1, "the entries' default");
        assert!(matches!(&cards[0].action, Action::Start(Target::LoaderEntry { systemd_boot: None, .. })));
    }

    fn kinds(cards: &[Card]) -> Vec<(&str, bool)> {
        cards.iter().map(|c| (c.title.as_str(), c.more)).collect()
    }

    #[test]
    fn more_level() {
        let mut options = vec![option(1, "Linux Boot Manager", true), option(2, "Windows Boot Manager", false)];
        options.push(BootOption { number: 3, title: "EFI USB Device".into(), kind: "usb", whole_device: true, ..Default::default() });
        options.push(BootOption { number: 4, title: "SanDisk".into(), kind: "usb", ..Default::default() });
        options.push(BootOption { number: 5, title: "EFI Network".into(), kind: "network", whole_device: true, ..Default::default() });
        options.push(BootOption { number: 6, title: "EFI Shell".into(), kind: "shell", ..Default::default() });
        let mut sd = loader(0);
        sd.entries[1].title = "Arch (fallback)".into();
        let (mut cards, default) = build_cards(&options, Some(&sd), &[], true);
        let config = config::parse("[[entry]]\nmatch = \"EFI Shell\"\nmore = false\n");
        let default = apply_rules(&mut cards, default, &config, None);
        assert_eq!(
            kinds(&cards),
            [
                ("first", false),
                ("Arch (fallback)", true),
                ("Windows Boot Manager", false),
                ("EFI USB Device", true),
                ("SanDisk", false),
                ("EFI Network", true),
                ("EFI Shell", false),
                ("Firmware setup", true),
            ]
        );
        let (main, main_default) = view(&cards, default, Level::Main);
        assert_eq!(titles(&main), ["first", "Windows Boot Manager", "SanDisk", "EFI Shell", "More…"]);
        assert_eq!(main_default, Some(0));
        assert_eq!(main[4].subtitle, "4 other entries");
        let (more, more_default) = view(&cards, default, Level::More);
        assert_eq!(titles(&more), ["Back", "Arch (fallback)", "EFI USB Device", "EFI Network", "Firmware setup"]);
        assert!(matches!(more[0].action, Action::ShowMain));
        assert_eq!(more_default, None);
    }

    #[test]
    fn entries_of_your_own_before_the_firmware_setup_as_default() {
        let config = config::parse("[[entry]]\ntitle = \"Kernel\"\nlinux = \"/vmlinuz\"\n");
        let (mut cards, default) = build_cards(&[], None, &[], true);
        let default = apply_rules(&mut cards, default, &config, None);
        assert_eq!(cards[default].title, "Kernel");
    }

    #[test]
    fn entries_under_more() {
        let options = [option(1, "Windows Boot Manager", false), option(2, "EFI Network 0", false)];
        let config = config::parse(
            "[[entry]]\nmatch = \"EFI Network*\"\nmore = true\ntitle = \"Network\"\n\
             [[entry]]\ntitle = \"Tool\"\nefi = \"/tool.efi\"\nmore = true\n",
        );
        assert!(config.warnings.is_empty(), "{:?}", config.warnings);
        let (mut cards, default) = build_cards(&options, None, &[], false);
        let default = apply_rules(&mut cards, default, &config, None);
        let (main, _) = view(&cards, default, Level::Main);
        assert_eq!(titles(&main), ["Windows Boot Manager", "More…"]);
        let (more, _) = view(&cards, default, Level::More);
        assert_eq!(titles(&more), ["Back", "Network", "Tool"], "rules applied, entries of the file's own too");
    }

    #[test]
    fn default_stays_on_the_main_row() {
        let options = [option(1, "Windows Boot Manager", false)];
        let config = config::parse("[[entry]]\nmatch = \"Firmware setup\"\ndefault = true\n");
        let (mut cards, default) = build_cards(&options, None, &[], true);
        let default = apply_rules(&mut cards, default, &config, None);
        assert_eq!((default, cards[default].more), (1, false));
        let (main, _) = view(&cards, default, Level::Main);
        assert_eq!(titles(&main), ["Windows Boot Manager", "Firmware setup"], "no More… card when nothing is under it");
    }

    #[test]
    fn entries_of_your_own_and_options() {
        let options = [option(4, "Linux Boot Manager", true), option(5, "Windows Boot Manager", false)];
        let mut sd = loader(0);
        sd.entries[0].options = "initrd=\\initramfs.img root=LABEL=a rw".into();
        let config = config::parse(
            r#"
            [[entry]]
            match = "first.conf"
            options = "root=LABEL=a rw quiet"
            [[entry]]
            title = "Debug"
            linux = "/vmlinuz"
            initrd = "/initrd.img"
            options = "loglevel=7"
            default = true
            [[entry]]
            title = "Memtest"
            efi = "/EFI/memtest.efi"
            more = true
            "#,
        );
        assert!(config.warnings.is_empty(), "{:?}", config.warnings);
        let (mut cards, default) = build_cards(&options, Some(&sd), &[], true);
        let default = apply_rules(&mut cards, default, &config, None);
        assert_eq!(titles(&cards), ["first", "second", "Windows Boot Manager", "Debug", "Memtest", "Firmware setup"]);
        assert_eq!(default, 3);
        assert_eq!(cards[0].options(), Some("initrd=\\initramfs.img root=LABEL=a rw quiet"), "initrds kept");
        match &cards[3].action {
            Action::Start(Target::Program { path, options }) => {
                assert_eq!((path.as_str(), options.as_str()), ("\\vmlinuz", "initrd=\\initrd.img loglevel=7"))
            }
            other => panic!("{other:?}"),
        }
        assert!(cards[4].more);
        assert_eq!(cards[4].kind, "app");
        assert!(cards[2].options().is_none(), "firmware entries have no kernel options");
    }

    #[test]
    fn no_options() {
        let (cards, default) = build_cards(&[], None, &[], false);
        assert!(cards.is_empty());
        assert_eq!(default, 0);
    }

    #[test]
    fn entry_rules_on_cards() {
        let options = [option(1, "EFI USB Device", false), option(4, "Linux Boot Manager", true), option(5, "Windows Boot Manager", false)];
        let sd = loader(0);
        let config = config::parse(
            "[[entry]]\nmatch = \"EFI USB Device\"\nmore = true\n\
             [[entry]]\nmatch = \"second.conf\"\ntitle = \"Arch (fallback)\"\nicon = \"disk\"\n\
             [[entry]]\nmatch = \"Windows*\"\ndefault = true\n",
        );
        let (mut cards, default) = build_cards(&options, Some(&sd), &[], true);
        assert_eq!(default, 0, "BootOrder starts with the USB entry");
        let default = apply_rules(&mut cards, default, &config, None);
        assert_eq!(titles(&cards), ["EFI USB Device", "first", "Arch (fallback)", "Windows Boot Manager", "Firmware setup"]);
        assert!(cards[0].more);
        assert_eq!(cards[2].kind, "disk");
        assert_eq!(default, 3, "default = yes");

        // The firmware's default is under More…: it comes back to the main row.
        let config = config::parse("[[entry]]\nmatch = \"EFI USB Device\"\nmore = true\n");
        let (mut cards, default) = build_cards(&options, Some(&sd), &[], false);
        assert_eq!(apply_rules(&mut cards, default, &config, None), 0);
        assert!(!cards[0].more);

        // No rules: unchanged.
        let (mut cards, default) = build_cards(&options, Some(&sd), &[], false);
        let config = Config::default();
        assert_eq!(apply_rules(&mut cards, 1, &config, None), 1);
        assert_eq!(cards.len(), 4);
        assert_eq!(default, 0);
    }


    #[test]
    fn one_shot_is_the_default_on_the_main_row() {
        let options = [option(4, "Linux Boot Manager", true), option(5, "Windows Boot Manager", false)];
        let sd = loader(0);
        let config = config::parse("[[entry]]\nmatch = \"second.conf\"\nmore = true\n[[entry]]\nmatch = \"Windows*\"\ndefault = true\n");
        let (mut cards, default) = build_cards(&options, Some(&sd), &[], false);
        assert_eq!(apply_rules(&mut cards, default, &config, Some("second.conf")), 1, "over the rule's default");
        assert!(!cards[1].more);
        let (mut cards, default) = build_cards(&options, Some(&sd), &[], false);
        assert_eq!(apply_rules(&mut cards, default, &config, Some("gone.conf")), 2, "not there: the rule's");
    }

    #[test]
    fn found_on_disks() {
        let found = |title: &str, kind: &'static str| FoundEntry {
            title: title.into(),
            kind,
            program: "\\p.efi".into(),
            options: "root=x".into(),
            ..Default::default()
        };
        let options = [option(1, "Ubuntu", false)];
        let all = [found("macOS 14.5", "apple"), found("Windows", "windows"), found("Fedora", "linux")];
        let (mut cards, default) = build_cards(&options, None, &all, true);
        let default = apply_rules(&mut cards, default, &Config::default(), None);
        let placed: Vec<(&str, bool)> = cards.iter().map(|c| (c.title.as_str(), c.more)).collect();
        assert_eq!(
            placed,
            [("Ubuntu", false), ("macOS 14.5", false), ("Windows", false), ("Fedora", true), ("Firmware setup", true)]
        );
        assert_eq!(default, 0);
        // A found kernel's options can be edited (E).
        assert_eq!(cards[3].options(), Some("root=x"));
    }

    #[test]
    fn failed_entries_say_so() {
        let mut config = loader(0);
        config.entries[0].failed = true;
        let (cards, _) = build_cards(&[], Some(&config), &[], false);
        assert_eq!(cards[0].subtitle, "Failed to start (no tries left)");
        assert_eq!(cards[1].subtitle, "Linux");
    }
}
