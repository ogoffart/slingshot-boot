// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Slingshot: a graphical UEFI boot manager built with Slint.
//!
//! - Built for `target_os = "uefi"`: the boot manager. [`firmware`] reads and
//!   writes the firmware variables, starts the systems, and runs the UI on
//!   [`uefi_platform`], a Slint platform for UEFI that knows nothing else of
//!   Slingshot.
//! - Built for a desktop: [`desktop`]: `slingshot config` (the default), the
//!   same UI configuring this computer, and `slingshot simulate`, the UI in
//!   a winit window with simulated firmware.
//!
//! The UI and its logic are in [`app`], which only talks to the machine
//! through the [`app::Backend`] trait.

#![cfg_attr(target_os = "uefi", no_std)]
#![cfg_attr(target_os = "uefi", no_main)]

extern crate alloc;

mod app;
mod parse;
mod tr;

cfg_select! {
    target_os = "uefi" => {
        mod firmware;
        mod uefi_platform;

        #[uefi::entry]
        fn main() -> uefi::Status {
            firmware::main()
        }
    }
    _ => {
        mod desktop;

        fn main() {
            desktop::main()
        }
    }
}
