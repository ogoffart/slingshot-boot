// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! A Slint platform for UEFI applications: a `MinimalSoftwareWindow` drawn
//! into a buffer of GOP pixels, with the keyboard and the pointers (mice,
//! touchpads, touch screens) read from the firmware's console protocols and
//! a cursor drawn on top. The event loop sleeps in `WaitForEvent` until
//! input or the next Slint timer.
//!
//! Self-contained: it depends on `slint`, `uefi`, `uefi_raw`, `log` and
//! `libm` only, so that any application can use it. What an application adds
//! (keyboard layouts, pointers the firmware has no driver for, the display
//! modes it accepts…) goes through [`Hooks`]; work between frames is
//! Slint's timers.
//!
//! ```ignore
//! slint::platform::set_platform(Box::new(UefiPlatform::new(MyHooks))).unwrap();
//! MainWindow::new().unwrap().run().unwrap();
//! ```

mod cursor;
mod display;
mod input;
mod timer;

use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::time::Duration;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{LogicalPosition, PhysicalSize, SharedString};
use uefi::Event;
use uefi::boot::{self, EventType, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol, TimerTrigger, Tpl};
use uefi::proto::console::text::Key as UefiKey;
use uefi_raw::protocol::console::KeyShiftState;

use display::{Cursor, Display, DisplayMode};
use input::{Keyboard, Pointers};
pub use input::PointerReport;
pub use timer::{timer_freq, timer_tick};

/// What a notch of the mouse wheel scrolls, in logical pixels: about three
/// lines, like desktops (positive: toward the top).
const WHEEL_NOTCH: f32 = 60.;

/// Opens a protocol without taking it away from other users (the console
/// splitter and the firmware keep using the same protocols).
pub fn open_shared<P: uefi::proto::ProtocolPointer + ?Sized>(handle: uefi::Handle) -> Option<ScopedProtocol<P>> {
    // SAFETY: The protocols opened this way are only read or drawn with;
    // other users are not expected to interfere while the UI runs.
    unsafe {
        boot::open_protocol::<P>(
            OpenProtocolParams { handle, agent: boot::image_handle(), controller: None },
            OpenProtocolAttributes::GetProtocol,
        )
        .ok()
    }
}

/// What the application adds to the platform. Each has a default that does
/// nothing.
pub trait Hooks {
    /// The display modes, smallest first, at startup. The application
    /// switches to one by giving the window its size (`Window::set_size`,
    /// with a `PhysicalSize`), also before the event loop runs.
    fn modes_found(&self, _modes: &[(usize, usize)]) {}
    /// The screen's size, at startup and after each change, just before
    /// `window` gets it. The application may set the scale factor for it then
    /// (`WindowEvent::ScaleFactorChanged`), or at any time: the platform
    /// follows the window's.
    fn mode_changed(&self, _window: &slint::Window, _width: usize, _height: usize) {}
    /// The characters a printable key types, when not the US layout ones the
    /// firmware sends: a keyboard layout's, with AltGr (right Alt) and dead
    /// keys.
    fn type_key(&self, _c: char, _altgr: bool) -> Option<Vec<char>> {
        None
    }
    /// Pointers the firmware has no driver for, polled each turn: their
    /// reports pushed to `reports`. Whether there are any (the loop then
    /// wakes up every 8 ms to poll them).
    fn poll_pointers(&self, _reports: &mut Vec<PointerReport>) -> bool {
        false
    }
    /// How much further than the firmware's counts a relative move goes, at
    /// `speed` pixels per millisecond: the pointer's speed and acceleration.
    fn pointer_gain(&self, _speed: f32) -> f32 {
        1.
    }
    /// A key was pressed, the pointer moved or clicked.
    fn input(&self) {}
    /// Pointers of these kinds sent something, called each time one more
    /// kind does.
    fn pointer_working(&self, _relative: bool, _absolute: bool) {}
    /// A frame was drawn: the part of the screen it covered (0 to 1), how
    /// long it took, and when it was done (Slint's time).
    fn frame_drawn(&self, _covered: f32, _took: Duration, _at: Duration) {}
    /// The first frame is on the screen: slower work can start (in Slint
    /// timers, between frames and input).
    fn first_frame(&self) {}
}

pub struct UefiPlatform<H> {
    window: Rc<MinimalSoftwareWindow>,
    hooks: H,
    timer_freq: f64,
    timer_start: u64,
}

impl<H: Hooks> UefiPlatform<H> {
    pub fn new(hooks: H) -> Self {
        Self {
            window: MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer),
            hooks,
            timer_freq: timer_freq() as f64,
            timer_start: timer_tick(),
        }
    }

    fn now(&self) -> Duration {
        slint::platform::Platform::duration_since_start(self)
    }

    /// Tells the application the screen's size, then gives it to the window,
    /// at the scale factor the application may have set.
    fn resized(&self, scale: &mut f32, display: &Display) {
        self.hooks.mode_changed(&self.window, display.width, display.height);
        *scale = self.window.scale_factor();
        self.window.set_size(PhysicalSize::new(display.width as u32, display.height as u32));
    }

    /// Switches to the mode of the size the application gave the window
    /// (`Window::set_size`), if one has it. Whether the window's size wasn't
    /// the screen's: it's to be given the screen's (`resized`).
    fn switch_to_window_size(&self, display: &mut Display, modes: &[DisplayMode]) -> bool {
        let size = self.window.size();
        let asked = (size.width as usize, size.height as usize);
        if asked == (display.width, display.height) {
            return false;
        }
        if let Some(mode) = modes.iter().find(|m| (m.width, m.height) == asked) {
            display.set_mode(mode);
        }
        true
    }

    /// Makes the logical size again when the application changed the scale
    /// factor.
    fn follow_scale(&self, scale: &mut f32, display: &Display) {
        let window_scale = self.window.scale_factor();
        if window_scale != *scale {
            *scale = window_scale;
            self.window.set_size(PhysicalSize::new(display.width as u32, display.height as u32));
        }
    }

    /// Sends a key to the window, as a press and a release (the firmware
    /// only reports presses), with the modifiers held.
    fn dispatch_key(&self, key: UefiKey, mut shift_state: KeyShiftState) {
        let typed = match key {
            UefiKey::Printable(c) if char::from(c) >= ' ' => {
                let altgr = shift_state.contains(KeyShiftState::RIGHT_ALT);
                let typed = self.hooks.type_key(char::from(c), altgr);
                if typed.is_some() && altgr {
                    shift_state.remove(KeyShiftState::RIGHT_ALT);
                }
                typed
            }
            _ => None,
        };
        let texts: Vec<SharedString> = match typed {
            Some(chars) => chars.into_iter().map(SharedString::from).collect(),
            None => input::map_key(key).into_iter().collect(),
        };
        let modifiers = input::modifiers(shift_state);
        for text in texts {
            for m in &modifiers {
                self.window.dispatch_event(WindowEvent::KeyPressed { text: m.clone() });
            }
            self.window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
            self.window.dispatch_event(WindowEvent::KeyReleased { text });
            for m in modifiers.iter().rev() {
                self.window.dispatch_event(WindowEvent::KeyReleased { text: m.clone() });
            }
        }
    }
}

/// The pointer's state across turns of the event loop.
struct PointerState {
    cursor: Cursor,
    left: bool,
    right: bool,
    seen_relative: bool,
    seen_absolute: bool,
    wheel_seen: bool,
    /// When the last relative move came, and whether the last turn had one.
    last_move: Duration,
    moving: bool,
}

impl<H: Hooks> slint::platform::Platform for UefiPlatform<H> {
    /// `debug()` in the .slint files: to the log.
    fn debug_log(&self, arguments: core::fmt::Arguments) {
        log::info!("slint: {arguments}");
    }

    fn create_window_adapter(&self) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        Duration::from_secs_f64((timer_tick() - self.timer_start) as f64 / self.timer_freq)
    }

    fn run_event_loop(&self) -> Result<(), slint::PlatformError> {
        let mut display = Display::open()?;
        let modes = display.modes();
        self.hooks.modes_found(&modes.iter().map(|m| (m.width, m.height)).collect::<Vec<_>>());
        // A resolution the application asked for before the loop ran.
        self.switch_to_window_size(&mut display, &modes);

        let mut keyboard = Keyboard::open();
        let mut pointers = Pointers::open();
        let mut pointer = PointerState {
            cursor: Cursor {
                x: display.width as f32 / 2.,
                y: display.height as f32 / 2.,
                visible: false,
                drawn_at: None,
            },
            left: false,
            right: false,
            seen_relative: false,
            seen_absolute: false,
            wheel_seen: false,
            last_move: Duration::ZERO,
            moving: false,
        };
        let mut first_frame = false;
        // SAFETY: No callback. Never closed: the event loop doesn't end.
        let mut timer = unsafe { boot::create_event(EventType::TIMER, Tpl::APPLICATION, None, None) }.ok();

        let window = &self.window;
        let mut scale = 1.;
        self.resized(&mut scale, &display);

        loop {
            let frame_start = self.now();
            // From here, the callbacks: another program may run in one.
            let callbacks_start = self.now();
            let console = display::console_cursor();

            slint::platform::update_timers_and_animations();
            self.follow_scale(&mut scale, &display);

            let mut had_input = false;
            let mut cursor_moved = false;

            while let Some((key, shift_state)) = keyboard.read() {
                had_input = true;
                if pointer.cursor.visible {
                    pointer.cursor.visible = false;
                    cursor_moved = true;
                }
                self.dispatch_key(key, shift_state);
            }

            let mut reports = Vec::new();
            pointers.read(&mut reports);
            let polled = self.hooks.poll_pointers(&mut reports);
            had_input |= !reports.is_empty();
            cursor_moved |= self.dispatch_pointer(&mut pointer, &reports, &display, scale);

            if had_input {
                self.hooks.input();
            }

            // A callback this turn may have started another program, which
            // drew on the screen: it changed the mode, printed (the text
            // cursor moved), or ran a while (Slint's callbacks take a few
            // ms). Its mode and picture are put back.
            if display.mode_lost()
                || display::console_cursor() != console
                || self.now() - callbacks_start > Duration::from_millis(200)
            {
                display.restore(&modes);
                pointer.cursor.drawn_at = None;
                cursor_moved = true;
            }

            // A size the application gave the window: that mode, or the
            // screen's size back, and the application told either way.
            if self.switch_to_window_size(&mut display, &modes) {
                pointer.cursor.fit(&display);
                window.request_redraw();
                self.resized(&mut scale, &display);
            }

            self.follow_scale(&mut scale, &display);
            let mut redrawn = false;
            window.draw_if_needed(|renderer| {
                let render_start = self.now();
                let region = renderer.render(&mut display.fb, display.width);
                let done = self.now();
                let covered: usize = region.iter().map(|(_, s)| s.width as usize * s.height as usize).sum();
                self.hooks.frame_drawn(covered as f32 / (display.width * display.height) as f32, done - render_start, done);
                for (origin, size) in region.iter() {
                    display.blit_from_fb(origin.x as usize, origin.y as usize, size.width as usize, size.height as usize);
                }
                redrawn = true;
            });
            if redrawn || cursor_moved {
                display.draw_cursor(&mut pointer.cursor, scale);
            }
            if redrawn && !core::mem::replace(&mut first_frame, true) {
                log::info!("display: first frame drawn");
                self.hooks.first_frame();
            }

            let mut timeout = slint::platform::duration_until_next_timer_update();
            if window.has_active_animations() {
                // About 60 frames per second; input still wakes us up.
                let frame = Duration::from_millis(16).saturating_sub(self.now() - frame_start);
                timeout = Some(timeout.map_or(frame, |t| t.min(frame)));
            }
            // Polled pointers have no event to wait on.
            if polled {
                timeout = Some(timeout.map_or(Duration::from_millis(8), |t| t.min(Duration::from_millis(8))));
            }
            wait_for_input(&keyboard, &pointers, &mut timer, timeout);
        }
    }
}

impl<H: Hooks> UefiPlatform<H> {
    /// Moves the cursor and sends the pointer events for `reports`: whether
    /// the cursor moved.
    fn dispatch_pointer(&self, pointer: &mut PointerState, reports: &[PointerReport], display: &Display, scale: f32) -> bool {
        // The devices give moves in pixels at scale 1: in the screen's scale
        // (a screen scaled ×2 has twice the pixels to cross), times the
        // gain. Their speed: since the last move, or over a report's ~10 ms
        // when starting (a flick's first report counts).
        let distance: f32 = reports.iter().filter_map(|r| r.delta).map(|(dx, dy)| libm::hypotf(dx, dy)).sum();
        let mut gain = scale;
        if distance > 0. {
            let now = self.now();
            let since = now - pointer.last_move;
            let ms = if pointer.moving || since < Duration::from_millis(50) { (since.as_secs_f32() * 1000.).max(1.) } else { 10. };
            pointer.last_move = now;
            gain *= self.hooks.pointer_gain(distance / ms);
        }
        pointer.moving = distance > 0.;
        let mut moved = false;
        let (width, height) = (display.width as f32, display.height as f32);
        for r in reports {
            if (r.delta.is_some() && !pointer.seen_relative) || (r.position.is_some() && !pointer.seen_absolute) {
                pointer.seen_relative |= r.delta.is_some();
                pointer.seen_absolute |= r.position.is_some();
                self.hooks.pointer_working(pointer.seen_relative, pointer.seen_absolute);
            }
            let cursor = &mut pointer.cursor;
            let (old_x, old_y) = (cursor.x, cursor.y);
            if let Some((dx, dy)) = r.delta {
                cursor.x = (cursor.x + dx * gain).clamp(0., width - 1.);
                cursor.y = (cursor.y + dy * gain).clamp(0., height - 1.);
            }
            if let Some((x, y)) = r.position {
                cursor.x = (x * width).clamp(0., width - 1.);
                cursor.y = (y * height).clamp(0., height - 1.);
            }
            if !cursor.visible || old_x != cursor.x || old_y != cursor.y {
                moved = true;
            }
            cursor.visible = true;
            let position = LogicalPosition::new(cursor.x / scale, cursor.y / scale);
            self.window.dispatch_event(WindowEvent::PointerMoved { position });
            if r.wheel != 0 {
                if !pointer.wheel_seen {
                    pointer.wheel_seen = true;
                    log::info!("mouse wheel working");
                }
                let delta_y = (r.wheel as f32 * WHEEL_NOTCH).clamp(-10. * WHEEL_NOTCH, 10. * WHEEL_NOTCH);
                self.window.dispatch_event(WindowEvent::PointerScrolled { position, delta_x: 0., delta_y });
            }
            for (pressed, state, button) in [
                (r.left, &mut pointer.left, PointerEventButton::Left),
                (r.right, &mut pointer.right, PointerEventButton::Right),
            ] {
                if pressed != *state {
                    *state = pressed;
                    self.window.dispatch_event(if pressed {
                        WindowEvent::PointerPressed { position, button }
                    } else {
                        WindowEvent::PointerReleased { position, button }
                    });
                }
            }
        }
        moved
    }
}

/// Waits for input, or `timeout` (at most a few seconds: the timer is set
/// again for each wait).
fn wait_for_input(keyboard: &Keyboard, pointers: &Pointers, timer: &mut Option<Event>, timeout: Option<Duration>) {
    let timeout = timeout.unwrap_or(Duration::from_secs(5)).min(Duration::from_secs(5));
    if timeout.is_zero() {
        return;
    }
    // Setting it again cancels the previous wait's.
    let Some(event) = timer.take_if(|t| boot::set_timer(t, TimerTrigger::Relative(timeout)).is_ok()) else { return };
    // Lent to the wait, the timer first.
    let mut events = vec![event];
    input::input_events(keyboard, pointers, &mut events);
    let _ = boot::wait_for_event(&events);
    *timer = Some(events.swap_remove(0));
}
