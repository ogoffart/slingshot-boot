// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! The batteries and the AC adapter: their state through ACPI (`acpi.rs`, on
//! x86 PCs), completed by the static details SMBIOS gives.
//!
//! Nothing here runs at startup: only on the System screen. The interpreter
//! panics on some AML it doesn't support, and a panic stops Slingshot: the
//! panic handler then remembers it in a variable (see `probing`), and this
//! version of Slingshot doesn't read the batteries on that machine again.

use alloc::vec::Vec;
use core::cell::OnceCell;
use core::sync::atomic::{AtomicBool, Ordering};

use uefi::cstr16;
use uefi::runtime::{self, VariableAttributes};

use super::nvram::VENDOR;
use crate::parse::battery::{Battery, Power};

// Port I/O and the PC's embedded controller; the `battery` feature.
#[cfg(all(target_arch = "x86_64", feature = "battery"))]
pub(super) mod acpi;

/// Elsewhere, only SMBIOS's details. ARM laptops' firmware reaches the
/// battery through I2C or other controllers this doesn't handle.
#[cfg(not(all(target_arch = "x86_64", feature = "battery")))]
mod acpi {
    use crate::parse::battery::Power;

    pub struct Acpi;

    impl Acpi {
        pub fn load() -> Option<Acpi> {
            None
        }

        pub fn power(&self) -> Power {
            Power::default()
        }
    }
}

/// Set while AML runs, for the panic handler.
static PROBING: AtomicBool = AtomicBool::new(false);
const NO_PROBE: &uefi::CStr16 = cstr16!("SlingshotNoBattery");

/// Called by the panic handler: whether the panic happened while running
/// AML. If so, this version won't read the batteries on this machine again.
pub fn panicked_in_aml() -> bool {
    if !PROBING.load(Ordering::Relaxed) {
        return false;
    }
    let _ = runtime::set_variable(
        NO_PROBE,
        &VENDOR,
        VariableAttributes::NON_VOLATILE | VariableAttributes::BOOTSERVICE_ACCESS,
        env!("CARGO_PKG_VERSION").as_bytes(),
    );
    true
}

/// The batteries, through ACPI, completed by SMBIOS's static details.
pub struct Batteries {
    /// Loaded the first time, `None` when it can't.
    acpi: OnceCell<Option<acpi::Acpi>>,
    smbios: Vec<Battery>,
    /// The log said that only SMBIOS's details are there.
    told: core::cell::Cell<bool>,
}

impl Batteries {
    pub fn new(smbios: Vec<Battery>) -> Self {
        Batteries {
            acpi: OnceCell::new(),
            smbios,
            told: Default::default(),
        }
    }

    pub fn power(&self) -> Power {
        let mut power = self.acpi.get_or_init(acpi::Acpi::load).as_ref().map(acpi::Acpi::power).unwrap_or_default();
        if power.batteries.is_empty() {
            // No charge level, but the details.
            if !self.smbios.is_empty() && !self.told.replace(true) {
                log::info!("battery: no reading from ACPI: SMBIOS's {} battery details only", self.smbios.len());
            }
            power.batteries = self.smbios.clone();
        } else {
            for (b, s) in power.batteries.iter_mut().zip(&self.smbios) {
                for (field, value) in [
                    (&mut b.manufacturer, &s.manufacturer),
                    (&mut b.manufacture_date, &s.manufacture_date),
                    (&mut b.technology, &s.technology),
                    (&mut b.serial, &s.serial),
                    (&mut b.model, &s.model),
                ] {
                    if field.is_empty() {
                        field.clone_from(value);
                    }
                }
            }
        }
        power
    }
}
