// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Measured boot: records what Slingshot passes on in the TPM, as
//! systemd-boot does (<https://systemd.io/TPM2_PCR_MEASUREMENTS/>), so that
//! TPM policies and systemd-pcrlock see the same measurements whichever of
//! the two starts the kernel. The firmware itself measures the programs
//! started (PCR 4); Linux measures its initrd (PCR 9).

use alloc::vec::Vec;

use uefi::boot;
use uefi::proto::tcg::v2::{HashLogExtendEventFlags, PcrEventInputs, Tcg};
use uefi::proto::tcg::{EventType, PcrIndex};

/// PCR 5, `boot-loader-config`.
const PCR_BOOT_LOADER_CONFIG: PcrIndex = PcrIndex(5);
/// PCR 12, `kernel-config`.
const PCR_KERNEL_CONFIG: PcrIndex = PcrIndex(12);
/// systemd's event tag for loader.conf.
const LOADER_CONF_TAG: u32 = 0xf5bc_582a;

fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// Extends `pcr` with the hash of `data`, with `event` in the event log.
/// Nothing without a TPM.
fn measure(pcr: PcrIndex, event_type: EventType, event: &[u8], data: &[u8]) {
    let Ok(handle) = boot::get_handle_for_protocol::<Tcg>() else { return };
    let Ok(mut tcg) = boot::open_protocol_exclusive::<Tcg>(handle) else { return };
    if !tcg.get_capability().is_ok_and(|c| c.tpm_present()) {
        return;
    }
    let Ok(input) = PcrEventInputs::new_in_box(pcr, event_type, event) else { return };
    match tcg.hash_log_extend_event(HashLogExtendEventFlags::empty(), data, &input) {
        Ok(()) => log::info!("measured into PCR {}", pcr.0),
        Err(e) => log::warn!("cannot measure into PCR {}: {:?}", pcr.0, e.status()),
    }
}

/// The kernel command line passed to a kernel (PCR 12, `EV_IPL`): the hash
/// covers it in UTF-16 without the nul, the event log has it with.
pub fn kernel_command_line(command_line: &str) {
    if command_line.is_empty() {
        return;
    }
    let data = utf16(command_line);
    let mut event = data.clone();
    event.extend_from_slice(&[0, 0]);
    measure(PCR_KERNEL_CONFIG, EventType::IPL, &event, &data);
}

/// systemd-boot's loader.conf, as read (PCR 5, a tagged event).
pub fn loader_conf(contents: &[u8]) {
    let mut description = utf16("loader.conf");
    description.extend_from_slice(&[0, 0]);
    // TCG_PCClientTaggedEvent: the tag, the size, then the description.
    let mut event = Vec::with_capacity(8 + description.len());
    event.extend_from_slice(&LOADER_CONF_TAG.to_le_bytes());
    event.extend_from_slice(&(description.len() as u32).to_le_bytes());
    event.extend_from_slice(&description);
    measure(PCR_BOOT_LOADER_CONFIG, EventType::EVENT_TAG, &event, contents);
}
