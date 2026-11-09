// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! HID, for laptop touchpads on I2C (`firmware/touchpad.rs`): the HID over
//! I2C descriptor, and where a report descriptor puts the mouse's buttons
//! and movement, or a touch pad's fingers. Precision touchpads should start
//! in mouse mode (relative movement in a mouse collection) until the OS
//! switches them to report fingers, but some firmware leaves them reporting
//! fingers (the Framework 13's): both are read.

#![cfg_attr(not(all(target_os = "uefi", target_arch = "x86_64", feature = "touchpad")), allow(dead_code))] // the firmware's, with `touchpad`

/// The HID over I2C descriptor's registers (HID over I2C 1.0, section 5.1).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct I2cHidDescriptor {
    pub report_descriptor_length: u16,
    pub report_descriptor_register: u16,
    pub input_register: u16,
    pub max_input_length: u16,
    pub command_register: u16,
    pub data_register: u16,
    pub vendor: u16,
    pub product: u16,
}

pub fn parse_i2c_hid_descriptor(d: &[u8]) -> Option<I2cHidDescriptor> {
    let word = |i: usize| Some(u16::from_le_bytes([*d.get(i)?, *d.get(i + 1)?]));
    // wHIDDescLength (30), bcdVersion (1.00).
    if word(0)? != 30 || word(2)? != 0x0100 {
        return None;
    }
    Some(I2cHidDescriptor {
        report_descriptor_length: word(4)?,
        report_descriptor_register: word(6)?,
        input_register: word(8)?,
        max_input_length: word(10)?,
        command_register: word(16)?,
        data_register: word(18)?,
        vendor: word(20)?,
        product: word(22)?,
    })
}

/// A field of a report: where it is, in bits after the report id.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Field {
    pub offset: usize,
    pub size: usize,
    pub signed: bool,
}

/// Where a mouse report has its buttons and movement.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MouseLayout {
    /// The report's id, when the device numbers its reports.
    pub report_id: Option<u8>,
    /// The first button's bit (button 2 and 3 follow).
    pub buttons: Option<usize>,
    pub x: Option<Field>,
    pub y: Option<Field>,
}

/// One mouse report.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MouseReport {
    pub dx: i32,
    pub dy: i32,
    pub left: bool,
    pub right: bool,
}

const GENERIC_DESKTOP: u32 = 0x01;
const BUTTON: u32 = 0x09;
const MOUSE: u32 = 0x02;
const POINTER: u32 = 0x01;
const X: u32 = 0x30;
const Y: u32 = 0x31;

/// Finds the mouse in a report descriptor: the first application collection
/// for a mouse (or pointer) with relative X and Y.
pub fn mouse_layout(descriptor: &[u8]) -> Option<MouseLayout> {
    let mut usage_page = 0u32;
    let mut report_size = 0usize;
    let mut report_count = 0usize;
    let mut logical_min = 0i64;
    let mut report_id: Option<u8> = None;
    let mut usages: alloc::vec::Vec<u32> = alloc::vec::Vec::new();
    let mut usage_min: Option<u32> = None;
    // Bit offsets so far, per report id.
    let mut offsets: alloc::collections::BTreeMap<Option<u8>, usize> = Default::default();
    let mut depth = 0usize;
    // The depth of the mouse's application collection, while in it.
    let mut in_mouse: Option<usize> = None;
    let mut layout: Option<MouseLayout> = None;

    let mut i = 0;
    while i < descriptor.len() {
        let prefix = descriptor[i];
        if prefix == 0xfe {
            // A long item: skip it.
            let size = *descriptor.get(i + 1)? as usize;
            i += 3 + size;
            continue;
        }
        let size = [0, 1, 2, 4][(prefix & 3) as usize];
        let data = descriptor.get(i + 1..i + 1 + size)?;
        i += 1 + size;
        let value = data.iter().rev().fold(0u32, |v, b| (v << 8) | *b as u32);
        let signed = match size {
            1 => value as u8 as i8 as i64,
            2 => value as u16 as i16 as i64,
            _ => value as i32 as i64,
        };
        let full_usage = |u: u32| if size == 4 { u } else { (usage_page << 16) | u };
        match (prefix >> 2) & 3 {
            // Main items.
            0 => {
                match prefix >> 4 {
                    // Collection.
                    0xa => {
                        depth += 1;
                        let usage = usages.first().copied().unwrap_or(0);
                        if value == 1 && in_mouse.is_none() && layout.is_none_or(|l| l.x.is_none())
                            && (usage == (GENERIC_DESKTOP << 16 | MOUSE) || usage == (GENERIC_DESKTOP << 16 | POINTER))
                        {
                            in_mouse = Some(depth);
                        }
                    }
                    // End collection.
                    0xc => {
                        if in_mouse == Some(depth) {
                            in_mouse = None;
                        }
                        depth = depth.saturating_sub(1);
                    }
                    // Input.
                    0x8 => {
                        let offset = offsets.entry(report_id).or_insert(0);
                        let start = *offset;
                        // Reports are at most a few hundred bytes: a
                        // descriptor saying more is broken.
                        if report_size > 32 || report_count > 1024 {
                            return None;
                        }
                        *offset = offset.saturating_add(report_size * report_count);
                        let constant = value & 1 != 0;
                        let relative = value & 4 != 0;
                        if in_mouse.is_some() && !constant {
                            let l = layout.get_or_insert(MouseLayout { report_id, ..Default::default() });
                            if l.report_id == report_id {
                                let field = |n: usize| Field { offset: start + n * report_size, size: report_size, signed: logical_min < 0 };
                                for n in 0..report_count {
                                    let usage = match (usages.get(n).or(usages.last()), usage_min) {
                                        (Some(u), _) => *u,
                                        (None, Some(min)) => min + n as u32,
                                        (None, None) => 0,
                                    };
                                    if usage >> 16 == BUTTON && l.buttons.is_none() {
                                        l.buttons = Some(start + n * report_size);
                                    } else if usage == (GENERIC_DESKTOP << 16 | X) && relative {
                                        l.x = Some(field(n));
                                    } else if usage == (GENERIC_DESKTOP << 16 | Y) && relative {
                                        l.y = Some(field(n));
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
                usages.clear();
                usage_min = None;
            }
            // Global items.
            1 => match prefix >> 4 {
                0x0 => usage_page = value,
                0x1 => logical_min = signed,
                0x7 => report_size = value as usize,
                0x8 => report_id = Some(value as u8),
                0x9 => report_count = value as usize,
                _ => {}
            },
            // Local items.
            2 => match prefix >> 4 {
                0x0 => usages.push(full_usage(value)),
                0x1 => usage_min = Some(full_usage(value)),
                _ => {}
            },
            _ => {}
        }
    }
    layout.filter(|l| l.x.is_some() && l.y.is_some())
}

fn bits(data: &[u8], field: Field) -> Option<i32> {
    if field.size == 0 || field.size > 32 || field.offset.saturating_add(field.size).div_ceil(8) > data.len() {
        return None;
    }
    let mut v = 0u64;
    for bit in 0..field.size {
        let at = field.offset + bit;
        v |= (((data[at / 8] >> (at % 8)) & 1) as u64) << bit;
    }
    Some(if field.signed && v >> (field.size - 1) & 1 != 0 { (v as i64 - (1i64 << field.size)) as i32 } else { v as i32 })
}

const DIGITIZER: u32 = 0x0d;
const TOUCH_PAD: u32 = 0x05;
const FINGER: u32 = 0x22;
const TIP_SWITCH: u32 = 0x42;
const CONTACT_ID: u32 = 0x51;
const CONTACT_COUNT: u32 = 0x54;

/// Where a touch pad's input report (Digitizer, Touch Pad) puts its first
/// finger, its button and how many fingers touch.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TouchLayout {
    pub report_id: Option<u8>,
    pub tip: Option<Field>,
    pub contact: Option<Field>,
    pub x: Option<Field>,
    pub y: Option<Field>,
    /// The button's bit (a clickpad's press).
    pub button: Option<usize>,
    pub count: Option<Field>,
}

/// The first finger of a touch pad report.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Touch {
    pub touching: bool,
    /// Which finger it is: the same while it stays down.
    pub contact: i32,
    pub x: i32,
    pub y: i32,
    pub fingers: i32,
    pub button: bool,
}

/// Finds a touch pad in a report descriptor: the first finger of its input
/// report, with absolute X and Y.
pub fn touchpad_layout(descriptor: &[u8]) -> Option<TouchLayout> {
    let mut usage_page = 0u32;
    let mut report_size = 0usize;
    let mut report_count = 0usize;
    let mut logical_min = 0i64;
    let mut report_id: Option<u8> = None;
    let mut usages: alloc::vec::Vec<u32> = alloc::vec::Vec::new();
    let mut usage_min: Option<u32> = None;
    let mut offsets: alloc::collections::BTreeMap<Option<u8>, usize> = Default::default();
    let mut depth = 0usize;
    // The depth of the touch pad's application collection, while in it.
    let mut in_pad: Option<usize> = None;
    // Fingers met so far, and the depth of the first one's collection.
    let mut fingers = 0;
    let mut in_finger: Option<usize> = None;
    let mut layout: Option<TouchLayout> = None;

    let mut i = 0;
    while i < descriptor.len() {
        let prefix = descriptor[i];
        if prefix == 0xfe {
            let size = *descriptor.get(i + 1)? as usize;
            i += 3 + size;
            continue;
        }
        let size = [0, 1, 2, 4][(prefix & 3) as usize];
        let data = descriptor.get(i + 1..i + 1 + size)?;
        i += 1 + size;
        let value = data.iter().rev().fold(0u32, |v, b| (v << 8) | *b as u32);
        let signed = match size {
            1 => value as u8 as i8 as i64,
            2 => value as u16 as i16 as i64,
            _ => value as i32 as i64,
        };
        let full_usage = |u: u32| if size == 4 { u } else { (usage_page << 16) | u };
        match (prefix >> 2) & 3 {
            0 => {
                match prefix >> 4 {
                    0xa => {
                        depth += 1;
                        let usage = usages.first().copied().unwrap_or(0);
                        if value == 1 && in_pad.is_none() && layout.is_none() && usage == (DIGITIZER << 16 | TOUCH_PAD) {
                            in_pad = Some(depth);
                        } else if in_pad.is_some() && usage == (DIGITIZER << 16 | FINGER) {
                            fingers += 1;
                            if fingers == 1 {
                                in_finger = Some(depth);
                            }
                        }
                    }
                    0xc => {
                        if in_finger == Some(depth) {
                            in_finger = None;
                        }
                        if in_pad == Some(depth) {
                            in_pad = None;
                        }
                        depth = depth.saturating_sub(1);
                    }
                    // Input.
                    0x8 => {
                        let offset = offsets.entry(report_id).or_insert(0);
                        let start = *offset;
                        *offset += report_size * report_count;
                        let constant = value & 1 != 0;
                        let relative = value & 4 != 0;
                        if in_pad.is_some() && !constant {
                            let l = layout.get_or_insert(TouchLayout { report_id, ..Default::default() });
                            if l.report_id == report_id {
                                let field = |n: usize| Field { offset: start + n * report_size, size: report_size, signed: logical_min < 0 };
                                for n in 0..report_count {
                                    let usage = match (usages.get(n).or(usages.last()), usage_min) {
                                        (Some(u), _) => *u,
                                        (None, Some(min)) => min + n as u32,
                                        (None, None) => 0,
                                    };
                                    match usage {
                                        u if in_finger.is_some() && u == (DIGITIZER << 16 | TIP_SWITCH) => l.tip = Some(field(n)),
                                        u if in_finger.is_some() && u == (DIGITIZER << 16 | CONTACT_ID) => l.contact = Some(field(n)),
                                        u if in_finger.is_some() && !relative && u == (GENERIC_DESKTOP << 16 | X) => l.x = Some(field(n)),
                                        u if in_finger.is_some() && !relative && u == (GENERIC_DESKTOP << 16 | Y) => l.y = Some(field(n)),
                                        u if u == (BUTTON << 16 | 1) && l.button.is_none() => l.button = Some(start + n * report_size),
                                        u if u == (DIGITIZER << 16 | CONTACT_COUNT) => l.count = Some(field(n)),
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
                usages.clear();
                usage_min = None;
            }
            1 => match prefix >> 4 {
                0x0 => usage_page = value,
                0x1 => logical_min = signed,
                0x7 => report_size = value as usize,
                0x8 => report_id = Some(value as u8),
                0x9 => report_count = value as usize,
                _ => {}
            },
            2 => match prefix >> 4 {
                0x0 => usages.push(full_usage(value)),
                0x1 => usage_min = Some(full_usage(value)),
                _ => {}
            },
            _ => {}
        }
    }
    layout.filter(|l| l.tip.is_some() && l.x.is_some() && l.y.is_some())
}

/// The first finger of a touch pad report (with its report id first, when
/// the device uses them); `None` for another report.
pub fn parse_touch_report(layout: &TouchLayout, report: &[u8]) -> Option<Touch> {
    let data = match layout.report_id {
        Some(id) => report.strip_prefix(&[id])?,
        None => report,
    };
    let get = |f: Option<Field>| f.and_then(|f| bits(data, f));
    Some(Touch {
        touching: get(layout.tip)? != 0,
        contact: get(layout.contact).unwrap_or(0),
        x: get(layout.x)?,
        y: get(layout.y)?,
        fingers: get(layout.count).unwrap_or(1),
        button: layout.button.and_then(|b| bits(data, Field { offset: b, size: 1, signed: false })) == Some(1),
    })
}

/// A mouse report, from an input report (with its report id first, when
/// the device uses them); `None` for another report.
pub fn parse_mouse_report(layout: &MouseLayout, report: &[u8]) -> Option<MouseReport> {
    let data = match layout.report_id {
        Some(id) => report.strip_prefix(&[id])?,
        None => report,
    };
    let button = |n: usize| layout.buttons.and_then(|b| bits(data, Field { offset: b + n, size: 1, signed: false })) == Some(1);
    Some(MouseReport {
        dx: bits(data, layout.x?)?,
        dy: bits(data, layout.y?)?,
        left: button(0),
        right: button(1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Framework Laptop 13 (AMD Ryzen AI 300)'s touchpad (PIXA 093a:0274),
    /// which its firmware leaves reporting fingers: its report descriptor.
    const FRAMEWORK: &str = concat!(
        "05 01 09 02 a1 01 85 02 05 01 09 01 a1 00 05 09 19 01 29 02 15 00 25 01 75 01 95 02 81 02 05 0d ",
        "09 32 75 01 95 01 81 02 75 05 95 01 81 03 05 01 75 10 95 01 35 00 45 00 16 00 80 26 ff 7f 09 30 ",
        "81 26 16 00 80 26 ff 7f 09 31 81 26 c0 c0 05 0d 09 05 a1 01 85 01 05 0d 09 22 a1 02 09 47 09 42 ",
        "15 00 25 01 75 01 95 02 81 02 95 06 81 03 09 51 25 0f 75 08 95 01 81 02 05 01 09 30 75 10 55 0e ",
        "65 11 35 00 46 5a 04 27 39 05 00 00 81 02 09 31 46 da 02 27 6c 03 00 00 81 02 c0 05 0d 09 22 a1 ",
        "02 09 47 09 42 15 00 25 01 75 01 95 02 81 02 95 06 81 03 09 51 25 0f 75 08 95 01 81 02 05 01 09 ",
        "30 75 10 55 0e 65 11 35 00 46 5a 04 27 39 05 00 00 81 02 09 31 46 da 02 27 6c 03 00 00 81 02 c0 ",
        "05 0d 09 22 a1 02 09 47 09 42 15 00 25 01 75 01 95 02 81 02 95 06 81 03 09 51 25 0f 75 08 95 01 ",
        "81 02 05 01 09 30 75 10 55 0e 65 11 35 00 46 5a 04 27 39 05 00 00 81 02 09 31 46 da 02 27 6c 03 ",
        "00 00 81 02 c0 05 0d 09 22 a1 02 09 47 09 42 15 00 25 01 75 01 95 02 81 02 95 06 81 03 09 51 25 ",
        "0f 75 08 95 01 81 02 05 01 09 30 75 10 55 0e 65 11 35 00 46 5a 04 27 39 05 00 00 81 02 09 31 46 ",
        "da 02 27 6c 03 00 00 81 02 c0 05 0d 09 22 a1 02 09 47 09 42 15 00 25 01 75 01 95 02 81 02 95 06 ",
        "81 03 09 51 25 0f 75 08 95 01 81 02 05 01 09 30 75 10 55 0e 65 11 35 00 46 5a 04 27 39 05 00 00 ",
        "81 02 09 31 46 da 02 27 6c 03 00 00 81 02 c0 05 0d 09 54 15 00 25 05 75 08 95 01 81 02 05 09 09 ",
        "01 09 02 09 03 15 00 25 01 75 01 95 03 81 02 95 05 81 03 05 0d 09 56 55 0c 66 01 10 35 00 47 ff ",
        "ff 00 00 15 00 27 ff ff 00 00 75 10 95 01 81 02 05 0d 09 55 15 00 25 05 75 08 95 01 85 03 b1 02 ",
        "05 0d 09 59 15 00 25 0f 75 08 95 01 85 04 b1 02 06 00 ff 85 05 75 08 15 00 09 c6 25 08 95 01 b1 ",
        "02 09 c7 26 ff 00 95 20 b1 02 c0 05 0d 09 0e a1 01 05 0d 09 22 a1 02 09 52 15 00 25 0a 75 08 95 ",
        "01 85 06 b1 02 c0 05 0d 09 22 a1 00 09 57 09 58 15 00 25 01 75 01 95 02 85 07 b1 02 95 06 b1 03 ",
        "c0 05 0d 09 60 15 00 25 01 75 01 95 01 85 08 b1 02 95 07 b1 03 c0 06 00 ff 09 01 a1 01 85 42 09 ",
        "06 15 00 26 ff 00 75 08 95 03 b1 02 06 00 ff 09 05 15 00 26 ff 00 75 08 96 00 01 85 41 b1 02 85 ",
        "43 09 06 15 00 26 ff 00 75 08 95 03 b1 02 c0",
    );

    fn hex(s: &str) -> alloc::vec::Vec<u8> {
        s.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
    }

    #[test]
    fn framework_touchpad_reports_fingers() {
        let d = hex(FRAMEWORK);
        let l = touchpad_layout(&d).unwrap();
        assert_eq!(l.report_id, Some(1));
        // What it sent: a finger down (contact 0), and lifted.
        let zeros = " 00".repeat(24);
        let mut down = hex(&format!("01 03 00 3b 02 bd 01{zeros} 01 00 1e b7"));
        let t = parse_touch_report(&l, &down).unwrap();
        assert_eq!((t.touching, t.contact, t.x, t.y, t.fingers, t.button), (true, 0, 0x23b, 0x1bd, 1, false));
        down[32] = 1; // the button
        assert!(parse_touch_report(&l, &down).unwrap().button);
        let up = hex(&format!("01 01 00 37 02 b7 01{zeros} 01 00 bd be"));
        assert!(!parse_touch_report(&l, &up).unwrap().touching);
        assert_eq!(parse_touch_report(&l, &hex("02 00 01 00 01 00")), None, "the mouse report");
        // Its mouse collection is found too.
        assert_eq!(mouse_layout(&d).unwrap().report_id, Some(2));
    }

    /// A precision touchpad's descriptor, shortened: its mouse collection
    /// (report 2), then its touchpad collection (report 1, absolute).
    const TOUCHPAD: &[u8] = &[
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x02, // Usage (Mouse)
        0xa1, 0x01, // Collection (Application)
        0x85, 0x02, //   Report ID (2)
        0x09, 0x01, //   Usage (Pointer)
        0xa1, 0x00, //   Collection (Physical)
        0x05, 0x09, //     Usage Page (Button)
        0x19, 0x01, //     Usage Minimum (1)
        0x29, 0x02, //     Usage Maximum (2)
        0x15, 0x00, //     Logical Minimum (0)
        0x25, 0x01, //     Logical Maximum (1)
        0x75, 0x01, //     Report Size (1)
        0x95, 0x02, //     Report Count (2)
        0x81, 0x02, //     Input (Data, Variable, Absolute)
        0x95, 0x06, //     Report Count (6)
        0x81, 0x03, //     Input (Constant)
        0x05, 0x01, //     Usage Page (Generic Desktop)
        0x09, 0x30, //     Usage (X)
        0x09, 0x31, //     Usage (Y)
        0x15, 0x81, //     Logical Minimum (-127)
        0x25, 0x7f, //     Logical Maximum (127)
        0x75, 0x08, //     Report Size (8)
        0x95, 0x02, //     Report Count (2)
        0x81, 0x06, //     Input (Data, Variable, Relative)
        0xc0, //       End Collection
        0xc0, //     End Collection
        0x05, 0x0d, // Usage Page (Digitizer)
        0x09, 0x05, // Usage (Touch Pad)
        0xa1, 0x01, // Collection (Application)
        0x85, 0x01, //   Report ID (1)
        0x09, 0x22, //   Usage (Finger)
        0xa1, 0x02, //   Collection (Logical)
        0x05, 0x01, //     Usage Page (Generic Desktop)
        0x09, 0x30, //     Usage (X)
        0x75, 0x10, //     Report Size (16)
        0x95, 0x01, //     Report Count (1)
        0x81, 0x02, //     Input (Data, Variable, Absolute)
        0xc0, //       End Collection
        0xc0, //     End Collection
    ];

    #[test]
    fn touchpad_mouse_collection() {
        let layout = mouse_layout(TOUCHPAD).unwrap();
        assert_eq!(layout.report_id, Some(2));
        assert_eq!(layout.buttons, Some(0));
        assert_eq!(layout.x, Some(Field { offset: 8, size: 8, signed: true }));
        assert_eq!(layout.y, Some(Field { offset: 16, size: 8, signed: true }));

        let report = parse_mouse_report(&layout, &[2, 0b01, 5, (-3i8) as u8]).unwrap();
        assert_eq!(report, MouseReport { dx: 5, dy: -3, left: true, right: false });
        let report = parse_mouse_report(&layout, &[2, 0b10, 0, 0]).unwrap();
        assert!(report.right && !report.left);
        // The touchpad's own reports (fingers) aren't the mouse's.
        assert_eq!(parse_mouse_report(&layout, &[1, 0, 0, 0]), None);
    }

    #[test]
    fn boot_mouse_without_report_ids() {
        // A plain mouse with 16-bit movement and no report ids.
        let d: &[u8] = &[
            0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x15, 0x00, 0x25, 0x01, 0x75,
            0x01, 0x95, 0x03, 0x81, 0x02, 0x75, 0x05, 0x95, 0x01, 0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31,
            0x16, 0x01, 0x80, 0x26, 0xff, 0x7f, 0x75, 0x10, 0x95, 0x02, 0x81, 0x06, 0xc0,
        ];
        let layout = mouse_layout(d).unwrap();
        assert_eq!(layout.report_id, None);
        let report = parse_mouse_report(&layout, &[0b11, 0x2c, 0x01, 0xff, 0xff]).unwrap();
        assert_eq!(report, MouseReport { dx: 300, dy: -1, left: true, right: true });
    }

    #[test]
    fn no_mouse() {
        // Only the absolute touchpad collection: nothing to use.
        let touchpad = TOUCHPAD.windows(2).position(|w| w == [0x05, 0x0d]).unwrap();
        assert_eq!(mouse_layout(&TOUCHPAD[touchpad..]), None);
        assert_eq!(mouse_layout(&[0x05]), None);
    }

    #[test]
    fn i2c_hid_descriptor() {
        let mut d = [0u8; 30];
        for (i, v) in [30u16, 0x100, 700, 2, 3, 64, 4, 64, 5, 6, 0x093a, 0x0274].iter().enumerate() {
            d[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
        }
        let h = parse_i2c_hid_descriptor(&d).unwrap();
        assert_eq!((h.report_descriptor_length, h.report_descriptor_register, h.input_register), (700, 2, 3));
        assert_eq!((h.max_input_length, h.command_register, h.data_register), (64, 5, 6));
        assert_eq!((h.vendor, h.product), (0x093a, 0x0274));
        assert_eq!(parse_i2c_hid_descriptor(&d[..20]), None);
        d[0] = 31;
        assert_eq!(parse_i2c_hid_descriptor(&d), None);
    }
}
