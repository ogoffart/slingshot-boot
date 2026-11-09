// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! `slingshot.toml`, next to the binary: defaults for the settings, who may
//! change them, and per-entry rules (rename, icon, default, under More…).
//!
//! ```toml
//! # Defaults; the Settings screen can change them (saved in NVRAM).
//! theme = "dark"
//! style = "cards"
//! accent = "teal"
//! timeout = 5
//! startup = "splash"
//! resolution = "1920x1080"
//! firmware-setup = false
//!
//! [splash]
//! text = "Welcome back"
//! image = "splash.png"
//!
//! [settings]
//! access = "password"          # "shown" (default), "hidden" or "password"
//! password = "sha256:5e88489..."
//! editor = false               # E, the kernel options: by default allowed,
//!                              # unless Secure Boot is on or loader.conf
//!                              # says `editor no`
//!
//! # Rules for boot entries, by title or systemd-boot entry id (* and ?).
//! [[entry]]
//! match = "EFI USB Device"
//! more = true
//!
//! [[entry]]
//! match = "arch*.conf"
//! title = "Arch Linux"
//! icon = "linux"             # or an image: image = "arch.png"
//! default = true
//! ```
//!
//! A value that isn't understood is skipped with a warning; the rest of the
//! file still applies.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::settings::{STYLES, Settings, THEMES};
use crate::parse::bls::glob_match;

/// The icons a card can have: drawn ones, then the systems' logos
/// (`LOGOS`, in the order of `logos::FILES`).
pub const ICONS: [&str; 31] = [
    "linux", "usb", "network", "shell", "disc", "app", "disk", "sliders", "windows", "macos", "tux", "arch", "fedora", "ubuntu",
    "opensuse", "debian", "mint", "popos", "elementary", "zorin", "bazzite", "rocky", "almalinux", "manjaro", "endeavouros", "cachyos", "kali", "nixos", "gentoo", "void", "alpine"
];
/// The icons of `ICONS` that are logos (pictures, `os:<name>`).
pub const LOGOS: &[&str] = ICONS.split_at(8).1;
/// Accent names, in the order of `Theme.accents`.
/// The last one, "wallpaper", is the wallpaper's own color.
pub const ACCENTS: [&str; 6] = ["blue", "teal", "violet", "pink", "orange", "wallpaper"];

#[derive(Clone, Debug, PartialEq, Default)]
pub enum SettingsAccess {
    #[default]
    Shown,
    /// No Settings screen; the settings saved in NVRAM are ignored.
    Hidden,
    /// The Settings screen asks for the password whose SHA-256 this is.
    Password([u8; 32]),
}

/// Settings values given in the file; `None` keeps the built-in default.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SettingsDefaults {
    pub theme: Option<u8>,
    pub style: Option<u8>,
    pub accent: Option<u8>,
    pub splash: Option<bool>,
    /// `default = "last"`: the entry started last is the default.
    pub remember_last: Option<bool>,
    pub timeout: Option<u16>,
    pub mode: Option<(u16, u16)>,
}

impl SettingsDefaults {
    /// The defaults that make `chosen` out of `builtin`: only what differs.
    pub fn between(builtin: &Settings, chosen: &Settings) -> Self {
        fn differs<T: PartialEq>(a: T, b: T) -> Option<T> {
            (a != b).then_some(b)
        }
        SettingsDefaults {
            theme: differs(builtin.theme, chosen.theme),
            style: differs(builtin.style, chosen.style),
            accent: differs(builtin.accent, chosen.accent),
            splash: differs(builtin.splash, chosen.splash),
            remember_last: differs(builtin.remember_last, chosen.remember_last),
            timeout: differs(builtin.timeout, chosen.timeout),
            mode: differs(builtin.mode, chosen.mode).filter(|m| *m != (0, 0)),
        }
    }

    pub fn apply(&self, settings: &mut Settings) {
        settings.theme = self.theme.unwrap_or(settings.theme);
        settings.style = self.style.unwrap_or(settings.style);
        settings.accent = self.accent.unwrap_or(settings.accent);
        settings.splash = self.splash.unwrap_or(settings.splash);
        settings.remember_last = self.remember_last.unwrap_or(settings.remember_last);
        settings.timeout = self.timeout.unwrap_or(settings.timeout);
        settings.mode = self.mode.unwrap_or(settings.mode);
    }
}

/// An `[entry PATTERN]` section.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct EntryRule {
    /// Matched against the entry's title (as the firmware or systemd-boot
    /// names it) and its systemd-boot id, case-insensitively.
    pub pattern: String,
    pub title: Option<String>,
    pub icon: Option<&'static str>,
    /// A PNG next to slingshot.toml, shown instead of the icon.
    pub image: Option<String>,
    pub default: bool,
    /// Under "More…" (`Some(true)`), on the main row (`Some(false)`), or
    /// where Slingshot puts it.
    pub more: Option<bool>,
    /// The kernel options, instead of the boot entry's (Linux entries).
    pub options: Option<String>,
    /// For an entry of its own (no `match`): what it starts.
    pub custom: Option<CustomEntry>,
}

/// An entry defined in slingshot.toml: a kernel, or an EFI program, on the
/// file system Slingshot is on.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CustomEntry {
    /// The kernel (`linux`) or the program (`efi`), e.g. `\vmlinuz-linux`.
    pub program: String,
    /// Initrds, for a kernel.
    pub initrd: Vec<String>,
    pub is_linux: bool,
}

impl EntryRule {
    pub fn matches(&self, title: &str, id: Option<&str>) -> bool {
        self.custom.is_none()
            && (glob_match(&self.pattern, title) || id.is_some_and(|id| glob_match(&self.pattern, id)))
    }

    /// What a custom entry passes to what it starts.
    pub fn command_line(&self) -> String {
        let options = self.options.as_deref().unwrap_or("");
        match &self.custom {
            Some(c) if c.is_linux => crate::parse::bls::command_line(&c.initrd, options),
            _ => options.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub splash_text: String,
    pub splash_image: String,
    /// The splash screen's parts: the entry's name (when there's no text),
    /// the picture (the logo in its ring, or the image), and the "Press any
    /// key for the boot menu" line.
    pub splash_name: bool,
    pub splash_picture: bool,
    pub splash_hint: bool,
    /// A PNG next to slingshot.toml, behind the boot menu ("" = none).
    pub wallpaper: String,
    /// The keyboard layout (index in `parse::keymap::LAYOUTS`; 0, US, by
    /// default): the firmware types as on a US keyboard.
    pub keyboard: usize,
    /// The language ("fr", "de", "zh"; English without translation); `None`: the
    /// firmware's (PlatformLang), or the desktop's.
    pub language: Option<String>,
    pub defaults: SettingsDefaults,
    pub settings_access: SettingsAccess,
    /// `settings.editor`: whether the kernel options can be edited (E);
    /// `None`: unless Secure Boot is on or loader.conf says `editor no`.
    pub editor: Option<bool>,
    /// Show the "Firmware setup" card (when the firmware supports it).
    pub firmware_setup: bool,
    pub entries: Vec<EntryRule>,
    /// Lines that were not understood, to log.
    pub warnings: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            splash_text: String::new(),
            splash_image: String::new(),
            splash_name: true,
            splash_picture: true,
            splash_hint: true,
            wallpaper: String::new(),
            keyboard: 0,
            language: None,
            defaults: SettingsDefaults::default(),
            settings_access: SettingsAccess::Shown,
            editor: None,
            firmware_setup: true,
            entries: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

impl Config {
    /// Whether `password` unlocks the Settings screen.
    pub fn check_password(&self, password: &str) -> bool {
        match &self.settings_access {
            SettingsAccess::Password(hash) => hash_password(password) == *hash,
            _ => true,
        }
    }

    /// The first rule that matches an entry.
    pub fn rule_for(&self, title: &str, id: Option<&str>) -> Option<&EntryRule> {
        self.entries.iter().find(|r| r.matches(title, id))
    }
}

fn parse_mode(value: &str) -> Option<(u16, u16)> {
    let (w, h) = value.split_once(['x', 'X', '×'])?;
    let (w, h) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

fn parse_hash(value: &str) -> Option<[u8; 32]> {
    let hex = value.strip_prefix("sha256:")?.trim();
    if hex.len() != 64 {
        return None;
    }
    let mut hash = [0u8; 32];
    for (i, byte) in hash.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(hash)
}

/// Reads the values of a TOML table, with a warning for each one that has
/// the wrong type, and for the keys that nothing read.
struct Reader<'a> {
    table: &'a toml::Table,
    /// "", "splash.", "entry 2: "…, for the warnings.
    prefix: String,
    used: Vec<&'static str>,
}

impl<'a> Reader<'a> {
    fn new(table: &'a toml::Table, prefix: String) -> Self {
        Reader { table, prefix, used: Vec::new() }
    }

    fn value(&mut self, key: &'static str) -> Option<&'a toml::Value> {
        self.used.push(key);
        self.table.get(key)
    }

    fn str(&mut self, key: &'static str, warnings: &mut Vec<String>) -> Option<&'a str> {
        let v = self.value(key)?;
        let s = v.as_str();
        if s.is_none() {
            warnings.push(format!("{}{key}: expected a string in quotes", self.prefix));
        }
        s
    }

    fn bool(&mut self, key: &'static str, warnings: &mut Vec<String>) -> Option<bool> {
        let v = self.value(key)?;
        let b = v.as_bool();
        if b.is_none() {
            warnings.push(format!("{}{key}: expected true or false", self.prefix));
        }
        b
    }

    /// A string value that must be one of `choices`; returns its index.
    fn choice(&mut self, key: &'static str, choices: &[&str], warnings: &mut Vec<String>) -> Option<usize> {
        let s = self.str(key, warnings)?;
        let i = choices.iter().position(|c| c.eq_ignore_ascii_case(s));
        if i.is_none() {
            warnings.push(format!("{}{key}: expected one of {}", self.prefix, choices.join(", ")));
        }
        i
    }

    /// A string, or a list of strings.
    fn strings(&mut self, key: &'static str, warnings: &mut Vec<String>) -> Vec<String> {
        let Some(v) = self.value(key) else { return Vec::new() };
        if let Some(s) = v.as_str() {
            return alloc::vec![s.into()];
        }
        match v.as_array().map(|a| a.iter().map(|x| x.as_str().map(String::from)).collect::<Option<Vec<_>>>()) {
            Some(Some(list)) => list,
            _ => {
                warnings.push(format!("{}{key}: expected a string or a list of strings", self.prefix));
                Vec::new()
            }
        }
    }

    fn table(&mut self, key: &'static str, warnings: &mut Vec<String>) -> Option<&'a toml::Table> {
        let v = self.value(key)?;
        let t = v.as_table();
        if t.is_none() {
            warnings.push(format!("{}{key}: expected a [{key}] section", self.prefix));
        }
        t
    }

    fn finish(self, warnings: &mut Vec<String>) {
        for key in self.table.keys() {
            if !self.used.contains(&key.as_str()) {
                warnings.push(format!("{}{key}: unknown option", self.prefix));
            }
        }
    }
}

pub fn parse(text: &str) -> Config {
    let mut config = Config::default();
    let w = &mut Vec::new();
    let table = match text.parse::<toml::Table>() {
        Ok(t) => t,
        Err(e) => {
            config.warnings.push(format!("slingshot.toml is not valid TOML, it is ignored: {e}"));
            return config;
        }
    };
    let mut top = Reader::new(&table, String::new());

    config.defaults.theme = top.choice("theme", &THEMES, w).map(|i| i as u8);
    config.defaults.style = top.choice("style", &STYLES, w).map(|i| i as u8);
    config.defaults.accent = top.choice("accent", &ACCENTS, w).map(|i| i as u8);
    config.defaults.splash = top.choice("startup", &["menu", "splash"], w).map(|i| i == 1);
    config.defaults.remember_last = top.choice("default", &["entry", "last"], w).map(|i| i == 1);
    if let Some(v) = top.value("timeout") {
        match v.as_integer() {
            Some(t) if t >= 0 => config.defaults.timeout = Some(t.min(255) as u16),
            _ => w.push(String::from("timeout: expected a number of seconds (0 = never)")),
        }
    }
    if let Some(r) = top.str("resolution", w) {
        match parse_mode(r) {
            Some(m) => config.defaults.mode = Some(m),
            None => w.push(String::from("resolution: expected \"WIDTHxHEIGHT\", e.g. \"1920x1080\"")),
        }
    }
    if let Some(b) = top.bool("firmware-setup", w) {
        config.firmware_setup = b;
    }
    config.wallpaper = top.str("wallpaper", w).unwrap_or_default().into();
    config.language = top.str("language", w).map(String::from);
    if let Some(name) = top.str("keyboard", w) {
        match crate::parse::keymap::find(name) {
            Some(i) => config.keyboard = i,
            None => w.push(format!("keyboard: unknown layout {name:?}")),
        }
    }

    if let Some(splash) = top.table("splash", w) {
        let mut r = Reader::new(splash, String::from("splash."));
        config.splash_text = r.str("text", w).unwrap_or_default().into();
        config.splash_image = r.str("image", w).unwrap_or_default().into();
        config.splash_name = r.bool("name", w).unwrap_or(true);
        config.splash_picture = r.bool("picture", w).unwrap_or(true);
        config.splash_hint = r.bool("hint", w).unwrap_or(true);
        r.finish(w);
    }

    if let Some(settings) = top.table("settings", w) {
        let mut r = Reader::new(settings, String::from("settings."));
        let access = r.choice("access", &["shown", "hidden", "password"], w);
        let password = r.str("password", w).and_then(|p| {
            let hash = parse_hash(p);
            if hash.is_none() {
                w.push(String::from("settings.password: expected \"sha256:\" and 64 hex digits"));
            }
            hash
        });
        config.editor = r.bool("editor", w);
        r.finish(w);
        // A password alone also locks the settings. "password" without a
        // usable password hides them: nothing could unlock them.
        config.settings_access = match (access, password) {
            (Some(1), _) => SettingsAccess::Hidden,
            (Some(2), None) => {
                w.push(String::from("settings: access = \"password\" needs a password: Settings hidden"));
                SettingsAccess::Hidden
            }
            (Some(2) | None, Some(hash)) => SettingsAccess::Password(hash),
            _ => SettingsAccess::Shown,
        };
    }

    if let Some(v) = top.value("entry") {
        let entries = v.as_array().map(Vec::as_slice).unwrap_or_else(|| {
            w.push(String::from("entry: expected [[entry]] sections"));
            &[]
        });
        for (n, e) in entries.iter().enumerate() {
            let prefix = format!("entry {}: ", n + 1);
            let Some(e) = e.as_table() else {
                w.push(format!("{prefix}expected an [[entry]] section"));
                continue;
            };
            let mut r = Reader::new(e, prefix.clone());
            let pattern = r.str("match", w).map(str::trim).filter(|p| !p.is_empty());
            let linux = r.str("linux", w);
            let efi = r.str("efi", w);
            let initrd = r.strings("initrd", w);
            let mut rule = EntryRule { pattern: pattern.unwrap_or("").into(), ..Default::default() };
            rule.default = r.bool("default", w).unwrap_or(false);
            rule.title = r.str("title", w).map(Into::into);
            rule.icon = r.choice("icon", &ICONS, w).map(|i| ICONS[i]);
            rule.image = r.str("image", w).map(Into::into);
            rule.more = r.bool("more", w);
            rule.options = r.str("options", w).map(Into::into);
            r.finish(w);
            match (pattern, linux, efi) {
                (Some(_), None, None) => {}
                (None, Some(program), None) | (None, None, Some(program)) => {
                    if rule.title.is_none() {
                        w.push(format!("{prefix}title: missing (an entry of your own needs one)"));
                        continue;
                    }
                    rule.custom = Some(CustomEntry {
                        program: crate::parse::bls::efi_path(program),
                        initrd,
                        is_linux: linux.is_some(),
                    });
                }
                (Some(_), _, _) => {
                    w.push(format!("{prefix}linux and efi are for entries of your own, without match"));
                    continue;
                }
                (None, Some(_), Some(_)) => {
                    w.push(format!("{prefix}either linux or efi, not both"));
                    continue;
                }
                (None, None, None) => {
                    w.push(format!("{prefix}match (an entry to change) or linux / efi (an entry of your own) is missing"));
                    continue;
                }
            }
            config.entries.push(rule);
        }
    }
    top.finish(w);
    config.warnings = core::mem::take(w).into_iter().map(|m| format!("slingshot.toml: {m}")).collect();
    config
}

// --- Editing (the Customize dialog: change_entry) ---------------------------

/// One boot entry, as the Customize dialog edits it.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct EntryEdit {
    /// The title the firmware or systemd-boot gives it.
    pub title: String,
    /// The systemd-boot entry id, for systemd-boot entries.
    pub id: Option<String>,
    /// The title to show instead ("" = unchanged).
    pub new_title: String,
    pub icon: Option<&'static str>,
    /// An image file instead of the icon.
    pub image: Option<String>,
    pub default: bool,
    /// Under "More…", on the main row, or where Slingshot puts it.
    pub more: Option<bool>,
    /// Kernel options instead of the entry's.
    pub options: Option<String>,
}

impl EntryEdit {
    fn is_plain(&self) -> bool {
        self.new_title.is_empty()
            && self.icon.is_none()
            && self.image.is_none()
            && !self.default
            && self.more.is_none()
            && self.options.is_none()
    }
}

fn has_wildcards(pattern: &str) -> bool {
    pattern.contains(['*', '?'])
}

/// The rows for `entries` (title, systemd-boot id), from the rules
/// of `config`.
pub fn entry_edits(config: &Config, entries: &[(String, Option<String>)]) -> Vec<EntryEdit> {
    entries
        .iter()
        .map(|(title, id)| {
            let rule = config.rule_for(title, id.as_deref());
            EntryEdit {
                title: title.clone(),
                id: id.clone(),
                new_title: rule.and_then(|r| r.title.clone()).unwrap_or_default(),
                icon: rule.and_then(|r| r.icon),
                image: rule.and_then(|r| r.image.clone()),
                default: rule.is_some_and(|r| r.default),
                more: rule.and_then(|r| r.more),
                options: rule.and_then(|r| r.options.clone()),
            }
        })
        .collect()
}

/// The `[[entry]]` rules to write for the edited rows: one per row that has
/// something set (by systemd-boot id when there is one, else by title), and
/// one for a row that a wildcard rule matched, so that the row keeps what
/// the row shows. The original wildcard rules, and the rules for entries
/// that aren't on this machine now (a USB stick that isn't plugged in), are
/// kept after them.
pub fn rules_from_edits(edits: &[EntryEdit], original: &[EntryRule]) -> Vec<EntryRule> {
    let mut rules = Vec::new();
    for e in edits {
        let matched = original.iter().find(|r| r.matches(&e.title, e.id.as_deref()));
        if e.is_plain() && matched.is_none() {
            continue;
        }
        rules.push(EntryRule {
            pattern: e.id.clone().unwrap_or_else(|| e.title.clone()),
            title: (!e.new_title.is_empty()).then(|| e.new_title.clone()),
            icon: e.icon,
            image: e.image.clone(),
            default: e.default,
            more: e.more,
            options: e.options.clone(),
            custom: None,
        });
    }
    for r in original {
        let applies_to_a_row = edits.iter().any(|e| r.matches(&e.title, e.id.as_deref()));
        // Entries of your own all have an empty pattern: always keep them.
        let duplicate = r.custom.is_none() && rules.iter().any(|x| x.pattern == r.pattern);
        if (has_wildcards(&r.pattern) || !applies_to_a_row || r.custom.is_some()) && !duplicate {
            rules.push(r.clone());
        }
    }
    rules
}

/// A TOML string, quoted and escaped.
fn quoted(s: &str) -> String {
    use alloc::string::ToString;
    toml::Value::String(s.into()).to_string()
}

/// Writes `config` as slingshot.toml (comments of the original file are not
/// kept).
pub fn to_text(config: &Config) -> String {
    let mut out = String::from("# Slingshot configuration: docs/configuration.md lists the options.\n");
    let mut line = |s: String| {
        out.push_str(&s);
        out.push('\n');
    };
    let d = &config.defaults;
    if let Some(name) = d.theme.and_then(|t| THEMES.get(t as usize)) {
        line(format!("theme = {}", quoted(name)));
    }
    if let Some(name) = d.style.and_then(|t| STYLES.get(t as usize)) {
        line(format!("style = {}", quoted(name)));
    }
    if let Some(name) = d.accent.and_then(|a| ACCENTS.get(a as usize)) {
        line(format!("accent = {}", quoted(name)));
    }
    if let Some(t) = d.timeout {
        line(format!("timeout = {t}"));
    }
    if let Some(splash) = d.splash {
        line(format!("startup = {}", quoted(if splash { "splash" } else { "menu" })));
    }
    if let Some(last) = d.remember_last {
        line(format!("default = {}", quoted(if last { "last" } else { "entry" })));
    }
    if let Some((w, h)) = d.mode {
        line(format!("resolution = {}", quoted(&format!("{w}x{h}"))));
    }
    if !config.firmware_setup {
        line(String::from("firmware-setup = false"));
    }
    if let Some(language) = &config.language {
        line(format!("language = {}", quoted(language)));
    }
    if config.keyboard != 0
        && let Some((name, _)) = crate::parse::keymap::LAYOUTS.get(config.keyboard)
    {
        line(format!("keyboard = {}", quoted(name)));
    }
    if !config.wallpaper.is_empty() {
        line(format!("wallpaper = {}", quoted(&config.wallpaper)));
    }
    if !config.splash_text.is_empty() || !config.splash_image.is_empty() || !config.splash_hint || !config.splash_name || !config.splash_picture {
        line(String::from("\n[splash]"));
        if !config.splash_text.is_empty() {
            line(format!("text = {}", quoted(&config.splash_text)));
        }
        if !config.splash_image.is_empty() {
            line(format!("image = {}", quoted(&config.splash_image)));
        }
        for (key, shown) in [("name", config.splash_name), ("picture", config.splash_picture), ("hint", config.splash_hint)] {
            if !shown {
                line(format!("{key} = false"));
            }
        }
    }
    match &config.settings_access {
        SettingsAccess::Shown => {}
        SettingsAccess::Hidden => line(String::from("\n[settings]\naccess = \"hidden\"")),
        SettingsAccess::Password(hash) => {
            let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
            line(format!("\n[settings]\naccess = \"password\"\npassword = \"sha256:{hex}\""));
        }
    }
    if let Some(editor) = config.editor {
        if config.settings_access == SettingsAccess::Shown {
            line(String::from("\n[settings]"));
        }
        line(format!("editor = {editor}"));
    }
    for r in &config.entries {
        line(String::from("\n[[entry]]"));
        match &r.custom {
            None => line(format!("match = {}", quoted(&r.pattern))),
            Some(c) => {
                line(format!("{} = {}", if c.is_linux { "linux" } else { "efi" }, quoted(&c.program)));
                if !c.initrd.is_empty() {
                    let list: Vec<String> = c.initrd.iter().map(|i| quoted(i)).collect();
                    line(format!("initrd = [{}]", list.join(", ")));
                }
            }
        }
        // A rule that changes nothing (just `match`) keeps the entry as it
        // is, over a wildcard rule after it.
        if let Some(t) = &r.title {
            line(format!("title = {}", quoted(t)));
        }
        if let Some(i) = r.icon {
            line(format!("icon = {}", quoted(i)));
        }
        if let Some(i) = &r.image {
            line(format!("image = {}", quoted(i)));
        }
        if let Some(o) = &r.options {
            line(format!("options = {}", quoted(o)));
        }
        if let Some(m) = r.more {
            line(format!("more = {m}"));
        }
        if r.default {
            line(String::from("default = true"));
        }
    }
    out
}

/// The changes made to one entry in the firmware's "Customize" dialog.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct EntryChange {
    /// The title to show ("" = the entry's own).
    pub title: String,
    pub more: Option<bool>,
    pub default: bool,
    /// Kernel options, for Linux entries ("" = the entry's own).
    pub options: Option<String>,
    /// `None`: the entry's own icon.
    pub icon: Option<&'static str>,
    /// A PNG next to slingshot.toml instead of the icon.
    pub image: Option<String>,
}

/// `config` with one entry changed: an entry of the file's own (`custom`,
/// its index in `entries`), or an entry found by its title and systemd-boot
/// id. Only one entry stays the default.
pub fn change_entry(config: &Config, title: &str, id: Option<&str>, custom: Option<usize>, change: &EntryChange) -> Config {
    let mut entries = config.entries.clone();
    let changed_at = match custom.filter(|i| *i < entries.len()) {
        Some(i) => {
            let r = &mut entries[i];
            if !change.title.is_empty() {
                r.title = Some(change.title.clone());
            }
            r.more = change.more;
            r.default = change.default;
            r.icon = change.icon;
            r.image = change.image.clone();
            if let Some(o) = &change.options {
                r.options = (!o.trim().is_empty()).then(|| o.trim().into());
            }
            i
        }
        None => {
            let key = [(String::from(title), id.map(String::from))];
            let mut edit = entry_edits(config, &key).remove(0);
            edit.new_title = change.title.trim().into();
            edit.more = change.more;
            edit.default = change.default;
            edit.icon = change.icon;
            edit.image = change.image.clone();
            if let Some(o) = &change.options {
                edit.options = (!o.trim().is_empty()).then(|| o.trim().into());
            }
            entries = rules_from_edits(&[edit], &config.entries);
            // The new rule comes first (rules_from_edits puts the rows first).
            0
        }
    };
    if change.default {
        for (i, r) in entries.iter_mut().enumerate() {
            if i != changed_at {
                r.default = false;
            }
        }
    }
    // Rules left with nothing to do go, unless they keep an entry from a
    // wildcard rule after them.
    let is_plain = |r: &EntryRule| {
        r.custom.is_none()
            && !r.default
            && r.title.is_none()
            && r.icon.is_none()
            && r.image.is_none()
            && r.more.is_none()
            && r.options.is_none()
    };
    let snapshot = entries.clone();
    let mut i = 0;
    entries.retain(|r| {
        let keep = !is_plain(r)
            || snapshot[i + 1..].iter().any(|later| has_wildcards(&later.pattern) && glob_match(&later.pattern, &r.pattern));
        i += 1;
        keep
    });
    Config { entries, warnings: Vec::new(), ..config.clone() }
}

/// The SHA-256 of a password, as `settings-password` holds it.
pub fn hash_password(password: &str) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(password.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every TOML example in docs/configuration.md is understood.
    #[test]
    fn documented_examples() {
        let doc = include_str!("../../docs/configuration.md");
        for block in doc.split("```toml\n").skip(1).map(|b| b.split("```").next().unwrap()) {
            let c = parse(block);
            assert!(c.warnings.is_empty(), "{block}\n{:?}", c.warnings);
        }
        // The documented defaults are the defaults.
        let settings = doc.split("```toml\n").nth(1).unwrap().split("```").next().unwrap();
        let c = parse(settings);
        let mut d = Settings::default();
        c.defaults.apply(&mut d);
        assert_eq!(d, Settings::default());
        assert_eq!(c.keyboard, Config::default().keyboard);
        assert_eq!(c.firmware_setup, Config::default().firmware_setup);
    }

    #[test]
    fn splash() {
        let c = parse("[splash]\ntext = \"Welcome\"\nname = false\npicture = false\nhint = false\n");
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
        assert_eq!((c.splash_text.as_str(), c.splash_name, c.splash_picture, c.splash_hint), ("Welcome", false, false, false));
        let back = parse(&to_text(&c));
        assert_eq!((back.splash_name, back.splash_picture, back.splash_hint), (false, false, false));
        let d = parse("");
        assert!(d.splash_name && d.splash_picture && d.splash_hint, "shown by default");
        assert!(!to_text(&parse("")).contains("[splash]"));
    }

    #[test]
    fn language() {
        let c = parse("language = \"de\"\n");
        assert_eq!(c.language.as_deref(), Some("de"));
        assert_eq!(parse(&to_text(&c)).language.as_deref(), Some("de"));
    }

    #[test]
    fn keyboard() {
        let c = parse("keyboard = \"fr\"\n");
        assert_eq!((c.keyboard, c.warnings.len()), (2, 0));
        assert_eq!(parse(&to_text(&c)).keyboard, 2);
        assert_eq!(parse("keyboard = \"klingon\"\n").warnings.len(), 1);
    }

    #[test]
    fn wallpaper() {
        let c = parse("wallpaper = \"mountains.png\"\n");
        assert_eq!((c.wallpaper.as_str(), c.warnings.len()), ("mountains.png", 0));
        assert_eq!(parse(&to_text(&c)).wallpaper, "mountains.png");
    }

    fn edits() -> Vec<(String, Option<String>)> {
        alloc::vec![
            (String::from("Arch Linux"), Some(String::from("arch.conf"))),
            (String::from("EFI USB Device"), None),
            (String::from("EFI Network 0 for IPv4"), None),
            (String::from("Windows Boot Manager"), None),
        ]
    }

    #[test]
    fn rows_from_rules() {
        let c = parse(
            "[[entry]]\nmatch = \"EFI Network*\"\nmore = true\n\
             [[entry]]\nmatch = \"arch.conf\"\ntitle = \"Arch\"\nicon = \"linux\"\nimage = \"arch.png\"\n",
        );
        let rows = entry_edits(&c, &edits());
        assert_eq!((rows[0].new_title.as_str(), rows[0].icon), ("Arch", Some("linux")));
        assert_eq!(rows[0].image.as_deref(), Some("arch.png"));
        assert_eq!(rows[1].more, None);
        assert_eq!(rows[2].more, Some(true), "the wildcard rule");
        assert!(rows[3].is_plain());
    }

    #[test]
    fn rules_from_rows() {
        let c = parse(
            "[[entry]]\nmatch = \"EFI Network*\"\nmore = true\n\
             [[entry]]\nmatch = \"arch.conf\"\ntitle = \"Arch\"\n\
             [[entry]]\nmatch = \"EFI DVD/CDROM\"\nmore = true\n",
        );
        let mut rows = entry_edits(&c, &edits());
        rows[0].new_title.clear(); // nothing set any more
        rows[1].more = Some(true); // new rule, by title
        rows[2].more = Some(false); // on the main row, over the wildcard
        rows[3].default = true;
        rows[3].icon = Some("windows");
        let rules = rules_from_edits(&rows, &c.entries);
        let patterns: Vec<&str> = rules.iter().map(|r| r.pattern.as_str()).collect();
        // arch.conf: still written, as the rule existed (now empty); the DVD
        // rule is kept (no such entry now), and the wildcard after the rows.
        assert_eq!(
            patterns,
            ["arch.conf", "EFI USB Device", "EFI Network 0 for IPv4", "Windows Boot Manager", "EFI Network*", "EFI DVD/CDROM"]
        );
        assert_eq!(rules[2].more, Some(false));
        assert!(rules[3].default);

        // Written and read back, the rows are the same.
        let config = Config { entries: rules, ..Config::default() };
        let text = to_text(&config);
        let back = parse(&text);
        assert!(back.warnings.is_empty(), "{:?}\n{text}", back.warnings);
        assert_eq!(entry_edits(&back, &edits()), rows);
    }

    #[test]
    fn written_file_reads_back() {
        let original = parse(&format!(
            r#"theme = "light"
            style = "classic"
            accent = "pink"
            timeout = 0
            startup = "splash"
            resolution = "2256x1504"
            firmware-setup = false
            [splash]
            text = "Hi \"there\" \\ ünïcode"
            image = "s.png"
            [settings]
            password = "{HASH}"
            [[entry]]
            match = "x"
            title = "Y"
            icon = "disc"
            image = "y.png"
            default = true
            options = "root=LABEL=x quiet"
            more = false
            [[entry]]
            title = "Debug \"kernel\""
            linux = "/vmlinuz"
            initrd = ["/a.img", "/b.img"]
            options = "loglevel=7"
            more = true
            [[entry]]
            title = "Tool"
            efi = "/EFI/tool.efi"
            "#
        ));
        assert!(original.warnings.is_empty(), "{:?}", original.warnings);
        let text = to_text(&original);
        assert_eq!(parse(&text), original, "{text}");
        assert_eq!(parse(&to_text(&Config::default())), Config::default());
        let hidden = parse("[settings]\naccess = \"hidden\"");
        assert_eq!(parse(&to_text(&hidden)).settings_access, SettingsAccess::Hidden);
    }

    #[test]
    fn change_one_entry() {
        let c = parse(
            r#"
            [[entry]]
            match = "Windows*"
            default = true
            [[entry]]
            title = "Debug"
            linux = "/vmlinuz"
            [[entry]]
            match = "EFI Network*"
            more = true
            "#,
        );
        // A systemd-boot entry: a new rule, first; the old default goes.
        let change = EntryChange { title: "Arch".into(), more: Some(false), default: true, options: Some("quiet".into()), ..Default::default() };
        let new = change_entry(&c, "Arch Linux", Some("arch.conf"), None, &change);
        assert_eq!(new.entries.len(), 3);
        let arch = new.rule_for("Arch Linux", Some("arch.conf")).unwrap();
        assert_eq!((arch.pattern.as_str(), arch.title.as_deref(), arch.options.as_deref()), ("arch.conf", Some("Arch"), Some("quiet")));
        assert!(arch.default);
        assert!(new.rule_for("Windows Boot Manager", None).is_none(), "the old default's rule had nothing else: gone");
        // Written and read back.
        assert_eq!(parse(&to_text(&new)).entries, new.entries);

        // An entry of the file's own changes in place.
        let change = EntryChange { more: Some(true), options: Some("loglevel=7".into()), ..Default::default() };
        let new = change_entry(&c, "Debug", None, Some(1), &change);
        assert_eq!(new.entries.len(), 3);
        assert_eq!((new.entries[1].more, new.entries[1].command_line().as_str()), (Some(true), "loglevel=7"));
        assert!(new.entries[0].default, "not made the default: the other one stays");

        // A firmware entry under More….
        let change = EntryChange { more: Some(true), ..Default::default() };
        let new = change_entry(&c, "EFI USB Device", None, None, &change);
        assert_eq!(new.rule_for("EFI USB Device", None).unwrap().more, Some(true));

        // An icon and a picture, written and read back; then back to the
        // entry's own, which leaves nothing to keep.
        let change = EntryChange { icon: Some("usb"), image: Some("stick.png".into()), ..Default::default() };
        let new = change_entry(&c, "EFI USB Device", None, None, &change);
        let usb = new.rule_for("EFI USB Device", None).unwrap();
        assert_eq!((usb.icon, usb.image.as_deref()), (Some("usb"), Some("stick.png")));
        assert_eq!(parse(&to_text(&new)).entries, new.entries);
        let back = change_entry(&new, "EFI USB Device", None, None, &EntryChange::default());
        assert_eq!(back.entries.len(), new.entries.len() - 1);

        // Back to its own place, an entry that a wildcard rule moves: the
        // rule stays (just `match`), to win over the wildcard, also once
        // written and read back.
        let new = change_entry(&c, "EFI Network 0", None, None, &EntryChange::default());
        assert_eq!(new.rule_for("EFI Network 0", None).unwrap().more, None);
        assert_eq!(new.rule_for("EFI Network 1", None).unwrap().more, Some(true));
        assert_eq!(parse(&to_text(&new)).entries, new.entries);

        // Changing a firmware entry keeps every entry of the file's own.
        let two = parse("[[entry]]\ntitle = \"A\"\nefi = \"/a.efi\"\n[[entry]]\ntitle = \"B\"\nefi = \"/b.efi\"\n");
        let new = change_entry(&two, "Windows Boot Manager", None, None, &EntryChange { more: Some(true), ..Default::default() });
        let titles: Vec<_> = new.entries.iter().map(|r| r.title.as_deref()).collect();
        assert_eq!(titles, [None, Some("A"), Some("B")]);
    }

    #[test]
    fn password_hash() {
        let c = parse(&format!("[settings]\npassword = \"{HASH}\""));
        assert_eq!(SettingsAccess::Password(hash_password("password")), c.settings_access);
    }

    #[test]
    fn empty() {
        assert_eq!(parse(""), Config::default());
        assert_eq!(parse("# only a comment\n\n"), Config::default());
    }

    #[test]
    fn general_keys() {
        let c = parse(
            r#"
            theme = "light"
            style = "classic"
            accent = "Teal"
            startup = "splash"
            timeout = 3
            resolution = "1920x1080"
            firmware-setup = false
            [splash]
            text = "Hello = world"
            image = "splash.png"
            "#,
        );
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
        assert_eq!(
            c.defaults,
            SettingsDefaults {
                theme: Some(1),
                style: Some(3),
                accent: Some(1),
                splash: Some(true),
                remember_last: None,
                timeout: Some(3),
                mode: Some((1920, 1080))
            }
        );
        assert_eq!(c.splash_text, "Hello = world");
        assert_eq!(c.splash_image, "splash.png");
        assert!(!c.firmware_setup);
    }

    #[test]
    fn defaults_apply_to_settings() {
        let mut s = Settings::default();
        parse("timeout = 0\ntheme = \"light\"").defaults.apply(&mut s);
        assert_eq!(s, Settings { timeout: 0, theme: 1, ..Settings::default() });
    }

    #[test]
    fn bad_values_warn() {
        let c = parse(
            "theme = \"purple\"\ntimeout = \"soon\"\nresolution = \"big\"\nwhat = 1\nfirmware-setup = \"no\"\n\
             [splash]\ntext = 3\nsize = 2\n",
        );
        assert_eq!(c.warnings.len(), 7, "{:?}", c.warnings);
        assert!(c.warnings.iter().all(|w| w.starts_with("slingshot.toml: ")));
        assert!(c.warnings.iter().any(|w| w.contains("splash.size: unknown option")));
        assert_eq!(c.defaults, SettingsDefaults::default());
        assert!(c.firmware_setup);

        // A bad `entry` doesn't lose the warnings before it.
        let c = parse("timeout = \"x\"\nentry = 3\n");
        assert_eq!(c.warnings.len(), 2, "{:?}", c.warnings);
    }

    #[test]
    fn invalid_toml() {
        let c = parse("theme = dark\n");
        assert_eq!(c.warnings.len(), 1);
        assert!(c.warnings[0].contains("not valid TOML"), "{:?}", c.warnings);
        assert_eq!(Config { warnings: Vec::new(), ..c }, Config::default());
    }

    const HASH: &str = "sha256:5e884898da28047151d0e56f8dc6292773603d0d6aabbdd62a11ef721d1542d8";

    #[test]
    fn editor() {
        assert_eq!(parse("").editor, None);
        let off = parse("[settings]\neditor = false");
        assert_eq!(off.editor, Some(false));
        assert_eq!(parse(&to_text(&off)).editor, Some(false));
        let both = parse(&format!("[settings]\naccess = \"password\"\npassword = \"{HASH}\"\neditor = true"));
        let again = parse(&to_text(&both));
        assert_eq!(again.editor, Some(true));
        assert_eq!(again.settings_access, both.settings_access);
    }

    #[test]
    fn settings_access() {
        let hidden = parse("[settings]\naccess = \"hidden\"");
        assert_eq!(hidden.settings_access, SettingsAccess::Hidden);
        let c = parse(&format!("[settings]\naccess = \"password\"\npassword = \"{HASH}\""));
        let SettingsAccess::Password(h) = c.settings_access else { panic!("{:?}", c.settings_access) };
        assert_eq!(&h[..4], &[0x5e, 0x88, 0x48, 0x98]);
        // printf %s password | sha256sum
        assert!(c.check_password("password"));
        assert!(!c.check_password("Password"));
        assert!(!c.check_password(""));
        assert!(Config::default().check_password("anything"));
        // A password alone locks too.
        let alone = parse(&format!("[settings]\npassword = \"{HASH}\""));
        assert!(matches!(alone.settings_access, SettingsAccess::Password(_)));
        // Hidden wins over a password.
        let both = parse(&format!("[settings]\naccess = \"hidden\"\npassword = \"{HASH}\""));
        assert_eq!(both.settings_access, SettingsAccess::Hidden);
        // No usable password: hidden, with a warning.
        let c = parse("[settings]\naccess = \"password\"\npassword = \"hunter2\"");
        assert_eq!(c.settings_access, SettingsAccess::Hidden);
        assert_eq!(c.warnings.len(), 2);
    }

    #[test]
    fn entry_rules() {
        let c = parse(
            r#"
            timeout = 5
            [[entry]]
            match = "EFI USB Device"
            more = true
            [[entry]]
            match = "arch*.conf"
            title = "Arch Linux"
            icon = "Linux"
            image = "arch.png"
            default = true
            [[entry]]
            match = "EFI Network*"
            more = true
            timeout = 3
            "#,
        );
        assert_eq!(c.entries.len(), 3);
        assert_eq!(c.defaults.timeout, Some(5));
        assert_eq!(c.warnings.len(), 1, "timeout in an entry: {:?}", c.warnings);
        let usb = c.rule_for("efi usb device", None).unwrap();
        assert!(usb.more == Some(true) && usb.title.is_none());
        let arch = c.rule_for("Arch Linux (linux)", Some("arch-lts.conf")).unwrap();
        assert_eq!((arch.title.as_deref(), arch.icon, arch.default), (Some("Arch Linux"), Some("linux"), true));
        assert_eq!(arch.image.as_deref(), Some("arch.png"));
        assert_eq!(c.rule_for("EFI Network 0 for IPv4", None).unwrap().more, Some(true));
        assert!(c.rule_for("Windows Boot Manager", None).is_none());
        assert_eq!(parse("[[entry]]\nmatch = \"x\"\nicon = \"penguin\"").warnings.len(), 1);
        // The systems' logos are icons too.
        let c = parse("[[entry]]\nmatch = \"x\"\nicon = \"arch\"");
        assert!(c.warnings.is_empty());
        assert_eq!(c.entries[0].icon, Some("arch"));
        assert!(LOGOS.contains(&"arch") && !LOGOS.contains(&"linux"));
        let files: Vec<&str> = super::super::logos::FILES.iter().map(|(name, _)| *name).collect();
        assert_eq!(files, LOGOS);
        let no_match = parse("[[entry]]\nmore = true");
        assert_eq!((no_match.entries.len(), no_match.warnings.len()), (0, 1));
    }

    #[test]
    fn entries_of_your_own() {
        let c = parse(
            r#"
            [[entry]]
            title = "Arch Linux (debug)"
            linux = "/vmlinuz-linux"
            initrd = ["/intel-ucode.img", "/initramfs-linux.img"]
            options = "root=LABEL=arch rw loglevel=7"
            more = true
            [[entry]]
            title = "Memtest"
            efi = "/EFI/tools/memtest.efi"
            icon = "app"
            [[entry]]
            match = "arch.conf"
            options = "root=LABEL=arch rw quiet"
            more = false
            [[entry]]
            linux = "/vmlinuz"
            [[entry]]
            title = "Both"
            linux = "/a"
            efi = "/b"
            "#,
        );
        assert_eq!(c.entries.len(), 3);
        assert_eq!(c.warnings.len(), 2, "{:?}", c.warnings);
        let debug = &c.entries[0];
        let custom = debug.custom.as_ref().unwrap();
        assert!(custom.is_linux);
        assert_eq!(custom.program, "\\vmlinuz-linux");
        assert_eq!(debug.command_line(), "initrd=\\intel-ucode.img initrd=\\initramfs-linux.img root=LABEL=arch rw loglevel=7");
        assert_eq!(debug.more, Some(true));
        assert!(!debug.matches("Arch Linux (debug)", None), "custom entries don't match others");
        let memtest = &c.entries[1];
        assert_eq!((memtest.custom.as_ref().unwrap().is_linux, memtest.command_line().as_str()), (false, ""));
        let arch = c.rule_for("Arch Linux", Some("arch.conf")).unwrap();
        assert_eq!((arch.options.as_deref(), arch.more), (Some("root=LABEL=arch rw quiet"), Some(false)));
        assert_eq!(parse("[[entry]]\ntitle = \"x\"\nefi = \"/x.efi\"\ninitrd = 3").warnings.len(), 1);
    }
}
