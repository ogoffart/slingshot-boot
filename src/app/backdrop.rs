// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! What the wallpaper gives the UI: a blurred copy, drawn behind the cards
//! and the top bar (frosted glass: the blur is done once, here, not at every
//! frame), and an accent color taken from it.

use alloc::vec;
use alloc::vec::Vec;

/// An RGBA picture.
pub struct Pixels {
    pub width: usize,
    pub height: usize,
    pub data: Vec<[u8; 4]>,
}

/// A copy at most `max_width` wide (averaging the pixels it covers).
pub fn shrink(src: &Pixels, max_width: usize) -> Pixels {
    let factor = src.width.div_ceil(max_width).max(1);
    let (width, height) = ((src.width / factor).max(1), (src.height / factor).max(1));
    let mut data = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            let mut sum = [0u32; 4];
            let mut n = 0;
            for sy in y * factor..((y + 1) * factor).min(src.height) {
                for sx in x * factor..((x + 1) * factor).min(src.width) {
                    let p = src.data[sy * src.width + sx];
                    for c in 0..4 {
                        sum[c] += u32::from(p[c]);
                    }
                    n += 1;
                }
            }
            data.push(sum.map(|s| (s / n.max(1)) as u8));
        }
    }
    Pixels { width, height, data }
}

/// A box blur of `radius` along one direction (`dx`, `dy` = 1, 0 or 0, 1).
fn box_pass(p: &Pixels, radius: usize, horizontal: bool) -> Vec<[u8; 4]> {
    let (w, h) = (p.width, p.height);
    let mut out = vec![[0u8; 4]; w * h];
    let (lines, len) = if horizontal { (h, w) } else { (w, h) };
    let at = |line: usize, i: usize| if horizontal { line * w + i } else { i * w + line };
    let r = radius as isize;
    for line in 0..lines {
        let mut sum = [0i32; 4];
        // The window around i = 0, edges repeated.
        for k in -r..=r {
            let px = p.data[at(line, k.clamp(0, len as isize - 1) as usize)];
            for c in 0..4 {
                sum[c] += i32::from(px[c]);
            }
        }
        let n = 2 * r + 1;
        for i in 0..len {
            out[at(line, i)] = sum.map(|s| (s / n as i32) as u8);
            let leaving = p.data[at(line, (i as isize - r).clamp(0, len as isize - 1) as usize)];
            let entering = p.data[at(line, (i as isize + r + 1).clamp(0, len as isize - 1) as usize)];
            for c in 0..4 {
                sum[c] += i32::from(entering[c]) - i32::from(leaving[c]);
            }
        }
    }
    out
}

/// A blur close to a Gaussian one: three box blurs each way.
pub fn blur(p: &Pixels, radius: usize) -> Pixels {
    let mut data = p.data.clone();
    for _ in 0..3 {
        let horizontal = Pixels { width: p.width, height: p.height, data };
        let pass = box_pass(&horizontal, radius, true);
        data = box_pass(&Pixels { width: p.width, height: p.height, data: pass }, radius, false);
    }
    Pixels { width: p.width, height: p.height, data }
}

/// The picture's main color, made bright and saturated enough to be an
/// accent; `None` for a grey picture.
pub fn accent(p: &Pixels) -> Option<[u8; 3]> {
    // Hue histogram of the colorful pixels, weighted by how colorful.
    const BINS: usize = 24;
    let mut weight = [0f32; BINS];
    let mut sums = [[0f32; 3]; BINS];
    for px in &p.data {
        let [r, g, b] = [px[0], px[1], px[2]].map(|c| f32::from(c) / 255.);
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let (s, v) = (if max > 0. { (max - min) / max } else { 0. }, max);
        if s < 0.25 || v < 0.2 || px[3] < 128 {
            continue;
        }
        let hue = hue(r, g, b, max, min);
        let bin = ((hue / 360. * BINS as f32) as usize).min(BINS - 1);
        let w = s * v;
        weight[bin] += w;
        for (sum, c) in sums[bin].iter_mut().zip([r, g, b]) {
            *sum += c * w;
        }
    }
    let (best, total) = weight.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
    // At least a little color: a few percent of the picture.
    if *total < p.data.len() as f32 * 0.02 {
        return None;
    }
    let [r, g, b] = sums[best].map(|s| s / total);
    // The same hue, bright and saturated (like the built-in accents).
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let h = hue(r, g, b, max, min);
    let s = if max > 0. { ((max - min) / max).max(0.55) } else { 0.55 };
    Some(hsv(h, s.min(0.75), 0.95))
}

fn hue(r: f32, g: f32, b: f32, max: f32, min: f32) -> f32 {
    let d = max - min;
    if d <= 0. {
        return 0.;
    }
    let h = if max == r {
        (g - b) / d
    } else if max == g {
        (b - r) / d + 2.
    } else {
        (r - g) / d + 4.
    };
    let h = h * 60.;
    if h < 0. { h + 360. } else { h }
}

fn hsv(h: f32, s: f32, v: f32) -> [u8; 3] {
    let c = v * s;
    // No `rem_euclid` without std: h is in [0, 360).
    let x = c * (1. - (libm::fmodf(h / 60., 2.) - 1.).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.) as u32 {
        0 => (c, x, 0.),
        1 => (x, c, 0.),
        2 => (0., c, x),
        3 => (0., x, c),
        4 => (x, 0., c),
        _ => (c, 0., x),
    };
    [r, g, b].map(|v| libm::roundf((v + m) * 255.).clamp(0., 255.) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: usize, height: usize, color: [u8; 4]) -> Pixels {
        Pixels { width, height, data: vec![color; width * height] }
    }

    #[test]
    fn shrinks_by_averaging() {
        let mut p = solid(4, 2, [0, 0, 0, 255]);
        p.data[0] = [200, 100, 0, 255];
        let s = shrink(&p, 2);
        assert_eq!((s.width, s.height), (2, 1));
        assert_eq!(s.data[0], [50, 25, 0, 255]);
    }

    #[test]
    fn blur_spreads_and_keeps_solid_colors() {
        assert_eq!(blur(&solid(5, 5, [10, 20, 30, 255]), 2).data[12], [10, 20, 30, 255]);
        let mut p = solid(9, 1, [0, 0, 0, 255]);
        p.data[4] = [255, 255, 255, 255];
        let b = blur(&p, 1);
        assert!(b.data[4][0] < 255 && b.data[2][0] > 0, "{:?}", b.data);
    }

    #[test]
    fn accent_of_a_blue_picture_is_blue() {
        let mut p = solid(10, 10, [12, 14, 20, 255]);
        for px in p.data.iter_mut().take(40) {
            *px = [30, 60, 140, 255];
        }
        let [r, g, b] = accent(&p).unwrap();
        assert!(b > r && b > g, "{r} {g} {b}");
        assert_eq!(accent(&solid(4, 4, [128, 128, 128, 255])), None);
    }
}
