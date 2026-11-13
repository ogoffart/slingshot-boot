// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Unlocking LUKS2 disks before starting Linux: the partitions with a LUKS2
//! header, the passphrase check (on another processor when the firmware
//! offers one, so the spinner keeps turning), and the handoff: the entry's
//! initrds plus a small one with the keys (`parse::handoff`), given to the
//! kernel through the LINUX_EFI_INITRD_MEDIA protocol.
//!
//! A disk unlocked from the file browser is also offered to the firmware,
//! decrypted and read-only (`Mapped`): a file system driver can then read
//! it, as for any other disk.
//!
//! The keys only live in memory: wiped once they're copied into the handoff
//! (whether the kernel starts or not) or when the start is cancelled, and
//! the handoff buffer is wiped when the kernel exits the boot services.

use crate::tr::tr;
use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use core::time::Duration;

use uefi::boot::{self, EventType, Tpl};
use uefi::proto::device_path::DevicePath;
use uefi::proto::media::block::BlockIO;
use uefi::proto::pi::mp::MpServices;
use uefi::{Event, Guid, Handle, Identify, Status, guid};
use uefi_raw::protocol::block::{BlockIoMedia, BlockIoProtocol, Lba};
use uefi_raw::protocol::media::LoadFile2Protocol;

use crate::app::{LockedVolume, Secret, UnlockDone, UnlockResult, format_bytes};
use crate::parse::handoff::{self, Unlocked};
use crate::parse::luks::{self, Attempt, Decryptor, Header};

use super::dir::read_disk as read;
use crate::uefi_platform::open_shared;

/// A partition with a LUKS2 header.
struct Disk {
    handle: Handle,
    media_id: u32,
    device_path: Vec<u8>,
    size: u64,
    header: Header,
    /// The GPT partition's GUID, lowercase.
    partuuid: Option<String>,
    /// Its partition number (`cryptdevice=/dev/nvme0n1p2` names it so).
    number: Option<u32>,
    detail: String,
}

impl Disk {
    fn is(&self, wanted: &handoff::Wanted) -> bool {
        wanted.matches(&self.header.uuid, self.partuuid.as_deref(), self.number)
    }
}

/// An unlocked volume, waiting to be handed over.
struct Opened {
    /// In `Luks::disks`.
    disk: usize,
    volume_key: Secret,
    passphrase: Secret,
}

pub struct Luks {
    /// Found the first time they're needed.
    disks: RefCell<Option<Rc<Vec<Disk>>>>,
    opened: RefCell<Vec<Opened>>,
    /// The disks offered decrypted to the firmware: `(disk, device path,
    /// the protocol's data)`.
    mapped: RefCell<Vec<(usize, Vec<u8>, *mut Mapped)>>,
    timer: Rc<slint::Timer>,
    /// The check under way: the other processors run it, the timer gets
    /// it back. Taken back by the next `unlock` if the timer couldn't.
    in_flight: Rc<Cell<*mut Attempt>>,
    /// Counts the checks, and the `forget`s: a check whose number is no
    /// longer the current one was cancelled, and its key is dropped.
    generation: Rc<Cell<u32>>,
}

impl Default for Luks {
    fn default() -> Self {
        Luks {
            disks: Default::default(),
            opened: Default::default(),
            mapped: Default::default(),
            timer: Default::default(),
            in_flight: Rc::new(Cell::new(core::ptr::null_mut())),
            generation: Default::default(),
        }
    }
}


fn find_disks() -> Vec<Disk> {
    let mut disks = Vec::new();
    let Ok(handles) = boot::find_handles::<BlockIO>() else { return disks };
    for handle in handles {
        let Some(block) = open_shared::<BlockIO>(handle) else { continue };
        let media = block.media();
        if !media.is_logical_partition() || !media.is_media_present() {
            continue;
        }
        let (media_id, size) = (media.media_id(), (media.last_block() + 1) * u64::from(media.block_size()));
        drop(block);
        let Some(first) = read(handle, media_id, 0, 4096) else { continue };
        if !luks::is_luks(&first) {
            continue;
        }
        let header = luks::header_size(&first)
            .and_then(|size| luks::parse(&read(handle, media_id, 0, size as usize).ok_or(luks::Error::Read)?));
        let header = match header {
            Ok(h) => h,
            Err(e) => {
                log::info!("LUKS partition not usable: {e:?}");
                continue;
            }
        };
        let device_path = open_shared::<DevicePath>(handle).map(|dp| dp.as_bytes().to_vec());
        let partuuid = device_path.as_deref().and_then(crate::parse::detect::partition_uuid);
        let number = device_path.as_deref().and_then(crate::parse::detect::partition_number);
        let what = device_path
            .as_deref()
            .map(|dp| crate::parse::load_option::describe_volume(dp).1)
            .unwrap_or_else(|| tr("Disk"));
        log::info!("LUKS2 {} on {what}: {} key slots, tokens {:?}", header.uuid, header.keyslots.len(), header.tokens);
        let device_path = device_path.unwrap_or_default();
        disks.push(Disk { handle, media_id, device_path, size, header, partuuid, number, detail: format!("{what} · {}", format_bytes(size)) });
    }
    disks
}

/// Whether a kernel on the file system `fs_device_path` may get the keys of
/// `disk`: it's on Slingshot's own disk, or on that disk itself (or inside
/// it, decrypted). Not a USB stick's: one with a signed kernel, its own
/// initrd and the disk's UUID on its command line would get the
/// passphrase from Slingshot's genuine prompt.
fn trusted(disk: &Disk, fs_device_path: &[u8]) -> bool {
    use crate::parse::load_option::disk_part;
    let fs_disk = disk_part(fs_device_path);
    let ok = !fs_disk.is_empty() && (fs_disk == disk_part(&super::own_fs_device_path()) || fs_disk == disk_part(&disk.device_path));
    if !ok {
        log::info!("{}: the kernel is on another disk, it doesn't get the keys", disk.header.uuid);
    }
    ok
}

/// Whether the kernel `program` (on the file system `handle`) can take the
/// keys: it has its initrds on the command line (`initrd=`; a unified kernel
/// image carries its own), and finds them
/// through LINUX_EFI_INITRD_MEDIA once they leave it (Linux 5.8 and later).
fn can_hand_over(handle: Handle, program: &str, options: &str) -> bool {
    if !options.split_ascii_whitespace().any(|o| o.starts_with("initrd=")) {
        return false;
    }
    let new_enough = super::dir::read_file_start(handle, program, 4096).is_some_and(|k| handoff::takes_initrd_media(&k));
    if !new_enough {
        log::info!("{program}: older than Linux 5.8, it only reads initrd=: Linux asks for the passphrase");
    }
    new_enough
}

impl Luks {
    fn disks(&self) -> Rc<Vec<Disk>> {
        self.disks.borrow_mut().get_or_insert_with(|| Rc::new(find_disks())).clone()
    }

    /// After devices were connected (the menu is up before: the first
    /// look may have been too early) or a disk plugged in: the encrypted
    /// partitions found since, added after the others (whose positions
    /// the unlocked ones keep).
    pub fn refresh(&self) {
        let mut cached = self.disks.borrow_mut();
        let Some(known) = cached.as_ref() else { return };
        let new: Vec<Disk> = find_disks().into_iter().filter(|d| !known.iter().any(|k| k.device_path == d.device_path)).collect();
        if new.is_empty() {
            return;
        }
        let mut all: Vec<Disk> = Vec::new();
        // Moved out of the old list when nothing else holds it, else read again.
        let old = cached.take().unwrap();
        match Rc::try_unwrap(old) {
            Ok(old) => all.extend(old),
            Err(old) => {
                *cached = Some(old);
                return;
            }
        }
        all.extend(new);
        *cached = Some(Rc::new(all));
    }

    /// The disk with this device path, for `unlock` and `open`, if
    /// Slingshot can read its data.
    pub fn disk_at(&self, device_path: &[u8]) -> Option<usize> {
        self.disks().iter().position(|d| d.device_path == device_path && d.header.segment.is_some())
    }

    /// Whether `device_path` is a disk offered decrypted (`open`), or the
    /// encrypted partition of one.
    pub fn is_mapped(&self, device_path: &[u8]) -> bool {
        let disks = self.disks();
        self.mapped.borrow().iter().any(|(d, dp, _)| dp == device_path || disks[*d].device_path == device_path)
    }

    /// Offers the unlocked disk `id` (`unlock` first) decrypted to the
    /// firmware, so that its file system drivers read it. Read-only.
    pub fn open(&self, id: usize) -> Result<(), String> {
        if self.mapped.borrow().iter().any(|(d, _, _)| *d == id) {
            return Ok(());
        }
        let disks = self.disks();
        let disk = disks.get(id).ok_or_else(|| tr("This disk is gone."))?;
        let segment = disk.header.segment.as_ref().ok_or_else(|| tr("Slingshot can't read this disk's data."))?;
        let opened = self.opened.borrow();
        let key = &opened.iter().find(|o| o.disk == id).ok_or_else(|| tr("This disk is locked."))?.volume_key;
        let decryptor = Decryptor::new(segment, &key.0).map_err(|e| describe(&e))?;
        drop(opened);
        let size = segment.size.unwrap_or(disk.size.saturating_sub(segment.offset));
        if segment.offset.checked_add(size).is_none_or(|end| end > disk.size) {
            return Err(tr("This disk's encryption couldn't be read."));
        }
        let (device_path, mapped) = Mapped::install(disk, segment.offset, size, decryptor)?;
        log::info!("{}: decrypted, {} bytes, {}-byte sectors", disk.detail, size, segment.sector_size);
        self.mapped.borrow_mut().push((id, device_path, mapped));
        Ok(())
    }

    /// Wipes the disks unlocked so far; a check under way gives nothing.
    pub fn forget(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        self.opened.borrow_mut().clear();
    }

    /// Before starting something that isn't a kernel getting the keys
    /// (Windows, any EFI program): the keys wiped, and with `close`, the
    /// decrypted disks too (they read nothing more).
    pub fn forget_all(&self, close: bool) {
        self.forget();
        if close {
            for (_, _, mapped) in self.mapped.borrow().iter() {
                // SAFETY: a `Mapped` lives forever; this processor is the
                // only one that uses it.
                unsafe { Mapped::close(*mapped) };
            }
        }
    }

    /// The disks the kernel `program` on the file system `fs_device_path`
    /// needs unlocked, with `options`.
    pub fn locked_volumes(&self, fs_device_path: &[u8], program: &str, options: &str) -> Vec<LockedVolume> {
        let wanted = handoff::wanted_volumes(options);
        if wanted.is_empty() || !super::dir::handle_of(fs_device_path).is_some_and(|h| can_hand_over(h, program, options)) {
            return Vec::new();
        }
        let disks = self.disks();
        let opened = self.opened.borrow();
        wanted
            .iter()
            .filter_map(|w| {
                let id = disks.iter().position(|d| d.is(w)).filter(|id| !opened.iter().any(|o| o.disk == *id))?;
                if !trusted(&disks[id], fs_device_path) {
                    return None;
                }
                // A TPM or a security key opens it: the initrd does that.
                (!disks[id].header.unlocks_by_itself()).then(|| LockedVolume {
                    id,
                    name: w.name.clone(),
                    detail: disks[id].detail.clone(),
                })
            })
            .collect()
    }

    /// Stops the passphrase check under way: its result is
    /// `Error::Cancelled`, soon (Argon2 stops at its next slice).
    pub fn cancel(&self) {
        CANCEL.store(true, Ordering::Release);
    }

    pub fn unlock(self: &Rc<Self>, volume: usize, passphrase: Secret, done: UnlockDone) {
        // A cancelled check still running on the other processors: this one
        // waits for it (they share the crew), a moment at most.
        if !wait_for_processors() {
            return done(UnlockResult::Failed(tr("A passphrase check is still running. Try again in a moment.")));
        }
        // Its result, if the timer didn't take it back.
        let previous = self.in_flight.replace(core::ptr::null_mut());
        if !previous.is_null() {
            // SAFETY: no other processor runs it any more.
            drop(unsafe { Box::from_raw(previous) });
        }
        CANCEL.store(false, Ordering::Release);
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        // Before `Attempt::new` builds argon2 (and sha2), which looks once
        // whether it can use AVX2.
        enable_avx2();
        let disks = self.disks();
        let Some(disk) = disks.get(volume) else {
            return done(UnlockResult::Failed(tr("This disk is gone.")));
        };
        let attempt = Attempt::new(&disk.header, &passphrase.0, |offset, len| read(disk.handle, disk.media_id, offset, len));
        let attempt = match attempt {
            Ok(a) => Box::new(a),
            Err(e) => return done(UnlockResult::Failed(describe(&e))),
        };
        let this = self.clone();
        let finish = move |attempt: Box<Attempt>| {
            let result = match attempt.result() {
                // Cancelled, or forgotten, while checking.
                Ok(_) if this.generation.get() != generation => UnlockResult::Failed(String::new()),
                Ok(key) => {
                    this.opened.borrow_mut().push(Opened { disk: volume, volume_key: Secret(key.to_vec()), passphrase });
                    UnlockResult::Unlocked
                }
                Err(luks::Error::WrongPassphrase) => UnlockResult::WrongPassphrase,
                Err(luks::Error::Cancelled) => UnlockResult::Cancelled,
                Err(e) => UnlockResult::Failed(describe(&e)),
            };
            drop(attempt);
            done(result);
        };
        for slot in &disk.header.keyslots {
            if let luks::Kdf::Argon2 { time, memory, cpus, .. } = slot.kdf {
                log::info!("key slot {}: Argon2, {} MiB, {time} passes, {cpus} lanes", slot.id, memory / 1024);
            }
        }
        let lanes = attempt.lanes();
        let attempt = Box::into_raw(attempt);
        self.in_flight.set(attempt);
        let in_flight = self.in_flight.clone();
        let started = crate::uefi_platform::uptime();
        let took = move || (crate::uefi_platform::uptime() - started).as_millis();
        let events = start_on_other_processors(attempt, lanes);
        let timer = Rc::downgrade(&self.timer);
        let mut finish = Some(finish);
        match events {
            Some(mut events) => {
                self.timer.start(slint::TimerMode::Repeated, Duration::from_millis(30), move || {
                    close_signaled(&mut events);
                    if !events.is_empty() {
                        return;
                    }
                    if let Some(t) = timer.upgrade() {
                        t.stop();
                    }
                    if in_flight.replace(core::ptr::null_mut()) != attempt {
                        return;
                    }
                    // SAFETY: the other processors are done with it (signaled).
                    let mut attempt = unsafe { Box::from_raw(attempt) };
                    if !RAN.load(Ordering::Acquire) {
                        log::info!("that processor can't run AVX2 code: checking on this one");
                        attempt.run_here(&cancelled);
                    }
                    log::info!("passphrase checked in {} ms", took());
                    if let Some(finish) = finish.take() {
                        finish(attempt);
                    }
                });
            }
            None => {
                // On this processor: draw "Checking…" first.
                log::info!("checking the passphrase");
                self.timer.start(slint::TimerMode::SingleShot, Duration::from_millis(60), move || {
                    if in_flight.replace(core::ptr::null_mut()) != attempt {
                        return;
                    }
                    // SAFETY: nothing else has it.
                    let mut attempt = unsafe { Box::from_raw(attempt) };
                    // The screen doesn't change until it's done, but
                    // Escape still stops it.
                    let (last, pressed) = (Cell::new(0), Cell::new(false));
                    attempt.run_here(&|| {
                        pressed.set(pressed.get() || escape_pressed(&last));
                        pressed.get()
                    });
                    log::info!("passphrase checked in {} ms", took());
                    if let Some(finish) = finish.take() {
                        finish(attempt);
                    }
                });
            }
        }
    }

    /// The handoff for starting the kernel `program` with `options` from the
    /// file system `fs_device_path`: the new command line, and the initrds
    /// installed until the returned value is dropped. `None` when no
    /// unlocked disk is needed (or the initrds can't be read: Linux then
    /// asks for the passphrase itself).
    pub fn handoff(&self, fs_device_path: &[u8], program: &str, options: &str) -> Option<(String, Handoff)> {
        // Taken: wiped when this returns, the handoff or not (the initrd
        // has its own copy).
        let opened = core::mem::take(&mut *self.opened.borrow_mut());
        if opened.is_empty() {
            return None;
        }
        let wanted = handoff::wanted_volumes(options);
        let disks = self.disks();
        let volumes: Vec<Unlocked> = wanted
            .iter()
            .filter_map(|w| {
                let id = disks.iter().position(|d| d.is(w))?;
                let o = opened.iter().find(|o| o.disk == id)?;
                if !trusted(&disks[id], fs_device_path) {
                    return None;
                }
                Some(Unlocked { wanted: w, volume_key: &o.volume_key.0, passphrase: &o.passphrase.0 })
            })
            .collect();
        if volumes.is_empty() {
            return None;
        }
        let handle = super::dir::handle_of(fs_device_path)?;
        if !can_hand_over(handle, program, options) {
            return None;
        }
        let (initrds, rest) = handoff::split_initrds(options);
        let (archive, extra) = handoff::archive(&volumes, options);
        let archive = Secret(archive);
        // The archive first: the kernel doesn't always find one after a
        // compressed initrd (its lz4 decoder reads on into it), and the
        // distribution's has none of its files. Copied in once the initrds
        // are read: no copy of the keys is left when one can't be.
        let mut files = Vec::new();
        for initrd in &initrds {
            let Some(file) = super::dir::read_file(handle, initrd) else {
                log::info!("initrd {initrd}: cannot read it");
                return None;
            };
            files.push(file);
        }
        // Each file copied once, into a buffer of the right size.
        let start = archive.0.len().next_multiple_of(4);
        let mut data = Vec::with_capacity(start + files.iter().map(|f| f.len().next_multiple_of(4)).sum::<usize>());
        data.resize(start, 0);
        for file in files {
            data.extend_from_slice(&file);
            data.resize(data.len().next_multiple_of(4), 0);
        }
        data[..archive.0.len()].copy_from_slice(&archive.0);
        log::info!("handing {} unlocked disk(s) over: {} bytes of initrd", volumes.len(), data.len());
        let command_line = if extra.is_empty() { rest } else { format!("{rest} {extra}") };
        Some((command_line, Handoff::install(data)?))
    }
}

/// What the person is told; the details go to the log.
fn describe(e: &luks::Error) -> String {
    log::info!("LUKS: {e:?}");
    match e {
        luks::Error::OutOfMemory => tr("Not enough memory to check the passphrase."),
        luks::Error::Read => tr("The disk couldn't be read."),
        luks::Error::Unsupported(_) => tr("Slingshot can't unlock this kind of encrypted disk."),
        _ => tr("This disk's encryption couldn't be read."),
    }
}

/// Whether Escape was pressed (the other keys pressed meanwhile are
/// dropped: nothing is typed while checking). Asked at most every 50 ms:
/// `last` is when it was, in ms.
fn escape_pressed(last: &Cell<u64>) -> bool {
    let now = crate::uefi_platform::uptime().as_millis() as u64;
    if now.saturating_sub(last.get()) < 50 {
        return false;
    }
    last.set(now);
    let mut escape = false;
    uefi::system::with_stdin(|stdin| {
        while let Ok(Some(key)) = stdin.read_key() {
            escape |= key == uefi::proto::console::text::Key::Special(uefi::proto::console::text::ScanCode::ESCAPE);
        }
    });
    escape
}

// --- On other processors ---------------------------------------------------------
//
// The attempt runs on an application processor, so that this one keeps
// drawing. Others help it with Argon2's lanes, as cryptsetup does with
// threads: the KDF was tuned for that many. They can't call the firmware,
// so they wait for work spinning on `CREW`.

/// The programs started on other processors that haven't returned.
static RUNNING: AtomicUsize = AtomicUsize::new(0);

/// Set by this processor to stop the passphrase check the others run.
static CANCEL: AtomicBool = AtomicBool::new(false);

/// Whether `CANCEL` is set: the check's `stop` on the other processors.
fn cancelled() -> bool {
    CANCEL.load(Ordering::Acquire)
}

/// Counts a program on another processor as returned when dropped.
struct Returning;

impl Drop for Returning {
    fn drop(&mut self) {
        RUNNING.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Waits until no other processor runs Slingshot's code (a cancelled
/// passphrase check stops within a slice of Argon2): before starting
/// something, which may take the memory they use.
/// `false` if they still do after 10 s: nothing may start then.
pub fn wait_for_processors() -> bool {
    for _ in 0..20_000 {
        if RUNNING.load(Ordering::Acquire) == 0 {
            return true;
        }
        boot::stall(Duration::from_micros(500));
    }
    log::info!("other processors still busy after 10 s");
    false
}

/// The helpers' shared state. One attempt at a time.
struct Crew {
    /// The helpers that joined; `CLOSED` once the attempt starts: a helper
    /// that comes later leaves at once (else it would be waited for).
    joined: AtomicUsize,
    /// Bumped for each slice of lanes to compute, and to quit.
    round: AtomicUsize,
    quit: AtomicBool,
    /// The round's work: a `*const argon2::Segments`, which gives each
    /// segment out once.
    job: AtomicPtr<()>,
    /// The helpers done with the round.
    parked: AtomicUsize,
}

const CLOSED: usize = 1 << (usize::BITS - 1);

static CREW: Crew = Crew {
    joined: AtomicUsize::new(0),
    round: AtomicUsize::new(0),
    quit: AtomicBool::new(false),
    job: AtomicPtr::new(core::ptr::null_mut()),
    parked: AtomicUsize::new(0),
};

/// Whether the boot processor runs AVX2 code. argon2 decides once whether
/// to use it: a helper that can't stays out.
static AVX2: AtomicBool = AtomicBool::new(false);

/// Enables AVX on this processor when it has it and the firmware didn't
/// (Insyde's doesn't): CR4.OSXSAVE, and the SSE and AVX states in XCR0. The
/// kernel sets its own when it starts. Then whether it runs AVX2 code.
fn enable_avx2() -> bool {
    cfg_select! {
        target_arch = "x86_64" => {
            use core::arch::x86_64::{__cpuid, __cpuid_count, _xgetbv, _xsetbv};
            /// XCR0, with the x87, SSE and AVX states added.
            #[target_feature(enable = "xsave")]
            unsafe fn xcr0_with_avx() -> u64 {
                // SAFETY: OSXSAVE is set, and the processor has these states
                // (checked by the caller).
                unsafe {
                    if _xgetbv(0) & 7 != 7 {
                        _xsetbv(0, _xgetbv(0) | 7);
                    }
                    _xgetbv(0)
                }
            }
            // XSAVE and AVX, and the x87, SSE and AVX states.
            let leaf1 = __cpuid(1);
            if leaf1.ecx & (1 << 26) == 0 || leaf1.ecx & (1 << 28) == 0 || __cpuid_count(0xd, 0).eax & 7 != 7 {
                return false;
            }
            // SAFETY: OSXSAVE only allows XSAVE and XGETBV/XSETBV; the states
            // are supported (checked above).
            unsafe {
                let cr4: u64;
                core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
                if cr4 & (1 << 18) == 0 {
                    core::arch::asm!("mov cr4, {}", in(reg) cr4 | (1 << 18), options(nostack));
                }
                xcr0_with_avx() & 6 == 6 && __cpuid_count(7, 0).ebx & (1 << 5) != 0
            }
        }
        _ => false,
    }
}

extern "efiapi" fn help(_arg: *mut c_void) {
    let _returning = Returning;
    if enable_avx2() != AVX2.load(Ordering::Acquire) {
        return;
    }
    // Rounds only start once joining is closed: this is the current one.
    let mut seen = CREW.round.load(Ordering::Acquire);
    let joined = CREW.joined.try_update(Ordering::AcqRel, Ordering::Acquire, |j| (j & CLOSED == 0).then_some(j + 1));
    if joined.is_err() {
        return;
    }
    loop {
        let round = CREW.round.load(Ordering::Acquire);
        if round == seen {
            core::hint::spin_loop();
            continue;
        }
        seen = round;
        if CREW.quit.load(Ordering::Acquire) {
            return;
        }
        // SAFETY: the segments live until every helper is parked (`with_helpers`).
        let segments = unsafe { &*CREW.job.load(Ordering::Acquire).cast::<argon2::Segments<'_>>() };
        segments.work();
        CREW.parked.fetch_add(1, Ordering::AcqRel);
    }
}

/// Computes `segments` with the `helpers` that joined, returning once
/// they're all done: the parking (acquire) makes what they wrote visible to
/// the next slice.
fn with_helpers(helpers: usize, segments: &argon2::Segments<'_>) {
    if helpers == 0 {
        return luks::one_by_one(segments, &cancelled);
    }
    CREW.job.store((&raw const *segments).cast_mut().cast(), Ordering::Release);
    CREW.parked.store(0, Ordering::Release);
    CREW.round.fetch_add(1, Ordering::AcqRel);
    segments.work();
    while CREW.parked.load(Ordering::Acquire) < helpers {
        core::hint::spin_loop();
    }
}

/// Whether `run_attempt` ran the attempt: not on a processor that can't run
/// the AVX2 code argon2 chose (the boot processor then does).
static RAN: AtomicBool = AtomicBool::new(false);

extern "efiapi" fn run_attempt(arg: *mut c_void) {
    let _returning = Returning;
    if enable_avx2() == AVX2.load(Ordering::Acquire) {
        // SAFETY: `start_on_other_processors` passes a valid attempt that
        // nothing else touches until the event is signaled.
        let attempt = unsafe { &mut *arg.cast::<Attempt>() };
        let helpers = CREW.joined.load(Ordering::Acquire) & !CLOSED;
        attempt.run(&|segments| with_helpers(helpers, segments), &cancelled);
        RAN.store(true, Ordering::Release);
    }
    CREW.quit.store(true, Ordering::Release);
    CREW.round.fetch_add(1, Ordering::AcqRel);
}

/// Closes the events that are signaled, keeping the others. The firmware
/// signals one once it has seen its processor finish: closing it before is
/// a use after free.
fn close_signaled(events: &mut Vec<Event>) {
    for event in events.extract_if(.., |e| boot::check_event(e).unwrap_or(false)) {
        let _ = boot::close_event(event);
    }
}

/// Starts `program(arg)` on the processor `ap`; its event is signaled when
/// it's done.
fn start_on(mp: &MpServices, ap: usize, program: extern "efiapi" fn(*mut c_void), arg: *mut c_void) -> Option<Event> {
    // An event makes the call return at once (it's signaled at the end).
    // SAFETY: no notification function.
    let event = unsafe { boot::create_event(EventType::empty(), Tpl::APPLICATION, None, None) }.ok()?;
    // Counted before it starts: it may return before this does.
    RUNNING.fetch_add(1, Ordering::AcqRel);
    // SAFETY: an event of our own, closed after.
    match mp.startup_this_ap(ap, program, arg, Some(unsafe { event.unsafe_clone() }), None) {
        Ok(()) => Some(event),
        Err(e) => {
            RUNNING.fetch_sub(1, Ordering::AcqRel);
            log::info!("StartupThisAP: {:?}", e.status());
            let _ = boot::close_event(event);
            None
        }
    }
}

/// Runs the attempt on an application processor, with up to `lanes - 1`
/// others helping: the events to wait for before touching the attempt
/// again. `None` if the firmware has no such service (or a single
/// processor): the caller runs it.
fn start_on_other_processors(attempt: *mut Attempt, lanes: usize) -> Option<Vec<Event>> {
    let handle = boot::get_handle_for_protocol::<MpServices>().ok()?;
    let mp = open_shared::<MpServices>(handle)?;
    let count = mp.get_number_of_processors().ok()?;
    let me = mp.who_am_i().ok()?;
    let aps: Vec<usize> = (0..count.total)
        .filter(|&i| i != me && mp.get_processor_info(i).is_ok_and(|p| p.is_enabled() && p.is_healthy() && !p.is_bsp()))
        .collect();
    let (&first, others) = aps.split_first()?;
    AVX2.store(enable_avx2(), Ordering::Release);
    RAN.store(false, Ordering::Release);
    CREW.joined.store(0, Ordering::Release);
    CREW.quit.store(false, Ordering::Release);
    let mut events: Vec<Event> = others
        .iter()
        .take(lanes.saturating_sub(1))
        .filter_map(|&ap| start_on(&mp, ap, help, core::ptr::null_mut()))
        .collect();
    // They join in microseconds; one that doesn't within 50 ms stays out.
    for _ in 0..500 {
        if CREW.joined.load(Ordering::Acquire) >= events.len() {
            break;
        }
        boot::stall(Duration::from_micros(100));
    }
    let helpers = CREW.joined.fetch_or(CLOSED, Ordering::AcqRel) & !CLOSED;
    let Some(event) = start_on(&mp, first, run_attempt, attempt.cast()) else {
        // The helpers that joined wait for work: let them go.
        CREW.quit.store(true, Ordering::Release);
        CREW.round.fetch_add(1, Ordering::AcqRel);
        while !events.is_empty() {
            close_signaled(&mut events);
        }
        return None;
    };
    log::info!(
        "checking the passphrase on {} processor(s) of {}, AVX2 {}",
        helpers + 1,
        count.total,
        if AVX2.load(Ordering::Acquire) { "on" } else { "off" }
    );
    events.insert(0, event);
    Some(events)
}

// --- Decrypted disks --------------------------------------------------------------

/// The vendor device path node of a decrypted disk, after its partition's.
const MAPPED_NODE: Guid = guid!("6c1a4f3e-2d5b-4c7a-9e81-5f0b3a7d2c19");

/// A decrypted disk: a BlockIO protocol (first, so that the firmware's
/// pointer to it is one to this) over the encrypted partition's DiskIo.
#[repr(C)]
struct Mapped {
    protocol: BlockIoProtocol,
    media: BlockIoMedia,
    parent: Handle,
    parent_media_id: u32,
    /// Where the data starts on the partition.
    start: u64,
    decryptor: Decryptor,
    /// `Luks::forget_all`: its keys are wiped.
    closed: bool,
}

impl Mapped {
    /// Installs it on a new handle and connects the drivers to it: its
    /// device path.
    fn install(disk: &Disk, start: u64, size: u64, decryptor: Decryptor) -> Result<(Vec<u8>, *mut Mapped), String> {
        let block_size = decryptor.sector_size() as u32;
        let blocks = size / u64::from(block_size);
        if blocks == 0 {
            return Err(tr("This disk is empty."));
        }
        let mapped = Box::new(Mapped {
            protocol: BlockIoProtocol {
                revision: BlockIoProtocol::REVISION,
                media: core::ptr::null(),
                reset,
                read_blocks,
                write_blocks,
                flush_blocks,
            },
            media: BlockIoMedia {
                media_id: 1,
                removable_media: false.into(),
                media_present: true.into(),
                logical_partition: true.into(),
                read_only: true.into(),
                write_caching: false.into(),
                block_size,
                io_align: 0,
                last_block: blocks - 1,
                lowest_aligned_lba: 0,
                logical_blocks_per_physical_block: 1,
                optimal_transfer_length_granularity: 0,
            },
            parent: disk.handle,
            parent_media_id: disk.media_id,
            start,
            decryptor,
            closed: false,
        });
        // Never freed: the firmware's drivers keep using it. The keys are
        // wiped when the system leaves the boot services.
        let mapped = Box::into_raw(mapped);
        // SAFETY: just allocated, never moved.
        unsafe { (*mapped).protocol.media = &raw const (*mapped).media };
        // The partition's device path, then a node of ours.
        let mut device_path = disk.device_path.clone();
        device_path.truncate(device_path.len().saturating_sub(4));
        device_path.extend_from_slice(&[4, 3, 20, 0]);
        device_path.extend_from_slice(&MAPPED_NODE.to_bytes());
        device_path.extend_from_slice(&[0x7f, 0xff, 4, 0]);
        let dp = Box::leak(device_path.clone().into_boxed_slice());
        // SAFETY: a device path and a protocol that live forever.
        let handle = unsafe { boot::install_protocol_interface(None, &DevicePath::GUID, dp.as_ptr().cast()) }
            .map_err(|e| format!("{:?}", e.status()))?;
        // SAFETY: as above.
        unsafe { boot::install_protocol_interface(Some(handle), &BlockIoProtocol::GUID, mapped.cast()) }
            .map_err(|e| format!("{:?}", e.status()))?;
        // SAFETY: the notification only wipes the keys.
        let _ = unsafe {
            boot::create_event(
                EventType::SIGNAL_EXIT_BOOT_SERVICES,
                Tpl::NOTIFY,
                Some(wipe_mapped),
                core::ptr::NonNull::new(mapped.cast()),
            )
        };
        // The disk I/O, partition and file system drivers.
        if let Err(e) = boot::connect_controller(handle, &[], None, true) {
            log::info!("decrypted disk: no driver took it ({:?})", e.status());
        }
        Ok((device_path, mapped))
    }

    /// Reads nothing more, and its keys are wiped.
    ///
    /// # Safety
    /// `mapped` is a `Mapped` that was installed, and not in use.
    unsafe fn close(mapped: *mut Mapped) {
        // SAFETY: as the caller says.
        unsafe {
            (*mapped).closed = true;
            wipe_decryptor(mapped);
        }
    }
}

/// # Safety
/// `mapped` points to a `Mapped`.
unsafe fn wipe_decryptor(mapped: *mut Mapped) {
    // SAFETY: the keys (expanded) are in it, not elsewhere on the heap.
    unsafe {
        let decryptor = &raw mut (*mapped).decryptor;
        core::ptr::write_bytes(decryptor.cast::<u8>(), 0, size_of::<Decryptor>());
    }
}

unsafe extern "efiapi" fn wipe_mapped(_event: Event, ctx: Option<core::ptr::NonNull<c_void>>) {
    if let Some(mapped) = ctx {
        // SAFETY: the `Mapped` given to the event, which lives forever.
        unsafe { wipe_decryptor(mapped.as_ptr().cast()) };
    }
}

unsafe extern "efiapi" fn reset(_this: *mut BlockIoProtocol, _extended: uefi_raw::Boolean) -> Status {
    Status::SUCCESS
}

unsafe extern "efiapi" fn read_blocks(this: *const BlockIoProtocol, media_id: u32, lba: Lba, size: usize, buffer: *mut c_void) -> Status {
    // SAFETY: the firmware calls it with the protocol installed, the first
    // field of a `Mapped`.
    let mapped = unsafe { &*this.cast::<Mapped>() };
    let block_size = mapped.media.block_size as usize;
    if mapped.closed {
        return Status::NO_MEDIA;
    }
    if media_id != mapped.media.media_id {
        return Status::MEDIA_CHANGED;
    }
    if size == 0 {
        return Status::SUCCESS;
    }
    if buffer.is_null() {
        return Status::INVALID_PARAMETER;
    }
    if !size.is_multiple_of(block_size) {
        return Status::BAD_BUFFER_SIZE;
    }
    if lba.checked_add((size / block_size) as u64).is_none_or(|end| end > mapped.media.last_block + 1) {
        return Status::INVALID_PARAMETER;
    }
    // SAFETY: the caller gives a buffer of `size` bytes.
    let data = unsafe { core::slice::from_raw_parts_mut(buffer.cast::<u8>(), size) };
    let Some(disk) = open_shared::<uefi::proto::media::disk::DiskIo>(mapped.parent) else { return Status::DEVICE_ERROR };
    let offset = lba * block_size as u64;
    if disk.read_disk(mapped.parent_media_id, mapped.start + offset, data).is_err() {
        return Status::DEVICE_ERROR;
    }
    mapped.decryptor.decrypt(offset, data);
    Status::SUCCESS
}

unsafe extern "efiapi" fn write_blocks(_this: *mut BlockIoProtocol, _media_id: u32, _lba: Lba, _size: usize, _buffer: *const c_void) -> Status {
    Status::WRITE_PROTECTED
}

unsafe extern "efiapi" fn flush_blocks(_this: *mut BlockIoProtocol) -> Status {
    Status::SUCCESS
}

// --- The initrd media ------------------------------------------------------------

/// LINUX_EFI_INITRD_MEDIA_GUID: the Linux EFI stub loads its initrd through
/// the LoadFile2 protocol on this vendor media device path.
const INITRD_MEDIA: Guid = guid!("5568e427-68fc-4f3d-ac74-ca555231cc68");

#[repr(C)]
struct InitrdDevicePath {
    vendor: [u8; 4],
    guid: [u8; 16],
    end: [u8; 4],
}

static DEVICE_PATH: InitrdDevicePath = InitrdDevicePath {
    vendor: [4, 3, 20, 0],
    guid: INITRD_MEDIA.to_bytes(),
    end: [0x7f, 0xff, 4, 0],
};

static DATA: AtomicPtr<u8> = AtomicPtr::new(core::ptr::null_mut());
static LEN: AtomicUsize = AtomicUsize::new(0);

unsafe extern "efiapi" fn load_initrd(
    _this: *mut LoadFile2Protocol,
    _file_path: *const uefi_raw::protocol::device_path::DevicePathProtocol,
    boot_policy: uefi_raw::Boolean,
    size: *mut usize,
    buffer: *mut c_void,
) -> Status {
    let (data, len) = (DATA.load(Ordering::Acquire), LEN.load(Ordering::Acquire));
    if size.is_null() || bool::from(boot_policy) {
        return Status::INVALID_PARAMETER;
    }
    if data.is_null() {
        return Status::NOT_FOUND;
    }
    // SAFETY: the firmware passes valid pointers.
    unsafe {
        if buffer.is_null() || *size < len {
            *size = len;
            return Status::BUFFER_TOO_SMALL;
        }
        core::ptr::copy_nonoverlapping(data, buffer.cast::<u8>(), len);
        *size = len;
    }
    Status::SUCCESS
}

static PROTOCOL: LoadFile2Protocol = LoadFile2Protocol { load_file: load_initrd };

/// Wipes the initrd when the kernel leaves the boot services: it has its own
/// copy by then.
unsafe extern "efiapi" fn wipe_on_exit(_event: Event, _ctx: Option<core::ptr::NonNull<c_void>>) {
    wipe_data();
}

fn wipe_data() {
    let (data, len) = (DATA.swap(core::ptr::null_mut(), Ordering::AcqRel), LEN.swap(0, Ordering::AcqRel));
    if !data.is_null() {
        // SAFETY: `Handoff` keeps the buffer alive while DATA points to it.
        zeroize::Zeroize::zeroize(unsafe { core::slice::from_raw_parts_mut(data, len) });
    }
}

/// The initrds, offered to the kernel until dropped.
pub struct Handoff {
    handle: Handle,
    data: Vec<u8>,
    exit_event: Option<Event>,
}

impl Handoff {
    fn install(mut data: Vec<u8>) -> Option<Handoff> {
        DATA.store(data.as_mut_ptr(), Ordering::Release);
        LEN.store(data.len(), Ordering::Release);
        let dp = (&raw const DEVICE_PATH).cast::<c_void>();
        // SAFETY: a device path and a protocol in statics.
        let handle = unsafe { boot::install_protocol_interface(None, &DevicePath::GUID, dp) };
        let handle = match handle {
            Ok(h) => h,
            Err(e) => {
                // Another loader's (systemd-stub's) initrd may be there.
                log::info!("initrd media: {:?}", e.status());
                wipe_data();
                return None;
            }
        };
        let protocol = (&raw const PROTOCOL).cast::<c_void>();
        // SAFETY: as above.
        if unsafe { boot::install_protocol_interface(Some(handle), &LoadFile2Protocol::GUID, protocol) }.is_err() {
            // SAFETY: installed just above.
            let _ = unsafe { boot::uninstall_protocol_interface(handle, &DevicePath::GUID, dp) };
            wipe_data();
            return None;
        }
        // SAFETY: the notification only wipes memory.
        let exit_event = unsafe {
            boot::create_event(EventType::SIGNAL_EXIT_BOOT_SERVICES, Tpl::NOTIFY, Some(wipe_on_exit), None)
        }
        .ok();
        Some(Handoff { handle, data, exit_event })
    }
}

impl Drop for Handoff {
    /// The kernel returned (it failed to start): take it all back.
    fn drop(&mut self) {
        // SAFETY: installed by `install`.
        unsafe {
            let _ = boot::uninstall_protocol_interface(self.handle, &LoadFile2Protocol::GUID, (&raw const PROTOCOL).cast());
            let _ = boot::uninstall_protocol_interface(self.handle, &DevicePath::GUID, (&raw const DEVICE_PATH).cast());
        }
        if let Some(event) = self.exit_event.take() {
            let _ = boot::close_event(event);
        }
        DATA.store(core::ptr::null_mut(), Ordering::Release);
        LEN.store(0, Ordering::Release);
        zeroize::Zeroize::zeroize(&mut self.data);
    }
}
