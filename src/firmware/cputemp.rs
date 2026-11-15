// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! The processor's own temperature, from Intel's digital thermal sensor:
//! what's left on machines whose firmware has no ACPI thermal zone (desktops,
//! whose firmware reads its sensors its own way).

use crate::parse::sensors::Temperature;

/// The package's temperature, on an Intel processor that has the sensor.
pub fn package() -> Option<Temperature> {
    #[cfg(target_arch = "x86_64")]
    return intel::package();
    #[cfg(not(target_arch = "x86_64"))]
    None
}

#[cfg(target_arch = "x86_64")]
mod intel {
    use core::arch::x86_64::__cpuid;
    use core::sync::atomic::{AtomicU32, Ordering};

    use crate::parse::sensors::Temperature;
    use crate::uefi_platform::rdmsr;

    /// The temperature the processor is made to run at most (TjMax), in
    /// bits 16 to 23.
    const TEMPERATURE_TARGET: u32 = 0x1a2;
    /// How far below TjMax the hottest core of the package is, in bits 16
    /// to 22.
    const PACKAGE_THERM_STATUS: u32 = 0x1b1;
    /// TjMax, once known: 0 before, `NONE` without the sensor.
    static TJ_MAX: AtomicU32 = AtomicU32::new(0);
    const NONE: u32 = u32::MAX;

    pub fn package() -> Option<Temperature> {
        let tj_max = match TJ_MAX.load(Ordering::Relaxed) {
            0 => {
                let tj_max = tj_max().unwrap_or(NONE);
                TJ_MAX.store(tj_max, Ordering::Relaxed);
                tj_max
            }
            t => t,
        };
        if tj_max == NONE {
            return None;
        }
        // SAFETY: `tj_max` said the processor has it.
        let below = (unsafe { rdmsr(PACKAGE_THERM_STATUS) } >> 16) as u32 & 0x7f;
        let celsius = tj_max.saturating_sub(below);
        Some(Temperature { name: "CPU".into(), celsius: celsius as f32, critical: Some(tj_max as f32) })
    }

    /// TjMax, on an Intel processor with the package's sensor.
    fn tj_max() -> Option<u32> {
        let vendor = __cpuid(0);
        let intel = (vendor.ebx, vendor.edx, vendor.ecx) == (0x756e_6547, 0x4965_6e69, 0x6c65_746e);
        // CPUID 6, EAX bit 6: the package's thermal status registers (Sandy
        // Bridge and later, which also have the target register).
        if !intel || vendor.eax < 6 || __cpuid(6).eax & (1 << 6) == 0 {
            log::info!("sensors: no temperature sensor of the processor's");
            return None;
        }
        // SAFETY: CPUID said the processor has it.
        let tj_max = (unsafe { rdmsr(TEMPERATURE_TARGET) } >> 16) as u32 & 0xff;
        // 0: not given, 100 °C as Linux takes it.
        let tj_max = if tj_max == 0 { 100 } else { tj_max };
        log::info!("sensors: the processor's package sensor, TjMax {tj_max} °C");
        Some(tj_max)
    }
}
