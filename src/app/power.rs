// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! The System screen's battery card: the charge, whether it's charging, the
//! AC adapter, and the details the firmware gives.

use crate::tr::{tr, trf};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::parse::battery::{Battery, ChargeState, Power};

#[derive(Debug, Default, PartialEq)]
pub struct PowerView {
    /// The level, 0 to 100, when known.
    pub level: Option<f32>,
    /// Which way the charge goes: 1 up, -1 down, 0 neither.
    pub direction: i32,
    /// "Charging", "On battery"…
    pub state: String,
    /// "1 h 05 min until full"
    pub time_left: String,
    /// "AC adapter plugged in", or "" when nothing says.
    pub ac: String,
    pub ac_online: bool,
    pub low: bool,
    /// Measurements: label and value.
    pub facts: Vec<(String, String)>,
    /// What the battery is: "Li-poly · SMP 5B10W13930 · Serial 1234".
    pub identity: String,
}

fn duration(minutes: u32) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => trf("{} min", &[&m]),
        (h, 0) => trf("{} h", &[&h]),
        (h, m) => trf("{} h {} min", &[&h, &format!("{m:02}")]),
    }
}

/// "52.0 Wh", or "4000 mAh" when it can't be turned into energy.
fn capacity(b: &Battery, value: Option<u32>) -> Option<String> {
    match b.mwh(value) {
        Some(mwh) => Some(format!("{:.1} Wh", mwh / 1000.)),
        None => value.map(|v| format!("{v} mAh")),
    }
}

/// The chemistry, as the firmware's abbreviations ("LION", "LiP") say it.
fn chemistry(t: &str) -> String {
    match t.to_ascii_lowercase().replace(['-', ' '], "").as_str() {
        "lion" | "liion" | "li" => "Li-ion".into(),
        "lip" | "lipo" | "lipoly" | "lipolymer" => "Li-poly".into(),
        "nimh" => "NiMH".into(),
        "nicd" => "NiCd".into(),
        "pbac" | "leadacid" => tr("Lead acid"),
        _ => t.to_string(),
    }
}

pub fn view(power: &Power) -> Option<PowerView> {
    let first = power.batteries.first()?;
    let state = power.state();
    let level = power.level();
    let charging = state == ChargeState::Charging;
    let ac_online = power.ac_online.unwrap_or(charging || matches!(state, ChargeState::Full | ChargeState::NotCharging));
    let mut v = PowerView {
        level,
        direction: match state {
            ChargeState::Charging => 1,
            ChargeState::Discharging => -1,
            _ => 0,
        },
        state: match state {
            ChargeState::Charging => tr("Charging"),
            ChargeState::Discharging => tr("On battery"),
            ChargeState::Full => tr("Fully charged"),
            ChargeState::NotCharging => tr("Plugged in, not charging"),
            ChargeState::Unknown if level.is_none() => tr("Charge level unknown"),
            ChargeState::Unknown => tr("Battery"),
        },
        ac: match power.ac_online {
            Some(true) => tr("AC adapter plugged in"),
            Some(false) => tr("AC adapter unplugged"),
            None => String::new(),
        },
        ac_online,
        low: power.batteries.iter().any(|b| b.critical)
            || (state == ChargeState::Discharging && level.is_some_and(|l| l < 10.)),
        ..PowerView::default()
    };
    v.time_left = match (power.minutes_left(), state) {
        (Some(m), ChargeState::Charging) => trf("{} until full", &[&duration(m)]),
        (Some(m), ChargeState::Discharging) => trf("{} left", &[&duration(m)]),
        _ => String::new(),
    };

    let facts = &mut v.facts;
    let mut fact = |label: String, value: String| {
        if !value.is_empty() {
            facts.push((label, value));
        }
    };
    if power.batteries.len() > 1 {
        let levels: Vec<String> =
            power.batteries.iter().map(|b| b.level().map(|l| format!("{l:.0} %")).unwrap_or("?".into())).collect();
        fact(tr("Batteries"), format!("{} · {}", power.batteries.len(), levels.join(&tr(" and "))));
    }
    let watts: Option<f32> = power.batteries.iter().map(|b| b.power_mw()).sum();
    if let Some(mw) = watts.filter(|w| *w > 0.) {
        let label = if charging { tr("Charging at") } else { tr("Drawing") };
        fact(label, format!("{:.1} W", mw / 1000.));
    }
    let b = first;
    if let (Some(now), Some(full)) = (capacity(b, b.remaining), capacity(b, b.full)) {
        // "32.8 of 52.0 Wh"
        let unit = full.rsplit(' ').next().unwrap_or("");
        fact(tr("Energy"), trf("{} of {}", &[&now.trim_end_matches(unit).trim_end(), &full]));
    }
    if let Some(health) = b.health() {
        let design = capacity(b, b.design).map(|d| trf(" · {} new", &[&d])).unwrap_or_default();
        fact(tr("Health"), format!("{health:.0} %{design}"));
    } else if let Some(design) = capacity(b, b.design) {
        fact(tr("Capacity"), design);
    }
    if let Some(mv) = b.voltage_mv.or(b.design_voltage_mv) {
        fact(tr("Voltage"), format!("{:.2} V", mv as f32 / 1000.));
    }
    if let Some(c) = b.cycle_count {
        fact(tr("Cycles"), c.to_string());
    }
    let model = [b.manufacturer.as_str(), b.model.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>();
    let parts = [
        chemistry(&b.technology),
        model.join(" "),
        if b.serial.is_empty() { String::new() } else { trf("Serial {}", &[&b.serial]) },
        if b.manufacture_date.is_empty() { String::new() } else { trf("Made {}", &[&b.manufacture_date]) },
    ];
    v.identity = parts.iter().filter(|p| !p.is_empty()).cloned().collect::<Vec<_>>().join(" · ");
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn laptop() -> Battery {
        Battery {
            state: ChargeState::Discharging,
            remaining: Some(26000),
            full: Some(52000),
            design: Some(57000),
            rate: Some(10400),
            voltage_mv: Some(11800),
            cycle_count: Some(312),
            model: "5B10W13930".into(),
            manufacturer: "SMP".into(),
            technology: "LiP".into(),
            ..Default::default()
        }
    }

    #[test]
    fn discharging() {
        let v = view(&Power { ac_online: Some(false), batteries: vec![laptop()] }).unwrap();
        assert_eq!(v.level, Some(50.));
        assert_eq!((v.direction, v.state.as_str(), v.time_left.as_str()), (-1, "On battery", "2 h 30 min left"));
        assert_eq!((v.ac.as_str(), v.ac_online, v.low), ("AC adapter unplugged", false, false));
        let facts: Vec<(&str, &str)> = v.facts.iter().map(|(l, v)| (l.as_str(), v.as_str())).collect();
        assert_eq!(
            facts,
            [
                ("Drawing", "10.4 W"),
                ("Energy", "26.0 of 52.0 Wh"),
                ("Health", "91 % · 57.0 Wh new"),
                ("Voltage", "11.80 V"),
                ("Cycles", "312"),
            ]
        );
        assert_eq!(v.identity, "Li-poly · SMP 5B10W13930");
    }

    #[test]
    fn charging_and_full() {
        let b = Battery { state: ChargeState::Charging, rate: Some(26000), ..laptop() };
        let v = view(&Power { ac_online: None, batteries: vec![b.clone()] }).unwrap();
        assert_eq!((v.direction, v.time_left.as_str(), v.ac_online, v.ac.as_str()), (1, "1 h until full", true, ""));
        assert_eq!(v.facts[0], ("Charging at".into(), "26.0 W".into()));

        let full = Battery { state: ChargeState::Full, rate: Some(0), remaining: Some(52000), ..laptop() };
        let v = view(&Power { ac_online: Some(true), batteries: vec![full] }).unwrap();
        assert_eq!((v.direction, v.state.as_str(), v.time_left.as_str()), (0, "Fully charged", ""));

        let low = Battery { remaining: Some(2600), rate: Some(13000), ..laptop() };
        let v = view(&Power { ac_online: Some(false), batteries: vec![low] }).unwrap();
        assert!(v.low);
        assert_eq!(v.time_left, "12 min left");
    }

    #[test]
    fn two_batteries_and_none() {
        let a = Battery { remaining: Some(52000), rate: Some(0), ..laptop() };
        let v = view(&Power { ac_online: Some(false), batteries: vec![a, laptop()] }).unwrap();
        assert_eq!(v.level, Some(75.));
        assert_eq!(v.facts[0], ("Batteries".into(), "2 · 100 % and 50 %".into()));
        assert_eq!(v.time_left, "7 h 30 min left");
        assert_eq!(view(&Power::default()), None);
    }

    #[test]
    fn only_smbios() {
        // SMBIOS's details without ACPI: no level.
        let b = Battery {
            design: Some(57000),
            design_voltage_mv: Some(11520),
            technology: "LION".into(),
            serial: "BEEF".into(),
            manufacture_date: "2021-03-14".into(),
            ..Default::default()
        };
        let v = view(&Power { ac_online: None, batteries: vec![b] }).unwrap();
        assert_eq!((v.level, v.state.as_str(), v.direction), (None, "Charge level unknown", 0));
        let facts: Vec<&str> = v.facts.iter().map(|f| f.0.as_str()).collect();
        assert_eq!(facts, ["Capacity", "Voltage"]);
        assert_eq!(v.identity, "Li-ion · Serial BEEF · Made 2021-03-14");
    }

    #[test]
    fn mah() {
        let b = Battery { mah: true, remaining: Some(2000), full: Some(4000), ..Default::default() };
        let v = view(&Power { ac_online: None, batteries: vec![b] }).unwrap();
        assert_eq!(v.facts, [("Energy".into(), "2000 of 4000 mAh".into())]);
        assert_eq!(duration(65), "1 h 05 min");
    }
}
