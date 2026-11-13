// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! The processor's counter: the time stamp counter on x86, the generic
//! timer on ARM.

use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;

use uefi::boot;

fn timer_tick() -> u64 {
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

/// The time since the processor's counter started (about when the machine
/// was reset). The first call takes 10 ms when the counter's frequency
/// has to be measured, and returns the time it was called.
pub fn uptime() -> Duration {
    let (ticks, freq) = (timer_tick(), timer_freq());
    // In whole seconds, then the rest: no overflow.
    Duration::new(ticks / freq, (ticks % freq * 1_000_000_000 / freq) as u32)
}

/// The timer's frequency (`timer_tick` per second): the TSC's from CPUID
/// leaf 0x15 when the processor says, else measured the first time.
fn timer_freq() -> u64 {
    static FREQ: AtomicU64 = AtomicU64::new(0);
    let freq = FREQ.load(Ordering::Relaxed);
    if freq != 0 {
        return freq;
    }
    let said = cfg_select! {
        target_arch = "x86_64" => {{
            use core::arch::x86_64::__cpuid;
            // Leaf 0 says which leaves exist.
            let leaf = (__cpuid(0).eax >= 0x15).then(|| __cpuid(0x15));
            leaf.filter(|r| r.eax != 0 && r.ebx != 0 && r.ecx != 0).map(|r| u64::from(r.ecx) * u64::from(r.ebx) / u64::from(r.eax))
        }}
        _ => None,
    };
    let freq = said.unwrap_or_else(|| {
        let start = timer_tick();
        boot::stall(Duration::from_millis(10));
        (timer_tick() - start).max(1) * 100
    });
    FREQ.store(freq, Ordering::Relaxed);
    freq
}
