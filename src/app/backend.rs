// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! What the UI needs from the machine. Implemented with UEFI services by the
//! `slingshot` binary, and with simulated data by the desktop simulator.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::parse::bls::ends_with_ignore_case;

use super::Hub;
use super::settings::Saved;

/// One firmware boot entry (`Boot####`).
#[derive(Clone, Debug, Default)]
pub struct BootOption {
    pub number: u16,
    pub title: String,
    pub subtitle: String,
    /// Icon kind: linux, windows, usb, network, shell, disc, app or disk.
    pub kind: &'static str,
    /// Whether this entry starts systemd-boot.
    pub is_systemd_boot: bool,
    /// A whole device rather than a file on it: the generic entries
    /// firmware creates ("EFI USB Device", a network card…).
    pub whole_device: bool,
    /// A USB disk's entry, and the disk isn't plugged in (any more): no
    /// card for it.
    pub absent: bool,
}

/// A Boot Loader Specification entry (`loader/entries/*.conf` or a unified
/// kernel image): what systemd-boot shows, and Slingshot can start itself.
#[derive(Clone, Debug, Default)]
pub struct LoaderEntry {
    /// The entry id: its file name, e.g. "arch.conf".
    pub id: String,
    pub title: String,
    pub version: String,
    pub kind: &'static str,
    /// The program to start (the kernel, or an EFI program), on the EFI
    /// System Partition, e.g. `\vmlinuz-linux`.
    pub program: String,
    /// Its command line (initrds and kernel options).
    pub options: String,
    /// Boot counting: every try failed, so it isn't the default any more.
    pub failed: bool,
}

impl From<crate::parse::bls::Entry> for LoaderEntry {
    fn from(e: crate::parse::bls::Entry) -> Self {
        LoaderEntry {
            failed: e.is_bad(),
            id: e.id,
            title: e.title,
            version: e.version,
            kind: e.kind,
            program: e.program,
            options: e.options,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct LoaderConfig {
    /// Entries in systemd-boot's menu order.
    pub entries: Vec<LoaderEntry>,
    /// Index of the entry systemd-boot boots by default.
    pub default: usize,
    /// The entry asked for this boot only (`bootctl reboot
    /// --boot-loader-entry`): the default whatever slingshot.toml says.
    pub one_shot: Option<String>,
    /// The default is `@saved` (loader.conf, or `bootctl set-default
    /// @saved`): the entry started last time.
    pub saved: bool,
    /// The entry systemd-boot started last (`LoaderEntryLastBooted`).
    pub last_booted: Option<String>,
    /// loader.conf says `editor no`: no editing the kernel options.
    pub editor_off: bool,
}

/// The boot configuration read at startup.
#[derive(Clone, Debug, Default)]
pub struct BootData {
    /// Visible entries, in firmware order.
    pub options: Vec<BootOption>,
    /// The full `BootOrder`, including entries that aren't shown.
    pub order: Vec<u16>,
    /// The Boot Loader Specification entries (systemd-boot's format).
    pub loader_entries: Option<LoaderConfig>,
    /// What was found on the disks and isn't in the entries above.
    pub found: Vec<FoundEntry>,
    pub firmware_setup_supported: bool,
    /// The firmware's `Timeout` variable, in seconds.
    pub firmware_timeout: Option<u16>,
    /// Secure Boot is enforced.
    pub secure_boot: bool,
}

/// Something found on a disk (`parse::detect`).
#[derive(Clone, Debug, Default)]
pub struct FoundEntry {
    pub title: String,
    pub subtitle: String,
    pub kind: &'static str,
    /// The file system's device path.
    pub fs: Vec<u8>,
    pub program: String,
    pub options: String,
}

/// What a card in the boot picker starts.
#[derive(Clone, Debug)]
pub enum Target {
    /// A firmware boot entry (Boot####).
    Firmware(u16),
    /// A Boot Loader Specification entry. Started directly (`program` with
    /// `options`); else through systemd-boot's Boot#### entry, `boot`, when
    /// there is one.
    LoaderEntry { systemd_boot: Option<u16>, id: String, program: String, options: String },
    /// An entry of slingshot.toml's own: a kernel or an EFI program on the
    /// file system Slingshot is on, with its command line.
    Program { path: String, options: String },
    /// A file picked in "Start a file…": `path` on `Backend::volumes()[volume]`.
    File { volume: usize, path: String },
    /// Something found on a disk (macOS, Windows, a Linux kernel): `program`
    /// on the file system with the device path `fs`, with its command line.
    Found {
        #[cfg_attr(not(target_os = "uefi"), allow(dead_code))] // the firmware's
        fs: Vec<u8>,
        program: String,
        options: String,
    },
}

/// An encrypted disk a boot entry needs, still locked (`Backend::locked_volumes`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LockedVolume {
    /// Passed back to `Backend::unlock`.
    pub id: usize,
    /// What it opens as (`root`, `luks-…`).
    pub name: String,
    /// Which disk it is ("NVMe disk · partition 2 · 476 GB").
    pub detail: String,
}

/// How a passphrase went.
#[derive(Clone, Debug, PartialEq)]
// Without the `luks` feature, unlocking only fails.
#[cfg_attr(not(feature = "luks"), allow(dead_code))]
pub enum UnlockResult {
    Unlocked,
    WrongPassphrase,
    /// It couldn't be checked: why (an unsupported cipher, too little memory…).
    Failed(String),
    /// Escape pressed while it was checked where nothing else reads the
    /// keyboard: as Cancel.
    Cancelled,
}

pub type UnlockDone = Box<dyn FnOnce(UnlockResult)>;

/// A passphrase: wiped when dropped.
pub struct Secret(pub Vec<u8>);

impl Drop for Secret {
    fn drop(&mut self) {
        crate::parse::wipe(&mut self.0);
    }
}

/// What the Install screen shows (`slingshot config` on the desktop).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InstallStatus {
    /// Why it couldn't be read ("" when it was).
    pub error: String,
    pub esp: String,
    pub installed: bool,
    pub file: String,
    /// The Boot#### number of Slingshot's entry, or "".
    pub entry: String,
    /// In the boot order: "first", "later" or "none".
    pub order: String,
    pub systemd_boot: bool,
    pub systemd_boot_entry: bool,
    pub secure_boot: bool,
    /// The slingshot.efi to install ("" if none was found).
    pub program: String,
}

/// Called with what an install step printed, or why it failed.
pub type InstallDone = Box<dyn FnOnce(Result<String, String>)>;

/// A file system, in "Start a file…".
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Volume {
    /// Its label, or what it is ("NVMe disk · partition 1").
    pub name: String,
    pub detail: String,
    /// An icon: "disk", "usb" or "disc".
    pub kind: &'static str,
    /// For a partition the firmware can't read, what it holds ("ext4",
    /// "LUKS"…: `parse::fstype`); empty for a file system.
    pub format: &'static str,
    /// An encrypted partition Slingshot can open: its id for
    /// `Backend::unlock`, then `Backend::open_unlocked`.
    pub unlock: Option<usize>,
}

/// A file or folder, in "Start a file…".
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

/// What "Start a file…" shows of a folder: its folders, then the EFI
/// programs (or the pictures, when choosing one), each sorted by name.
pub fn browse_entries(mut entries: Vec<DirEntry>, pictures: bool) -> Vec<DirEntry> {
    let wanted = |name: &str| if pictures { is_picture(name) } else { ends_with_ignore_case(name, ".efi") };
    entries.retain(|e| e.name != "." && e.name != ".." && (e.is_dir || wanted(&e.name)));
    entries.sort_by_key(|e| (!e.is_dir, e.name.to_ascii_lowercase()));
    entries
}

/// One of the six cards of the System screen.
#[derive(Clone, Debug, Default)]
pub struct InfoCard {
    pub label: String,
    pub title: String,
    pub detail: String,
    /// "" for no icon.
    pub icon: &'static str,
    pub good: bool,
}

impl InfoCard {
    pub fn new(label: &str, title: impl Into<String>, detail: impl Into<String>, icon: &'static str, good: bool) -> Self {
        InfoCard { label: label.into(), title: title.into(), detail: detail.into(), icon, good }
    }
}

#[derive(Clone, Debug, Default)]
pub struct MemoryUsage {
    pub usable: u64,
    pub runtime: u64,
    pub acpi: u64,
    pub reserved: u64,
}

impl MemoryUsage {
    // For the firmware's Memory row (the simulator gives its own).
    #[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
    pub fn total(&self) -> u64 {
        self.usable + self.runtime + self.acpi + self.reserved
    }
}

#[derive(Clone, Debug, Default)]
pub struct SystemInfo {
    /// Processor, memory, system, firmware, display, Secure Boot.
    pub cards: Vec<InfoCard>,
    pub memory: Option<MemoryUsage>,
    /// One line about the firmware, for the boot picker's footer.
    pub firmware_line: String,
}

/// What happened when starting something. When it works on real firmware,
/// the started program takes over and `start` doesn't return.
#[derive(Debug)]
// Each target constructs only some of the variants.
#[allow(dead_code)]
pub enum StartResult {
    /// The simulator: what would have happened. The UI simulates a restart.
    Simulated(String),
    /// The program ran and came back to Slingshot, e.g. the shell's `exit`.
    Returned(String),
    /// It could not be started.
    Failed(String),
}

/// What a machine offers, beyond what every backend does: the firmware's
/// is the default.
#[derive(Clone, Copy, Debug, Default)]
pub struct Capabilities {
    /// `slingshot config`: configuring this computer from its desktop, which
    /// has its own power buttons, clock and file browser; the settings are
    /// saved in slingshot.toml (the desktop can't reach the firmware's
    /// variable).
    pub configure: bool,
    /// The desktop simulator: the UI says so, and there's no firmware log
    /// or keyboard to set.
    pub simulation: bool,
    /// `add_picture` asks with the desktop's own file dialog.
    pub picture_dialog: bool,
    /// The Install screen is there (desktop configuration only).
    pub install: bool,
}

pub trait Backend {
    fn boot_data(&self) -> BootData;
    /// The entries again (`options`, with `absent` and the descriptions)
    /// and what the disks can start (`found`): after a USB stick came or
    /// went. The rest may be left out, not read again.
    fn rescan(&self) -> BootData {
        self.boot_data()
    }
    /// `rescan`'s texts again, in the language just chosen: without reading
    /// the disks again, where the backend can.
    fn retranslate(&self) -> BootData {
        self.rescan()
    }
    /// `display` is the current resolution, `modes` the available ones.
    fn system_info(&self, display: (usize, usize), modes: &[(usize, usize)]) -> SystemInfo;
    /// The batteries and the AC adapter, read again each time.
    fn power(&self) -> crate::parse::battery::Power {
        Default::default()
    }
    /// Said after the top bar's clock: the simulator's is UTC.
    fn time_zone(&self) -> &str {
        ""
    }
    /// The firmware's clock, if it can be read.
    fn date_time(&self) -> Option<DateTime> {
        None
    }
    /// Sets the firmware's clock.
    fn set_date_time(&self, _time: DateTime) -> Result<(), String> {
        Err(String::from("not supported"))
    }

    /// What this machine offers.
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    fn load_settings(&self) -> Option<Saved>;
    fn save_settings(&self, settings: &Saved) -> Result<(), String>;
    /// Reads `path` of `volumes()[volume]` (a picture to copy).
    fn read_volume_file(&self, _volume: usize, _path: &str) -> Result<Vec<u8>, String> {
        Err(String::from("not supported here"))
    }
    /// Asks for a PNG and copies it next to slingshot.toml: its name, or
    /// `None` when none was chosen.
    fn add_picture(&self) -> Result<Option<String>, String> {
        Ok(None)
    }
    /// Reads the install status, and calls `done` with it later.
    fn install_status(&self, _done: Box<dyn FnOnce(InstallStatus)>) {}
    /// Runs install step `action` (0 install, 1 try once, 2 make default,
    /// 3 undefault, 4 systemd-boot entry, 5 uninstall): only its dry run
    /// (the commands) when `plan`. Calls `done` later.
    fn install_step(&self, _action: usize, _plan: bool, done: InstallDone) {
        done(Err(String::from("not supported here")));
    }
    /// Asks for the slingshot.efi to install; true if one was chosen.
    fn choose_install_program(&self) -> bool {
        false
    }
    /// Restarts the computer (after "once, on the next restart").
    fn restart(&self) -> Result<(), String> {
        Err(String::from("not supported here"))
    }
    /// Restarts the computer, or turns it off (`shut_down`). Only returns
    /// when it couldn't, or in the simulator.
    fn power_off(&self, _shut_down: bool) -> StartResult {
        StartResult::Failed(String::from("not supported here"))
    }
    /// Saves the log next to Slingshot (F12 on the System screen), for a bug
    /// report: where it went. Only the firmware keeps one.
    fn save_log(&self) -> Result<String, String> {
        Err(String::from("not supported here"))
    }
    /// What the top bar says this is ("" in the firmware).
    fn badge(&self) -> &str {
        ""
    }
    fn reset_settings(&self) -> Result<(), String>;

    fn set_boot_order(&self, order: &[u16]) -> Result<(), String>;

    /// Reads a file from Slingshot's own directory (`slingshot.toml`, the
    /// splash image), if it exists.
    fn read_file(&self, name: &str) -> Option<Vec<u8>>;
    /// The file systems, for "Start a file…" (cached until the next call).
    fn volumes(&self) -> Vec<Volume> {
        Vec::new()
    }
    /// The partitions the firmware has no file system for, that Slingshot
    /// recognizes (`Volume::format`): shown in the file browser, to say why.
    fn unreadable_volumes(&self) -> Vec<Volume> {
        Vec::new()
    }
    /// The files of folder `path` (`\` separated, `\` for the root) of
    /// `volumes()[volume]`.
    fn list_dir(&self, _volume: usize, _path: &str) -> Result<Vec<DirEntry>, String> {
        Err(String::from("not supported here"))
    }
    /// Adds a firmware boot entry (Boot####, at the end of the boot order)
    /// that starts `path` of `volumes()[volume]`. Returns its number.
    fn add_boot_entry(&self, _volume: usize, _path: &str, _title: &str) -> Result<u16, String> {
        Err(String::from("not supported here"))
    }
    /// Whether a disk came or went since the last `boot_data` (a USB stick
    /// plugged in or out): the boot picker reads it again.
    fn media_changed(&self) -> bool {
        false
    }
    /// The pictures next to slingshot.toml, for choosing pictures.
    fn list_pictures(&self) -> Vec<String> {
        Vec::new()
    }
    /// Writes a file in Slingshot's own directory (slingshot.toml).
    fn write_file(&self, _name: &str, _data: &[u8]) -> Result<(), String> {
        Err(String::from("not supported"))
    }
    /// The encrypted disks `target` needs that Slingshot can unlock, still
    /// locked: asked for before starting it, so that Linux gets them open.
    fn locked_volumes(&self, _target: &Target) -> Vec<LockedVolume> {
        Vec::new()
    }
    /// Checks `passphrase` for `locked_volumes()[…].id`; `done` gets the
    /// result later, on the UI thread (the check takes a second or two).
    /// `start` then hands the unlocked disk to Linux.
    fn unlock(&self, _volume: usize, _passphrase: Secret, done: UnlockDone) {
        done(UnlockResult::Failed(String::from("not supported here")));
    }
    /// Forgets (and wipes) the disks unlocked so far: the start was cancelled.
    fn forget_unlocked(&self) {}
    /// The system that hibernated, when one did and hasn't resumed yet
    /// (systemd's `HibernateLocation`): it must be started again.
    fn hibernated(&self) -> Option<crate::parse::hibernate::Hibernated> {
        None
    }
    /// The entry started last (`Card::key`), kept across restarts.
    fn last_started(&self) -> Option<String> {
        None
    }
    /// Keeps `key` (`Card::key`) as the entry started last; `loader_entry`
    /// is its systemd-boot entry, if it is one (`LoaderEntryLastBooted`).
    fn set_last_started(&self, _key: &str, _loader_entry: Option<&str>) {}
    /// Offers a disk unlocked from the file browser (`Volume::unlock`) to
    /// the firmware, decrypted: it's then among `volumes()` when a driver
    /// reads its file system, else among `unreadable_volumes()`.
    fn open_unlocked(&self, _id: usize) -> Result<(), String> {
        Err(String::from("not supported here"))
    }
    /// The keyboard layout the firmware's keys are translated with.
    /// Desktops have their own.
    fn set_keymap(&self, _keymap: crate::parse::keymap::Keymap) {}
    /// How fast the pointer moves: `factor` times, and with `acceleration`,
    /// fast moves go further. Desktops have their own.
    fn set_pointer(&self, _factor: f32, _acceleration: bool) {}
    /// The machine's language, as a locale or language tag ("fr_FR.UTF-8",
    /// "de-DE"), when slingshot.toml doesn't say.
    fn language(&self) -> Option<String> {
        None
    }
    fn start(&self, target: &Target) -> StartResult;
    fn reboot_to_firmware_setup(&self) -> StartResult;

    /// Switches the display to `width`×`height`. The platform applies it and
    /// then calls `hub.on_mode_changed`.
    fn request_mode(&self, hub: &Hub, _window: &slint::Window, width: usize, height: usize) {
        hub.requested_mode.set(Some((width, height)));
    }

    /// Called once the UI is created, before the event loop runs.
    fn ui_created(&self, _hub: &Hub, _window: &slint::Window) {}
}

/// Whether a file is a picture Slingshot can show: PNG or JPEG.
fn is_picture(name: &str) -> bool {
    let extensions: &[&str] = &[
        #[cfg(feature = "png")]
        ".png",
        #[cfg(feature = "jpeg")]
        ".jpg",
        #[cfg(feature = "jpeg")]
        ".jpeg",
    ];
    extensions.iter().any(|e| ends_with_ignore_case(name, e))
}

/// The pictures among a folder's file names: PNG and JPEG files, sorted.
pub fn picture_names(names: impl Iterator<Item = String>) -> Vec<String> {
    let mut names: Vec<String> = names.filter(|n| is_picture(n)).collect();
    names.sort_by_key(|n| n.to_ascii_lowercase());
    names
}

/// Builds the new `BootOrder`: the positions held by the visible entries get
/// them in the user's order; hidden entries keep their place.
pub fn merge_order(full: &[u16], visible_in_new_order: &[u16]) -> Vec<u16> {
    let mut next = visible_in_new_order.iter();
    full.iter()
        .map(|n| if visible_in_new_order.contains(n) { *next.next().unwrap_or(n) } else { *n })
        .collect()
}

/// "1.5 GB", "512 MB"
pub fn format_bytes(bytes: u64) -> String {
    let gb = bytes as f64 / (1u64 << 30) as f64;
    if gb >= 10. {
        format!("{:.0} GB", libm::round(gb))
    } else if gb >= 0.95 {
        format!("{gb:.1} GB")
    } else {
        format!("{} MB", bytes >> 20)
    }
}

/// A file's size: in KB below a megabyte (small EFI programs are common).
pub fn format_file_size(bytes: u64) -> String {
    if bytes < 1 << 20 { format!("{} KB", bytes.div_ceil(1024)) } else { format_bytes(bytes) }
}

/// The firmware's clock, to the minute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
}

pub fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        2 if (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl DateTime {
    /// The fields, in the order the dialog shows them: day, month, year,
    /// hour, minute.
    pub fn fields(&self) -> [i32; 5] {
        [self.day.into(), self.month.into(), self.year.into(), self.hour.into(), self.minute.into()]
    }

    /// Moves one field up or down, wrapping around in its range (years
    /// stay in 2000..=2099); the day is kept valid for the month.
    pub fn step(mut self, field: usize, delta: i32) -> Self {
        let wrap = |v: i32, lo: i32, hi: i32| lo + (v - lo + delta).rem_euclid(hi - lo + 1);
        match field {
            0 => self.day = wrap(self.day.into(), 1, days_in_month(self.year, self.month).into()) as u8,
            1 => self.month = wrap(self.month.into(), 1, 12) as u8,
            2 => self.year = wrap(self.year.into(), 2000, 2099) as u16,
            3 => self.hour = wrap(self.hour.into(), 0, 23) as u8,
            4 => self.minute = wrap(self.minute.into(), 0, 59) as u8,
            _ => {}
        }
        self.day = self.day.min(days_in_month(self.year, self.month));
        self
    }

    /// Sets a field to a typed number, brought into its range: a two-digit
    /// year is in this century, and the day is kept valid for the month.
    pub fn set(mut self, field: usize, value: i32) -> Self {
        match field {
            0 => self.day = value.clamp(1, 31) as u8,
            1 => self.month = value.clamp(1, 12) as u8,
            2 => self.year = (if value < 100 { 2000 + value } else { value }).clamp(2000, 2099) as u16,
            3 => self.hour = value.clamp(0, 23) as u8,
            4 => self.minute = value.clamp(0, 59) as u8,
            _ => {}
        }
        self.day = self.day.min(days_in_month(self.year, self.month));
        self
    }

    /// "Sat 7 Nov · 14:32"
    pub fn short_text(&self) -> String {
        format_clock(self.year, self.month, self.day, self.hour, self.minute)
    }

    /// "Saturday 7 November 2026 · 14:32"
    pub fn long_text(&self) -> String {
        use crate::tr::tr;
        let days = [
            tr("Sunday"), tr("Monday"), tr("Tuesday"), tr("Wednesday"), tr("Thursday"), tr("Friday"), tr("Saturday"),
        ];
        let months = [
            tr("January"), tr("February"), tr("March"), tr("April"), tr("May"), tr("June"), tr("July"),
            tr("August"), tr("September"), tr("October"), tr("November"), tr("December"),
        ];
        let m = (self.month as usize).clamp(1, 12);
        // Weekday, day, month, year: translations reorder them ({0}…{3}).
        let weekday = &days[day_of_week(self.year, self.month, self.day)];
        let date = crate::tr::trf("{} {} {} {}", &[weekday, &self.day, &months[m - 1], &self.year]);
        format!("{date} · {:02}:{:02}", self.hour, self.minute)
    }
}

/// 0 = Sunday (Sakamoto's algorithm).
fn day_of_week(year: u16, month: u8, day: u8) -> usize {
    const OFFSETS: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let m = (month as usize).clamp(1, 12);
    let y = year as i32 - i32::from(m < 3);
    (y + y / 4 - y / 100 + y / 400 + OFFSETS[m - 1] + day as i32).rem_euclid(7) as usize
}

/// A date and time as "Sat 7 Nov · 14:32".
fn format_clock(year: u16, month: u8, day: u8, hour: u8, minute: u8) -> String {
    use crate::tr::tr;
    let days = [tr("Sun"), tr("Mon"), tr("Tue"), tr("Wed"), tr("Thu"), tr("Fri"), tr("Sat")];
    let months = [
        tr("Jan"), tr("Feb"), tr("Mar"), tr("Apr"), tr("May"), tr("Jun"), tr("Jul"), tr("Aug"), tr("Sep"), tr("Oct"),
        tr("Nov"), tr("Dec"),
    ];
    let m = (month as usize).clamp(1, 12);
    // Weekday, day, month: translations reorder them ({0}…{2}).
    let date = crate::tr::trf("{} {} {}", &[&days[day_of_week(year, month, day)], &day, &months[m - 1]]);
    format!("{date} · {:02}:{:02}", hour, minute)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_order_merge() {
        // 2 and 7 are hidden: they keep their positions.
        assert_eq!(merge_order(&[1, 2, 3, 7, 4], &[4, 1, 3]), [4, 2, 1, 7, 3]);
        assert_eq!(merge_order(&[1, 2, 3], &[1, 2, 3]), [1, 2, 3]);
        assert_eq!(merge_order(&[], &[]), [] as [u16; 0]);
        assert_eq!(merge_order(&[5, 6], &[]), [5, 6]);
    }

    #[test]
    fn browsing() {
        let e = |name: &str, is_dir: bool| DirEntry { name: name.into(), is_dir, size: 0 };
        let shown = browse_entries(alloc::vec![e(".", true), e("b.EFI", false), e("readme.txt", false), e("Zed", true), e("a.efi", false), e("apps", true)], false);
        let names: Vec<&str> = shown.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["apps", "Zed", "a.efi", "b.EFI"]);
    }

    #[test]
    #[cfg(all(feature = "png", feature = "jpeg"))]
    fn pictures() {
        let names = ["b.PNG", "slingshot.toml", "a.png", "C.png", "png", "photo.JPG", "d.jpeg"].map(String::from);
        assert_eq!(picture_names(names.into_iter()), ["a.png", "b.PNG", "C.png", "d.jpeg", "photo.JPG"]);
    }

    #[test]
    fn bytes() {
        assert_eq!(format_bytes(512 << 20), "512 MB");
        assert_eq!(format_bytes(1 << 30), "1.0 GB");
        assert_eq!(format_bytes(3 << 29), "1.5 GB");
        assert_eq!(format_bytes(16 << 30), "16 GB");
        assert_eq!(format_bytes(0), "0 MB");
        assert_eq!(format_file_size(5232), "6 KB");
        assert_eq!(format_file_size(3 << 20), "3 MB");
    }

    #[test]
    fn date_time_steps() {
        let t = DateTime { year: 2024, month: 1, day: 31, hour: 23, minute: 59 };
        assert_eq!(t.step(4, 1).minute, 0, "wraps");
        assert_eq!(t.step(3, 1).hour, 0);
        assert_eq!(t.step(0, 1).day, 1);
        // 31 January → February: the day is kept valid (2024 is a leap year).
        assert_eq!(t.step(1, 1), DateTime { month: 2, day: 29, ..t });
        assert_eq!(t.step(1, 1).step(2, 1).day, 28, "2025 isn't");
        assert_eq!(t.step(1, -1).month, 12);
        assert_eq!(DateTime { year: 2099, ..t }.step(2, 1).year, 2000);
        assert_eq!(t.fields(), [31, 1, 2024, 23, 59]);
        assert_eq!(t.set(1, 4).day, 30, "typed: 31 April isn't");
        assert_eq!(t.set(2, 26).year, 2026);
        assert_eq!(t.set(2, 1999).year, 2000);
        assert_eq!((t.set(3, 99).hour, t.set(0, 0).day), (23, 1));
        assert_eq!(t.long_text(), "Wednesday 31 January 2024 · 23:59");
        assert_eq!((days_in_month(1900, 2), days_in_month(2000, 2), days_in_month(2026, 4)), (28, 29, 30));
    }

    #[test]
    fn clock() {
        assert_eq!(format_clock(2026, 11, 7, 14, 5), "Sat 7 Nov · 14:05");
        assert_eq!(format_clock(2000, 1, 1, 0, 0), "Sat 1 Jan · 00:00");
        assert_eq!(format_clock(2024, 2, 29, 23, 59), "Thu 29 Feb · 23:59");
        assert_eq!(format_clock(1970, 1, 1, 0, 0), "Thu 1 Jan · 00:00");
        // Out-of-range months don't panic.
        assert_eq!(format_clock(2026, 0, 1, 0, 0).split(' ').nth(2), Some("Jan"));
        assert_eq!(format_clock(2026, 13, 1, 0, 0).split(' ').nth(2), Some("Dec"));
    }
}
