// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Write-combining for the screen's memory, through the processor's page
//! attribute table (x86). Firmware maps it uncached, and won't change that:
//! each write then crosses PCIe on its own, and a 4K frame takes about
//! 200 ms to reach the screen. Write-combined, the writes go out in bursts.
//!
//! A write-combining page attribute wins over an uncached MTRR. So PAT
//! entry 7 (uncached by default, unused by EDK II firmware) becomes
//! write-combining, and the screen's page entries select it. Large pages
//! that go past the screen are split first, with the same mapping, so the
//! memory around it (the device's registers) stays as it was.

use alloc::vec::Vec;
use core::arch::asm;
use core::arch::x86_64::__cpuid;
use core::ptr::NonNull;

use uefi::boot::{self, AllocateType, MemoryType, Tpl};

const IA32_PAT: u32 = 0x277;
/// The firmware's PAT, as the processor starts: write-back, write-through,
/// uncached-minus, uncached, then the same again.
const DEFAULT_PAT: u64 = 0x0007_0406_0007_0406;
/// The same with entry 7 (the PAT, PCD and PWT bits of a page entry)
/// write-combining.
const COMBINING_PAT: u64 = 0x0107_0406_0007_0406;

const PRESENT: u64 = 1;
const PWT: u64 = 1 << 3;
const PCD: u64 = 1 << 4;
/// A large page (2 MiB or 1 GiB), in a directory entry.
const LARGE: u64 = 1 << 7;
/// The PAT bit: bit 7 in a 4 KiB page's entry, bit 12 in a large page's.
const PAT_SMALL: u64 = 1 << 7;
const PAT_LARGE: u64 = 1 << 12;
const ADDRESS: u64 = 0x000f_ffff_ffff_f000;

const CR0_WP: u64 = 1 << 16;
const CR4_PGE: u64 = 1 << 7;
const CR4_LA57: u64 = 1 << 12;

/// Makes `size` bytes at `base` write-combining, and logs how it went.
pub fn enable(base: u64, size: u64) {
    if __cpuid(1).edx & (1 << 16) == 0 {
        log::info!("display: no PAT, the screen's memory stays uncached");
        return;
    }
    // Already changed for a previous mode, or the default.
    let pat = rdmsr(IA32_PAT);
    if pat != DEFAULT_PAT && pat != COMBINING_PAT {
        log::info!("display: PAT {pat:#x} isn't the default, the screen's memory stays as it is");
        return;
    }
    let start = base & !0xfff;
    let end = (base + size).next_multiple_of(0x1000);
    let levels = if read_cr4() & CR4_LA57 != 0 { 5 } else { 4 };
    let mut leaves = Vec::new();
    let mut splits = 0;
    if let Err(e) = walk(read_cr3() & ADDRESS, levels, 0, start..end, &mut leaves, &mut splits) {
        log::info!("display: the screen's memory stays uncached: {e}");
        return;
    }
    writable_page_tables(|| {
        wrmsr(IA32_PAT, COMBINING_PAT);
        for &(entry, level) in &leaves {
            let pat_bit = if level == 1 { PAT_SMALL } else { PAT_LARGE };
            // SAFETY: A page entry `walk` found for the screen's memory.
            unsafe { entry.write_volatile(entry.read_volatile() | PWT | PCD | pat_bit) };
        }
        flush_tlb();
    });
    log::info!(
        "display: the screen's memory ({:#x} bytes at {start:#x}) write-combining: {} page entries, {splits} large pages split",
        end - start,
        leaves.len()
    );
}

/// The memory an entry maps at `level`: 4 KiB at 1, 2 MiB at 2, 1 GiB at 3…
fn entry_size(level: u32) -> u64 {
    1 << (12 + 9 * (level - 1))
}

/// Finds the leaf entries mapping `range` in the table at `table` (of
/// `level`, mapping from `table_start`), splitting the large pages that go
/// past it.
fn walk(
    table: u64,
    level: u32,
    table_start: u64,
    range: core::ops::Range<u64>,
    leaves: &mut Vec<(*mut u64, u32)>,
    splits: &mut usize,
) -> Result<(), &'static str> {
    let size = entry_size(level);
    let first = (range.start.max(table_start) - table_start) / size;
    let last = (range.end.min(table_start + size * 512) - 1 - table_start) / size;
    for i in first..=last {
        // SAFETY: An entry of a page table the processor uses (identity
        // mapped, as UEFI requires).
        let ptr = unsafe { (table as *mut u64).add(i as usize) };
        let start = table_start + i * size;
        // SAFETY: As above.
        let mut entry = unsafe { ptr.read_volatile() };
        if entry & PRESENT == 0 {
            return Err("not mapped");
        }
        if level == 1 || (level <= 3 && entry & LARGE != 0) {
            if start >= range.start && start + size <= range.end {
                leaves.push((ptr, level));
                continue;
            }
            entry = split(ptr, entry, level)?;
            *splits += 1;
        }
        walk(entry & ADDRESS, level - 1, start, range.clone(), leaves, splits)?;
    }
    Ok(())
}

/// Replaces the large page at `ptr` with a table of smaller pages, mapping
/// the same memory the same way. Returns the new entry.
fn split(ptr: *mut u64, entry: u64, level: u32) -> Result<u64, &'static str> {
    let page: NonNull<u8> = boot::allocate_pages(AllocateType::AnyPages, MemoryType::BOOT_SERVICES_DATA, 1)
        .map_err(|_| "no memory for a page table")?;
    let table = page.as_ptr().cast::<u64>();
    let address = entry & ADDRESS & !(entry_size(level) - 1);
    let pat = entry & PAT_LARGE != 0;
    // The flags without the address: the low 12 bits (PS included), and
    // the top ones (no-execute).
    let flags = entry & !ADDRESS;
    let child_flags = if level == 2 {
        (flags & !LARGE) | if pat { PAT_SMALL } else { 0 }
    } else {
        flags | if pat { PAT_LARGE } else { 0 }
    };
    for i in 0..512 {
        // SAFETY: The page just allocated, 512 entries.
        unsafe { table.add(i).write((address + i as u64 * entry_size(level - 1)) | child_flags) };
    }
    // Present, writable and user as before: the smaller pages say the rest.
    let new = page.as_ptr() as u64 | (entry & 0x7) | 0x20;
    // SAFETY: One write, of an entry mapping the same memory.
    writable_page_tables(|| unsafe { ptr.write_volatile(new) });
    Ok(new)
}

/// Runs `f` with the interrupts off and the page tables writable (firmware
/// can make them read-only, and lifts CR0.WP the same way to change them).
fn writable_page_tables(f: impl FnOnce()) {
    // SAFETY: Raised for `f` only, which doesn't allocate or wait.
    let _tpl = unsafe { boot::raise_tpl(Tpl::HIGH_LEVEL) };
    let cr0 = read_cr0();
    write_cr0(cr0 & !CR0_WP);
    f();
    write_cr0(cr0);
}

/// Forgets every translation, the global ones too.
fn flush_tlb() {
    let cr4 = read_cr4();
    if cr4 & CR4_PGE != 0 {
        write_cr4(cr4 & !CR4_PGE);
        write_cr4(cr4);
    } else {
        write_cr3(read_cr3());
    }
}

fn rdmsr(msr: u32) -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: The PAT, which every processor with a PAT has.
    unsafe { asm!("rdmsr", in("ecx") msr, out("eax") low, out("edx") high, options(nomem, nostack)) };
    u64::from(high) << 32 | u64::from(low)
}

fn wrmsr(msr: u32, value: u64) {
    // SAFETY: The PAT, its entry 7 only, which nothing maps with.
    unsafe { asm!("wrmsr", in("ecx") msr, in("eax") value as u32, in("edx") (value >> 32) as u32, options(nostack)) };
}

fn read_cr0() -> u64 {
    let v;
    // SAFETY: Reading a control register has no side effect.
    unsafe { asm!("mov {}, cr0", out(reg) v, options(nomem, nostack)) };
    v
}

fn write_cr0(v: u64) {
    // SAFETY: Only the write protection bit changes, around page table writes.
    unsafe { asm!("mov cr0, {}", in(reg) v, options(nostack)) };
}

fn read_cr3() -> u64 {
    let v;
    // SAFETY: As above.
    unsafe { asm!("mov {}, cr3", out(reg) v, options(nomem, nostack)) };
    v
}

fn write_cr3(v: u64) {
    // SAFETY: The same page tables again: only flushes the translations.
    unsafe { asm!("mov cr3, {}", in(reg) v, options(nostack)) };
}

fn read_cr4() -> u64 {
    let v;
    // SAFETY: As above.
    unsafe { asm!("mov {}, cr4", out(reg) v, options(nomem, nostack)) };
    v
}

fn write_cr4(v: u64) {
    // SAFETY: Only the global pages bit, off and back on: flushes them.
    unsafe { asm!("mov cr4, {}", in(reg) v, options(nostack)) };
}
