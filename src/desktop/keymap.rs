// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! `slingshot keymap`: this system's keyboard layout as a keymap.txt for the
//! firmware (see `parse::keymap`). The layout comes from `localectl` or the
//! configuration files; xkbcommon says what each key types, and the
//! system's compose table what the dead keys make.

use std::collections::BTreeSet;
use std::path::PathBuf;

use xkbcommon::xkb;

use crate::parse::keymap::{Keymap, Out, us_keys};

/// The system's XKB layout and variant (the first of a list).
pub fn system_layout() -> Option<(String, String)> {
    let first = |s: &str| s.split(',').next().unwrap_or_default().trim().trim_matches('"').to_string();
    if let Ok(out) = std::process::Command::new("localectl").arg("status").output() {
        let text = String::from_utf8_lossy(&out.stdout);
        let field = |name: &str| text.lines().find_map(|l| l.trim().strip_prefix(name)).map(first);
        if let Some(layout) = field("X11 Layout:").filter(|l| !l.is_empty() && l != "n/a") {
            return Some((layout, field("X11 Variant:").unwrap_or_default()));
        }
    }
    for file in ["/etc/vconsole.conf", "/etc/default/keyboard"] {
        let Ok(text) = std::fs::read_to_string(file) else { continue };
        let field = |name: &str| text.lines().find_map(|l| l.trim().strip_prefix(name)).map(first);
        if let Some(layout) = field("XKBLAYOUT=").filter(|l| !l.is_empty()) {
            return Some((layout, field("XKBVARIANT=").unwrap_or_default()));
        }
    }
    None
}

/// The layout's name, from XKB's list ("French (AZERTY)").
fn description(layout: &str, variant: &str) -> String {
    let list = std::fs::read_to_string("/usr/share/X11/xkb/rules/evdev.lst").unwrap_or_default();
    let mut section = "";
    for line in list.lines() {
        if let Some(s) = line.strip_prefix("! ") {
            section = s.trim();
            continue;
        }
        let mut words = line.split_whitespace();
        let (Some(name), rest) = (words.next(), words.collect::<Vec<_>>().join(" ")) else { continue };
        match section {
            "variant" if !variant.is_empty() && name == variant => {
                if let Some(d) = rest.strip_prefix(&format!("{layout}: ")) {
                    return d.into();
                }
            }
            "layout" if variant.is_empty() && name == layout => return rest,
            _ => {}
        }
    }
    if variant.is_empty() { layout.into() } else { format!("{layout} ({variant})") }
}

/// The character a dead key shows.
fn dead_char(name: &str) -> Option<char> {
    Some(match name.strip_prefix("dead_")? {
        "grave" => '`',
        "acute" => '´',
        "circumflex" => '^',
        "tilde" => '~',
        "diaeresis" => '¨',
        "cedilla" => '¸',
        "abovering" => '°',
        "caron" => 'ˇ',
        "macron" => '¯',
        "breve" => '˘',
        "abovedot" => '˙',
        "doubleacute" => '˝',
        "ogonek" => '˛',
        _ => return None,
    })
}

/// The keymap of `layout` (with `variant`).
pub fn generate(layout: &str, variant: &str) -> Result<Keymap, String> {
    let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let compile = |layout: &str, variant: &str| {
        xkb::Keymap::new_from_names(&ctx, "evdev", "pc105", layout, variant, None, xkb::KEYMAP_COMPILE_NO_FLAGS)
            .ok_or_else(|| format!("unknown keyboard layout {layout:?} {variant:?}"))
    };
    let us = compile("us", "")?;
    let keys = compile(layout, variant)?;
    let wanted: BTreeSet<(char, char)> = us_keys().collect();
    let char_of = |sym: xkb::Keysym| char::from_u32(xkb::keysym_to_utf32(sym)).filter(|c| *c != '\0');
    let first = |map: &xkb::Keymap, key: xkb::Keycode, level: u32| map.key_get_syms_by_level(key, 0, level).first().copied();

    let mut keymap = Keymap::default();
    keymap.name = description(layout, variant);
    // What a key types, and its keysym (for the compose table).
    let mut dead = Vec::new();
    let mut typed = Vec::new();
    for code in us.min_keycode().raw()..=us.max_keycode().raw() {
        let key = xkb::Keycode::new(code);
        let (Some(c0), Some(c1)) = (first(&us, key, 0).and_then(char_of), first(&us, key, 1).and_then(char_of)) else {
            continue;
        };
        if !wanted.contains(&(c0, c1)) {
            continue;
        }
        let mut out = [Out::None; 4];
        for (level, o) in out.iter_mut().enumerate() {
            let Some(sym) = first(&keys, key, level as u32) else { continue };
            if let Some(d) = dead_char(&xkb::keysym_get_name(sym)) {
                *o = Out::Dead(d);
                dead.push((d, sym));
            } else if let Some(c) = char_of(sym) {
                *o = Out::Char(c);
                typed.push((c, sym));
            }
        }
        keymap.add_key(c0, out);
    }

    // The dead keys' combinations, from the system's compose table.
    let locale = std::env::var("LANG").ok().filter(|l| l.contains("UTF-8")).unwrap_or_else(|| "en_US.UTF-8".into());
    let table = xkb::compose::Table::new_from_locale(&ctx, locale.as_ref(), xkb::compose::COMPILE_NO_FLAGS)
        .or_else(|_| xkb::compose::Table::new_from_locale(&ctx, "en_US.UTF-8".as_ref(), xkb::compose::COMPILE_NO_FLAGS));
    if let Ok(table) = table {
        dead.sort_by_key(|(d, _)| *d);
        dead.dedup_by_key(|(d, _)| *d);
        typed.sort_by_key(|(c, _)| *c);
        typed.dedup_by_key(|(c, _)| *c);
        let mut state = xkb::compose::State::new(&table, xkb::compose::STATE_NO_FLAGS);
        for &(d, dsym) in &dead {
            for &(c, csym) in &typed {
                state.reset();
                state.feed(dsym);
                state.feed(csym);
                if state.status() != xkb::compose::Status::Composed {
                    continue;
                }
                let result = state.utf8().unwrap_or_default();
                let mut chars = result.chars();
                if let (Some(r), None) = (chars.next(), chars.next())
                    && r != c
                {
                    keymap.add_compose(d, c, r);
                }
            }
        }
    }
    Ok(keymap)
}

pub const USAGE: &str = "slingshot keymap [--layout L] [--variant V] [-o FILE]
    Writes this system's keyboard layout (or L) as keymap.txt for the boot menu:
    the firmware types as on a US keyboard, Slingshot translates with it.";

pub fn run(mut args: impl Iterator<Item = String>) {
    let (mut layout, mut variant, mut output) = (None::<String>, String::new(), None::<PathBuf>);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--layout" => layout = args.next(),
            "--variant" => variant = args.next().unwrap_or_default(),
            "-o" | "--output" => output = args.next().map(PathBuf::from),
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            other => {
                eprintln!("unknown option {other}\n\n{USAGE}");
                std::process::exit(2);
            }
        }
    }
    let (layout, variant) = match layout {
        Some(l) => (l, variant),
        None => system_layout().unwrap_or_else(|| {
            eprintln!("This system's keyboard layout isn't known (localectl, /etc/vconsole.conf). Use --layout.");
            std::process::exit(1);
        }),
    };
    let keymap = generate(&layout, &variant).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });
    let text = format!("# The keyboard layout for Slingshot: {layout} {variant}\n# (slingshot keymap; see docs/configuration.md)\n{}", keymap.to_text());
    match output {
        Some(path) => std::fs::write(&path, text).unwrap_or_else(|e| {
            eprintln!("{}: {e}", path.display());
            std::process::exit(1);
        }),
        None => print!("{text}"),
    }
}
