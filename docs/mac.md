# Slingshot on a Mac

Untested on real Macs so far: reports welcome. Your Mac's own startup picker
stays available, so you can't lock yourself out: hold **Option** at startup
on an Intel Mac, or the **power button** on an Apple silicon Mac.

## Intel Macs (2008 to 2020)

Slingshot's usual x86 build runs on their EFI firmware. It starts macOS,
Linux and Windows, and can replace rEFInd or the Option-key picker.

### Build

Build it on any computer with [Rust](https://rustup.rs), macOS included:

```sh
rustup target add x86_64-unknown-uefi
rustup component add rust-src
RUSTC_BOOTSTRAP=1 cargo build --release \
    --target targets/x86_64-unknown-uefi-sse.json -Zjson-target-spec \
    -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem
# → target/x86_64-unknown-uefi-sse/release/slingshot.efi
```

### Try it once

1. Mount the EFI System Partition (usually `disk0s1`; see `diskutil list`):

   ```sh
   sudo diskutil mount disk0s1          # → /Volumes/EFI
   ```

2. Copy Slingshot to the path Macs start when a disk has no other boot
   loader:

   ```sh
   sudo mkdir -p /Volumes/EFI/EFI/BOOT
   sudo cp target/x86_64-unknown-uefi-sse/release/slingshot.efi /Volumes/EFI/EFI/BOOT/BOOTX64.EFI
   ```

   If `EFI/BOOT/BOOTX64.EFI` already exists (rEFInd, Linux or Windows), don't
   overwrite it: use Slingshot's own folder, as in "Start it on every boot"
   below.

3. Restart holding **Option**, and choose **EFI Boot**.

**Macs with a T2 chip (2018 to 2020)** only start Apple's and Microsoft's
signed programs at first. In Recovery (hold Command-R at startup), open
Utilities > Startup Security Utility, set Secure Boot to **No Security**,
and allow starting from external media for a USB stick.

### Start it on every boot

Put Slingshot in its own folder and *bless* it (tell the firmware to start
it):

```sh
sudo mkdir -p /Volumes/EFI/EFI/slingshot
sudo cp target/x86_64-unknown-uefi-sse/release/slingshot.efi /Volumes/EFI/EFI/slingshot/
sudo bless --mount /Volumes/EFI --setBoot --file /Volumes/EFI/EFI/slingshot/slingshot.efi --shortform
```

With System Integrity Protection on (OS X 10.11 and later), `bless`
refuses: run it from the Terminal in Recovery instead, after mounting the
ESP there (`diskutil mount disk0s1`).

To go back, choose your macOS disk in System Settings > General > Startup
Disk, or:

```sh
sudo bless --mount / --setBoot
```

Then delete `/Volumes/EFI/EFI/slingshot`.

### Starting macOS

Slingshot finds macOS itself (`System/Library/CoreServices/boot.efi`, on the
**Preboot** volume from macOS 11 on) and shows it with its version, such as
"macOS 14.5". This needs firmware that reads APFS: Macs that got macOS 10.13
or later do.

### What doesn't work on a Mac

- **No battery card:** Macs report a Smart Battery (ACPI0002), which
  Slingshot doesn't read.
- **The trackpad** of MacBooks may not work: use the keyboard (USB mice
  work).
- **Macs from 2006 and 2007** have 32-bit EFI: not supported.

## Apple silicon Macs (M1 and later)

These have no UEFI firmware: Slingshot can't replace the startup options or
start macOS. Hold the power button at startup to choose macOS or Linux.

With **[Asahi Linux](https://asahilinux.org)**, Linux boots through U-Boot,
which provides UEFI and starts `EFI/BOOT/BOOTAA64.EFI` from Asahi's EFI
System Partition. Slingshot's 64-bit ARM build can run there as the menu for
your Linux kernels (systemd-boot entries, unified kernel images, or GRUB).

### Build

Build it under Asahi Linux or on any machine with Rust:

```sh
rustup target add aarch64-unknown-uefi
cargo build --release --target aarch64-unknown-uefi   # → target/aarch64-unknown-uefi/release/slingshot.efi
```

### Install

U-Boot doesn't keep firmware boot entries, so `scripts/slingshot-setup`
(which uses `efibootmgr`) doesn't work here. Replace the file U-Boot starts,
keeping the original. The commands use `/boot/efi`, where Fedora Asahi Remix
mounts the EFI System Partition (`findmnt -t vfat` shows yours):

```sh
# ⚠ /boot/efi: replace with your ESP
sudo mkdir -p /boot/efi/EFI/slingshot
sudo cp /boot/efi/EFI/BOOT/BOOTAA64.EFI /boot/efi/EFI/slingshot/previous-bootaa64.efi
sudo cp target/aarch64-unknown-uefi/release/slingshot.efi /boot/efi/EFI/BOOT/BOOTAA64.EFI
```

Slingshot then shows your distribution's boot loader or kernels; **Start a
file…** can start `EFI/slingshot/previous-bootaa64.efi`. To undo, copy that
file back to `EFI/BOOT/BOOTAA64.EFI`. If Linux doesn't start any more,
choose macOS at startup (hold the power button) and fix the file from there:
the ESP is the small FAT partition `diskutil list` shows.

### What to expect

- Keyboard only: the trackpad isn't a USB device.
- U-Boot may not keep Slingshot's settings and boot order across restarts;
  defaults in `slingshot.toml` (next to `slingshot.efi`) still apply.
- One display mode, the screen's own resolution.
- No battery card.

## Other ARM computers

The `aarch64` build runs on other 64-bit ARM machines with UEFI firmware:
Windows on ARM laptops, ARM servers, a Raspberry Pi with UEFI firmware,
virtual machines (`scripts/run-qemu.sh --aarch64`). No battery card there
yet (x86 only).
