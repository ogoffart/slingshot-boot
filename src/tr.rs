// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Translations. The .slint files mark their texts with `@tr()`; the texts
//! made in Rust use `tr()` and `trf()`. Both come from the same files,
//! `translations/<lang>/LC_MESSAGES/slingshot.po` (updated by
//! `scripts/update-translations.py`): Slint bundles them, and build.rs
//! compiles them into `LANGUAGES` for the Rust side.

use alloc::string::{String, ToString};
use core::fmt::Display;
use core::sync::atomic::{AtomicUsize, Ordering};

// `LANGUAGES: &[(&str, &[(&str, &str)])]`: per language, its
// translations sorted by the English text.
include!(concat!(env!("OUT_DIR"), "/translations.rs"));

/// The index in `LANGUAGES`, or `usize::MAX` for English.
static CURRENT: AtomicUsize = AtomicUsize::new(usize::MAX);

/// The languages there are translations for ("de", "fr", "zh"), English aside.
pub fn languages() -> impl Iterator<Item = &'static str> {
    LANGUAGES.iter().map(|(l, _)| *l)
}

/// The language code a locale or language tag selects ("fr" for "fr_CH.UTF-8"
/// or "fr-FR"): one of `languages()`, or "en".
pub fn language_of(locale: &str) -> &'static str {
    let base = locale.split(['_', '-', '.', '@']).next().unwrap_or_default().to_ascii_lowercase();
    languages().find(|l| *l == base).unwrap_or("en")
}

/// Switches the language (a code from `language_of`): the Rust texts from
/// now on, and the .slint ones right away. Call it after creating the
/// window.
pub fn set_language(language: &str) {
    let index = LANGUAGES.iter().position(|(l, _)| *l == language).unwrap_or(usize::MAX);
    CURRENT.store(index, Ordering::Relaxed);
    let _ = slint::select_bundled_translation(if index == usize::MAX { "" } else { language });
}

/// The current language ("en" when not translated).
pub fn language() -> &'static str {
    LANGUAGES.get(CURRENT.load(Ordering::Relaxed)).map_or("en", |(l, _)| l)
}

fn lookup(text: &str) -> &str {
    lookup_in(CURRENT.load(Ordering::Relaxed), text)
}

fn lookup_in(language: usize, text: &str) -> &str {
    let Some((_, table)) = LANGUAGES.get(language) else { return text };
    match table.binary_search_by(|(id, _)| (*id).cmp(text)) {
        Ok(i) => table[i].1,
        Err(_) => text,
    }
}

/// `text` in the current language.
pub fn tr(text: &str) -> String {
    lookup(text).to_string()
}

/// `text` in the current language, with each `{}` replaced by the next of
/// `args` (in the translation's order: `{0}`, `{1}`… pick one).
pub fn trf(text: &str, args: &[&dyn Display]) -> String {
    let translated = lookup(text);
    let mut out = String::new();
    let mut next = 0;
    let mut rest = translated;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let start = 0;
        let Some(end) = rest.find('}') else { break };
        let inner = &rest[start + 1..start + end];
        let index = if inner.is_empty() {
            next += 1;
            Some(next - 1)
        } else {
            inner.parse().ok()
        };
        match index.and_then(|i| args.get(i)) {
            Some(arg) => out.push_str(&alloc::format!("{arg}")),
            None => out.push_str(&rest[start..start + end + 1]),
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting() {
        assert_eq!(trf("Disk {} of {}", &[&1, &2]), "Disk 1 of 2");
        assert_eq!(trf("{1} then {0}", &[&"a", &"b"]), "b then a");
        assert_eq!(trf("no {braces", &[&1]), "no {braces");
    }

    #[test]
    fn languages_and_lookup() {
        assert_eq!(language_of("fr_CH.UTF-8"), "fr");
        assert_eq!(language_of("de-DE"), "de");
        assert_eq!(language_of("C"), "en");
        // Not through set_language: the tests run in parallel.
        let fr = LANGUAGES.iter().position(|(l, _)| *l == "fr").unwrap();
        assert_eq!(lookup_in(fr, "Restart"), "Redémarrer");
        assert_eq!(lookup_in(fr, "not translated"), "not translated");
        assert_eq!(lookup_in(usize::MAX, "Restart"), "Restart");
        assert!(LANGUAGES.iter().all(|(_, t)| t.windows(2).all(|w| w[0].0 < w[1].0)), "sorted");
    }
}
