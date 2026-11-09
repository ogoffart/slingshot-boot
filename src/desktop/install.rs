// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! The Install panel of `slingshot config`. It drives the setup script
//! (scripts/slingshot-setup, embedded here), so that the window and the
//! command line install the same way: `--dry-run` first, to show the
//! commands, then for real, as root through pkexec.

use std::collections::BTreeMap;
use std::io::Write;
use super::machine::{find_in_path, is_root};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SCRIPT: &str = include_str!("../../scripts/slingshot-setup");

/// What `slingshot-setup status --porcelain` says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// The EFI System Partition's mount point.
    pub esp: String,
    pub installed: bool,
    /// Where Slingshot is (or would be) on the ESP.
    pub file: String,
    /// The Boot#### number of Slingshot's firmware entry, or "".
    pub entry: String,
    /// In the firmware's boot order: "first", "later" or "none".
    pub order: String,
    pub systemd_boot: bool,
    pub systemd_boot_entry: bool,
    pub secure_boot: bool,
}

impl Status {
    fn parse(text: &str) -> Status {
        let map: BTreeMap<&str, &str> = text.lines().filter_map(|l| l.split_once('=')).collect();
        let text = |k: &str| map.get(k).copied().unwrap_or_default().to_string();
        let flag = |k: &str| map.get(k) == Some(&"1");
        Status {
            esp: text("esp"),
            installed: flag("installed"),
            file: text("file"),
            entry: text("entry"),
            order: text("order"),
            systemd_boot: flag("systemd_boot"),
            systemd_boot_entry: flag("systemd_boot_entry"),
            secure_boot: flag("secure_boot"),
        }
    }
}

/// The script's commands that change the system, in the panel's order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Install,
    Try,
    MakeDefault,
    Undefault,
    SystemdBootEntry,
    Uninstall,
}

impl Action {
    pub const ALL: [Action; 6] =
        [Action::Install, Action::Try, Action::MakeDefault, Action::Undefault, Action::SystemdBootEntry, Action::Uninstall];

    fn command(self) -> &'static str {
        match self {
            Action::Install => "install",
            Action::Try => "try",
            Action::MakeDefault => "make-default",
            Action::Undefault => "undefault",
            Action::SystemdBootEntry => "systemd-boot-entry",
            Action::Uninstall => "uninstall",
        }
    }
}

/// Where the setup script finds things: the program to install, and the
/// ESP if `slingshot config` was given one.
pub struct Paths<'a> {
    pub efi: &'a Path,
    pub esp: Option<&'a Path>,
    /// This program, for `slingshot keymap` (the system's keyboard layout).
    pub program: Option<&'a Path>,
}

fn args(action: Action, paths: &Paths) -> Vec<String> {
    let mut args = vec![String::from("--yes")];
    if let Some(esp) = paths.esp {
        args.extend([String::from("--esp"), esp.display().to_string()]);
    }
    match action {
        Action::Install => {
            args.extend([String::from("--efi"), paths.efi.display().to_string()]);
            if let Some(program) = paths.program {
                args.extend([String::from("--program"), program.display().to_string()]);
            }
        }
        // The window offers to restart itself.
        Action::Try => args.push(String::from("--no-restart")),
        _ => {}
    }
    args.push(action.command().into());
    args
}

/// What the setup script says about this computer. The error is the
/// script's message (not UEFI, no efibootmgr, no ESP…).
pub fn status(esp: Option<&Path>) -> Result<Status, String> {
    let mut args = vec![String::from("--porcelain")];
    if let Some(esp) = esp {
        args.extend([String::from("--esp"), esp.display().to_string()]);
    }
    args.push(String::from("status"));
    script(&args, false).map(|out| Status::parse(&out))
}

/// The commands `action` would run, from the script's `--dry-run`.
pub fn plan(action: Action, paths: &Paths) -> Result<String, String> {
    let mut args = args(action, paths);
    args.insert(0, String::from("--dry-run"));
    script(&args, false).map(|out| commands(&out))
}

/// The commands of a dry run's output, without the rest of what it says
/// (the next steps of the command line). All of it if there are none.
fn commands(out: &str) -> String {
    let commands: Vec<&str> = out.lines().filter_map(|l| l.trim().strip_prefix("[dry run] ")).collect();
    if commands.is_empty() { String::from(out) } else { commands.join("\n") }
}

/// Does `action`, as root. Returns what the script said, without its
/// pointers to its other commands (the window has buttons for them).
pub fn run(action: Action, paths: &Paths) -> Result<String, String> {
    script(&args(action, paths), true).map(|out| without_next_steps(&out))
}

fn without_next_steps(out: &str) -> String {
    let lines = out.lines().filter(|l| !l.trim_start().starts_with("slingshot-setup "));
    lines.map(|l| l.strip_suffix(" Next:").unwrap_or(l)).collect::<Vec<_>>().join("\n")
}

/// Runs the setup script with `args` (as root if `root`), and returns what
/// it printed.
fn script(args: &[String], root: bool) -> Result<String, String> {
    // pkexec wants the program's full path.
    let bash = find_in_path("bash").ok_or("bash isn't installed")?;
    let mut command = if root && !is_root() {
        let mut c = Command::new("pkexec");
        c.arg(bash);
        c
    } else {
        Command::new(bash)
    };
    let mut child = command
        .arg("-s")
        .arg("--")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Cannot run the setup script: {e}"))?;
    child.stdin.take().map(|mut s| s.write_all(SCRIPT.as_bytes())).transpose().map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let text = text.trim_end().to_string();
    match out.status.code() {
        Some(0) => Ok(text),
        // pkexec: the password dialog was dismissed, or not allowed.
        Some(126 | 127) if root && !is_root() => Err(String::from("Not done: no permission (the password was not given).")),
        _ if text.is_empty() => Err(format!("The setup script failed ({})", out.status)),
        _ => Err(text.trim_start_matches("error: ").to_string()),
    }
}

/// The Rust target of the firmware build for this computer.
const UEFI_TARGET: &str = if cfg!(target_arch = "aarch64") { "aarch64-unknown-uefi" } else { "x86_64-unknown-uefi" };

/// The slingshot.efi to install: next to this program (a release or a
/// package), or the build next to it (`cargo build-uefi` in this tree).
pub fn find_efi() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [
        dir.join("slingshot.efi"),
        dir.join(format!("../{UEFI_TARGET}/release/slingshot.efi")),
        dir.join(format!("../../{UEFI_TARGET}/release/slingshot.efi")),
        dir.join("../lib/slingshot/slingshot.efi"),
        PathBuf::from("/usr/lib/slingshot/slingshot.efi"),
    ]
    .into_iter()
    .find(|p| p.is_file())
    .and_then(|p| p.canonicalize().ok())
}

/// The Install screen's side of the boot UI (`Backend::install_*`): runs
/// the setup script on a thread, and hands the result back on the UI
/// thread (a timer polls; the UI code isn't `Send`).
pub struct Installer {
    esp: Option<PathBuf>,
    program: std::cell::RefCell<Option<PathBuf>>,
    jobs: std::rc::Rc<std::cell::RefCell<Vec<Job>>>,
    timer: slint::Timer,
}

enum Job {
    Status(std::sync::mpsc::Receiver<crate::app::InstallStatus>, Box<dyn FnOnce(crate::app::InstallStatus)>),
    Step(std::sync::mpsc::Receiver<Result<String, String>>, crate::app::InstallDone),
}

impl Installer {
    pub fn new(esp: Option<PathBuf>) -> Self {
        Installer { esp, program: std::cell::RefCell::new(find_efi()), jobs: Default::default(), timer: Default::default() }
    }

    fn poll_later(&self) {
        if self.timer.running() {
            return;
        }
        let jobs = self.jobs.clone();
        self.timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(100), move || {
            // Take the finished jobs out, then call them back (they may add
            // jobs).
            let mut done = Vec::new();
            jobs.borrow_mut().retain_mut(|job| {
                let finished = match job {
                    Job::Status(rx, _) => rx.try_recv().map(Ok),
                    Job::Step(rx, _) => rx.try_recv().map(Err),
                };
                match finished {
                    Ok(result) => {
                        done.push((std::mem::replace(job, Job::Step(std::sync::mpsc::channel().1, Box::new(|_| {}))), result));
                        false
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => true,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
                }
            });
            for (job, result) in done {
                match (job, result) {
                    (Job::Status(_, callback), Ok(status)) => callback(status),
                    (Job::Step(_, callback), Err(result)) => callback(result),
                    _ => {}
                }
            }
        });
    }

    pub fn status(&self, done: Box<dyn FnOnce(crate::app::InstallStatus)>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let esp = self.esp.clone();
        let program = self.program.borrow().as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        std::thread::spawn(move || {
            let status = match self::status(esp.as_deref()) {
                Ok(s) => crate::app::InstallStatus {
                    error: String::new(),
                    esp: s.esp,
                    installed: s.installed,
                    file: s.file,
                    entry: s.entry,
                    order: s.order,
                    systemd_boot: s.systemd_boot,
                    systemd_boot_entry: s.systemd_boot_entry,
                    secure_boot: s.secure_boot,
                    program,
                },
                Err(error) => crate::app::InstallStatus { error, program, ..Default::default() },
            };
            let _ = tx.send(status);
        });
        self.jobs.borrow_mut().push(Job::Status(rx, done));
        self.poll_later();
    }

    pub fn step(&self, action: usize, plan_only: bool, done: crate::app::InstallDone) {
        let Some(&action) = Action::ALL.get(action) else { return done(Err(String::from("no such step"))) };
        let efi = self.program.borrow().clone().unwrap_or_default();
        let esp = self.esp.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let program = std::env::current_exe().ok();
            let paths = Paths { efi: &efi, esp: esp.as_deref(), program: program.as_deref() };
            let _ = tx.send(if plan_only { plan(action, &paths) } else { run(action, &paths) });
        });
        self.jobs.borrow_mut().push(Job::Step(rx, done));
        self.poll_later();
    }

    /// A file dialog for slingshot.efi.
    pub fn choose_program(&self) -> bool {
        let mut dialog = rfd::FileDialog::new().set_title("Choose slingshot.efi").add_filter("EFI program", &["efi", "EFI"]);
        if let Some(dir) = self.program.borrow().as_ref().and_then(|p| p.parent()).filter(|d| d.is_dir()) {
            dialog = dialog.set_directory(dir);
        }
        match dialog.pick_file() {
            Some(file) => {
                *self.program.borrow_mut() = Some(file);
                true
            }
            None => false,
        }
    }
}

/// Restarts the computer.
pub fn restart() -> Result<(), String> {
    Command::new("systemctl").arg("reboot").spawn().map(drop).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain() {
        let s = Status::parse("esp=/boot\ninstalled=1\nfile=/boot/EFI/slingshot/slingshot.efi\nentry=0003\norder=first\nsystemd_boot=1\nsystemd_boot_entry=0\nsecure_boot=0\n");
        assert_eq!((s.esp.as_str(), s.installed, s.entry.as_str(), s.order.as_str()), ("/boot", true, "0003", "first"));
        assert!(s.systemd_boot && !s.systemd_boot_entry && !s.secure_boot);
    }

    #[test]
    fn dry_run_commands() {
        let out = "==> Installing\n    EFI System Partition: /boot\n    [dry run] mkdir -p /boot/EFI/slingshot\n    ✓ Copied\n    [dry run] efibootmgr --create-only\nDone. Next:\n";
        assert_eq!(commands(out), "mkdir -p /boot/EFI/slingshot\nefibootmgr --create-only");
        assert_eq!(commands("Nothing to do"), "Nothing to do");
        let out = "    ✓ Created\nDone. Nothing boots differently yet. Next:\n    slingshot-setup try    start once\n";
        assert_eq!(without_next_steps(out), "    ✓ Created\nDone. Nothing boots differently yet.");
    }

    #[test]
    fn arguments() {
        let paths = Paths { efi: Path::new("/tmp/s.efi"), esp: Some(Path::new("/efi")), program: Some(Path::new("/bin/s")) };
        assert_eq!(args(Action::Install, &paths), ["--yes", "--esp", "/efi", "--efi", "/tmp/s.efi", "--program", "/bin/s", "install"]);
        let paths = Paths { efi: Path::new("/tmp/s.efi"), esp: None, program: None };
        assert_eq!(args(Action::Try, &paths), ["--yes", "--no-restart", "try"]);
    }
}
