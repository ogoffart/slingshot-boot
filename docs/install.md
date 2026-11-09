# Installing Slingshot

Slingshot goes next to your current boot loader, which keeps working: try it
once before making it the default, and uninstall it at any time.

## On Linux

With [Rust](https://rustup.rs), to build it:

```sh
git clone https://github.com/ogoffart/slingshot-boot
cd slingshot-boot
scripts/slingshot-setup install        # build, copy to the EFI System Partition, add a boot entry
scripts/slingshot-setup try
scripts/slingshot-setup make-default
```

The script shows each command before running it, and asks before changing
anything. `scripts/slingshot-setup help` lists the other commands, including
`undefault` and `uninstall`; `cargo run --release` opens the configuration
window, whose Install screen does the same.

On a PC, it builds with hardware floating point: it draws about twice as
fast, and needs Rust's `rust-src` component, which it adds with rustup.
`--no-sse` builds without it.

To try Slingshot without installing it, the boot menu in a window, with
example entries:

```sh
cargo run --release -- simulate     # or, installed: slingshot simulate
```

or in a virtual machine (QEMU and OVMF; `--aarch64` for 64-bit ARM):

```sh
scripts/run-qemu.sh
```


## systemd-boot and GRUB

With systemd-boot, Slingshot shows its entries (`loader/entries/*.conf` and
unified kernel images in `EFI/Linux/`) and starts the kernels itself:
systemd-boot is no longer needed, but can stay as a fallback. Boot counting
works as with systemd-boot (entries named `name+3.conf`): an update that
fails to start a few times stops being the default, and says so.

With GRUB, the kernels are usually on a partition the firmware can't read,
so Slingshot starts GRUB and its menu. With a file system driver, Slingshot
offers those kernels itself: [drivers.md](drivers.md).

## Encrypted disks

Slingshot asks for an encrypted disk's passphrase itself for entries with a
separate kernel and initrd; a unified kernel image's own initrd asks
instead. With mkinitcpio, `slingshot-setup install` offers to add an entry
with separate files next to the image (later: `slingshot-setup luks-entry`).
Details: [encrypted-disks.md](encrypted-disks.md).

## Secure Boot

Slingshot isn't signed by Microsoft. If you sign your boot files with
`sbctl`, the setup script signs Slingshot too. Behind your distribution's
shim, Slingshot starts its signed kernels and refuses unsigned ones:
[secure-boot.md](secure-boot.md).

## Elsewhere

- Each step by hand, or from a USB stick: [install-manually.md](install-manually.md)
- From Windows: [install-windows.md](install-windows.md)
- Macs (Intel, and Apple silicon with Asahi Linux): [mac.md](mac.md)
