// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Before starting an entry that needs encrypted disks: the passphrase
//! dialog, one disk after the other. A passphrase that opened one disk is
//! tried on the next before asking again (people often use one for all).

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use slint::ComponentHandle;


use super::{AppWindow, Backend, LockedVolume, Passphrase, Secret, Target, UnlockResult};

/// Starts a target (after the unlocking): the "Starting…" transition.
pub type Start = Rc<dyn Fn(String, Target)>;

/// What to do once the disks are unlocked (or skipped).
enum Then {
    /// Start this (after the "Starting…" transition), titled so.
    Start(String, Target),
    /// The file browser's: whether the disk was unlocked.
    Open(Box<dyn FnOnce(bool)>),
}

struct Flow {
    then: Then,
    volumes: Vec<LockedVolume>,
    /// How many were unlocked (not skipped).
    unlocked: usize,
    current: usize,
    /// The passphrase that opened the previous disk, to try on this one.
    previous: Option<Secret>,
}

#[derive(Clone)]
struct Unlocker {
    ui: slint::Weak<AppWindow>,
    backend: Rc<dyn Backend>,
    start: Start,
    flow: Rc<RefCell<Option<Flow>>>,
}

impl Unlocker {
    /// Shows the current disk (or starts the target when there's none left).
    fn show_current(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let mut flow = self.flow.borrow_mut();
        let Some(f) = flow.as_mut() else { return };
        let Some(volume) = f.volumes.get(f.current).cloned() else {
            let f = flow.take().unwrap();
            drop(flow);
            ui.global::<Passphrase>().set_visible(false);
            match f.then {
                Then::Start(title, target) => (self.start)(title, target),
                Then::Open(open) => open(f.unlocked > 0),
            }
            return;
        };
        // `luks-<uuid>` (rd.luks.uuid=) says nothing; `root`, `home` do.
        let detail = if volume.name.starts_with("luks-") { volume.detail.clone() } else { format!("{} · {}", volume.name, volume.detail) };
        let counter = if f.volumes.len() > 1 { crate::tr::trf("Disk {} of {}", &[&(f.current + 1), &f.volumes.len()]) } else { String::new() };
        let dialog = ui.global::<Passphrase>();
        dialog.set_detail(detail.into());
        dialog.set_counter(counter.into());
        dialog.set_browsing(matches!(f.then, Then::Open(_)));
        dialog.set_wrong(false);
        dialog.set_error(Default::default());
        let previous = f.previous.take();
        drop(flow);
        dialog.set_visible(true);
        match previous {
            Some(p) => self.check(volume.id, p, true),
            None => dialog.invoke_retry(),
        }
    }

    /// Checks `passphrase` for the current disk. `reused`: it was typed for
    /// the previous one, so a wrong one isn't an error, just a question.
    fn check(&self, id: usize, passphrase: Secret, reused: bool) {
        let Some(ui) = self.ui.upgrade() else { return };
        let dialog = ui.global::<Passphrase>();
        dialog.set_checking(true);
        dialog.set_wrong(false);
        dialog.set_error(Default::default());
        let keep = Secret(passphrase.0.clone());
        let this = self.clone();
        self.backend.unlock(
            id,
            passphrase,
            Box::new(move |result| {
                let Some(ui) = this.ui.upgrade() else { return };
                let dialog = ui.global::<Passphrase>();
                dialog.set_checking(false);
                // Cancelled while checking: the dialog is gone already.
                if this.flow.borrow().is_none() {
                    return;
                }
                match result {
                    UnlockResult::Unlocked => {
                        if let Some(f) = this.flow.borrow_mut().as_mut() {
                            f.current += 1;
                            f.unlocked += 1;
                            f.previous = Some(keep);
                        }
                        this.show_current();
                    }
                    UnlockResult::WrongPassphrase => {
                        dialog.set_wrong(!reused);
                        dialog.invoke_retry();
                    }
                    UnlockResult::Failed(why) => {
                        dialog.set_error(why.into());
                        dialog.invoke_retry();
                    }
                    UnlockResult::Cancelled => dialog.invoke_cancel(),
                }
            }),
        );
    }

    fn current_id(&self) -> Option<usize> {
        let flow = self.flow.borrow();
        let f = flow.as_ref()?;
        f.volumes.get(f.current).map(|v| v.id)
    }
}

/// Asks for the passphrase of `volume`, from the file browser: `then`
/// gets whether it was unlocked.
pub type Unlock = Rc<dyn Fn(LockedVolume, Box<dyn FnOnce(bool)>)>;

/// Wires the dialog. Returns how to start a target: through the dialog when
/// it needs encrypted disks, else directly; and how to unlock a disk.
pub fn wire(ui: &AppWindow, backend: Rc<dyn Backend>, start: Start, stop_countdown: Rc<dyn Fn()>) -> (Start, Unlock) {
    let unlocker = Unlocker { ui: ui.as_weak(), backend: backend.clone(), start: start.clone(), flow: Default::default() };
    {
        let u = unlocker.clone();
        ui.global::<Passphrase>().on_submit(move |text| {
            if text.is_empty() {
                return;
            }
            let Some(id) = u.current_id() else { return };
            u.check(id, Secret(text.as_bytes().to_vec()), false);
        });
    }
    {
        let u = unlocker.clone();
        ui.global::<Passphrase>().on_skip(move || {
            if let Some(f) = u.flow.borrow_mut().as_mut() {
                f.current += 1;
            }
            u.show_current();
        });
    }
    {
        let u = unlocker.clone();
        ui.global::<Passphrase>().on_cancel(move || {
            u.flow.borrow_mut().take();
            // Also stops a check under way.
            u.backend.forget_unlocked();
            if let Some(ui) = u.ui.upgrade() {
                let dialog = ui.global::<Passphrase>();
                dialog.set_checking(false);
                dialog.set_visible(false);
            }
        });
    }
    let unlock: Unlock = {
        let unlocker = unlocker.clone();
        Rc::new(move |volume: LockedVolume, then: Box<dyn FnOnce(bool)>| {
            let flow = Flow { then: Then::Open(then), volumes: alloc::vec![volume], unlocked: 0, current: 0, previous: None };
            *unlocker.flow.borrow_mut() = Some(flow);
            unlocker.show_current();
        })
    };
    let start = Rc::new(move |title: String, target: Target| {
        let volumes = backend.locked_volumes(&target);
        if volumes.is_empty() {
            start(title, target);
            return;
        }
        stop_countdown();
        let flow = Flow { then: Then::Start(title, target), volumes, unlocked: 0, current: 0, previous: None };
        *unlocker.flow.borrow_mut() = Some(flow);
        unlocker.show_current();
    });
    (start, unlock)
}
