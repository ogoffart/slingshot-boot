# Installing Slingshot manually

The steps `scripts/slingshot-setup` runs: Slingshot next to your existing
boot loader, without a USB stick and without changing what boots by
default. The commands are for Linux and need `efibootmgr`.

> [!WARNING]
> The commands show example values: `/boot` for the EFI System Partition,
> `/dev/nvme0n1` partition `1`, boot entry `0003`. Each step says how to find
> yours: replace them before running a command. A wrong disk or entry number
> can change another system's boot entry.

**What Slingshot changes on your machine:** it starts the firmware entry you
pick as the firmware would, so your usual boot loader (systemd-boot, GRUB…)
still boots Linux. When it can't start an entry itself, it sets `BootNext`
and restarts (the firmware clears `BootNext` on the next boot). It also
writes `OsIndications` for "Firmware setup", `BootOrder` only when you save
it in Settings (after a confirmation), and its own settings variable.

## 1. Get slingshot.efi

Build it from the source, with [Rust](https://rustup.rs), on the computer
itself or any Linux machine:

```sh
git clone https://github.com/ogoffart/slingshot-boot && cd slingshot-boot
rustup target add x86_64-unknown-uefi
rustup component add rust-src
RUSTC_BOOTSTRAP=1 cargo build --release \
    --target targets/x86_64-unknown-uefi-sse.json -Zjson-target-spec \
    -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem
# → target/x86_64-unknown-uefi-sse/release/slingshot.efi

# Or, on a stable toolchain without RUSTC_BOOTSTRAP: slower to draw.
#   cargo build --release --target x86_64-unknown-uefi
#   → target/x86_64-unknown-uefi/release/slingshot.efi
# On 64-bit ARM:
#   rustup target add aarch64-unknown-uefi
#   cargo build --release --target aarch64-unknown-uefi
#   → target/aarch64-unknown-uefi/release/slingshot.efi
```

**Why `RUSTC_BOOTSTRAP=1`:** Rust's standard UEFI target has no hardware
floating point (SSE), and the menu's drawing uses floats everywhere, so
the build is for a variant of that target with SSE turned on
(`targets/x86_64-unknown-uefi-sse.json`, `-Zjson-target-spec`). Rust ships no ready-made library
for a custom target: Cargo builds `core` and `alloc` from their source
(`rust-src`), and the options for that (`-Zbuild-std`,
`-Zjson-target-spec`) are still unstable. `RUSTC_BOOTSTRAP=1` lets a stable
toolchain use them; with a nightly toolchain, leave it out and use
`cargo +nightly build …`. The result draws about twice as fast.

## 2. Copy it to the EFI System Partition

The commands below use `/boot`, where Arch mounts the EFI System Partition
(ESP). Yours may be `/efi` or `/boot/efi`; this prints it:

```sh
bootctl --print-esp-path
```

```sh
# ⚠ /boot: replace with your ESP (bootctl --print-esp-path)
sudo mkdir -p /boot/EFI/slingshot
sudo cp target/x86_64-unknown-uefi-sse/release/slingshot.efi /boot/EFI/slingshot/
# Built the other way: target/x86_64-unknown-uefi/release/slingshot.efi
# or, on ARM, target/aarch64-unknown-uefi/release/slingshot.efi instead.
```

The wallpapers, to choose in Settings (optional; any PNG or JPEG next to
`slingshot.efi` works), and one of them as the wallpaper:

```sh
# ⚠ /boot: replace with your ESP
sudo cp assets/wallpapers/*.jpg assets/wallpapers/SOURCES.md /boot/EFI/slingshot/
echo 'wallpaper = "lake-mystery.jpg"' | sudo tee -a /boot/EFI/slingshot/slingshot.toml
```

## 3. Secure Boot

The binary isn't signed: with Secure Boot on, the firmware refuses it (and
falls back to the normal boot). Is it on?

```sh
bootctl status | grep -i "secure boot"
```

If it is, either sign Slingshot with your `sbctl` keys:

```sh
# ⚠ /boot: replace with your ESP
sudo sbctl sign -s /boot/EFI/slingshot/slingshot.efi
```

or put it behind your distribution's shim ([secure-boot.md](secure-boot.md)),
or disable Secure Boot in the firmware setup.

## 4. Add a firmware boot entry, without making it the default

`efibootmgr` needs the ESP's disk and partition number. This prints the
ESP's partition:

```sh
findmnt -no SOURCE /boot
```

`/dev/nvme0n1p1` is disk `/dev/nvme0n1`, partition `1`; `/dev/sda2` would be
disk `/dev/sda`, partition `2`. With yours:

```sh
# ⚠ /dev/nvme0n1 and 1: replace with your ESP's disk and partition (above)
sudo efibootmgr --create-only --disk /dev/nvme0n1 --part 1 \
     --label "Slingshot" --loader '\EFI\slingshot\slingshot.efi'
```

`--create-only` leaves `BootOrder` unchanged. It prints the entries: the new
one is `Boot0003* Slingshot`, say (`efibootmgr` alone shows them again).

## 5. Try it once

With an encrypted disk, read [Encrypted disk (LUKS)](#encrypted-disk-luks)
first.

With the number of Slingshot's entry:

```sh
# ⚠ 0003: replace with Slingshot's entry number (step 4)
sudo efibootmgr --bootnext 0003
systemctl reboot
```

Pick your usual boot loader ("Linux Boot Manager" for systemd-boot,
"GRUB"…) to boot Linux. The next restart goes back to normal whatever
happens: if anything goes wrong, turn the computer off and on. Some
firmware's one-time boot menu also lists entries that aren't in
`BootOrder`.

## 6. Make it the default (optional)

`efibootmgr` shows the current order, say `BootOrder: 0001,0000`. Put
Slingshot's number first, then the same order:

```sh
# ⚠ replace with Slingshot's number, then your current BootOrder
sudo efibootmgr --bootorder 0003,0001,0000
```

Set the auto-boot timeout in Slingshot's Settings. It starts your previous
boot loader (the first entry after Slingshot) by default.

## With systemd-boot

Slingshot reads systemd-boot's entries (`/boot/loader/entries/*.conf` and
unified kernel images in `/boot/EFI/Linux/`) and starts the kernels itself,
with their initrds and options. Only when a kernel can't be loaded does it go
through systemd-boot (setting `LoaderEntryOneShot`), if installed.

So once Slingshot is the default, systemd-boot is optional: keep it as a
fallback, or remove it with `bootctl remove` (the entries stay). When the EFI
System Partition has no entries, Slingshot reads them from an XBOOTLDR
partition (or Fedora's `/boot`) on the same disk, if the firmware can read
it (ext4 needs a [driver](drivers.md)).

## Encrypted disk (LUKS)

If Linux is on a LUKS2 disk, Slingshot can ask for the passphrase itself and
hand the opened disk to Linux, which then doesn't ask again. Read
[encrypted-disks.md](encrypted-disks.md) before step 5: it works for entries
with a separate kernel and initrd (a unified kernel image needs an extra
entry), and the initrd needs the `cryptsetup` program, or Linux checks the
passphrase a second time.

Slingshot types the passphrase with a US keyboard layout unless it knows
yours. The setup script writes it as `keymap.txt`; by hand, build the desktop
program (it needs `libxkbcommon`) and run its `keymap` command:

```sh
cargo build --release
# ⚠ /boot: replace with your ESP
sudo target/release/slingshot keymap -o /boot/EFI/slingshot/keymap.txt
```

Or name one of the built-in layouts, `keyboard = "fr"`, in `slingshot.toml`
next to `slingshot.efi` ([configuration.md](configuration.md#keyboard-layout)).

## Alternative: start it from systemd-boot

Instead of steps 4–6, add a systemd-boot menu entry (no firmware variables
change):

```sh
printf 'title Slingshot\nefi /EFI/slingshot/slingshot.efi\n' | \
    sudo tee /boot/loader/entries/slingshot.conf   # ⚠ /boot: your ESP
```

## Removing it

First, in Slingshot, use Settings → "Reset all settings": its settings
variable can't be seen from Linux. Then:

With the number of Slingshot's entry (`efibootmgr` shows it):

```sh
# ⚠ 0003: Slingshot's entry number; /boot: your ESP
sudo efibootmgr --delete-bootnum --bootnum 0003
sudo rm -rf /boot/EFI/slingshot /boot/loader/entries/slingshot.conf
```

## From a USB stick

Copy the built `slingshot.efi` to a FAT32 USB stick as `EFI/BOOT/BOOTX64.EFI` (ARM:
`BOOTAA64.EFI`) and start it from the firmware's boot menu. Secure Boot must
be off, or the binary signed (step 3).
