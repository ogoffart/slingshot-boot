// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! The processor's own temperature, from Intel's digital thermal sensor:
//! what's left on machines whose firmware has no ACPI thermal zone (desktops,
//! whose firmware reads its sensors its own way).

use core::arch::x86_64::__cpuid;

use crate::parse::sensors::Temperature;
use crate::uefi_platform::rdmsr;

/// TjMax, in bits 16 to 23.
const TEMPERATURE_TARGET: u32 = 0x1a2;
/// How far below TjMax the hottest core of the package is, in bits 16 to 22.
const PACKAGE_THERM_STATUS: u32 = 0x1b1;

/// The package's sensor, on an Intel processor that has it.
pub struct Sensor {
    /// The temperature the processor is made to run at most, in °C.
    tj_max: u32,
}

impl Sensor {
    /// The processor's sensor, if it has one.
    pub fn find() -> Option<Sensor> {
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
        Some(Sensor { tj_max })
    }

    /// The package's temperature now.
    pub fn read(&self) -> Temperature {
        // SAFETY: `find` checked the processor has it.
        let below = (unsafe { rdmsr(PACKAGE_THERM_STATUS) } >> 16) as u32 & 0x7f;
        let celsius = self.tj_max.saturating_sub(below);
        Temperature { name: "CPU".into(), celsius: celsius as f32, critical: Some(self.tj_max as f32) }
    }
}
