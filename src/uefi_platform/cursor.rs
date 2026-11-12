// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! The mouse cursor, drawn by the platform on top of the UI (the firmware
//! has no cursor of its own).

use uefi::proto::console::gop::BltPixel;

// 'X' = outline, '.' = fill, ' ' = transparent.
const ARROW: [&str; 19] = [
    "X           ",
    "XX          ",
    "X.X         ",
    "X..X        ",
    "X...X       ",
    "X....X      ",
    "X.....X     ",
    "X......X    ",
    "X.......X   ",
    "X........X  ",
    "X.........X ",
    "X......XXXXX",
    "X...X..X    ",
    "X..XX..X    ",
    "X.X  X..X   ",
    "XX   X..X   ",
    "X     X..X  ",
    "      X..X  ",
    "       XX   ",
];

pub fn size(scale: usize) -> (usize, usize) {
    (ARROW[0].len() * scale, ARROW.len() * scale)
}

/// Returns the pixel at (x, y) of the cursor drawn over `background`.
pub fn blend(background: BltPixel, x: usize, y: usize, scale: usize) -> BltPixel {
    match ARROW[y / scale].as_bytes()[x / scale] {
        b'X' => BltPixel::new(16, 18, 28),
        b'.' => BltPixel::new(255, 255, 255),
        _ => background,
    }
}
