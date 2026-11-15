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
    # From the list: New…, Rename, Copy and Delete (Paste, Revert and
    # Save are disabled), then the editor.
    for _ in range(5):
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


def accept_dialog(window, label):
    """Clicks the dialog's `label` button: the last one, after the screen's own."""
    assert wait_until(lambda: len(buttons(window, label)) == 2)
    buttons(window, label)[-1].single_click(PointerEventButton.Left)
    assert wait_until(lambda: len(buttons(window, label)) == 1)


def make(window, name, folder=False):
    """Ctrl+N: a new file (the dialog's first choice) or folder."""
    press(window, "n", ctrl=True)
    type_text(window, name)
    if folder:
        window.query_descendants().match_accessible_role(AccessibleRole.RadioButton).match_accessible_label(
            "Folder"
        ).find_one().single_click(PointerEventButton.Left)
    buttons(window, "Make")[0].single_click(PointerEventButton.Left)
    assert wait_until(lambda: selected(window) == [name])


def test_a_new_folder_is_made_where_the_row_is(window):
    open_files_tab(window)
    make(window, "Backup", folder=True)
    assert "EFI" in names(window), "in the disk, beside its folders"


def test_a_new_file_opens_in_the_editor(window):
    open_files_tab(window)
    make(window, "notes.txt")
    assert wait_until(lambda: editor(window).accessible_value == "")
    editor(window).single_click(PointerEventButton.Left)
    type_text(window, "hello")
    buttons(window, "Save")[0].single_click(PointerEventButton.Left)
    accept_dialog(window, "Save")
    # Back to the list, then the file opened again.
    press(window, keys.Escape)
    go(window, "/notes.txt")
    press(window, keys.Return)
    assert wait_until(lambda: editor(window).accessible_value == "hello")


def test_paste_into_an_empty_folder(window):
    """Paste puts the file in the folder selected (in a file's folder for a file)."""
    open_files_tab(window)
    make(window, "Backup", folder=True)
    go(window, "/loader/loader.conf")
    press(window, "c", ctrl=True)
    go(window, "/Backup")
    press(window, "v", ctrl=True)
    assert wait_until(lambda: selected(window) == ["loader.conf"])
    # In loader, and now in Backup.
    assert names(window).count("loader.conf") == 2


def menu_item(window, label):
    """The context menu's entry `label` (not the button of the same name)."""
    found = [e for e in labels(window, label) if e.accessible_role != AccessibleRole.Button]
    return found[0] if found else None


def test_right_click_selects_the_row_and_shows_its_menu(window):
    open_files_tab(window)
    press(window, keys.RightArrow)
    assert wait_until(lambda: "loader" in names(window))
    row = next(r for r in rows(window) if r.accessible_label == "loader")
    row.single_click(PointerEventButton.Right)
    assert wait_until(lambda: selected(window) == ["loader"])
    assert wait_until(lambda: menu_item(window, "Rename"))
    menu_item(window, "Rename").single_click(PointerEventButton.Left)
    # The rename dialog, for that row.
    assert wait_until(lambda: labels(window, "Rename loader"))


def test_a_file_is_copied_into_another_folder(window):
    open_files_tab(window)
    go(window, "/loader/loader.conf")
    press(window, "c", ctrl=True)
    go(window, "/EFI")
    press(window, "v", ctrl=True)
    assert wait_until(lambda: selected(window) == ["loader.conf"])
    assert names(window).count("loader.conf") == 2
    press(window, keys.Return)
    assert wait_until(lambda: "timeout 3" in editor(window).accessible_value)
    # Pasted again: it's already there.
    press(window, "v", ctrl=True)
    assert wait_until(lambda: labels(window, "loader.conf is already in this folder."))


def test_a_file_is_renamed(window):
    open_files_tab(window)
    go(window, "/loader/loader.conf")
    press(window, "r", ctrl=True)
    # The name is selected: typing replaces it.
    type_text(window, "old.conf")
    press(window, keys.Return)
    assert wait_until(lambda: selected(window) == ["old.conf"])
    assert "loader.conf" not in names(window)
    press(window, keys.Return)
    assert wait_until(lambda: "timeout 3" in editor(window).accessible_value)


def test_delete_asks_and_keeps_folders_with_files(window):
    open_files_tab(window)
    go(window, "/loader/loader.conf")
    press(window, keys.Delete)
    accept_dialog(window, "Delete")
    assert wait_until(lambda: "loader.conf" not in names(window))
    assert selected(window) == ["loader"], "the folder, now empty"
    # A folder with files stays.
    go(window, "/EFI")
    press(window, keys.Delete)
    accept_dialog(window, "Delete")
    assert wait_until(lambda: labels(window, "Could not delete EFI: the folder isn't empty"))
    assert "EFI" in names(window)


def test_a_long_text_opens_in_the_editor(window):
    """3000 lines: taller than 32767 px, which Slint's software renderer
    couldn't lay out (an overflow, a panic in this debug build)."""
    open_files_tab(window)
    open_text(window, "/EFI/tools/changelog.txt", "1. A change")
    assert "3000. A change" in editor(window).accessible_value
    press(window, keys.End, ctrl=True)
    type_text(window, "#")
