// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! The keyboard and the pointers, from the firmware's console protocols.

use alloc::vec::Vec;

use slint::SharedString;
use slint::platform::Key;
use uefi::Event;
use uefi::boot::{self, ScopedProtocol};
use uefi::proto::console::pointer::{AbsolutePointer, Pointer};
use uefi::proto::console::text::{Input, InputEx, Key as UefiKey, ScanCode};
use uefi_raw::protocol::console::KeyShiftState;

use super::open_shared;

/// What a pointer reported: a move (in pixels at scale 1) or a position (0
/// to 1 across the screen), the wheel (in notches, away from the user:
/// positive), and the buttons.
#[derive(Clone, Copy, Debug, Default)]
pub struct PointerReport {
    pub delta: Option<(f32, f32)>,
    pub position: Option<(f32, f32)>,
    pub wheel: i32,
    pub left: bool,
    pub right: bool,
}

/// The handle where the console splitter publishes the merged keyboard and
/// pointer protocols.
fn console_in_handle() -> Option<uefi::Handle> {
    let st = uefi::table::system_table_raw()?;
    // SAFETY: The system table is valid while boot services run.
    let handle = unsafe { st.as_ref().stdin_handle };
    // SAFETY: The handle comes from the system table.
    unsafe { uefi::Handle::from_ptr(handle) }
}

/// Finds a protocol, preferring the console-in handle where the console
/// splitter merges every keyboard and pointer.
fn open_input<P: uefi::proto::ProtocolPointer + ?Sized>() -> Option<ScopedProtocol<P>> {
    if let Some(handle) = console_in_handle()
        && let Some(p) = open_shared::<P>(handle)
    {
        return Some(p);
    }
    let handle = boot::get_handle_for_protocol::<P>().ok()?;
    open_shared::<P>(handle)
}

pub struct Keyboard {
    ex: Option<ScopedProtocol<InputEx>>,
    simple: Option<ScopedProtocol<Input>>,
}

impl Keyboard {
    pub fn open() -> Self {
        let ex = open_input::<InputEx>();
        let simple = if ex.is_none() { open_input::<Input>() } else { None };
        log::info!("keyboard: ex={} simple={}", ex.is_some(), simple.is_some());
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
    pub fn read(&mut self) -> Option<(UefiKey, KeyShiftState)> {
        if let Some(ex) = &mut self.ex {
            let data = ex.read_key().ok()??;
            Some((data.key, data.key_state.key_shift_state.unwrap_or(KeyShiftState::empty())))
        } else {
            let key = self.simple.as_mut()?.read_key().ok()??;
            Some((key, KeyShiftState::empty()))
        }
    }
}

/// The key Slint knows for a firmware key, as text.
pub fn map_key(key: UefiKey) -> Option<SharedString> {
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

/// The modifier keys held in `shift_state`, as Slint's keys.
pub fn modifiers(shift_state: KeyShiftState) -> Vec<SharedString> {
    [
        (KeyShiftState::LEFT_ALT | KeyShiftState::RIGHT_ALT, Key::Alt),
        (KeyShiftState::LEFT_SHIFT | KeyShiftState::RIGHT_SHIFT, Key::Shift),
        (KeyShiftState::LEFT_CONTROL | KeyShiftState::RIGHT_CONTROL, Key::Control),
    ]
    .into_iter()
    .filter(|(mask, _)| shift_state.intersects(*mask))
    .map(|(_, k)| SharedString::from(k))
    .collect()
}

/// The firmware's pointer protocols: mice and touchpads (relative), touch
/// screens and tablets (absolute).
pub struct Pointers {
    relative: Option<ScopedProtocol<Pointer>>,
    absolute: Option<ScopedProtocol<AbsolutePointer>>,
}

impl Pointers {
    pub fn open() -> Self {
        let mut pointers = Pointers { relative: open_input::<Pointer>(), absolute: open_input::<AbsolutePointer>() };
        if let Some(p) = &mut pointers.relative {
            let _ = p.reset(false);
            // A Z resolution of 0: the firmware's driver has no wheel.
            let m = p.mode();
            log::info!("firmware pointer: resolution {}/{}/{} (x/y/z)", m.resolution_x, m.resolution_y, m.resolution_z);
        }
        if let Some(p) = &mut pointers.absolute {
            let _ = p.reset(false);
        }
        log::info!("pointer: relative={} absolute={}", pointers.relative.is_some(), pointers.absolute.is_some());
        pointers
    }

    /// The reports waiting.
    pub fn read(&mut self, reports: &mut Vec<PointerReport>) {
        if let Some(p) = &mut self.relative {
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
                // II's does): Z, in notches, as the USB report has it.
                reports.push(PointerReport {
                    delta: Some((dx, dy)),
                    wheel: state.relative_movement_z,
                    left: bool::from(state.left_button),
                    right: bool::from(state.right_button),
                    ..Default::default()
                });
            }
        }
        if let Some(p) = &mut self.absolute {
            let mode = *p.mode();
            while let Ok(Some(state)) = p.read_state() {
                let sx = (mode.absolute_max_x.saturating_sub(mode.absolute_min_x)).max(1) as f32;
                let sy = (mode.absolute_max_y.saturating_sub(mode.absolute_min_y)).max(1) as f32;
                let x = (state.current_x.saturating_sub(mode.absolute_min_x)) as f32 / sx;
                let y = (state.current_y.saturating_sub(mode.absolute_min_y)) as f32 / sy;
                log::debug!("absolute pointer: {state:?} mode: {mode:?}");
                // Some firmware exposes the touchpad as an absolute pointer,
                // so the cursor shows for those too.
                reports.push(PointerReport {
                    position: Some((x, y)),
                    left: state.active_buttons & 1 != 0,
                    ..Default::default()
                });
            }
        }
    }
}

/// Adds the events that end a wait to `events`: a key, a pointer.
pub fn input_events(keyboard: &Keyboard, pointers: &Pointers, events: &mut Vec<Event>) {
    events.extend(keyboard.event());
    if let Some(p) = &pointers.relative {
        events.extend(p.wait_for_input_event().ok());
    }
    if let Some(p) = &pointers.absolute {
        events.extend(p.wait_for_input_event().ok());
    }
}
