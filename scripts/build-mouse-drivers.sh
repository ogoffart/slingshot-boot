#!/bin/sh
# Builds EDK II's USB mouse drivers (UsbMouseDxe, UsbMouseAbsolutePointerDxe)
# into qemu/drivers/, to try Slingshot with the firmware's own mouse
# drivers: OVMF has none, and Slingshot otherwise reads USB mice itself. It
# loads the drivers in a `drivers` folder next to itself, and
# scripts/run-qemu.sh installs these ones.
#
# Needs: git, gcc, make, nasm, python3, uuid headers
#   Arch:          pacman -S base-devel nasm python util-linux-libs
#   Debian/Ubuntu: apt install build-essential nasm python3 uuid-dev
set -e
missing=
for tool in git gcc make nasm python3; do
    command -v "$tool" >/dev/null 2>&1 || missing="$missing $tool"
done
if [ -n "$missing" ]; then
    echo "Missing:$missing" >&2
    echo "  Arch:          sudo pacman -S --needed base-devel nasm python util-linux-libs" >&2
    echo "  Debian/Ubuntu: sudo apt install build-essential nasm python3 uuid-dev" >&2
    exit 1
fi
cd "$(dirname "$0")/.."
OUT=$PWD/qemu/drivers
SRC=${EDK2_DIR:-$PWD/qemu/edk2}

if [ ! -d "$SRC" ]; then
    git clone --depth 1 https://github.com/tianocore/edk2 "$SRC"
fi
# Only the submodules these drivers need.
git -C "$SRC" submodule update --init --depth 1 \
    BaseTools/Source/C/BrotliCompress/brotli \
    MdeModulePkg/Library/BrotliCustomDecompressLib/brotli \
    MdePkg/Library/MipiSysTLib/mipisyst
cd "$SRC"
make -C BaseTools -j"$(nproc)"
export WORKSPACE="$SRC" PACKAGES_PATH="$SRC"
# edksetup.sh reads the positional parameters: don't pass ours.
set --
# shellcheck disable=SC1091
. ./edksetup.sh
mkdir -p "$OUT"
for m in UsbMouseAbsolutePointerDxe UsbMouseDxe; do
    build -a X64 -t GCC -b RELEASE -p MdeModulePkg/MdeModulePkg.dsc \
        -m "MdeModulePkg/Bus/Usb/$m/$m.inf"
    cp "Build/MdeModule/RELEASE_GCC/X64/$m.efi" "$OUT/"
done
echo "Built: $(ls "$OUT")"
echo "scripts/run-qemu.sh installs them next to Slingshot."
