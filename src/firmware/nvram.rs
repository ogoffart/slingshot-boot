// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Stores the app settings in one small NVRAM variable that is only visible
//! while boot services run (no RUNTIME_ACCESS), so the OS cannot see it.

use uefi::runtime::{self, VariableAttributes, VariableVendor};
use crate::app::Saved;
use uefi::{cstr16, guid};

const NAME: &uefi::CStr16 = cstr16!("SlingshotSettings");
/// Slingshot's own variables.
pub(super) const VENDOR: VariableVendor = VariableVendor(guid!("6f1f43a3-6b1c-4d51-9a3b-6c1e2f5c7d42"));

pub fn load() -> Option<Saved> {
    let mut buf = [0u8; 32];
    let (data, _) = runtime::get_variable(NAME, &VENDOR, &mut buf).ok()?;
    Saved::decode(data)
}

pub fn save(settings: &Saved) -> uefi::Result {
    runtime::set_variable(
        NAME,
        &VENDOR,
        VariableAttributes::NON_VOLATILE | VariableAttributes::BOOTSERVICE_ACCESS,
        &settings.encode(),
    )
}

pub fn reset() -> uefi::Result {
    match runtime::delete_variable(NAME, &VENDOR) {
        Err(e) if e.status() == uefi::Status::NOT_FOUND => Ok(()),
        r => r,
    }
}

const LAST: &uefi::CStr16 = cstr16!("SlingshotLastEntry");

/// The entry started last (`Card::key`), as UTF-8.
pub fn last_entry() -> Option<alloc::string::String> {
    let (data, _) = runtime::get_variable_boxed(LAST, &VENDOR).ok()?;
    core::str::from_utf8(&data).ok().map(Into::into)
}

/// Keeps the entry started last; written only when it changes (NVRAM wears).
pub fn set_last_entry(key: &str) {
    if last_entry().as_deref() == Some(key) {
        return;
    }
    let attributes = VariableAttributes::NON_VOLATILE | VariableAttributes::BOOTSERVICE_ACCESS;
    if let Err(e) = runtime::set_variable(LAST, &VENDOR, attributes, key.as_bytes()) {
        log::info!("SlingshotLastEntry: {:?}", e.status());
    }
}

/// systemd's own variables (`HibernateLocation`).
const SYSTEMD: VariableVendor = VariableVendor(guid!("8cf2644b-4b0b-428f-9387-6d876050dc67"));

/// What hibernated, if a system did and hasn't resumed (systemd clears the
/// variable once resumed).
pub fn hibernated() -> Option<crate::parse::hibernate::Hibernated> {
    let (data, _) = runtime::get_variable_boxed(cstr16!("HibernateLocation"), &SYSTEMD).ok()?;
    let h = crate::parse::hibernate::parse(&data);
    log::info!("HibernateLocation: {h:?}");
    h
}
