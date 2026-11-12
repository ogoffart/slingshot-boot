// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Where what the user changes is saved: slingshot.toml, and the settings
//! (the firmware's variable, or slingshot.toml when configuring from the
//! desktop).

use alloc::rc::Rc;
use alloc::string::String;

use super::config::{self, Config};
use super::{Backend, Saved, Settings, State, chosen_language, settings, stamp};
use crate::tr::{tr, trf};

/// Writes `new` as slingshot.toml and makes it the configuration: the error
/// to show if it couldn't. Unless `force`, not when the file as read had
/// problems: what couldn't be read would be lost.
pub(super) fn commit_config(st: &mut State, backend: &dyn Backend, mut new: Config, force: bool) -> Result<(), String> {
    let not_saved = |e: &str| trf("Could not save slingshot.toml: {}", &[&e]);
    if !force && !st.config.warnings.is_empty() {
        return Err(not_saved("it has errors (see the log), and saving would lose what couldn't be read"));
    }
    new.warnings.clear();
    backend.write_file("slingshot.toml", config::to_text(&new).as_bytes()).map_err(|e| not_saved(&e))?;
    st.config = Rc::new(new);
    Ok(())
}

// The settings are saved in the firmware's variable, or `in_file`:
// slingshot.toml's defaults (`Capabilities::configure`).

/// The settings shown at first: what was saved over the `defaults`.
pub(super) fn initial_settings(in_file: bool, config: &Config, defaults: Settings, saved: Option<Saved>) -> Settings {
    let mut settings = saved.and_then(|saved| saved.over(&defaults)).unwrap_or(defaults);
    // Not defaults from slingshot.toml: kept when the file changed.
    if let Some(saved) = saved {
        settings.language = saved.settings.language;
        settings.keyboard = saved.settings.keyboard;
    }
    if in_file {
        // Saved in the file: its own settings.
        let chosen = config.language.as_deref().map(crate::tr::language_of);
        settings.language = chosen.and_then(|c| settings::languages().position(|l| l == c)).map_or(0, |i| i as u8 + 1);
        settings.keyboard = if config.keyboard == 0 { 0 } else { config.keyboard as u8 + 1 };
    }
    settings
}

/// `config` with `st.settings` as its defaults, for slingshot.toml.
pub(super) fn with_settings(st: &State, config: Config) -> Config {
    let settings = st.settings;
    let defaults = config::SettingsDefaults::between(&st.builtin, &settings);
    let language = chosen_language(settings.language).map(String::from);
    let keyboard = usize::from(settings.keyboard).saturating_sub(1);
    Config { defaults, language, keyboard, ..config }
}

/// Saves `st.settings`: what to say, or the error.
pub(super) fn save_settings(in_file: bool, st: &mut State, backend: &dyn Backend) -> Result<Option<String>, String> {
    let settings = st.settings;
    if in_file {
        let new = with_settings(st, (*st.config).clone());
        commit_config(st, backend, new, true)?;
        st.defaults = settings;
        Ok(Some(tr("Settings saved in slingshot.toml")))
    } else {
        let saved = Saved { settings, base: Some(stamp(&st.defaults)) };
        backend.save_settings(&saved).map_err(|e| trf("Could not save the settings: {}", &[&e]))?;
        Ok(None)
    }
}

/// Forgets what was saved: `st.defaults` are the settings then.
pub(super) fn reset_settings(in_file: bool, st: &mut State, backend: &dyn Backend) -> Result<(), String> {
    if in_file {
        // Back to the built-in settings: no defaults in the file.
        let new = Config { defaults: config::SettingsDefaults::default(), language: None, keyboard: 0, ..(*st.config).clone() };
        commit_config(st, backend, new, true)?;
        st.defaults = st.builtin;
        Ok(())
    } else {
        backend.reset_settings().map_err(|e| trf("Could not reset the settings: {}", &[&e]))
    }
}
