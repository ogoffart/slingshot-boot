// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Reads folders on any file system, including those of file system
//! drivers such as rEFInd's (ext4, btrfs…).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use uefi::proto::media::file::{File, FileAttribute, FileInfo, FileMode, RegularFile};
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::{CString16, Handle};

/// `len` bytes at `offset` of the disk or partition `handle`.
pub fn read_disk(handle: Handle, media_id: u32, offset: u64, len: usize) -> Option<Vec<u8>> {
    let disk = crate::uefi_platform::open_shared::<uefi::proto::media::disk::DiskIo>(handle)?;
    let mut data = zeroed(len)?;
    disk.read_disk(media_id, offset, &mut data).ok()?;
    Some(data)
}

/// A partition without a file system the firmware reads, and what it holds
/// ("ext4", "LUKS"…, from `parse::fstype`).
pub struct Unreadable {
    pub device_path: Vec<u8>,
    pub kind: &'static str,
    pub size: u64,
}

/// The partitions no file system driver took: what they are, logged too
/// (the file browser shows those it recognizes, to say why).
pub fn unreadable_partitions() -> Vec<Unreadable> {
    use uefi::proto::device_path::DevicePath;
    use uefi::proto::media::block::BlockIO;
    let mut found = Vec::new();
    let Ok(handles) = uefi::boot::find_handles::<BlockIO>() else { return found };
    for handle in handles {
        let Some(block) = crate::uefi_platform::open_shared::<BlockIO>(handle) else { continue };
        let media = block.media();
        let (partition, present) = (media.is_logical_partition(), media.is_media_present());
        let (media_id, size) = (media.media_id(), (media.last_block() + 1) * u64::from(media.block_size()));
        drop(block);
        let Some(dp) = crate::uefi_platform::open_shared::<DevicePath>(handle).map(|d| d.as_bytes().to_vec()) else { continue };
        let what = crate::parse::load_option::describe_volume(&dp).1;
        let readable = crate::uefi_platform::open_shared::<SimpleFileSystem>(handle).is_some();
        // Whole devices too: a disk formatted without a partition table.
        if !present || readable {
            log::info!(
                "disk: {what}, {size} bytes{}{}",
                if partition { "" } else { ", whole device" },
                if !present { ", no media" } else if readable { ", file system" } else { "" }
            );
            continue;
        }
        let head = read_disk(handle, media_id, 0, crate::parse::fstype::HEAD).unwrap_or_default();
        let at_64k = if size > crate::parse::fstype::BTRFS_AT + crate::parse::fstype::HEAD as u64 {
            read_disk(handle, media_id, crate::parse::fstype::BTRFS_AT, crate::parse::fstype::HEAD).unwrap_or_default()
        } else {
            Vec::new()
        };
        let kind = crate::parse::fstype::identify(&head, &at_64k);
        if kind.is_none() && !partition {
            log::info!("disk: {what}, {size} bytes, whole device");
            continue;
        }
        log::info!("disk: {what}, {size} bytes, no file system driver: {}", kind.unwrap_or("not recognized"));
        if let Some(kind) = kind.filter(|k| *k != "swap") {
            found.push(Unreadable { device_path: dp, kind, size });
        }
    }
    found
}

/// A folder's entry: name, whether it's a folder, size.
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

/// The entries of the folder `path` on the file system `handle`, without
/// "." and "..".
///
/// `uefi::fs` first asks for the size each entry needs, with an empty
/// buffer; rEFInd's drivers don't say (they return BUFFER_TOO_SMALL with no
/// size), so `uefi::fs` reads nothing from them. This reads into a buffer
/// that fits any entry, as rEFInd does.
pub fn read_dir(handle: Handle, path: &str) -> Result<Vec<Entry>, String> {
    let mut sfs = crate::uefi_platform::open_shared::<SimpleFileSystem>(handle).ok_or("cannot open the disk")?;
    let mut root = sfs.open_volume().map_err(|e| format!("{:?}", e.status()))?;
    let mut dir = if path.is_empty() || path == "\\" {
        root
    } else {
        let name = CString16::try_from(path).map_err(|_| String::from("bad path"))?;
        let file = root.open(&name, FileMode::Read, FileAttribute::empty()).map_err(|e| format!("{:?}", e.status()))?;
        file.into_directory().ok_or("not a folder")?
    };
    let mut entries = Vec::new();
    let mut buffer = alloc::boxed::Box::new(InfoBuffer([0; 4096]));
    loop {
        match dir.read_entry(&mut buffer.0) {
            Ok(Some(info)) => {
                let name = format!("{}", info.file_name());
                if name != "." && name != ".." {
                    entries.push(Entry { name, is_dir: info.is_directory(), size: info.file_size() });
                }
            }
            Ok(None) => break,
            Err(e) => return Err(format!("{:?}", e.status())),
        }
    }
    Ok(entries)
}

/// Bytes for a `FileInfo`: 80, then a name of up to 255 UCS-2 characters
/// on usual file systems.
#[repr(C, align(8))]
pub struct InfoBuffer(pub [u8; 4096]);

/// Opens `path` on the file system `handle`.
fn open(handle: Handle, path: &str, mode: FileMode) -> Option<uefi::proto::media::file::FileHandle> {
    let mut sfs = crate::uefi_platform::open_shared::<SimpleFileSystem>(handle)?;
    let mut root = sfs.open_volume().ok()?;
    root.open(&CString16::try_from(path).ok()?, mode, FileAttribute::empty()).ok()
}

/// Whether the file `path` exists on the file system `handle`.
pub fn exists(handle: Handle, path: &str) -> bool {
    open(handle, path, FileMode::Read).is_some()
}

/// The contents of the file `path` on the file system `handle`. Like
/// [`read_dir`], with a buffer for the file's information that fits any
/// name (`uefi::fs` asks its size first, which rEFInd's drivers don't say).
pub fn read_file(handle: Handle, path: &str) -> Option<Vec<u8>> {
    read_at_most(handle, path, usize::MAX)
}

/// The most a text file (a configuration, os-release…) may take: a file
/// system that says a few GB doesn't get them.
pub const SMALL: usize = 1 << 20;

/// Like `read_file`, `None` if the file is larger than `max` bytes.
pub fn read_at_most(handle: Handle, path: &str, max: usize) -> Option<Vec<u8>> {
    let mut file = open(handle, path, FileMode::Read)?.into_regular_file()?;
    let size = usize::try_from(file.get_boxed_info::<FileInfo>().ok()?.file_size()).ok()?;
    if size > max {
        log::info!("{path}: {size} bytes, too large");
        return None;
    }
    read_up_to(&mut file, size)
}

/// `len` zeros, or `None` when there isn't the memory (a size read from a
/// disk can be anything).
fn zeroed(len: usize) -> Option<Vec<u8>> {
    let mut data = Vec::new();
    data.try_reserve_exact(len).ok()?;
    data.resize(len, 0);
    Some(data)
}

/// The first `len` bytes of the file `path` (fewer if it's shorter).
#[cfg_attr(not(feature = "luks"), allow(dead_code))] // the LUKS handoff's
pub fn read_file_start(handle: Handle, path: &str, len: usize) -> Option<Vec<u8>> {
    read_up_to(&mut open(handle, path, FileMode::Read)?.into_regular_file()?, len)
}

fn read_up_to(file: &mut RegularFile, len: usize) -> Option<Vec<u8>> {
    let mut data = zeroed(len)?;
    let mut read = 0;
    while read < data.len() {
        match file.read(&mut data[read..]) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(_) => return None,
        }
    }
    data.truncate(read);
    Some(data)
}

/// The file system with the device path `fs_device_path`.
pub fn handle_of(fs_device_path: &[u8]) -> Option<Handle> {
    let dp: &uefi::proto::device_path::DevicePath = fs_device_path.try_into().ok()?;
    uefi::boot::locate_device_path::<SimpleFileSystem>(&mut &*dp).ok()
}
