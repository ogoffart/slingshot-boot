# Reproducible builds

The released `slingshot.efi` and `slingshot-aarch64.efi` can be rebuilt from
the source byte for byte: anyone can check that a release (or a signed copy,
signatures removed) matches its source, without trusting the build machine.

## Checking a release

With the release's tag, the Rust version in `Cargo.toml` (`rust-version`)
and the `rust-src` component:

```sh
git checkout vX.Y.Z
rustup toolchain install 1.97 --component rust-src --target x86_64-unknown-uefi,aarch64-unknown-uefi
rustup override set 1.97
scripts/build-efi.sh --check
```

The SHA-256 it prints must match the release's `SHA256SUMS`.

For a signed copy, `sbattach --remove` gives the unsigned file back.

GitHub also attests that the release files were built by the release
workflow from the tag: `gh attestation verify slingshot.efi --repo
ogoffart/slingshot-boot`.

## What makes it reproducible

- **Same inputs:** `Cargo.lock` (`--locked`; it pins the patched crates'
  commits), Slint pinned to a commit, and the Rust version Cargo.toml
  names (another version gives other bytes: the script warns).
- **No build machine paths** (panic messages carry source file names):
  `scripts/build-efi.sh` maps the source tree to `/slingshot`, Cargo's home to
  `/cargo` and the Rust installation to `/rust` (`--remap-path-prefix`).
- **No time:** the PE header's time stamp is a hash of the contents
  (`/Brepro`, in `.cargo/config.toml` and the script).
- **No debug record:** no PDB, nor the record pointing to one, whose
  identifier depends on where the objects were built (`/DEBUG:NONE`).

Checked by building in two different folders: the same SHA-256.
