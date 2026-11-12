// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! The log: kept in memory, so that it can be saved to the EFI System
//! Partition from the System screen (F12) and sent with a bug report from a
//! real machine. With the `debugcon` feature, also written to QEMU's debug
//! console as it goes: I/O port 0x402 on x86 (run QEMU with `-debugcon
//! file:debug.log -global isa-debugcon.iobase=0x402`), the `virt` machine's
//! UART on ARM (shared with the firmware's serial console). On real hardware
//! that port or address may belong to a device, hence the feature.

use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The most the log keeps: the oldest lines go first.
const MAX: usize = 256 * 1024;
/// The longest line kept.
const MAX_LINE: usize = 4096;

struct Buffer {
    lock: AtomicBool,
    data: UnsafeCell<Vec<u8>>,
}

// SAFETY: `data` is only touched with `lock` held.
unsafe impl Sync for Buffer {}

/// The timer when Slingshot started: each line says the milliseconds since.
static START: AtomicU64 = AtomicU64::new(0);

static BUFFER: Buffer = Buffer { lock: AtomicBool::new(false), data: UnsafeCell::new(Vec::new()) };

impl Buffer {
    /// Runs `f` on the buffer, unless another processor holds it (then the
    /// line is lost rather than waited for: the log mustn't hang).
    fn with<T>(&self, f: impl FnOnce(&mut Vec<u8>) -> T) -> Option<T> {
        self.lock.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).ok()?;
        // SAFETY: The lock is held.
        let result = f(unsafe { &mut *self.data.get() });
        self.lock.store(false, Ordering::Release);
        Some(result)
    }
}

#[cfg(feature = "debugcon")]
fn write_port(bytes: &[u8]) {
    for b in bytes {
        cfg_select! {
            target_arch = "x86_64" => {
                // SAFETY: Writing to the debug console port has no other
                // effect in QEMU.
                unsafe { core::arch::asm!("out dx, al", in("dx") 0x402u16, in("al") *b) };
            }
            target_arch = "aarch64" => {
                // SAFETY: The data register of the PL011 UART of QEMU's
                // `virt` machine, identity-mapped by the firmware.
                unsafe { (0x0900_0000 as *mut u32).write_volatile(*b as u32) };
            }
        }
    }
}

struct Logger;

impl log::Log for Logger {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        let ticks = crate::uefi_platform::timer_tick().saturating_sub(START.load(Ordering::Relaxed));
        let ms = ticks.saturating_mul(1000) / crate::uefi_platform::timer_freq();
        let mut line = alloc::format!("[{ms:>6} ms] [{}] {}: {}\n", record.level(), record.target(), record.args());
        // A line made of what a disk holds (a title, a command line) can be
        // huge: cut, so it never takes the whole buffer.
        if line.len() > MAX_LINE {
            let mut cut = MAX_LINE;
            while !line.is_char_boundary(cut) {
                cut -= 1;
            }
            line.truncate(cut);
            line.push_str("…\n");
        }
        #[cfg(feature = "debugcon")]
        write_port(line.as_bytes());
        BUFFER.with(|data| {
            if data.len() + line.len() > MAX {
                // Drop the oldest lines, at a line boundary.
                let cut = data.len() + line.len() - MAX;
                let cut = data[cut..].iter().position(|b| *b == b'\n').map_or(data.len(), |i| cut + i + 1);
                data.drain(..cut);
            }
            data.extend_from_slice(line.as_bytes());
        });
    }
    fn flush(&self) {}
}

static LOGGER: Logger = Logger;

pub fn init() {
    START.store(crate::uefi_platform::timer_tick(), Ordering::Relaxed);
    // Measured now (10 ms), not in the middle of a line.
    crate::uefi_platform::timer_freq();
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(log::LevelFilter::Info);
}

/// What the log holds.
pub fn contents() -> Vec<u8> {
    BUFFER.with(|data| data.clone()).unwrap_or_default()
}
