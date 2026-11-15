// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Temperatures and fans: ACPI's thermal zones (`_TMP`) and fan devices
//! (`_FST`) in the firmware, Linux's sysfs on the desktop.

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sensors {
    pub temperatures: Vec<Temperature>,
    pub fans: Vec<Fan>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Temperature {
    /// The thermal zone ("TZ00", "CPUZ"…, "x86_pkg_temp" on Linux).
    pub name: String,
    pub celsius: f32,
    /// Where the system shuts down (`_CRT`), if it says.
    pub critical: Option<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fan {
    pub name: String,
    pub rpm: u32,
}

/// ACPI's tenths of a kelvin in °C; `None` for what no sensor reads
/// (0, "not available" values, or past 150 °C).
pub fn celsius_from_tenths_kelvin(tenths: u64) -> Option<f32> {
    let celsius = (tenths as f32 - 2731.5) / 10.;
    (-40. ..=150.).contains(&celsius).then_some(celsius)
}

/// A fan's speed from its `_FST` (ACPI 4.0: revision, control, speed);
/// `None` when it doesn't know it (0xFFFFFFFF).
pub fn rpm_from_fst(fst: &[u64]) -> Option<u32> {
    let speed = *fst.get(2)?;
    (speed < u64::from(u32::MAX)).then_some(speed as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temperatures() {
        assert_eq!(celsius_from_tenths_kelvin(3252), Some(52.05));
        assert_eq!(celsius_from_tenths_kelvin(2732), Some(0.05));
        assert_eq!(celsius_from_tenths_kelvin(0), None, "not read");
        assert_eq!(celsius_from_tenths_kelvin(5000), None, "past 150 °C");
    }

    #[test]
    fn fans() {
        assert_eq!(rpm_from_fst(&[0, 50, 2400]), Some(2400));
        assert_eq!(rpm_from_fst(&[0, 50, 0xFFFF_FFFF]), None, "unknown");
        assert_eq!(rpm_from_fst(&[0, 50]), None);
    }
}
