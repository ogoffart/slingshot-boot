// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Slingshot's UI and logic. The machine is reached only through
//! [`Backend`], so the same code runs on UEFI and in the desktop simulator.

mod backdrop;
mod backend;
mod cards;
pub mod config;
mod hub;
mod logos;
mod power;
mod settings;
mod store;
mod unlock;

pub use backend::*;
use cards::{Action, Card, Level, apply_rules, browse_card, build_cards, view};
use config::{Config, ICONS, SettingsAccess};
use store::{commit_config, initial_settings, reset_settings, save_settings};
pub use hub::*;
pub use settings::*;

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use core::cell::RefCell;
use core::time::Duration;

use crate::tr::{tr, trf};
use slint::{Color, ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};

slint::include_modules!();

fn boot_entry(title: &str, subtitle: &str, kind: &str) -> BootEntry {
    BootEntry { title: title.into(), subtitle: subtitle.into(), kind: kind.into(), ..Default::default() }
}

fn card_entry(card: &Card, images: &BTreeMap<String, slint::Image>) -> BootEntry {
    let mut entry = boot_entry(&card.title, &card.subtitle, card.kind);
    entry.editable = card.options().is_some();
    entry.customizable = matches!(card.action, Action::Start(_) | Action::FirmwareSetup);
    // A picture chosen for it, else a logo chosen, else its system's
    // logo, unless an icon was chosen.
    let logo = if card.icon_chosen { config::LOGOS.contains(&card.kind).then_some(card.kind) } else { logos::logo_for(card) };
    let image = match &card.image {
        Some(f) => images.get(f),
        None => logo.and_then(|name| images.get(&format!("os:{name}"))),
    };
    if let Some(image) = image {
        entry.image = image.clone();
        entry.has_image = true;
    }
    entry
}

/// The entry being started, for the passphrase screen (if it asks).
fn show_unlock_entry(ui: &AppWindow, card: &Card, st: &State) {
    ui.global::<Passphrase>().set_entry(card_entry(card, &st.images));
}

fn option_entry(option: &BootOption) -> BootEntry {
    boot_entry(&option.title, &option.subtitle, option.kind)
}

/// The keyboard layout chosen in Settings (`choice`), else keymap.txt (the
/// installed system's, written by `slingshot keymap`), else slingshot.toml's
/// `keyboard`.
fn keymap(backend: &dyn Backend, config: &Config, choice: u8) -> crate::parse::keymap::Keymap {
    use crate::parse::keymap::{Keymap, LAYOUTS};
    let chosen = usize::from(choice).checked_sub(1).filter(|&i| i < LAYOUTS.len());
    let keymap = chosen.map(Keymap::builtin).or_else(|| {
        let Some(data) = backend.read_file("keymap.txt") else {
            log::info!("no keymap.txt");
            return None;
        };
        Keymap::parse(&String::from_utf8_lossy(&data)).inspect_err(|e| log::warn!("{e}")).ok()
    });
    keymap.unwrap_or_else(|| Keymap::builtin(config.keyboard.min(LAYOUTS.len() - 1)))
}

/// Types with that layout (see `keymap`) from now on.
fn use_keymap(backend: &dyn Backend, hub: &Hub, config: &Config, choice: u8) {
    let keymap = keymap(backend, config, choice);
    log::info!("keyboard layout: {}", keymap.name);
    *hub.keymap.borrow_mut() = Some(keymap);
}

/// Moves the pointer at the speed of `settings` (`POINTER_FACTORS`), and
/// with its acceleration, fast moves further.
fn use_pointer(hub: &Hub, settings: &Settings) {
    let factor = settings::POINTER_FACTORS[usize::from(settings.pointer_speed)];
    log::info!("pointer: speed ×{factor}, acceleration {}", if settings.pointer_acceleration { "on" } else { "off" });
    hub.pointer.set(Some((factor, settings.pointer_acceleration)));
}

/// The screen animations, after the slowest full redraw: off when it's
/// slower than a frame at slingshot.toml's `animations-fps` (0: always on).
/// Screens take more or less time to draw: the slowest one's transitions
/// would stutter, and deciding on each would turn them on and off at every
/// screen.
fn update_animations(ui: &AppWindow, st: &mut State) {
    let Some(took) = st.full_redraw else { return };
    let fps = st.config.animations_fps;
    let slow = fps > 0 && took > Duration::from_secs(1) / u32::from(fps);
    if slow != st.slow_rendering {
        log::info!("display: the slowest full redraw takes {} ms: {} {fps} frames per second", took.as_millis(), if slow { "under" } else { "at least" });
        st.slow_rendering = slow;
        ui.global::<Theme>().set_motion(!slow);
    }
}

/// The language chosen in Settings (`choice`, see `Settings::language`):
/// none for "Automatic".
fn chosen_language(choice: u8) -> Option<&'static str> {
    usize::from(choice).checked_sub(1).and_then(|i| settings::languages().nth(i))
}

/// The language "Automatic" means: slingshot.toml's, else the firmware's.
fn automatic_language(backend: &dyn Backend, config: &Config) -> String {
    config.language.clone().or_else(|| backend.language()).unwrap_or_default()
}

/// The language to use: chosen in Settings, else the automatic one.
fn use_language(backend: &dyn Backend, config: &Config, choice: u8) {
    let wanted = chosen_language(choice).map_or_else(|| automatic_language(backend, config), String::from);
    crate::tr::set_language(crate::tr::language_of(&wanted));
    log::info!("language: {} (asked: {wanted:?})", crate::tr::language());
}

/// A language in itself: "Français" for French.
fn language_name(code: &str) -> &str {
    match code {
        "en" => "English",
        "fr" => "Français",
        "de" => "Deutsch",
        "zh" => "中文",
        other => other,
    }
}

/// The choices of the Settings' language and keyboard pickers, in the
/// current language: "Automatic (what it is here)" first.
fn show_language_choices(ui: &AppWindow, backend: &dyn Backend, config: &Config, settings: &Settings) {
    let auto = {
        language_name(crate::tr::language_of(&automatic_language(backend, config)))
    };
    let languages: Vec<SharedString> =
        core::iter::once(trf("Automatic ({})", &[&auto])).chain(settings::languages().map(|l| String::from(language_name(l)))).map(SharedString::from).collect();
    let auto = keymap(backend, config, 0).name;
    let keyboards: Vec<SharedString> = core::iter::once(trf("Automatic ({})", &[&auto]))
        .chain((0..crate::parse::keymap::LAYOUTS.len()).map(crate::parse::keymap::layout_name))
        .map(SharedString::from)
        .collect();
    let page = ui.global::<SettingsPage>();
    page.set_language_names(ModelRc::new(VecModel::from(languages)));
    page.set_keyboard_names(ModelRc::new(VecModel::from(keyboards)));
    page.set_language_index(settings.language.into());
    page.set_keyboard_index(settings.keyboard.into());
}

fn read_config(backend: &dyn Backend) -> Config {
    let Some(data) = backend.read_file("slingshot.toml") else { return Config::default() };
    let config = config::parse(&String::from_utf8_lossy(&data));
    for w in &config.warnings {
        log::warn!("{w}");
    }
    config
}

/// Loads the images that the `[[entry]]` rules name.
fn entry_images(ui: &AppWindow, backend: &dyn Backend, config: &Config) -> BTreeMap<String, slint::Image> {
    let mut images = BTreeMap::new();
    // The systems' logos, as "os:<name>", decoded once for the cards and
    // the icon picker.
    let logos: Vec<Logo> = logos::FILES
        .iter()
        .map(|(name, png)| Logo { name: (*name).into(), image: decode_picture(png).unwrap_or_default() })
        .collect();
    for logo in &logos {
        images.insert(format!("os:{}", logo.name), logo.image.clone());
    }
    ui.global::<Logos>().set_all(Rc::new(VecModel::from(logos)).into());
    for file in config.entries.iter().filter_map(|r| r.image.as_ref()) {
        if load_picture(backend, &mut images, file).is_none() {
            log::warn!("cannot read the entry image {file}");
        }
    }
    images
}

/// A PNG file next to slingshot.toml, read once.
fn load_picture(backend: &dyn Backend, images: &mut BTreeMap<String, slint::Image>, file: &str) -> Option<slint::Image> {
    if let Some(image) = images.get(file) {
        return Some(image.clone());
    }
    let image = backend.read_file(file).as_deref().and_then(decode_picture)?;
    images.insert(file.into(), image.clone());
    Some(image)
}

/// Shows picture `picture` of the list (0 = none) in the Customize dialog.
fn show_picture_preview(ui: &AppWindow, st: &mut State, backend: &dyn Backend, picture: usize) {
    let file = picture.checked_sub(1).and_then(|i| st.pictures.get(i)).cloned();
    let image = file.and_then(|f| load_picture(backend, &mut st.images, &f));
    ui.global::<Customize>().set_has_picture_preview(image.is_some());
    ui.global::<Customize>().set_picture_preview(image.unwrap_or_default());
}

/// Shows the wallpaper (or none: an empty image), with its blurred copy for
/// the cards (frosted glass) and its accent color.
fn show_wallpaper(ui: &AppWindow, image: slint::Image) {
    let theme = ui.global::<Theme>();
    let source = image.to_rgba8();
    ui.global::<SettingsPage>().set_wallpaper(image);
    let Some(source) = source else {
        theme.set_backdrop(slint::Image::default());
        theme.set_has_wallpaper_accent(false);
        return;
    };
    let pixels = backdrop::Pixels {
        width: source.width() as usize,
        height: source.height() as usize,
        data: source.as_slice().iter().map(|p| [p.r, p.g, p.b, p.a]).collect(),
    };
    // Small: the blur is cheap, and it's drawn stretched anyway.
    let small = backdrop::shrink(&pixels, 480);
    let blurred = backdrop::blur(&small, 3);
    let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(blurred.width as u32, blurred.height as u32);
    for (out, p) in buffer.make_mut_slice().iter_mut().zip(&blurred.data) {
        *out = slint::Rgba8Pixel { r: p[0], g: p[1], b: p[2], a: p[3] };
    }
    theme.set_backdrop(slint::Image::from_rgba8(buffer));
    match backdrop::accent(&small) {
        Some([r, g, b]) => {
            theme.set_wallpaper_accent(Color::from_rgb_u8(r, g, b));
            theme.set_has_wallpaper_accent(true);
        }
        None => theme.set_has_wallpaper_accent(false),
    }
}

/// The pictures of slingshot.toml that the Settings screen chooses.
#[derive(Clone, Copy)]
enum ScreenPicture {
    Splash,
    Wallpaper,
}

impl ScreenPicture {
    fn file(self, config: &Config) -> &str {
        match self {
            ScreenPicture::Splash => &config.splash_image,
            ScreenPicture::Wallpaper => &config.wallpaper,
        }
    }

    fn with(self, config: &Config, file: String) -> Config {
        let mut new = config.clone();
        match self {
            ScreenPicture::Splash => new.splash_image = file,
            ScreenPicture::Wallpaper => new.wallpaper = file,
        }
        new
    }

    fn show(self, ui: &AppWindow, image: slint::Image) {
        match self {
            ScreenPicture::Splash => ui.global::<SettingsPage>().set_splash_image(image),
            ScreenPicture::Wallpaper => show_wallpaper(ui, image),
        }
    }

    fn set_index(self, ui: &AppWindow, index: i32) {
        match self {
            ScreenPicture::Splash => ui.global::<SettingsPage>().set_splash_picture(index),
            ScreenPicture::Wallpaper => ui.global::<SettingsPage>().set_wallpaper_picture(index),
        }
    }
}

/// The position of `file` in a picture list with "None" first: 0 when
/// there's no file.
fn picture_index(pictures: &[String], file: Option<&str>) -> usize {
    file.and_then(|f| pictures.iter().position(|p| p.eq_ignore_ascii_case(f))).map_or(0, |i| i + 1)
}

/// Decodes a PNG or a JPEG file into an image (the formats the `png` and
/// `jpeg` features enable).
/// The largest picture decoded, each way: a file's header can say 65535
/// × 65535 (16 GiB of pixels).
const MAX_PICTURE: u32 = 8192;

fn decode_picture(data: &[u8]) -> Option<slint::Image> {
    if data.starts_with(&[0xff, 0xd8]) {
        #[cfg(feature = "jpeg")]
        return decode_jpeg(data);
    } else if data.starts_with(b"\x89PNG") {
        #[cfg(feature = "png")]
        return decode_png(data);
    }
    None
}

#[cfg(feature = "jpeg")]
fn decode_jpeg(data: &[u8]) -> Option<slint::Image> {
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        .set_max_width(MAX_PICTURE as usize)
        .set_max_height(MAX_PICTURE as usize);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(zune_core::bytestream::ZCursor::new(data), options);
    let pixels = decoder.decode().ok()?;
    let (width, height) = decoder.dimensions()?;
    let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&pixels, width as u32, height as u32);
    Some(slint::Image::from_rgba8(buffer))
}

#[cfg(feature = "png")]
fn decode_png(data: &[u8]) -> Option<slint::Image> {
    let header = minipng::decode_png_header(data).ok()?;
    if header.width() > MAX_PICTURE || header.height() > MAX_PICTURE {
        return None;
    }
    let mut buffer = alloc::vec![0; header.required_bytes_rgba8bpc()];
    let mut png = minipng::decode_png(data, &mut buffer).ok()?;
    png.convert_to_rgba8bpc().ok()?;
    let pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
        png.pixels(),
        png.width(),
        png.height(),
    );
    Some(slint::Image::from_rgba8(pixels))
}


fn mode_label(w: usize, h: usize) -> SharedString {
    format!("{w} × {h}").into()
}

struct State {
    /// The full BootOrder as read at startup.
    original_order: Vec<u16>,
    /// The BootOrder currently in the firmware (changes after a save).
    firmware_order: Vec<u16>,
    /// Visible entries in firmware order.
    options: Vec<BootOption>,
    loader_entries: Option<LoaderConfig>,
    found: Vec<FoundEntry>,
    firmware_setup_supported: bool,
    /// All the boot picker's cards, main row and "More…".
    all_cards: Vec<Card>,
    /// The default card, in `all_cards`.
    default_card: usize,
    /// The cards shown: the main row or the "More…" level.
    cards: Vec<Card>,
    /// The order being edited in Settings (indices into `options`).
    edited: Vec<usize>,
    settings: Settings,
    settings_dirty: bool,
    /// How long the slowest full redraw took (since the resolution changed),
    /// and whether that's too slow for the screen animations.
    full_redraw: Option<Duration>,
    slow_rendering: bool,
    /// Index in the mode list shown in the Settings resolution picker.
    mode_candidate: usize,
    /// The resolution to go back to if the new one isn't confirmed.
    revert_to: Option<(usize, usize)>,
    revert_left: i32,
    countdown_left: i32,
    /// The default entry's passphrase was asked for at startup (once: not
    /// again when its start is cancelled or fails).
    passphrase_asked: bool,
    /// The firmware connects the disks after the first frame
    /// (`Hub::devices_connected`); the desktop has them all at once.
    devices_connected: bool,
    /// On the splash, its countdown held until the disks are connected: the
    /// default entry's encrypted disk may be among them, and its passphrase
    /// is then asked instead.
    waiting_for_disks: bool,
    config: Rc<Config>,
    /// The settings without anything saved: built-in, then slingshot.toml.
    defaults: Settings,
    /// The card started last (`Card::key`).
    last_started: Option<String>,
    /// The system that hibernated, if one did: started again first.
    hibernated: Option<crate::parse::hibernate::Hibernated>,
    /// The built-in settings (with the firmware's timeout).
    builtin: Settings,
    /// "Start a file…" works here.
    can_browse: bool,
    /// The cards that just appeared (title and subtitle), shown animated
    /// once: a USB stick plugged in.
    arrived: Vec<(String, String)>,
    /// The pictures read so far (the entries', the previews), by file name.
    images: BTreeMap<String, slint::Image>,
    /// The PNG files next to Slingshot, as `picture-names` lists them
    /// after "None".
    pictures: Vec<String>,
    browse: Browse,
}

/// What a picture is chosen for.
#[derive(Clone, Copy)]
enum PictureFor {
    /// The entry being customized.
    Entry,
    Screen(ScreenPicture),
}

/// The file browser ("Start a file…", or choosing a picture).
#[derive(Default)]
struct Browse {
    /// Choosing a picture (else EFI programs, to start).
    pick: Option<PictureFor>,
    volumes: Vec<Volume>,
    /// The partitions the firmware can't read, shown after the disks.
    unreadable: Vec<Volume>,
    /// The disk being browsed, and the folders opened on it.
    volume: Option<usize>,
    folders: Vec<String>,
    /// What the rows shown are.
    rows: Vec<BrowseRow>,
}

#[derive(Clone, Debug)]
enum BrowseRow {
    Volume(usize),
    Unreadable(usize),
    Up,
    Folder(String),
    File(String),
}

/// The path of `file` in `folders` (`\` for the root).
fn folder_path(folders: &[String], file: Option<&str>) -> String {
    let mut path = String::new();
    for part in folders.iter().map(String::as_str).chain(file) {
        path.push('\\');
        path.push_str(part);
    }
    if path.is_empty() { String::from("\\") } else { path }
}

/// Opens the file browser on the list of disks (`pick`: to choose a
/// picture).
fn open_browser(ui: &AppWindow, st: &mut State, backend: &dyn Backend, pick: Option<PictureFor>) {
    st.browse = Browse { pick, ..Default::default() };
    st.browse.reload_volumes(backend);
    show_browse(ui, &mut st.browse, backend, None);
    ui.global::<FileBrowser>().set_visible(true);
}

impl Browse {
    /// Reads the disks again.
    fn reload_volumes(&mut self, backend: &dyn Backend) {
        self.volumes = backend.volumes();
        self.unreadable = backend.unreadable_volumes();
    }

    fn path(&self, file: Option<&str>) -> String {
        folder_path(&self.folders, file)
    }

    /// The title of a boot entry for `file`: its name, or the disk's for the
    /// default program of a removable disk (`\EFI\BOOT\BOOTX64.EFI`).
    fn entry_title(&self, file: &str) -> String {
        let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
        match self.volume.and_then(|v| self.volumes.get(v)) {
            Some(v) if stem.eq_ignore_ascii_case(&format!("boot{}", crate::parse::EFI_ARCH)) => v.name.clone(),
            _ => stem.into(),
        }
    }
}

/// A path typed in the file browser, on `volume` from the `folders`: the
/// folders to show and the file to select, spelled as on the disk (file
/// systems ignore case, the names shown don't); or the part not found.
fn resolve_path(backend: &dyn Backend, volume: usize, mut folders: Vec<String>, path: &str) -> Result<(Vec<String>, Option<String>), String> {
    let parts: Vec<&str> = path.split(['\\', '/']).filter(|p| !p.is_empty() && *p != ".").collect();
    for (i, part) in parts.iter().enumerate() {
        if *part == ".." {
            folders.pop();
            continue;
        }
        let entries = backend.list_dir(volume, &folder_path(&folders, None)).map_err(|_| String::from(*part))?;
        match entries.iter().find(|e| e.name.eq_ignore_ascii_case(part)) {
            Some(e) if e.is_dir => folders.push(e.name.clone()),
            Some(e) if i == parts.len() - 1 => return Ok((folders, Some(e.name.clone()))),
            _ => return Err(String::from(*part)),
        }
    }
    Ok((folders, None))
}

/// Type-ahead in the file browser: the row to go to for what was typed,
/// from the `current` one on (the same letter again goes on to the next
/// row with it, as in other file browsers).
fn find_row(names: &[String], typed: &str, current: usize) -> Option<usize> {
    let typed = typed.to_lowercase();
    let first = typed.chars().next()?;
    let same = typed.chars().all(|c| c == first);
    let matches = |name: &String, what: &str| name.to_lowercase().starts_with(what);
    let n = names.len();
    let from = |start: usize, what: &str| (0..n).map(|k| (start + k) % n).find(|&i| matches(&names[i], what));
    if same && typed.chars().count() > 1 && !names.get(current).is_some_and(|c| matches(c, &typed)) {
        return from(current + 1, &String::from(first));
    }
    if same && typed.chars().count() == 1 {
        return from(current + 1, &typed);
    }
    from(current, &typed)
}

/// Shows the file browser's current folder (or the disks), with the row
/// named `select` focused.
fn show_browse(ui: &AppWindow, b: &mut Browse, backend: &dyn Backend, select: Option<&str>) {
    let mut rows = Vec::new();
    let mut shown = Vec::new();
    let mut message = String::new();
    let row = |name: &str, detail: String, kind: &str, file: bool| FileRow { name: name.into(), detail: detail.into(), kind: kind.into(), file, dim: false };
    match b.volume {
        None => {
            ui.global::<FileBrowser>().set_location(tr("All disks").into());
            ui.global::<FileBrowser>().set_path("".into());
            for (i, v) in b.volumes.iter().enumerate() {
                rows.push(BrowseRow::Volume(i));
                shown.push(row(&v.name, v.detail.clone(), v.kind, false));
            }
            for (i, v) in b.unreadable.iter().enumerate() {
                rows.push(BrowseRow::Unreadable(i));
                shown.push(FileRow { dim: true, ..row(&v.name, v.detail.clone(), v.kind, false) });
            }
            if b.volumes.is_empty() {
                message = tr("The firmware can't read any disk.");
            }
        }
        Some(v) => {
            let name = b.volumes.get(v).map(|v| v.name.as_str()).unwrap_or_default();
            ui.global::<FileBrowser>().set_location(name.into());
            ui.global::<FileBrowser>().set_path(b.path(None).into());
            rows.push(BrowseRow::Up);
            let up = if b.folders.is_empty() {
                tr("All disks")
            } else {
                let path = b.path(None);
                let parent = path.rsplit_once('\\').map_or("", |(p, _)| p).trim_start_matches('\\');
                if parent.is_empty() { tr("Back to the top folder") } else { trf("Back to {}", &[&parent]) }
            };
            shown.push(row("..", up, "back", false));
            match backend.list_dir(v, &b.path(None)) {
                Ok(entries) => {
                    let entries = browse_entries(entries, b.pick.is_some());
                    if entries.is_empty() {
                        message = if b.pick.is_some() { tr("No folders or pictures here.") } else { tr("No folders or EFI programs here.") };
                    }
                    for e in entries {
                        if e.is_dir {
                            shown.push(row(&e.name, tr("Folder"), "folder", false));
                            rows.push(BrowseRow::Folder(e.name));
                        } else {
                            shown.push(row(&e.name, format_file_size(e.size), if b.pick.is_some() { "picture" } else { "app" }, true));
                            rows.push(BrowseRow::File(e.name));
                        }
                    }
                }
                Err(e) => {
                    log::warn!("file browser: {} on disk {v}: {e}", b.path(None));
                    message = trf("Cannot read this folder: {}", &[&e]);
                }
            }
        }
    }
    let named = |r: &BrowseRow| match r {
        BrowseRow::Volume(i) => b.volumes.get(*i).map(|v| v.name.clone()),
        BrowseRow::Unreadable(i) => b.unreadable.get(*i).map(|v| v.name.clone()),
        BrowseRow::Folder(n) | BrowseRow::File(n) => Some(n.clone()),
        BrowseRow::Up => None,
    };
    let current = select
        .and_then(|s| rows.iter().position(|r| named(r).as_deref() == Some(s)))
        .unwrap_or(if b.volume.is_some() && rows.len() > 1 { 1 } else { 0 });
    let browser = ui.global::<FileBrowser>();
    browser.set_picking(b.pick.is_some());
    browser.set_rows(ModelRc::new(VecModel::from(shown)));
    browser.set_current(current as i32);
    browser.set_message(message.into());
    b.rows = rows;
}

impl State {
    /// Lists the PNG files next to Slingshot again, for the picture choices,
    /// keeping `also` (a file slingshot.toml names) even if it's missing.
    fn refresh_pictures(&mut self, ui: &AppWindow, backend: &dyn Backend, also: Option<&str>) {
        let mut pictures = backend.list_pictures();
        let named = |f: &str| (!f.is_empty()).then(|| String::from(f));
        let (splash, wallpaper) = (named(&self.config.splash_image), named(&self.config.wallpaper));
        for file in [splash.as_deref(), wallpaper.as_deref(), also].into_iter().flatten() {
            if !pictures.iter().any(|p| p.eq_ignore_ascii_case(file)) {
                pictures.push(file.into());
            }
        }
        let names: Vec<SharedString> = core::iter::once(tr("None")).chain(pictures.iter().cloned()).map(SharedString::from).collect();
        let page = ui.global::<SettingsPage>();
        page.set_picture_names(ModelRc::new(VecModel::from(names)));
        page.set_splash_picture(picture_index(&pictures, splash.as_deref()) as i32);
        page.set_wallpaper_picture(picture_index(&pictures, wallpaper.as_deref()) as i32);
        self.pictures = pictures;
    }

    /// The full BootOrder with the visible entries in the edited order.
    fn edited_order(&self, full: &[u16]) -> Vec<u16> {
        let numbers: Vec<u16> = self.edited.iter().map(|i| self.options[*i].number).collect();
        merge_order(full, &numbers)
    }
}

/// Shows `settings` in the theme and the Settings screen.
fn show_settings(ui: &AppWindow, settings: &Settings) {
    let theme = ui.global::<Theme>();
    theme.set_theme_index(settings.theme.into());
    theme.set_style(settings.style.into());
    theme.set_accent_index(settings.accent.into());
    let page = ui.global::<SettingsPage>();
    page.set_startup_index(if settings.splash { 1 } else { 0 });
    page.set_default_index(settings.remember_last.into());
    page.set_timeout_seconds(settings.timeout.into());
    page.set_pointer_speed(settings.pointer_speed.into());
    page.set_pointer_acceleration(settings.pointer_acceleration);
}

/// Rebuilds the boot picker's cards from the entries in `st`, and shows the
/// main row.
fn show_cards(ui: &AppWindow, st: &mut State) {
    let (mut cards, default_card) = build_cards(
        &st.options,
        st.loader_entries.as_ref(),
        &st.found,
        st.firmware_setup_supported && st.config.firmware_setup,
    );
    let one_shot = st.loader_entries.as_ref().and_then(|l| l.one_shot.as_deref());
    st.default_card = apply_rules(&mut cards, default_card, &st.config, one_shot);
    // Else the one started last, when asked (Settings, or systemd-boot's
    // `@saved`).
    let remember = st.settings.remember_last || st.loader_entries.as_ref().is_some_and(|l| l.saved);
    let last = st
        .last_started
        .clone()
        .or_else(|| st.loader_entries.as_ref().and_then(|l| l.last_booted.as_ref()).map(|id| format!("entry:{id}")));
    let one_shot_shown = one_shot.is_some_and(|id| cards.iter().any(|c| c.id() == Some(id)));
    if !one_shot_shown
        && let Some(i) = last.filter(|_| remember).and_then(|key| cards.iter().position(|c| c.key().as_deref() == Some(&key)))
    {
        st.default_card = i;
        cards[i].more = false;
    }
    // Starting any file is like the EFI shell: not when Settings are hidden
    // (a locked-down machine); behind the password when they have one.
    if !matches!(st.config.settings_access, SettingsAccess::Hidden) && st.can_browse {
        cards.push(browse_card());
    }
    st.all_cards = cards;
    show_view(ui, st, Level::Main);
}

/// Reads again what the disks can start (a USB stick came or went) and
/// shows the cards again, on the same level and the same card if it's
/// still there.
fn refresh_found(ui: &AppWindow, st: &mut State, backend: &dyn Backend) {
    let fresh = backend.rescan();
    for option in &mut st.options {
        if let Some(f) = fresh.options.iter().find(|f| f.number == option.number) {
            option.absent = f.absent;
        }
    }
    st.found = fresh.found;
    show_cards_again(ui, st);
}

/// On the splash screen (it says "starting this one"), a default entry that
/// needs a passphrase asks for it right away (Esc or Cancel: the boot menu),
/// not after the wait; once. On the boot menu, where one chooses, the
/// countdown runs as usual. Whether it asked.
fn passphrase_first(ui: &AppWindow, st: &mut State, backend: &dyn Backend) -> bool {
    let bootable = st.cards.iter().any(|c| matches!(c.action, Action::Start(_)));
    let encrypted = match st.all_cards.get(st.default_card).map(|c| &c.action) {
        Some(Action::Start(target)) => !backend.locked_volumes(target).is_empty(),
        _ => false,
    };
    if st.settings.timeout == 0 || !st.settings.splash || !bootable || !encrypted || st.passphrase_asked {
        return false;
    }
    st.passphrase_asked = true;
    st.countdown_left = -1;
    ui.global::<BootMenu>().set_countdown(-1);
    ui.set_splash_visible(false);
    let ui_weak = ui.as_weak();
    Timer::single_shot(Duration::ZERO, move || {
        if let Some(ui) = ui_weak.upgrade() {
            ui.global::<BootMenu>().invoke_boot(ui.global::<BootMenu>().get_default_entry());
        }
    });
    true
}

/// After the first frame, the devices connected (`Hub::devices_connected`):
/// what the disks can start, and, when drivers were started (a file system
/// they read can hold systemd-boot's entries), the boot data again. The
/// firmware's entries don't change.
fn devices_connected(ui: &AppWindow, st: &mut State, backend: &dyn Backend, tick: &Timer, drivers: bool) {
    st.devices_connected = true;
    if drivers {
        let boot = backend.boot_data();
        ui.set_options_editor(options_editor(&st.config, &boot));
        st.loader_entries = boot.loader_entries;
        st.found = boot.found;
        show_cards_again(ui, st);
    } else {
        refresh_found(ui, st, backend);
    }
    // The encrypted disk may only be known now (QEMU connects its NVMe
    // disks then): still on the splash, untouched, its passphrase first,
    // else the countdown.
    if core::mem::take(&mut st.waiting_for_disks) && !passphrase_first(ui, st, backend) {
        count_down(ui, st, tick, st.settings.timeout as i32);
    }
}

/// Starts the countdown to the default entry, `seconds` long.
fn count_down(ui: &AppWindow, st: &mut State, tick: &Timer, seconds: i32) {
    st.countdown_left = seconds;
    // Count whole seconds from now, in step with the animation.
    tick.restart();
    let menu = ui.global::<BootMenu>();
    menu.set_countdown(-1);
    menu.set_countdown_total(seconds);
    menu.set_countdown(seconds);
}

/// Whether `card` starts with an encrypted disk (`rd.luks.*`), found yet or
/// not.
fn wants_encrypted_disk(card: &Card) -> bool {
    cfg_select! {
        feature = "luks" => card.options().is_some_and(|o| !crate::parse::handoff::wanted_volumes(o).is_empty()),
        _ => {
            let _ = card;
            false
        }
    }
}

/// Whether the kernel options can be edited before starting (E): as
/// slingshot.toml says, else unless Secure Boot is on (a signed kernel
/// with `init=/bin/sh` is a root shell) or loader.conf says `editor no`.
fn options_editor(config: &Config, boot: &BootData) -> bool {
    config.editor.unwrap_or(!boot.secure_boot && !boot.loader_entries.as_ref().is_some_and(|l| l.editor_off))
}

/// The cards made again, on the same level and the same card if it's still
/// there.
fn show_cards_again(ui: &AppWindow, st: &mut State) {
    let focused = st.cards.get(ui.global::<BootMenu>().get_focused_entry().max(0) as usize).map(|c| (c.title.clone(), c.subtitle.clone()));
    let level = if ui.global::<BootMenu>().get_more_level() { Level::More } else { Level::Main };
    let before: Vec<(String, String)> = st.all_cards.iter().map(|c| (c.title.clone(), c.subtitle.clone())).collect();
    show_cards(ui, st);
    // The new cards come in animated, once, and the row slides while cards
    // come or go.
    st.arrived =
        st.all_cards.iter().map(|c| (c.title.clone(), c.subtitle.clone())).filter(|key| !before.contains(key)).collect();
    if !st.arrived.is_empty() || st.all_cards.len() != before.len() {
        ui.global::<Theme>().set_cards_sliding(true);
        let ui_weak = ui.as_weak();
        Timer::single_shot(Duration::from_millis(400), move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.global::<Theme>().set_cards_sliding(false);
            }
        });
    }
    show_view(ui, st, level);
    st.arrived.clear();
    if let Some(i) = focused.and_then(|f| st.cards.iter().position(|c| (c.title.clone(), c.subtitle.clone()) == f)) {
        ui.global::<BootMenu>().set_focused_entry(i as i32);
    }
}

/// Shows the main row, or the "More…" level.
fn show_view(ui: &AppWindow, st: &mut State, level: Level) {
    let more = level != Level::Main;
    let (cards, default) = view(&st.all_cards, st.default_card, level);
    let entries: Vec<BootEntry> = cards
        .iter()
        .map(|c| BootEntry {
            arrived: st.arrived.iter().any(|(title, subtitle)| *title == c.title && *subtitle == c.subtitle),
            ..card_entry(c, &st.images)
        })
        .collect();
    st.cards = cards;
    let menu = ui.global::<BootMenu>();
    menu.set_entries(ModelRc::new(VecModel::from(entries)));
    menu.set_more_level(more);
    menu.set_default_entry(default.map_or(-1, |d| d as i32));
    // On the "More…" level, the first entry after "Back".
    menu.set_focused_entry(default.unwrap_or(if more && st.cards.len() > 1 { 1 } else { 0 }) as i32);
}

/// Fills the System screen.
fn update_system_info(ui: &AppWindow, backend: &dyn Backend, hub: &Hub) {
    let info = backend.system_info(hub.current_size.get(), &hub.modes.borrow());
    let items: Vec<InfoItem> = info
        .cards
        .iter()
        .map(|c| InfoItem {
            label: c.label.as_str().into(),
            title: c.title.as_str().into(),
            detail: c.detail.as_str().into(),
            icon: c.icon.into(),
            good: c.good,
        })
        .collect();
    ui.global::<System>().set_items(ModelRc::new(VecModel::from(items)));

    if let Some(u) = info.memory {
        // Usable, firmware runtime, ACPI, reserved.
        let segments = [
            (u.usable, Color::from_rgb_u8(0x5b, 0x8c, 0xff)),
            (u.runtime, Color::from_rgb_u8(0x2f, 0xb6, 0xa3)),
            (u.acpi, Color::from_rgb_u8(0xe5, 0x8a, 0x3a)),
            (u.reserved, Color::from_rgb_u8(0x6b, 0x73, 0x90)),
        ];
        let model: Vec<MemorySegment> = segments
            .iter()
            .filter(|s| s.0 >= 1 << 20)
            .map(|&(bytes, color)| MemorySegment { weight: bytes as f32, color })
            .collect();
        ui.global::<System>().set_memory(ModelRc::new(VecModel::from(model)));
    }
    ui.set_firmware_line(info.firmware_line.into());
}

/// Fills the System screen's battery card.
fn update_battery(ui: &AppWindow, backend: &dyn Backend) {
    let info = match power::view(&backend.power()) {
        None => BatteryInfo::default(),
        Some(v) => BatteryInfo {
            present: true,
            level: v.level.unwrap_or(-1.),
            direction: v.direction,
            state: v.state.into(),
            time_left: v.time_left.into(),
            ac: v.ac.into(),
            low: v.low,
            facts: ModelRc::new(VecModel::from(
                v.facts.into_iter().map(|(label, value)| BatteryFact { label: label.into(), value: value.into() }).collect::<Vec<_>>(),
            )),
            identity: v.identity.into(),
        },
    };
    ui.global::<System>().set_battery(info);
}

/// What each install step is called, in `Backend::install_step` order.
fn install_steps() -> [String; 6] {
    [
        tr("Install Slingshot"),
        tr("Start Slingshot once, on the next restart"),
        tr("Start Slingshot on every boot"),
        tr("Stop starting Slingshot on every boot"),
        tr("Add Slingshot to systemd-boot's menu"),
        tr("Uninstall Slingshot"),
    ]
}

/// The Install screen: each step shows its commands (a dry run), then runs
/// once confirmed.
fn install_screen(ui: &AppWindow, backend: &Rc<dyn Backend>, show_status: impl Fn(&str) + 'static) {
    ui.global::<Install>().set_available(true);
    let refresh = {
        let (ui_weak, backend) = (ui.as_weak(), backend.clone());
        Rc::new(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            ui.global::<Install>().set_busy(true);
            let ui_weak = ui.as_weak();
            backend.install_status(Box::new(move |s| {
                let Some(ui) = ui_weak.upgrade() else { return };
                let install = ui.global::<Install>();
                install.set_info(InstallInfo {
                    known: true,
                    error: s.error.into(),
                    esp: s.esp.into(),
                    installed: s.installed,
                    file: s.file.into(),
                    entry: s.entry.into(),
                    order: s.order.into(),
                    systemd_boot: s.systemd_boot,
                    systemd_boot_entry: s.systemd_boot_entry,
                    secure_boot: s.secure_boot,
                    program: s.program.into(),
                });
                install.set_busy(false);
            }));
        })
    };
    refresh();
    let pending = Rc::new(core::cell::Cell::new(0usize));
    {
        let (ui_weak, backend, pending) = (ui.as_weak(), backend.clone(), pending.clone());
        ui.global::<Install>().on_act(move |action| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let action = action.clamp(0, 5) as usize;
            pending.set(action);
            ui.global::<Install>().set_busy(true);
            let ui_weak = ui.as_weak();
            backend.install_step(action, true, Box::new(move |result| {
                let Some(ui) = ui_weak.upgrade() else { return };
                let install = ui.global::<Install>();
                install.set_busy(false);
                match result {
                    Ok(plan) => {
                        install.set_confirm_title(install_steps()[action].as_str().into());
                        install.set_confirm_text(plan.into());
                        install.set_confirm_destructive(action == 5);
                        install.set_confirm_visible(true);
                    }
                    Err(e) => {
                        install.set_result_ok(false);
                        install.set_result_text(e.into());
                        install.set_offer_restart(false);
                        install.set_result_visible(true);
                    }
                }
            }));
        });
    }
    {
        let (ui_weak, backend, refresh) = (ui.as_weak(), backend.clone(), refresh.clone());
        ui.global::<Install>().on_confirm(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let action = pending.get();
            ui.global::<Install>().set_busy(true);
            let (ui_weak, refresh) = (ui.as_weak(), refresh.clone());
            backend.install_step(action, false, Box::new(move |result| {
                let Some(ui) = ui_weak.upgrade() else { return };
                let install = ui.global::<Install>();
                install.set_busy(false);
                install.set_offer_restart(action == 1 && result.is_ok());
                install.set_result_ok(result.is_ok());
                install.set_result_text(match result {
                    Ok(text) | Err(text) => text.into(),
                });
                install.set_result_visible(true);
                refresh();
            }));
        });
    }
    {
        let (backend, refresh) = (backend.clone(), refresh.clone());
        ui.global::<Install>().on_choose_program(move || {
            if backend.choose_install_program() {
                refresh();
            }
        });
    }
    {
        let backend = backend.clone();
        ui.global::<Install>().on_restart(move || {
            if let Err(e) = backend.restart() {
                show_status(&trf("Could not restart: {}", &[&e]));
            }
        });
    }
}

/// The top bar's clock, and the System screen's date with the time under
/// it: one reading of the clock.
fn show_clock(ui: &AppWindow, backend: &dyn Backend) {
    let t = backend.date_time();
    let zone = backend.time_zone();
    let short = t.map(|t| if zone.is_empty() { t.short_text() } else { format!("{} {zone}", t.short_text()) });
    ui.set_clock(short.unwrap_or_default().into());
    ui.global::<System>().set_date_time_text(t.map(|t| t.long_text().replacen(" · ", "\n", 1)).unwrap_or_default().into());
}

/// Creates the UI and runs the event loop.
pub fn run(backend: Rc<dyn Backend>, hub: Rc<Hub>) -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    // The language first: the texts made from here on use it.
    let (config, saved) = {
        let config = read_config(&*backend);
        let saved = match config.settings_access {
            SettingsAccess::Hidden => None,
            _ => backend.load_settings(),
        };
        use_language(&*backend, &config, saved.map_or(0, |s| s.settings.language));
        (config, saved)
    };
    // The arrows in lists and rows move the keyboard focus as Tab and
    // Shift+Tab do: a Tab key, after the key being handled.
    {
        let ui_weak = ui.as_weak();
        ui.global::<Focus>().on_move(move |forward| {
            let ui_weak = ui_weak.clone();
            Timer::single_shot(Duration::ZERO, move || {
                let Some(ui) = ui_weak.upgrade() else { return };
                let key: SharedString = if forward { slint::platform::Key::Tab.into() } else { slint::platform::Key::Backtab.into() };
                ui.window().dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.clone() });
                ui.window().dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key });
            });
        });
    }
    let caps = backend.capabilities();
    ui.set_simulated(caps.simulation);
    ui.set_badge(backend.badge().into());
    ui.global::<SettingsPage>().set_in_file(caps.configure);
    ui.global::<System>().set_app_version(concat!("v", env!("CARGO_PKG_VERSION")).into());

    // --- Boot entries -------------------------------------------------------
    let boot = backend.boot_data();
    let entries: Vec<BootEntry> = boot.options.iter().map(option_entry).collect();
    let order_model = Rc::new(VecModel::from(entries));
    ui.global::<SettingsPage>().set_order(order_model.clone().into());

    // --- Settings -----------------------------------------------------------
    let firmware_timeout = match boot.firmware_timeout {
        Some(0xffff) => 0,
        // The firmware's own timeout, when it's 1 to 60 seconds.
        Some(t) if (1..=60).contains(&t) => t,
        _ => 10,
    };
    // Built-in defaults, then slingshot.toml, then what was saved from the
    // Settings screen (unless the screen is hidden).
    let config = Rc::new(config);
    let builtin = Settings { timeout: firmware_timeout, ..Settings::default() };
    let mut defaults = builtin;
    config.defaults.apply(&mut defaults);
    let settings = initial_settings(caps.configure, &config, defaults, saved);
    show_settings(&ui, &settings);
    use_pointer(&hub, &settings);
    show_language_choices(&ui, &*backend, &config, &settings);
    // F10 too, not only the card.
    ui.set_firmware_setup_supported(boot.firmware_setup_supported && config.firmware_setup);
    ui.set_options_editor(options_editor(&config, &boot));
    ui.set_settings_access(match config.settings_access {
        SettingsAccess::Shown => 0,
        SettingsAccess::Hidden => 1,
        SettingsAccess::Password(_) => 2,
    });

    {
        let config = config.clone();
        ui.on_check_password(move |password| config.check_password(&password));
    }

    // Splash screen text, hint and image, from slingshot.toml.
    let page = ui.global::<SettingsPage>();
    page.set_splash_text(config.splash_text.as_str().into());
    page.set_splash_name(config.splash_name);
    page.set_splash_picture_shown(config.splash_picture);
    page.set_splash_hint(config.splash_hint);
    if !config.splash_image.is_empty() {
        match backend.read_file(&config.splash_image).as_deref().and_then(decode_picture) {
            Some(image) => ui.global::<SettingsPage>().set_splash_image(image),
            None => log::warn!("cannot read the splash image {}", config.splash_image),
        }
    }
    if !config.wallpaper.is_empty() {
        match backend.read_file(&config.wallpaper).as_deref().and_then(decode_picture) {
            Some(image) => show_wallpaper(&ui, image),
            None => log::warn!("cannot read the wallpaper {}", config.wallpaper),
        }
    }

    let state = Rc::new(RefCell::new(State {
        original_order: boot.order.clone(),
        firmware_order: boot.order,
        edited: (0..boot.options.len()).collect(),
        options: boot.options,
        loader_entries: boot.loader_entries,
        found: boot.found,
        firmware_setup_supported: boot.firmware_setup_supported,
        all_cards: Vec::new(),
        default_card: 0,
        cards: Vec::new(),
        settings,
        settings_dirty: false,
        full_redraw: None,
        slow_rendering: false,
        mode_candidate: 0,
        revert_to: None,
        revert_left: -1,
        countdown_left: -1,
        passphrase_asked: false,
        devices_connected: !cfg!(target_os = "uefi"),
        waiting_for_disks: false,
        config: config.clone(),
        defaults,
        builtin,
        can_browse: !caps.configure,
        arrived: Vec::new(),
        images: entry_images(&ui, &*backend, &config),
        pictures: Vec::new(),
        browse: Browse::default(),
        last_started: backend.last_started(),
        hibernated: backend.hibernated(),
    }));
    state.borrow_mut().refresh_pictures(&ui, &*backend, None);
    show_cards(&ui, &mut state.borrow_mut());

    // --- Countdown and clock -------------------------------------------------
    ui.global::<System>().set_clock_settable(backend.date_time().is_some() && !caps.configure);
    show_clock(&ui, &*backend);

    // Ticks every second: the clock, the countdown, the resolution revert,
    // and the battery every 5 seconds on the System screen.
    let tick = Rc::new(Timer::default());
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let backend = backend.clone();
        let seconds = core::cell::Cell::new(0u32);
        tick.start(TimerMode::Repeated, Duration::from_secs(1), move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            show_clock(&ui, &*backend);
            seconds.set(seconds.get().wrapping_add(1));
            if ui.get_screen() == 1 && seconds.get().is_multiple_of(5) {
                update_battery(&ui, &*backend);
            }
            // A USB stick plugged in or out: its card comes or goes (not
            // under a dialog that works on a card).
            let dialog = ui.global::<Customize>().get_visible() || ui.global::<BootMenu>().get_options_visible() || ui.global::<Passphrase>().get_visible() || ui.get_password_visible();
            if !dialog && backend.media_changed() {
                let mut st = state.borrow_mut();
                refresh_found(&ui, &mut st, &*backend);
                // The file browser's list of disks too (inside a disk, the
                // list is that disk's).
                if ui.global::<FileBrowser>().get_visible() && st.browse.volume.is_none() {
                    let selected = st.browse.rows.get(ui.global::<FileBrowser>().get_current().max(0) as usize).and_then(|r| match r {
                        BrowseRow::Volume(v) => st.browse.volumes.get(*v).map(|v| v.name.clone()),
                        _ => None,
                    });
                    st.browse.reload_volumes(&*backend);
                    show_browse(&ui, &mut st.browse, &*backend, selected.as_deref());
                }
            }
            let mut st = state.borrow_mut();
            if st.countdown_left > 0 {
                st.countdown_left -= 1;
                ui.global::<BootMenu>().set_countdown(st.countdown_left);
                if st.countdown_left == 0 {
                    st.countdown_left = -1;
                    drop(st);
                    // Not "starts in 0 s" any more if it doesn't start (a
                    // passphrase asked first, then cancelled; an error).
                    ui.global::<BootMenu>().set_countdown(-1);
                    let index = ui.global::<BootMenu>().get_default_entry();
                    ui.global::<BootMenu>().invoke_boot(index);
                    return;
                }
            }
            if st.revert_left > 0 {
                st.revert_left -= 1;
                ui.global::<SettingsPage>().set_revert_countdown(st.revert_left);
                if st.revert_left == 0 {
                    drop(st);
                    ui.global::<SettingsPage>().invoke_revert_mode();
                }
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        *hub.on_pointer_status.borrow_mut() = Some(Box::new(move |status| {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_pointer_status(status.into());
            }
        }));
        ui.set_pointer_status(hub.pointer_status.borrow().as_str().into());
    }

    // Any key or pointer movement stops the countdown. The UI reports key
    // presses and hovering; the UEFI platform also reports raw pointer
    // movement through the hub.
    let stop_countdown = {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        move || {
            let mut st = state.borrow_mut();
            st.waiting_for_disks = false;
            if st.countdown_left >= 0 {
                st.countdown_left = -1;
                if let Some(ui) = ui_weak.upgrade() {
                    ui.global::<BootMenu>().set_countdown(-1);
                }
            }
        }
    };
    {
        // Raw input from the UEFI platform, including pointer movement: on
        // the splash screen, only a key press or a click (reported by the UI)
        // counts, so a jittery pointer can't dismiss it.
        let ui_weak = ui.as_weak();
        let stop_countdown = stop_countdown.clone();
        *hub.on_input.borrow_mut() = Some(Box::new(move || {
            if ui_weak.upgrade().is_some_and(|ui| !ui.get_splash_visible()) {
                stop_countdown();
            }
        }));
    }
    ui.on_user_activity(stop_countdown.clone());

    // Transient messages in the footer.
    let status_timer = Rc::new(Timer::default());
    let show_status = {
        let ui_weak = ui.as_weak();
        let status_timer = status_timer.clone();
        move |message: &str| {
            let Some(ui) = ui_weak.upgrade() else { return };
            ui.set_status_message(message.into());
            let ui_weak = ui.as_weak();
            status_timer.start(TimerMode::SingleShot, Duration::from_secs(5), move || {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_status_message(SharedString::new());
                }
            });
        }
    };

    // The clock dialog edits a copy of the firmware's time until "Set".
    {
        let edit: Rc<core::cell::Cell<Option<DateTime>>> = Rc::default();
        let show_fields = |ui: &AppWindow, t: &DateTime| {
            ui.global::<System>().set_time_fields(ModelRc::new(VecModel::from(t.fields().to_vec())));
        };
        {
            let (ui_weak, backend, edit) = (ui.as_weak(), backend.clone(), edit.clone());
            ui.global::<System>().on_open_time_dialog(move || {
                let Some(ui) = ui_weak.upgrade() else { return };
                let Some(now) = backend.date_time() else { return };
                edit.set(Some(now));
                show_fields(&ui, &now);
                ui.global::<System>().set_time_dialog_visible(true);
            });
        }
        {
            let (ui_weak, edit) = (ui.as_weak(), edit.clone());
            ui.global::<System>().on_time_step(move |field, delta| {
                let Some(ui) = ui_weak.upgrade() else { return };
                let Some(t) = edit.get() else { return };
                let t = t.step(field as usize, delta);
                edit.set(Some(t));
                show_fields(&ui, &t);
            });
        }
        {
            let (ui_weak, edit) = (ui.as_weak(), edit.clone());
            ui.global::<System>().on_time_set(move |field, value| {
                let Some(ui) = ui_weak.upgrade() else { return };
                let Some(t) = edit.get() else { return };
                let t = t.set(field as usize, value);
                edit.set(Some(t));
                show_fields(&ui, &t);
            });
        }
        {
            let (ui_weak, backend, show_status) = (ui.as_weak(), backend.clone(), show_status.clone());
            ui.global::<System>().on_apply_time(move || {
                let Some(ui) = ui_weak.upgrade() else { return };
                let Some(t) = edit.take() else { return };
                ui.global::<System>().set_time_dialog_visible(false);
                match backend.set_date_time(t) {
                    Ok(()) => {
                        show_clock(&ui, &*backend);
                        show_status(&trf("Clock set to {}", &[&t.long_text()]));
                    }
                    Err(e) => show_status(&trf("Could not set the clock: {}", &[&e])),
                }
            });
        }
    }

    // What the machine shows when it starts: the boot screen with the default
    // entry focused, the countdown, and the splash screen if enabled. Also
    // used by the simulator after a simulated restart.
    let show_startup = {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let tick = tick.clone();
        let backend = backend.clone();
        move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            let settings = st.settings;
            ui.set_screen(0);
            // The cards again: the default may be the one started last.
            show_cards(&ui, &mut st);
            // A system hibernated: that one, whatever the default (else its
            // session is lost). Started after a short countdown when it's
            // known for sure, else chosen by the person.
            let resume = st
                .hibernated
                .as_ref()
                .and_then(|h| cards::hibernated_card(&st.all_cards, h, st.last_started.as_deref()));
            if let Some((i, _)) = resume {
                st.all_cards[i].more = false;
                st.default_card = i;
            }
            show_view(&ui, &mut st, Level::Main);
            let menu = ui.global::<BootMenu>();
            menu.set_focused_entry(menu.get_default_entry());
            menu.set_hibernated(st.hibernated.is_some());
            menu.set_resume_known(resume.is_some_and(|(_, sure)| sure));
            if let Some((_, sure)) = resume {
                log::info!("a system hibernated: {}", if sure { "resuming it" } else { "not sure which" });
                ui.set_splash_visible(false);
                // Unless the timeout is "never": then it's chosen too.
                count_down(&ui, &mut st, &tick, if sure && settings.timeout != 0 { 3 } else { -1 });
                return;
            }
            // Never count down into the firmware setup.
            let bootable = st.cards.iter().any(|c| matches!(c.action, Action::Start(_)));
            if passphrase_first(&ui, &mut st, backend.as_ref()) {
                return;
            }
            if settings.timeout > 0 && bootable {
                ui.set_splash_visible(settings.splash);
                let default = st.all_cards.get(st.default_card);
                if settings.splash && !st.devices_connected && default.is_some_and(wants_encrypted_disk) {
                    // Not a countdown its passphrase would cut short: no
                    // countdown until the disks are there.
                    st.waiting_for_disks = true;
                    st.countdown_left = -1;
                    ui.global::<BootMenu>().set_countdown(-1);
                } else {
                    count_down(&ui, &mut st, &tick, settings.timeout as i32);
                }
            } else {
                st.countdown_left = -1;
                ui.global::<BootMenu>().set_countdown(-1);
                ui.set_splash_visible(false);
            }
        }
    };

    // --- Booting --------------------------------------------------------------
    let boot_timer = Rc::new(Timer::default());
    // Shows the transition, then runs `action` (which normally resets the
    // machine). On return, shows the message.
    let run_with_transition = {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let boot_timer = boot_timer.clone();
        let show_status = show_status.clone();
        let show_startup = show_startup.clone();
        // `overlay` is what shows meanwhile ("Starting Arch Linux…").
        move |title: String, overlay: String, action: Box<dyn Fn() -> StartResult>| {
            let Some(ui) = ui_weak.upgrade() else { return };
            state.borrow_mut().countdown_left = -1;
            ui.global::<BootMenu>().set_countdown(-1);
            ui.set_starting_title(overlay.as_str().into());
            let ui_weak = ui.as_weak();
            let show_status = show_status.clone();
            let show_startup = show_startup.clone();
            let restart_timer = boot_timer.clone();
            // Just long enough to draw the "Starting…" screen.
            boot_timer.start(TimerMode::SingleShot, Duration::from_millis(30), move || {
                match action() {
                    StartResult::Simulated(message) => {
                        let ui_weak = ui_weak.clone();
                        let show_status = show_status.clone();
                        let show_startup = show_startup.clone();
                        restart_timer.start(TimerMode::SingleShot, Duration::from_millis(1200), move || {
                            if let Some(ui) = ui_weak.upgrade() {
                                ui.set_starting_title(SharedString::new());
                            }
                            show_startup();
                            show_status(&message);
                        });
                    }
                    StartResult::Returned(how) => {
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.set_starting_title(SharedString::new());
                        }
                        show_status(&format!("{title} {how}"));
                    }
                    StartResult::Failed(e) => {
                        // A dialog: the reason matters (Secure Boot…) and
                        // the status line is small and short.
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.set_starting_title(SharedString::new());
                            ui.set_error_title(trf("Couldn't start {}", &[&title]).into());
                            ui.set_error_text(e.into());
                            ui.set_error_visible(true);
                        }
                    }
                }
            });
        }
    };
    let start_with_transition = {
        let run = run_with_transition.clone();
        move |title: String, action: Box<dyn Fn() -> StartResult>| {
            let overlay = trf("Starting {}…", &[&title]);
            run(title, overlay, action)
        }
    };
    // F12: the log, saved next to Slingshot for a bug report.
    ui.set_can_save_log(!caps.simulation);
    {
        let (backend, show_status) = (backend.clone(), show_status.clone());
        ui.on_save_log(move || match backend.save_log() {
            Ok(path) => show_status(&trf("Log saved as {}", &[&path])),
            Err(e) => show_status(&trf("Couldn't save the log: {}", &[&e])),
        });
    }
    // The Boot screen's Restart and Shut down buttons.
    ui.global::<BootMenu>().set_power_buttons(!caps.configure);
    {
        let (run, backend) = (run_with_transition.clone(), backend.clone());
        ui.global::<BootMenu>().on_power_off(move |shut_down| {
            let backend = backend.clone();
            let (title, overlay) = if shut_down { (tr("shut down"), tr("Shutting down…")) } else { (tr("restart"), tr("Restarting…")) };
            run(title, overlay, Box::new(move || backend.power_off(shut_down)));
        });
    }
    // Entries that need encrypted disks ask for their passphrase first.
    let (start_target, unlock_disk) = {
        let start = start_with_transition.clone();
        let direct_backend = backend.clone();
        let direct: unlock::Start = Rc::new(move |title, target| {
            let backend = direct_backend.clone();
            start(title, Box::new(move || backend.start(&target)));
        });
        let stop_countdown = Rc::new(stop_countdown.clone());
        use_keymap(&*backend, &hub, &config, settings.keyboard);
        unlock::wire(&ui, backend.clone(), direct, stop_countdown)
    };
    // Shared by the firmware setup card and F10.
    let firmware_setup = {
        let backend = backend.clone();
        let start = start_with_transition.clone();
        Rc::new(move || {
            let backend = backend.clone();
            start(String::from("firmware setup"), Box::new(move || backend.reboot_to_firmware_setup()));
        })
    };
    // The card started last: the default next time, if asked (and how the
    // system that hibernates is found).
    let remember_started = {
        let (state, backend) = (state.clone(), backend.clone());
        Rc::new(move |card: &Card| {
            if let Some(key) = card.key() {
                backend.set_last_started(&key, card.id());
                let mut st = state.borrow_mut();
                st.last_started = Some(key);
                // Started: resumed, or that session is gone.
                st.hibernated = None;
            }
        })
    };
    {
        let state = state.clone();
        let start = start_target.clone();
        let firmware_setup = firmware_setup.clone();
        let ui_weak = ui.as_weak();
        let remember_started = remember_started.clone();
        ui.global::<BootMenu>().on_boot(move |index| {
            let Some(card) = state.borrow().cards.get(index as usize).cloned() else { return };
            if matches!(card.action, Action::Start(_)) {
                remember_started(&card);
                if let Some(ui) = ui_weak.upgrade() {
                    show_unlock_entry(&ui, &card, &state.borrow());
                }
            }
            match card.action {
                Action::Start(target) => start(card.title, target),
                Action::FirmwareSetup => firmware_setup(),
                Action::BrowseFiles => {
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.invoke_ask_browse();
                    }
                }
                Action::ShowMore | Action::ShowMain => {
                    let Some(ui) = ui_weak.upgrade() else { return };
                    let level = if matches!(card.action, Action::ShowMore) { Level::More } else { Level::Main };
                    show_view(&ui, &mut state.borrow_mut(), level);
                }
            }
        });
    }
    // C: customize an entry, saved in slingshot.toml.
    {
        let (state, ui_weak, backend) = (state.clone(), ui.as_weak(), backend.clone());
        ui.global::<Customize>().on_open(move |index| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            let Some(card) = st.cards.get(index as usize).cloned() else { return };
            if !matches!(card.action, Action::Start(_) | Action::FirmwareSetup) {
                return;
            }
            let config = st.config.clone();
            let rule = match card.custom {
                Some(i) => config.entries.get(i),
                None => config.rule_for(&card.match_title, card.id()),
            };
            let image = rule.and_then(|r| r.image.as_deref());
            st.refresh_pictures(&ui, &*backend, image);
            let picture = picture_index(&st.pictures, image);
            let customize = ui.global::<Customize>();
            customize.set_icon(rule.and_then(|r| r.icon).and_then(|i| ICONS.iter().position(|x| *x == i)).map_or(0, |p| p as i32 + 1));
            customize.set_picture(picture as i32);
            show_picture_preview(&ui, &mut st, &*backend, picture);
            customize.set_entry(index);
            customize.set_title(rule.and_then(|r| r.title.clone()).filter(|_| card.custom.is_none()).unwrap_or_default().into());
            customize.set_more(card.more);
            customize.set_is_default(ui.global::<BootMenu>().get_default_entry() == index);
            customize.set_options(rule.and_then(|r| r.options.clone()).unwrap_or_default().into());
            customize.set_options_placeholder(card.kernel_options().unwrap_or_default().into());
            customize.set_visible(true);
        });
    }
    {
        let (state, ui_weak, backend) = (state.clone(), ui.as_weak(), backend.clone());
        ui.global::<Customize>().on_picture_changed(move |picture| {
            let Some(ui) = ui_weak.upgrade() else { return };
            show_picture_preview(&ui, &mut state.borrow_mut(), &*backend, picture as usize);
        });
    }
    {
        let (state, ui_weak, backend, show_status) = (state.clone(), ui.as_weak(), backend.clone(), show_status.clone());
        ui.global::<Customize>().on_save(move |index| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            let Some(card) = st.cards.get(index as usize).cloned() else { return };
            let customize = ui.global::<Customize>();
            let more = customize.get_more();
            let change = config::EntryChange {
                title: customize.get_title().trim().into(),
                more: (more != card.more_by_default() || card.custom.is_some()).then_some(more),
                default: customize.get_is_default(),
                options: card.options().filter(|_| ui.get_can_edit_options()).map(|_| customize.get_options().trim().into()),
                icon: (customize.get_icon() as usize).checked_sub(1).and_then(|i| ICONS.get(i)).copied(),
                image: (customize.get_picture() as usize).checked_sub(1).and_then(|i| st.pictures.get(i)).cloned(),
            };
            let new = config::change_entry(&st.config, &card.match_title, card.id(), card.custom, &change);
            let saved = commit_config(&mut st, &*backend, new, false);
            if saved.is_ok() {
                show_cards(&ui, &mut st);
            }
            drop(st);
            show_status(&saved.map_or_else(|e| e, |()| tr("Saved in slingshot.toml")));
        });
    }
    // E: start a Linux entry once with other kernel options.
    {
        let (state, ui_weak) = (state.clone(), ui.as_weak());
        ui.global::<BootMenu>().on_open_options(move |index| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let st = state.borrow();
            let Some(options) = st.cards.get(index as usize).and_then(Card::options) else { return };
            let menu = ui.global::<BootMenu>();
            menu.set_options_text(options.into());
            menu.set_options_entry(index);
            menu.set_options_visible(true);
        });
    }
    {
        let state = state.clone();
        let start = start_target.clone();
        let remember_started = remember_started.clone();
        let ui_weak = ui.as_weak();
        ui.global::<BootMenu>().on_boot_with_options(move |index, text| {
            let Some(mut card) = state.borrow().cards.get(index as usize).cloned() else { return };
            if let Action::Start(Target::LoaderEntry { options, .. } | Target::Program { options, .. } | Target::Found { options, .. }) = &mut card.action {
                *options = text.trim().into();
            }
            if let Action::Start(target) = card.action.clone() {
                remember_started(&card);
                if let Some(ui) = ui_weak.upgrade() {
                    show_unlock_entry(&ui, &card, &state.borrow());
                }
                start(card.title, target);
            }
        });
    }
    ui.on_firmware_setup(move || firmware_setup());

    // --- Settings -----------------------------------------------------------------
    let save_settings = {
        let state = state.clone();
        let backend = backend.clone();
        let show_status = show_status.clone();
        move || {
            let mut st = state.borrow_mut();
            if st.settings_dirty {
                st.settings_dirty = false;
                let saved = save_settings(caps.configure, &mut st, &*backend);
                drop(st);
                if let Some(message) = saved.unwrap_or_else(Some) {
                    show_status(&message);
                }
            }
        }
    };
    {
        let save_settings = save_settings.clone();
        let (ui_weak, backend) = (ui.as_weak(), backend.clone());
        ui.on_screen_changed(move |screen| {
            if screen != 2 {
                save_settings();
            }
            if screen == 1
                && let Some(ui) = ui_weak.upgrade()
            {
                update_battery(&ui, &*backend);
            }
        });
    }
    // A choice in Settings: `set` updates the settings, saved when leaving.
    let on_setting = |set: fn(&mut Settings, usize)| {
        let state = state.clone();
        move |index: i32| {
            let mut st = state.borrow_mut();
            set(&mut st.settings, index.max(0) as usize);
            st.settings_dirty = true;
        }
    };
    let page = ui.global::<SettingsPage>();
    page.on_theme_selected(on_setting(|s, i| s.theme = i as u8));
    page.on_accent_selected(on_setting(|s, i| s.accent = i as u8));
    page.on_startup_selected(on_setting(|s, i| s.splash = i == 1));
    page.on_timeout_changed(on_setting(|s, seconds| s.timeout = seconds.min(255) as u16));
    {
        // Applied right away.
        let (state, hub) = (state.clone(), hub.clone());
        page.on_pointer_changed(move |speed, acceleration| {
            let mut st = state.borrow_mut();
            st.settings.pointer_speed = speed.clamp(0, settings::POINTER_SPEEDS.len() as i32 - 1) as u8;
            st.settings.pointer_acceleration = acceleration;
            st.settings_dirty = true;
            use_pointer(&hub, &st.settings);
        });
    }
    {
        // The default entry: the boot screen shows the new one.
        let (state, ui_weak) = (state.clone(), ui.as_weak());
        ui.global::<SettingsPage>().on_default_selected(move |i| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            st.settings.remember_last = i == 1;
            st.settings_dirty = true;
            show_cards(&ui, &mut st);
        });
    }
    // The splash screen's text and hint are in slingshot.toml too.
    {
        let (state, ui_weak, backend, show_status) = (state.clone(), ui.as_weak(), backend.clone(), show_status.clone());
        ui.global::<SettingsPage>().on_splash_text_entered(move |text| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            let text = text.trim();
            if text == st.config.splash_text {
                return;
            }
            let new = Config { splash_text: text.into(), ..(*st.config).clone() };
            let saved = commit_config(&mut st, &*backend, new, false);
            ui.global::<SettingsPage>().set_splash_text(st.config.splash_text.as_str().into());
            drop(st);
            show_status(&saved.map_or_else(|e| e, |()| tr("Saved in slingshot.toml")));
        });
    }
    {
        let (state, ui_weak, backend, show_status) = (state.clone(), ui.as_weak(), backend.clone(), show_status.clone());
        ui.global::<SettingsPage>().on_splash_shown(move |which, shown| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            let mut new = (*st.config).clone();
            match which.as_str() {
                "name" => new.splash_name = shown,
                "picture" => new.splash_picture = shown,
                _ => new.splash_hint = shown,
            }
            let saved = commit_config(&mut st, &*backend, new, false);
            let page = ui.global::<SettingsPage>();
            page.set_splash_name(st.config.splash_name);
            page.set_splash_picture_shown(st.config.splash_picture);
            page.set_splash_hint(st.config.splash_hint);
            drop(st);
            show_status(&saved.map_or_else(|e| e, |()| tr("Saved in slingshot.toml")));
        });
    }
    // The splash picture and the wallpaper are in slingshot.toml, not in
    // the saved settings.
    for which in [ScreenPicture::Splash, ScreenPicture::Wallpaper] {
        let (state, ui_weak, backend, show_status) = (state.clone(), ui.as_weak(), backend.clone(), show_status.clone());
        let selected = move |picture: i32| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            let file = (picture as usize).checked_sub(1).and_then(|i| st.pictures.get(i)).cloned().unwrap_or_default();
            let new = which.with(&st.config, file.clone());
            match commit_config(&mut st, &*backend, new, false) {
                Ok(()) => {
                    let image = if file.is_empty() { None } else { load_picture(&*backend, &mut st.images, &file) };
                    which.show(&ui, image.unwrap_or_default());
                    drop(st);
                    show_status(&tr("Saved in slingshot.toml"));
                }
                Err(e) => {
                    let old = picture_index(&st.pictures, Some(which.file(&st.config)));
                    which.set_index(&ui, old as i32);
                    drop(st);
                    show_status(&e);
                }
            }
        };
        match which {
            ScreenPicture::Splash => ui.global::<SettingsPage>().on_splash_picture_selected(selected),
            ScreenPicture::Wallpaper => ui.global::<SettingsPage>().on_wallpaper_picture_selected(selected),
        }
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let backend = backend.clone();
        let show_status = show_status.clone();
        ui.global::<SettingsPage>().on_reset_settings(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let reset = reset_settings(caps.configure, &mut state.borrow_mut(), &*backend);
            if let Err(e) = reset {
                show_status(&e);
                return;
            }
            let defaults = state.borrow().defaults;
            show_settings(&ui, &defaults);
            // Applied as if chosen (the texts, the keys, the pointer).
            let page = ui.global::<SettingsPage>();
            page.invoke_language_selected(defaults.language.into());
            page.invoke_keyboard_selected(defaults.keyboard.into());
            page.invoke_pointer_changed(defaults.pointer_speed.into(), defaults.pointer_acceleration);
            let mut st = state.borrow_mut();
            st.settings = defaults;
            st.settings_dirty = false;
            drop(st);
            show_status(&tr("Settings reset to their defaults"));
        });
    }

    // Resolution
    let update_mode_label = {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let hub = hub.clone();
        move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let modes = hub.modes.borrow();
            let st = state.borrow();
            if let Some(&(w, h)) = modes.get(st.mode_candidate) {
                ui.global::<SettingsPage>().set_mode_label(mode_label(w, h));
                ui.global::<SettingsPage>().set_mode_is_current((w, h) == hub.current_size.get());
            }
            ui.global::<SettingsPage>().set_mode_count(modes.len() as i32);
            if let Some(&(w, h)) = modes.last() {
                ui.global::<SettingsPage>().set_mode_max(mode_label(w, h));
            }
        }
    };
    {
        // Called by the platform once the display is set up, and after each
        // mode change.
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let hub2 = hub.clone();
        let backend = backend.clone();
        let update_mode_label = update_mode_label.clone();
        {
            let (ui_weak, state, backend, tick) = (ui.as_weak(), state.clone(), backend.clone(), tick.clone());
            *hub.on_devices_connected.borrow_mut() = Some(Box::new(move |drivers| {
                if let Some(ui) = ui_weak.upgrade() {
                    devices_connected(&ui, &mut state.borrow_mut(), &*backend, &tick, drivers);
                }
            }));
        }
        {
            let (state, ui_weak) = (state.clone(), ui.as_weak());
            *hub.on_full_redraw.borrow_mut() = Some(Box::new(move |took| {
                if let Some(ui) = ui_weak.upgrade() {
                    let mut st = state.borrow_mut();
                    st.full_redraw = st.full_redraw.max(Some(took));
                    update_animations(&ui, &mut st);
                }
            }));
        }
        *hub.on_mode_changed.borrow_mut() = Some(Box::new(move |w, h| {
            let index = hub2.modes.borrow().iter().position(|m| *m == (w, h));
            {
                let mut st = state.borrow_mut();
                if let Some(i) = index {
                    st.mode_candidate = i;
                }
                // Another size to draw: measured again.
                st.full_redraw = None;
            }
            update_mode_label();
            if let Some(ui) = ui_weak.upgrade() {
                // Refresh the Display card.
                update_system_info(&ui, &*backend, &hub2);
            }
        }));
    }
    let step_mode = {
        let state = state.clone();
        let hub = hub.clone();
        let update = update_mode_label.clone();
        move |forward: bool| {
            let n = hub.modes.borrow().len();
            if n == 0 {
                return;
            }
            let mut st = state.borrow_mut();
            st.mode_candidate = (st.mode_candidate + if forward { 1 } else { n - 1 }) % n;
            drop(st);
            update();
        }
    };
    {
        let step_mode = step_mode.clone();
        ui.global::<SettingsPage>().on_previous_mode(move || step_mode(false));
    }
    ui.global::<SettingsPage>().on_next_mode(move || step_mode(true));
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let hub = hub.clone();
        let backend = backend.clone();
        ui.global::<SettingsPage>().on_apply_mode(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let candidate = hub.modes.borrow().get(state.borrow().mode_candidate).copied();
            let Some((w, h)) = candidate else { return };
            if (w, h) == hub.current_size.get() {
                return;
            }
            let mut st = state.borrow_mut();
            st.revert_to = Some(hub.current_size.get());
            st.revert_left = 10;
            drop(st);
            ui.global::<SettingsPage>().set_revert_countdown(10);
            backend.request_mode(&hub, ui.window(), w, h);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let hub = hub.clone();
        ui.global::<SettingsPage>().on_keep_mode(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            st.revert_to = None;
            st.revert_left = -1;
            let (w, h) = hub.current_size.get();
            st.settings.mode = (w as u16, h as u16);
            st.settings_dirty = true;
            ui.global::<SettingsPage>().set_revert_countdown(-1);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let hub = hub.clone();
        let backend = backend.clone();
        ui.global::<SettingsPage>().on_revert_mode(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            st.revert_left = -1;
            let revert_to = st.revert_to.take();
            drop(st);
            ui.global::<SettingsPage>().set_revert_countdown(-1);
            if let Some((w, h)) = revert_to {
                backend.request_mode(&hub, ui.window(), w, h);
            }
        });
    }

    // Boot order editing
    let refresh_order = {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let order_model = order_model.clone();
        move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let st = state.borrow();
            let entries: Vec<BootEntry> = st.edited.iter().map(|i| option_entry(&st.options[*i])).collect();
            order_model.set_vec(entries);
            ui.global::<SettingsPage>().set_order_dirty(st.edited_order(&st.firmware_order) != st.firmware_order);
            ui.global::<SettingsPage>().set_order_changed_from_original(st.edited_order(&st.original_order) != st.original_order);
        }
    };
    // The language: right away, the .slint texts and the Rust ones.
    {
        let (state, ui_weak, backend, hub, refresh_order) = (state.clone(), ui.as_weak(), backend.clone(), hub.clone(), refresh_order.clone());
        ui.global::<SettingsPage>().on_language_selected(move |index| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            st.settings.language = index.max(0) as u8;
            st.settings_dirty = true;
            use_language(&*backend, &st.config, st.settings.language);
            // The descriptions made when reading the entries.
            let fresh = backend.retranslate();
            for option in &mut st.options {
                if let Some(f) = fresh.options.iter().find(|f| f.number == option.number) {
                    option.subtitle.clone_from(&f.subtitle);
                }
            }
            st.found = fresh.found;
            show_cards(&ui, &mut st);
            let (config, settings) = (st.config.clone(), st.settings);
            drop(st);
            refresh_order();
            update_system_info(&ui, &*backend, &hub);
            show_language_choices(&ui, &*backend, &config, &settings);
        });
    }
    {
        let (state, ui_weak, backend, hub) = (state.clone(), ui.as_weak(), backend.clone(), hub.clone());
        ui.global::<SettingsPage>().on_keyboard_selected(move |index| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            st.settings.keyboard = index.max(0) as u8;
            st.settings_dirty = true;
            use_keymap(&*backend, &hub, &st.config, st.settings.keyboard);
            ui.global::<SettingsPage>().set_keyboard_index(index);
        });
    }
    // "Add…" in the picture choosers: the desktop's file dialog, or
    // Slingshot's own, which copies the file next to slingshot.toml.
    let picture_added: Rc<dyn Fn(PictureFor, String)> = {
        let (state, ui_weak, backend) = (state.clone(), ui.as_weak(), backend.clone());
        Rc::new(move |which, name: String| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let picture = {
                let mut st = state.borrow_mut();
                st.images.remove(&name);
                st.refresh_pictures(&ui, &*backend, Some(&name));
                picture_index(&st.pictures, Some(&name))
            };
            match which {
                PictureFor::Entry => {
                    ui.global::<Customize>().set_picture(picture as i32);
                    show_picture_preview(&ui, &mut state.borrow_mut(), &*backend, picture);
                }
                PictureFor::Screen(screen) => {
                    screen.set_index(&ui, picture as i32);
                    match screen {
                        ScreenPicture::Splash => ui.global::<SettingsPage>().invoke_splash_picture_selected(picture as i32),
                        ScreenPicture::Wallpaper => ui.global::<SettingsPage>().invoke_wallpaper_picture_selected(picture as i32),
                    }
                }
            }
        })
    };
    let add_picture: Rc<dyn Fn(PictureFor)> = {
        let (state, ui_weak, backend, show_status, added) = (state.clone(), ui.as_weak(), backend.clone(), show_status.clone(), picture_added.clone());
        Rc::new(move |which| {
            let Some(ui) = ui_weak.upgrade() else { return };
            if !caps.picture_dialog {
                open_browser(&ui, &mut state.borrow_mut(), &*backend, Some(which));
                return;
            }
            match backend.add_picture() {
                Ok(Some(name)) => added(which, name),
                Ok(None) => {}
                Err(e) => show_status(&trf("Could not add the picture: {}", &[&e])),
            }
        })
    };
    {
        let add = add_picture.clone();
        ui.global::<Customize>().on_add_picture(move || add(PictureFor::Entry));
    }
    {
        let add = add_picture.clone();
        ui.global::<SettingsPage>().on_add_splash_picture(move || add(PictureFor::Screen(ScreenPicture::Splash)));
    }
    {
        let add = add_picture.clone();
        ui.global::<SettingsPage>().on_add_wallpaper_picture(move || add(PictureFor::Screen(ScreenPicture::Wallpaper)));
    }

    // The Install screen (`slingshot config` on the desktop).
    if caps.install {
        install_screen(&ui, &backend, show_status.clone());
    }

    // "Start a file…": the file browser.
    {
        let (state, ui_weak, backend) = (state.clone(), ui.as_weak(), backend.clone());
        ui.global::<FileBrowser>().on_show(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            open_browser(&ui, &mut state.borrow_mut(), &*backend, None);
        });
    }
    {
        let (state, ui_weak, backend) = (state.clone(), ui.as_weak(), backend.clone());
        let start = start_with_transition.clone();
        let (picture_added, show_status) = (picture_added.clone(), show_status.clone());
        let unlock_disk = unlock_disk.clone();
        ui.global::<FileBrowser>().on_open(move |index| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            let b = &mut st.browse;
            let Some(row) = b.rows.get(index as usize).cloned() else {
                log::warn!("file browser: row {index} of {}", b.rows.len());
                return;
            };
            log::info!("file browser: {row:?}");
            let select = match row {
                BrowseRow::Volume(v) => {
                    b.volume = Some(v);
                    b.folders.clear();
                    None
                }
                BrowseRow::Unreadable(v) => {
                    let Some(v) = b.unreadable.get(v) else { return };
                    // An encrypted disk Slingshot can open: its passphrase,
                    // then the disk decrypted, among the others.
                    if let Some(id) = v.unlock {
                        let name = v.name.clone();
                        let locked = LockedVolume { id, name: name.clone(), detail: v.detail.clone() };
                        drop(st);
                        let (state, ui_weak, backend) = (state.clone(), ui_weak.clone(), backend.clone());
                        unlock_disk(
                            locked,
                            Box::new(move |unlocked| {
                                let Some(ui) = ui_weak.upgrade() else { return };
                                if !unlocked {
                                    return;
                                }
                                let opened = backend.open_unlocked(id);
                                let mut st = state.borrow_mut();
                                let b = &mut st.browse;
                                let before = core::mem::take(&mut b.volumes);
                                b.reload_volumes(&*backend);
                                // The new disk: the one the list didn't have.
                                let new = b.volumes.iter().position(|v| !before.contains(v));
                                match (opened, new) {
                                    (Ok(()), Some(v)) => {
                                        b.volume = Some(v);
                                        b.folders.clear();
                                        show_browse(&ui, b, &*backend, None);
                                    }
                                    (Ok(()), None) => {
                                        show_browse(&ui, b, &*backend, Some(&name));
                                        ui.global::<FileBrowser>().set_message(
                                            tr("Unlocked, but its files can't be shown: this computer has no driver for what's inside.").into(),
                                        );
                                    }
                                    (Err(e), _) => {
                                        show_browse(&ui, b, &*backend, Some(&name));
                                        ui.global::<FileBrowser>().set_message(e.into());
                                    }
                                }
                            }),
                        );
                        return;
                    }
                    let why = if crate::parse::fstype::is_encrypted(v.format) {
                        trf("{} is encrypted in a way Slingshot can't unlock.", &[&v.name])
                    } else {
                        trf("Slingshot can't read {} disks: this computer has no driver for them.", &[&v.format])
                    };
                    ui.global::<FileBrowser>().set_message(why.into());
                    return;
                }
                BrowseRow::Up if b.folders.is_empty() => b.volume.take().and_then(|v| b.volumes.get(v)).map(|v| v.name.clone()),
                BrowseRow::Up => b.folders.pop(),
                BrowseRow::Folder(name) => {
                    b.folders.push(name);
                    None
                }
                BrowseRow::File(name) if b.pick.is_some() => {
                    // A picture: copied next to slingshot.toml.
                    let (Some(volume), Some(which)) = (b.volume, b.pick) else { return };
                    let copied = backend.read_volume_file(volume, &b.path(Some(&name))).and_then(|data| backend.write_file(&name, &data));
                    drop(st);
                    ui.global::<FileBrowser>().set_visible(false);
                    match copied {
                        Ok(()) => picture_added(which, name),
                        Err(e) => show_status(&trf("Could not add the picture: {}", &[&e])),
                    }
                    return;
                }
                BrowseRow::File(name) => {
                    let Some(volume) = b.volume else { return };
                    let target = Target::File { volume, path: b.path(Some(&name)) };
                    drop(st);
                    ui.global::<FileBrowser>().set_visible(false);
                    let backend = backend.clone();
                    start(name, Box::new(move || backend.start(&target)));
                    return;
                }
            };
            show_browse(&ui, b, &*backend, select.as_deref());
        });
    }
    {
        // Typing in the list: the first row whose name starts with it.
        let ui_weak = ui.as_weak();
        ui.global::<FileBrowser>().on_find(move |typed| {
            // Function keys and the like come as private-use characters.
            if typed.chars().any(|c| c.is_control() || ('\u{e000}'..='\u{f8ff}').contains(&c)) {
                return -2;
            }
            let Some(ui) = ui_weak.upgrade() else { return -1 };
            let rows = ui.global::<FileBrowser>().get_rows();
            let names: Vec<String> = (0..slint::Model::row_count(&rows))
                .filter_map(|i| slint::Model::row_data(&rows, i))
                .map(|r| String::from(r.name.as_str()))
                .collect();
            find_row(&names, &typed, ui.global::<FileBrowser>().get_current().max(0) as usize).map_or(-1, |i| i as i32)
        });
    }
    {
        // A path typed in the location field: `\EFI\BOOT` or `/EFI/BOOT/x.efi`,
        // on the disk being browsed (or the one selected in the list).
        let (state, ui_weak, backend) = (state.clone(), ui.as_weak(), backend.clone());
        ui.global::<FileBrowser>().on_go(move |path| {
            let Some(ui) = ui_weak.upgrade() else { return false };
            let mut st = state.borrow_mut();
            let b = &mut st.browse;
            let absolute = path.starts_with(['\\', '/']);
            // From all disks: the selected one first, then the others.
            let candidates: Vec<(usize, Vec<String>)> = match b.volume {
                Some(v) => alloc::vec![(v, if absolute { Vec::new() } else { b.folders.clone() })],
                None => {
                    let selected = match b.rows.get(ui.global::<FileBrowser>().get_current().max(0) as usize) {
                        Some(BrowseRow::Volume(v)) => Some(*v),
                        _ => None,
                    };
                    selected.into_iter().chain((0..b.volumes.len()).filter(|v| Some(*v) != selected)).map(|v| (v, Vec::new())).collect()
                }
            };
            let mut missing = String::new();
            for (volume, start) in candidates {
                match resolve_path(&*backend, volume, start, &path) {
                    Ok((folders, select)) => {
                        b.volume = Some(volume);
                        b.folders = folders;
                        show_browse(&ui, b, &*backend, select.as_deref());
                        return true;
                    }
                    Err(part) => {
                        if missing.is_empty() {
                            missing = part;
                        }
                    }
                }
            }
            ui.global::<FileBrowser>().set_message(trf("Not found: {}", &[&missing]).into());
            false
        });
    }
    {
        let (state, ui_weak) = (state.clone(), ui.as_weak());
        ui.global::<FileBrowser>().on_up(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            // Row 0 is "..", on a disk.
            if state.borrow().browse.volume.is_some() {
                ui.global::<FileBrowser>().invoke_open(0);
            }
        });
    }
    {
        let (state, ui_weak, backend, show_status, refresh) = (state.clone(), ui.as_weak(), backend.clone(), show_status.clone(), refresh_order.clone());
        ui.global::<FileBrowser>().on_add(move |index| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            let b = &st.browse;
            let (Some(BrowseRow::File(name)), Some(volume)) = (b.rows.get(index as usize), b.volume) else { return };
            let title = b.entry_title(name);
            match backend.add_boot_entry(volume, &b.path(Some(name)), &title) {
                Ok(number) => {
                    ui.global::<FileBrowser>().set_visible(false);
                    let boot = backend.boot_data();
                    st.edited = (0..boot.options.len()).collect();
                    st.options = boot.options;
                    // What's found on the disks: the new entry may start one.
                    st.found = boot.found;
                    st.original_order = boot.order.clone();
                    st.firmware_order = boot.order;
                    show_cards(&ui, &mut st);
                    drop(st);
                    refresh();
                    show_status(&trf("Added “{}” as Boot{}", &[&title, &format!("{number:04X}")]));
                }
                Err(e) => {
                    drop(st);
                    show_status(&trf("Could not add a boot entry: {}", &[&e]));
                }
            }
        });
    }
    {
        let state = state.clone();
        let refresh = refresh_order.clone();
        ui.global::<SettingsPage>().on_move_entry(move |from, to| {
            let mut st = state.borrow_mut();
            let n = st.edited.len() as i32;
            if from < 0 || to < 0 || from >= n || to >= n {
                return;
            }
            let item = st.edited.remove(from as usize);
            st.edited.insert(to as usize, item);
            drop(st);
            refresh();
        });
    }
    {
        let state = state.clone();
        let refresh = refresh_order.clone();
        ui.global::<SettingsPage>().on_restore_order(move || {
            let mut st = state.borrow_mut();
            // Put the visible entries back in their original relative order.
            let original = st.original_order.clone();
            let numbers: Vec<u16> = st.options.iter().map(|o| o.number).collect();
            st.edited.sort_by_key(|i| original.iter().position(|n| *n == numbers[*i]).unwrap_or(usize::MAX));
            drop(st);
            refresh();
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let backend = backend.clone();
        let refresh = refresh_order.clone();
        let show_status = show_status.clone();
        ui.global::<SettingsPage>().on_save_order(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let st = state.borrow();
            let new_order = st.edited_order(&st.firmware_order);
            drop(st);
            match backend.set_boot_order(&new_order) {
                Ok(()) => {
                    let mut st = state.borrow_mut();
                    st.firmware_order = new_order;
                    // The boot picker follows the firmware order.
                    let options: Vec<BootOption> = st.edited.iter().map(|i| st.options[*i].clone()).collect();
                    st.options = options;
                    st.edited = (0..st.options.len()).collect();
                    show_cards(&ui, &mut st);
                    drop(st);
                    refresh();
                    show_status(&tr("Boot order saved to the firmware"));
                }
                Err(e) => show_status(&trf("Could not save the boot order: {}", &[&e])),
            }
        });
    }

    // The splash screen only shows while counting down.
    show_startup();

    // Both platforms report the mode before the first frame, which fills the
    // System screen.
    backend.ui_created(&hub, ui.window());

    // Apply the saved resolution, if it is still available.
    if settings.mode != (0, 0) {
        let (w, h) = (settings.mode.0 as usize, settings.mode.1 as usize);
        if hub.modes.borrow().contains(&(w, h)) || hub.modes.borrow().is_empty() {
            backend.request_mode(&hub, ui.window(), w, h);
        }
    }

    ui.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_ahead() {
        let names: Vec<String> = ["..", "EFI", "boot", "Bootx64.efi", "ext"].map(String::from).to_vec();
        assert_eq!(find_row(&names, "e", 0), Some(1));
        assert_eq!(find_row(&names, "ef", 1), Some(1), "typing on stays on the row");
        assert_eq!(find_row(&names, "ee", 1), Some(4), "the same letter goes on");
        assert_eq!(find_row(&names, "e", 4), Some(1), "and round");
        assert_eq!(find_row(&names, "bootx", 2), Some(3));
        assert_eq!(find_row(&names, "z", 0), None);
    }

}
