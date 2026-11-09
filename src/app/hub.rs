// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! State shared between the platform (event loop, display, input) and the
//! application logic.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

/// The logical size the UI is designed for. The scale factor is picked so
/// that the logical size is at least this.
pub const DESIGN_WIDTH: f32 = 1280.;
pub const DESIGN_HEIGHT: f32 = 800.;

/// The scale factors the UI uses (the glyphs scale to any: see build.rs).
pub const SCALE_FACTORS: &[f32] = &[0.8, 1.0, 1.25, 1.5, 2.0];

/// Picks the scale factor so that the logical size is at least the design size.
pub fn scale_factor_for(width: usize, height: usize) -> f32 {
    let ideal = (width as f32 / DESIGN_WIDTH).min(height as f32 / DESIGN_HEIGHT);
    SCALE_FACTORS.iter().copied().rfind(|s| *s <= ideal + 0.001).unwrap_or(SCALE_FACTORS[0])
}

/// A callback the UI sets and the platform calls.
type Callback<F> = RefCell<Option<Box<F>>>;

#[derive(Default)]
pub struct Hub {
    /// Called for every key press or pointer movement.
    pub on_input: Callback<dyn Fn()>,
    /// Called after the resolution changed (width, height).
    pub on_mode_changed: Callback<dyn Fn(usize, usize)>,
    /// Called when the pointer diagnostics change.
    pub on_pointer_status: Callback<dyn Fn(&str)>,
    /// Called with whether redrawing the whole screen is too slow to
    /// animate (a large screen, a slow machine): measured on full redraws.
    pub on_slow_rendering: Callback<dyn Fn(bool)>,
    /// Called once, after the first frame, when the devices are connected:
    /// whether drivers were started too (file systems they read).
    pub on_devices_connected: Callback<dyn Fn(bool)>,
    /// Usable display modes (width, height), sorted by size.
    pub modes: RefCell<Vec<(usize, usize)>>,
    /// The mode requested by the application, applied by the event loop.
    pub requested_mode: Cell<Option<(usize, usize)>>,
    pub current_size: Cell<(usize, usize)>,
    pub pointer_status: RefCell<String>,
}

impl Hub {
    #[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
    pub fn input(&self) {
        if let Some(cb) = &*self.on_input.borrow() {
            cb();
        }
    }

    pub fn mode_changed(&self, width: usize, height: usize) {
        self.current_size.set((width, height));
        if let Some(cb) = &*self.on_mode_changed.borrow() {
            cb(width, height);
        }
    }

    #[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
    pub fn devices_connected(&self, drivers: bool) {
        if let Some(cb) = &*self.on_devices_connected.borrow() {
            cb(drivers);
        }
    }

    #[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
    pub fn slow_rendering(&self, slow: bool) {
        if let Some(cb) = &*self.on_slow_rendering.borrow() {
            cb(slow);
        }
    }

    #[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
    pub fn set_pointer_status(&self, status: String) {
        if *self.pointer_status.borrow() != status {
            log::info!("{status}");
        }
        if let Some(cb) = &*self.on_pointer_status.borrow() {
            cb(&status);
        }
        *self.pointer_status.borrow_mut() = status;
    }
}

#[cfg(test)]
mod tests {
    use super::scale_factor_for;

    #[test]
    fn scale_factors() {
        assert_eq!(scale_factor_for(1280, 800), 1.0);
        assert_eq!(scale_factor_for(1920, 1080), 1.25);
        assert_eq!(scale_factor_for(1920, 1200), 1.5);
        assert_eq!(scale_factor_for(2256, 1504), 1.75_f32.min(1.5));
        assert_eq!(scale_factor_for(2560, 1600), 2.0);
        assert_eq!(scale_factor_for(3840, 2160), 2.0, "the largest embedded factor");
        assert_eq!(scale_factor_for(1024, 768), 0.8);
        assert_eq!(scale_factor_for(640, 480), 0.8, "the smallest embedded factor");
    }
}
