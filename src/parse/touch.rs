// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! A touchpad's fingers (`hid::Touch`) as a pointer, as desktops do it:
//!
//! - one finger moves the pointer, and when several do, the one that moved
//!   the most (one presses, another drags);
//! - two fingers moving together scroll, like a wheel: up when they go up;
//! - a press clicks: a right click in the bottom right corner;
//! - a tap (down and up quickly, without moving) clicks too: with two
//!   fingers, a right click.

use alloc::vec::Vec;
use core::time::Duration;

use super::hid::{Finger, MouseReport, Touch};

/// What the pointer does: a move and a scroll in pixels at scale 1 (the
/// scroll positive toward the top), and the buttons.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pointer {
    pub dx: f32,
    pub dy: f32,
    pub scroll: f32,
    pub left: bool,
    pub right: bool,
}

impl From<MouseReport> for Pointer {
    fn from(m: MouseReport) -> Self {
        Pointer { dx: m.dx as f32, dy: m.dy as f32, scroll: 0., left: m.left, right: m.right }
    }
}

/// How far a finger across the pad moves the pointer, in pixels at scale
/// 1: 2.5 times the Framework's screen (11 cm of pad, scaled ×1.5). The
/// platform accelerates fast moves.
const PAD_WIDTH_PIXELS: f32 = 2273.;
/// The longest a tap lasts.
const TAP_TIME: Duration = Duration::from_millis(200);
/// How little a tap's fingers move: the pad's width over this.
const TAP_STILL: i32 = 40;
/// The right button's area, in the pad's size: its right half, bottom
/// fifth.
const RIGHT_BUTTON: (f32, f32) = (0.5, 0.8);

/// How far a finger moved, roughly.
fn distance((x, y): (i32, i32)) -> i32 {
    x.abs() + y.abs()
}

#[derive(Default)]
pub struct Gestures {
    /// The largest X and Y.
    size: (i32, i32),
    /// The fingers at the last report.
    fingers: Vec<Finger>,
    /// Two fingers moved together: they scroll until there are more or
    /// fewer, even when one stays still a moment.
    scrolling: bool,
    press: Option<Press>,
    tap: Option<Tap>,
    /// A tap clicked: released at the next poll.
    tapped: bool,
    /// The pad said once that no finger touches it: before, a finger may
    /// be one the pad had before Slingshot (the click that restarted), and
    /// lifting it isn't a tap.
    lifted: bool,
}

struct Press {
    right: bool,
    /// The pointer's movement since, logged on release.
    moved: (f32, f32),
}

/// Fingers down since `start`, and still a tap.
struct Tap {
    start: Duration,
    fingers: usize,
    moved: i32,
}

impl Gestures {
    /// For a pad whose fingers go up to `size`.
    pub fn new(size: (i32, i32)) -> Self {
        Gestures { size, ..Default::default() }
    }

    /// After a tap's click, its release: once.
    pub fn release(&mut self) -> Option<Pointer> {
        core::mem::take(&mut self.tapped).then(Pointer::default)
    }

    /// The pointer for a report, at `now`.
    pub fn touch(&mut self, t: Touch, now: Duration) -> Pointer {
        let mut out = Pointer::default();
        // Each finger's move since the last report.
        let moves: Vec<(i32, i32)> = t
            .fingers
            .iter()
            .filter_map(|f| {
                let before = self.fingers.iter().find(|b| b.contact == f.contact)?;
                Some((f.x - before.x, f.y - before.y))
            })
            .collect();
        let speed = PAD_WIDTH_PIXELS / self.size.0.max(1) as f32;
        if t.fingers.len() != self.fingers.len() || t.button {
            self.scrolling = false;
        }
        if let [a, b, ..] = moves[..]
            && !t.button
            && (self.scrolling || a.0 * b.0 + a.1 * b.1 > 0)
        {
            self.scrolling = true;
            out.scroll = -(a.1 + b.1) as f32 / 2. * speed;
        } else if let Some(&(x, y)) = moves.iter().max_by_key(|&&m| distance(m)) {
            (out.dx, out.dy) = (x as f32 * speed, y as f32 * speed);
        }

        match (&mut self.press, t.button) {
            (None, true) => {
                // The lowest finger is the one pressing.
                let right = t
                    .fingers
                    .iter()
                    .max_by_key(|f| f.y)
                    .is_some_and(|f| {
                        f.x as f32 > self.size.0 as f32 * RIGHT_BUTTON.0 && f.y as f32 > self.size.1 as f32 * RIGHT_BUTTON.1
                    });
                log::info!("touchpad: pressed, {} finger(s): {} click", t.fingers.len(), if right { "right" } else { "left" });
                self.press = Some(Press { right, moved: (out.dx, out.dy) });
            }
            (Some(p), true) => p.moved = (p.moved.0 + out.dx, p.moved.1 + out.dy),
            (Some(p), false) => {
                log::info!("touchpad: released, pointer moved by {:.0},{:.0} while pressed", p.moved.0, p.moved.1);
                self.press = None;
            }
            (None, false) => {}
        }

        if !t.fingers.is_empty() {
            if self.fingers.is_empty() && self.lifted {
                self.tap = Some(Tap { start: now, fingers: 0, moved: 0 });
            }
            if let Some(tap) = &mut self.tap {
                tap.fingers = tap.fingers.max(t.fingers.len());
                tap.moved += moves.iter().map(|&m| distance(m)).sum::<i32>();
                if t.button || t.palm || now - tap.start > TAP_TIME || tap.moved > self.size.0 / TAP_STILL {
                    self.tap = None;
                }
            }
        } else if let Some(tap) = self.tap.take()
            && !t.palm
            && now - tap.start <= TAP_TIME
        {
            log::info!("touchpad: tapped, {} finger(s)", tap.fingers);
            (out.left, out.right) = (tap.fingers == 1, tap.fingers >= 2);
            self.tapped = true;
        }
        if t.fingers.is_empty() {
            self.lifted = true;
        }

        if let Some(p) = &self.press {
            (out.left, out.right) = (!p.right, p.right);
        }
        self.fingers = t.fingers;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (i32, i32) = (1337, 876);

    fn at(fingers: &[(i32, i32, i32)], button: bool) -> Touch {
        Touch { fingers: fingers.iter().map(|&(contact, x, y)| Finger { contact, x, y }).collect(), button, palm: false }
    }

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    /// The move and the scroll, to the pixel.
    fn moved(p: Pointer) -> (i32, i32, i32) {
        (libm::roundf(p.dx) as i32, libm::roundf(p.dy) as i32, libm::roundf(p.scroll) as i32)
    }

    #[test]
    fn one_finger_moves() {
        let mut g = Gestures::new(SIZE);
        assert_eq!(g.touch(at(&[(0, 100, 100)], false), ms(0)), Pointer::default(), "just down");
        let p = g.touch(at(&[(0, 110, 100)], false), ms(10));
        assert_eq!(moved(p), (17, 0, 0));
    }

    #[test]
    fn two_fingers_together_scroll() {
        let mut g = Gestures::new(SIZE);
        g.touch(at(&[(0, 100, 100), (1, 300, 100)], false), ms(0));
        let p = g.touch(at(&[(0, 100, 110), (1, 300, 110)], false), ms(10));
        assert_eq!(moved(p), (0, 0, -17), "down: toward the bottom");
        // Once scrolling, a finger still a moment doesn't stop it.
        let p = g.touch(at(&[(0, 100, 120), (1, 300, 110)], false), ms(20));
        assert_eq!(moved(p), (0, 0, -9));
        // Apart (or one resting): not a scroll, the one that moves moves the pointer.
        let mut g = Gestures::new(SIZE);
        g.touch(at(&[(0, 100, 100), (1, 300, 100)], false), ms(0));
        let p = g.touch(at(&[(0, 100, 110), (1, 300, 100)], false), ms(20));
        assert_eq!(moved(p), (0, 17, 0));
    }

    #[test]
    fn press_left_or_bottom_right() {
        let mut g = Gestures::new(SIZE);
        g.touch(at(&[(0, 200, 800)], false), ms(0));
        let p = g.touch(at(&[(0, 200, 800)], true), ms(10));
        assert!(p.left && !p.right);
        // Another finger drags: still the left button, and the pointer follows it.
        g.touch(at(&[(0, 200, 800), (1, 600, 300)], true), ms(20));
        let p = g.touch(at(&[(0, 200, 800), (1, 620, 300)], true), ms(30));
        assert!(p.left && p.dx > 0.);
        assert!(!g.touch(at(&[], false), ms(40)).left);
        // In the bottom right corner: the right button.
        g.touch(at(&[(2, 1200, 800)], false), ms(1000));
        let p = g.touch(at(&[(2, 1200, 800)], true), ms(1010));
        assert!(p.right && !p.left);
    }

    #[test]
    fn tap_clicks() {
        let mut g = Gestures::new(SIZE);
        // A finger down before the first report (the click that
        // restarted), lifted: not a tap.
        g.touch(at(&[(9, 500, 400)], false), ms(0));
        assert!(!g.touch(at(&[], false), ms(50)).left);
        g.touch(at(&[(0, 500, 400)], false), ms(100));
        g.touch(at(&[(0, 502, 400)], false), ms(150));
        let p = g.touch(at(&[], false), ms(200));
        assert!(p.left && !p.right);
        assert_eq!(g.release(), Some(Pointer::default()));
        assert_eq!(g.release(), None);
        // Two fingers: a right click.
        g.touch(at(&[(1, 500, 400), (2, 700, 400)], false), ms(1000));
        assert!(g.touch(at(&[], false), ms(1100)).right);
        // A palm seen: not a tap.
        g.release();
        g.touch(at(&[(5, 500, 400)], false), ms(1500));
        let mut palm = at(&[], false);
        palm.palm = true;
        assert!(!g.touch(palm, ms(1550)).left);
        // Too long, or moving: not a tap.
        g.release();
        g.touch(at(&[(3, 500, 400)], false), ms(2000));
        assert!(!g.touch(at(&[], false), ms(2300)).left);
        g.touch(at(&[(4, 500, 400)], false), ms(3000));
        g.touch(at(&[(4, 600, 400)], false), ms(3050));
        assert!(!g.touch(at(&[], false), ms(3100)).left);
    }
}
