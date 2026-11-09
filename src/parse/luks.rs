// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! LUKS2 volumes: what Slingshot needs to check a passphrase and get the
//! volume key, read-only (no writing, no dm-crypt). The steps cryptsetup
//! takes to open a volume:
//!
//! 1. the binary header (4 KiB), then the JSON metadata after it;
//! 2. per key slot: derive a key from the passphrase (Argon2i/Argon2id or
//!    PBKDF2), decrypt the slot's area with it (AES-XTS, 512-byte sectors,
//!    "plain64" sector numbers), and merge the anti-forensic stripes;
//! 3. the candidate volume key is right if its PBKDF2 digest matches.
//!
//! Format reference: "LUKS2 On-Disk Format Specification" (cryptsetup), and
//! luks-rs (MIT) for the details.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use base64::Engine;
use serde_json::Value;
use sha2::Digest;

use super::wipe;

const MAGIC: &[u8; 6] = b"LUKS\xba\xbe";

/// A LUKS2 header, as much of it as unlocking needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    /// The volume's UUID (what `rd.luks.uuid=` names).
    pub uuid: String,
    /// The size of the header with its JSON area: read that much first.
    pub header_size: u64,
    pub keyslots: Vec<Keyslot>,
    pub digests: Vec<KeyDigest>,
    /// The token types (`systemd-tpm2`, `systemd-fido2`…): other ways to
    /// unlock it.
    pub tokens: Vec<String>,
    /// Where the data is, when Slingshot can decrypt it (AES-XTS, without
    /// integrity protection): to read its files.
    pub segment: Option<Segment>,
}

/// The encrypted data of a volume.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    /// Its start, in bytes from the start of the device.
    pub offset: u64,
    /// Its size, or `None` for the rest of the device ("dynamic").
    pub size: Option<u64>,
    /// The size of what's encrypted as a unit: 512 or 4096 bytes.
    pub sector_size: u32,
    /// Added to the sector numbers (in 512-byte sectors).
    pub iv_tweak: u64,
}

impl Header {
    /// Whether the volume normally unlocks without a passphrase (a TPM, a
    /// security key): Slingshot doesn't ask then, the initrd does it. Not a
    /// recovery key (`systemd-recovery`): that's typed like a passphrase.
    pub fn unlocks_by_itself(&self) -> bool {
        self.tokens.iter().any(|t| matches!(t.as_str(), "systemd-tpm2" | "systemd-fido2" | "systemd-pkcs11"))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Keyslot {
    pub id: String,
    /// The volume key's size.
    pub key_size: usize,
    pub kdf: Kdf,
    /// Where the encrypted stripes are, from the start of the device.
    pub area_offset: u64,
    pub area_key_size: usize,
    pub stripes: usize,
    pub af_hash: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kdf {
    Pbkdf2 { hash: String, iterations: u32, salt: Vec<u8> },
    /// Argon2i or Argon2id: memory in KiB.
    Argon2 { id: bool, time: u32, memory: u32, cpus: u32, salt: Vec<u8> },
}

/// How a candidate volume key is checked.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyDigest {
    pub keyslots: Vec<String>,
    pub hash: String,
    pub iterations: u32,
    pub salt: Vec<u8>,
    pub digest: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    NotLuks2,
    /// Something this code doesn't handle (a cipher, a hash…).
    Unsupported(String),
    Malformed(&'static str),
    /// The device couldn't be read.
    Read,
    /// Not enough memory for the key derivation.
    OutOfMemory,
    WrongPassphrase,
    /// `CANCEL` was set while checking.
    Cancelled,
}

/// Set (from another processor, or the UI) to stop the passphrase check
/// being run: `Attempt::run` then gives `Error::Cancelled` soon after.
pub static CANCEL: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// When the check runs on the processor that draws the screen (no other
/// one to run it), nothing reads the keyboard meanwhile: this, set with
/// `set_poll`, is asked between steps whether to stop (Escape pressed).
static POLL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// `poll`, asked while checking on this processor; `None` after.
pub fn set_poll(poll: Option<fn() -> bool>) {
    POLL.store(poll.map_or(0, |f| f as usize), core::sync::atomic::Ordering::Release);
}

fn cancelled() -> bool {
    use core::sync::atomic::Ordering;
    if CANCEL.load(Ordering::Acquire) {
        return true;
    }
    let poll = POLL.load(Ordering::Acquire);
    if poll != 0 {
        // SAFETY: stored by `set_poll` from a `fn() -> bool`.
        let poll: fn() -> bool = unsafe { core::mem::transmute(poll) };
        if poll() {
            CANCEL.store(true, Ordering::Release);
            return true;
        }
    }
    false
}

/// A lane runner that stops computing once `CANCEL` is set.
struct Stoppable<'a>(&'a dyn argon2::LaneRunner);

// SAFETY: runs the lanes with the runner it wraps.
unsafe impl argon2::LaneRunner for Stoppable<'_> {
    fn run(&self, lanes: usize, segment: &(dyn Fn(usize) + Sync)) {
        self.0.run(lanes, segment);
    }
    fn stop(&self) -> bool {
        cancelled() || self.0.stop()
    }
}

impl Keyslot {
    /// The bytes of the area the stripes take (whole 512-byte sectors): each
    /// stripe is the size of the volume key, not of the area's key.
    pub fn material_size(&self) -> usize {
        (self.key_size * self.stripes).div_ceil(512) * 512
    }
}

fn number(v: &Value) -> Option<u64> {
    // LUKS2 writes the large numbers as strings.
    v.as_u64().or_else(|| v.as_str()?.parse().ok())
}

/// A number of the header within `range`: the header comes from the disk,
/// and a crafted one mustn't make the check overflow, run for hours or take
/// all the memory. The ranges are wider than what cryptsetup makes.
fn bounded(v: &Value, range: core::ops::RangeInclusive<u64>, what: &'static str) -> Result<u64, Error> {
    number(v).filter(|n| range.contains(n)).ok_or(Error::Malformed(what))
}

/// At most: PBKDF2's iterations (cryptsetup picks about 2 s worth, a few
/// million on fast processors), Argon2's passes and lanes (cryptsetup:
/// at most 4 lanes), its memory in KiB (cryptsetup: at most 4 GiB), the
/// key slots LUKS2 has, the volume key's bytes.
const MAX_ITERATIONS: u64 = 100_000_000;
const MAX_PASSES: u64 = 1000;
const MAX_LANES: u64 = 64;
const MAX_MEMORY: u64 = 16 << 20;
const MAX_KEYSLOTS: usize = 32;
const MAX_KEY_SIZE: u64 = 128;

fn bytes(v: &Value) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(v.as_str()?).ok()
}

/// Whether `data` (the device's first bytes) starts a LUKS volume.
pub fn is_luks(data: &[u8]) -> bool {
    data.starts_with(MAGIC)
}

/// The header size from the first 4 KiB, to read the rest.
pub fn header_size(first: &[u8]) -> Result<u64, Error> {
    if first.len() < 16 || !first.starts_with(MAGIC) {
        return Err(Error::NotLuks2);
    }
    if u16::from_be_bytes([first[6], first[7]]) != 2 {
        return Err(Error::Unsupported(String::from("LUKS1 (only LUKS2 is supported)")));
    }
    let size = u64::from_be_bytes(first[8..16].try_into().unwrap());
    if !(4096..=4 << 20).contains(&size) {
        return Err(Error::Malformed("header size"));
    }
    Ok(size)
}

/// Parses the header: `data` is its first `header_size` bytes.
pub fn parse(data: &[u8]) -> Result<Header, Error> {
    let header_size = header_size(data)?;
    if (data.len() as u64) < header_size {
        return Err(Error::Malformed("short header"));
    }
    let uuid = data.get(0xa8..0xa8 + 40).ok_or(Error::Malformed("uuid"))?;
    let uuid = String::from_utf8_lossy(uuid.split(|b| *b == 0).next().unwrap_or_default()).into_owned();
    let json_area = &data[4096..header_size as usize];
    let json_len = json_area.iter().position(|b| *b == 0).unwrap_or(json_area.len());
    let json: Value = serde_json::from_slice(&json_area[..json_len]).map_err(|_| Error::Malformed("JSON"))?;

    let mut keyslots = Vec::new();
    for (id, slot) in json["keyslots"].as_object().ok_or(Error::Malformed("keyslots"))? {
        if slot["type"] != "luks2" {
            continue;
        }
        let kdf = &slot["kdf"];
        let kdf = match kdf["type"].as_str() {
            Some("pbkdf2") => Kdf::Pbkdf2 {
                hash: kdf["hash"].as_str().unwrap_or("sha256").to_string(),
                iterations: bounded(&kdf["iterations"], 1..=MAX_ITERATIONS, "iterations")? as u32,
                salt: bytes(&kdf["salt"]).ok_or(Error::Malformed("salt"))?,
            },
            Some(t @ ("argon2i" | "argon2id")) => Kdf::Argon2 {
                id: t == "argon2id",
                time: bounded(&kdf["time"], 1..=MAX_PASSES, "time")? as u32,
                memory: bounded(&kdf["memory"], 8..=MAX_MEMORY, "memory")? as u32,
                cpus: if kdf["cpus"].is_null() { 1 } else { bounded(&kdf["cpus"], 1..=MAX_LANES, "cpus")? as u32 },
                salt: bytes(&kdf["salt"]).ok_or(Error::Malformed("salt"))?,
            },
            other => return Err(Error::Unsupported(alloc::format!("key derivation {other:?}"))),
        };
        let area = &slot["area"];
        if area["encryption"] != "aes-xts-plain64" {
            return Err(Error::Unsupported(alloc::format!("key slot encryption {}", area["encryption"])));
        }
        if keyslots.len() == MAX_KEYSLOTS {
            return Err(Error::Malformed("too many key slots"));
        }
        keyslots.push(Keyslot {
            id: id.clone(),
            key_size: bounded(&slot["key_size"], 1..=MAX_KEY_SIZE, "key_size")? as usize,
            kdf,
            area_offset: bounded(&area["offset"], 0..=1 << 50, "area offset")?,
            area_key_size: bounded(&area["key_size"], 1..=MAX_KEY_SIZE, "area key_size")? as usize,
            // What LUKS2 always has (the specification's only value).
            stripes: if slot["af"]["stripes"].is_null() { 4000 } else { bounded(&slot["af"]["stripes"], 4000..=4000, "stripes")? as usize },
            af_hash: slot["af"]["hash"].as_str().unwrap_or("sha256").to_string(),
        });
    }
    // Lowest priority last: cryptsetup tries the slots in order.
    keyslots.sort_by_key(|k| k.id.parse::<u32>().unwrap_or(u32::MAX));

    let mut digests = Vec::new();
    for digest in json["digests"].as_object().ok_or(Error::Malformed("digests"))?.values() {
        if digest["type"] != "pbkdf2" {
            continue;
        }
        let hash = digest["hash"].as_str().unwrap_or("sha256").to_string();
        let value = bytes(&digest["digest"]).ok_or(Error::Malformed("digest"))?;
        // As long as the hash: a short (or empty) one would let wrong
        // passphrases through.
        if value.len() != hash_size(&hash)? {
            return Err(Error::Malformed("digest size"));
        }
        if digests.len() == MAX_KEYSLOTS {
            return Err(Error::Malformed("too many digests"));
        }
        digests.push(KeyDigest {
            keyslots: digest["keyslots"]
                .as_array()
                .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
                .unwrap_or_default(),
            hash,
            iterations: bounded(&digest["iterations"], 1..=MAX_ITERATIONS, "digest iterations")? as u32,
            salt: bytes(&digest["salt"]).ok_or(Error::Malformed("digest salt"))?,
            digest: value,
        });
    }
    let tokens = json["tokens"]
        .as_object()
        .map(|t| t.values().filter_map(|t| t["type"].as_str().map(String::from)).collect())
        .unwrap_or_default();
    let segment = json["segments"].as_object().and_then(|segments| {
        // The first one: there's a single one unless a reencryption is
        // under way (then Slingshot doesn't read it).
        let (_, s) = segments.iter().min_by_key(|(id, _)| id.parse::<u32>().unwrap_or(u32::MAX))?;
        let supported = segments.len() == 1
            && s["type"] == "crypt"
            && s["encryption"] == "aes-xts-plain64"
            && s.get("integrity").is_none_or(Value::is_null);
        let sector_size = number(&s["sector_size"]).unwrap_or(512) as u32;
        let segment = Segment {
            offset: number(&s["offset"])?,
            size: if s["size"] == "dynamic" { None } else { Some(number(&s["size"])?) },
            sector_size,
            iv_tweak: number(&s["iv_tweak"]).unwrap_or(0),
        };
        (supported && matches!(sector_size, 512 | 1024 | 2048 | 4096)).then_some(segment)
    });
    Ok(Header { uuid, header_size, keyslots, digests, tokens, segment })
}

/// The hashes LUKS2 volumes use (`pbkdf2` and the AF stripes).
fn check_hash(hash: &str) -> Result<(), Error> {
    match hash {
        "sha256" | "sha512" => Ok(()),
        other => Err(Error::Unsupported(alloc::format!("hash {other}"))),
    }
}

/// The output size of a hash `check_hash` accepts.
fn hash_size(hash: &str) -> Result<usize, Error> {
    check_hash(hash)?;
    Ok(if hash == "sha512" { 64 } else { 32 })
}

/// `hash` was checked by `check_hash`. Stops early, with `Cancelled`, when
/// `CANCEL` is set: millions of iterations take seconds.
fn pbkdf2(hash: &str, password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8]) -> Result<(), Error> {
    use hmac::KeyInit;
    let bad = |_| Error::Malformed("key");
    if hash == "sha512" {
        pbkdf2_with(hmac::Hmac::<sha2::Sha512>::new_from_slice(password).map_err(bad)?, salt, iterations, out)
    } else {
        pbkdf2_with(hmac::Hmac::<sha2::Sha256>::new_from_slice(password).map_err(bad)?, salt, iterations, out)
    }
}

/// PBKDF2 (RFC 8018) with `prf`, HMAC keyed with the password.
fn pbkdf2_with<M: hmac::Mac + Clone>(prf: M, salt: &[u8], iterations: u32, out: &mut [u8]) -> Result<(), Error> {
    let mut result = Ok(());
    for (i, chunk) in out.chunks_mut(<M as hmac::digest::OutputSizeUser>::output_size()).enumerate() {
        let mut mac = prf.clone();
        mac.update(salt);
        mac.update(&(i as u32 + 1).to_be_bytes());
        let mut u = mac.finalize().into_bytes();
        let mut block = u.clone();
        for n in 1..iterations {
            if n % 4096 == 0 && cancelled() {
                result = Err(Error::Cancelled);
                break;
            }
            let mut mac = prf.clone();
            mac.update(&u);
            wipe(&mut u);
            u = mac.finalize().into_bytes();
            for (b, x) in block.iter_mut().zip(u.iter()) {
                *b ^= x;
            }
        }
        chunk.copy_from_slice(&block[..chunk.len()]);
        wipe(&mut u);
        wipe(&mut block);
        if result.is_err() {
            break;
        }
    }
    result
}

/// Compares without stopping at the first difference.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |d, (x, y)| d | (x ^ y)) == 0
}

fn argon2(kdf: &Kdf, out_len: usize) -> Option<argon2::Argon2<'static>> {
    let Kdf::Argon2 { id, time, memory, cpus, .. } = kdf else { return None };
    let params = argon2::Params::new(*memory, *time, *cpus, Some(out_len)).ok()?;
    let algorithm = if *id { argon2::Algorithm::Argon2id } else { argon2::Algorithm::Argon2i };
    Some(argon2::Argon2::new(algorithm, argon2::Version::V0x13, params))
}

/// Decrypts a key slot's area (AES-XTS, 512-byte sectors numbered from the
/// area's start). The key is 64 bytes (checked by `Attempt::new`).
fn decrypt_area(key: &[u8], area: &mut [u8]) -> Result<(), Error> {
    use aes::cipher::KeyInit;
    let (k1, k2) = key.split_at(key.len() / 2);
    let xts = xts_mode::Xts128::new(
        aes::Aes256::new_from_slice(k1).map_err(|_| Error::Malformed("key"))?,
        aes::Aes256::new_from_slice(k2).map_err(|_| Error::Malformed("key"))?,
    );
    xts.decrypt_area(area, 512, 0, xts_mode::get_tweak_default);
    Ok(())
}

/// Decrypts a volume's data (read-only), as dm-crypt does for LUKS2:
/// AES-XTS, the tweak being the sector's number ("plain64"), counted in the
/// segment's sector size (`iv_large_sectors`, which cryptsetup sets for
/// LUKS2 sectors larger than 512 bytes).
pub struct Decryptor {
    cipher: Cipher,
    sector_size: usize,
    iv_tweak: u64,
}

// Not boxed: the expanded keys stay inside the `Decryptor`, where the
// firmware wipes them (`firmware::luks::wipe_mapped`).
#[allow(clippy::large_enum_variant)]
enum Cipher {
    Aes128(xts_mode::Xts128<aes::Aes128>),
    Aes256(xts_mode::Xts128<aes::Aes256>),
}

impl Decryptor {
    /// `key` is the volume key: 32 bytes (AES-128) or 64 (AES-256).
    pub fn new(segment: &Segment, key: &[u8]) -> Result<Decryptor, Error> {
        use aes::cipher::KeyInit;
        let (k1, k2) = key.split_at(key.len() / 2);
        let cipher = match key.len() {
            32 => Cipher::Aes128(xts_mode::Xts128::new(
                aes::Aes128::new_from_slice(k1).map_err(|_| Error::Malformed("key"))?,
                aes::Aes128::new_from_slice(k2).map_err(|_| Error::Malformed("key"))?,
            )),
            64 => Cipher::Aes256(xts_mode::Xts128::new(
                aes::Aes256::new_from_slice(k1).map_err(|_| Error::Malformed("key"))?,
                aes::Aes256::new_from_slice(k2).map_err(|_| Error::Malformed("key"))?,
            )),
            n => return Err(Error::Unsupported(alloc::format!("{}-bit AES-XTS key", n * 8))),
        };
        Ok(Decryptor { cipher, sector_size: segment.sector_size as usize, iv_tweak: segment.iv_tweak })
    }

    pub fn sector_size(&self) -> usize {
        self.sector_size
    }

    /// Decrypts `data`, read at `offset` bytes into the segment: both
    /// multiples of the sector size.
    pub fn decrypt(&self, offset: u64, data: &mut [u8]) {
        // Wrapping, as dm-crypt's 64-bit sector numbers do.
        let first = u128::from(self.iv_tweak.wrapping_add(offset / 512) / (self.sector_size as u64 / 512));
        let tweak = xts_mode::get_tweak_default;
        match &self.cipher {
            Cipher::Aes128(x) => x.decrypt_area(data, self.sector_size, first, tweak),
            Cipher::Aes256(x) => x.decrypt_area(data, self.sector_size, first, tweak),
        }
    }
}

/// One round of the anti-forensic diffusion: each hash-sized block of `d`
/// becomes hash(block number, big-endian u32 ‖ block).
fn diffuse<H: Digest>(d: &mut [u8]) {
    let size = <H as Digest>::output_size();
    for (i, block) in d.chunks_mut(size).enumerate() {
        let mut h = H::new();
        h.update((i as u32).to_be_bytes());
        h.update(&*block);
        let mut out = h.finalize();
        block.copy_from_slice(&out[..block.len()]);
        wipe(&mut out);
    }
}

/// Merges the anti-forensic stripes back into the key, in `d` (key-sized).
/// `material` holds them all (checked by `Attempt::new`).
fn af_merge(material: &[u8], d: &mut [u8], stripes: usize, hash: &str) {
    let key_size = d.len();
    d.fill(0);
    for stripe in material.chunks(key_size).take(stripes - 1) {
        for (a, b) in d.iter_mut().zip(stripe) {
            *a ^= b;
        }
        if hash == "sha512" {
            diffuse::<sha2::Sha512>(d);
        } else {
            diffuse::<sha2::Sha256>(d);
        }
    }
    let last = &material[(stripes - 1) * key_size..stripes * key_size];
    for (a, b) in d.iter_mut().zip(last) {
        *a ^= b;
    }
}

/// Trying a passphrase on a volume: `new` reads the key slots and sets
/// aside all the memory (Argon2 needs up to a GiB or more), then `run` does
/// the slow part without allocating or touching the disk, so that it can
/// run on another processor while the screen keeps moving. Everything is
/// wiped when it's dropped.
pub struct Attempt {
    digests: Vec<KeyDigest>,
    slots: Vec<(Keyslot, Vec<u8>)>,
    passphrase: Vec<u8>,
    key: Vec<u8>,
    candidate: Vec<u8>,
    digest: Vec<u8>,
    blocks: Vec<argon2::Block>,
    block_count: usize,
    lanes: usize,
    /// Set by `run`: the index of the slot that opened, or the error.
    result: Option<Result<usize, Error>>,
}

impl Attempt {
    /// `read(offset, len)` reads the device. Fails if something isn't
    /// supported, the memory can't be found, or the device can't be read.
    pub fn new(
        header: &Header,
        passphrase: &[u8],
        mut read: impl FnMut(u64, usize) -> Option<Vec<u8>>,
    ) -> Result<Self, Error> {
        let mut slots = Vec::new();
        let mut blocks = 0;
        let mut lanes = 1;
        let mut key_len = 0;
        for slot in &header.keyslots {
            if slot.area_key_size != 64 {
                return Err(Error::Unsupported(alloc::format!("{}-bit XTS key", slot.area_key_size * 8)));
            }
            check_hash(&slot.af_hash)?;
            match &slot.kdf {
                Kdf::Pbkdf2 { hash, .. } => check_hash(hash)?,
                kdf => {
                    let a = argon2(kdf, slot.area_key_size).ok_or(Error::Malformed("argon2 parameters"))?;
                    blocks = blocks.max(a.params().block_count());
                    lanes = lanes.max(a.params().p_cost() as usize);
                }
            }
            if slot.key_size == 0 || slot.stripes == 0 {
                return Err(Error::Malformed("key slot"));
            }
            let material = read(slot.area_offset, slot.material_size()).ok_or(Error::Read)?;
            if material.len() < slot.key_size * slot.stripes {
                return Err(Error::Malformed("key slot area"));
            }
            key_len = key_len.max(slot.key_size);
            slots.push((slot.clone(), material));
        }
        for digest in &header.digests {
            check_hash(&digest.hash)?;
        }
        let digest_len = header.digests.iter().map(|d| d.digest.len()).max().unwrap_or(0);
        // Reserved here, filled by `run` (on the other processor).
        let mut memory = Vec::new();
        memory.try_reserve_exact(blocks).map_err(|_| Error::OutOfMemory)?;
        Ok(Attempt {
            digests: header.digests.clone(),
            slots,
            passphrase: passphrase.to_vec(),
            key: vec![0; 64],
            candidate: vec![0; key_len],
            digest: vec![0; digest_len],
            blocks: memory,
            block_count: blocks,
            lanes,
            result: None,
        })
    }

    /// How many processors Argon2 can use: its lanes (1 for PBKDF2).
    pub fn lanes(&self) -> usize {
        self.lanes
    }

    /// The slow part: tries each key slot in turn, with Argon2's lanes run
    /// by `lanes`. Doesn't allocate.
    pub fn run(&mut self, lanes: &dyn argon2::LaneRunner) {
        // Within the reserved capacity: doesn't allocate.
        self.blocks.resize(self.block_count, argon2::Block::default());
        let lanes = Stoppable(lanes);
        let result = (0..self.slots.len()).find_map(|i| match self.try_slot(i, &lanes) {
            _ if cancelled() => Some(Err(Error::Cancelled)),
            Ok(false) => None,
            r => Some(r.map(|_| i)),
        });
        self.result = Some(result.unwrap_or(Err(Error::WrongPassphrase)));
        // The derived keys and Argon2's memory are no longer needed.
        wipe(&mut self.key);
        wipe(&mut self.passphrase);
        let len = self.blocks.len() * argon2::Block::SIZE;
        // SAFETY: blocks are plain integers.
        wipe(unsafe { core::slice::from_raw_parts_mut(self.blocks.as_mut_ptr().cast::<u8>(), len) });
    }

    fn try_slot(&mut self, i: usize, lanes: &dyn argon2::LaneRunner) -> Result<bool, Error> {
        let (slot, material) = &mut self.slots[i];
        let key = &mut self.key[..slot.area_key_size];
        match &slot.kdf {
            Kdf::Pbkdf2 { hash, iterations, salt } => pbkdf2(hash, &self.passphrase, salt, *iterations, key)?,
            kdf @ Kdf::Argon2 { salt, .. } => argon2(kdf, key.len())
                .ok_or(Error::Malformed("argon2 parameters"))?
                .hash_password_into_with_memory_and_lanes(&self.passphrase, salt, &mut *key, &mut self.blocks[..], lanes)
                .map_err(|_| Error::Malformed("argon2"))?,
        }
        // In place: each slot is tried once per attempt.
        decrypt_area(key, material)?;
        let candidate = &mut self.candidate[..slot.key_size];
        af_merge(material, candidate, slot.stripes, &slot.af_hash);
        wipe(material);
        for digest in self.digests.iter().filter(|d| d.keyslots.contains(&slot.id)) {
            let out = &mut self.digest[..digest.digest.len()];
            pbkdf2(&digest.hash, candidate, &digest.salt, digest.iterations, out)?;
            if same(out, &digest.digest) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// After `run`: the volume key, if the passphrase was right.
    pub fn result(&self) -> Result<&[u8], Error> {
        match &self.result {
            Some(Ok(i)) => Ok(&self.candidate[..self.slots[*i].0.key_size]),
            Some(Err(e)) => Err(e.clone()),
            None => Err(Error::Malformed("not run")),
        }
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        wipe(&mut self.passphrase);
        wipe(&mut self.key);
        wipe(&mut self.candidate);
        for (_, material) in &mut self.slots {
            wipe(material);
        }
    }
}

/// The volume key, if `passphrase` opens one of the key slots: `Attempt`
/// in one go. `read(offset, len)` reads the device. For the tests and the
/// simulator (the firmware runs `Attempt` on another processor).
#[cfg_attr(target_os = "uefi", allow(dead_code))]
pub fn unlock(
    header: &Header,
    passphrase: &[u8],
    read: impl FnMut(u64, usize) -> Option<Vec<u8>>,
) -> Result<Vec<u8>, Error> {
    let mut attempt = Attempt::new(header, passphrase, read)?;
    attempt.run(&OneByOne);
    attempt.result().map(<[u8]>::to_vec)
}

/// Argon2's lanes one after the other, on this processor.
pub struct OneByOne;

// SAFETY: each lane once, in turn.
unsafe impl argon2::LaneRunner for OneByOne {
    fn run(&self, lanes: usize, segment: &(dyn Fn(usize) + Sync)) {
        (0..lanes).for_each(segment);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A LUKS2 volume made by cryptsetup, with cheap key derivation
    /// (tests/luks2-pbkdf2.img: `cryptsetup luksFormat --type luks2
    /// --pbkdf pbkdf2 --pbkdf-force-iterations 1000`; passphrase "slingshot").
    /// Its volume key, from `cryptsetup luksDump --dump-volume-key`, is in
    /// tests/luks2-pbkdf2.key.
    fn check(image: &[u8], expected_key_hex: &str) {
        let size = header_size(image).unwrap();
        let header = parse(&image[..size as usize]).unwrap();
        assert!(!header.uuid.is_empty());
        let read = |offset: u64, len: usize| image.get(offset as usize..offset as usize + len).map(<[u8]>::to_vec);
        let key = unlock(&header, b"slingshot", read).unwrap();
        let hex: String = key.iter().map(|b| alloc::format!("{b:02x}")).collect();
        assert_eq!(hex, expected_key_hex.trim());
        assert_eq!(unlock(&header, b"wrong", read), Err(Error::WrongPassphrase));
    }

    #[test]
    fn pbkdf2_volume() {
        check(include_bytes!("../../tests/luks2-pbkdf2.img"), include_str!("../../tests/luks2-pbkdf2.key"));
    }

    /// The test volume's header with `from` replaced by `to` in its JSON
    /// (the checksum isn't checked).
    fn crafted(from: &str, to: &str) -> Result<Header, Error> {
        let image = include_bytes!("../../tests/luks2-pbkdf2.img");
        let size = header_size(image).unwrap() as usize;
        let mut data = image[..size].to_vec();
        let end = 4096 + data[4096..].iter().position(|b| *b == 0).unwrap();
        let json = String::from_utf8(data[4096..end].to_vec()).unwrap();
        assert!(json.contains(from), "{from}");
        let json = json.replace(from, to);
        data[4096..].fill(0);
        data[4096..4096 + json.len()].copy_from_slice(json.as_bytes());
        parse(&data)
    }

    #[test]
    fn crafted_headers() {
        let digest = "\"digest\":\"Qpc2LOdL/X+onSIdH0yhenVyOWQl/wIhuHrLoHUXpMc=\"";
        // An empty digest would match any passphrase's.
        assert_eq!(crafted(digest, "\"digest\":\"\"").err(), Some(Error::Malformed("digest size")));
        assert_eq!(crafted(digest, "\"digest\":\"QQ==\"").err(), Some(Error::Malformed("digest size")));
        // A key size times stripes that would wrap, and other stripes.
        assert!(crafted("\"stripes\":4000", "\"stripes\":6148914691236517206").is_err());
        assert!(crafted("\"stripes\":4000", "\"stripes\":1").is_err());
        assert!(crafted("\"key_size\":64,\"af\"", "\"key_size\":99999999,\"af\"").is_err());
        // Hours of PBKDF2.
        assert!(crafted("\"iterations\":1000,\"salt\":\"Aj29", "\"iterations\":4294967295,\"salt\":\"Aj29").is_err());
        assert!(crafted("\"iterations\":1000,\"salt\":\"vATt", "\"iterations\":0,\"salt\":\"vATt").is_err());
        assert!(crafted("\"iterations\":1000", "\"iterations\":1000").is_ok());
    }

    #[test]
    fn pbkdf2_test_vectors() {
        let mut ours = [0u8; 100];
        pbkdf2("sha256", b"password", b"salt", 4096, &mut ours).unwrap();
        // RFC 7914's test vector for PBKDF2-HMAC-SHA256 (c = 4096), first bytes.
        assert_eq!(&ours[..8], &[0xc5, 0xe4, 0x78, 0xd5, 0x92, 0x88, 0xc8, 0x41]);
        let mut sha512 = [0u8; 64];
        pbkdf2("sha512", b"password", b"salt", 1, &mut sha512).unwrap();
        assert_eq!(&sha512[..4], &[0x86, 0x7f, 0x70, 0xcf]);
    }

    /// Each lane on a thread of its own: the vendored argon2's LaneRunner.
    struct Threads;

    // SAFETY: each lane once, joined before returning.
    unsafe impl argon2::LaneRunner for Threads {
        fn run(&self, lanes: usize, segment: &(dyn Fn(usize) + Sync)) {
            std::thread::scope(|s| {
                for lane in 0..lanes {
                    s.spawn(move || segment(lane));
                }
            });
        }
    }

    #[test]
    fn argon2_lanes_in_parallel() {
        let params = argon2::Params::new(256, 3, 4, Some(32)).unwrap();
        let a = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
        let (mut one, mut threads) = ([0u8; 32], [0u8; 32]);
        a.hash_password_into(b"slingshot", b"saltsaltsaltsalt", &mut one).unwrap();
        let mut memory = alloc::vec![argon2::Block::default(); 256];
        a.hash_password_into_with_memory_and_lanes(b"slingshot", b"saltsaltsaltsalt", &mut threads, &mut memory, &Threads)
            .unwrap();
        assert_eq!(one, threads);
    }

    #[test]
    fn argon2id_volume() {
        check(include_bytes!("../../tests/luks2-argon2id.img"), include_str!("../../tests/luks2-argon2id.key"));
    }

    #[test]
    fn stripes_are_volume_key_sized() {
        let image = include_bytes!("../../tests/luks2-pbkdf2.img");
        let header = parse(&image[..header_size(image).unwrap() as usize]).unwrap();
        // `--integrity hmac-sha256`: a 96-byte volume key, a 64-byte XTS key.
        let slot = Keyslot { key_size: 96, area_key_size: 64, stripes: 4000, ..header.keyslots[0].clone() };
        assert_eq!(slot.material_size(), 96 * 4000);
    }

    #[test]
    fn data_segment() {
        let image = include_bytes!("../../tests/luks2-pbkdf2.img");
        let header = parse(&image[..header_size(image).unwrap() as usize]).unwrap();
        assert_eq!(header.segment, Some(Segment { offset: 1 << 20, size: None, sector_size: 4096, iv_tweak: 0 }));
    }

    /// Sectors encrypted the way dm-crypt does (each sector on its own,
    /// its number as the tweak) read back, from anywhere in the segment.
    #[test]
    fn decrypts_sectors() {
        use aes::cipher::KeyInit;
        let key: Vec<u8> = (0..64u8).collect();
        for sector_size in [512usize, 4096] {
            let plain: Vec<u8> = (0..sector_size * 4).map(|i| (i * 7 % 251) as u8).collect();
            let mut data = plain.clone();
            let xts = xts_mode::Xts128::new(
                aes::Aes256::new_from_slice(&key[..32]).unwrap(),
                aes::Aes256::new_from_slice(&key[32..]).unwrap(),
            );
            for (n, sector) in data.chunks_mut(sector_size).enumerate() {
                xts.encrypt_sector(sector, ((n + 10) as u128).to_le_bytes().into());
            }
            let segment = Segment { offset: 0, size: None, sector_size: sector_size as u32, iv_tweak: 0 };
            let d = Decryptor::new(&segment, &key).unwrap();
            let mut tail = data[sector_size..].to_vec();
            d.decrypt(11 * sector_size as u64, &mut tail);
            assert_eq!(tail, plain[sector_size..]);
            d.decrypt(10 * sector_size as u64, &mut data);
            assert_eq!(data, plain);
        }
    }

    #[test]
    fn not_luks() {
        assert_eq!(header_size(&[0u8; 4096]), Err(Error::NotLuks2));
    }

    #[test]
    fn unlocks_by_itself() {
        let image = include_bytes!("../../tests/luks2-pbkdf2.img");
        let mut header = parse(&image[..header_size(image).unwrap() as usize]).unwrap();
        assert!(!header.unlocks_by_itself());
        // A recovery key is typed, like a passphrase.
        header.tokens = alloc::vec!["systemd-recovery".into()];
        assert!(!header.unlocks_by_itself());
        header.tokens.push("systemd-tpm2".into());
        assert!(header.unlocks_by_itself());
    }
}
