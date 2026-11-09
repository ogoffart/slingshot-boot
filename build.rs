// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

// The glyphs are signed distance fields (`with_sdf_fonts`): one rendering
// of each glyph, scaled to every size and scale factor. Slint renders them at
// 2/3 of the largest size it's given (here or as a literal in the .slint
// files, the largest being 34): 36 makes 24 px fields, sharp enough up to the
// title at scale factor 2 (68 px). Each pixel more costs on every glyph, in
// every weight: the .efi's size.
const SDF_SIZE: &str = "36";

fn main() {
    // SAFETY: build scripts are single-threaded.
    unsafe { std::env::set_var("SLINT_FONT_SIZES", SDF_SIZE) };
    println!("cargo:rerun-if-changed=build.rs");
    // Characters Inter doesn't have (Chinese) come from the fallback fonts,
    // which are the system's unless given: give ours, so that every build
    // has them. Inter stays the primary font.
    let fonts = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ui/fonts");
    // SAFETY: as above.
    unsafe {
        std::env::set_var("SLINT_DEFAULT_FONT", fonts.join("InterVariable.ttf"));
        std::env::set_var("SLINT_FONT_PATH", fonts.join("NotoSansSC-subset.ttf"));
    }

    translations();

    // slint-project.json sets the main file and the edition, for this build
    // and for the editor's live preview.
    println!("cargo:rerun-if-changed=slint-project.json");
    // Embedding for the software renderer drops the accessibility properties,
    // which the UI tests need: their build loads the fonts at run time instead.
    let embed = || {
        if std::env::var_os("CARGO_FEATURE_SYSTEM_TESTING").is_some() {
            slint_build::EmbedResourcesKind::EmbedFiles
        } else {
            slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer
        }
    };
    slint_build::compile_with_config(
        "slint-project.json",
        slint_build::CompilerConfiguration::new()
            .embed_resources(embed())
            .with_sdf_fonts(true)
            // translations/<lang>/LC_MESSAGES/slingshot.po, without
            // contexts: each text is translated the same everywhere.
            .with_bundled_translations("translations")
            .with_default_translation_context(slint_build::DefaultTranslationContext::None),
    )
    .unwrap();
}

/// translations/<lang>/LC_MESSAGES/slingshot.po, for the texts made in Rust
/// (src/tr.rs): `LANGUAGES`, each with its (English, translation) pairs
/// sorted by the English text. Slint bundles the same files for @tr().
fn translations() {
    let root = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("translations");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut languages = Vec::new();
    let mut dirs: Vec<_> = std::fs::read_dir(&root).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    dirs.sort();
    for dir in dirs {
        let po = dir.join("LC_MESSAGES/slingshot.po");
        let Ok(text) = std::fs::read_to_string(&po) else { continue };
        println!("cargo:rerun-if-changed={}", po.display());
        let mut pairs = parse_po(&text);
        pairs.retain(|(id, s)| !id.is_empty() && !s.is_empty());
        pairs.sort();
        pairs.dedup_by(|a, b| a.0 == b.0);
        let items: Vec<String> = pairs.iter().map(|(id, s)| format!("({id:?}, {s:?})")).collect();
        let lang = dir.file_name().unwrap().to_string_lossy().into_owned();
        languages.push(format!("({lang:?}, &[{}])", items.join(", ")));
    }
    let code = format!("static LANGUAGES: &[(&str, &[(&str, &str)])] = &[{}];\n", languages.join(",\n"));
    std::fs::write(std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("translations.rs"), code).unwrap();
}

/// The (msgid, msgstr) pairs of a .po file; fuzzy entries are left out.
fn parse_po(text: &str) -> Vec<(String, String)> {
    fn unquote(s: &str) -> String {
        let s = s.trim();
        let s = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or("");
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(c) => out.push(c),
                None => {}
            }
        }
        out
    }
    let mut pairs = Vec::new();
    let (mut id, mut str_, mut fuzzy) = (String::new(), String::new(), false);
    let mut field = 0; // 1: msgid, 2: msgstr
    let mut flush = |id: &mut String, str_: &mut String, fuzzy: &mut bool| {
        if !*fuzzy {
            pairs.push((std::mem::take(id), std::mem::take(str_)));
        }
        id.clear();
        str_.clear();
        *fuzzy = false;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("#,") && line.contains("fuzzy") {
            fuzzy = true;
        } else if let Some(rest) = line.strip_prefix("msgid ") {
            if field == 2 {
                flush(&mut id, &mut str_, &mut fuzzy);
            }
            id = unquote(rest);
            field = 1;
        } else if let Some(rest) = line.strip_prefix("msgstr ") {
            str_ = unquote(rest);
            field = 2;
        } else if line.starts_with('"') {
            match field {
                1 => id.push_str(&unquote(line)),
                2 => str_.push_str(&unquote(line)),
                _ => {}
            }
        }
    }
    if field == 2 {
        flush(&mut id, &mut str_, &mut fuzzy);
    }
    pairs
}
