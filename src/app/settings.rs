// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! App settings and their encoding (stored in NVRAM on UEFI).

#[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
const VERSION: u8 = 7;

/// The themes, in the order of `Theme.theme-names` (and slingshot.toml's
/// names).
pub const THEMES: [&str; 2] = ["dark", "light"];

/// How the boot entries are drawn (`Theme.style`), in its order.
pub const STYLES: [&str; 4] = ["cards", "colorful", "flat", "classic"];

/// The pointer's speeds (`SettingsPage.pointer-speed`), in its order, and
/// what they multiply the moves by.
pub const POINTER_SPEEDS: [&str; 5] = ["slower", "slow", "normal", "fast", "faster"];
pub const POINTER_FACTORS: [f32; POINTER_SPEEDS.len()] = [0.5, 0.7, 1., 1.4, 2.];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Index in `THEMES`.
    pub theme: u8,
    /// Index in `STYLES`.
    pub style: u8,
    /// Index in `POINTER_SPEEDS`.
    pub pointer_speed: u8,
    /// Fast movements of the mouse or the touchpad go further.
    pub pointer_acceleration: bool,
    /// Index in `Theme.accents`.
    pub accent: u8,
    /// Show the splash screen at startup instead of the boot menu.
    pub splash: bool,
    /// The default entry is the one started last.
    pub remember_last: bool,
    pub timeout: u16,
    /// Preferred resolution, (0, 0) if none.
    pub mode: (u16, u16),
    /// 0: automatic (slingshot.toml's, else the firmware's), else an index
    /// in `languages()` plus 1.
    pub language: u8,
    /// 0: automatic (keymap.txt, else slingshot.toml's), else an index in
    /// `keymap::LAYOUTS` plus 1.
    pub keyboard: u8,
}

/// The languages to choose from: English, then the translations.
pub fn languages() -> impl Iterator<Item = &'static str> {
    core::iter::once("en").chain(crate::tr::languages())
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: 0,
            style: 0,
            pointer_speed: 2,
            pointer_acceleration: true,
            accent: 0,
            splash: false,
            remember_last: false,
            timeout: 10,
            mode: (0, 0),
            language: 0,
            keyboard: 0,
        }
    }
}

/// Settings saved from the Settings screen, with the defaults (built-in,
/// then slingshot.toml) they were chosen over: `stamp(defaults)`. When the
/// file's defaults change after that (edited on the desktop), the file
/// wins: the most recent change counts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Saved {
    pub settings: Settings,
    /// `None` when the defaults don't matter (the desktop's, saved in the
    /// file itself).
    pub base: Option<u32>,
}

impl Saved {
    /// The saved settings if the defaults are still those they were chosen
    /// over.
    pub fn over(self, defaults: &Settings) -> Option<Settings> {
        self.base.is_none_or(|b| b == stamp(defaults)).then_some(self.settings)
    }
}

/// A checksum of `defaults` (FNV-1a of their encoding).
pub fn stamp(defaults: &Settings) -> u32 {
    // The language and the keyboard aren't defaults from slingshot.toml:
    // they don't count. The pointer only when it isn't the default, so
    // that version 6's stamps (from before it) stay valid.
    let body = defaults.body();
    let pointer = (body[9] != Settings::default().body()[9]).then_some(&body[9]);
    body[..9].iter().chain(pointer).fold(0x811c_9dc5u32, |h, b| (h ^ u32::from(*b)).wrapping_mul(0x0100_0193))
}

impl Settings {
    fn body(&self) -> [u8; 12] {
        let t = self.timeout.min(255) as u8;
        let [w0, w1] = self.mode.0.to_le_bytes();
        let [h0, h1] = self.mode.1.to_le_bytes();
        // The splash and "remember the last one" share a byte (bits 0 and 1).
        let startup = self.splash as u8 | (self.remember_last as u8) << 1;
        // The pointer's speed, and bit 4 for no acceleration.
        let pointer = self.pointer_speed | (!self.pointer_acceleration as u8) << 4;
        [self.theme, t, startup, w0, w1, h0, h1, self.accent, self.style, pointer, self.language, self.keyboard]
    }
}

// The encoding is only used by the UEFI build (NVRAM).
#[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
impl Saved {
    pub fn encode(&self) -> [u8; 17] {
        let mut data = [0; 17];
        data[0] = VERSION;
        data[1..13].copy_from_slice(&self.settings.body());
        data[13..].copy_from_slice(&self.base.unwrap_or(0).to_le_bytes());
        data
    }

    /// `None` for anything else than what `encode` writes (another
    /// version's: the defaults then), or version 6's (no pointer yet).
    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() == 16 && data[0] == 6 {
            let mut v7 = [0; 17];
            v7[0] = VERSION;
            v7[1..10].copy_from_slice(&data[1..10]);
            v7[10] = Settings::default().body()[9];
            v7[11..].copy_from_slice(&data[10..]);
            return Saved::decode(&v7);
        }
        if data.len() != 17 || data[0] != VERSION {
            return None;
        }
        let (style, pointer, language, keyboard) = (data[9], data[10], data[11], data[12]);
        let speed = pointer & 0xf;
        let settings = Settings {
            theme: if usize::from(data[1]) < THEMES.len() { data[1] } else { 0 },
            accent: data[8],
            style: if (style as usize) < STYLES.len() { style } else { 0 },
            pointer_speed: if usize::from(speed) < POINTER_SPEEDS.len() { speed } else { Settings::default().pointer_speed },
            pointer_acceleration: pointer & 0x10 == 0,
            timeout: data[2] as u16,
            splash: data[3] & 1 != 0,
            remember_last: data[3] & 2 != 0,
            mode: (u16::from_le_bytes([data[4], data[5]]), u16::from_le_bytes([data[6], data[7]])),
            // Out of range (a language since removed): automatic.
            language: if usize::from(language) <= languages().count() { language } else { 0 },
            keyboard: if usize::from(keyboard) <= crate::parse::keymap::LAYOUTS.len() { keyboard } else { 0 },
        };
        Some(Saved { settings, base: Some(u32::from_le_bytes([data[13], data[14], data[15], data[16]])) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let s = Saved {
            settings: Settings {
                theme: 1,
                style: 2,
                pointer_speed: 4,
                pointer_acceleration: false,
                accent: 3,
                splash: true,
                remember_last: true,
                timeout: 30,
                mode: (1920, 1200),
                language: 2,
                keyboard: 3,
            },
            base: Some(0x0403_0201),
        };
        assert_eq!(s.encode(), [7, 1, 30, 3, 0x80, 0x07, 0xb0, 0x04, 3, 2, 0x14, 2, 3, 1, 2, 3, 4]);
        assert_eq!(Saved::decode(&s.encode()), Some(s));
    }

    #[test]
    fn the_language_and_keyboard_dont_change_the_stamp() {
        let d = Settings { style: 1, ..Settings::default() };
        assert_eq!(stamp(&d), stamp(&Settings { language: 2, keyboard: 3, ..d }));
        let mut data = Saved { settings: Settings { language: 99, keyboard: 99, ..d }, base: None }.encode();
        let s = Saved::decode(&data).unwrap().settings;
        assert_eq!((s.language, s.keyboard), (0, 0), "unknown: automatic");
        data[11] = 1;
        assert_eq!(Saved::decode(&data).unwrap().settings.language, 1);
    }

    #[test]
    fn version_6_is_kept() {
        let old = [6, 1, 30, 3, 0x80, 0x07, 0xb0, 0x04, 3, 2, 2, 3, 1, 2, 3, 4];
        let s = Saved::decode(&old).unwrap();
        let d = Settings::default();
        assert_eq!((s.settings.theme, s.settings.keyboard, s.base), (1, 3, Some(0x0403_0201)));
        assert_eq!((s.settings.pointer_speed, s.settings.pointer_acceleration), (d.pointer_speed, d.pointer_acceleration));
        // Its stamp, made without the pointer, still matches.
        let v6_stamp = d.body()[..9].iter().fold(0x811c_9dc5u32, |h, b| (h ^ u32::from(*b)).wrapping_mul(0x0100_0193));
        assert_eq!(stamp(&d), v6_stamp);
    }

    #[test]
    fn the_style_changes_the_stamp() {
        let d = Settings::default();
        assert_ne!(stamp(&Settings { style: 3, ..d }), stamp(&d));
        assert_ne!(stamp(&Settings { pointer_speed: 3, ..d }), stamp(&d));
    }

    #[test]
    fn long_timeouts_are_capped() {
        let s = Saved { settings: Settings { timeout: 1000, ..Settings::default() }, base: None };
        assert_eq!(Saved::decode(&s.encode()).unwrap().settings.timeout, 255);
    }

    #[test]
    fn the_file_wins_when_its_defaults_change() {
        let defaults = Settings::default();
        let chosen = Settings { theme: 1, ..defaults };
        let saved = Saved { settings: chosen, base: Some(stamp(&defaults)) };
        assert_eq!(saved.over(&defaults), Some(chosen));
        let edited = Settings { timeout: 3, ..defaults };
        assert_eq!(saved.over(&edited), None, "slingshot.toml changed since: its defaults count");
        assert_eq!(Saved { base: None, ..saved }.over(&edited), Some(chosen), "no defaults recorded: the saved settings");
    }

    #[test]
    fn rejects_other_data() {
        assert_eq!(Saved::decode(&[]), None);
        assert_eq!(Saved::decode(&[7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]), None, "too short");
        assert_eq!(Saved::decode(&[5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]), None, "another version");
    }
}
