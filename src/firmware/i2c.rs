// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! A polled driver for the DesignWare I2C controller, as master: Intel's
//! LPSS controllers and AMD's are this design (Linux's
//! `i2c-designware`). For the touchpad (`touchpad.rs`), so no interrupts,
//! no DMA: each transfer pushes its commands and waits for the bytes.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

const IC_CON: usize = 0x00;
const IC_TAR: usize = 0x04;
const IC_DATA_CMD: usize = 0x10;
const IC_SS_SCL_HCNT: usize = 0x14;
const IC_SS_SCL_LCNT: usize = 0x18;
const IC_FS_SCL_HCNT: usize = 0x1c;
const IC_FS_SCL_LCNT: usize = 0x20;
const IC_INTR_MASK: usize = 0x30;
const IC_RAW_INTR_STAT: usize = 0x34;
const IC_RX_TL: usize = 0x38;
const IC_TX_TL: usize = 0x3c;
const IC_CLR_INTR: usize = 0x40;
const IC_CLR_TX_ABRT: usize = 0x54;
const IC_CLR_STOP_DET: usize = 0x60;
const IC_ENABLE: usize = 0x6c;
const IC_TXFLR: usize = 0x74;
const IC_RXFLR: usize = 0x78;
const IC_SDA_HOLD: usize = 0x7c;
const IC_TX_ABRT_SOURCE: usize = 0x80;
const IC_ENABLE_STATUS: usize = 0x9c;
const IC_COMP_PARAM_1: usize = 0xf4;
const IC_COMP_TYPE: usize = 0xfc;

/// IC_COMP_TYPE of a DesignWare controller ("DW").
pub const DESIGNWARE: u32 = 0x4457_0140;

const TX_ABRT: u32 = 1 << 6;
const STOP_DET: u32 = 1 << 9;
const READ: u32 = 1 << 8;
const STOP: u32 = 1 << 9;
const RESTART: u32 = 1 << 10;

/// The bus speed and its timing, as the controller's ACPI methods give them
/// (`SSCN`, `FMCN`: SCL high and low counts, SDA hold).
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    pub fast: bool,
    pub high: u16,
    pub low: u16,
    pub hold: Option<u16>,
}

impl Timing {
    /// Standard mode with counts that stay under 100 kHz for the input
    /// clocks these controllers have (100 to 150 MHz): slow, but safe.
    pub const SAFE: Timing = Timing { fast: false, high: 640, low: 760, hold: None };
}

pub struct I2c {
    base: usize,
    tx_depth: usize,
    rx_depth: usize,
}

/// Reads a controller register.
pub fn read_register(base: usize, register: usize) -> u32 {
    // SAFETY: The controller's MMIO registers, which the firmware maps 1:1.
    unsafe { ((base + register) as *const u32).read_volatile() }
}

impl I2c {
    fn read(&self, register: usize) -> u32 {
        read_register(self.base, register)
    }

    fn write(&self, register: usize, value: u32) {
        // SAFETY: As above.
        unsafe { ((self.base + register) as *mut u32).write_volatile(value) }
    }

    /// Waits until `done`, for up to `ms` milliseconds.
    fn wait(&self, ms: u32, mut done: impl FnMut(&Self) -> bool) -> bool {
        for _ in 0..ms * 100 {
            if done(self) {
                return true;
            }
            uefi::boot::stall(Duration::from_micros(10));
        }
        done(self)
    }

    fn disable(&self) -> Result<(), String> {
        self.write(IC_ENABLE, 0);
        if self.wait(25, |c| c.read(IC_ENABLE_STATUS) & 1 == 0) { Ok(()) } else { Err(String::from("doesn't turn off")) }
    }

    /// Sets the controller at `base` up as a master with `timing`.
    pub fn new(base: usize, timing: Timing) -> Result<I2c, String> {
        let kind = read_register(base, IC_COMP_TYPE);
        if kind != DESIGNWARE {
            return Err(format!("not a DesignWare I2C controller (type {kind:#x})"));
        }
        let params = read_register(base, IC_COMP_PARAM_1);
        let tx_depth = (((params >> 16) & 0xff) as usize + 1).max(2);
        let rx_depth = (((params >> 8) & 0xff) as usize + 1).max(2);
        let i2c = I2c { base, tx_depth, rx_depth };
        i2c.disable()?;
        // Master, standard (1) or fast (2) speed, restarts, no slave.
        let speed = if timing.fast { 2 } else { 1 };
        i2c.write(IC_CON, 1 | speed << 1 | 1 << 5 | 1 << 6);
        let (high, low) = if timing.fast { (IC_FS_SCL_HCNT, IC_FS_SCL_LCNT) } else { (IC_SS_SCL_HCNT, IC_SS_SCL_LCNT) };
        i2c.write(high, timing.high as u32);
        i2c.write(low, timing.low as u32);
        if let Some(hold) = timing.hold {
            i2c.write(IC_SDA_HOLD, hold as u32);
        }
        i2c.write(IC_INTR_MASK, 0);
        i2c.write(IC_RX_TL, 0);
        i2c.write(IC_TX_TL, 0);
        log::info!("i2c: controller at {base:#x}, {timing:?}, FIFOs {tx_depth}/{rx_depth}, params {params:#x}");
        Ok(i2c)
    }

    /// Writes `write` to the device at `address`, then reads `read` bytes
    /// (after a repeated start): one transfer.
    pub fn transfer(&mut self, address: u16, write: &[u8], read: usize) -> Result<Vec<u8>, String> {
        self.disable()?;
        self.write(IC_TAR, address as u32);
        self.write(IC_ENABLE, 1);
        self.read(IC_CLR_INTR);

        let total = write.len() + read;
        let command = |i: usize| -> u32 {
            let last = if i + 1 == total { STOP } else { 0 };
            if i < write.len() {
                write[i] as u32 | last
            } else {
                let restart = if i == write.len() && !write.is_empty() { RESTART } else { 0 };
                READ | restart | last
            }
        };
        let mut sent = 0;
        let mut data = Vec::with_capacity(read);
        let mut abort = None;
        let finished = self.wait(100, |c| {
            // Keep the FIFO fed, without asking for more bytes than the
            // receive FIFO holds.
            let waiting = |sent: usize, received: usize| sent.saturating_sub(write.len()).saturating_sub(received);
            while sent < total && (c.read(IC_TXFLR) as usize) < c.tx_depth && waiting(sent, data.len()) < c.rx_depth {
                c.write(IC_DATA_CMD, command(sent));
                sent += 1;
            }
            while c.read(IC_RXFLR) > 0 && data.len() < read {
                data.push(c.read(IC_DATA_CMD) as u8);
            }
            let raw = c.read(IC_RAW_INTR_STAT);
            if raw & TX_ABRT != 0 {
                abort = Some(c.read(IC_TX_ABRT_SOURCE));
                c.read(IC_CLR_TX_ABRT);
                return true;
            }
            sent == total && data.len() == read && raw & STOP_DET != 0
        });
        self.read(IC_CLR_STOP_DET);
        let _ = self.disable();
        if let Some(source) = abort {
            // Bit 0..3: the address or a byte wasn't acknowledged.
            return Err(format!("transfer aborted (source {source:#x})"));
        }
        if !finished {
            return Err(format!("timed out ({sent} of {total} sent, {} of {read} read)", data.len()));
        }
        Ok(data)
    }
}
