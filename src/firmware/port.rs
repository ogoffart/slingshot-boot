// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! x86 port I/O: the embedded controller and the AML's ports, PCI
//! configuration, QEMU's debug console.

/// Selects a register of a PCI function's configuration space (segment 0,
/// its first 256 bytes): its dword is then at port 0xcfc.
pub fn pci_select(bus: u8, device: u8, function: u8, offset: u8) {
    outl(0xcf8, 0x8000_0000 | u32::from(bus) << 16 | u32::from(device) << 11 | u32::from(function) << 8 | u32::from(offset & 0xfc));
}

pub fn inb(port: u16) -> u8 {
    let v: u8;
    // SAFETY: Ports Slingshot's drivers and the AML ask for; Slingshot owns
    // the hardware.
    unsafe { core::arch::asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack)) };
    v
}
pub fn inw(port: u16) -> u16 {
    let v: u16;
    // SAFETY: As above.
    unsafe { core::arch::asm!("in ax, dx", out("ax") v, in("dx") port, options(nomem, nostack)) };
    v
}
pub fn inl(port: u16) -> u32 {
    let v: u32;
    // SAFETY: As above.
    unsafe { core::arch::asm!("in eax, dx", out("eax") v, in("dx") port, options(nomem, nostack)) };
    v
}
pub fn outb(port: u16, v: u8) {
    // SAFETY: As above.
    unsafe { core::arch::asm!("out dx, al", in("dx") port, in("al") v, options(nomem, nostack)) };
}
pub fn outw(port: u16, v: u16) {
    // SAFETY: As above.
    unsafe { core::arch::asm!("out dx, ax", in("dx") port, in("ax") v, options(nomem, nostack)) };
}
pub fn outl(port: u16, v: u32) {
    // SAFETY: As above.
    unsafe { core::arch::asm!("out dx, eax", in("dx") port, in("eax") v, options(nomem, nostack)) };
}
