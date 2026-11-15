// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! The Files screen: the disks and their folders, a file's details, and the
//! file opened: a text to edit (saved on disks that can be written), a
//! picture, an EFI program to start, or its first bytes.

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use super::{
    AppWindow, Backend, DateTime, DirEntry, Explorer, HexRow, LockedVolume, PictureFor, ProgramFact, ScreenPicture, Target, TreeRow,
    Volume,
};
use crate::tr::{tr, trf};

/// The largest text the editor opens, and the most bytes shown of a file.
const TEXT_MAX: usize = 256 << 10;
const BYTES_SHOWN: usize = 4096;
/// The largest file opened at all (a picture can be a few MB).
const OPEN_MAX: u64 = 32 << 20;

#[derive(Default)]
struct Files {
    volumes: Vec<Volume>,
    unreadable: Vec<Volume>,
    /// The tree's rows as shown: the disks, with the folders expanded
    /// under them.
    nodes: Vec<Node>,
    opened: Option<Opened>,
    /// The file copied (Ctrl+C), to paste in another folder.
    clipboard: Option<Clip>,
}

struct Clip {
    volume: usize,
    path: String,
    name: String,
}

struct Node {
    depth: usize,
    what: What,
    expanded: bool,
}

enum What {
    Volume(usize),
    Unreadable(usize),
    /// On disk `volume`, in the folders `folders`.
    Entry { volume: usize, folders: Vec<String>, entry: DirEntry },
}

struct Opened {
    volume: usize,
    path: String,
    name: String,
    data: Vec<u8>,
    /// A text's line endings and byte order mark, kept when it's saved.
    crlf: bool,
    bom: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Text,
    Picture,
    Program,
    Bytes,
}

fn kind_of(name: &str, data: &[u8]) -> Kind {
    if crate::parse::bls::ends_with_ignore_case(name, ".efi") {
        Kind::Program
    } else if super::is_picture(name) {
        Kind::Picture
    } else if data.len() <= TEXT_MAX && !data.contains(&0) && core::str::from_utf8(data).is_ok() {
        Kind::Text
    } else {
        Kind::Bytes
    }
}

/// The next match of `query` in `text` from byte `from`, ignoring ASCII
/// case, wrapping around: its byte range, its number (from 1) and how many
/// there are.
fn find_text(text: &str, query: &str, from: usize) -> Option<(usize, usize, usize, usize)> {
    if query.is_empty() {
        return None;
    }
    let (text, query) = (text.as_bytes(), query.as_bytes());
    // One pass: how many, the first one, and the first one from `from`.
    let (mut count, mut first, mut next) = (0, None, None);
    for i in 0..=text.len().checked_sub(query.len())? {
        if text[i..i + query.len()].eq_ignore_ascii_case(query) {
            count += 1;
            first = first.or(Some((i, count)));
            if i >= from && next.is_none() {
                next = Some((i, count));
            }
        }
    }
    let (start, n) = next.or(first)?;
    Some((start, start + query.len(), n, count))
}

/// A text as the editor shows it: without its byte order mark, and with
/// `\n` line endings. Also whether it had them.
fn text_for_editing(data: &[u8]) -> (String, bool, bool) {
    let (bom, data) = match data.strip_prefix(b"\xef\xbb\xbf") {
        Some(rest) => (true, rest),
        None => (false, data),
    };
    let text = String::from_utf8_lossy(data);
    let crlf = text.contains("\r\n");
    (text.replace("\r\n", "\n"), crlf, bom)
}

/// The edited text as the file had it: its line endings, its byte order mark.
fn text_to_save(text: &str, crlf: bool, bom: bool) -> Vec<u8> {
    let text = if crlf { text.replace("\r\n", "\n").replace('\n', "\r\n") } else { text.to_string() };
    let mut data = if bom { b"\xef\xbb\xbf".to_vec() } else { Vec::new() };
    data.extend_from_slice(text.as_bytes());
    data
}

/// `name` trimmed, if it can name a file or folder (FAT's rules: none of
/// \ / : * ? " < > |), else what's wrong.
fn check_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    let bad = |c: char| c.is_control() || "\\/:*?\"<>|".contains(c);
    if name.is_empty() || name == "." || name == ".." || name.chars().any(bad) {
        return Err(trf("\"{}\" can't be a name.", &[&name]));
    }
    Ok(name)
}

/// Whether disk `volume`'s folder `folders` has an entry `name` (ignoring
/// case, as FAT does).
fn taken(backend: &dyn Backend, volume: usize, folders: &[String], name: &str) -> bool {
    super::resolve_path(backend, volume, folders.to_vec(), name).is_ok()
}

/// The first bytes of `data`, 16 per row: offset, hex, characters.
fn hex_rows(data: &[u8]) -> Vec<HexRow> {
    data.chunks(16)
        .take(BYTES_SHOWN / 16)
        .enumerate()
        .map(|(i, chunk)| {
            let mut hex = String::new();
            for (j, b) in chunk.iter().enumerate() {
                if j > 0 {
                    hex.push_str(if j == 8 { "  " } else { " " });
                }
                hex.push_str(&format!("{b:02X}"));
            }
            let ascii: String = chunk.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' }).collect();
            HexRow { offset: format!("{:08X}", i * 16).into(), hex: hex.into(), ascii: ascii.into() }
        })
        .collect()
}

/// An EFI program's details, as lines of label and value; `None` when it
/// isn't one.
fn program_facts(data: &[u8]) -> Option<Vec<ProgramFact>> {
    use crate::parse::pe::Subsystem;
    let p = crate::parse::pe::program(data)?;
    let fact = |label: String, value: String| ProgramFact { label: label.into(), value: value.into() };
    let mut facts = alloc::vec![
        fact(tr("Processor"), p.machine),
        fact(
            tr("Kind"),
            match p.subsystem {
                Subsystem::Application if p.uki => tr("Unified kernel image"),
                Subsystem::Application => tr("EFI application"),
                Subsystem::BootServiceDriver => tr("EFI driver"),
                Subsystem::RuntimeDriver => tr("EFI runtime driver"),
                Subsystem::Other => tr("Not an EFI program"),
            },
        ),
        fact(tr("Signature"), if p.signed { tr("Signed (not checked here)") } else { tr("Not signed") }),
    ];
    if !p.sbat.is_empty() {
        facts.push(fact(String::from("SBAT"), p.sbat.join(", ")));
    }
    for (label, value) in [(tr("System"), p.os), (tr("Kernel"), p.kernel), (tr("Options"), p.cmdline)] {
        if let Some(value) = value {
            facts.push(fact(label, value));
        }
    }
    Some(facts)
}

fn format_date(t: &DateTime) -> String {
    format!("{}-{:02}-{:02} {:02}:{:02}", t.year, t.month, t.day, t.hour, t.minute)
}

/// A file's size, date and attributes, as the line under the list says them.
fn entry_details(e: &DirEntry) -> String {
    let mut parts = Vec::new();
    if !e.is_dir {
        parts.push(super::format_file_size(e.size));
    }
    if let Some(t) = &e.modified {
        parts.push(format_date(t));
    }
    if e.read_only {
        parts.push(tr("read-only"));
    }
    if e.hidden {
        parts.push(tr("hidden"));
    }
    parts.join(" · ")
}

fn row_kind(e: &DirEntry) -> &'static str {
    if e.is_dir {
        "folder"
    } else if crate::parse::bls::ends_with_ignore_case(&e.name, ".efi") {
        "app"
    } else if super::is_picture(&e.name) {
        "picture"
    } else {
        "file"
    }
}

impl Files {
    /// The disks again, as the tree's top rows (collapsed), if they changed.
    fn reload_volumes(&mut self, backend: &dyn Backend) {
        let (volumes, unreadable) = (backend.volumes(), backend.unreadable_volumes());
        if volumes == self.volumes && unreadable == self.unreadable && !self.nodes.is_empty() {
            return;
        }
        (self.volumes, self.unreadable) = (volumes, unreadable);
        self.nodes = (0..self.volumes.len())
            .map(|i| Node { depth: 0, what: What::Volume(i), expanded: false })
            .chain((0..self.unreadable.len()).map(|i| Node { depth: 0, what: What::Unreadable(i), expanded: false }))
            .collect();
    }

    /// The disk and the folder of node `i` (the folder it is, or holds it).
    fn place(&self, i: usize) -> Option<(usize, Vec<String>)> {
        match &self.nodes.get(i)?.what {
            What::Volume(v) => Some((*v, Vec::new())),
            What::Entry { volume, folders, entry } if entry.is_dir => {
                let mut folders = folders.clone();
                folders.push(entry.name.clone());
                Some((*volume, folders))
            }
            What::Entry { volume, folders, .. } => Some((*volume, folders.clone())),
            What::Unreadable(_) => None,
        }
    }

    fn expandable(&self, i: usize) -> bool {
        matches!(&self.nodes[i].what, What::Volume(_) | What::Entry { entry: DirEntry { is_dir: true, .. }, .. })
    }

    /// Shows the folder or disk of node `i` expanded: its entries under it.
    fn expand(&mut self, i: usize, backend: &dyn Backend) -> Result<(), String> {
        if !self.expandable(i) || self.nodes[i].expanded {
            return Ok(());
        }
        let (volume, folders) = self.place(i).ok_or("not a folder")?;
        let path = super::folder_path(&folders, None);
        let mut entries = backend.list_dir(volume, &path).map_err(|e| {
            log::warn!("files: {path} on disk {volume}: {e}");
            e
        })?;
        entries.retain(|e| e.name != "." && e.name != "..");
        entries.sort_by_key(|e| (!e.is_dir, e.name.to_ascii_lowercase()));
        let depth = self.nodes[i].depth + 1;
        let children = entries.into_iter().map(|entry| Node {
            depth,
            what: What::Entry { volume, folders: folders.clone(), entry },
            expanded: false,
        });
        self.nodes.splice(i + 1..i + 1, children);
        self.nodes[i].expanded = true;
        Ok(())
    }

    /// Hides what's under node `i`.
    fn collapse(&mut self, i: usize) {
        let depth = self.nodes[i].depth;
        let end = (i + 1..self.nodes.len()).find(|&j| self.nodes[j].depth <= depth).unwrap_or(self.nodes.len());
        self.nodes.drain(i + 1..end);
        self.nodes[i].expanded = false;
    }

    /// The row of disk `volume`'s folder `folders` (or the disk), if shown.
    fn folder_row(&self, volume: usize, folders: &[String]) -> Option<usize> {
        (0..self.nodes.len()).find(|&i| self.expandable(i) && self.place(i).is_some_and(|(v, f)| v == volume && f == folders))
    }

    /// Reads disk `volume`'s folder `folders` again, after a change, and
    /// shows its entry `select` (else the folder). Its row, expanded.
    fn reload_folder(&mut self, ui: &AppWindow, backend: &dyn Backend, volume: usize, folders: &[String], select: Option<&str>) -> Option<usize> {
        let row = self.folder_row(volume, folders)?;
        let depth = self.nodes[row].depth;
        let inside = |s: &Self, j: usize| j < s.nodes.len() && s.nodes[j].depth > depth;
        // Its subfolders that are open, to open again.
        let open: Vec<Vec<String>> =
            (row + 1..).take_while(|&j| inside(self, j)).filter(|&j| self.nodes[j].expanded).filter_map(|j| self.place(j).map(|(_, f)| f)).collect();
        if self.nodes[row].expanded {
            self.collapse(row);
        }
        if let Err(e) = self.expand(row, backend) {
            ui.global::<Explorer>().set_message(trf("Cannot read this folder: {}", &[&e]).into());
        }
        // In order: a folder opened shows its own, opened after it.
        let mut j = row + 1;
        while inside(self, j) {
            if !self.nodes[j].expanded && self.expandable(j) && self.place(j).is_some_and(|(_, f)| open.contains(&f)) {
                let _ = self.expand(j, backend);
            }
            j += 1;
        }
        let current = select.and_then(|name| self.child_named(row, name)).unwrap_or(row);
        self.show(ui, backend, current);
        Some(row)
    }

    /// The entry `path` of disk `volume` as the disk now has it (after a
    /// save): its size and date, in its row.
    fn refresh_entry(&mut self, backend: &dyn Backend, volume: usize, path: &str) {
        for n in &mut self.nodes {
            if let What::Entry { volume: v, folders, entry } = &mut n.what
                && *v == volume
                && super::folder_path(folders, Some(&entry.name)) == path
            {
                let listed = backend.list_dir(volume, &super::folder_path(folders, None)).ok();
                if let Some(e) = listed.and_then(|l| l.into_iter().find(|e| e.name == entry.name)) {
                    *entry = e;
                }
            }
        }
    }

    /// The row named `name` in the folder at row `folder`.
    fn child_named(&self, folder: usize, name: &str) -> Option<usize> {
        let depth = self.nodes[folder].depth + 1;
        (folder + 1..self.nodes.len()).take_while(|&j| self.nodes[j].depth >= depth).find(|&j| self.nodes[j].depth == depth && self.name(j) == name)
    }

    /// The node above `i` in the tree, if it's in one.
    fn parent(&self, i: usize) -> Option<usize> {
        let depth = self.nodes.get(i)?.depth;
        (0..i).rev().find(|&j| self.nodes[j].depth < depth)
    }

    fn name(&self, i: usize) -> String {
        match &self.nodes[i].what {
            What::Volume(v) => self.volumes[*v].name.clone(),
            What::Unreadable(v) => self.unreadable[*v].name.clone(),
            What::Entry { entry, .. } => entry.name.clone(),
        }
    }

    /// Shows the rows, with row `current` the current one.
    fn show(&self, ui: &AppWindow, backend: &dyn Backend, current: usize) {
        let rows: Vec<TreeRow> = (0..self.nodes.len())
            .map(|i| {
                let node = &self.nodes[i];
                let (detail, kind, file, dim) = match &node.what {
                    What::Volume(v) => (self.volumes[*v].detail.clone(), self.volumes[*v].kind, false, false),
                    What::Unreadable(v) => (self.unreadable[*v].detail.clone(), self.unreadable[*v].kind, false, true),
                    What::Entry { entry, .. } if entry.is_dir => (String::new(), "folder", false, entry.hidden),
                    What::Entry { entry, .. } => (super::format_file_size(entry.size), row_kind(entry), true, entry.hidden),
                };
                TreeRow {
                    name: self.name(i).into(),
                    detail: detail.into(),
                    kind: kind.into(),
                    file,
                    dim,
                    depth: node.depth as i32,
                    expandable: self.expandable(i),
                    expanded: node.expanded,
                }
            })
            .collect();
        let explorer = ui.global::<Explorer>();
        explorer.set_rows(ModelRc::new(VecModel::from(rows)));
        let current = current.min(self.nodes.len().saturating_sub(1));
        explorer.set_current(current as i32);
        explorer.set_message(if self.volumes.is_empty() { tr("The firmware can't read any disk.") } else { String::new() }.into());
        self.show_selected(ui, backend, current);
    }

    /// What the header, the location field and the details line say of row
    /// `i`: its disk, its folder, its size and date.
    fn show_selected(&self, ui: &AppWindow, backend: &dyn Backend, i: usize) {
        let explorer = ui.global::<Explorer>();
        let place = self.place(i);
        let volume = place.as_ref().and_then(|(v, _)| self.volumes.get(*v).map(|vol| (*v, vol)));
        match volume {
            Some((v, vol)) => {
                explorer.set_location(vol.name.as_str().into());
                let mut space = backend
                    .volume_space(v)
                    .map(|(free, size)| trf("{} free of {}", &[&super::format_bytes(free), &super::format_bytes(size)]))
                    .unwrap_or_default();
                if !vol.writable {
                    space = if space.is_empty() { tr("read-only") } else { format!("{space} · {}", tr("read-only")) };
                }
                explorer.set_space(space.into());
            }
            None => {
                explorer.set_location(tr("All disks").into());
                explorer.set_space(SharedString::new());
            }
        }
        let entry = match self.nodes.get(i).map(|n| &n.what) {
            Some(What::Entry { entry, .. }) => Some(entry),
            _ => None,
        };
        let path = match (&place, entry) {
            (Some((_, folders)), Some(entry)) if !entry.is_dir => super::folder_path(folders, Some(&entry.name)),
            (Some((_, folders)), _) => super::folder_path(folders, None),
            _ => String::new(),
        };
        explorer.set_path(path.into());
        explorer.set_details(entry.map(entry_details).unwrap_or_default().into());
        // What can be done to it.
        let writable = volume.is_some_and(|(_, vol)| vol.writable);
        explorer.set_can_create(writable);
        explorer.set_can_change(writable && entry.is_some_and(|e| !e.read_only));
        explorer.set_can_copy(entry.is_some_and(|e| !e.is_dir));
        explorer.set_can_paste(writable && self.clipboard.is_some());
        explorer.set_selected_name(if i < self.nodes.len() { self.name(i) } else { String::new() }.into());
    }

    /// Opens `entry` of `folders` on disk `volume`: reads it, and shows it as its kind.
    fn open_file(&mut self, ui: &AppWindow, backend: &dyn Backend, volume: usize, folders: &[String], entry: &DirEntry) {
        let explorer = ui.global::<Explorer>();
        let path = super::folder_path(folders, Some(&entry.name));
        self.header(ui, volume, &path, entry);
        if entry.size > OPEN_MAX {
            explorer.set_kind("bytes".into());
            explorer.set_hex(ModelRc::default());
            explorer.set_note(tr("This file is too large to open here.").into());
            self.opened = None;
            return;
        }
        let data = match backend.read_volume_file(volume, &path) {
            Ok(data) => data,
            Err(e) => {
                log::warn!("files: reading {path} on disk {volume}: {e}");
                explorer.set_kind("bytes".into());
                explorer.set_hex(ModelRc::default());
                explorer.set_note(trf("Cannot read this file: {}", &[&e]).into());
                self.opened = None;
                return;
            }
        };
        self.show_file(ui, volume, path, entry, data);
    }

    /// The opened file's name, disk, path and size, over it.
    fn header(&self, ui: &AppWindow, volume: usize, path: &str, entry: &DirEntry) {
        let explorer = ui.global::<Explorer>();
        explorer.set_name(entry.name.as_str().into());
        explorer.set_about(self.about(volume, path, entry.size).into());
        explorer.set_dirty(false);
        explorer.set_note(SharedString::new());
    }

    /// Under the opened file's name: its disk, path and size.
    fn about(&self, volume: usize, path: &str, size: u64) -> String {
        let disk = self.volumes.get(volume).map(|v| v.name.clone()).unwrap_or_default();
        format!("{disk} · {path} · {}", super::format_file_size(size))
    }

    /// No file open any more (deleted): nothing on the right, nothing to save.
    fn close_opened(&mut self, ui: &AppWindow) {
        self.opened = None;
        let explorer = ui.global::<Explorer>();
        explorer.set_kind(SharedString::new());
        explorer.set_dirty(false);
    }

    /// Shows the file `path` of disk `volume`, `data`, as its kind (after
    /// `header`).
    fn show_file(&mut self, ui: &AppWindow, volume: usize, path: String, entry: &DirEntry, data: Vec<u8>) {
        let explorer = ui.global::<Explorer>();
        let mut kind = kind_of(&entry.name, &data);
        let writable = self.volumes.get(volume).is_some_and(|v| v.writable) && !entry.read_only;
        let (mut crlf, mut bom) = (false, false);
        match kind {
            Kind::Text => {
                let (text, has_crlf, has_bom) = text_for_editing(&data);
                (crlf, bom) = (has_crlf, has_bom);
                explorer.set_text(text.into());
                explorer.set_writable(writable);
                explorer.set_opened(explorer.get_opened() + 1);
                if !writable {
                    explorer.set_note(tr("This disk or file is read-only: changes can't be saved.").into());
                }
            }
            Kind::Picture => match super::decode_picture(&data) {
                Some(image) => {
                    let size = image.size();
                    explorer.set_picture(image);
                    explorer.set_note(format!("{} × {}", size.width, size.height).into());
                }
                None => {
                    kind = Kind::Bytes;
                    explorer.set_note(tr("This picture can't be shown.").into());
                }
            },
            _ => {}
        }
        if kind == Kind::Program {
            let facts = program_facts(&data);
            if facts.is_none() {
                explorer.set_note(tr("This isn't a valid EFI program.").into());
            }
            explorer.set_program_facts(ModelRc::new(VecModel::from(facts.unwrap_or_default())));
        }
        if matches!(kind, Kind::Program | Kind::Bytes) {
            explorer.set_hex(ModelRc::new(VecModel::from(hex_rows(&data))));
            if data.len() > BYTES_SHOWN && explorer.get_note().is_empty() {
                let shown = super::format_file_size(BYTES_SHOWN as u64);
                explorer.set_note(trf("The first {} of {}.", &[&shown, &super::format_file_size(data.len() as u64)]).into());
            }
        }
        explorer.set_kind(
            match kind {
                Kind::Text => "text",
                Kind::Picture => "picture",
                Kind::Program => "program",
                Kind::Bytes => "bytes",
            }
            .into(),
        );
        log::info!("files: opened {path} on disk {volume} ({kind:?}, {} bytes)", data.len());
        self.opened = Some(Opened { volume, path, name: entry.name.clone(), data, crlf, bom });
    }
}

/// What the Files screen does through the rest of the app.
pub struct Actions {
    /// Starts a target, with the screen's transition.
    pub start: Rc<dyn Fn(String, Target)>,
    /// Asks for a disk's passphrase; then says whether it was unlocked.
    pub unlock: super::unlock::Unlock,
    /// A picture copied next to slingshot.toml: makes it the wallpaper or splash.
    pub picture_added: Rc<dyn Fn(PictureFor, String)>,
    /// Adds a boot entry for a file (disk, path, title).
    pub add_boot_entry: Rc<dyn Fn(usize, String, String)>,
    pub show_status: Rc<dyn Fn(&str)>,
}

/// Wires the Files screen.
pub fn wire(ui: &AppWindow, backend: Rc<dyn Backend>, actions: Actions) {
    let files: Rc<RefCell<Files>> = Rc::default();
    let explorer = ui.global::<Explorer>();
    let actions = Rc::new(actions);

    {
        let (files, ui_weak, backend) = (files.clone(), ui.as_weak(), backend.clone());
        explorer.on_shown(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut f = files.borrow_mut();
            // The disks again: one may have come, gone or been unlocked.
            let before = f.nodes.len();
            f.reload_volumes(&*backend);
            let current = if f.nodes.len() == before { ui.global::<Explorer>().get_current().max(0) as usize } else { 0 };
            f.show(&ui, &*backend, current);
        });
    }
    {
        let (files, ui_weak, backend) = (files.clone(), ui.as_weak(), backend.clone());
        explorer.on_selected(move |index| {
            let Some(ui) = ui_weak.upgrade() else { return };
            files.borrow().show_selected(&ui, &*backend, index.max(0) as usize);
        });
    }
    {
        // → : expands a folder, or goes into one already expanded; opens a file.
        let (files, ui_weak, backend) = (files.clone(), ui.as_weak(), backend.clone());
        explorer.on_expand(move |index| {
            let Some(ui) = ui_weak.upgrade() else { return index };
            let i = index.max(0) as usize;
            let mut f = files.borrow_mut();
            if i >= f.nodes.len() {
                return index;
            }
            if !f.expandable(i) {
                drop(f);
                ui.global::<Explorer>().invoke_open(index);
                return index;
            }
            if f.nodes[i].expanded {
                let has_child = f.nodes.get(i + 1).is_some_and(|n| n.depth > f.nodes[i].depth);
                return if has_child { index + 1 } else { index };
            }
            if let Err(e) = f.expand(i, &*backend) {
                ui.global::<Explorer>().set_message(trf("Cannot read this folder: {}", &[&e]).into());
                return index;
            }
            f.show(&ui, &*backend, i);
            index
        });
    }
    {
        // ← : collapses a folder, or goes to the one it's in.
        let (files, ui_weak, backend) = (files.clone(), ui.as_weak(), backend.clone());
        explorer.on_collapse(move |index| {
            let Some(ui) = ui_weak.upgrade() else { return index };
            let i = index.max(0) as usize;
            let mut f = files.borrow_mut();
            if i >= f.nodes.len() {
                return index;
            }
            if f.nodes[i].expanded {
                f.collapse(i);
                f.show(&ui, &*backend, i);
                return index;
            }
            f.parent(i).map_or(index, |p| p as i32)
        });
    }
    {
        // Enter or a double click: a folder expands or collapses, a file opens.
        let (files, ui_weak, backend, actions) = (files.clone(), ui.as_weak(), backend.clone(), actions.clone());
        explorer.on_open(move |index| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let i = index.max(0) as usize;
            let mut f = files.borrow_mut();
            let Some(node) = f.nodes.get(i) else { return };
            match &node.what {
                What::Volume(_) | What::Entry { entry: DirEntry { is_dir: true, .. }, .. } => {
                    if node.expanded {
                        f.collapse(i);
                    } else if let Err(e) = f.expand(i, &*backend) {
                        ui.global::<Explorer>().set_message(trf("Cannot read this folder: {}", &[&e]).into());
                        return;
                    }
                    f.show(&ui, &*backend, i);
                }
                What::Entry { volume, folders, entry } => {
                    let (volume, folders, entry) = (*volume, folders.clone(), entry.clone());
                    f.open_file(&ui, &*backend, volume, &folders, &entry);
                }
                What::Unreadable(v) => {
                    let Some(v) = f.unreadable.get(*v) else { return };
                    match v.unlock {
                        Some(id) => {
                            // An encrypted disk: its passphrase, then its files.
                            let locked = LockedVolume { id, name: v.name.clone(), detail: v.detail.clone() };
                            drop(f);
                            let (files, ui_weak, backend) = (files.clone(), ui.as_weak(), backend.clone());
                            (actions.unlock)(
                                locked,
                                Box::new(move |unlocked| {
                                    let Some(ui) = ui_weak.upgrade() else { return };
                                    if !unlocked {
                                        return;
                                    }
                                    let opened = backend.open_unlocked(id);
                                    let mut f = files.borrow_mut();
                                    let before = f.volumes.clone();
                                    f.reload_volumes(&*backend);
                                    if let Err(e) = opened {
                                        ui.global::<Explorer>().set_message(e.into());
                                        return;
                                    }
                                    // The disk unlocked, expanded.
                                    let new = f.volumes.iter().position(|v| !before.contains(v));
                                    let row = new.and_then(|v| f.nodes.iter().position(|n| matches!(n.what, What::Volume(x) if x == v))).unwrap_or(0);
                                    let _ = f.expand(row, &*backend);
                                    f.show(&ui, &*backend, row);
                                }),
                            );
                        }
                        None => {
                            let why = if crate::parse::fstype::is_encrypted(v.format) {
                                trf("{} is encrypted in a way Slingshot can't unlock.", &[&v.name])
                            } else {
                                trf("Slingshot can't read {} disks: this computer has no driver for them.", &[&v.format])
                            };
                            ui.global::<Explorer>().set_message(why.into());
                        }
                    }
                }
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        explorer.on_find(move |typed| {
            if typed.chars().any(|c| c.is_control() || ('\u{e000}'..='\u{f8ff}').contains(&c)) {
                return -2;
            }
            let Some(ui) = ui_weak.upgrade() else { return -1 };
            let rows = ui.global::<Explorer>().get_rows();
            let names: Vec<String> = rows.iter().map(|r| String::from(r.name.as_str())).collect();
            super::find_row(&names, &typed, ui.global::<Explorer>().get_current().max(0) as usize).map_or(-1, |i| i as i32)
        });
    }
    {
        // A path typed in the location field: on the disk of the current
        // row (from its folder unless it starts with \), else on any disk.
        // The folders on the way expand.
        let (files, ui_weak, backend) = (files.clone(), ui.as_weak(), backend.clone());
        explorer.on_go(move |path| {
            let Some(ui) = ui_weak.upgrade() else { return false };
            let mut f = files.borrow_mut();
            let absolute = path.starts_with(['\\', '/']);
            let current = ui.global::<Explorer>().get_current().max(0) as usize;
            let candidates: Vec<(usize, Vec<String>)> = match f.place(current) {
                Some((v, folders)) => alloc::vec![(v, if absolute { Vec::new() } else { folders })],
                None => (0..f.volumes.len()).map(|v| (v, Vec::new())).collect(),
            };
            let mut missing = String::new();
            for (volume, start) in candidates {
                match super::resolve_path(&*backend, volume, start, &path) {
                    Ok((folders, select)) => {
                        let mut row = f.nodes.iter().position(|n| matches!(n.what, What::Volume(v) if v == volume)).unwrap_or(0);
                        let _ = f.expand(row, &*backend);
                        for name in folders.iter().map(String::as_str).chain(select.as_deref()) {
                            let Some(child) = f.child_named(row, name) else { break };
                            row = child;
                            if f.expandable(row) {
                                let _ = f.expand(row, &*backend);
                            }
                        }
                        f.show(&ui, &*backend, row);
                        return true;
                    }
                    Err(part) if missing.is_empty() => missing = part,
                    Err(_) => {}
                }
            }
            ui.global::<Explorer>().set_message(trf("Not found: {}", &[&missing]).into());
            false
        });
    }
    {
        let (files, ui_weak, backend, actions) = (files.clone(), ui.as_weak(), backend.clone(), actions.clone());
        explorer.on_save(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut f = files.borrow_mut();
            let Some(o) = &mut f.opened else { return };
            let data = text_to_save(&ui.global::<Explorer>().get_text(), o.crlf, o.bom);
            match backend.write_volume_file(o.volume, &o.path, &data) {
                Ok(()) => {
                    log::info!("files: saved {} on disk {} ({} bytes)", o.path, o.volume, data.len());
                    (actions.show_status)(&trf("Saved {}", &[&o.name]));
                    let (volume, path) = (o.volume, o.path.clone());
                    o.data = data;
                    ui.global::<Explorer>().set_dirty(false);
                    f.refresh_entry(&*backend, volume, &path);
                    f.show(&ui, &*backend, ui.global::<Explorer>().get_current().max(0) as usize);
                }
                Err(e) => {
                    log::warn!("files: saving {} on disk {}: {e}", o.path, o.volume);
                    (actions.show_status)(&trf("Could not save {}: {}", &[&o.name, &e]));
                }
            }
        });
    }
    {
        // Ctrl+F: the next match in the text, selected by the editor.
        let ui_weak = ui.as_weak();
        explorer.on_find_in_text(move |query, from| {
            let Some(ui) = ui_weak.upgrade() else { return false };
            let explorer = ui.global::<Explorer>();
            match find_text(&explorer.get_text(), &query, from.max(0) as usize) {
                Some((start, end, n, count)) => {
                    explorer.set_found_start(start as i32);
                    explorer.set_found_end(end as i32);
                    explorer.set_find_status(trf("{} of {}", &[&n, &count]).into());
                    true
                }
                None => {
                    explorer.set_find_status(if query.is_empty() { String::new() } else { tr("Not found") }.into());
                    false
                }
            }
        });
    }
    {
        // A new folder, or a new empty file (opened to be written), in the
        // current row's folder.
        let (files, ui_weak, backend, actions) = (files.clone(), ui.as_weak(), backend.clone(), actions.clone());
        explorer.on_new_entry(move |name, folder| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let name = match check_name(&name) {
                Ok(name) => name,
                Err(why) => return (actions.show_status)(&why),
            };
            let mut f = files.borrow_mut();
            let Some((volume, folders)) = f.place(ui.global::<Explorer>().get_current().max(0) as usize) else { return };
            if taken(&*backend, volume, &folders, name) {
                return (actions.show_status)(&trf("{} is already in this folder.", &[&name]));
            }
            let path = super::folder_path(&folders, Some(name));
            let made = if folder { backend.create_dir(volume, &path) } else { backend.write_volume_file(volume, &path, &[]) };
            if let Err(e) = made {
                log::warn!("files: making {path} on disk {volume}: {e}");
                (actions.show_status)(&trf("Could not make {}: {}", &[&name, &e]));
                return;
            }
            log::info!("files: made {path} on disk {volume}");
            let row = f.reload_folder(&ui, &*backend, volume, &folders, Some(name));
            // A new file: opened, empty (not read again); with changes not
            // saved in the file open, after asking about them.
            let new = row.and_then(|row| f.child_named(row, name));
            if let (false, Some(i)) = (folder, new)
                && ui.global::<Explorer>().get_dirty()
            {
                ui.global::<Explorer>().set_discard_row(i as i32);
            } else if let (false, Some(What::Entry { entry, .. })) = (folder, new.map(|i| &f.nodes[i].what)) {
                let (entry, path) = (entry.clone(), super::folder_path(&folders, Some(name)));
                f.header(&ui, volume, &path, &entry);
                f.show_file(&ui, volume, path, &entry, Vec::new());
            }
        });
    }
    {
        // Renames the current row's file or folder.
        let (files, ui_weak, backend, actions) = (files.clone(), ui.as_weak(), backend.clone(), actions.clone());
        explorer.on_rename(move |name| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let name = match check_name(&name) {
                Ok(name) => name,
                Err(why) => return (actions.show_status)(&why),
            };
            let mut f = files.borrow_mut();
            let current = ui.global::<Explorer>().get_current().max(0) as usize;
            let Some(What::Entry { volume, folders, entry }) = f.nodes.get(current).map(|n| &n.what) else { return };
            let (volume, folders, old) = (*volume, folders.clone(), entry.name.clone());
            if name == old {
                return;
            }
            // Only the case changing is fine.
            if !name.eq_ignore_ascii_case(&old) && taken(&*backend, volume, &folders, name) {
                return (actions.show_status)(&trf("{} is already in this folder.", &[&name]));
            }
            let path = super::folder_path(&folders, Some(&old));
            if let Err(e) = backend.rename(volume, &path, name) {
                log::warn!("files: renaming {path} on disk {volume} to {name}: {e}");
                (actions.show_status)(&trf("Could not rename {}: {}", &[&old, &e]));
                return;
            }
            log::info!("files: renamed {path} on disk {volume} to {name}");
            // The file open, or in the folder renamed: its new path (its
            // changes not saved stay).
            let new_path = super::folder_path(&folders, Some(name));
            if let Some(o) = &mut f.opened
                && o.volume == volume
            {
                // The file itself (nothing after), or one in the folder (`\` after).
                if let Some(rest) = o.path.strip_prefix(path.as_str()).filter(|r| r.is_empty() || r.starts_with('\\')) {
                    if rest.is_empty() {
                        o.name = name.into();
                        ui.global::<Explorer>().set_name(name.into());
                    }
                    o.path = format!("{new_path}{rest}");
                    let (path, size) = (o.path.clone(), o.data.len() as u64);
                    ui.global::<Explorer>().set_about(f.about(volume, &path, size).into());
                }
            }
            f.reload_folder(&ui, &*backend, volume, &folders, Some(name));
        });
    }
    {
        // Deletes the current row's file or empty folder (after Delete's confirmation).
        let (files, ui_weak, backend, actions) = (files.clone(), ui.as_weak(), backend.clone(), actions.clone());
        explorer.on_delete(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut f = files.borrow_mut();
            let current = ui.global::<Explorer>().get_current().max(0) as usize;
            let Some(What::Entry { volume, folders, entry }) = f.nodes.get(current).map(|n| &n.what) else { return };
            let (volume, folders, name) = (*volume, folders.clone(), entry.name.clone());
            // Then the next row in the folder, else the one before (else the folder).
            let depth = f.nodes[current].depth;
            let sibling = |j: usize| f.nodes[j].depth == depth;
            let next = (current + 1..f.nodes.len()).take_while(|&j| f.nodes[j].depth >= depth).find(|&j| sibling(j));
            let before = (0..current).rev().take_while(|&j| f.nodes[j].depth >= depth).find(|&j| sibling(j));
            let select = next.or(before).map(|j| f.name(j));
            let path = super::folder_path(&folders, Some(&name));
            if let Err(e) = backend.delete(volume, &path) {
                log::warn!("files: deleting {path} on disk {volume}: {e}");
                (actions.show_status)(&trf("Could not delete {}: {}", &[&name, &e]));
                return;
            }
            log::info!("files: deleted {path} on disk {volume}");
            (actions.show_status)(&trf("Deleted {}", &[&name]));
            if f.opened.as_ref().is_some_and(|o| o.volume == volume && o.path == path) {
                f.close_opened(&ui);
            }
            f.reload_folder(&ui, &*backend, volume, &folders, select.as_deref());
        });
    }
    {
        // Ctrl+C: the current file, to paste in another folder.
        let (files, ui_weak, backend, actions) = (files.clone(), ui.as_weak(), backend.clone(), actions.clone());
        explorer.on_copy(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut f = files.borrow_mut();
            let current = ui.global::<Explorer>().get_current().max(0) as usize;
            let Some(What::Entry { volume, folders, entry }) = f.nodes.get(current).map(|n| &n.what) else { return };
            if entry.is_dir {
                return;
            }
            let clip = Clip { volume: *volume, path: super::folder_path(folders, Some(&entry.name)), name: entry.name.clone() };
            (actions.show_status)(&trf("{} copied: Paste puts it in the folder chosen.", &[&clip.name]));
            f.clipboard = Some(clip);
            f.show_selected(&ui, &*backend, current);
        });
    }
    {
        // Ctrl+V: the file copied, into the current row's folder.
        let (files, ui_weak, backend, actions) = (files.clone(), ui.as_weak(), backend.clone(), actions.clone());
        explorer.on_paste(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut f = files.borrow_mut();
            let Some(clip) = &f.clipboard else { return };
            let (from_volume, from, name) = (clip.volume, clip.path.clone(), clip.name.clone());
            let Some((volume, folders)) = f.place(ui.global::<Explorer>().get_current().max(0) as usize) else { return };
            // Not over a file.
            if taken(&*backend, volume, &folders, &name) {
                return (actions.show_status)(&trf("{} is already in this folder.", &[&name]));
            }
            let path = super::folder_path(&folders, Some(&name));
            let copied = backend.read_volume_file(from_volume, &from).and_then(|data| backend.write_volume_file(volume, &path, &data));
            if let Err(e) = copied {
                log::warn!("files: copying {from} on disk {from_volume} to {path} on disk {volume}: {e}");
                (actions.show_status)(&trf("Could not copy {}: {}", &[&name, &e]));
                return;
            }
            log::info!("files: copied {from} on disk {from_volume} to {path} on disk {volume}");
            (actions.show_status)(&trf("Pasted {}", &[&name]));
            f.reload_folder(&ui, &*backend, volume, &folders, Some(&name));
        });
    }
    {
        let (files, ui_weak) = (files.clone(), ui.as_weak());
        explorer.on_revert(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let f = files.borrow();
            let Some(o) = &f.opened else { return };
            let explorer = ui.global::<Explorer>();
            explorer.set_text(text_for_editing(&o.data).0.into());
            explorer.set_dirty(false);
        });
    }
    {
        let (files, actions) = (files.clone(), actions.clone());
        explorer.on_start(move || {
            let f = files.borrow();
            let Some(o) = &f.opened else { return };
            let target = Target::File { volume: o.volume, path: o.path.clone() };
            let name = o.name.clone();
            drop(f);
            (actions.start)(name, target);
        });
    }
    {
        let (files, actions) = (files.clone(), actions.clone());
        explorer.on_add_to_menu(move || {
            let f = files.borrow();
            let Some(o) = &f.opened else { return };
            let title = o.name.rsplit_once('.').map_or(o.name.as_str(), |(stem, _)| stem).to_string();
            let (volume, path) = (o.volume, o.path.clone());
            drop(f);
            (actions.add_boot_entry)(volume, path, title);
        });
    }
    for screen in [ScreenPicture::Wallpaper, ScreenPicture::Splash] {
        let (files, backend, actions) = (files.clone(), backend.clone(), actions.clone());
        let use_as = move || {
            let f = files.borrow();
            let Some(o) = &f.opened else { return };
            // Copied next to slingshot.toml, where Settings finds pictures.
            match backend.write_file(&o.name, &o.data) {
                Ok(()) => {
                    let name = o.name.clone();
                    drop(f);
                    (actions.picture_added)(PictureFor::Screen(screen), name);
                    (actions.show_status)(&match screen {
                        ScreenPicture::Wallpaper => tr("The wallpaper is changed."),
                        ScreenPicture::Splash => tr("The splash picture is changed."),
                    });
                }
                Err(e) => (actions.show_status)(&trf("Could not add the picture: {}", &[&e])),
            }
        };
        match screen {
            ScreenPicture::Wallpaper => explorer.on_use_as_wallpaper(use_as),
            ScreenPicture::Splash => explorer.on_use_as_splash(use_as),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(kind_of("BOOTX64.EFI", b"MZ"), Kind::Program);
        assert_eq!(kind_of("loader.conf", b"timeout 3\n"), Kind::Text);
        assert_eq!(kind_of("BCD", b"regf\0\0"), Kind::Bytes);
        assert_eq!(kind_of("latin1.txt", b"caf\xe9"), Kind::Bytes);
        assert_eq!(kind_of("big.txt", &alloc::vec![b'a'; TEXT_MAX + 1]), Kind::Bytes);
    }

    #[test]
    fn line_endings_and_bom_kept() {
        let (text, crlf, bom) = text_for_editing(b"\xef\xbb\xbfa\r\nb\r\n");
        assert_eq!((text.as_str(), crlf, bom), ("a\nb\n", true, true));
        assert_eq!(text_to_save("a\nb\nc\n", crlf, bom), b"\xef\xbb\xbfa\r\nb\r\nc\r\n");
        let (text, crlf, bom) = text_for_editing(b"a\nb");
        assert_eq!(text_to_save(&text, crlf, bom), b"a\nb");
    }

    #[test]
    fn find_wraps_and_ignores_case() {
        let text = "timeout 3\nTimeout 5\neditor no\n";
        assert_eq!(find_text(text, "timeout", 0), Some((0, 7, 1, 2)));
        assert_eq!(find_text(text, "TIMEOUT", 7), Some((10, 17, 2, 2)));
        assert_eq!(find_text(text, "timeout", 17), Some((0, 7, 1, 2)), "back to the first");
        assert_eq!(find_text(text, "nothing", 0), None);
        assert_eq!(find_text(text, "", 0), None);
        assert_eq!(find_text("é é", "é", 1), Some((3, 5, 2, 2)), "byte offsets");
    }

    #[test]
    fn names() {
        assert_eq!(check_name("  new folder "), Ok("new folder"));
        assert_eq!(check_name("a.efi"), Ok("a.efi"));
        for bad in ["", " ", ".", "..", "a\\b", "a/b", "a:b", "a*", "a?", "\"a\"", "a<b>", "a|b", "a\nb"] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn hex() {
        let rows = hex_rows(b"MZ\x90\0abcdefghijklmnopqrstuvwxyz");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].offset, "00000000");
        assert_eq!(rows[0].hex, "4D 5A 90 00 61 62 63 64  65 66 67 68 69 6A 6B 6C");
        assert_eq!(rows[0].ascii, "MZ..abcdefghijkl");
        assert_eq!(rows[1].offset, "00000010");
        assert_eq!(hex_rows(&alloc::vec![0; 10 * BYTES_SHOWN]).len(), BYTES_SHOWN / 16);
    }

    #[test]
    fn details() {
        let e = DirEntry {
            name: "a.txt".into(),
            size: 2048,
            modified: Some(DateTime { year: 2026, month: 11, day: 9, hour: 14, minute: 5 }),
            read_only: true,
            ..Default::default()
        };
        assert_eq!(entry_details(&e), "2 KB · 2026-11-09 14:05 · read-only");
    }
}
