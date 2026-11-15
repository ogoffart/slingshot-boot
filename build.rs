// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

// The glyphs are signed distance fields (`with_sdf_fonts`): one rendering
// of each glyph, scaled to every size and scale factor. Slint renders them at
// 2/3 of the largest size it's given (here or as a literal in the .slint
// files, the largest being 34): 36 makes 24 px fields, sharp enough up to the
// title at scale factor 2 (68 px). Each pixel more costs on every glyph, in
// every weight: the .efi's size.
const SDF_SIZE: &str = "36";

use rspolib::TranslatedEntry;

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
        if !po.exists() {
            continue;
        }
        println!("cargo:rerun-if-changed={}", po.display());
        let catalog = rspolib::pofile(po.as_path()).unwrap_or_else(|e| panic!("{}: {e}", po.display()));
        // Translated: not fuzzy, nor obsolete, nor empty (as Slint takes them).
        let mut pairs: Vec<(String, String)> =
            catalog.entries.into_iter().filter(|e| e.translated()).filter_map(|e| Some((e.msgid, e.msgstr?))).collect();
        pairs.sort();
        pairs.dedup_by(|a, b| a.0 == b.0);
        let items: Vec<String> = pairs.iter().map(|(id, s)| format!("({id:?}, {s:?})")).collect();
        let lang = dir.file_name().unwrap().to_string_lossy().into_owned();
        languages.push(format!("({lang:?}, &[{}])", items.join(", ")));
    }
    let code = format!("static LANGUAGES: &[(&str, &[(&str, &str)])] = &[{}];\n", languages.join(",\n"));
    std::fs::write(std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("translations.rs"), code).unwrap();
}
