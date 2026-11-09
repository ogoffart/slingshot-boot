# Copyright © Olivier Goffart
# SPDX-License-Identifier: GPL-3.0-or-later

from slint_testing import PointerEventButton, keys, wait_until

from conftest import press
from test_boot import labels, stay


def rows(window):
    return window.query_descendants().match_id("FileDialog::file-row").find_all()


def names(window):
    return [row.accessible_label for row in rows(window)]


def selected(window):
    return [row.accessible_label for row in rows(window) if row.accessible_item_selected]


def open_files(window):
    """"Start a file…": the last entry under "More…" (the simulator has no
    password on its settings)."""
    stay(window)
    main = labels(window)
    press(window, keys.End)
    press(window, keys.Return)
    assert wait_until(lambda: labels(window) != main)
    press(window, keys.End)
    press(window, keys.Return)
    # Then the partitions the firmware can't read.
    assert wait_until(lambda: names(window)[:2] == ["EFI System Partition", "MY STICK"])


def type_text(window, text):
    for c in text:
        press(window, c)


def test_typing_a_name_selects_it(window):
    open_files(window)
    assert selected(window) == ["EFI System Partition"]
    press(window, "m")
    assert selected(window) == ["MY STICK"]
    # No name starts with "me": "e" alone, from the next row on.
    press(window, "e")
    assert selected(window) == ["EFI System Partition"]


def test_a_typed_path_opens_its_folder(window):
    open_files(window)
    # "/" starts a path; case doesn't matter, as on the disks.
    type_text(window, "/efi/microsoft/boot")
    press(window, keys.Return)
    assert wait_until(lambda: "bootmgfw.efi" in names(window))
    # The file itself: opened, and selected.
    type_text(window, "/EFI/BOOT/bootx64.efi")
    press(window, keys.Return)
    assert wait_until(lambda: selected(window) == ["BOOTX64.EFI"])


def test_a_click_selects_a_row(window):
    open_files(window)
    rows(window)[1].single_click(PointerEventButton.Left)
    assert wait_until(lambda: selected(window) == ["MY STICK"])
    # The keys go on from there.
    press(window, keys.UpArrow)
    assert selected(window) == ["EFI System Partition"]
