// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Keyboard layouts. PC firmware translates keys as on a US keyboard, so
//! on an AZERTY keyboard the A key arrives as "q". Each layout maps the
//! character the firmware sends (the US one for that key) to the one
//! printed on the key; AltGr (right Alt, when the firmware reports it)
//! has its own table. `slingshot keymap` writes the system's own layout
//! (keymap.txt, dead keys included); the built-in tables are the fallback.

use alloc::string::String;
use alloc::vec::Vec;

/// The layouts: slingshot.toml's name, and the name shown.
pub const LAYOUTS: [(&str, &str); 6] = [
    ("us", "English (US)"),
    ("uk", "English (UK)"),
    ("fr", "French (AZERTY)"),
    ("de", "German (QWERTZ)"),
    ("es", "Spanish"),
    ("it", "Italian"),
];

// The US characters, by key: unshifted then shifted.
const US: &str = "`1234567890-=qwertyuiop[]\\asdfghjkl;'zxcvbnm,./~!@#$%^&*()_+QWERTYUIOP{}|ASDFGHJKL:\"ZXCVBNM<>?";

// The same keys on each layout (same positions as `US`).
const UK: &str = "`1234567890-=qwertyuiop[]#asdfghjkl;'zxcvbnm,./¬!\"£$%^&*()_+QWERTYUIOP{}~ASDFGHJKL:@ZXCVBNM<>?";
const FR: &str = "²&é\"'(-è_çà)=azertyuiop^$*qsdfghjklmùwxcvbn,;:!²1234567890°+AZERTYUIOP¨£µQSDFGHJKLM%WXCVBN?./§";
const DE: &str = "^1234567890ß´qwertzuiopü+#asdfghjklöäyxcvbnm,.-°!\"§$%&/()=?`QWERTZUIOPÜ*'ASDFGHJKLÖÄYXCVBNM;:_";
const ES: &str = "º1234567890'¡qwertyuiop`+çasdfghjklñ´zxcvbnm,.-ª!\"·$%&/()=?¿QWERTYUIOP^*ÇASDFGHJKLÑ¨ZXCVBNM;:_";
const IT: &str = "\\1234567890'ìqwertyuiopè+ùasdfghjklòàzxcvbnm,.-|!\"£$%&/()=?^QWERTYUIOPé*§ASDFGHJKLç°ZXCVBNM;:_";

// AltGr: (US character of the key, character).
const UK_ALTGR: &[(char, char)] = &[('4', '€')];
const FR_ALTGR: &[(char, char)] =
    &[('2', '~'), ('3', '#'), ('4', '{'), ('5', '['), ('6', '|'), ('7', '`'), ('8', '\\'), ('9', '^'), ('0', '@'), ('-', ']'), ('=', '}'), ('e', '€')];
const DE_ALTGR: &[(char, char)] =
    &[('q', '@'), ('e', '€'), ('7', '{'), ('8', '['), ('9', ']'), ('0', '}'), ('-', '\\'), (']', '~'), ('2', '²'), ('3', '³')];
const ES_ALTGR: &[(char, char)] =
    &[('1', '|'), ('2', '@'), ('3', '#'), ('4', '~'), ('e', '€'), ('[', '['), (']', ']'), ('\'', '{'), ('\\', '}')];
const IT_ALTGR: &[(char, char)] = &[('e', '€'), ('[', '['), (']', ']'), (';', '@'), ('\'', '#')];

fn tables(layout: usize) -> Option<(&'static str, &'static [(char, char)])> {
    match layout {
        1 => Some((UK, UK_ALTGR)),
        2 => Some((FR, FR_ALTGR)),
        3 => Some((DE, DE_ALTGR)),
        4 => Some((ES, ES_ALTGR)),
        5 => Some((IT, IT_ALTGR)),
        _ => None,
    }
}

/// The index in `LAYOUTS` of a name (`fr`), or of a Linux console keymap
/// (`fr-latin9`, `de-latin1-nodeadkeys`, `uk`, `gb`…). `None` if unknown.
pub fn find(name: &str) -> Option<usize> {
    let name = name.trim().to_ascii_lowercase();
    let base = name.split(['-', '_', '.']).next().unwrap_or_default();
    let base = match base {
        "gb" => "uk",
        "azerty" => "fr",
        b => b,
    };
    LAYOUTS.iter().position(|(n, _)| *n == base)
}

/// A built-in layout's name, in the current language.
pub fn layout_name(layout: usize) -> String {
    use crate::tr::tr;
    match layout {
        1 => tr("English (UK)"),
        2 => tr("French (AZERTY)"),
        3 => tr("German (QWERTZ)"),
        4 => tr("Spanish"),
        5 => tr("Italian"),
        _ => tr("English (US)"),
    }
}

/// What a key level types.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Out {
    None,
    Char(char),
    /// A dead key: combines with the next key (shown as this character).
    Dead(char),
}

#[derive(Clone, Debug, PartialEq)]
struct Key {
    /// What the firmware sends for it: the US characters, plain and Shift.
    us: [char; 2],
    /// Plain, Shift, AltGr, Shift+AltGr.
    out: [Out; 4],
}

/// A keyboard layout: for each key the firmware reports (as on a US
/// keyboard), what it types; and the dead keys' combinations.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Keymap {
    /// Shown in the passphrase dialog ("French (AZERTY)").
    pub name: String,
    keys: Vec<Key>,
    /// (dead key, next character, result)
    compose: Vec<(char, char, char)>,
}

impl Keymap {
    /// A built-in layout (index in `LAYOUTS`).
    pub fn builtin(layout: usize) -> Keymap {
        let layout = layout.min(LAYOUTS.len() - 1);
        let mut keymap = Keymap { name: layout_name(layout), ..Keymap::default() };
        let Some((chars, altgr)) = tables(layout) else { return keymap };
        let us: Vec<char> = US.chars().collect();
        let to: Vec<char> = chars.chars().collect();
        let half = us.len() / 2;
        for i in 0..half {
            let alt = |c: char| altgr.iter().find(|(k, _)| *k == c).map_or(Out::None, |(_, v)| Out::Char(*v));
            keymap.keys.push(Key {
                us: [us[i], us[half + i]],
                out: [Out::Char(to[i]), Out::Char(to[half + i]), alt(us[i]), alt(us[i])],
            });
        }
        keymap
    }

    /// A `keymap.txt` file (written by `slingshot keymap`):
    ///
    /// ```text
    /// name French (AZERTY)
    /// key q a A æ Æ           # US character, then plain, Shift, AltGr, Shift+AltGr
    /// key [ dead:^ dead:¨ - - # "-": nothing; "dead:X": a dead key
    /// compose ^ e ê           # a dead key, then a character
    /// ```
    ///
    /// Characters are themselves, or `U+XXXX` (for space and `#`).
    pub fn parse(text: &str) -> Result<Keymap, String> {
        fn char_of(s: &str) -> Option<char> {
            match s.strip_prefix("U+") {
                Some(hex) if !hex.is_empty() => u32::from_str_radix(hex, 16).ok().and_then(char::from_u32),
                _ => {
                    let mut chars = s.chars();
                    let c = chars.next()?;
                    chars.next().is_none().then_some(c)
                }
            }
        }
        fn out_of(s: &str) -> Option<Out> {
            if s == "-" {
                return Some(Out::None);
            }
            match s.strip_prefix("dead:") {
                Some(d) => char_of(d).map(Out::Dead),
                None => char_of(s).map(Out::Char),
            }
        }
        let mut keymap = Keymap::default();
        for (n, line) in text.lines().enumerate() {
            let line = line.split(" #").next().unwrap_or_default().trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let bad = || alloc::format!("keymap.txt line {}: {line:?}", n + 1);
            let mut words = line.split_ascii_whitespace();
            match words.next() {
                Some("name") => keymap.name = line[4..].trim().into(),
                Some("key") => {
                    let w: Vec<&str> = words.collect();
                    if w.len() != 5 {
                        return Err(bad());
                    }
                    let us = char_of(w[0]).ok_or_else(bad)?;
                    let mut out = [Out::None; 4];
                    for (o, s) in out.iter_mut().zip(&w[1..]) {
                        *o = out_of(s).ok_or_else(bad)?;
                    }
                    let us_shift = US.chars().position(|c| c == us).and_then(|i| US.chars().nth(i + US.chars().count() / 2));
                    keymap.keys.push(Key { us: [us, us_shift.unwrap_or(us)], out });
                }
                Some("compose") => {
                    let w: Vec<char> = words.filter_map(char_of).collect();
                    let [dead, next, result] = w[..] else { return Err(bad()) };
                    keymap.compose.push((dead, next, result));
                }
                _ => return Err(bad()),
            }
        }
        if keymap.keys.is_empty() {
            return Err(String::from("keymap.txt: no keys"));
        }
        Ok(keymap)
    }

    /// What a key typed: the firmware sent `c` (as on a US keyboard),
    /// `altgr` if the right Alt key was down; `dead` is the pending dead key
    /// (from the previous key). Returns the characters typed (none while a
    /// dead key waits, two when a dead key doesn't combine), or `None` for
    /// AltGr combinations the layout doesn't have (an Alt shortcut then).
    // The firmware's keys (and the tests).
    #[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
    pub fn type_key(&self, dead: &mut Option<char>, c: char, altgr: bool) -> Option<Vec<char>> {
        let out = match self.keys.iter().find(|k| k.us.contains(&c)) {
            Some(key) => {
                let shift = key.us[0] != c;
                let o = key.out[usize::from(shift) + if altgr { 2 } else { 0 }];
                if altgr && o == Out::None {
                    return None;
                }
                o
            }
            None if altgr => return None,
            None => Out::Char(c),
        };
        let mut typed = Vec::new();
        match (dead.take(), out) {
            (_, Out::None) => {}
            (Some(d), Out::Char(c)) => match self.compose.iter().find(|(k, n, _)| *k == d && *n == c) {
                Some((_, _, r)) => typed.push(*r),
                // A dead key then space types the accent itself.
                None if c == ' ' => typed.push(d),
                None => typed.extend([d, c]),
            },
            (Some(d), Out::Dead(e)) => {
                typed.push(d);
                *dead = Some(e);
            }
            (None, Out::Char(c)) => typed.push(c),
            (None, Out::Dead(e)) => *dead = Some(e),
        }
        Some(typed)
    }

    /// As `keymap.txt` (see `parse`).
    // `slingshot keymap` (desktop only).
    #[cfg_attr(target_os = "uefi", allow(dead_code))]
    pub fn to_text(&self) -> String {
        fn ch(c: char) -> String {
            if c.is_whitespace() || c == '#' || c.is_control() {
                alloc::format!("U+{:04X}", c as u32)
            } else {
                String::from(c)
            }
        }
        let mut text = alloc::format!("name {}\n", self.name);
        for key in &self.keys {
            text.push_str("key ");
            text.push_str(&ch(key.us[0]));
            for o in key.out {
                text.push(' ');
                text.push_str(&match o {
                    Out::None => String::from("-"),
                    Out::Char(c) => ch(c),
                    Out::Dead(c) => alloc::format!("dead:{}", ch(c)),
                });
            }
            text.push('\n');
        }
        for (d, n, r) in &self.compose {
            text.push_str(&alloc::format!("compose {} {} {}\n", ch(*d), ch(*n), ch(*r)));
        }
        text
    }

    /// For `slingshot keymap`: a key (by its US character), what it types.
    // `slingshot keymap` (desktop only).
    #[cfg_attr(target_os = "uefi", allow(dead_code))]
    pub fn add_key(&mut self, us: char, out: [Out; 4]) {
        let us_shift = US.chars().position(|c| c == us).and_then(|i| US.chars().nth(i + US.chars().count() / 2));
        self.keys.push(Key { us: [us, us_shift.unwrap_or(us)], out });
    }

    // `slingshot keymap` (desktop only).
    #[cfg_attr(target_os = "uefi", allow(dead_code))]
    pub fn add_compose(&mut self, dead: char, next: char, result: char) {
        self.compose.push((dead, next, result));
    }
}

/// The US characters of the keys, plain then Shift (for `slingshot keymap`,
/// desktop only).
#[cfg_attr(target_os = "uefi", allow(dead_code))]
pub fn us_keys() -> impl Iterator<Item = (char, char)> {
    let n = US.chars().count() / 2;
    US.chars().take(n).zip(US.chars().skip(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(keymap: &Keymap, keys: &[(char, bool)]) -> String {
        let mut dead = None;
        keys.iter().flat_map(|&(c, altgr)| keymap.type_key(&mut dead, c, altgr).unwrap_or_default()).collect()
    }

    fn plain(keymap: &Keymap, us: &str) -> String {
        typed(keymap, &us.chars().map(|c| (c, false)).collect::<Vec<_>>())
    }

    #[test]
    fn tables_line_up() {
        let n = US.chars().count();
        for (layout, (name, _)) in LAYOUTS.iter().enumerate().skip(1) {
            assert_eq!(tables(layout).unwrap().0.chars().count(), n, "{name}");
        }
    }

    #[test]
    fn azerty() {
        let fr = Keymap::builtin(find("fr").unwrap());
        // The keys printed A, M and comma send US "q", ";" and "m".
        assert_eq!(plain(&fr, "q;m!2"), "am,1é");
        assert_eq!(typed(&fr, &[('0', true), ('E', true)]), "@€");
        assert_eq!(fr.type_key(&mut None, 'x', true), None, "no AltGr+X: an Alt shortcut");
    }

    #[test]
    fn qwertz() {
        let de = Keymap::builtin(find("de-latin1-nodeadkeys").unwrap());
        assert_eq!(plain(&de, "yZ@"), "zY\"");
        assert_eq!(typed(&de, &[('q', true)]), "@");
    }

    #[test]
    fn us_and_names() {
        let us = Keymap::builtin(0);
        assert_eq!(plain(&us, "q;"), "q;");
        assert_eq!(us.type_key(&mut None, 'q', true), None);
        assert_eq!(find("gb"), find("uk"));
        assert_eq!(find("UK"), Some(1));
        assert_eq!(find("dvorak"), None);
        assert_eq!(plain(&Keymap::builtin(find("uk").unwrap()), "\""), "@");
    }

    #[test]
    fn file_with_dead_keys() {
        let text = "# from slingshot keymap\nname French (bépo-ish)\nkey q a A æ Æ\nkey [ dead:^ dead:¨ - -\nkey e e E € -\nkey U+0020 U+0020 U+0020 - -\ncompose ^ e ê\n";
        let k = Keymap::parse(text).unwrap();
        assert_eq!(k.name, "French (bépo-ish)");
        assert_eq!(plain(&k, "q[e"), "aê");
        assert_eq!(plain(&k, "[q"), "^a", "no combination: both");
        assert_eq!(plain(&k, "[ "), "^", "then space: the accent");
        assert_eq!(plain(&k, "{e"), "¨e", "Shift: the other dead key");
        assert_eq!(typed(&k, &[('e', true)]), "€");
        assert_eq!(plain(&k, "z"), "z", "keys it doesn't list stay as they are");
        assert_eq!(Keymap::parse(&k.to_text()), Ok(k));
        assert!(Keymap::parse("key q a").is_err());
        assert!(Keymap::parse("name only").is_err());
    }
}
