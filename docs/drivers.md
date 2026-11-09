# File system drivers

Firmware reads only FAT (the EFI System Partition, USB sticks). With file
system drivers, Slingshot also reads Linux partitions (ext4, btrfs…):

- Boot Loader Specification entries on a `/boot` partition (Fedora's
  `loader/entries/`, on ext4) become cards, and their kernels start.
- Kernels in a root file system's `/boot` or on a `/boot` partition are
  found (under "More…") with their initramfs and command line: from
  `/etc/kernel/cmdline`, the GRUB menu (`grub.cfg`) or the Boot Loader
  Specification entries, else `root=` the partition.
- "Start a file…" browses those partitions too, and the
  [encrypted ones](encrypted-disks.md) once unlocked.

Slingshot loads every EFI driver in the `drivers` folder next to it at
startup:

```
EFI/slingshot/slingshot.efi
EFI/slingshot/drivers/ext4_x64.efi
```

## Where to get them

[rEFInd](https://www.rodsbooks.com/refind/)'s drivers work: ext4 (also ext2
and ext3), btrfs, ReiserFS, HFS+ and ISO 9660.

- Arch: the `refind` package, in `/usr/share/refind/drivers_x64/`.
- Debian, Ubuntu: the `refind` package, in
  `/usr/share/refind/refind/drivers_x64/` (you can say no when it asks to
  install rEFInd).
- Anywhere: the "binary zip file" from rEFInd's download page, in
  `refind/drivers_x64/` (`drivers_aa64/` for 64-bit ARM).

[EfiFs](https://github.com/pbatard/efifs) has more: XFS, F2FS, ZFS, JFS,
NTFS, exFAT, UDF, and the ones above.

`slingshot-setup install` adds the driver for the file system of `/` and
`/boot`: rEFInd's when its package is installed (no need to set rEFInd up),
else EfiFs's, downloaded from its release and checked against the SHA-256
sums in the script. It signs it with `sbctl` when Secure Boot is on, or with
`--sign`. `slingshot-setup drivers` does it later.

By hand (each driver makes the start a little slower), with `/boot` being
where your EFI System Partition is mounted (`bootctl --print-esp-path` says):

```sh
# ⚠ /boot: replace with your ESP
sudo mkdir -p /boot/EFI/slingshot/drivers
sudo cp /usr/share/refind/drivers_x64/ext4_x64.efi /boot/EFI/slingshot/drivers/
```

[EfiFs](https://github.com/pbatard/efifs) has drivers for more file systems
(NTFS, XFS, ZFS…), untested with Slingshot.

To remove a driver, delete it from the folder.

## Secure Boot

With Secure Boot on, the firmware doesn't load a driver that isn't signed
with a key it trusts (Slingshot runs without it). If you sign your boot
files with `sbctl`, sign the drivers too:

```sh
# ⚠ /boot: replace with your ESP
sudo sbctl sign -s /boot/EFI/slingshot/drivers/ext4_x64.efi
```

## Other drivers

The folder takes any EFI driver, such as a mouse driver (rarely needed:
Slingshot has its own USB mouse driver).

A broken driver can stop the firmware. If Slingshot doesn't start any more,
start another system from the firmware's boot menu and remove the driver.
