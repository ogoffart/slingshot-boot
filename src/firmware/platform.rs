// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! What Slingshot adds to the UEFI platform (`crate::uefi_platform`): the
//! `Hub` it shares with the application, the keyboard layout, its own USB
//! mouse and touchpad drivers, and connecting the drivers after the first
//! frame.

use alloc::format;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::time::Duration;

use uefi::boot;
use uefi::proto::console::pointer::{AbsolutePointer, Pointer};

use crate::app::{Hub, scale_factor_for};
use crate::uefi_platform::{Hooks, PointerReport, uptime};

pub struct Slingshot {
    hub: Rc<Hub>,
    /// A dead key waiting for the next one.
    dead_key: Cell<Option<char>>,
    /// A frame of (nearly) the whole screen was drawn.
    warmed_up: Cell<bool>,
    pointers: Rc<Pointers>,
}

/// The pointers no firmware driver handles, read by Slingshot: found once
/// the devices are connected.
#[derive(Default)]
struct Pointers {
    usb: RefCell<Vec<super::usbmouse::UsbMouse>>,
    /// A touchpad on I2C (the `touchpad` feature).
    #[cfg(all(target_arch = "x86_64", feature = "touchpad"))]
    touchpad: RefCell<Option<super::touchpad::Touchpad>>,
}

/// Logs and shows the pointer protocols' handles: the console splitter
/// always provides both, more means the firmware has an actual mouse or
/// touch driver.
fn report_pointer_handles(hub: &Hub, when: &str) {
    let simple = boot::find_handles::<Pointer>().map(|h| h.len()).unwrap_or(0);
    let absolute = boot::find_handles::<AbsolutePointer>().map(|h| h.len()).unwrap_or(0);
    log::info!("pointer handles {when}: simple={simple} absolute={absolute}");
    hub.set_pointer_status(format!("Pointer: {simple} mouse, {absolute} touch protocol handles · no movement yet"));
}

impl Slingshot {
    pub fn new(hub: Rc<Hub>) -> Self {
        #[cfg(not(all(target_arch = "x86_64", feature = "touchpad")))]
        log::info!("touchpad: not in this build (the touchpad feature, x86 only)");
        report_pointer_handles(&hub, "at startup");
        Slingshot { hub, dead_key: Cell::new(None), warmed_up: Cell::new(false), pointers: Default::default() }
    }
}

/// Connects the drivers and the other devices (a "connect all" takes seconds
/// on some machines) a slice of about 10 ms at a time, between frames and
/// input: then the mice and the touchpad they make available, and the app
/// rescans the disks.
fn connect(mut connecting: super::Connecting, started: Duration, hub: Rc<Hub>, pointers: Rc<Pointers>) {
    let slice = uptime();
    if !connecting.step(|| uptime() - slice >= Duration::from_millis(10)) {
        slint::Timer::single_shot(Duration::ZERO, move || connect(connecting, started, hub, pointers));
        return;
    }
    *pointers.usb.borrow_mut() = super::usbmouse::find();
    #[cfg(all(target_arch = "x86_64", feature = "touchpad"))]
    {
        *pointers.touchpad.borrow_mut() = super::touchpad::find();
    }
    log::info!(
        "devices connected in {} ms, USB mice read by Slingshot: {}",
        (uptime() - started).as_millis(),
        pointers.usb.borrow().len()
    );
    report_pointer_handles(&hub, "once connected");
    hub.devices_connected(connecting.drivers);
}

impl Hooks for Slingshot {
    /// Those big enough for the UI (`AppWindow`'s minimum size) are offered.
    fn modes_found(&self, modes: &[(usize, usize)]) {
        *self.hub.modes.borrow_mut() = modes.iter().copied().filter(|&(w, h)| w >= 1024 && h >= 640).collect();
    }

    /// The UI's scale factor for the screen's size.
    fn mode_changed(&self, window: &slint::Window, width: usize, height: usize) {
        let scale_factor = scale_factor_for(width, height);
        window.dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor });
        self.hub.mode_changed(width, height);
    }

    fn type_key(&self, c: char, altgr: bool) -> Option<Vec<char>> {
        let mut dead_key = self.dead_key.get();
        let typed = self.hub.keymap.borrow().as_ref().and_then(|k| k.type_key(&mut dead_key, c, altgr));
        self.dead_key.set(dead_key);
        typed
    }

    fn poll_pointers(&self, reports: &mut Vec<PointerReport>) -> bool {
        let mut usb = self.pointers.usb.borrow_mut();
        for m in usb.iter_mut() {
            while let Some(r) = m.poll() {
                // Like EDK II's driver: one pixel per count.
                let delta = Some((r.dx as f32, r.dy as f32));
                reports.push(PointerReport { delta, wheel: r.wheel, left: r.left, right: r.right, ..Default::default() });
            }
        }
        #[cfg(all(target_arch = "x86_64", feature = "touchpad"))]
        if let Some(t) = &mut *self.pointers.touchpad.borrow_mut() {
            // Every report waiting: it sends one each ~10 ms and a frame can
            // take more, so reports left behind would make the pointer and
            // the clicks lag. Bounded, for a pad that repeats.
            for _ in 0..16 {
                let Some(r) = t.poll() else { break };
                let delta = Some((r.dx as f32, r.dy as f32));
                reports.push(PointerReport { delta, left: r.left, right: r.right, ..Default::default() });
            }
            return true;
        }
        !usb.is_empty()
    }

    /// The speed chosen in Settings, and with acceleration, up to 3 times
    /// more for fast moves.
    fn pointer_gain(&self, speed: f32) -> f32 {
        let (factor, acceleration) = self.hub.pointer.get().unwrap_or((1., true));
        // Under 0.4 px per ms (about 5 cm/s), as it comes.
        factor * if acceleration { (1. + (speed - 0.4) * 0.5).clamp(1., 3.) } else { 1. }
    }

    fn input(&self) {
        self.hub.input();
    }

    fn pointer_working(&self, relative: bool, absolute: bool) {
        self.hub.set_pointer_status(format!(
            "Pointer: {}{}{} working",
            if relative { "mouse" } else { "" },
            if relative && absolute { " and " } else { "" },
            if absolute { "absolute pointer" } else { "" },
        ));
    }

    /// A redraw of (nearly) the whole screen, after a mode change or during a
    /// screen transition, says whether full-screen animations can keep up.
    /// The first one is left out: it decodes the images and the glyphs too.
    fn frame_drawn(&self, covered: f32, took: Duration, at: Duration) {
        if covered >= 0.9 && self.warmed_up.replace(true) {
            self.hub.full_redraw(took, at);
        }
    }

    /// The menu is on the screen: now the drivers and the other devices.
    fn first_frame(&self) {
        connect(super::Connecting::start(), uptime(), self.hub.clone(), self.pointers.clone());
    }
}
