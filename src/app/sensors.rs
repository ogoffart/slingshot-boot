// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! The System screen's temperatures and fans: a gauge and a graph of the
//! last minutes for each temperature, a turning fan for each fan.

use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use slint::{ComponentHandle, Model, ModelRc, VecModel};

use super::{AppWindow, Backend, FanLine, SensorLine, System};
use crate::tr::trf;

/// Read every 5 s, like the battery, but not on the same tick; the graphs
/// show two minutes.
pub const EVERY_SECONDS: u32 = 5;
const HISTORY: usize = 24;
/// Without a critical temperature, where the gauge is full and "hot" starts.
const FULL_SCALE: f32 = 100.;
const HOT: f32 = 85.;

/// The graph's box, as the `.slint` Path's viewbox.
const GRAPH_W: f32 = 100.;
const GRAPH_H: f32 = 30.;

/// The temperatures read so far, and the rows shown (changed in place: the
/// gauges move from where they were).
#[derive(Default)]
pub struct Sensors {
    /// Per temperature, in the list's order: its name, its last readings.
    history: Vec<(String, VecDeque<f32>)>,
    temperatures: Rc<VecModel<SensorLine>>,
    fans: Rc<VecModel<FanLine>>,
    /// Just read (the screen shown): the next periodic reading is skipped.
    pub fresh: bool,
}

impl Sensors {
    /// Gives the System screen its rows, once.
    pub fn show(&self, ui: &AppWindow) {
        let system = ui.global::<System>();
        system.set_temperatures(ModelRc::from(self.temperatures.clone()));
        system.set_fans(ModelRc::from(self.fans.clone()));
    }
}

/// The graph of `values` (oldest first) in `GRAPH_W` × `GRAPH_H`: its line,
/// and the area under it. Five degrees at least from bottom to top, so
/// that noise stays flat.
fn sparkline(values: &[f32]) -> (String, String) {
    if values.is_empty() {
        return (String::new(), String::new());
    }
    let min = values.iter().copied().fold(f32::INFINITY, f32::min);
    let max = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let (low, high) = if max - min < 5. { ((min + max) / 2. - 2.5, (min + max) / 2. + 2.5) } else { (min, max) };
    // From the right edge: the newest reading at the end, a full graph after two minutes.
    let step = GRAPH_W / (HISTORY - 1) as f32;
    let x0 = GRAPH_W - step * (values.len() - 1) as f32;
    let mut line = String::with_capacity(values.len() * 12);
    for (i, v) in values.iter().enumerate() {
        let (x, y) = (x0 + step * i as f32, GRAPH_H - 2. - (v - low) / (high - low) * (GRAPH_H - 4.));
        let _ = write!(line, "{}{x:.1} {y:.1}", if i == 0 { "M" } else { " L" });
    }
    let mut area = line.clone();
    let _ = write!(area, " L{GRAPH_W:.1} {GRAPH_H:.1} L{x0:.1} {GRAPH_H:.1} Z");
    (line, area)
}

/// `model`'s rows as `rows`: changed in place when there are as many, and
/// only those that differ (each change draws its row again).
fn set_rows<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    if model.row_count() == rows.len() {
        for (i, row) in rows.into_iter().enumerate() {
            if model.row_data(i).as_ref() != Some(&row) {
                model.set_row_data(i, row);
            }
        }
    } else {
        model.set_vec(rows);
    }
}

/// Reads the sensors and shows them, keeping each temperature's history.
pub fn update(ui: &AppWindow, backend: &dyn Backend, state: &mut Sensors) {
    let sensors = backend.sensors();
    let degrees = |c: f32| trf("{} °C", &[&libm::roundf(c)]);
    state.history.resize_with(sensors.temperatures.len(), Default::default);
    let mut temperatures = Vec::with_capacity(sensors.temperatures.len());
    for (t, (name, values)) in sensors.temperatures.iter().zip(&mut state.history) {
        // Another sensor there now: its own graph.
        if *name != t.name {
            (*name, *values) = (t.name.clone(), VecDeque::new());
        }
        if values.len() == HISTORY {
            values.pop_front();
        }
        values.push_back(t.celsius);
        let (line, area) = sparkline(values.make_contiguous());
        temperatures.push(SensorLine {
            name: t.name.as_str().into(),
            value: degrees(t.celsius).into(),
            // From the degrees shown: it moves when they do.
            level: (libm::roundf(t.celsius) / t.critical.unwrap_or(FULL_SCALE)).clamp(0.02, 1.),
            hot: t.celsius >= t.critical.map_or(HOT, |c| c - 10.),
            line: line.into(),
            area: area.into(),
        });
    }
    let fans = sensors.fans.iter().map(|f| FanLine { name: f.name.as_str().into(), rpm: f.rpm as i32 }).collect();
    set_rows(&state.temperatures, temperatures);
    set_rows(&state.fans, fans);
    // The hottest, over the gauges.
    let hottest = sensors.temperatures.iter().map(|t| t.celsius).reduce(f32::max);
    ui.global::<System>().set_sensors_summary(hottest.map(degrees).unwrap_or_default().into());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph() {
        let (line, area) = sparkline(&[40., 50.]);
        // Two points on the right: the lowest at the bottom, the highest at the top.
        assert_eq!(line, "M95.7 28.0 L100.0 2.0");
        assert!(area.starts_with(&line) && area.ends_with("L100.0 30.0 L95.7 30.0 Z"));
        // A flat line in the middle, not across the whole height.
        let (line, _) = sparkline(&[45., 45., 45.]);
        assert!(line.ends_with("L100.0 15.0"), "{line}");
        assert_eq!(sparkline(&[]), (String::new(), String::new()));
    }
}
