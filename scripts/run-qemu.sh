#!/bin/sh
# Builds Slingshot and runs it in QEMU with OVMF (UEFI firmware for VMs).
# The boot entries are OVMF's own, plus Slingshot on its ESP (qemu/esp).
#
# Packages:
#   Arch:          qemu-desktop edk2-ovmf (--aarch64: qemu-system-aarch64 edk2-aarch64)
#   Debian/Ubuntu: qemu-system-x86 ovmf (--aarch64: qemu-system-arm qemu-efi-aarch64)
#   Fedora:        qemu-system-x86 edk2-ovmf (--aarch64: qemu-system-aarch64 edk2-aarch64)
#
# Options:
#   --headless   no window (control QEMU through the monitor socket qemu/mon.sock)
#   --reset      start again from fresh firmware variables
#   --battery    add a laptop battery and AC adapter (scripts/qemu-battery.asl;
#                needs iasl: Arch acpica, Debian/Ubuntu acpica-tools)
#   --aarch64    a 64-bit ARM machine (QEMU's `virt`, with AAVMF), emulated
#                unless the host is ARM: slow. Its own disk and variables
#                (qemu/esp-aa64, qemu/vars-aa64.fd); the log is qemu/serial.log.
#   --sse        x86: the build with hardware floating point (cargo
#                build-uefi-sse: needs the rust-src component)
# Environment: OVMF_CODE / OVMF_VARS to use other firmware files.
set -e
cd "$(dirname "$0")/.."

die() { echo "error: $*" >&2; exit 1; }

HEADLESS=0
BATTERY=""
RESET=0
ARCH=x86_64
SSE=0
for arg in "$@"; do
    case "$arg" in
        --headless) HEADLESS=1 ;;
        --battery) BATTERY=1 ;;
        --reset) RESET=1 ;;
        --aarch64) ARCH=aarch64 ;;
        --sse) SSE=1 ;;
        *) die "unknown option $arg" ;;
    esac
done

if [ "$ARCH" = aarch64 ]; then
    QEMU=qemu-system-aarch64
    ESP=qemu/esp-aa64
    VARS=qemu/vars-aa64.fd
    BOOT_FILE=BOOTAA64.EFI
    [ -z "$BATTERY" ] || die "--battery: the battery is only read on x86 PCs"
    command -v $QEMU >/dev/null ||
        die "$QEMU not found (Arch: pacman -S qemu-system-aarch64, Debian/Ubuntu: apt install qemu-system-arm)"
else
    QEMU=qemu-system-x86_64
    ESP=qemu/esp
    VARS=qemu/vars.fd
    BOOT_FILE=BOOTX64.EFI
    command -v $QEMU >/dev/null ||
        die "$QEMU not found (Arch: pacman -S qemu-desktop, Debian/Ubuntu: apt install qemu-system-x86)"
fi
[ "$RESET" = 0 ] || rm -f "$VARS"
TARGET=$ARCH-unknown-uefi

# AAVMF (OVMF for ARM): padded to the 64 MB the `virt` machine's flash has.
if [ "$ARCH" = aarch64 ] && [ -z "$OVMF_CODE" ]; then
    for pair in \
        /usr/share/AAVMF/AAVMF_CODE.fd:/usr/share/AAVMF/AAVMF_VARS.fd \
        /usr/share/edk2/aarch64/QEMU_CODE.fd:/usr/share/edk2/aarch64/QEMU_VARS.fd \
        /usr/share/edk2/aarch64/QEMU_EFI-pflash.raw:/usr/share/edk2/aarch64/vars-template-pflash.raw; do
        code=${pair%%:*}
        vars=${pair#*:}
        if [ -f "$code" ] && [ -f "$vars" ]; then
            OVMF_CODE=$code
            OVMF_VARS=$vars
            break
        fi
    done
    [ -n "$OVMF_CODE" ] ||
        die "AAVMF not found (Arch: pacman -S edk2-aarch64, Debian/Ubuntu: apt install qemu-efi-aarch64), or set OVMF_CODE and OVMF_VARS"
fi

# OVMF: the code (read-only) and a template for the variables, as packaged by
# the distributions (4 MB builds first).
if [ -z "$OVMF_CODE" ]; then
    for pair in \
        /usr/share/edk2/x64/OVMF_CODE.4m.fd:/usr/share/edk2/x64/OVMF_VARS.4m.fd \
        /usr/share/edk2/x64/OVMF_CODE.fd:/usr/share/edk2/x64/OVMF_VARS.fd \
        /usr/share/OVMF/OVMF_CODE_4M.fd:/usr/share/OVMF/OVMF_VARS_4M.fd \
        /usr/share/OVMF/OVMF_CODE.fd:/usr/share/OVMF/OVMF_VARS.fd \
        /usr/share/edk2/ovmf/OVMF_CODE.fd:/usr/share/edk2/ovmf/OVMF_VARS.fd \
        /usr/share/edk2-ovmf/x64/OVMF_CODE.fd:/usr/share/edk2-ovmf/x64/OVMF_VARS.fd \
        /usr/share/qemu/edk2-x86_64-code.fd:/usr/share/qemu/edk2-i386-vars.fd; do
        code=${pair%%:*}
        vars=${pair#*:}
        if [ -f "$code" ] && [ -f "$vars" ]; then
            OVMF_CODE=$code
            OVMF_VARS=$vars
            break
        fi
    done
fi
[ -n "$OVMF_CODE" ] && [ -f "$OVMF_CODE" ] ||
    die "OVMF firmware not found (Arch: pacman -S edk2-ovmf, Debian/Ubuntu: apt install ovmf), or set OVMF_CODE and OVMF_VARS"
[ -n "$OVMF_VARS" ] && [ -f "$OVMF_VARS" ] || die "OVMF_VARS not found (set it next to OVMF_CODE=$OVMF_CODE)"
echo "Firmware: $OVMF_CODE"

# Without the prebuilt target (`rustup target add $TARGET`),
# build core/alloc from source instead (needs the rust-src component).
BUILD_STD=""
if command -v rustup >/dev/null && ! rustup target list --installed 2>/dev/null | grep -q $TARGET; then
    BUILD_STD="-Zbuild-std=core,alloc,compiler_builtins -Zbuild-std-features=compiler-builtins-mem"
    export RUSTC_BOOTSTRAP=1
fi
if [ "$SSE" = 1 ]; then
    [ "$ARCH" = x86_64 ] || die "--sse is for x86"
    RUSTC_BOOTSTRAP=1 cargo build-uefi-sse --features debugcon
    TARGET=x86_64-unknown-uefi-sse
else
    cargo build --release --target $TARGET --features debugcon $BUILD_STD
fi
EFI=${CARGO_TARGET_DIR:-target}/$TARGET/release/slingshot.efi

mkdir -p $ESP/EFI/BOOT
cp "$EFI" $ESP/EFI/BOOT/$BOOT_FILE
# Extra drivers, if built (scripts/build-mouse-drivers.sh): Slingshot loads
# the drivers next to it. The mouse doesn't need them: Slingshot reads USB
# mice itself when the firmware has no driver for them (OVMF has none).
rm -rf $ESP/EFI/BOOT/drivers
if [ "$ARCH" = x86_64 ] && ls qemu/drivers/*.efi >/dev/null 2>&1; then
    mkdir -p $ESP/EFI/BOOT/drivers
    cp qemu/drivers/*.efi $ESP/EFI/BOOT/drivers/
    echo "Drivers:" qemu/drivers/*.efi
fi
# A USB boot-protocol mouse (QEMU's usb-tablet isn't one). It's relative:
# QEMU only sends its movement while it holds the pointer.
POINTER="-device usb-mouse"
echo "Mouse: move the pointer into the QEMU window (or click in it); Ctrl+Alt+G releases it."
[ -f "$VARS" ] || cp "$OVMF_VARS" "$VARS"
chmod u+w "$VARS"

if [ "$HEADLESS" = 1 ]; then
    DISPLAY_OPTS="-display none"
elif [ "$POINTER" = "-device usb-mouse" ] && $QEMU -display help 2>/dev/null | grep -qx gtk; then
    # Grab the mouse as soon as the pointer enters the window.
    DISPLAY_OPTS="-display gtk,grab-on-hover=on"
else
    # Use whatever display backend this QEMU was built with (gtk, sdl, …).
    DISPLAY_OPTS=""
fi

if [ -n "$BATTERY" ]; then
    command -v iasl >/dev/null || die "iasl not found (Arch: pacman -S acpica, Debian/Ubuntu: apt install acpica-tools)"
    iasl -p qemu/battery scripts/qemu-battery.asl >/dev/null
    BATTERY="-acpitable file=qemu/battery.aml"
fi

# KVM when available, much faster.
ACCEL=""
[ -w /dev/kvm ] && [ "$(uname -m)" = "$ARCH" ] && ACCEL="-accel kvm"

if [ "$ARCH" = aarch64 ]; then
    # The firmware's serial console and Slingshot's log share the UART.
    if [ -n "$ACCEL" ]; then CPU=host; else CPU=max; fi
    exec $QEMU -machine virt -cpu $CPU -m 1G -smp 2 $ACCEL \
      -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
      -drive if=pflash,format=raw,file="$VARS" \
      -device qemu-xhci,id=xhci \
      -drive if=none,id=esp,format=raw,file=fat:rw:$ESP -device virtio-blk-pci,drive=esp,bootindex=0 \
      -device usb-kbd $POINTER \
      -device virtio-gpu-pci $DISPLAY_OPTS \
      -monitor unix:qemu/mon.sock,server,nowait \
      -serial file:qemu/serial.log
fi

exec $QEMU -machine q35 -m 1G -smp 2 $ACCEL \
  -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
  -drive if=pflash,format=raw,file="$VARS" \
  -device qemu-xhci,id=xhci \
  -drive if=none,id=esp,format=raw,file=fat:rw:$ESP -device virtio-blk-pci,drive=esp,bootindex=0 \
  -device usb-kbd $POINTER \
  -vga std $DISPLAY_OPTS \
  -monitor unix:qemu/mon.sock,server,nowait \
  -debugcon file:qemu/debug.log -global isa-debugcon.iobase=0x402 \
  -serial file:qemu/serial.log $BATTERY
