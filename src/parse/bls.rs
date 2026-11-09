// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Boot Loader Specification entries, as systemd-boot reads them: parsing
//! `loader/entries/*.conf`, the menu order, the `default` pattern, and boot
//! counting (systemd's Automatic Boot Assessment).

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cmp::Ordering;

use super::load_option::normalize_path;

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// The entry id: its file name without the boot counter, e.g.
    /// "arch.conf" or "arch-linux.efi".
    pub id: String,
    /// The entry's own file, with its boot counter if it has one:
    /// `\loader\entries\arch+3-0.conf`, `\EFI\Linux\arch.efi`.
    pub path: String,
    /// Its boot counter (`+3-0`), if it has one.
    pub tries: Option<Tries>,
    pub title: String,
    pub version: String,
    pub sort_key: String,
    /// What the entry starts, to pick an icon: "linux", "windows" or "app".
    pub kind: &'static str,
    /// The program to start, on the same file system, e.g. `\vmlinuz-linux`.
    pub program: String,
    /// Its command line: `initrd=` for each initrd, then the `options`, as
    /// systemd-boot passes them to the kernel's EFI stub.
    pub options: String,
}

/// A boot counter in an entry's file name: `arch+3-1.conf` has 3 tries left
/// and 1 done. The boot loader counts a try before starting the entry, and
/// `systemd-bless-boot` removes the counter once the system has started.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tries {
    pub left: u32,
    pub done: Option<u32>,
}

impl Entry {
    /// No tries left: every try failed. Such entries come last.
    pub fn is_bad(&self) -> bool {
        self.tries.is_some_and(|t| t.left == 0)
    }
}

/// Splits a file name into the name without its boot counter and the
/// counter: `arch+3-1.conf` → `arch.conf`, 3 left, 1 done.
pub fn split_counter(name: &str) -> (String, Option<Tries>) {
    let (stem, ext) = name.rfind('.').map_or((name, ""), |i| name.split_at(i));
    let number = |s: &str| (!s.is_empty() && s.bytes().all(|c| c.is_ascii_digit())).then(|| s.parse::<u32>().ok()).flatten();
    let Some(plus) = stem.rfind('+') else { return (name.into(), None) };
    let counter = &stem[plus + 1..];
    let tries = match counter.split_once('-') {
        Some((left, done)) => number(left).zip(number(done)).map(|(left, done)| Tries { left, done: Some(done) }),
        None => number(counter).map(|left| Tries { left, done: None }),
    };
    match tries {
        Some(t) => (alloc::format!("{}{ext}", &stem[..plus]), Some(t)),
        None => (name.into(), None),
    }
}

/// The path an entry's file gets when a try is counted (`arch+2-1.conf`),
/// or `None` when it has no counter or no tries left.
#[cfg_attr(not(target_os = "uefi"), allow(dead_code))]
pub fn next_try_path(path: &str) -> Option<String> {
    let (dir, name) = path.rsplit_once('\\').unwrap_or(("", path));
    let (id, tries) = split_counter(name);
    let t = tries.filter(|t| t.left > 0)?;
    let (stem, ext) = id.rfind('.').map_or((id.as_str(), ""), |i| id.split_at(i));
    Some(alloc::format!("{dir}\\{stem}+{}-{}{ext}", t.left - 1, t.done.unwrap_or(0) + 1))
}

/// `/vmlinuz-linux` → `\vmlinuz-linux`
pub fn efi_path(path: &str) -> String {
    let p = path.replace('/', "\\");
    if p.starts_with('\\') { p } else { alloc::format!("\\{p}") }
}

pub fn ends_with_ignore_case(s: &str, suffix: &str) -> bool {
    let (s, suffix) = (s.as_bytes(), suffix.as_bytes());
    s.len() >= suffix.len() && s[s.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
}

fn guess_kind(title: &str, loader: &str) -> &'static str {
    let t = title.to_lowercase();
    let l = loader.to_lowercase();
    if t.contains("windows") || l.contains("microsoft") {
        "windows"
    } else if l.is_empty() || l.contains("vmlinuz") || l.contains("linux") || t.contains("linux") {
        "linux"
    } else {
        "app"
    }
}

/// Parses a Boot Loader Specification type #1 entry, from its file
/// `\loader\entries\<name>`. Also returns the files it needs (kernel,
/// initrds, EFI program).
pub fn parse_entry(name: &str, text: &str) -> (Entry, Vec<String>) {
    let (id, tries) = split_counter(name);
    let id = id.as_str();
    let mut title = String::new();
    let mut version = String::new();
    let mut sort_key = String::new();
    let mut loader = String::new();
    let mut files: Vec<String> = Vec::new();
    let mut initrds: Vec<String> = Vec::new();
    let mut options: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let value = value.trim();
        match key {
            "title" => title = value.into(),
            "version" => version = value.into(),
            "sort-key" => sort_key = value.into(),
            "linux" | "efi" | "uki" => {
                loader = value.into();
                files.push(value.into());
            }
            "initrd" => {
                files.push(value.into());
                initrds.push(value.into());
            }
            "options" if !value.is_empty() => options.push(value.into()),
            _ => {}
        }
    }
    if title.is_empty() {
        title = id.trim_end_matches(".conf").into();
    }
    let kind = guess_kind(&title, &loader);
    let program = if loader.is_empty() { String::new() } else { efi_path(&loader) };
    let options = command_line(&initrds, &options.join(" "));
    let path = alloc::format!("\\loader\\entries\\{name}");
    (Entry { id: id.into(), path, tries, title, version, sort_key, kind, program, options }, files)
}

/// Whether an entry's files include `own_file` (lowercase, with
/// backslashes, as from [`normalize_path`]): the entry starts that program.
pub fn starts_file(files: &[String], own_file: &str) -> bool {
    let own = own_file.trim_start_matches('\\');
    !own.is_empty() && files.iter().any(|f| normalize_path(f).trim_start_matches('\\') == own)
}

/// A kernel command line as systemd-boot builds it: `initrd=` for each
/// initrd (the kernel's EFI stub loads them), then the options.
pub fn command_line(initrds: &[String], options: &str) -> String {
    let mut parts: Vec<String> = initrds.iter().map(|i| alloc::format!("initrd={}", efi_path(i))).collect();
    if !options.trim().is_empty() {
        parts.push(options.trim().into());
    }
    parts.join(" ")
}

/// The entry for a unified kernel image in `\EFI\Linux`. Its real title is
/// in its .osrel section; the file name is a good enough approximation.
pub fn uki_entry(file_name: &str) -> Entry {
    let (id, tries) = split_counter(file_name);
    let stem = id.get(..id.len().saturating_sub(4)).unwrap_or(&id);
    Entry {
        id: id.clone(),
        path: alloc::format!("\\EFI\\Linux\\{file_name}"),
        tries,
        title: stem.replace(['-', '_'], " "),
        version: String::new(),
        sort_key: String::new(),
        kind: "linux",
        program: alloc::format!("\\EFI\\Linux\\{file_name}"),
        options: String::new(),
    }
}

/// Compares versions like systemd-boot: runs of digits as numbers, so
/// "6.9" comes before "6.10".
pub fn version_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    let digits = |s: &[u8]| s.iter().take_while(|c| c.is_ascii_digit()).count();
    loop {
        match (a.first(), b.first()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let (na, nb) = (digits(a), digits(b));
                let zeros = |s: &[u8]| s.iter().take_while(|c| **c == b'0').count();
                let (da, db) = (&a[zeros(&a[..na])..na], &b[zeros(&b[..nb])..nb]);
                let ord = da.len().cmp(&db.len()).then_with(|| da.cmp(db));
                if ord != Ordering::Equal {
                    return ord;
                }
                (a, b) = (&a[na..], &b[nb..]);
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(y);
                }
                (a, b) = (&a[1..], &b[1..]);
            }
        }
    }
}

/// systemd-boot's menu order: entries that failed every try last; then
/// entries with a sort key first (by key, then newest version first), then
/// the others by id in reverse order (newest version first).
pub fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| a.is_bad().cmp(&b.is_bad()).then_with(|| match (a.sort_key.is_empty(), b.sort_key.is_empty()) {
        (false, false) => a.sort_key.cmp(&b.sort_key).then_with(|| version_cmp(&b.version, &a.version)),
        (false, true) => Ordering::Less,
        (true, false) => Ordering::Greater,
        (true, true) => version_cmp(&b.id, &a.id),
    }));
}

/// Matches systemd-boot's `default` patterns (`*` and `?` wildcards,
/// case-insensitive).
///
/// Iterative, going back only to the last `*`: linear time, whatever the
/// pattern (they come from variables and files any OS can write).
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti) = (0, 0);
    // The last `*` seen, and where in the text it started matching.
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        match p.get(pi) {
            Some('*') => {
                star = Some((pi, ti));
                pi += 1;
            }
            Some(c) if *c == '?' || c.eq_ignore_ascii_case(&t[ti]) => {
                pi += 1;
                ti += 1;
            }
            _ => match star {
                // That `*` takes one more character.
                Some((sp, st)) => {
                    pi = sp + 1;
                    ti = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// The `default` setting of `loader.conf`.
pub fn loader_conf_default(conf: &str) -> Option<String> {
    conf.lines().map(str::trim).find_map(|l| {
        let (key, value) = l.split_once(char::is_whitespace).unwrap_or((l, ""));
        (key == "default").then(|| value.trim().to_string())
    })
}

/// The `editor` setting of `loader.conf`: whether the kernel options may
/// be edited before starting an entry (yes when it isn't there).
pub fn loader_conf_editor(conf: &str) -> bool {
    conf.lines().map(str::trim).filter_map(|l| {
        let (key, value) = l.split_once(char::is_whitespace)?;
        (key == "editor").then(|| value.trim())
    }).next_back().is_none_or(|v| !matches!(v, "no" | "false" | "0" | "off" | "n" | "f"))
}

/// The index of the default entry for a `default` pattern: the first entry
/// in menu order that matches it, else the first entry. Special values like
/// `@saved` are not supported and also give the first entry.
pub fn default_index(pattern: Option<&str>, entries: &[Entry]) -> usize {
    pattern
        .filter(|p| !p.starts_with('@'))
        .and_then(|p| entries.iter().position(|e| glob_match(p, &e.id)))
        .unwrap_or(0)
}

/// systemd-boot's menu, from what an EFI System Partition holds:
/// - `conf_files`: `(file name, contents)` of `\loader\entries\*.conf`,
/// - `ukis`: file names in `\EFI\Linux` (unified kernel images),
/// - `exists`: whether a file named in an entry exists,
/// - `own_file`: Slingshot's own path, whose entries are left out,
/// - `default_var`: `LoaderEntryDefault` (`bootctl set-default`),
/// - `loader_conf`: the contents of `\loader\loader.conf`.
///
/// Returns the entries in menu order and the index of the default one.
pub fn menu(
    conf_files: &[(String, String)],
    ukis: &[String],
    exists: &mut dyn FnMut(&str) -> bool,
    own_file: &str,
    default_var: Option<&str>,
    loader_conf: Option<&str>,
) -> (Vec<Entry>, usize) {
    let mut entries: Vec<Entry> = Vec::new();
    for (name, text) in conf_files {
        let (entry, files) = parse_entry(name, text);
        if starts_file(&files, own_file) {
            continue;
        }
        // Like systemd-boot, leave out entries whose files are missing.
        if files.iter().all(|f| exists(f)) {
            entries.push(entry);
        } else {
            log::info!("systemd-boot: skipping {name}: missing file");
        }
    }
    entries.extend(ukis.iter().map(|name| uki_entry(name)));
    sort_entries(&mut entries);
    let pattern = default_var.map(String::from).or_else(|| loader_conf.and_then(loader_conf_default));
    let default = default_index(pattern.as_deref(), &entries);
    (entries, default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn entry(id: &str, version: &str, sort_key: &str) -> Entry {
        Entry { id: id.into(), path: String::new(), tries: None, title: id.into(), version: version.into(), sort_key: sort_key.into(), kind: "linux", program: String::new(), options: String::new() }
    }

    fn ids(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.id.as_str()).collect()
    }

    #[test]
    fn parses_an_entry() {
        let text = "# Arch Linux\n\
            title   Arch Linux (fallback)\n\
            version 6.17.1-arch1-1\n\
            sort-key arch\n\
            linux   /vmlinuz-linux\n\
            initrd  /intel-ucode.img\n\
            initrd  /initramfs-linux-fallback.img\n\
            options root=UUID=1234 rw\n";
        let (e, files) = parse_entry("arch-fallback.conf", text);
        assert_eq!(e.program, "\\vmlinuz-linux");
        assert_eq!(e.options, "initrd=\\intel-ucode.img initrd=\\initramfs-linux-fallback.img root=UUID=1234 rw");
        assert_eq!(e.title, "Arch Linux (fallback)");
        assert_eq!(e.version, "6.17.1-arch1-1");
        assert_eq!(e.sort_key, "arch");
        assert_eq!(e.kind, "linux");
        assert_eq!(files, ["/vmlinuz-linux", "/intel-ucode.img", "/initramfs-linux-fallback.img"]);
    }

    #[test]
    fn entry_defaults_and_kinds() {
        let (e, files) = parse_entry("windows.conf", "efi /EFI/Microsoft/Boot/bootmgfw.efi\n");
        assert_eq!((e.program.as_str(), e.options.as_str()), ("\\EFI\\Microsoft\\Boot\\bootmgfw.efi", ""));
        assert_eq!((e.title.as_str(), e.kind), ("windows", "windows"));
        assert_eq!(files.len(), 1);
        let (e, _) = parse_entry("shell.conf", "title UEFI Shell\nefi /shellx64.efi");
        assert_eq!(e.kind, "app");
        // Tabs and indented comments.
        let (e, files) = parse_entry("x.conf", "\ttitle\tX\n  # linux /nope\n");
        assert_eq!(e.title, "X");
        assert!(files.is_empty());
    }

    #[test]
    fn boot_counters() {
        let t = |left, done| Some(Tries { left, done });
        assert_eq!(split_counter("arch+3-1.conf"), ("arch.conf".into(), t(3, Some(1))));
        assert_eq!(split_counter("arch+3.conf"), ("arch.conf".into(), t(3, None)));
        assert_eq!(split_counter("arch-6.1+0-3.efi"), ("arch-6.1.efi".into(), t(0, Some(3))));
        // Not counters.
        for name in ["arch.conf", "c++.conf", "a+b.conf", "a+1-.conf", "a+-1.conf", "a+1-2-3.conf"] {
            assert_eq!(split_counter(name), (name.into(), None), "{name}");
        }
        assert_eq!(next_try_path("\\loader\\entries\\arch+3.conf").as_deref(), Some("\\loader\\entries\\arch+2-1.conf"));
        assert_eq!(next_try_path("\\EFI\\Linux\\arch+1-4.efi").as_deref(), Some("\\EFI\\Linux\\arch+0-5.efi"));
        assert_eq!(next_try_path("\\EFI\\Linux\\arch+0-5.efi"), None);
        assert_eq!(next_try_path("\\loader\\entries\\arch.conf"), None);
    }

    #[test]
    fn counted_entries() {
        let (e, _) = parse_entry("arch+2-1.conf", "linux /vmlinuz\n");
        assert_eq!((e.id.as_str(), e.path.as_str(), e.title.as_str()), ("arch.conf", "\\loader\\entries\\arch+2-1.conf", "arch"));
        assert!(!e.is_bad());
        let u = uki_entry("arch+0-3.efi");
        assert_eq!((u.id.as_str(), u.program.as_str(), u.title.as_str()), ("arch.efi", "\\EFI\\Linux\\arch+0-3.efi", "arch"));
        assert!(u.is_bad());

        // An entry that failed every try comes last, so it's no longer the
        // default; the default pattern matches the id without the counter.
        let confs = [
            ("new+0-3.conf".to_string(), "linux /k\n".to_string()),
            ("old.conf".to_string(), "linux /k\n".to_string()),
        ];
        let (entries, default) = menu(&confs, &[], &mut |_| true, "", None, None);
        assert_eq!(ids(&entries), ["old.conf", "new.conf"]);
        assert_eq!(default, 0);
        let (_, default) = menu(&confs, &[], &mut |_| true, "", Some("new.conf"), None);
        assert_eq!(default, 1);
    }

    #[test]
    fn globs_in_linear_time() {
        assert!(glob_match("arch*.conf", "Arch-linux.conf"));
        assert!(glob_match("*", ""));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
        assert!(glob_match("*lts*", "arch-lts.conf"));
        assert!(!glob_match("*lts", "arch-lts.conf"));
        assert!(glob_match("a*b*c", "aXbYbZc"));
        // Exponential for a backtracking matcher: instant here.
        let pattern = alloc::format!("{}x", "*".repeat(40));
        assert!(!glob_match(&pattern, &"a".repeat(1000)));
    }

    #[test]
    fn uki_entries() {
        let e = uki_entry("arch-linux_lts.efi");
        assert_eq!((e.program.as_str(), e.options.as_str()), ("\\EFI\\Linux\\arch-linux_lts.efi", ""));
        assert_eq!((e.id.as_str(), e.title.as_str(), e.kind), ("arch-linux_lts.efi", "arch linux lts", "linux"));
        assert_eq!(uki_entry(".efi").title, "");
    }

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(version_cmp("6.9", "6.10"), Ordering::Less);
        assert_eq!(version_cmp("6.10", "6.9"), Ordering::Greater);
        assert_eq!(version_cmp("6.010", "6.10"), Ordering::Equal);
        assert_eq!(version_cmp("6.1", "6.1.1"), Ordering::Less);
        assert_eq!(version_cmp("linux-6.1", "linux-lts"), b'6'.cmp(&b'l'));
        assert_eq!(version_cmp("", ""), Ordering::Equal);
    }

    #[test]
    fn menu_order() {
        let mut entries = vec![
            entry("linux-6.9.conf", "", ""),
            entry("b.conf", "1.0", "zzz"),
            entry("linux-6.10.conf", "", ""),
            entry("a-old.conf", "6.9", "arch"),
            entry("a-new.conf", "6.10", "arch"),
        ];
        sort_entries(&mut entries);
        assert_eq!(ids(&entries), ["a-new.conf", "a-old.conf", "b.conf", "linux-6.10.conf", "linux-6.9.conf"]);
    }

    #[test]
    fn globs() {
        assert!(glob_match("arch*", "arch-lts.conf"));
        assert!(glob_match("ARCH.conf", "arch.conf"));
        assert!(glob_match("a?ch.conf", "arch.conf"));
        assert!(glob_match("*", ""));
        assert!(glob_match("*.conf", "x.conf"));
        assert!(!glob_match("*.conf", "x.efi"));
        assert!(!glob_match("arch", "arch.conf"));
        assert!(!glob_match("?", ""));
    }

    #[test]
    fn loader_conf() {
        let conf = "timeout 3\n# default nope\n  default  arch-*.conf \nconsole-mode max\n";
        assert_eq!(loader_conf_default(conf).as_deref(), Some("arch-*.conf"));
        assert_eq!(loader_conf_default("default-mode x\n"), None);
        assert!(loader_conf_editor(conf));
        assert!(!loader_conf_editor("editor no\n"));
        assert!(!loader_conf_editor("editor   false"));
        assert!(loader_conf_editor("editor no\neditor yes"));
        assert_eq!(loader_conf_default("timeout 3"), None);
    }

    #[test]
    fn default_entry() {
        let entries = vec![entry("arch.conf", "", ""), entry("windows.conf", "", ""), entry("win-old.conf", "", "")];
        assert_eq!(default_index(Some("win*"), &entries), 1);
        assert_eq!(default_index(Some("nothing"), &entries), 0);
        assert_eq!(default_index(Some("@saved"), &entries), 0);
        assert_eq!(default_index(None, &entries), 0);
        assert_eq!(default_index(Some("x"), &[]), 0);
    }

    #[test]
    fn own_entry() {
        let files = vec![String::from("/EFI/Slingshot/slingshot.efi")];
        assert!(starts_file(&files, "\\efi\\slingshot\\slingshot.efi"));
        assert!(starts_file(&[String::from("EFI\\slingshot\\slingshot.efi")], "\\efi\\slingshot\\slingshot.efi"));
        assert!(!starts_file(&files, "\\efi\\other.efi"));
        assert!(!starts_file(&files, ""));
        assert!(!starts_file(&[], "\\efi\\slingshot\\slingshot.efi"));
    }

    #[test]
    fn whole_menu() {
        let confs = vec![
            (String::from("arch.conf"), String::from("title Arch\nlinux /vmlinuz-linux\n")),
            (String::from("gone.conf"), String::from("title Gone\nlinux /vmlinuz-gone\n")),
            (String::from("slingshot.conf"), String::from("title Slingshot\nefi /EFI/slingshot/slingshot.efi\n")),
            (String::from("lts.conf"), String::from("title LTS\nlinux /vmlinuz-lts\n")),
        ];
        let ukis = vec![String::from("arch-uki.efi")];
        let mut exists = |f: &str| f != "/vmlinuz-gone";
        let own = "\\efi\\slingshot\\slingshot.efi";
        let (entries, default) = menu(&confs, &ukis, &mut exists, own, None, Some("default lts.conf\n"));
        assert_eq!(ids(&entries), ["lts.conf", "arch.conf", "arch-uki.efi"]);
        assert_eq!(default, 0);
        let (entries, default) = menu(&confs, &ukis, &mut exists, own, Some("arch*"), Some("default lts.conf"));
        assert_eq!(entries[default].id, "arch.conf", "LoaderEntryDefault wins over loader.conf");
    }

    #[test]
    fn suffixes() {
        assert!(ends_with_ignore_case("ARCH.CONF", ".conf"));
        assert!(!ends_with_ignore_case("f", ".conf"));
    }
}
