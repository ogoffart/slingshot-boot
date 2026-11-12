// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! The processor's counter: the time stamp counter on x86, the generic
//! timer on ARM.

use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;

use uefi::boot;

pub fn timer_tick() -> u64 {
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
pub fn timer_freq() -> u64 {
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
