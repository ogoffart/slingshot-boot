#!/bin/sh
# Builds the released slingshot.efi files, reproducibly: the same source,
# Rust version and Cargo.lock give the same bytes, whoever builds them and
# wherever (docs/reproducible-builds.md).
#
#   scripts/build-efi.sh          → dist/slingshot.efi, dist/slingshot-aarch64.efi
#   scripts/build-efi.sh --check  → also prints their SHA-256
#
# What would differ otherwise: the paths compiled in (the source tree's,
# Cargo's and the Rust library's, in panic messages), mapped to fixed names,
# the PE header's time stamp, which /Brepro makes a hash of the contents,
# and the debug directory: a PDB's identifier, which depends on where the
# objects were (/DEBUG:NONE: no PDB; the firmware has no use for one). Both
# also in .cargo/config.toml, which RUSTFLAGS overrides.
set -eu
cd "$(dirname "$0")/.."

rustc_version=$(rustc --version)
wanted=$(sed -n 's/^rust-version = "\(.*\)"/\1/p' Cargo.toml)
case "$rustc_version" in
    "rustc $wanted "* | "rustc $wanted.0 "*) ;;
    *) echo "warning: $rustc_version, Cargo.toml says $wanted: the bytes depend on the compiler" >&2 ;;
esac

sysroot=$(rustc --print sysroot)
cargo_home=${CARGO_HOME:-$HOME/.cargo}
remap="--remap-path-prefix=$PWD=/slingshot --remap-path-prefix=$cargo_home=/cargo --remap-path-prefix=$sysroot=/rust"
export RUSTFLAGS="${RUSTFLAGS:-} $remap -C link-arg=/Brepro -C link-arg=/DEBUG:NONE"
# -Zbuild-std (the x86 build with hardware floating point) on a stable toolchain.
export RUSTC_BOOTSTRAP=1
# Nothing that depends on the time it was built, should anything look.
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || echo 0)}

cargo build-uefi-sse --locked
# ARM: core built from source too, as for x86, rather than rustup's prebuilt
# one, so that whether that one is installed doesn't change the bytes.
cargo build --release --locked --target aarch64-unknown-uefi \
    -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem

mkdir -p dist
cp target/x86_64-unknown-uefi-sse/release/slingshot.efi dist/slingshot.efi
cp target/aarch64-unknown-uefi/release/slingshot.efi dist/slingshot-aarch64.efi
if [ "${1:-}" = --check ]; then
    (cd dist && sha256sum slingshot.efi slingshot-aarch64.efi)
fi
