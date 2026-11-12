// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! The screen: the UI drawn into a buffer of GOP pixels, copied to the
//! screen where it changed, with the cursor on top.

use alloc::vec;
use alloc::vec::Vec;

use slint::platform::software_renderer::{PremultipliedRgbaColor, TargetPixel};
use uefi::boot::{self, ScopedProtocol};
use uefi::proto::console::gop::{BltOp, BltPixel, BltRegion, GraphicsOutput, Mode};

use super::{cursor, open_shared};

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

/// A display mode that can be switched to.
#[derive(Clone, Copy, Debug)]
pub struct DisplayMode {
    pub width: usize,
    pub height: usize,
    mode: Mode,
}

/// The text console's cursor, and whether it shows: whatever prints on the
/// screen moves it. Read from the protocol's memory, without calling the
/// firmware.
pub fn console_cursor() -> ((usize, usize), bool) {
    uefi::system::with_stdout(|out| (out.cursor_position(), out.cursor_visible()))
}

/// Where the cursor is and was drawn, in physical pixels.
pub struct Cursor {
    pub x: f32,
    pub y: f32,
    pub visible: bool,
    /// Where it was last drawn, to restore the background.
    pub drawn_at: Option<(usize, usize)>,
}

impl Cursor {
    /// Keeps it on the screen after a mode change, to be drawn again.
    pub fn fit(&mut self, display: &Display) {
        self.x = self.x.min(display.width as f32 - 1.);
        self.y = self.y.min(display.height as f32 - 1.);
        self.drawn_at = None;
    }
}

pub struct Display {
    gop: ScopedProtocol<GraphicsOutput>,
    pub width: usize,
    pub height: usize,
    pub fb: Vec<Pixel>,
}

impl Display {
    pub fn open() -> Result<Self, slint::PlatformError> {
        let handle = boot::get_handle_for_protocol::<GraphicsOutput>()
            .map_err(|_| slint::PlatformError::Other("No GOP".into()))?;
        let gop = open_shared::<GraphicsOutput>(handle)
            .ok_or_else(|| slint::PlatformError::Other("Cannot open GOP".into()))?;
        let (width, height) = gop.current_mode_info().resolution();
        Ok(Display { gop, width, height, fb: vec![Pixel(BltPixel::new(0, 0, 0)); width * height] })
    }

    /// The modes, without duplicates, smallest first.
    pub fn modes(&self) -> Vec<DisplayMode> {
        let mut modes: Vec<DisplayMode> = Vec::new();
        for mode in self.gop.modes() {
            let (w, h) = mode.info().resolution();
            if modes.iter().any(|m| m.width == w && m.height == h) {
                continue;
            }
            modes.push(DisplayMode { width: w, height: h, mode });
        }
        modes.sort_by_key(|m| (m.width * m.height, m.width));
        modes
    }

    pub fn blit_from_fb(&mut self, x: usize, y: usize, w: usize, h: usize) {
        if w == 0 || h == 0 {
            return;
        }
        // SAFETY: Pixel is a repr(transparent) BltPixel.
        let buffer = unsafe { core::slice::from_raw_parts(self.fb.as_ptr() as *const BltPixel, self.fb.len()) };
        let _ = self.gop.blt(BltOp::BufferToVideo {
            buffer,
            src: BltRegion::SubRectangle { coords: (x, y), px_stride: self.width },
            dest: (x, y),
            dims: (w, h),
        });
    }

    pub fn set_mode(&mut self, mode: &DisplayMode) -> bool {
        if self.gop.set_mode(&mode.mode).is_err() {
            return false;
        }
        let (w, h) = self.gop.current_mode_info().resolution();
        self.width = w;
        self.height = h;
        self.fb = vec![Pixel(BltPixel::new(0, 0, 0)); w * h];
        true
    }

    /// Whether the mode isn't this display's any more: another program
    /// changed it, or switched to text.
    pub fn mode_lost(&self) -> bool {
        self.gop.current_mode_info().resolution() != (self.width, self.height)
    }

    /// Puts back the mode and the picture after another program drew on
    /// the screen.
    pub fn restore(&mut self, modes: &[DisplayMode]) {
        if self.mode_lost()
            && let Some(mode) = modes.iter().find(|m| m.width == self.width && m.height == self.height)
        {
            let _ = self.gop.set_mode(&mode.mode);
        }
        self.blit_from_fb(0, 0, self.width, self.height);
    }

    /// Draws the cursor where it is now, after restoring what was under it.
    pub fn draw_cursor(&mut self, cursor: &mut Cursor, scale: f32) {
        let cursor_scale = if scale >= 1.75 { 2 } else { 1 };
        let (cw, ch) = cursor::size(cursor_scale);
        if let Some((x, y)) = cursor.drawn_at.take() {
            let w = cw.min(self.width - x);
            let h = ch.min(self.height - y);
            self.blit_from_fb(x, y, w, h);
        }
        if cursor.visible {
            let (x, y) = (cursor.x as usize, cursor.y as usize);
            let w = cw.min(self.width - x);
            let h = ch.min(self.height - y);
            let mut buf: Vec<BltPixel> = Vec::with_capacity(w * h);
            for row in 0..h {
                for col in 0..w {
                    let px = self.fb[(y + row) * self.width + x + col].0;
                    buf.push(cursor::blend(px, col, row, cursor_scale));
                }
            }
            let _ = self.gop.blt(BltOp::BufferToVideo { buffer: &buf, src: BltRegion::Full, dest: (x, y), dims: (w, h) });
            cursor.drawn_at = Some((x, y));
        }
    }
}
