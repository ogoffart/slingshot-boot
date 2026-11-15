// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Runs Slingshot's UI in a desktop window, with simulated firmware: made-up
//! boot entries (a systemd-boot with two entries, another OS, a USB stick,
//! network boot and a shell), or this computer's entries and files
//! (`slingshot simulate --this-machine`).
//! Starting an entry shows what would happen instead of restarting.
//!
//! The window uses Slint's software renderer, like the UEFI build, so it
//! looks the same.

use std::cell::RefCell;

use crate::parse::battery::{ChargeState, Power};
use std::rc::Rc;

use crate::app::{
    Backend, BootData, BootOption, Capabilities, DateTime, DirEntry, Hub, InfoCard, LoaderConfig, LoaderEntry, MemoryUsage, Saved, Settings,
    InstallDone, InstallStatus, StartResult, SystemInfo, Target, Volume, scale_factor_for,
};

const MODES: &[(usize, usize)] = &[(1280, 800), (1440, 900), (1600, 900), (1920, 1080), (1920, 1200), (2256, 1504)];

pub struct SimBackend {
    options: RefCell<Vec<BootOption>>,
    loader_entries: Option<LoaderConfig>,
    /// What "was found on the disks" (parse::detect, in the firmware).
    found: Vec<crate::app::FoundEntry>,
    order: RefCell<Vec<u16>>,
    settings: RefCell<Option<Settings>>,
    config_dir: Option<std::path::PathBuf>,
    /// The boot menu's footer: says what the entries are.
    pub footer: String,
    /// Seconds added to the real clock, after "setting" the time.
    clock_offset: std::cell::Cell<i64>,
    /// `slingshot config`: this computer's entries, and changes are saved
    /// (slingshot.toml on the ESP, the firmware's boot order).
    configure: bool,
    /// The boot entries are this computer's: the boot order is saved to
    /// the firmware.
    real_firmware: bool,
    badge: &'static str,
    pub battery: Battery,
    /// Pictures are chosen in the boot manager's own file browser, from the
    /// simulated disks, as in the firmware.
    pub firmware_picker: bool,
    /// A system hibernated (`--hibernated`), until something is started.
    pub hibernated: std::cell::Cell<bool>,
    /// `--power-off-after`: turn off after so many seconds without input,
    /// whatever the setting.
    pub power_off_after: Option<u32>,
    /// `--hotplug`: since when a USB stick comes and goes, every 4 seconds.
    pub hotplug: Option<std::time::Instant>,
    /// The stick is in, as the menu last saw it.
    stick_in: std::cell::Cell<bool>,
    /// The entry started last, for this session.
    last_started: RefCell<Option<String>>,
    /// The files "next to Slingshot" without `config_dir`: kept for the
    /// session (pictures added, slingshot.toml), nothing is written.
    memory: RefCell<std::collections::BTreeMap<String, Vec<u8>>>,
    /// This computer's file system (`--this-machine`): one disk instead of
    /// the made-up ones. Read for real, changed in `changes` only.
    pub root: Option<std::path::PathBuf>,
    /// What the Files screen changed on the disks, by volume and path
    /// (`edit_key`): kept for the session, nothing is written.
    changes: RefCell<std::collections::BTreeMap<(usize, String), Change>>,
    /// The Install screen, in `slingshot config`.
    installer: Option<super::install::Installer>,
    /// The made-up machine's encrypted disk: unlocked yet.
    #[cfg(feature = "luks")]
    disk_unlocked: Rc<std::cell::Cell<bool>>,
    /// Its file system shown in the file browser (`open_unlocked`).
    #[cfg(feature = "luks")]
    disk_open: std::cell::Cell<bool>,
    #[cfg(feature = "luks")]
    unlock_timer: Rc<slint::Timer>,
}

/// A change the Files screen made to a file or folder, kept for the session.
enum Change {
    /// Written: its path as given (the key may be lowercase), its contents.
    File { path: String, data: Vec<u8>, modified: DateTime },
    /// A folder made.
    Dir { path: String, modified: DateTime },
    Removed,
}

/// The made-up machine's encrypted disk: a real LUKS2 volume (the test one,
/// passphrase "slingshot"), so the simulator checks passphrases for real.
#[cfg(feature = "luks")]
const SIM_LUKS: &[u8] = include_bytes!("../../tests/luks2-argon2id.img");

#[cfg(feature = "luks")]
fn sim_luks_header() -> Option<crate::parse::luks::Header> {
    let size = crate::parse::luks::header_size(SIM_LUKS).ok()?;
    crate::parse::luks::parse(SIM_LUKS.get(..size as usize)?).ok()
}

/// `slingshot config` starts nothing: Slingshot does, at boot.
const NOTHING_STARTED: &str = "Nothing is started from here: Slingshot starts it at boot";

/// Where the System screen's battery card comes from.
#[derive(Clone, Copy, PartialEq)]
pub enum Battery {
    /// A laptop that charges for a minute, then runs on battery for a minute.
    Simulated,
    /// /sys/class/power_supply.
    ThisMachine,
    /// A desktop computer.
    None,
}

/// The simulated laptop battery, `seconds` after the epoch.
fn simulated_power(seconds: u64) -> Power {
    // Up from 40 % in a minute, then down for a minute.
    let t = (seconds % 120) as u32;
    let charging = t < 60;
    let tenths = if charging { 400 + t * 10 } else { 1000 - (t - 60) * 10 };
    let full = 50_160;
    Power {
        ac_online: Some(charging),
        batteries: vec![crate::parse::battery::Battery {
            state: if charging { ChargeState::Charging } else { ChargeState::Discharging },
            remaining: Some(full * tenths / 1000),
            full: Some(full),
            design: Some(57_000),
            rate: Some(if charging { 24_500 } else { 9_800 }),
            voltage_mv: Some(if charging { 12_600 } else { 11_900 }),
            design_voltage_mv: Some(11_520),
            cycle_count: Some(312),
            model: "Simulated battery".into(),
            technology: "Li-poly".into(),
            ..Default::default()
        }],
    }
}

fn option(number: u16, title: &str, subtitle: &str, kind: &'static str) -> BootOption {
    BootOption {
        number,
        title: title.into(),
        subtitle: subtitle.into(),
        kind,
        is_systemd_boot: title == "Linux Boot Manager",
        whole_device: matches!(kind, "usb" | "network"),
        absent: false,
    }
}

impl SimBackend {
    /// Simulates a machine with this boot configuration.
    pub fn from_data(boot: BootData, config_dir: Option<std::path::PathBuf>) -> Self {
        SimBackend {
            options: RefCell::new(boot.options),
            loader_entries: boot.loader_entries,
            found: boot.found,
            order: RefCell::new(boot.order),
            settings: RefCell::new(None),
            config_dir,
            clock_offset: Default::default(),
            footer: "Preview: this computer's boot entries; starting one does nothing".into(),
            configure: false,
            real_firmware: false,
            installer: None,
            badge: "Preview · this computer",
            #[cfg(feature = "luks")]
            disk_unlocked: Default::default(),
            #[cfg(feature = "luks")]
            disk_open: Default::default(),
            #[cfg(feature = "luks")]
            unlock_timer: Default::default(),
            battery: Battery::ThisMachine,
            firmware_picker: false,
            hibernated: Default::default(),
            hotplug: None,
            power_off_after: None,
            stick_in: Default::default(),
            last_started: Default::default(),
            memory: RefCell::default(),
            root: None,
            changes: RefCell::default(),
        }
    }

    /// `slingshot config`: configures this computer, with the boot UI.
    pub fn configure(boot: BootData, config_dir: std::path::PathBuf, real_firmware: bool, esp: Option<std::path::PathBuf>) -> Self {
        SimBackend {
            configure: true,
            real_firmware,
            installer: Some(super::install::Installer::new(esp)),
            badge: "Configuring this computer",
            footer: "Changes are saved to slingshot.toml on the EFI System Partition".into(),
            ..Self::from_data(boot, Some(config_dir))
        }
    }

    /// The made-up machine.
    pub fn new(with_systemd_boot: bool, config_dir: Option<std::path::PathBuf>) -> Self {
        let options = vec![
            // "Linux Boot Manager" is what systemd-boot calls its entry: it is
            // replaced by the simulated systemd-boot entries below.
            option(0x0001, "Linux Boot Manager", "Simulated disk", "linux"),
            option(0x0000, "Other simulated OS", "Simulated disk", "windows"),
            option(0x0003, "Simulated USB drive", "Simulated removable disk", "usb"),
            option(0x0004, "Simulated network boot", "Simulated network", "network"),
            option(0x0006, "Simulated shell", "Simulated firmware program", "shell"),
        ];
        let systemd_boot = with_systemd_boot.then(|| LoaderConfig {
            entries: vec![
                LoaderEntry {
                    id: "example.conf".into(),
                    title: "Simulated Linux".into(),
                    version: String::new(),
                    kind: "linux",
                    program: "\\vmlinuz-example".into(),
                    options: "initrd=\\initramfs-example.img root=LABEL=example rw".into(),
                    failed: false,
                },
                #[cfg(feature = "luks")]
                LoaderEntry {
                    id: "example-encrypted.conf".into(),
                    title: "Simulated Linux (encrypted)".into(),
                    version: String::new(),
                    kind: "linux",
                    program: "\\vmlinuz-example".into(),
                    options: format!(
                        "initrd=\\initramfs-example.img rd.luks.name={}=root root=/dev/mapper/root rw",
                        sim_luks_header().map(|h| h.uuid).unwrap_or_default()
                    ),
                    failed: false,
                },
                LoaderEntry {
                    id: "example-fallback.conf".into(),
                    title: "Simulated Linux (fallback)".into(),
                    version: String::new(),
                    kind: "linux",
                    program: "\\vmlinuz-example".into(),
                    options: "initrd=\\initramfs-example-fallback.img root=LABEL=example rw".into(),
                    failed: false,
                },
            ],
            default: 0,
            one_shot: None,
            saved: false,
            last_booted: None,
            editor_off: false,
        });
        // 0x0002 is the simulator itself: in BootOrder but not shown.
        let order = vec![0x0001, 0x0002, 0x0000, 0x0003, 0x0004, 0x0006];
        let found = vec![crate::app::FoundEntry {
            title: "Simulated Linux".into(),
            subtitle: crate::tr::trf("Kernel {}", &[&"6.17.1-simulated"]),
            kind: "linux",
            fs: Vec::new(),
            program: "\\boot\\vmlinuz-6.17.1-simulated".into(),
            options: "initrd=\\boot\\initramfs-6.17.1-simulated.img root=PARTUUID=simulated ro".into(),
        }];
        // `boot_data` says the rest (firmware setup, its timeout).
        let boot = BootData { options, order, loader_entries: systemd_boot, found, ..Default::default() };
        SimBackend {
            footer: "Simulator: these boot entries are examples, nothing is real".into(),
            badge: "Simulator · nothing is real",
            battery: Battery::Simulated,
            ..Self::from_data(boot, config_dir)
        }
    }

    /// Where `changes` keeps a path (without a final `\`): the made-up
    /// disks are FAT, case-blind; this computer's aren't.
    fn edit_key(&self, volume: usize, path: &str) -> (usize, String) {
        let path = path.trim_end_matches('\\');
        (volume, if self.root.is_some() { path.into() } else { path.to_ascii_lowercase() })
    }

    /// The name of disk `volume` if its files can be changed.
    fn writable(&self, volume: usize) -> Result<String, String> {
        self.volumes().into_iter().nth(volume).filter(|v| v.writable).map(|v| v.name).ok_or_else(|| String::from("read-only"))
    }

    fn now(&self) -> DateTime {
        self.date_time().unwrap_or(DateTime { year: 2026, month: 1, day: 1, hour: 0, minute: 0 })
    }

    /// `entries` of folder `path` on disk `volume`, with the session's changes.
    fn with_changes(&self, volume: usize, path: &str, mut entries: Vec<DirEntry>) -> Vec<DirEntry> {
        let changes = self.changes.borrow();
        let folder = self.edit_key(volume, path);
        let key = |name: &str| self.edit_key(volume, &format!("{}\\{name}", folder.1));
        entries.retain(|e| !matches!(changes.get(&key(&e.name)), Some(Change::Removed)));
        for e in &mut entries {
            if let Some(Change::File { path, data, modified }) = changes.get(&key(&e.name)) {
                (e.size, e.modified) = (data.len() as u64, Some(*modified));
                // Its name as written last (only its case may differ).
                e.name = path.rsplit_once('\\').map_or(path.as_str(), |(_, n)| n).into();
            }
        }
        for ((v, k), change) in changes.iter() {
            let (Some((parent, _)), true) = (k.rsplit_once('\\'), *v == volume) else { continue };
            let (path, is_dir, size, modified) = match change {
                Change::File { path, data, modified } => (path, false, data.len() as u64, *modified),
                Change::Dir { path, modified } => (path, true, 0, *modified),
                Change::Removed => continue,
            };
            let name = path.rsplit_once('\\').map_or(path.as_str(), |(_, n)| n);
            if parent == folder.1 && !entries.iter().any(|e| key(&e.name).1 == *k) {
                entries.push(DirEntry { name: name.into(), is_dir, size, modified: Some(modified), ..Default::default() });
            }
        }
        entries
    }

    /// The encrypted disk's file system is in the file browser.
    fn disk_open(&self) -> bool {
        #[cfg(feature = "luks")]
        return self.disk_open.get();
        #[cfg(not(feature = "luks"))]
        false
    }

    fn describe(&self, target: &Target) -> String {
        match target {
            Target::Firmware(n) => {
                let options = self.options.borrow();
                let title = options.iter().find(|o| o.number == *n).map(|o| o.title.as_str()).unwrap_or("?");
                format!("Simulator: would set BootNext={n:04X} ({title}) and restart")
            }
            Target::LoaderEntry { systemd_boot, id, program, options } => match systemd_boot {
                Some(b) => format!("Simulator: would start {program} {options} ({id}; else through Boot{b:04X})"),
                None => format!("Simulator: would start {program} {options} ({id})"),
            },
            Target::Program { path, options } | Target::Found { program: path, options, .. } => {
                format!("Simulator: would start {path} {options}")
            }
            Target::File { volume, path } => {
                let name = self.volumes().get(*volume).map_or_else(|| String::from("?"), |v| v.name.clone());
                format!("Simulator: would start {path} on {name}")
            }
        }
    }
}

/// The simulator's file systems, for "Start a file…": a name, what it is,
/// its icon, and its files (folders end with `\`).
/// The last one is the encrypted disk's file system, once it's unlocked
/// in the file browser.
const SIM_VOLUMES: [(&str, &str, &str, &[&str]); 3] = [
    (
        "EFI System Partition",
        "Simulated NVMe disk · partition 1",
        "disk",
        &[
            "\\EFI\\",
            "\\EFI\\BOOT\\",
            "\\EFI\\BOOT\\BOOTX64.EFI",
            "\\EFI\\systemd\\",
            "\\EFI\\systemd\\systemd-bootx64.efi",
            "\\EFI\\Microsoft\\",
            "\\EFI\\Microsoft\\Boot\\",
            "\\EFI\\Microsoft\\Boot\\bootmgfw.efi",
            "\\EFI\\Microsoft\\Boot\\BCD",
            "\\EFI\\tools\\",
            "\\EFI\\tools\\shellx64.efi",
            "\\EFI\\tools\\memtest86.efi",
            "\\EFI\\tools\\changelog.txt",
            "\\loader\\",
            "\\loader\\loader.conf",
            "\\vmlinuz-linux",
        ],
    ),
    (
        "MY STICK",
        "Simulated USB · removable",
        "usb",
        &["\\EFI\\", "\\EFI\\BOOT\\", "\\EFI\\BOOT\\BOOTX64.EFI", "\\Pictures\\", "\\Pictures\\lake.jpg", "\\README.txt"],
    ),
    (
        "Simulated NVMe disk · partition 3",
        "ext4 · 400 GB · unlocked",
        "disk",
        &[
            "\\boot\\",
            "\\boot\\EFI\\",
            "\\boot\\EFI\\Linux\\",
            "\\boot\\EFI\\Linux\\arch-linux.efi",
            "\\boot\\vmlinuz-linux",
            "\\etc\\",
            "\\etc\\os-release",
            "\\home\\",
            "\\home\\me\\",
            "\\home\\me\\notes.txt",
            "\\home\\me\\Pictures\\",
            "\\home\\me\\Pictures\\mountains.png",
            "\\usr\\",
        ],
    ),
];

/// What the simulator's files hold: some text, pictures, and bytes for the
/// rest (a PE header for EFI programs).
fn sim_file(volume: usize, path: &str) -> Option<Vec<u8>> {
    let text = |t: &str| Some(t.as_bytes().to_vec());
    match (volume, path.to_ascii_lowercase().as_str()) {
        (0, "\\loader\\loader.conf") => text("default arch.conf\ntimeout 3\nconsole-mode max\neditor no\n"),
        // Taller than 32767 px in the editor: Slint once overflowed there.
        (0, "\\efi\\tools\\changelog.txt") => Some((1..=3000).map(|i| format!("{i}. A change\n")).collect::<String>().into_bytes()),
        (1, "\\readme.txt") => text("This stick starts Slingshot's simulator.\n\nIts EFI\\BOOT folder holds the program the firmware starts,\nand Pictures a photo to try as a wallpaper.\n"),
        (1, "\\pictures\\lake.jpg") => Some(include_bytes!("../../assets/wallpapers/lake-mystery.jpg").to_vec()),
        (2, "\\etc\\os-release") => text("NAME=\"Arch Linux\"\nPRETTY_NAME=\"Arch Linux\"\nID=arch\nBUILD_ID=rolling\nHOME_URL=\"https://archlinux.org/\"\n"),
        (2, "\\home\\me\\notes.txt") => text("Things to try in Slingshot:\n- the file explorer\n- the splash screen\n"),
        (2, "\\home\\me\\pictures\\mountains.png") => Some(include_bytes!("../../assets/os/arch.png").to_vec()),
        (v, p) if SIM_VOLUMES.get(v)?.3.iter().any(|f| f.eq_ignore_ascii_case(p)) && !p.ends_with('\\') => {
            let mut bytes: Vec<u8> = (0..4096u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
            if p.ends_with(".efi") {
                bytes[..2].copy_from_slice(b"MZ");
            }
            Some(bytes)
        }
        _ => None,
    }
}

/// Seconds since 1970 of `t`.
fn utc_seconds(t: std::time::SystemTime) -> Option<i64> {
    Some(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64)
}

/// The UTC date and time of `secs` since 1970, without extra dependencies.
fn utc(secs: i64) -> DateTime {
    let days = secs.div_euclid(86400);
    let (h, m) = (secs.rem_euclid(86400) / 3600, secs.rem_euclid(3600) / 60);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    DateTime { year: year as u16, month: month as u8, day: d as u8, hour: h as u8, minute: m as u8 }
}

/// `path` of the one disk, `root`, on this computer.
fn root_path(root: &std::path::Path, volume: usize, path: &str) -> Result<std::path::PathBuf, String> {
    if volume != 0 {
        return Err(String::from("no such volume"));
    }
    super::machine::disk_path(root, path).ok_or_else(|| format!("{path}: outside the disk"))
}

/// The USB stick of `--hotplug`.
fn simulated_stick() -> crate::app::FoundEntry {
    crate::app::FoundEntry {
        title: "SANDISK".into(),
        subtitle: "USB · removable".into(),
        kind: "usb",
        fs: Vec::new(),
        program: "\\EFI\\BOOT\\BOOTX64.EFI".into(),
        options: String::new(),
    }
}

impl Backend for SimBackend {
    fn media_changed(&self) -> bool {
        let Some(since) = self.hotplug else { return false };
        let inserted = (since.elapsed().as_secs() / 4) % 2 == 1;
        let changed = inserted != self.stick_in.get();
        self.stick_in.set(inserted);
        changed
    }

    fn boot_data(&self) -> BootData {
        let order = self.order.borrow().clone();
        // Visible entries in BootOrder order.
        let all = self.options.borrow();
        let options = order.iter().filter_map(|n| all.iter().find(|o| o.number == *n).cloned()).collect();
        BootData {
            options,
            order,
            loader_entries: self.loader_entries.clone(),
            found: self.found.iter().cloned().chain(self.stick_in.get().then(simulated_stick)).collect(),
            firmware_setup_supported: true,
            firmware_timeout: Some(5),
            secure_boot: false,
        }
    }

    fn system_info(&self, _size: (usize, usize), _modes: &[(usize, usize)]) -> SystemInfo {
        let card = InfoCard::new;
        SystemInfo {
            cards: vec![
                card(&crate::tr::tr("Processor"), "Simulated processor", "Simulated: 8 cores · 16 threads", "cpu", false),
                card(&crate::tr::tr("Memory"), "32 GB (simulated)", "Simulated: 2 × 16 GB", "memory", false),
                card(&crate::tr::tr("System"), "Simulated computer", "Slingshot desktop simulator", "computer", false),
                card(&crate::tr::tr("Firmware"), "Simulated firmware", "No real UEFI: nothing is read or written", "firmware", false),
                card(&crate::tr::tr("Secure Boot"), "Off (simulated)", "Simulated value", "shield-off", false),
            ],
            memory: Some(MemoryUsage {
                usable: 31 << 30 | 600 << 20,
                runtime: 40 << 20,
                acpi: 12 << 20,
                reserved: 340 << 20,
            }),
            firmware_line: self.footer.clone(),
        }
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            configure: self.configure,
            simulation: true,
            picture_dialog: !self.firmware_picker,
            install: self.installer.is_some(),
        }
    }

    fn power_off(&self, shut_down: bool) -> StartResult {
        StartResult::Simulated(
            if shut_down { "Simulated: the computer would turn off" } else { "Simulated: the computer would restart" }.into(),
        )
    }

    fn power(&self) -> Power {
        match self.battery {
            Battery::Simulated => simulated_power(
                utc_seconds(std::time::SystemTime::now()).map_or(0, |s| s as u64),
            ),
            Battery::ThisMachine => super::machine::power(),
            Battery::None => Power::default(),
        }
    }

    fn idle_off_override(&self) -> Option<u32> {
        self.power_off_after
    }

    fn sensors(&self) -> crate::parse::sensors::Sensors {
        use crate::parse::sensors::{Fan, Sensors, Temperature};
        if matches!(self.battery, Battery::ThisMachine) {
            return super::machine::sensors();
        }
        // A processor that warms up and cools down, and its fan.
        let wave = utc_seconds(std::time::SystemTime::now()).map_or(0, |s| s.rem_euclid(20)) as f32;
        let cpu = 46. + (wave - 10.).abs() / 2.;
        Sensors {
            // As many as a laptop has: the list scrolls.
            temperatures: vec![
                Temperature { name: "CPU".into(), celsius: cpu, critical: Some(105.) },
                Temperature { name: "GPU".into(), celsius: cpu - 6., critical: Some(100.) },
                Temperature { name: "NVME".into(), celsius: 41., critical: Some(85.) },
                Temperature { name: "TZ01".into(), celsius: 38.5, critical: None },
            ],
            fans: vec![
                Fan { name: "FAN0".into(), rpm: 1800 + (cpu as u32 - 46) * 100 },
                Fan { name: "FAN1".into(), rpm: 1500 },
            ],
        }
    }

    fn time_zone(&self) -> &str {
        "UTC"
    }

    fn date_time(&self) -> Option<DateTime> {
        // Shifted by what was "set".
        Some(utc(utc_seconds(std::time::SystemTime::now())? + self.clock_offset.get()))
    }

    fn set_date_time(&self, t: DateTime) -> Result<(), String> {
        // Days from civil (Howard Hinnant's algorithm), to keep an offset.
        let (y, m) = (t.year as i64 - i64::from(t.month <= 2), t.month as i64);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + t.day as i64 - 1;
        let days = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468;
        let target = days * 86400 + t.hour as i64 * 3600 + t.minute as i64 * 60;
        let now = utc_seconds(std::time::SystemTime::now()).ok_or("the clock is before 1970")?;
        self.clock_offset.set(target - now);
        println!("simulator: firmware clock set to {t:?}");
        Ok(())
    }

    fn load_settings(&self) -> Option<Saved> {
        self.settings.borrow().map(|settings| Saved { settings, base: None })
    }

    fn badge(&self) -> &str {
        self.badge
    }

    fn read_volume_file(&self, volume: usize, path: &str) -> Result<Vec<u8>, String> {
        match self.changes.borrow().get(&self.edit_key(volume, path)) {
            Some(Change::File { data, .. }) => return Ok(data.clone()),
            Some(_) => return Err(String::from("no such file")),
            None => {}
        }
        if let Some(root) = &self.root {
            use std::io::Read;
            let file = root_path(root, volume, path)?;
            let cannot = |e: std::io::Error| format!("cannot read {}: {e}", file.display());
            let mut f = std::fs::File::open(&file).map_err(cannot)?;
            let meta = f.metadata().map_err(cannot)?;
            // Not /dev/zero or a FIFO, which never end. The size is only a
            // backstop: the Files tab asks for less (`OPEN_MAX`).
            if !meta.is_file() || meta.len() > 256 << 20 {
                return Err(format!("{}: not a file of at most 256 MB", file.display()));
            }
            let mut data = Vec::with_capacity(meta.len() as usize);
            f.read_to_end(&mut data).map_err(cannot)?;
            return Ok(data);
        }
        sim_file(volume, path).ok_or_else(|| String::from("no such file"))
    }

    fn write_volume_file(&self, volume: usize, path: &str, data: &[u8]) -> Result<(), String> {
        let name = self.writable(volume)?;
        println!("Simulator: would write {} bytes to {path} on {name} (kept for this session)", data.len());
        let change = Change::File { path: path.into(), data: data.to_vec(), modified: self.now() };
        self.changes.borrow_mut().insert(self.edit_key(volume, path), change);
        Ok(())
    }

    fn create_dir(&self, volume: usize, path: &str) -> Result<(), String> {
        let name = self.writable(volume)?;
        println!("Simulator: would make the folder {path} on {name} (kept for this session)");
        let change = Change::Dir { path: path.trim_end_matches('\\').into(), modified: self.now() };
        self.changes.borrow_mut().insert(self.edit_key(volume, path), change);
        Ok(())
    }

    fn delete(&self, volume: usize, path: &str) -> Result<(), String> {
        let name = self.writable(volume)?;
        if self.list_dir(volume, path).is_ok_and(|l| !l.is_empty()) {
            return Err(String::from("the folder isn't empty"));
        }
        println!("Simulator: would delete {path} on {name} (for this session)");
        self.changes.borrow_mut().insert(self.edit_key(volume, path), Change::Removed);
        Ok(())
    }

    fn rename(&self, volume: usize, path: &str, new_name: &str) -> Result<(), String> {
        let name = self.writable(volume)?;
        let folder = path.trim_end_matches('\\').rsplit_once('\\').map_or("", |(f, _)| f);
        let new_path = format!("{folder}\\{new_name}");
        let change = match self.read_volume_file(volume, path) {
            Ok(data) => Change::File { path: new_path.clone(), data, modified: self.now() },
            // A folder: only an empty one (its files would have to move too).
            Err(_) if self.list_dir(volume, path).is_ok_and(|l| l.is_empty()) => Change::Dir { path: new_path.clone(), modified: self.now() },
            Err(_) => return Err(String::from("the simulator renames only files and empty folders")),
        };
        println!("Simulator: would rename {path} to {new_name} on {name} (for this session)");
        let mut changes = self.changes.borrow_mut();
        // Only the case changing (FAT): the same key, nothing to remove.
        let (old, new) = (self.edit_key(volume, path), self.edit_key(volume, &new_path));
        if old != new {
            changes.insert(old, Change::Removed);
        }
        changes.insert(new, change);
        Ok(())
    }

    fn volume_space(&self, volume: usize) -> Option<(u64, u64)> {
        if let Some(root) = &self.root {
            return super::machine::space(&root_path(root, volume, "\\").ok()?);
        }
        [(412 << 20, 1 << 30), (14_900 << 20, 32_000 << 20), (250_000_000_000, 400_000_000_000)].get(volume).copied()
    }

    fn add_picture(&self) -> Result<Option<String>, String> {
        let dialog = rfd::FileDialog::new().set_title("Add a picture").add_filter("Picture", &["png", "PNG", "jpg", "JPG", "jpeg", "JPEG"]);
        let Some(source) = dialog.pick_file() else { return Ok(None) };
        let name = source.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or("no file name")?;
        let Some(dir) = &self.config_dir else {
            // Only for this session.
            let data = std::fs::read(&source).map_err(|e| format!("cannot read {}: {e}", source.display()))?;
            self.memory.borrow_mut().insert(name.clone(), data);
            return Ok(Some(name));
        };
        let target = dir.join(&name);
        if source != target {
            let data = std::fs::read(&source).map_err(|e| format!("cannot read {}: {e}", source.display()))?;
            super::machine::write_file(&target, &data)?;
        }
        Ok(Some(name))
    }

    fn install_status(&self, done: Box<dyn FnOnce(InstallStatus)>) {
        if let Some(i) = &self.installer {
            i.status(done);
        }
    }

    fn install_step(&self, action: usize, plan: bool, done: InstallDone) {
        match &self.installer {
            Some(i) => i.step(action, plan, done),
            None => done(Err(String::from("not here"))),
        }
    }

    fn choose_install_program(&self) -> bool {
        self.installer.as_ref().is_some_and(|i| i.choose_program())
    }

    fn restart(&self) -> Result<(), String> {
        super::install::restart()
    }

    fn save_settings(&self, saved: &Saved) -> Result<(), String> {
        println!("simulator: settings saved: {saved:?}");
        *self.settings.borrow_mut() = Some(saved.settings);
        Ok(())
    }

    fn reset_settings(&self) -> Result<(), String> {
        *self.settings.borrow_mut() = None;
        Ok(())
    }

    fn set_boot_order(&self, order: &[u16]) -> Result<(), String> {
        if self.real_firmware {
            super::machine::set_boot_order(order)?;
        } else {
            println!("simulator: BootOrder = {order:04X?}");
        }
        *self.order.borrow_mut() = order.to_vec();
        Ok(())
    }

    fn write_file(&self, name: &str, data: &[u8]) -> Result<(), String> {
        if let (true, Some(dir)) = (self.configure, &self.config_dir) {
            return super::machine::write_file(&dir.join(name), data);
        }
        let Some(dir) = &self.config_dir else {
            println!("simulator: would write {name} (kept for this session)");
            self.memory.borrow_mut().insert(String::from(name), data.to_vec());
            return Ok(());
        };
        std::fs::write(dir.join(name), data).map_err(|e| e.to_string())
    }

    fn volumes(&self) -> Vec<Volume> {
        if let Some(root) = &self.root {
            let detail = root.display().to_string();
            return vec![Volume { name: "This computer".into(), detail, kind: "disk", format: "", unlock: None, writable: true }];
        }
        let open = self.disk_open();
        let shown = if open { SIM_VOLUMES.len() } else { SIM_VOLUMES.len() - 1 };
        SIM_VOLUMES[..shown]
            .iter()
            .enumerate()
            // The unlocked disk is read-only, as in the firmware.
            .map(|(i, (name, detail, kind, _))| Volume { name: (*name).into(), detail: (*detail).into(), kind, format: "", unlock: None, writable: i < 2 })
            .collect()
    }

    fn unreadable_volumes(&self) -> Vec<Volume> {
        if self.root.is_some() {
            return Vec::new();
        }
        let mut volumes = alloc::vec![Volume {
            name: "Simulated USB SSD · partition 1".into(),
            detail: "ext4 · 1.0 TB".into(),
            kind: "usb",
            format: "ext4",
            unlock: None,
            writable: false,
        }];
        let open = self.disk_open();
        if !open {
            volumes.push(Volume {
                name: "Simulated NVMe disk · partition 3".into(),
                detail: "LUKS · 400 GB · the passphrase is \"slingshot\"".into(),
                kind: "disk",
                format: "LUKS",
                unlock: cfg!(feature = "luks").then_some(0),
                writable: false,
            });
        }
        volumes
    }

    fn list_dir(&self, volume: usize, path: &str) -> Result<Vec<DirEntry>, String> {
        if let Some(root) = &self.root {
            let dir = root_path(root, volume, path)?;
            let entries = std::fs::read_dir(&dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
            let entry = |e: std::fs::DirEntry| {
                // Links are followed: /bin is one to a folder.
                let link = e.file_type().ok()?.is_symlink();
                let meta = if link { std::fs::metadata(e.path()) } else { e.metadata() }.ok()?;
                let name = e.file_name().to_string_lossy().into_owned();
                let is_dir = meta.is_dir();
                Some(DirEntry {
                    is_dir,
                    size: if is_dir { 0 } else { meta.len() },
                    modified: meta.modified().ok().and_then(utc_seconds).map(utc),
                    read_only: meta.permissions().readonly(),
                    hidden: name.starts_with('.'),
                    name,
                })
            };
            return Ok(self.with_changes(volume, path, entries.filter_map(Result::ok).filter_map(entry).collect()));
        }
        let (_, _, _, files) = SIM_VOLUMES.get(volume).ok_or("no such volume")?;
        let dir = if path.ends_with('\\') { String::from(path) } else { format!("{path}\\") };
        let entries = files.iter().filter_map(|f| {
            let rest = f.strip_prefix(dir.as_str())?;
            let (name, is_dir) = match rest.strip_suffix('\\') {
                Some(name) => (name, true),
                None => (rest, false),
            };
            let size = if is_dir { 0 } else { self.read_volume_file(volume, f).map_or(1_234_567, |d| d.len() as u64) };
            let modified = Some(crate::app::DateTime { year: 2026, month: 11, day: 9, hour: 14, minute: 22 });
            (!name.is_empty() && !name.contains('\\')).then(|| DirEntry { name: name.into(), is_dir, size, modified, ..Default::default() })
        });
        Ok(self.with_changes(volume, path, entries.collect()))
    }

    fn add_boot_entry(&self, volume: usize, path: &str, title: &str) -> Result<u16, String> {
        let mut options = self.options.borrow_mut();
        let mut order = self.order.borrow_mut();
        let number = (0..=u16::MAX).find(|n| !options.iter().any(|o| o.number == *n) && !order.contains(n)).ok_or("no free number")?;
        let kind = self.volumes().get(volume).map_or("disk", |v| v.kind);
        options.push(option(number, title, "Added in the simulator", if kind == "usb" { "usb" } else { "app" }));
        order.push(number);
        println!("Simulator: would add Boot{number:04X} \"{title}\" for {path}");
        Ok(number)
    }

    fn list_pictures(&self) -> Vec<String> {
        if self.config_dir.is_none() {
            return crate::app::picture_names(self.memory.borrow().keys().cloned());
        }
        let Some(entries) = self.config_dir.as_ref().and_then(|d| std::fs::read_dir(d).ok()) else { return Vec::new() };
        let files = entries.filter_map(Result::ok).filter(|e| e.file_type().is_ok_and(|t| t.is_file()));
        crate::app::picture_names(files.map(|e| e.file_name().to_string_lossy().into_owned()))
    }

    fn read_file(&self, name: &str) -> Option<Vec<u8>> {
        match &self.config_dir {
            Some(dir) => std::fs::read(dir.join(name)).ok(),
            None => self.memory.borrow().get(name).cloned(),
        }
    }

    #[cfg(feature = "luks")]
    fn locked_volumes(&self, target: &Target) -> Vec<crate::app::LockedVolume> {
        let Target::LoaderEntry { options, .. } = target else { return Vec::new() };
        let Some(header) = sim_luks_header() else { return Vec::new() };
        if self.configure || self.disk_unlocked.get() {
            return Vec::new();
        }
        crate::parse::handoff::wanted_volumes(options)
            .into_iter()
            .filter(|w| w.matches(&header.uuid, None, Some(2)))
            .map(|w| crate::app::LockedVolume {
                id: 0,
                name: w.name,
                detail: String::from("Simulated disk · partition 2 · the passphrase is \"slingshot\""),
            })
            .collect()
    }

    #[cfg(feature = "luks")]
    fn open_unlocked(&self, _id: usize) -> Result<(), String> {
        if !self.disk_unlocked.get() {
            return Err(String::from("This disk is locked."));
        }
        self.disk_open.set(true);
        Ok(())
    }

    #[cfg(feature = "luks")]
    fn forget_unlocked(&self) {
        // Also a check under way: its result is dropped.
        self.unlock_timer.stop();
        self.disk_unlocked.set(false);
    }

    #[cfg(feature = "luks")]
    fn unlock(&self, _volume: usize, passphrase: crate::app::Secret, done: crate::app::UnlockDone) {
        use crate::app::UnlockResult;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let result = match sim_luks_header() {
                Some(header) => {
                    let read = |offset: u64, len: usize| SIM_LUKS.get(offset as usize..offset as usize + len).map(<[u8]>::to_vec);
                    match crate::parse::luks::unlock(&header, &passphrase.0, read) {
                        Ok(_) => UnlockResult::Unlocked,
                        Err(crate::parse::luks::Error::WrongPassphrase) => UnlockResult::WrongPassphrase,
                        Err(crate::parse::luks::Error::Cancelled) => UnlockResult::Cancelled,
                        Err(e) => UnlockResult::Failed(format!("{e:?}")),
                    }
                }
                None => UnlockResult::Failed(String::from("no disk")),
            };
            // The test volume derives fast; a real one takes a second or two.
            std::thread::sleep(std::time::Duration::from_millis(1500).saturating_sub(started.elapsed()));
            let _ = tx.send(result);
        });
        let mut done = Some(done);
        let unlocked = self.disk_unlocked.clone();
        let timer = Rc::downgrade(&self.unlock_timer);
        self.unlock_timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(50), move || {
            if let Ok(result) = rx.try_recv()
                && let Some(done) = done.take()
            {
                if let Some(t) = timer.upgrade() {
                    t.stop();
                }
                if result == UnlockResult::Unlocked {
                    unlocked.set(true);
                }
                done(result);
            }
        });
    }

    fn language(&self) -> Option<String> {
        ["LC_ALL", "LC_MESSAGES", "LANG"].iter().find_map(|v| std::env::var(v).ok().filter(|l| !l.is_empty()))
    }

    fn hibernated(&self) -> Option<crate::parse::hibernate::Hibernated> {
        self.hibernated.get().then(|| crate::parse::hibernate::Hibernated {
            kernel_version: String::from("6.17.1-simulated"),
            os_id: String::from("example"),
        })
    }

    fn last_started(&self) -> Option<String> {
        self.last_started.borrow().clone()
    }

    fn set_last_started(&self, key: &str, _loader_entry: Option<&str>) {
        *self.last_started.borrow_mut() = Some(key.into());
    }

    fn start(&self, target: &Target) -> StartResult {
        // The system that hibernated resumes (or loses its session).
        self.hibernated.set(false);
        if self.configure {
            return StartResult::Simulated(String::from(NOTHING_STARTED));
        }
        #[allow(unused_mut)]
        let mut message = self.describe(target);
        #[cfg(feature = "luks")]
        if self.disk_unlocked.replace(false) {
            message.push_str(", with the encrypted disk unlocked");
        }
        println!("{message}");
        StartResult::Simulated(message)
    }

    fn reboot_to_firmware_setup(&self) -> StartResult {
        if self.configure {
            return StartResult::Simulated(String::from(NOTHING_STARTED));
        }
        let message = String::from("Simulator: would set OsIndications (boot to firmware UI) and restart");
        println!("{message}");
        StartResult::Simulated(message)
    }

    fn request_mode(&self, hub: &Hub, window: &slint::Window, width: usize, height: usize) {
        // Size the window like the firmware screen would be laid out: the
        // logical size the UEFI build uses at this resolution.
        let scale = scale_factor_for(width, height);
        window.set_size(slint::LogicalSize::new(width as f32 / scale, height as f32 / scale));
        hub.mode_changed(width, height);
    }

    fn ui_created(&self, hub: &Hub, window: &slint::Window) {
        let (w, h) = hub.current_size.get();
        self.request_mode(hub, window, w, h);
    }
}

pub struct Options {
    pub mode: (usize, usize),
    pub settings: Option<Settings>,
}

pub fn run(backend: SimBackend, options: Options) {
    let hub = Rc::new(Hub::default());
    let mut modes = MODES.to_vec();
    if !modes.contains(&options.mode) {
        modes.push(options.mode);
        modes.sort_by_key(|(w, h)| (w * h, *w));
    }
    *hub.modes.borrow_mut() = modes;
    hub.current_size.set(options.mode);
    *hub.pointer_status.borrow_mut() = "Simulator: the data on this screen is made up".into();
    if let Some(s) = options.settings {
        *backend.settings.borrow_mut() = Some(s);
    }
    if let Err(e) = crate::app::run(Rc::new(backend), hub) {
        eprintln!("cannot run the UI: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_machines_disks_are_read_for_real() {
        let dir = std::env::temp_dir().join(format!("slingshot-disk-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("EFI/BOOT")).unwrap();
        std::fs::write(dir.join("EFI/BOOT/BOOTX64.EFI"), b"MZ program").unwrap();
        std::os::unix::fs::symlink("EFI", dir.join("link")).unwrap();
        std::fs::write(dir.join("README"), b"upper").unwrap();
        std::fs::write(dir.join("readme"), b"lower").unwrap();
        let mut backend = SimBackend::new(false, None);
        backend.root = Some(dir.clone());

        assert_eq!(backend.volumes().iter().map(|v| v.name.as_str()).collect::<Vec<_>>(), ["This computer"]);
        assert!(backend.unreadable_volumes().is_empty());
        let mut root = backend.list_dir(0, "\\").unwrap();
        root.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<_> = root.iter().map(|e| (e.name.as_str(), e.is_dir)).collect();
        assert_eq!(names, [("EFI", true), ("README", false), ("link", true), ("readme", false)], "a link to a folder is one");
        let boot = backend.list_dir(0, "\\EFI\\BOOT").unwrap();
        assert_eq!((boot[0].name.as_str(), boot[0].size), ("BOOTX64.EFI", 10));
        assert!(boot[0].modified.is_some());
        assert_eq!(backend.read_volume_file(0, "\\EFI\\BOOT\\BOOTX64.EFI").unwrap(), b"MZ program");
        assert!(backend.read_volume_file(0, "\\..\\..\\etc\\passwd").is_err());
        assert!(backend.read_volume_file(0, "\\EFI").is_err(), "a folder");
        assert!(backend.volume_space(0).is_some_and(|(free, size)| free <= size && size > 0));
        // Edits stay in the session: the disk isn't written.
        backend.write_volume_file(0, "\\EFI\\BOOT\\BOOTX64.EFI", b"changed").unwrap();
        assert_eq!(backend.read_volume_file(0, "\\EFI\\BOOT\\BOOTX64.EFI").unwrap(), b"changed");
        assert_eq!(std::fs::read(dir.join("EFI/BOOT/BOOTX64.EFI")).unwrap(), b"MZ program");
        backend.write_volume_file(0, "\\README", b"edited").unwrap();
        assert_eq!(backend.read_volume_file(0, "\\readme").unwrap(), b"lower", "case-sensitive");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
