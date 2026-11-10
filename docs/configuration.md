# Configuring Slingshot

Slingshot reads `slingshot.toml` ([TOML](https://toml.io)) from its own
folder on the EFI System Partition: `EFI/slingshot/slingshot.toml` when
installed with the setup script. Every option is optional; the examples below
show the defaults (commented out when there is none).

Three ways to change it:

- with a text editor;
- at boot: the Settings screen, and "Customize" on a card (C, or a right
  click) for its title, place, default, icon, picture and kernel options;
- on Linux, with `slingshot`: the same screens in a window, with your
  computer's entries, saving to the file (it asks for your password when
  needed).

Slingshot rewrites the file when it saves, without its comments. A value it
doesn't understand is skipped; a file that isn't valid TOML is ignored.

## Settings

```toml
# language = "fr"           # "en", "fr", "de", "zh"; by default the firmware's
keyboard = "us"             # "us", "uk", "fr", "de", "es", "it"
style = "cards"             # "cards", "colorful", "flat", "classic"
theme = "dark"              # "dark", "light"
accent = "blue"             # "blue", "teal", "violet", "pink", "orange", "wallpaper"
timeout = 10                # seconds before the default entry starts; 0: never
startup = "menu"            # "menu" or "splash" (the splash screen)
default = "entry"           # "entry" (the default entry) or "last" (the one started last)
# resolution = "1920x1080"  # by default the firmware's
pointer-speed = "normal"    # "slower", "slow", "normal", "fast", "faster": mice and touchpads
pointer-acceleration = true # fast movements go further
animations = "auto"         # screen transitions: "on", "off", "auto" (off when slower than…)
animations-fps = 20         # … this many frames per second
firmware-setup = true       # the "Firmware setup" entry, and F10
```

These are the Settings screen's defaults. Changes made there at boot are
saved in the firmware (NVRAM) and win over the file, until the file changes
again: the most recent change counts. "Reset all settings" goes back to the
file's values. The style is only set here.

- **Styles:** cards; colorful (each kind of entry on a colored tile); flat
  (icons and titles, no cards); classic (a list, chosen with ↑ ↓).
- **`default = "last"`:** the entry started last is the default.
  systemd-boot's `default @saved` does the same.
- **Hibernated Linux:** when Linux hibernated (systemd 255 and later record
  it), Slingshot resumes it first, after a 3 s countdown that any key stops.

### Keyboard layout

PC firmware types as a US keyboard, whatever the keyboard: on AZERTY, A types
"q". That matters for passphrases.

`slingshot-setup install` writes your system's layout to `keymap.txt` next to
Slingshot: any layout or variant (Dvorak, BÉPO…), dead keys included. It
wins over `keyboard`, which picks a built-in layout. After changing layouts:

```sh
sudo slingshot keymap -o /efi/EFI/slingshot/keymap.txt   # your ESP's path
```

AltGr characters (`@`, `€`…) need firmware that reports the right Alt key.
The language and layout chosen in the Settings screen win over the file;
"Automatic" goes back to it.

## Splash screen

With `startup = "splash"`, Slingshot shows only the entry about to start,
with a countdown. Enter starts it; any other key or a click opens the boot
menu. If it needs an encrypted disk, the passphrase is asked right away
(Cancel: the boot menu; Skip: start, and let Linux ask).

```toml
[splash]
# text = "Welcome back"     # shown instead of the entry's name
# image = "logo.png"        # a PNG or JPEG next to slingshot.toml, instead of the logo (max 560 × 360)
name = true                 # the entry's name, when there's no text
picture = true              # the logo or image; false: a progress bar
hint = true                 # "Press any key for the boot menu"
```

The Settings screen (Startup) sets all of these.

## Wallpaper

```toml
# wallpaper = "lake.jpg"    # a PNG or JPEG next to slingshot.toml; by default none
```

Scaled to cover the screen: use one at least the screen's size. The cards
show it blurred, and `accent = "wallpaper"` takes its main color.
`slingshot-setup install` adds a few pictures ([sources](../assets/wallpapers/SOURCES.md))
and picks one at random when there's none.

## Who may change the settings

```toml
[settings]
access = "shown"            # "shown", "hidden" or "password"
# password = "sha256:…"     # with "password": from `slingshot-setup password`
# editor = true             # editing kernel options (E); default: see below
```

- `"hidden"`: no Settings screen; only the file counts. Also no "Start a
  file…" and no kernel option editing.
- `"password"`: Settings, the date and time, Customize, kernel option editing
  and "Start a file…" ask for the password. Prefer letters and digits: the
  firmware's layout may not be yours.

Kernel options can be edited by default, except with Secure Boot on or `editor no` in
systemd-boot's loader.conf: whoever is at the keyboard could otherwise start a
signed kernel with `init=/bin/sh`.

This keeps casual users out; it isn't security. Anyone who can write the EFI
System Partition can change the file: use Secure Boot and a firmware password
for that.

## Boot entries

Each `[[entry]]` changes the entries `match` names: a title as the menu shows
it, or a systemd-boot entry's file name (`arch.conf`). `*` and `?` are
wildcards, case doesn't matter, and the first matching section applies.

```toml
[[entry]]
match = "arch*.conf"
title = "Arch Linux"        # the card's title
icon = "linux"              # linux, usb, network, shell, disc, app, disk, sliders, or a logo:
                            # windows, macos, tux, arch, fedora, ubuntu, opensuse, debian
image = "arch.png"          # a picture next to slingshot.toml instead (max 56 × 56)
default = true              # Slingshot's default (the firmware's boot order isn't changed)
more = false                # true: under "More…"
options = "root=LABEL=arch rw quiet"   # kernel options, instead of the entry's
```

- **Logo:** without `icon` or `image`, Slingshot recognizes Windows, macOS,
  Ubuntu, Fedora, openSUSE, Arch and Debian (Tux for other Linux).
- **More…:** holds the firmware's generic entries ("EFI USB Device", "EFI
  Network"…, which fail when nothing is plugged in), tools, fallback kernels
  and "Firmware setup". The default entry is always on the main row.
- **Default:** without `default = true`, the first entry of the firmware's
  boot order after Slingshot, or systemd-boot's default when that comes first.
- **Kernel options:** E on a Linux card edits them for one start.

## Entries of your own

An `[[entry]]` without `match` adds a kernel or an EFI program from
Slingshot's partition. Slingshot starts kernels itself, as systemd-boot does.

```toml
[[entry]]
title = "Arch Linux (debug)"
linux = "/vmlinuz-linux"
initrd = ["/intel-ucode.img", "/initramfs-linux.img"]
options = "root=LABEL=arch rw loglevel=7"

[[entry]]
title = "Memtest86+"
efi = "/EFI/tools/memtest.efi"
more = true
```

They take `icon`, `image`, `default` and `more` too.

## Extra drivers

Slingshot loads every `.efi` driver in `EFI/slingshot/drivers/` at startup:
file systems, for example ([drivers](drivers.md)). USB mice need none.
