# Working on Slingshot

Notes for developers and coding agents. The README is for users: keep build and development details here.

## Layout

- `src/main.rs`: one crate, two programs (`cfg_select!` picks). Built for `target_os = "uefi"` it's the boot manager
  (`src/firmware/`). Built for a desktop it's `src/desktop/`: `slingshot` (or `slingshot config`) is the same UI
  configuring this computer, and `slingshot simulate` the simulator (winit; `--this-machine` shows this computer's
  entries, read by `machine.rs`). Configuring saves slingshot.toml on the ESP and the boot order (pkexec when needed),
  and the Install screen (`install.rs`) runs the embedded `scripts/slingshot-setup`, first as a dry run.
- `src/app/`: the UI logic, shared by both. It only reaches the machine through the `Backend` trait (`backend.rs`).
  `mod.rs` wires the window's callbacks, `cards.rs` builds the boot cards, `config.rs` is slingshot.toml,
  `settings.rs` the settings saved in a firmware variable.
- `src/parse/`: pure parsers (load options, device paths, Boot Loader Specification entries, SMBIOS, LUKS, HID…) and
  `detect.rs`, what a file system can start. They build on both targets, so `cargo test` runs them on the desktop.
- `src/firmware/`: UEFI only: the Slint platform (GOP, input, event loop), firmware variables, systemd-boot,
  chainloading, system info, USB mice (`usbmouse.rs`), batteries (`battery.rs`; `battery/acpi.rs` runs the firmware's
  ACPI code with the `battery` feature) and I2C touchpads (`touchpad.rs` over `i2c.rs`, with the `touchpad` feature,
  x86 only). Both features are on by default. QEMU has no I2C touchpad: test it on real machines, from the log.
- The log: `debuglog.rs`. F12 on the System screen saves it as `slingshot.log` next to `slingshot.efi`, with the
  machine's details. Log what a bug report from a real machine would need.
- Startup: the menu is drawn first, from what's already connected. Then the platform connects the drivers in slices of
  about 10 ms per turn of the event loop, finds the mice and the touchpad, and the app rescans the disks
  (`Hub::devices_connected`).
- `vendor/`: the `acpi` and `argon2` crates, patched (vendor/README.md).
- `ui/`: the `.slint` files: `main.slint` (the window, its state and the keyboard), one file per screen, `top-bar`,
  `footer`, `dialogs`, `widgets`, and `mock.slint`, sample data so that `slint-viewer ui/main.slint` shows the screens.
- Keyboard focus is Slint's own: each control has a FocusScope, and the arrows a control doesn't use move the focus
  like Tab (`Focus.move`). Dialogs are PopupWindows. Entering a screen focuses its first element (`enter-screen`). In
  Settings, ↑ ↓ go from setting to setting (`Section`'s `step`), and past the ends into the next section.
- `scripts/`: `slingshot-setup` (the installer), `run-qemu.sh`, `build-mouse-drivers.sh`, `test-setup.sh` (tests
  slingshot-setup on a fake machine; `eval "export $(scripts/test-setup.sh --env)"`, then `slingshot --config-dir DIR`
  and F4, tries the Install tab on it).

## Building

```sh
rustup target add x86_64-unknown-uefi
cargo build-uefi      # → target/x86_64-unknown-uefi/release/slingshot.efi
cargo build-uefi-arm  # → target/aarch64-unknown-uefi/release/slingshot.efi
cargo run --release -- simulate   # the simulator; --help for its options
cargo run --release               # slingshot config: configures this computer
```

Two firmware targets, x86_64 and aarch64. They differ in the timer, the debug log's output, the file names
(`parse::EFI_ARCH`) and the battery's ACPI reader (x86 only). CI lints and builds both. Building the desktop program
links libxkbcommon (`libxkbcommon-dev`).

Rust 1.97 or later (`rust-version` in Cargo.toml). CI and the releases use exactly that version: each Rust release adds
Clippy lints, so bump `rust-version` and the workflows together, and fix the new lints.

The `x86_64-unknown-uefi` target has no SSE, so floating point is emulated, and Slint uses floats for every element it
draws. `RUSTC_BOOTSTRAP=1 cargo build-uefi-sse` builds for `targets/x86_64-unknown-uefi-sse.json` instead (SSE2,
core and alloc built from source; needs `rust-src`), about twice as fast. CI and the releases build that one, and
`scripts/run-qemu.sh --sse` runs it.

Slint is pinned to a commit of the `pre-release/1.19` branch, and the `.slint` files use edition 1.19.

The software renderer only has the glyphs embedded at build time (signed distance fields; `SDF_SIZE` in `build.rs`):
the characters of string literals and the font weights that appear as literals. Characters or weights that only
appear in conditional bindings go in the hidden Text at the end of `ui/main.slint`.

## Tests

```sh
scripts/check.sh        # what CI runs: unit tests, the lints of both targets, the setup script
scripts/check.sh --ui   # and the UI tests
```

Run it before pushing. CI fails on Clippy warnings, for both targets; `allow(dead_code)` is only for code one target
doesn't use, with a comment saying which. CI skips pushes that only change Markdown, `docs/` or the wallpapers, and
the .efi files are built by the release workflow.

`ui-tests/` drives the simulator with [Slint UI Testing](https://testing.slint.dev), which needs a license token:

```sh
uv auth login https://testing.slint.dev/simple/ --token <TOKEN>   # once per machine
cd ui-tests && uv run pytest
```

They start a headless Weston when there is no display (or `Xvfb :88 &`, then `DISPLAY=:88 uv run pytest`). Run them
before pushing UI changes.

## QEMU

```sh
sudo pacman -S qemu-desktop edk2-ovmf mtools   # Arch
sudo apt install qemu-system-x86 ovmf mtools   # Debian / Ubuntu
scripts/run-qemu.sh            # --headless, --reset (fresh variables and disks), --battery (needs iasl),
                               # --aarch64 (the `virt` machine with AAVMF; emulated, give it a minute)
```

The script creates demo disks and builds with `--features debugcon`, which logs to `qemu/debug.log` (with
`--aarch64`, to `qemu/serial.log`). With `--headless`, QEMU's monitor is on `qemu/mon.sock` (`sendkey`,
`screendump`). The VM has a `usb-mouse`, which Slingshot reads itself, as OVMF has no mouse driver. File system
drivers (docs/drivers.md, e.g. rEFInd's) go in `qemu/drivers/`. QEMU's FAT folder doesn't see changes made while it
runs: restart it after editing the ESP.

Things a user can see in the firmware need checking in QEMU, not only in the simulator: the software renderer, input
and layout can differ.

### Secure Boot

`src/firmware/secureboot.rs`. To test with shim: OVMF's `OVMF_CODE_4M.secboot.fd` (with `-machine q35,smm=on -global
driver=cfi.pflash01,property=secure,value=on`), a copy of `OVMF_VARS_4M.ms.fd` with a test key in db (`virt-fw-vars
--add-db GUID db.crt --secure-boot`), Ubuntu's signed shim as `EFI/BOOT/BOOTX64.EFI` and Slingshot signed with the
test key (`sbsign`) as `EFI/BOOT/grubx64.efi`. shim refuses a program without a `.sbat` section, and its `verify`
uses the System V calling convention, not "efiapi".

### Encrypted disks (LUKS)

`src/parse/luks.rs` checks passphrases (tested against volumes made by cryptsetup, in `tests/`),
`src/parse/handoff.rs` builds the extra initrd, and `src/firmware/luks.rs` finds the partitions, runs the check on
other processors (MP services; `vendor/argon2`'s `LaneRunner` computes Argon2's lanes in parallel) and offers the
initrds through the LINUX_EFI_INITRD_MEDIA protocol. The simulator's "Simulated Linux (encrypted)" entry uses the
test volume (passphrase "slingshot").

To test the handoff end to end: a disk image with a LUKS2 partition, a dracut initrd with and without
`--install /usr/sbin/cryptsetup` (both handoff paths), and QEMU with `-smp 2 -m 2G`. Without KVM, use a light Argon2
(`cryptsetup luksFormat --pbkdf-memory 65536 --pbkdf-force-iterations 4`).

### Screenshots

The README's pictures come from QEMU, not the simulator (which says it's a simulation), taken with `screendump` on the
monitor socket. The demo machine: two Arch Linux entries in `loader/entries` on `qemu/esp`, a FAT disk with
`EFI/Microsoft/Boot/bootmgfw.efi` attached as NVMe (found as "Windows"), `--battery`, and a slingshot.toml with
`wallpaper = "lake-mystery.jpg"` (`startup = "splash"` for the splash). The demo video adds a LUKS2 disk for an
entry with `rd.luks.name=`, and was filmed with QEMU on Xvfb and `ffmpeg -f x11grab`, the keys sent through the
monitor.

## Translations

The .slint texts use `@tr()`, the Rust ones `tr()` and `trf()` (src/tr.rs). Both read
`translations/<lang>/LC_MESSAGES/slingshot.po`. After adding or changing texts, run `scripts/update-translations.py`:
it updates the .pot and .po files, and writes the translations' letters in the hidden glyph Text at the end of
ui/main.slint. Chinese glyphs come from `ui/fonts/NotoSansSC-subset.ttf` (GB2312's common characters); the script
says when a translation uses a character no font has. The language is slingshot.toml's `language`, else the
firmware's PlatformLang or the desktop's LANG.

## Keyboard layouts

The firmware sends US-layout characters. `src/parse/keymap.rs` maps them with the installed system's layout
(`keymap.txt`, written by `slingshot keymap` from xkbcommon) or a built-in table (`keyboard` in slingshot.toml).
`src/firmware/platform.rs` applies it, dead keys and AltGr included.

## Logos

`assets/os/*.png`, embedded by `src/app/logos.rs` (`FILES`) and decoded at startup (`ui/logos.slint`; not
`@image-url`, which the software renderer embeds as raw pixels). They come from Wikimedia Commons, rendered 120 px
wide (`Special:FilePath/<file>?width=120`): never redraw a logo, and only use freely licensed ones. A new logo goes in
`logos::FILES` and `config::ICONS`, in the same order, and its source and license in `assets/os/SOURCES.md`.

## Wallpapers

`assets/wallpapers/*.jpg` are not in the program: `slingshot-setup install` copies them next to slingshot.toml, and
picks one at random. Featured pictures from Wikimedia Commons, rendered 2560 px wide, freely licensed, with their
authors in `assets/wallpapers/SOURCES.md`. Calm ones with room for the cards, that read in both themes.

## Releases

Bump `version` in Cargo.toml, then push a tag `vX.Y.Z`: `.github/workflows/release.yaml` tests, builds, and drafts a
release with the Linux archive, `slingshot.efi`, `slingshot-aarch64.efi` and `SHA256SUMS`. Publish the draft by hand.

The .efi files are built reproducibly by `scripts/build-efi.sh` (docs/reproducible-builds.md). The release workflow's
`build` job runs the dependencies' code with a read-only token, and `release` only drafts the release and attests the
files' provenance. Actions are pinned by commit hash (the tag in a comment).

## Conventions

- Commit messages explain the why; the README stays user-centric.
