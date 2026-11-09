# Installing Slingshot from Windows

Untested so far. These steps add Slingshot next to Windows Boot Manager
without making it the default, like the Linux setup script.

**Before you start:**

- **Secure Boot** must be off in the firmware setup: Slingshot isn't signed
  by Microsoft, so the firmware won't start it otherwise.
- **BitLocker:** changing Secure Boot or the boot order can make Windows ask
  for the recovery key at the next start. Have it at hand first (in your
  Microsoft account under "Devices", or wherever you saved it).

You need `slingshot.efi`, built on any computer with Rust:
[the manual install](install-manually.md#1-get-slingshotefi) has the commands.

## 1. Copy Slingshot to the EFI System Partition

In a Command Prompt run as administrator:

```bat
mountvol S: /S
mkdir S:\EFI\slingshot
copy slingshot.efi S:\EFI\slingshot\
```

Put `slingshot.toml` (and a splash image) in `S:\EFI\slingshot\` too, if
you want. Then unmount the partition:

```bat
mountvol S: /D
```

## 2. Add a firmware boot entry

```bat
bcdedit /copy {bootmgr} /d "Slingshot"
```

This prints the new entry's identifier, such as
`{01234567-89ab-cdef-0123-456789abcdef}`. Use it below:

```bat
bcdedit /set {01234567-89ab-cdef-0123-456789abcdef} path \EFI\slingshot\slingshot.efi
```

## 3. Try it once

```bat
bcdedit /set {fwbootmgr} bootsequence {01234567-89ab-cdef-0123-456789abcdef}
```

The next restart starts Slingshot; the one after is back to normal.

## 4. Make it the default (optional)

```bat
bcdedit /set {fwbootmgr} displayorder {01234567-89ab-cdef-0123-456789abcdef} /addfirst
```

To start Windows, pick Windows Boot Manager in Slingshot.

## Removing it

```bat
bcdedit /delete {01234567-89ab-cdef-0123-456789abcdef}
mountvol S: /S
rmdir /s /q S:\EFI\slingshot
mountvol S: /D
```
