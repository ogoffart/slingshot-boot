# Copyright © Olivier Goffart
# SPDX-License-Identifier: GPL-3.0-or-later

# The Files tab, on the simulator's disks: the ESP (writable, with
# loader.conf), "MY STICK" (a picture, an EFI program).

from slint_testing import AccessibleRole, PointerEventButton, keys, wait_until

from conftest import press
from test_files import type_text


def rows(window):
    return window.query_descendants().match_id("FilesScreen::tree-row").find_all()


def names(window):
    return [row.accessible_label for row in rows(window)]


def selected(window):
    return [row.accessible_label for row in rows(window) if row.accessible_item_selected]


def buttons(window, label):
    return window.query_descendants().match_accessible_role(AccessibleRole.Button).match_accessible_label(label).find_all()


def editor(window):
    return window.query_descendants().match_id("FilesScreen::input").find_one()


def open_files_tab(window):
    press(window, keys.F2)
    tab = window.query_descendants().match_accessible_role(AccessibleRole.Tab).match_accessible_label("Files").find_one()
    assert wait_until(lambda: tab.accessible_item_selected)
    assert wait_until(lambda: names(window)[:2] == ["EFI System Partition", "MY STICK"])


def go(window, path):
    """Types `path` in the location field: its file is selected."""
    type_text(window, path)
    press(window, keys.Return)
    assert wait_until(lambda: selected(window) == [path.rsplit("/", 1)[-1]])


def open_text(window, path, contains):
    go(window, path)
    press(window, keys.Return)
    assert wait_until(lambda: contains in editor(window).accessible_value)


def test_right_and_left_expand_and_collapse_a_disk(window):
    open_files_tab(window)
    assert selected(window) == ["EFI System Partition"]
    press(window, keys.RightArrow)
    assert wait_until(lambda: "loader" in names(window))
    press(window, keys.LeftArrow)
    assert wait_until(lambda: "loader" not in names(window))
    assert selected(window) == ["EFI System Partition"]


def test_a_text_is_edited_and_saved(window):
    open_files_tab(window)
    open_text(window, "/loader/loader.conf", "timeout 3")
    assert not buttons(window, "Save")[0].accessible_enabled, "nothing to save yet"
    editor(window).single_click(PointerEventButton.Left)
    type_text(window, "#")
    assert wait_until(lambda: buttons(window, "Save")[0].accessible_enabled)
    # Save, then the confirmation's own Save.
    buttons(window, "Save")[0].single_click(PointerEventButton.Left)
    assert wait_until(lambda: len(buttons(window, "Save")) == 2)
    buttons(window, "Save")[-1].single_click(PointerEventButton.Left)
    assert wait_until(lambda: len(buttons(window, "Save")) == 1 and not buttons(window, "Save")[0].accessible_enabled)
    # Opened again: the simulator keeps it for the session.
    edited = editor(window).accessible_value
    assert "#" in edited
    open_text(window, "/loader/loader.conf", edited)


def test_tab_into_the_editor_keeps_the_text(window):
    """TextInput selects all its text when Tab focuses it: typing would
    replace the whole file."""
    open_files_tab(window)
    open_text(window, "/loader/loader.conf", "timeout 3")
    press(window, keys.Tab)
    type_text(window, "#")
    assert wait_until(lambda: "#" in editor(window).accessible_value)
    assert "timeout 3" in editor(window).accessible_value


def labels(window, text):
    return window.query_descendants().match_accessible_label(text).find_all()


def test_ctrl_f_finds_in_the_text(window):
    open_files_tab(window)
    open_text(window, "/loader/loader.conf", "timeout 3")
    press(window, "f", ctrl=True)
    type_text(window, "o")
    assert wait_until(lambda: labels(window, "1 of 7"))
    press(window, keys.Return)
    assert wait_until(lambda: labels(window, "2 of 7"))
    # The query grows: the match shown stays if it still matches.
    type_text(window, "ns")
    assert wait_until(lambda: labels(window, "1 of 1"))
    type_text(window, "z")
    assert wait_until(lambda: labels(window, "Not found"))
    press(window, keys.Backspace)
    assert wait_until(lambda: labels(window, "1 of 1"))
    # Escape goes back to the text, with the match selected: typing replaces it.
    press(window, keys.Escape)
    type_text(window, "#")
    assert wait_until(lambda: "c#ole-mode" in editor(window).accessible_value)


def test_opening_another_file_asks_about_the_changes(window):
    open_files_tab(window)
    open_text(window, "/loader/loader.conf", "timeout 3")
    editor(window).single_click(PointerEventButton.Left)
    type_text(window, "#")
    press(window, keys.Escape)
    go(window, "/EFI/BOOT/BOOTX64.EFI")
    press(window, keys.Return)
    assert wait_until(lambda: buttons(window, "Discard"))
    # Cancel keeps the text and its changes.
    buttons(window, "Cancel")[0].single_click(PointerEventButton.Left)
    assert wait_until(lambda: not buttons(window, "Discard"))
    assert "#" in editor(window).accessible_value
    press(window, keys.Return)
    assert wait_until(lambda: buttons(window, "Discard"))
    buttons(window, "Discard")[0].single_click(PointerEventButton.Left)
    assert wait_until(lambda: buttons(window, "Start"))


def test_a_picture_and_a_program_get_their_buttons(window):
    open_files_tab(window)
    press(window, keys.DownArrow)
    assert selected(window) == ["MY STICK"]
    go(window, "/Pictures/lake.jpg")
    press(window, keys.Return)
    assert wait_until(lambda: buttons(window, "Use as wallpaper") and buttons(window, "Use as splash"))
    go(window, "/EFI/BOOT/BOOTX64.EFI")
    press(window, keys.Return)
    assert wait_until(lambda: buttons(window, "Start") and buttons(window, "Add to boot menu"))
    assert not buttons(window, "Use as wallpaper")
