// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Batteries and the AC adapter: what ACPI's battery methods return (`_BIF`,
//! `_BIX`, `_BST`), and Linux's `/sys/class/power_supply` files.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum ChargeState {
    #[default]
    Unknown,
    Charging,
    Discharging,
    Full,
    /// Plugged in, but not charging (a charge limit, a cold battery…).
    NotCharging,
}

/// One battery. Capacities are in mWh, or in mAh when `mah` (ACPI lets the
/// firmware report either); rates in mW, or mA.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Battery {
    pub state: ChargeState,
    /// The firmware says the charge is critically low.
    pub critical: bool,
    pub mah: bool,
    pub remaining: Option<u32>,
    pub full: Option<u32>,
    pub design: Option<u32>,
    /// The present charge or discharge rate, positive either way.
    pub rate: Option<u32>,
    pub voltage_mv: Option<u32>,
    pub design_voltage_mv: Option<u32>,
    /// The charge level, when given as such (Linux's `capacity`).
    pub percent: Option<u8>,
    pub cycle_count: Option<u32>,
    pub model: String,
    pub serial: String,
    /// The chemistry: "LION", "Li-poly"…
    pub technology: String,
    pub manufacturer: String,
    pub manufacture_date: String,
}

/// The machine's power supply.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Power {
    /// Whether the AC adapter is plugged in, if there is one to ask.
    pub ac_online: Option<bool>,
    pub batteries: Vec<Battery>,
}

impl Battery {
    /// The charge level, 0 to 100.
    pub fn level(&self) -> Option<f32> {
        if let Some(p) = self.percent {
            return Some(p.min(100) as f32);
        }
        let full = self.full.or(self.design).filter(|f| *f > 0)?;
        Some((self.remaining? as f32 * 100. / full as f32).clamp(0., 100.))
    }

    /// The voltage used to turn mAh into mWh.
    fn volts(&self) -> Option<f32> {
        self.design_voltage_mv.or(self.voltage_mv).filter(|v| *v > 0).map(|v| v as f32 / 1000.)
    }

    /// A capacity in mWh.
    pub fn mwh(&self, value: Option<u32>) -> Option<f32> {
        let v = value? as f32;
        if self.mah { self.volts().map(|volts| v * volts) } else { Some(v) }
    }

    /// The present rate in mW.
    pub fn power_mw(&self) -> Option<f32> {
        let rate = self.rate.filter(|r| *r > 0)? as f32;
        if !self.mah {
            return Some(rate);
        }
        // mA times the present voltage, or the design one.
        let volts = self.voltage_mv.or(self.design_voltage_mv).filter(|v| *v > 0)? as f32 / 1000.;
        Some(rate * volts)
    }

    /// What the battery holds when full, compared to when new, 0 to 100.
    pub fn health(&self) -> Option<f32> {
        let (full, design) = (self.full? as f32, self.design.filter(|d| *d > 0)? as f32);
        Some((full * 100. / design).min(100.))
    }

    /// Minutes until empty (discharging) or full (charging), at the present
    /// rate.
    pub fn minutes_left(&self) -> Option<u32> {
        let rate = self.rate.filter(|r| *r > 0)? as f32;
        let remaining = self.remaining? as f32;
        let hours = match self.state {
            ChargeState::Discharging => remaining / rate,
            ChargeState::Charging => (self.full? as f32 - remaining).max(0.) / rate,
            _ => return None,
        };
        // A rate that rounds the time to more than two days isn't one.
        (hours < 48.).then(|| libm::roundf(hours * 60.) as u32)
    }
}

impl Power {
    /// Minutes until all batteries are empty (discharging) or full
    /// (charging), at their combined rate.
    pub fn minutes_left(&self) -> Option<u32> {
        let all = self.batteries.as_slice();
        if let [b] = all {
            return b.minutes_left();
        }
        let rate: f32 = all.iter().filter_map(Battery::power_mw).sum();
        if rate <= 0. {
            return None;
        }
        let remaining: Option<f32> = all.iter().map(|b| b.mwh(b.remaining)).sum();
        let full: Option<f32> = all.iter().map(|b| b.mwh(b.full.or(b.design))).sum();
        let hours = match self.state() {
            ChargeState::Discharging => remaining? / rate,
            ChargeState::Charging => (full? - remaining?).max(0.) / rate,
            _ => return None,
        };
        // A rate that rounds the time to more than two days isn't one.
        (hours < 48.).then(|| libm::roundf(hours * 60.) as u32)
    }

    /// The charge level of all batteries together, 0 to 100.
    pub fn level(&self) -> Option<f32> {
        match self.batteries.as_slice() {
            [] => None,
            [b] => b.level(),
            all => {
                // Weighted by capacity when they all know theirs.
                let full: Option<f32> = all.iter().map(|b| b.mwh(b.full.or(b.design))).sum();
                let remaining: Option<f32> = all.iter().map(|b| b.mwh(b.remaining)).sum();
                match (remaining, full) {
                    (Some(r), Some(f)) if f > 0. => Some((r * 100. / f).clamp(0., 100.)),
                    _ => {
                        let levels: Vec<f32> = all.iter().filter_map(Battery::level).collect();
                        (!levels.is_empty()).then(|| levels.iter().sum::<f32>() / levels.len() as f32)
                    }
                }
            }
        }
    }

    /// The overall state: charging if one battery charges, discharging if
    /// one discharges.
    pub fn state(&self) -> ChargeState {
        let has = |s| self.batteries.iter().any(|b| b.state == s);
        if has(ChargeState::Charging) {
            ChargeState::Charging
        } else if has(ChargeState::Discharging) {
            ChargeState::Discharging
        } else if !self.batteries.is_empty() && self.batteries.iter().all(|b| b.state == ChargeState::Full) {
            ChargeState::Full
        } else if has(ChargeState::NotCharging) || has(ChargeState::Full) {
            ChargeState::NotCharging
        } else {
            ChargeState::Unknown
        }
    }
}

/// ACPI's battery methods. Only read by the x86 firmware, with the `battery`
/// feature (`firmware/battery/acpi.rs`).
#[cfg_attr(not(all(target_os = "uefi", target_arch = "x86_64", feature = "battery")), allow(dead_code))]
pub mod acpi {
    use super::{Battery, ChargeState};
    use alloc::format;
    use alloc::string::{String, ToString};

    /// An element of a package returned by an ACPI method.
    #[derive(Clone, Debug, PartialEq)]
    pub enum AmlValue {
        Integer(u64),
        String(String),
        Other,
    }

    impl AmlValue {
        /// A 32-bit field, where all ones (or more) means unknown.
        fn dword(&self) -> Option<u32> {
            match self {
                AmlValue::Integer(v) if *v < 0xffff_ffff => Some(*v as u32),
                _ => None,
            }
        }

        /// A capacity: 0 means unknown too.
        fn capacity(&self) -> Option<u32> {
            self.dword().filter(|v| *v > 0)
        }

        /// A string field. Some firmware gives the serial number as an integer.
        fn text(&self) -> String {
            match self {
                AmlValue::String(s) => s.trim_matches(|c: char| c.is_whitespace() || c == '\0').to_string(),
                AmlValue::Integer(v) => format!("{v}"),
                AmlValue::Other => String::new(),
            }
        }
    }

    /// An ACPI device id from its compressed EISA form (`_HID` and `_CID` give
    /// "PNP0C0A" as 0x0A0CD041).
    pub fn eisa_id(value: u64) -> String {
        let v = (value as u32).swap_bytes();
        let letter = |shift: u32| char::from(b'@' + ((v >> shift) & 0x1f) as u8);
        format!("{}{}{}{:04X}", letter(26), letter(21), letter(16), v & 0xffff)
    }

    fn get(package: &[AmlValue], i: usize) -> &AmlValue {
        package.get(i).unwrap_or(&AmlValue::Other)
    }

    /// `_BIF`: the battery's static information (ACPI 1.0 to 3.0).
    pub fn from_bif(p: &[AmlValue]) -> Option<Battery> {
        if p.len() < 13 {
            return None;
        }
        Some(Battery {
            mah: get(p, 0).dword() == Some(1),
            design: get(p, 1).capacity(),
            full: get(p, 2).capacity(),
            design_voltage_mv: get(p, 4).capacity(),
            model: get(p, 9).text(),
            serial: get(p, 10).text(),
            technology: get(p, 11).text(),
            manufacturer: get(p, 12).text(),
            ..Battery::default()
        })
    }

    /// `_BIX`: the extended information (ACPI 4.0 and later), with the cycle
    /// count.
    pub fn from_bix(p: &[AmlValue]) -> Option<Battery> {
        if p.len() < 20 {
            return None;
        }
        Some(Battery {
            mah: get(p, 1).dword() == Some(1),
            design: get(p, 2).capacity(),
            full: get(p, 3).capacity(),
            design_voltage_mv: get(p, 5).capacity(),
            cycle_count: get(p, 8).dword(),
            model: get(p, 16).text(),
            serial: get(p, 17).text(),
            technology: get(p, 18).text(),
            manufacturer: get(p, 19).text(),
            ..Battery::default()
        })
    }

    /// `_BST`: the present state, into `b`.
    pub fn apply_bst(b: &mut Battery, p: &[AmlValue]) {
        let flags = get(p, 0).dword().unwrap_or(0);
        b.critical = flags & 4 != 0;
        b.rate = get(p, 1).dword();
        b.remaining = get(p, 2).dword();
        b.voltage_mv = get(p, 3).capacity();
        b.state = if flags & 1 != 0 {
            ChargeState::Discharging
        } else if flags & 2 != 0 {
            ChargeState::Charging
        } else if b.level().is_some_and(|l| l >= 97.) {
            ChargeState::Full
        } else {
            ChargeState::NotCharging
        };
    }
}

/// A battery from the files of a `/sys/class/power_supply/BAT*` directory,
/// given by `file(name)`. Energies are in µWh, charges in µAh.
#[cfg_attr(target_os = "uefi", allow(dead_code))] // the desktop's
pub fn from_sysfs(file: impl Fn(&str) -> Option<String>) -> Battery {
    let micro = |name: &str| -> Option<u32> {
        let v: i64 = file(name)?.trim().parse().ok()?;
        u32::try_from(v.unsigned_abs() / 1000).ok()
    };
    let text = |name: &str| file(name).map(|s| s.trim().to_string()).unwrap_or_default();
    let mah = file("energy_full").is_none() && file("charge_full").is_some();
    let (now, full, design, rate) = if mah {
        ("charge_now", "charge_full", "charge_full_design", "current_now")
    } else {
        ("energy_now", "energy_full", "energy_full_design", "power_now")
    };
    Battery {
        state: match text("status").as_str() {
            "Charging" => ChargeState::Charging,
            "Discharging" => ChargeState::Discharging,
            "Full" => ChargeState::Full,
            "Not charging" => ChargeState::NotCharging,
            _ => ChargeState::Unknown,
        },
        critical: text("capacity_level") == "Critical",
        mah,
        remaining: micro(now),
        full: micro(full).filter(|v| *v > 0),
        design: micro(design).filter(|v| *v > 0),
        rate: micro(rate),
        voltage_mv: micro("voltage_now").filter(|v| *v > 0),
        design_voltage_mv: micro("voltage_min_design").filter(|v| *v > 0),
        percent: file("capacity").and_then(|c| c.trim().parse().ok()),
        // The kernel reports 0 when the battery doesn't say.
        cycle_count: file("cycle_count").and_then(|c| c.trim().parse().ok()).filter(|c| *c > 0),
        model: text("model_name"),
        serial: text("serial_number"),
        technology: text("technology").replace("Unknown", ""),
        manufacturer: text("manufacturer"),
        manufacture_date: {
            let num = |name| file(name).and_then(|v| v.trim().parse::<u32>().ok());
            match (num("manufacture_year"), num("manufacture_month"), num("manufacture_day")) {
                (Some(y), Some(m), Some(d)) => format!("{y}-{m:02}-{d:02}"),
                (Some(y), _, _) => format!("{y}"),
                _ => String::new(),
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::acpi::*;
    use super::*;
    use alloc::vec;
    use AmlValue::{Integer as I, String as S};

    fn s(text: &str) -> AmlValue {
        S(text.into())
    }

    #[test]
    fn bix_and_bst() {
        let bix = vec![
            I(1),
            I(0), // mWh
            I(57000),
            I(50160),
            I(1),
            I(11520),
            I(5700),
            I(200),
            I(312),
            I(95000),
            I(0xffff_ffff),
            I(0xffff_ffff),
            I(0xffff_ffff),
            I(0xffff_ffff),
            I(1),
            I(1),
            s("5B10W13930"),
            s(" 1234 "),
            s("LiP\0"),
            s("SMP"),
            I(1),
        ];
        let mut b = from_bix(&bix).unwrap();
        assert_eq!((b.design, b.full, b.design_voltage_mv, b.cycle_count), (Some(57000), Some(50160), Some(11520), Some(312)));
        assert_eq!((b.model.as_str(), b.serial.as_str(), b.technology.as_str(), b.manufacturer.as_str()), ("5B10W13930", "1234", "LiP", "SMP"));
        assert!(!b.mah);
        assert_eq!(b.health(), Some(88.));

        apply_bst(&mut b, &[I(1), I(10032), I(25080), I(11800)]);
        assert_eq!(b.state, ChargeState::Discharging);
        assert_eq!(b.level(), Some(50.));
        assert_eq!(b.minutes_left(), Some(150));
        assert_eq!(b.power_mw(), Some(10032.));

        apply_bst(&mut b, &[I(2), I(25080), I(25080), I(12600)]);
        assert_eq!(b.state, ChargeState::Charging);
        assert_eq!(b.minutes_left(), Some(60));

        // Neither charging nor discharging: full, or held below full.
        apply_bst(&mut b, &[I(0), I(0), I(50000), I(12600)]);
        assert_eq!((b.state, b.minutes_left()), (ChargeState::Full, None));
        apply_bst(&mut b, &[I(4), I(0xffff_ffff), I(40000), I(0xffff_ffff)]);
        assert_eq!(b.state, ChargeState::NotCharging);
        assert!(b.critical);
        assert_eq!((b.rate, b.voltage_mv, b.power_mw()), (None, None, None));

        assert_eq!(from_bix(&bix[..19]), None);
    }

    #[test]
    fn bif_in_mah() {
        let bif = vec![I(1), I(4400), I(4000), I(1), I(10800), I(0), I(0), I(1), I(1), s("AL12"), I(42), s("Li-ion"), s("ACME")];
        let mut b = from_bif(&bif).unwrap();
        assert!(b.mah);
        assert_eq!(b.serial, "42");
        assert_eq!(b.mwh(b.full), Some(43200.));
        apply_bst(&mut b, &[I(1), I(1000), I(2000), I(11000)]);
        assert_eq!(b.power_mw(), Some(11000.));
        assert_eq!(b.minutes_left(), Some(120));
        assert_eq!(from_bif(&bif[..12]), None);
    }

    #[test]
    fn two_batteries() {
        let a = Battery { full: Some(20000), remaining: Some(20000), state: ChargeState::Full, ..Default::default() };
        let b = Battery { full: Some(60000), remaining: Some(20000), state: ChargeState::Discharging, ..Default::default() };
        let power = Power { ac_online: Some(false), batteries: vec![a.clone(), b] };
        assert_eq!(power.level(), Some(50.));
        assert_eq!(power.state(), ChargeState::Discharging);
        let full = Power { ac_online: Some(true), batteries: vec![a.clone(), a] };
        assert_eq!(full.state(), ChargeState::Full);
        assert_eq!(Power::default().level(), None);
        assert_eq!(Power::default().state(), ChargeState::Unknown);
    }

    #[test]
    fn eisa_ids() {
        assert_eq!(eisa_id(0x0A0CD041), "PNP0C0A");
        assert_eq!(eisa_id(0x090CD041), "PNP0C09");
    }

    #[test]
    fn sysfs() {
        let files = [
            ("status", "Charging\n"),
            ("capacity", "63\n"),
            ("energy_now", "32760000"),
            ("energy_full", "52000000"),
            ("energy_full_design", "57000000"),
            ("power_now", "-25000000"),
            ("voltage_now", "12400000"),
            ("cycle_count", "0"),
            ("model_name", "5B10W13930"),
            ("technology", "Li-poly"),
            ("manufacturer", "SMP"),
        ];
        let b = from_sysfs(|name| files.iter().find(|f| f.0 == name).map(|f| f.1.to_string()));
        assert_eq!(b.state, ChargeState::Charging);
        assert_eq!(b.level(), Some(63.));
        assert_eq!((b.remaining, b.full, b.design, b.rate), (Some(32760), Some(52000), Some(57000), Some(25000)));
        assert_eq!((b.voltage_mv, b.cycle_count), (Some(12400), None));
        assert_eq!(b.minutes_left(), Some(46));
        assert_eq!(b.technology, "Li-poly");

        let charge = [("charge_now", "2000000"), ("charge_full", "4000000"), ("current_now", "1000000"), ("status", "Discharging"), ("voltage_now", "11000000")];
        let b = from_sysfs(|name| charge.iter().find(|f| f.0 == name).map(|f| f.1.to_string()));
        assert!(b.mah);
        assert_eq!((b.level(), b.minutes_left(), b.power_mw()), (Some(50.), Some(120), Some(11000.)));
    }
}
