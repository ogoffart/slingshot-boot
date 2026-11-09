// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! The desktop program: `slingshot` (or `slingshot config`) configures
//! Slingshot on this computer, in the boot menu's own UI; `slingshot
//! simulate` is the boot menu in a window, with made-up or this computer's
//! boot entries, for development and the UI tests.

mod install;
mod keymap;
mod machine;
mod simulator;

use std::path::PathBuf;

use crate::app::Settings;

const USAGE: &str = "\
Usage:
  slingshot [config] [options]   configure Slingshot on this computer, in its own UI
  slingshot simulate [options]   the boot menu in a window, with example entries
  slingshot keymap [-o FILE]     this computer's keyboard layout, for the boot menu

Options of `config`:
  --esp DIR           where the EFI System Partition is mounted (default: found)
  --config-dir DIR    configure DIR instead of EFI/slingshot on the ESP

Options of `simulate`:
  --this-machine      this computer's boot entries (Linux) instead of examples
  --mode WxH          simulated screen resolution (default 1280x800)
  --light             start with the light theme
  --splash            start with the splash screen
  --no-systemd-boot   example entries without systemd-boot
  --no-battery        a desktop computer: no battery on the System screen
  --hotplug           a USB stick plugged in and out every 4 seconds
  --hibernated        a Linux system hibernated (the one with kernel
                      6.17.1-simulated): it's resumed first
  --firmware-picker   pictures are chosen in the boot manager's own file
                      browser (from the simulated disks), as in the firmware
  --config-dir DIR    read slingshot.toml (and the pictures) from DIR
  --esp DIR           with --this-machine: where the ESP is mounted";

fn fail(message: &str) -> ! {
    eprintln!("{message}\n\n{USAGE}");
    std::process::exit(2);
}

pub fn main() {
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().map(String::as_str) == Some("keymap") {
        args.next();
        keymap::run(args);
        return;
    }
    if args.peek().map(String::as_str) == Some("simulate") {
        args.next();
        simulate(args);
        return;
    }
    // Configuring this computer, the default: `config` is optional.
    if args.peek().map(String::as_str) == Some("config") {
        args.next();
    }
    let (mut esp, mut config_dir) = (None::<PathBuf>, None::<PathBuf>);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--esp" => esp = Some(args.next().unwrap_or_else(|| fail("--esp needs a directory")).into()),
            "--config-dir" => config_dir = Some(args.next().unwrap_or_else(|| fail("--config-dir needs a directory")).into()),
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            "--this-machine" | "--mode" | "--light" | "--splash" | "--no-systemd-boot" | "--no-battery" | "--hibernated" | "--hotplug"
            | "--firmware-picker" => fail(&format!("{arg} is an option of the simulator: slingshot simulate {arg}")),
            other => fail(&format!("unknown option {other}")),
        }
    }
    configure(esp, config_dir);
}

/// `slingshot simulate`: the boot menu in a window.
fn simulate(mut args: impl Iterator<Item = String>) {
    let mut mode = (1280, 800);
    let mut light = false;
    let mut with_systemd_boot = true;
    let mut splash = false;
    let mut config_dir: Option<PathBuf> = None;
    let mut this_machine = false;
    let mut esp: Option<PathBuf> = None;
    let mut battery = true;
    let mut firmware_picker = false;
    let mut hibernated = false;
    let mut hotplug = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mode" => {
                let value = args.next().unwrap_or_default();
                mode = value
                    .split_once('x')
                    .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                    .unwrap_or_else(|| fail("--mode expects WIDTHxHEIGHT, e.g. 1920x1080"));
            }
            "--light" => light = true,
            "--no-systemd-boot" => with_systemd_boot = false,
            "--splash" => splash = true,
            "--no-battery" => battery = false,
            "--firmware-picker" => firmware_picker = true,
            "--hibernated" => hibernated = true,
            "--hotplug" => hotplug = true,
            "--config-dir" => config_dir = Some(args.next().unwrap_or_else(|| fail("--config-dir needs a directory")).into()),
            "--this-machine" => this_machine = true,
            "--esp" => esp = Some(args.next().unwrap_or_else(|| fail("--esp needs a directory")).into()),
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            other => fail(&format!("unknown option {other}")),
        }
    }

    let settings = (light || splash).then(|| Settings { theme: if light { 1 } else { 0 }, splash, ..Settings::default() });
    let mut backend = if this_machine {
        let Some(m) = machine::read(esp) else {
            eprintln!("This computer wasn't started with UEFI, or this isn't Linux: no boot entries to show.");
            std::process::exit(1);
        };
        for note in &m.notes {
            eprintln!("{note}");
        }
        // The installed slingshot.toml, unless another one is given.
        let config_dir = config_dir.or_else(|| m.esp.as_ref().map(|e| e.join(machine::SLINGSHOT_DIR)));
        simulator::SimBackend::from_data(m.boot, config_dir)
    } else {
        simulator::SimBackend::new(with_systemd_boot, config_dir)
    };
    if !battery {
        backend.battery = simulator::Battery::None;
    }
    backend.firmware_picker = firmware_picker;
    backend.hibernated.set(hibernated);
    backend.hotplug = hotplug.then(std::time::Instant::now);
    simulator::run(backend, simulator::Options { mode, settings });
}

/// `slingshot config`: the boot UI with this computer's entries, saving to
/// slingshot.toml on the ESP (or `config_dir`) and to the firmware's boot
/// order.
fn configure(esp: Option<PathBuf>, config_dir: Option<PathBuf>) {
    let esp_arg = esp.clone();
    let (boot, dir, real_firmware) = match machine::read(esp) {
        Some(m) => {
            for note in &m.notes {
                eprintln!("{note}");
            }
            let Some(dir) = config_dir.or_else(|| m.esp.as_ref().map(|e| e.join(machine::SLINGSHOT_DIR))) else {
                eprintln!("The EFI System Partition wasn't found: is it mounted? (--esp DIR says where)");
                std::process::exit(1);
            };
            (m.boot, dir, true)
        }
        // Not started with UEFI: only a folder, with the example entries.
        None => match config_dir {
            Some(dir) => {
                use crate::app::Backend;
                eprintln!("This computer wasn't started with UEFI: example entries, saving to {}", dir.display());
                (simulator::SimBackend::new(true, None).boot_data(), dir, false)
            }
            None => {
                eprintln!(
                    "This computer wasn't started with UEFI, or this isn't Linux: nothing to configure here.\n\
                     `slingshot simulate` shows the boot menu with example entries."
                );
                std::process::exit(1);
            }
        },
    };
    simulator::run(simulator::SimBackend::configure(boot, dir, real_firmware, esp_arg), simulator::Options { mode: (1280, 800), settings: None });
}
