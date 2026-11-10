// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Slint platform for UEFI: a `MinimalSoftwareWindow` drawn into a buffer of
//! GOP `BltPixel`s, with keyboard, mouse and touch input read from the
//! firmware's console protocols. The event loop sleeps in `WaitForEvent`.

use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::time::Duration;

use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType, TargetPixel,
};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{LogicalPosition, PhysicalSize, SharedString};
use uefi::Event;
use uefi::boot::{
    self, EventType, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol, TimerTrigger, Tpl,
};
use uefi::proto::console::gop::{BltOp, BltPixel, BltRegion, GraphicsOutput, Mode};
use uefi::proto::console::pointer::{AbsolutePointer, Pointer};
use uefi::proto::console::text::{Input, InputEx, Key as UefiKey, ScanCode};
use uefi_raw::protocol::console::KeyShiftState;

use super::cursor;
use crate::app::{Hub, scale_factor_for};


#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct Pixel(BltPixel);

impl TargetPixel for Pixel {
    fn blend(&mut self, color: PremultipliedRgbaColor) {
        let a = (u8::MAX - color.alpha) as u16;
        self.0.red = (self.0.red as u16 * a / 255) as u8 + color.red;
        self.0.green = (self.0.green as u16 * a / 255) as u8 + color.green;
        self.0.blue = (self.0.blue as u16 * a / 255) as u8 + color.blue;
    }

    fn from_rgb(red: u8, green: u8, blue: u8) -> Self {
        Pixel(BltPixel::new(red, green, blue))
    }
}

/// A display mode the user can pick.
#[derive(Clone, Copy, Debug)]
pub struct DisplayMode {
    pub width: usize,
    pub height: usize,
    mode: Mode,
}

/// What a notch of the mouse wheel scrolls, in logical pixels: about three
/// lines, like desktops (positive: toward the top).
const WHEEL_NOTCH: f32 = 60.;

pub(super) fn timer_tick() -> u64 {
    cfg_select! {
        target_arch = "x86_64" => {
            // SAFETY: rdtsc has no side effect.
            unsafe { core::arch::x86_64::_rdtsc() }
        }
        target_arch = "aarch64" => {
            // The generic timer's virtual count (its frequency is measured
            // below like the TSC's, rather than trusting CNTFRQ_EL0).
            let count: u64;
            // SAFETY: Reading the counter has no side effect.
            unsafe { core::arch::asm!("isb", "mrs {}, cntvct_el0", out(reg) count, options(nomem, nostack)) };
            count
        }
    }
}

/// The timer's frequency (`timer_tick` per second), measured the first time.
pub(super) fn timer_freq() -> u64 {
    use core::sync::atomic::{AtomicU64, Ordering};
    static FREQ: AtomicU64 = AtomicU64::new(0);
    let freq = FREQ.load(Ordering::Relaxed);
    if freq != 0 {
        return freq;
    }
    let start = timer_tick();
    boot::stall(Duration::from_millis(10));
    let end = timer_tick();
    let freq = (end - start).max(1) * 100;
    FREQ.store(freq, Ordering::Relaxed);
    freq
}

/// Opens a protocol without taking it away from other users (the console
/// splitter and the firmware keep using the same protocols).
pub(super) fn open_shared<P: uefi::proto::ProtocolPointer + ?Sized>(
    handle: uefi::Handle,
) -> Option<ScopedProtocol<P>> {
    // SAFETY: We only read input, blit to the screen and read files; other
    // users of the protocols are not expected to interfere while the UI runs.
    unsafe {
        boot::open_protocol::<P>(
            OpenProtocolParams { handle, agent: boot::image_handle(), controller: None },
            OpenProtocolAttributes::GetProtocol,
        )
        .ok()
    }
}

/// Finds a protocol, preferring the console-in handle where the console
/// splitter merges every keyboard and pointer.
fn open_input<P: uefi::proto::ProtocolPointer + ?Sized>() -> Option<ScopedProtocol<P>> {
    if let Some(handle) = super::console_in_handle()
        && let Some(p) = open_shared::<P>(handle)
    {
        return Some(p);
    }
    let handle = boot::get_handle_for_protocol::<P>().ok()?;
    open_shared::<P>(handle)
}

pub struct Platform {
    window: Rc<MinimalSoftwareWindow>,
    shared: Rc<Hub>,
    /// The GOP modes behind `shared.modes`.
    modes: RefCell<Vec<DisplayMode>>,
    timer_freq: f64,
    timer_start: u64,
}

impl Platform {
    pub fn new(shared: Rc<Hub>) -> Self {
        Self {
            modes: RefCell::new(Vec::new()),
            window: MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer),
            shared,
            timer_freq: timer_freq() as f64,
            timer_start: timer_tick(),
        }
    }
}

struct Keyboard {
    ex: Option<ScopedProtocol<InputEx>>,
    simple: Option<ScopedProtocol<Input>>,
}

impl Keyboard {
    fn open() -> Self {
        let ex = open_input::<InputEx>();
        let simple = if ex.is_none() { open_input::<Input>() } else { None };
        Keyboard { ex, simple }
    }

    fn event(&self) -> Option<Event> {
        if let Some(ex) = &self.ex {
            ex.wait_for_key_event().ok()
        } else {
            self.simple.as_ref()?.wait_for_key_event().ok()
        }
    }

    /// Returns the next key with its shift state.
    fn read(&mut self) -> Option<(UefiKey, KeyShiftState)> {
        if let Some(ex) = &mut self.ex {
            let data = ex.read_key().ok()??;
            Some((data.key, data.key_state.key_shift_state.unwrap_or(KeyShiftState::empty())))
        } else {
            let key = self.simple.as_mut()?.read_key().ok()??;
            Some((key, KeyShiftState::empty()))
        }
    }
}

fn map_key(key: UefiKey) -> Option<SharedString> {
    let text: SharedString = match key {
        UefiKey::Printable(c) => match char::from(c) {
            '\r' | '\n' => Key::Return.into(),
            '\t' => Key::Tab.into(),
            '\u{8}' => Key::Backspace.into(),
            '\0' => return None,
            c => c.into(),
        },
        UefiKey::Special(scan) => match scan {
            ScanCode::UP => Key::UpArrow.into(),
            ScanCode::DOWN => Key::DownArrow.into(),
            ScanCode::RIGHT => Key::RightArrow.into(),
            ScanCode::LEFT => Key::LeftArrow.into(),
            ScanCode::HOME => Key::Home.into(),
            ScanCode::END => Key::End.into(),
            ScanCode::INSERT => Key::Insert.into(),
            ScanCode::DELETE => Key::Delete.into(),
            ScanCode::PAGE_UP => Key::PageUp.into(),
            ScanCode::PAGE_DOWN => Key::PageDown.into(),
            ScanCode::ESCAPE => Key::Escape.into(),
            ScanCode::FUNCTION_1 => Key::F1.into(),
            ScanCode::FUNCTION_2 => Key::F2.into(),
            ScanCode::FUNCTION_3 => Key::F3.into(),
            ScanCode::FUNCTION_4 => Key::F4.into(),
            ScanCode::FUNCTION_5 => Key::F5.into(),
            ScanCode::FUNCTION_6 => Key::F6.into(),
            ScanCode::FUNCTION_7 => Key::F7.into(),
            ScanCode::FUNCTION_8 => Key::F8.into(),
            ScanCode::FUNCTION_9 => Key::F9.into(),
            ScanCode::FUNCTION_10 => Key::F10.into(),
            ScanCode::FUNCTION_11 => Key::F11.into(),
            ScanCode::FUNCTION_12 => Key::F12.into(),
            _ => return None,
        },
    };
    Some(text)
}

struct Display {
    gop: ScopedProtocol<GraphicsOutput>,
    width: usize,
    height: usize,
    fb: Vec<Pixel>,
}

impl Display {
    fn blit_from_fb(&mut self, x: usize, y: usize, w: usize, h: usize) {
        if w == 0 || h == 0 {
            return;
        }
        // SAFETY: Pixel is a repr(transparent) BltPixel.
        let buffer = unsafe {
            core::slice::from_raw_parts(self.fb.as_ptr() as *const BltPixel, self.fb.len())
        };
        let _ = self.gop.blt(BltOp::BufferToVideo {
            buffer,
            src: BltRegion::SubRectangle { coords: (x, y), px_stride: self.width },
            dest: (x, y),
            dims: (w, h),
        });
    }

    fn set_mode(&mut self, mode: &DisplayMode) -> bool {
        if self.gop.set_mode(&mode.mode).is_err() {
            return false;
        }
        let (w, h) = self.gop.current_mode_info().resolution();
        self.width = w;
        self.height = h;
        self.fb = vec![Pixel(BltPixel::new(0, 0, 0)); w * h];
        true
    }
}

/// The keyboard layout (`Backend::set_keymap`); none: as the firmware sends.
pub struct KeymapCell(pub core::cell::RefCell<Option<crate::parse::keymap::Keymap>>);
// SAFETY: the firmware runs Slingshot on one processor (the other one
// only checks passphrases, and doesn't touch this).
unsafe impl Sync for KeymapCell {}
pub static KEYMAP: KeymapCell = KeymapCell(core::cell::RefCell::new(None));

/// Set after another program ran (and drew on the screen): the event loop
/// restores the display mode and repaints everything.
static FULL_REDRAW: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// The pointer's speed (an `f32`'s bits, 1.0 to start) and acceleration
/// (`Backend::set_pointer`).
pub static POINTER_FACTOR: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0x3f80_0000);
pub static POINTER_ACCELERATION: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(true);

pub fn request_full_redraw() {
    FULL_REDRAW.store(true, core::sync::atomic::Ordering::Relaxed);
}

struct Mouse {
    relative: Option<ScopedProtocol<Pointer>>,
    /// USB mice that no firmware driver handles: read by Slingshot.
    usb: Vec<super::usbmouse::UsbMouse>,
    /// A touchpad on I2C, read by Slingshot (the `touchpad` feature).
    #[cfg(all(target_arch = "x86_64", feature = "touchpad"))]
    touchpad: Option<super::touchpad::Touchpad>,
    absolute: Option<ScopedProtocol<AbsolutePointer>>,
    /// Physical position of the pointer.
    x: f32,
    y: f32,
    left: bool,
    right: bool,
    cursor_visible: bool,
    /// Where the cursor was last drawn (x, y), to restore the background.
    drawn_at: Option<(usize, usize)>,
}

impl slint::platform::Platform for Platform {
    /// `debug()` in the .slint files: to the log.
    fn debug_log(&self, arguments: core::fmt::Arguments) {
        log::info!("slint: {arguments}");
    }
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        Duration::from_secs_f64((timer_tick() - self.timer_start) as f64 / self.timer_freq)
    }

    fn run_event_loop(&self) -> Result<(), slint::PlatformError> {
        let gop_handle = boot::get_handle_for_protocol::<GraphicsOutput>()
            .map_err(|_| slint::PlatformError::Other("No GOP".into()))?;
        let gop = open_shared::<GraphicsOutput>(gop_handle)
            .ok_or_else(|| slint::PlatformError::Other("Cannot open GOP".into()))?;
        let (width, height) = gop.current_mode_info().resolution();
        let mut display = Display { gop, width, height, fb: vec![Pixel(BltPixel::new(0, 0, 0)); width * height] };

        // Collect the usable modes: big enough for the UI, without duplicates.
        {
            let mut modes: Vec<DisplayMode> = Vec::new();
            for mode in display.gop.modes() {
                let (w, h) = mode.info().resolution();
                if w < 1024 || h < 640 || modes.iter().any(|m| m.width == w && m.height == h) {
                    continue;
                }
                modes.push(DisplayMode { width: w, height: h, mode });
            }
            modes.sort_by_key(|m| (m.width * m.height, m.width));
            *self.shared.modes.borrow_mut() = modes.iter().map(|m| (m.width, m.height)).collect();
            *self.modes.borrow_mut() = modes;
        }
        self.shared.mode_changed(width, height);

        #[cfg(not(all(target_arch = "x86_64", feature = "touchpad")))]
        log::info!("touchpad: not in this build (the touchpad feature, x86 only)");
        let mut warmed_up = false;
        let mut first_frame_logged = false;
        // The drivers and the devices: connected after the first frame.
        let mut devices_connected = false;
        // Connecting them, a slice per turn of the loop, and since when.
        let mut connecting: Option<(super::Connecting, Duration)> = None;
        let mut keyboard = Keyboard::open();
        // A dead key waiting for the next one.
        let mut dead_key: Option<char> = None;
        let mut mouse = Mouse {
            relative: open_input::<Pointer>(),
            // Found once the devices are connected, after the first frame.
            usb: Vec::new(),
            #[cfg(all(target_arch = "x86_64", feature = "touchpad"))]
            touchpad: None,
            absolute: open_input::<AbsolutePointer>(),
            x: width as f32 / 2.,
            y: height as f32 / 2.,
            left: false,
            right: false,
            cursor_visible: false,
            drawn_at: None,
        };
        if let Some(p) = &mut mouse.relative {
            let _ = p.reset(false);
        }
        if let Some(p) = &mut mouse.absolute {
            let _ = p.reset(false);
        }
        let simple_handles = boot::find_handles::<Pointer>().map(|h| h.len()).unwrap_or(0);
        let absolute_handles = boot::find_handles::<AbsolutePointer>().map(|h| h.len()).unwrap_or(0);
        log::info!(
            "pointer handles: simple={simple_handles} absolute={absolute_handles}, USB mice read by Slingshot: {}",
            mouse.usb.len()
        );
        if let Some(p) = &mouse.relative {
            // A Z resolution of 0: the firmware's driver has no wheel.
            let m = p.mode();
            log::info!("firmware pointer: resolution {}/{}/{} (x/y/z)", m.resolution_x, m.resolution_y, m.resolution_z);
        }
        let set_pointer_status = |status: alloc::string::String| self.shared.set_pointer_status(status);
        // The console splitter always provides both protocols; more handles
        // means the firmware has an actual mouse or touch driver.
        set_pointer_status(alloc::format!(
            "Pointer: {} mouse, {} touch protocol handles · no movement yet",
            simple_handles,
            absolute_handles
        ));
        let mut seen_relative = false;
        // When the last relative move came.
        let mut last_move = Duration::ZERO;
        // The last turn had one: the pointer is moving.
        let mut moving = false;
        let mut seen_absolute = false;
        let mut wheel_seen = false;
        // SAFETY: No callback. Never closed: the event loop doesn't end.
        let timer = unsafe { boot::create_event(EventType::TIMER, Tpl::APPLICATION, None, None) }.ok();
        log::info!(
            "keyboard: ex={} simple={}, pointer: relative={} absolute={}",
            keyboard.ex.is_some(),
            keyboard.simple.is_some(),
            mouse.relative.is_some(),
            mouse.absolute.is_some()
        );

        let window = &self.window;
        let mut scale = scale_factor_for(width, height);
        window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
        window.set_size(PhysicalSize::new(width as u32, height as u32));

        loop {
            let frame_start = slint::platform::Platform::duration_since_start(self);
            // A resolution change requested by the UI.
            if let Some((w, h)) = self.shared.requested_mode.take() {
                let mode = self.modes.borrow().iter().find(|m| m.width == w && m.height == h).copied();
                if let Some(mode) = mode
                    && display.set_mode(&mode)
                {
                    scale = scale_factor_for(display.width, display.height);
                    // Scale factor first: set_size() converts to a logical
                    // size with the current scale factor.
                    window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
                    window.set_size(PhysicalSize::new(display.width as u32, display.height as u32));
                    window.request_redraw();
                    mouse.x = mouse.x.min(display.width as f32 - 1.);
                    mouse.y = mouse.y.min(display.height as f32 - 1.);
                    mouse.drawn_at = None;
                    self.shared.mode_changed(display.width, display.height);
                }
            }

            if FULL_REDRAW.swap(false, core::sync::atomic::Ordering::Relaxed) {
                // The program may have changed the mode (or switched to text).
                let (w, h) = display.gop.current_mode_info().resolution();
                if (w, h) != (display.width, display.height) {
                    let mode = self.modes.borrow().iter().find(|m| m.width == display.width && m.height == display.height).copied();
                    if let Some(mode) = mode {
                        let _ = display.gop.set_mode(&mode.mode);
                    }
                }
                display.blit_from_fb(0, 0, display.width, display.height);
                mouse.drawn_at = None;
                window.request_redraw();
            }

            slint::platform::update_timers_and_animations();

            let mut had_input = false;
            let mut cursor_moved = false;

            // Keyboard. EFI only reports key presses, so send press + release.
            while let Some((key, shift_state)) = keyboard.read() {
                had_input = true;
                if mouse.cursor_visible {
                    mouse.cursor_visible = false;
                    cursor_moved = true;
                }
                // The firmware translates keys as on a US keyboard: the
                // layout's own characters, AltGr (right Alt) and dead keys
                // included.
                let mut shift_state = shift_state;
                let typed = match key {
                    UefiKey::Printable(c) if char::from(c) >= ' ' => {
                        let altgr = shift_state.contains(KeyShiftState::RIGHT_ALT);
                        let typed = KEYMAP.0.borrow().as_ref().and_then(|k| k.type_key(&mut dead_key, char::from(c), altgr));
                        if typed.is_some() && altgr {
                            shift_state.remove(KeyShiftState::RIGHT_ALT);
                        }
                        typed
                    }
                    _ => None,
                };
                let texts: Vec<SharedString> = match typed {
                    Some(chars) => chars.into_iter().map(SharedString::from).collect(),
                    None => map_key(key).into_iter().collect(),
                };
                let modifiers: Vec<SharedString> = [
                    (KeyShiftState::LEFT_ALT | KeyShiftState::RIGHT_ALT, Key::Alt),
                    (KeyShiftState::LEFT_SHIFT | KeyShiftState::RIGHT_SHIFT, Key::Shift),
                    (KeyShiftState::LEFT_CONTROL | KeyShiftState::RIGHT_CONTROL, Key::Control),
                ]
                .into_iter()
                .filter(|(mask, _)| shift_state.intersects(*mask))
                .map(|(_, k)| SharedString::from(k))
                .collect();
                for text in texts {
                    for m in &modifiers {
                        window.dispatch_event(WindowEvent::KeyPressed { text: m.clone() });
                    }
                    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
                    window.dispatch_event(WindowEvent::KeyReleased { text });
                    for m in modifiers.iter().rev() {
                        window.dispatch_event(WindowEvent::KeyReleased { text: m.clone() });
                    }
                }
            }

            // Relative pointer (mouse, touchpad).
            let mut pointer_events = Vec::new();
            if let Some(p) = &mut mouse.relative {
                let mode = *p.mode();
                while let Ok(Some(state)) = p.read_state() {
                    log::debug!("relative pointer: {state:?}");
                    let rx = mode.resolution_x.max(1) as f32;
                    let ry = mode.resolution_y.max(1) as f32;
                    // The resolution is in counts per mm; aim for ~8 px per mm.
                    // EDK II's USB mouse driver always says 8, so that's one
                    // pixel per count, like QEMU sends them.
                    let dx = state.relative_movement_x as f32 / rx * 8.;
                    let dy = state.relative_movement_y as f32 / ry * 8.;
                    // The wheel, when the firmware's driver reads one (EDK
                    // II's does): Z, in notches, as the USB report has it
                    // (away from the user: positive).
                    pointer_events.push((
                        Some((dx, dy)),
                        None,
                        state.relative_movement_z,
                        bool::from(state.left_button),
                        bool::from(state.right_button),
                    ));
                }
            }
            for m in &mut mouse.usb {
                while let Some(r) = m.poll() {
                    // Like EDK II's driver: one pixel per count.
                    pointer_events.push((Some((r.dx as f32, r.dy as f32)), None, r.wheel, r.left, r.right));
                }
            }
            #[cfg(all(target_arch = "x86_64", feature = "touchpad"))]
            if let Some(t) = &mut mouse.touchpad {
                // Every report waiting: it sends one each ~10 ms and a frame
                // can take more, so reports left behind would make the
                // pointer and the clicks lag. Bounded, for a pad that repeats.
                for _ in 0..16 {
                    let Some(r) = t.poll() else { break };
                    pointer_events.push((Some((r.dx as f32, r.dy as f32)), None, 0, r.left, r.right));
                }
            }
            if let Some(p) = &mut mouse.absolute {
                let mode = *p.mode();
                while let Ok(Some(state)) = p.read_state() {
                    let sx = (mode.absolute_max_x.saturating_sub(mode.absolute_min_x)).max(1) as f32;
                    let sy = (mode.absolute_max_y.saturating_sub(mode.absolute_min_y)).max(1) as f32;
                    let x = (state.current_x.saturating_sub(mode.absolute_min_x)) as f32 / sx;
                    let y = (state.current_y.saturating_sub(mode.absolute_min_y)) as f32 / sy;
                    let pressed = state.active_buttons & 1 != 0;
                    log::debug!("absolute pointer: {state:?} mode: {mode:?}");
                    // Some firmware exposes the touchpad as an absolute
                    // pointer, so the cursor shows for those too.
                    pointer_events.push((None, Some((x, y)), 0, pressed, false));
                }
            }
            // The devices give moves in pixels at scale 1: in the screen's
            // scale (a screen scaled ×2 has twice the pixels to cross), times
            // the speed chosen in Settings, and with acceleration, up to 3
            // times more for fast moves. Their speed: since the last move,
            // or over a report's ~10 ms when starting (a flick's first report
            // counts).
            let distance: f32 = pointer_events.iter().filter_map(|e| e.0).map(|(dx, dy)| libm::hypotf(dx, dy)).sum();
            let mut gain = scale;
            if distance > 0. {
                use core::sync::atomic::Ordering::Relaxed;
                let now = slint::platform::Platform::duration_since_start(self);
                let since = now - last_move;
                let ms = if moving || since < Duration::from_millis(50) { (since.as_secs_f32() * 1000.).max(1.) } else { 10. };
                last_move = now;
                gain *= f32::from_bits(POINTER_FACTOR.load(Relaxed));
                if POINTER_ACCELERATION.load(Relaxed) {
                    // Under 0.4 px per ms (about 5 cm/s), as it comes.
                    gain *= (1. + (distance / ms - 0.4) * 0.5).clamp(1., 3.);
                }
            }
            moving = distance > 0.;
            for (delta, abs, wheel, left, right) in pointer_events {
                had_input = true;
                if (delta.is_some() && !seen_relative) || (abs.is_some() && !seen_absolute) {
                    seen_relative |= delta.is_some();
                    seen_absolute |= abs.is_some();
                    set_pointer_status(alloc::format!(
                        "Pointer: {}{}{} working",
                        if seen_relative { "mouse" } else { "" },
                        if seen_relative && seen_absolute { " and " } else { "" },
                        if seen_absolute { "absolute pointer" } else { "" },
                    ));
                }
                let (old_x, old_y) = (mouse.x, mouse.y);
                if let Some((dx, dy)) = delta {
                    mouse.x = (mouse.x + dx * gain).clamp(0., display.width as f32 - 1.);
                    mouse.y = (mouse.y + dy * gain).clamp(0., display.height as f32 - 1.);
                }
                if let Some((x, y)) = abs {
                    mouse.x = (x * display.width as f32).clamp(0., display.width as f32 - 1.);
                    mouse.y = (y * display.height as f32).clamp(0., display.height as f32 - 1.);
                }
                if !mouse.cursor_visible || old_x != mouse.x || old_y != mouse.y {
                    cursor_moved = true;
                }
                mouse.cursor_visible = true;
                let position = LogicalPosition::new(mouse.x / scale, mouse.y / scale);
                window.dispatch_event(WindowEvent::PointerMoved { position });
                if wheel != 0 {
                    if !wheel_seen {
                        wheel_seen = true;
                        log::info!("mouse wheel working");
                    }
                    // A notch: about three lines, like desktops scroll.
                    let delta_y = (wheel as f32 * WHEEL_NOTCH).clamp(-10. * WHEEL_NOTCH, 10. * WHEEL_NOTCH);
                    window.dispatch_event(WindowEvent::PointerScrolled { position, delta_x: 0., delta_y });
                }
                for (pressed, state, button) in [
                    (left, &mut mouse.left, PointerEventButton::Left),
                    (right, &mut mouse.right, PointerEventButton::Right),
                ] {
                    if pressed != *state {
                        *state = pressed;
                        window.dispatch_event(if pressed {
                            WindowEvent::PointerPressed { position, button }
                        } else {
                            WindowEvent::PointerReleased { position, button }
                        });
                    }
                }
            }

            if had_input {
                self.shared.input();
            }

            let mut redrawn = false;
            window.draw_if_needed(|renderer| {
                let render_start = slint::platform::Platform::duration_since_start(self);
                let region = renderer.render(&mut display.fb, display.width);
                // A redraw of (nearly) the whole screen, after a mode change
                // or during a screen transition, says whether full-screen
                // animations can keep up (the thresholds are below). The
                // first one is left out: it decodes the images and glyphs
                // too.
                let covered: usize = region.iter().map(|(_, s)| s.width as usize * s.height as usize).sum();
                if !first_frame_logged {
                    first_frame_logged = true;
                    log::info!("display: first frame drawn");
                }
                if covered * 10 >= display.width * display.height * 9 && core::mem::replace(&mut warmed_up, true) {
                    self.shared.full_redraw(slint::platform::Platform::duration_since_start(self) - render_start);
                }
                for (origin, size) in region.iter() {
                    display.blit_from_fb(
                        origin.x as usize,
                        origin.y as usize,
                        size.width as usize,
                        size.height as usize,
                    );
                }
                redrawn = true;
            });

            if redrawn || cursor_moved {
                let cursor_scale = if scale >= 1.75 { 2 } else { 1 };
                let (cw, ch) = cursor::size(cursor_scale);
                // Restore what was under the old cursor.
                if let Some((x, y)) = mouse.drawn_at.take() {
                    let w = cw.min(display.width - x);
                    let h = ch.min(display.height - y);
                    display.blit_from_fb(x, y, w, h);
                }
                if mouse.cursor_visible {
                    let (x, y) = (mouse.x as usize, mouse.y as usize);
                    let w = cw.min(display.width - x);
                    let h = ch.min(display.height - y);
                    let mut buf: Vec<BltPixel> = Vec::with_capacity(w * h);
                    for row in 0..h {
                        for col in 0..w {
                            let px = display.fb[(y + row) * display.width + x + col].0;
                            buf.push(cursor::blend(px, col, row, cursor_scale));
                        }
                    }
                    let _ = display.gop.blt(BltOp::BufferToVideo {
                        buffer: &buf,
                        src: BltRegion::Full,
                        dest: (x, y),
                        dims: (w, h),
                    });
                    mouse.drawn_at = Some((x, y));
                }
            }

            // The first frame is on screen: now the drivers and the other
            // devices (a "connect all" takes seconds on some machines), then
            // the mice and the touchpad they make available. The app rescans
            // the disks.
            // A slice of about 10 ms per turn, then drawing and input.
            if first_frame_logged && !devices_connected && connecting.is_none() {
                connecting = Some((super::Connecting::start(), slint::platform::Platform::duration_since_start(self)));
            }
            let slice_start = slint::platform::Platform::duration_since_start(self);
            let connected = connecting.as_mut().is_some_and(|(c, _)| {
                c.step(|| slint::platform::Platform::duration_since_start(self) - slice_start >= Duration::from_millis(10))
            });
            if connected && let Some((c, start)) = connecting.take() {
                devices_connected = true;
                let drivers = c.drivers;
                mouse.usb = super::usbmouse::find();
                #[cfg(all(target_arch = "x86_64", feature = "touchpad"))]
                {
                    mouse.touchpad = super::touchpad::find();
                }
                let simple = boot::find_handles::<Pointer>().map(|h| h.len()).unwrap_or(0);
                let absolute = boot::find_handles::<AbsolutePointer>().map(|h| h.len()).unwrap_or(0);
                log::info!(
                    "devices connected in {} ms: pointer handles simple={simple} absolute={absolute}, USB mice read by Slingshot: {}",
                    (slint::platform::Platform::duration_since_start(self) - start).as_millis(),
                    mouse.usb.len()
                );
                self.shared.set_pointer_status(alloc::format!(
                    "Pointer: {simple} mouse, {absolute} touch protocol handles · no movement yet"
                ));
                self.shared.devices_connected(drivers);
            }

            let mut timeout = slint::platform::duration_until_next_timer_update();
            if window.has_active_animations() {
                // About 60 frames per second; input still wakes us up.
                let elapsed = slint::platform::Platform::duration_since_start(self) - frame_start;
                let frame = Duration::from_millis(16).saturating_sub(elapsed);
                timeout = Some(timeout.map_or(frame, |t| t.min(frame)));
            }
            // USB mice and the touchpad are polled: no event to wait on.
            #[cfg(all(target_arch = "x86_64", feature = "touchpad"))]
            let polled = !mouse.usb.is_empty() || mouse.touchpad.is_some();
            #[cfg(not(all(target_arch = "x86_64", feature = "touchpad")))]
            let polled = !mouse.usb.is_empty();
            if polled {
                timeout = Some(timeout.map_or(Duration::from_millis(8), |t| t.min(Duration::from_millis(8))));
            }
            // Still connecting: the next slice right after this turn.
            if connecting.is_some() {
                timeout = Some(Duration::ZERO);
            }
            wait_for_input(&keyboard, &mouse, timer.as_ref(), timeout);
        }
    }
}

/// The event loop's timeout: one timer event, set again for each wait.
fn wait_for_input(keyboard: &Keyboard, mouse: &Mouse, timer: Option<&Event>, timeout: Option<Duration>) {
    // Wake up at least every few seconds; the clock needs it anyway.
    let timeout = timeout.unwrap_or(Duration::from_secs(5)).min(Duration::from_secs(5));
    if timeout.is_zero() {
        return;
    }
    // Setting it again cancels the previous wait's.
    let Some(timer) = timer else { return };
    if boot::set_timer(timer, TimerTrigger::Relative(timeout)).is_err() {
        return;
    }
    // SAFETY: The cloned handle is only used for this wait.
    let mut events: Vec<Event> = vec![unsafe { timer.unsafe_clone() }];
    events.extend(keyboard.event());
    if let Some(p) = &mouse.relative {
        events.extend(p.wait_for_input_event().ok());
    }
    if let Some(p) = &mouse.absolute {
        events.extend(p.wait_for_input_event().ok());
    }
    let _ = boot::wait_for_event(&events);
}
