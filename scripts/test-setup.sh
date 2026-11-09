#!/usr/bin/env bash
# Copyright © Olivier Goffart
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Tests scripts/slingshot-setup against a fake machine: a fake sysfs, ESP and
# firmware, and stand-ins for efibootmgr, findmnt, lsblk, bootctl, sudo and
# systemctl. Nothing on the real system is read or changed.
#
#   scripts/test-setup.sh          run the tests
#   scripts/test-setup.sh --env    print the environment of a fake machine
#                                  (to try the desktop app's Install panel)

set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
SETUP="$ROOT/scripts/slingshot-setup"

# --- the fake machine ----------------------------------------------------------

make_machine() {
    local t=$1
    mkdir -p "$t/sys/firmware/efi/efivars" "$t/sys/class/block/nvme0n1p1" "$t/bin" \
        "$t/esp/EFI/systemd" "$t/esp/loader/entries"
    echo 1 >"$t/sys/class/block/nvme0n1p1/partition"
    # SecureBoot: 4 bytes of attributes, then 0 (off).
    printf '\x06\x00\x00\x00\x00' >"$t/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c"
    touch "$t/esp/EFI/systemd/systemd-bootx64.efi"
    printf 'timeout 3\n' >"$t/esp/loader/loader.conf"
    printf 'slingshot.efi built for the test\n' >"$t/slingshot.efi"
    # The firmware's boot entries: "order=…" then "NNNN title" lines.
    printf 'order=0001,0002\n0001 Linux Boot Manager\n0002 UEFI Shell\n' >"$t/firmware"
    : >"$t/log"

    cat >"$t/bin/efibootmgr" <<'EOF'
#!/usr/bin/env bash
# A stand-in for efibootmgr, on the file $FAKE/firmware.
set -euo pipefail
state=$FAKE/firmware
echo "efibootmgr $*" >>"$FAKE/log"
order() { sed -n 's/^order=//p' "$state"; }
set_order() { sed -i "s/^order=.*/order=$1/" "$state"; }
label="" bootnum="" action=list
while [ $# -gt 0 ]; do
    case "$1" in
        --quiet) ;;
        --create-only) action=create ;;
        --disk|--part|--loader) shift ;;
        --label) label=$2; shift ;;
        --bootnext) action=bootnext; bootnum=$2; shift ;;
        --bootorder) action=bootorder; bootnum=$2; shift ;;
        --delete-bootnum) action=delete ;;
        --bootnum) bootnum=$2; shift ;;
        --delete-bootorder) action=delete-order ;;
        *) echo "fake efibootmgr: unknown $1" >&2; exit 2 ;;
    esac
    shift
done
case $action in
    list)
        printf 'BootOrder: %s\n' "$(order)"
        grep -v '^order=' "$state" | while read -r num title; do printf 'Boot%s* %s\tHD(1,GPT)/File(...)\n' "$num" "$title"; done ;;
    create)
        n=1
        while grep -q "^$(printf %04X $n) " "$state"; do n=$((n + 1)); done
        printf '%04X %s\n' "$n" "$label" >>"$state" ;;
    bootnext) echo "bootnext=$bootnum" >>"$FAKE/log" ;;
    bootorder) set_order "$bootnum" ;;
    delete)
        sed -i "/^$bootnum /d" "$state"
        set_order "$(order | tr , '\n' | { grep -vx "$bootnum" || true; } | paste -sd, -)" ;;
    delete-order) set_order "" ;;
esac
EOF
    cat >"$t/bin/findmnt" <<'EOF'
#!/usr/bin/env bash
# findmnt -no FSTYPE|SOURCE MOUNTPOINT; / and /boot from $FAKE/mounts
# ("MOUNTPOINT FSTYPE" lines).
if [ "$3" != "$FAKE/esp" ]; then
    [ "$2" = FSTYPE ] && grep -q "^$3 " "$FAKE/mounts" 2>/dev/null || exit 1
    sed -n "s|^$3 ||p" "$FAKE/mounts"
    exit 0
fi
case $2 in FSTYPE) echo vfat ;; SOURCE) echo /dev/nvme0n1p1 ;; esac
EOF
    cat >"$t/bin/lsblk" <<'EOF'
#!/usr/bin/env bash
# lsblk -no PKNAME DEV, or -dno UUID DEV ($FAKE/uuids: "DEV UUID" lines),
# or -rno PARTTYPE,MOUNTPOINT / PATH,PARTTYPE ($FAKE/partitions:
# "PATH PARTTYPE MOUNTPOINT" lines).
case $2 in
    PARTTYPE,MOUNTPOINT) awk '{ print $2, $3 }' "$FAKE/partitions" 2>/dev/null; exit 0 ;;
    PATH,PARTTYPE) awk '{ print $1, $2 }' "$FAKE/partitions" 2>/dev/null; exit 0 ;;
esac
if [ "$2" = UUID ]; then
    sed -n "s|^$3 ||p" "$FAKE/uuids" 2>/dev/null
    exit 0
fi
echo nvme0n1
EOF
    cat >"$t/bin/loadkeys" <<'EOF'
#!/usr/bin/env bash
# loadkeys --mktable: $FAKE/mktable, if the test wrote one, for the keymap
# named in $FAKE/keymaps (any, without it).
echo "loadkeys $*" >>"$FAKE/log"
[ ! -f "$FAKE/keymaps" ] || grep -qxF "${@: -1}" "$FAKE/keymaps" || exit 1
[ -f "$FAKE/mktable" ] && cat "$FAKE/mktable"
EOF
    cat >"$t/bin/bootctl" <<'EOF'
#!/usr/bin/env bash
# None on the system when $FAKE/no-bootctl exists (Ubuntu).
[ ! -e "$FAKE/no-bootctl" ] || exit 1
echo "$FAKE/esp"
EOF
    cat >"$t/bin/sudo" <<'EOF'
#!/usr/bin/env bash
[ "$1" = -n ] && shift
# A root-only ESP ($FAKE_LOCKED, mode 000): open for this command only.
if [ -n "${FAKE_LOCKED:-}" ]; then
    chmod 755 "$FAKE_LOCKED"
    status=0
    "$@" || status=$?
    chmod 000 "$FAKE_LOCKED"
    exit $status
fi
exec "$@"
EOF
    cat >"$t/bin/systemctl" <<'EOF'
#!/usr/bin/env bash
echo "systemctl $*" >>"$FAKE/log"
EOF
    chmod +x "$t/bin/"*
}

machine_env() {
    printf 'SLINGSHOT_TESTING=1 FAKE=%s SLINGSHOT_SYSFS=%s/sys SLINGSHOT_ARCH=x86_64 PATH=%s/bin:$PATH' "$1" "$1" "$1"
}

if [ "${1:-}" = --env ]; then
    t=$(mktemp -d)
    make_machine "$t"
    machine_env "$t"
    echo
    exit 0
fi

# --- tests --------------------------------------------------------------------

T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
make_machine "$T"
export SLINGSHOT_TESTING=1 FAKE=$T SLINGSHOT_SYSFS=$T/sys SLINGSHOT_ARCH=x86_64 PATH="$T/bin:$PATH"
# The console's keyboard: French.
mkdir -p "$T/etc"
printf 'KEYMAP=fr-latin9\n' >"$T/etc/vconsole.conf"
printf 'LANG=fr_FR.UTF-8\n' >"$T/etc/locale.conf"
export SLINGSHOT_ETC=$T/etc
# No slingshot program (the source tree's, if built, would read this
# machine's layout): the built-in layouts, but for the --program tests.
export SLINGSHOT_PROGRAM=$T/no-such-program

failures=0
setup() { bash "$SETUP" --yes "$@"; }
status() { setup status --porcelain | sed -n "s/^$1=//p"; }
check() { # check DESCRIPTION ACTUAL EXPECTED
    if [ "$2" = "$3" ]; then
        printf '  ok    %s\n' "$1"
    else
        printf '  FAIL  %s: got "%s", expected "%s"\n' "$1" "$2" "$3"
        failures=$((failures + 1))
    fi
}

echo "slingshot-setup on a fake machine"

check "not installed at first" "$(status installed)/$(status entry)/$(status order)" "0//none"
check "systemd-boot found" "$(status systemd_boot)" 1

setup --dry-run --efi "$T/slingshot.efi" install >/dev/null
check "--dry-run changes nothing" "$(status installed)/$(status entry)" "0/"

setup --efi "$T/slingshot.efi" install >/dev/null
check "install copies the program" "$(cat "$T/esp/EFI/slingshot/slingshot.efi")" "slingshot.efi built for the test"
check "install adds an entry, not in the boot order" "$(status installed)/$(status entry)/$(status order)" "1/0003/none"
check "install copies the wallpapers" "$(cd "$T/esp/EFI/slingshot" && ls ./*.jpg | wc -l)" "$(ls "$ROOT"/assets/wallpapers/*.jpg | wc -l)"

wallpaper=$(sed -n 's/^wallpaper = "\(.*\)"$/\1/p' "$T/esp/EFI/slingshot/slingshot.toml")
check "install makes one of them the wallpaper" "$(ls "$ROOT/assets/wallpapers" | grep -cxF "$wallpaper")" 1
check "install writes the console's keyboard layout and the language" "$(grep -v '^wallpaper' "$T/esp/EFI/slingshot/slingshot.toml")" 'language = "fr"
keyboard = "fr"'

# Without the slingshot program, the console's keymap (as loadkeys compiles
# it): Q types "a", "[" is a dead circumflex, AltGr+E "€".
{
    printf 'unsigned short plain_map[NR_KEYS] = {\n'
    for i in $(seq 0 27); do
        case $i in 16) printf '0xfb61, ' ;; 18) printf '0xfb65, ' ;; 26) printf '0xf402, ' ;; *) printf '0xf200, ' ;; esac
    done
    printf '\n};\nstatic unsigned short altgr_map[NR_KEYS] = {\n'
    for i in $(seq 0 18); do if [ "$i" = 18 ]; then printf '0x20ac,'; else printf '0xf200, '; fi; done
    printf "\n};\nstruct kbdiacruc accent_table[MAX_DIACR] = {\n\t{'^', 'e', 0x00ea},\t{'\\\\'', 'e', 0x00e9},\n};\n"
} >"$T/mktable"
setup --efi "$T/slingshot.efi" install >/dev/null
check "without the slingshot program, the console's keymap" "$(cat "$T/esp/EFI/slingshot/keymap.txt")" "name fr-latin9
key U+0031 - - - -
key U+0032 - - - -
key U+0033 - - - -
key U+0034 - - - -
key U+0035 - - - -
key U+0036 - - - -
key U+0037 - - - -
key U+0038 - - - -
key U+0039 - - - -
key U+0030 - - - -
key U+002D - - - -
key U+003D - - - -
key U+0071 U+0061 - - -
key U+0077 - - - -
key U+0065 U+0065 - U+20AC -
key U+0072 - - - -
key U+0074 - - - -
key U+0079 - - - -
key U+0075 - - - -
key U+0069 - - - -
key U+006F - - - -
key U+0070 - - - -
key U+005B dead:U+005E - - -
key U+005D - - - -
key U+0061 - - - -
key U+0073 - - - -
key U+0064 - - - -
key U+0066 - - - -
key U+0067 - - - -
key U+0068 - - - -
key U+006A - - - -
key U+006B - - - -
key U+006C - - - -
key U+003B - - - -
key U+0027 - - - -
key U+0060 - - - -
key U+005C - - - -
key U+007A - - - -
key U+0078 - - - -
key U+0063 - - - -
key U+0076 - - - -
key U+0062 - - - -
key U+006E - - - -
key U+006D - - - -
key U+002C - - - -
key U+002E - - - -
key U+002F - - - -
compose U+005E U+0065 U+00EA
compose U+00B4 U+0065 U+00E9"

# A desktop without a console keymap: the layout set with localectl, else
# in KDE's settings, then the console keymap for it that kbd has.
mv "$T/etc/vconsole.conf" "$T/vconsole.conf"
mkdir -p "$T/etc/X11/xorg.conf.d" "$T/home/.config"
printf 'Section "InputClass"\n        Option "XkbLayout" "de"\nEndSection\n' >"$T/etc/X11/xorg.conf.d/00-keyboard.conf"
printf 'de\n' >"$T/keymaps"
setup --efi "$T/slingshot.efi" install >/dev/null
check "no console keymap: the desktop's layout (localectl)" "$(head -1 "$T/esp/EFI/slingshot/keymap.txt")/$(grep -c "^loadkeys .* de-latin1$" "$T/log")" "name de/1"
rm "$T/etc/X11/xorg.conf.d/00-keyboard.conf"
printf '[Layout]\nLayoutList=ch,us\nVariantList=fr,\n' >"$T/home/.config/kxkbrc"
printf 'fr_CH\n' >"$T/keymaps"
SLINGSHOT_HOME=$T/home setup --efi "$T/slingshot.efi" install >/dev/null
check "... else KDE's" "$(head -1 "$T/esp/EFI/slingshot/keymap.txt")" "name fr_CH"
rm -f "$T/keymaps" "$T/home/.config/kxkbrc"
mv "$T/vconsole.conf" "$T/etc/vconsole.conf"
rm -f "$T/mktable" "$T/esp/EFI/slingshot/keymap.txt"
check "installing again keeps the one entry" "$(grep -c Slingshot "$T/firmware")" 1
check "and doesn't add the layout twice" "$(grep -c keyboard "$T/esp/EFI/slingshot/slingshot.toml")" 1
check "nor change the wallpaper" "$(grep '^wallpaper' "$T/esp/EFI/slingshot/slingshot.toml")" "wallpaper = \"$wallpaper\""

# A program for another processor is refused (the test machine is x86_64).
printf 'MZ' >"$T/arm.efi"; head -c 58 /dev/zero >>"$T/arm.efi"; printf '\100\0\0\0PE\0\0\144\252' >>"$T/arm.efi"
if setup --efi "$T/arm.efi" install >/dev/null 2>&1; then refused=no; else refused=yes; fi
check "install refuses a program for another processor" "$refused" "yes"
check "... and keeps the installed one" "$(cat "$T/esp/EFI/slingshot/slingshot.efi")" "slingshot.efi built for the test"

printf 'a newer slingshot.efi\n' >"$T/newer.efi"
setup --efi "$T/newer.efi" update >/dev/null
check "update copies the newer program" "$(cat "$T/esp/EFI/slingshot/slingshot.efi")" "a newer slingshot.efi"
check "update keeps the entry and the order" "$(status entry)/$(status order)" "0003/none"

# As a package installs it: the script in /usr/bin, the program in /usr/lib.
mkdir -p "$T/pkg/usr/bin" "$T/pkg/usr/lib/slingshot"
cp "$SETUP" "$T/pkg/usr/bin/slingshot-setup"
printf 'packaged slingshot.efi\n' >"$T/pkg/usr/lib/slingshot/slingshot.efi"
bash "$T/pkg/usr/bin/slingshot-setup" --yes update >/dev/null
check "a packaged script finds the packaged program" "$(cat "$T/esp/EFI/slingshot/slingshot.efi")" "packaged slingshot.efi"

setup --no-restart try >/dev/null
check "try sets BootNext" "$(grep -c '^bootnext=0003$' "$T/log")" 1
check "try --no-restart doesn't restart" "$(grep -c systemctl "$T/log" || true)" 0

setup make-default >/dev/null
check "make-default puts it first" "$(status order)/$(sed -n 's/^order=//p' "$T/firmware")" "first/0003,0001,0002"

setup systemd-boot-entry >/dev/null
check "systemd-boot-entry" "$(status systemd_boot_entry)" 1

setup undefault >/dev/null
check "undefault takes it out of the order" "$(status order)/$(sed -n 's/^order=//p' "$T/firmware")" "none/0001,0002"

setup uninstall >/dev/null
check "uninstall removes everything" "$(status installed)/$(status entry)/$(status systemd_boot_entry)" "0//0"
check "uninstall keeps the other entries" "$(grep -vc '^order=' "$T/firmware")" 2

setup --efi "$T/newer.efi" update >/dev/null
check "update does nothing when not installed" "$(status installed)" 0

# With the slingshot program: the system's keymap, keymap.txt.
cat >"$T/fake-slingshot" <<'EOF'
#!/bin/sh
[ "$1" = keymap ] && [ "$2" = -o ] && printf 'name Fake (test)\nkey q a A - -\n' >"$3"
EOF
chmod +x "$T/fake-slingshot"
setup --efi "$T/slingshot.efi" --program "$T/fake-slingshot" install >/dev/null
check "install writes keymap.txt with the program" "$(head -1 "$T/esp/EFI/slingshot/keymap.txt")" "name Fake (test)"

# The kernels on btrfs: rEFInd's driver, when its package is there.
printf '/ btrfs\n/boot vfat\n' >"$T/mounts"
check "/boot on the EFI System Partition: no driver needed" "$(setup --dry-run --efi "$T/slingshot.efi" install | grep -c "btrfs")" 0
printf '/ btrfs\n' >"$T/mounts"
check "without rEFInd's drivers, install offers EfiFs's" "$(setup --dry-run --efi "$T/slingshot.efi" install | grep -c "download .*/btrfs_x64.efi")" 1
mkdir -p "$T/refind"
echo "btrfs driver" >"$T/refind/btrfs_x64.efi"
SLINGSHOT_REFIND_DRIVERS=$T/refind setup drivers >/dev/null
check "drivers copies rEFInd's driver for the kernels' file system" "$(cat "$T/esp/EFI/slingshot/drivers/btrfs_x64.efi")" "btrfs driver"
check "... once" "$(SLINGSHOT_REFIND_DRIVERS=$T/refind setup drivers | grep -c 'already there')" 1

# The kernels on XFS: rEFInd has no driver, EfiFs's is downloaded, and
# only kept if it's the expected file.
printf '/ xfs\n' >"$T/mounts"
mkdir -p "$T/efifs"
echo "xfs driver" >"$T/efifs/xfs_x64.efi"
printf '%s  xfs_x64.efi\n' "$(echo "another file" | sha256sum | cut -d' ' -f1)" >"$T/efifs.sums"
efifs() { SLINGSHOT_REFIND_DRIVERS=$T/refind SLINGSHOT_EFIFS_URL=file://$T/efifs SLINGSHOT_EFIFS_SUMS=$T/efifs.sums setup "$@" drivers; }
check "drivers refuses an EfiFs driver that isn't the expected file" "$(efifs | grep -c "isn't the expected file")/$(ls "$T/esp/EFI/slingshot/drivers")" "1/btrfs_x64.efi"
(cd "$T/efifs" && sha256sum xfs_x64.efi) >"$T/efifs.sums"
efifs >/dev/null
check "drivers downloads EfiFs's driver when rEFInd has none" "$(cat "$T/esp/EFI/slingshot/drivers/xfs_x64.efi")" "xfs driver"
check "the real sums are there for each file system it offers" "$(SLINGSHOT_ARCH=x86_64 bash -c '. <(sed -n "/^EFIFS_SUMS=/,/^\"$/p" "$1"); printf "%s" "$EFIFS_SUMS"' _ "$SETUP" | grep -c '_x64.efi$')" 12

# --sign: with sbctl, Slingshot and the drivers it adds are signed.
check "--sign without sbctl stops" "$(setup --sign drivers 2>&1 | grep -c 'needs sbctl')" 1
cat >"$T/bin/sbctl" <<'SBCTL'
#!/bin/sh
echo "sbctl $*" >>"$FAKE/log"
SBCTL
chmod +x "$T/bin/sbctl"
rm "$T/esp/EFI/slingshot/drivers/xfs_x64.efi"
efifs --sign >/dev/null
check "--sign signs the driver it adds" "$(grep -c "^sbctl sign -s $T/esp/EFI/slingshot/drivers/xfs_x64.efi$" "$T/log")" 1
check "install --sign signs Slingshot" "$(setup --sign --dry-run --efi "$T/slingshot.efi" install | grep -c "sbctl sign -s $T/esp/EFI/slingshot/slingshot.efi$")" 1
rm "$T/bin/sbctl"
rm -f "$T/mounts"

# An encrypted disk and a unified kernel image only (mkinitcpio). The pacman
# hook has copied the kernel to the ESP; mkinitcpio -p builds the preset's
# initramfs, if it has one.
mkdir -p "$T/etc/mkinitcpio.d" "$T/etc/kernel" "$T/esp/EFI/Linux"
cat >"$T/etc/mkinitcpio.d/linux.preset" <<EOF
ALL_kver="$T/esp/vmlinuz-linux"
PRESETS=('default')
#default_image="/boot/initramfs-linux.img"
default_uki="$T/esp/EFI/Linux/arch-linux.efi"
EOF
cp "$T/etc/mkinitcpio.d/linux.preset" "$T/linux.preset.before"
printf 'HOOKS=(base systemd autodetect modconf kms keyboard sd-vconsole block encrypt filesystems fsck)\n' >"$T/etc/mkinitcpio.conf"
printf '# the root\ncryptdevice=PARTUUID=2bf61a6f-18c7-4bb7-b77c-f44c04cbbc38:root root=/dev/mapper/root\nrw\n' >"$T/etc/kernel/cmdline"
printf 'NAME="Arch Linux"\nPRETTY_NAME="Arch Linux"\n' >"$T/etc/os-release"
touch "$T/esp/vmlinuz-linux" "$T/esp/EFI/Linux/arch-linux.efi" "$T/esp/amd-ucode.img"
cat >"$T/bin/mkinitcpio" <<'EOF'
#!/usr/bin/env bash
echo "mkinitcpio $*" >>"$FAKE/log"
. "$FAKE/etc/mkinitcpio.d/$2.preset"
if [ -n "${default_image:-}" ]; then touch "$default_image"; fi
EOF
chmod +x "$T/bin/mkinitcpio"

offers_entry() { setup --dry-run --efi "$T/slingshot.efi" install | grep -c 'Unified kernel image with an encrypted disk' || true; }
check "install offers the entry" "$(offers_entry)" 1
check "... and says where" "$(bash "$SETUP" --dry-run --efi "$T/slingshot.efi" install <<<n | grep -c "Add a boot entry, $T/esp/loader/entries/arch-linux.conf, for Slingshot" || true)" 1
setup --dry-run luks-entry >/dev/null
check "luks-entry --dry-run changes nothing" "$(ls "$T/esp/loader/entries")/$(grep -c default_image= "$T/etc/mkinitcpio.d/linux.preset")" "/1"

setup luks-entry >/dev/null
check "luks-entry builds the initramfs too" "$(sed -n 's/^default_image=//p' "$T/etc/mkinitcpio.d/linux.preset")" "\"$T/esp/initramfs-linux.img\""
check "... with mkinitcpio" "$(grep -c '^mkinitcpio -p linux$' "$T/log")" 1
check "luks-entry writes the entry" "$(grep -v '^#' "$T/esp/loader/entries/arch-linux.conf")" "title   Arch Linux
linux   /vmlinuz-linux
initrd  /amd-ucode.img
initrd  /initramfs-linux.img
options cryptdevice=PARTUUID=2bf61a6f-18c7-4bb7-b77c-f44c04cbbc38:root root=/dev/mapper/root rw"
check "... and makes it Slingshot's default" "$(tail -3 "$T/esp/EFI/slingshot/slingshot.toml")" '[[entry]]
match = "arch-linux.conf"
default = true'
check "install doesn't offer it any more" "$(offers_entry)" 0

setup luks-entry >/dev/null
check "luks-entry again changes nothing" "$(grep -c default_image= "$T/etc/mkinitcpio.d/linux.preset")/$(grep -c 'default = true' "$T/esp/EFI/slingshot/slingshot.toml")" "2/1"

# With the microcode hook, the initramfs has it already.
rm "$T/esp/loader/entries/arch-linux.conf"
printf 'HOOKS=(base systemd autodetect microcode modconf kms keyboard block encrypt filesystems fsck)\n' >"$T/etc/mkinitcpio.conf"
setup luks-entry >/dev/null
check "no microcode initrd with the microcode hook" "$(grep -c ucode "$T/esp/loader/entries/arch-linux.conf" || true)" 0
check "the initramfs is there: no rebuild" "$(grep -c '^mkinitcpio -p linux$' "$T/log")" 1

# No loader/entries yet (no systemd-boot), and a rule in slingshot.toml that
# matches the entry: the default comes first, as the first match is used.
rm -r "$T/esp/loader/entries"
sed -i '/^\[\[entry\]\]$/,$d' "$T/esp/EFI/slingshot/slingshot.toml"
printf '\n[[entry]]\nmatch = "arch*.conf"\nicon = "linux"\n' >>"$T/esp/EFI/slingshot/slingshot.toml"
setup luks-entry >/dev/null
check "luks-entry makes loader/entries" "$(ls "$T/esp/loader/entries")" "arch-linux.conf"
check "... and puts the default rule first" "$(grep -A2 -m1 '^\[\[entry\]\]' "$T/esp/EFI/slingshot/slingshot.toml")" '[[entry]]
match = "arch-linux.conf"
default = true'
check "... before the other rules" "$(tail -3 "$T/esp/EFI/slingshot/slingshot.toml")" '[[entry]]
match = "arch*.conf"
icon = "linux"'

# An ESP only root can read (fmask=0077, dmask=0077), as Arch often mounts it.
rm "$T/esp/loader/entries/arch-linux.conf"
toml=$(cat "$T/esp/EFI/slingshot/slingshot.toml")
chmod 000 "$T/esp"
export FAKE_LOCKED=$T/esp
setup luks-entry >/dev/null
setup --efi "$T/slingshot.efi" install >/dev/null
unset FAKE_LOCKED
chmod 755 "$T/esp"
check "root-only ESP: luks-entry sees the files" "$(ls "$T/esp/loader/entries")/$(grep -c '^mkinitcpio -p linux$' "$T/log")" "arch-linux.conf/1"
check "... and install adds to slingshot.toml, not over it" "$(cat "$T/esp/EFI/slingshot/slingshot.toml")" "keyboard = \"fr\"
$toml"

# Without an encrypted disk, nothing to offer.
printf 'root=/dev/nvme0n1p2 rw\n' >"$T/etc/kernel/cmdline"
rm "$T/esp/loader/entries/arch-linux.conf"
check "no encrypted disk: install doesn't offer it" "$(offers_entry)" 0

# Uninstall takes back what luks-entry did, and only that.
printf 'title Mine\nlinux /vmlinuz-linux\n' >"$T/esp/loader/entries/mine.conf"
printf 'cryptdevice=PARTUUID=2bf61a6f-18c7-4bb7-b77c-f44c04cbbc38:root rw\n' >"$T/etc/kernel/cmdline"
rm -f "$T/esp/loader/entries/arch-linux.conf"
setup luks-entry >/dev/null
check "(the entry is back for this test)" "$(ls "$T/esp/loader/entries" | paste -sd ' ')" "arch-linux.conf mine.conf"
setup uninstall >/dev/null
check "uninstall removes the entry it wrote, not others" "$(ls "$T/esp/loader/entries")" "mine.conf"
check "... puts the preset back" "$(diff "$T/linux.preset.before" "$T/etc/mkinitcpio.d/linux.preset" && echo same)" "same"
check "... and removes the initramfs" "$(ls "$T/esp/initramfs-linux.img" 2>/dev/null || echo gone)" "gone"

# encrypt → sd-encrypt: the volume key, not the key derived again.
# The LUKS UUID: from udev, as the user can read it (cryptsetup needs root).
printf '/dev/disk/by-partuuid/2bf61a6f-18c7-4bb7-b77c-f44c04cbbc38 0b7e5c0a-8d3f-4b0f-9a52-2f1c6c9d4e11\n' >"$T/uuids"
cat >"$T/bin/cryptsetup" <<'EOF'
#!/usr/bin/env bash
exit 1
EOF
cat >"$T/bin/lsinitcpio" <<'EOF'
#!/usr/bin/env bash
echo usr/bin/systemd-cryptsetup
grep -q 'BINARIES.*cryptsetup' "$FAKE/etc/mkinitcpio.conf" && echo usr/bin/cryptsetup
true
EOF
chmod +x "$T/bin/cryptsetup" "$T/bin/lsinitcpio"
hooks='HOOKS=(base udev autodetect microcode modconf kms keyboard keymap consolefont block encrypt filesystems fsck)'
printf '# Comment\n%s\n' "$hooks" >"$T/etc/mkinitcpio.conf"
printf '# cryptdevice=UUID=old:root\ncryptdevice=PARTUUID=2bf61a6f-18c7-4bb7-b77c-f44c04cbbc38:root:allow-discards root=/dev/mapper/root rw\n' >"$T/etc/kernel/cmdline"
printf 'title Fallback\nlinux /vmlinuz-linux\noptions cryptdevice=PARTUUID=2bf61a6f-18c7-4bb7-b77c-f44c04cbbc38:root:allow-discards rw\n' \
    >"$T/esp/loader/entries/fallback.conf"
mkdir -p "$T/etc/default"
printf 'GRUB_CMDLINE_LINUX="cryptdevice=PARTUUID=2bf61a6f-18c7-4bb7-b77c-f44c04cbbc38:root:allow-discards"\n' >"$T/etc/default/grub"
: >"$T/log"
check "install suggests the switch" "$(setup --dry-run --efi "$T/slingshot.efi" install | grep -c 'luks-entry offers to switch')" 1
setup --dry-run luks-entry >/dev/null
check "--dry-run doesn't switch" "$(grep HOOKS "$T/etc/mkinitcpio.conf")" "$hooks"
setup luks-entry >"$T/out" 2>&1
check "luks-entry switches the hooks" "$(grep HOOKS "$T/etc/mkinitcpio.conf")" \
    "HOOKS=(base systemd autodetect microcode modconf kms keyboard sd-vconsole block sd-encrypt filesystems fsck)"
new='rd.luks.name=0b7e5c0a-8d3f-4b0f-9a52-2f1c6c9d4e11=root rd.luks.options=0b7e5c0a-8d3f-4b0f-9a52-2f1c6c9d4e11=discard'
check "... the kernel options (not a comment)" "$(cat "$T/etc/kernel/cmdline")" "# cryptdevice=UUID=old:root
$new root=/dev/mapper/root rw"
check "... every entry that has cryptdevice=" "$(grep '^options' "$T/esp/loader/entries/fallback.conf")" "options $new rw"
check "... and says to change GRUB's" "$(grep -c 'GRUB has cryptdevice=' "$T/out")" 1
check "... the entry's options" "$(grep '^options' "$T/esp/loader/entries/arch-linux.conf")" "options $new root=/dev/mapper/root rw"
check "... keeps the files as they were" "$(sed -n 2p "$T/etc/mkinitcpio.conf.slingshot-setup.bak")" "$hooks"
check "... builds the initramfs once" "$(grep -c '^mkinitcpio -p linux$' "$T/log")" 1
check "... adds the cryptsetup program" "$(tail -1 "$T/etc/mkinitcpio.conf")/$(grep -c 'has no cryptsetup program' "$T/out" || true)" "BINARIES+=(cryptsetup)/0"
check "install doesn't suggest it any more" "$(setup --dry-run --efi "$T/slingshot.efi" install | grep -c 'offers to switch' || true)" 0

# Already sd-encrypt, without cryptsetup (as sd-encrypt has it): offered.
printf 'HOOKS=(base systemd autodetect block sd-encrypt filesystems)\n' >"$T/etc/mkinitcpio.conf"
: >"$T/log"
setup luks-entry >"$T/out" 2>&1
check "sd-encrypt without cryptsetup: added, and built" "$(tail -1 "$T/etc/mkinitcpio.conf")/$(grep -c '^mkinitcpio -p linux$' "$T/log")" "BINARIES+=(cryptsetup)/1"
setup luks-entry >"$T/out" 2>&1
check "... once" "$(grep -c 'BINARIES+=(cryptsetup)' "$T/etc/mkinitcpio.conf")" 1

# A busybox-only hook: left to the user.
printf '%s\n' "${hooks/block/block netconf dropbear}" >"$T/etc/mkinitcpio.conf"
setup luks-entry >"$T/out" 2>&1
check "a busybox-only hook: no switch" "$(grep -c 'sd-encrypt' "$T/etc/mkinitcpio.conf")/$(grep -c 'see the link to switch' "$T/out")" "0/1"

# No bootctl, and the partition mounted somewhere unusual: found by its type.
touch "$T/no-bootctl"
printf '/dev/sda1 0fc63daf-8483-4772-8e79-3d69d8477de4 /\n/dev/nvme0n1p1 C12A7328-F81F-11D2-BA4B-00A0C93EC93B %s\n' "$T/esp" >"$T/partitions"
check "no bootctl: the partition of the EFI type" "$(setup status --porcelain 2>&1 | sed -n 's/^esp=//p')" "$T/esp"
# Not mounted: said so, with the device.
printf '/dev/nvme0n1p1 c12a7328-f81f-11d2-ba4b-00a0c93ec93b\n' >"$T/partitions"
check "not mounted: says which partition" "$(setup status 2>&1 | grep -c "(/dev/nvme0n1p1) isn't mounted")" 1
rm -f "$T/no-bootctl" "$T/partitions"

if [ "$failures" -gt 0 ]; then
    echo "$failures failure(s)"
    exit 1
fi
echo "all passed"
