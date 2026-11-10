# Copyright © Olivier Goffart
# SPDX-License-Identifier: GPL-3.0-or-later

from slint_testing import AccessibleRole, LogicalPosition, PointerEventButton, keys, wait_until

from conftest import press


def find(window, id):
    return window.query_descendants().match_id(f"SettingsScreen::{id}").find_one()


def button(window, label):
    return window.query_descendants().match_accessible_role(AccessibleRole.Button).match_accessible_label(label).find_one()


def checked(window, picker):
    """The label of the checked option of a picker."""
    options = find(window, picker).query_descendants().match_accessible_role(AccessibleRole.RadioButton).find_all()
    return [o.accessible_label for o in options if o.accessible_checked]


def order(window):
    return [row.accessible_label for row in window.query_descendants().match_id("SettingsScreen::order-row").find_all()]


def open_settings(window):
    press(window, keys.F3)
    tab = window.query_descendants().match_accessible_role(AccessibleRole.Tab).match_accessible_label("Settings").find_one()
    wait_until(lambda: tab.accessible_item_selected)


def open_section(window, name):
    """Shows a section of Settings, as a click in the sidebar does: the focus
    goes to its first item."""
    button(window, name).invoke_accessible_default_action()


def focused_row(window):
    rows = window.query_descendants().match_id("SettingsScreen::order-row").find_all()
    return [row.accessible_label for row in rows if row.accessible_item_selected]


def test_tabs_switch_screens(window):
    tabs = window.query_descendants().match_accessible_role(AccessibleRole.Tab).find_all()
    assert [t.accessible_label for t in tabs] == ["Boot", "System", "Settings"]
    assert tabs[0].accessible_item_selected
    press(window, keys.F2)
    wait_until(lambda: tabs[1].accessible_item_selected)
    tabs[2].single_click(PointerEventButton.Left)
    wait_until(lambda: tabs[2].accessible_item_selected)
    assert not tabs[1].accessible_item_selected


def test_left_right_change_the_theme(window):
    open_settings(window)
    assert checked(window, "theme-picker") == ["Dark"]
    press(window, keys.RightArrow)
    assert checked(window, "theme-picker") == ["Light"]
    # → on the last theme stays there: the accent isn't changed.
    press(window, keys.RightArrow)
    assert checked(window, "theme-picker") == ["Light"]
    assert checked(window, "accent-picker") == ["Blue"]
    press(window, keys.LeftArrow)
    assert checked(window, "theme-picker") == ["Dark"]


def test_accent_stops_at_the_ends(window):
    open_settings(window)
    open_section(window, "Appearance")
    press(window, keys.DownArrow)
    press(window, keys.LeftArrow)
    assert checked(window, "accent-picker") == ["Blue"]
    for _ in range(10):
        press(window, keys.RightArrow)
    assert checked(window, "accent-picker") == ["Orange"]
    assert checked(window, "theme-picker") == ["Dark"]


def test_clicking_an_option_selects_it(window):
    open_settings(window)
    open_section(window, "Startup")
    startup = find(window, "startup-picker")
    option = (
        startup.query_descendants()
        .match_accessible_role(AccessibleRole.RadioButton)
        .match_accessible_label("Splash screen")
        .find_one()
    )
    option.single_click(PointerEventButton.Left)
    assert checked(window, "startup-picker") == ["Splash screen"]
    # The focus follows the click: ← now changes it back.
    press(window, keys.LeftArrow)
    assert checked(window, "startup-picker") == ["Boot menu"]


def test_the_timeout_is_typed(window):
    open_settings(window)
    open_section(window, "Startup")
    # From the startup choice, down past the default entry's.
    press(window, keys.DownArrow)
    press(window, keys.DownArrow)
    timeout = find(window, "timeout-picker")
    for key, value in [("7", "7"), ("5", "75"), (keys.Backspace, "7"), (keys.LeftArrow, "6")]:
        press(window, key)
        assert timeout.accessible_value == value


def test_alt_down_moves_an_entry_and_restore_undoes_it(window):
    open_settings(window)
    open_section(window, "Boot order")
    original = order(window)
    assert focused_row(window) == [original[0]]
    save = button(window, "Save to firmware…")
    restore = button(window, "Restore original order")
    assert not save.accessible_enabled
    assert not restore.accessible_enabled

    press(window, keys.DownArrow, alt=True)
    assert order(window) == [original[1], original[0], *original[2:]]
    # The focus moved with the entry: Alt+↓ again moves it further.
    assert focused_row(window) == [original[0]]
    press(window, keys.DownArrow, alt=True)
    assert order(window) == [original[1], original[2], original[0], *original[3:]]
    assert save.accessible_enabled

    restore.invoke_accessible_default_action()
    assert order(window) == original
    assert not restore.accessible_enabled


def test_page_down_goes_from_section_to_section(window):
    open_settings(window)
    sections = ["Appearance", "Startup", "Language and input", "Boot order"]
    shown = lambda: [s for s in sections if button(window, s).accessible_item_selected]
    assert shown() == ["Appearance"]
    for section in sections[1:]:
        press(window, keys.PageDown)
        assert shown() == [section]
    press(window, keys.PageUp)
    assert shown() == ["Language and input"]
    press(window, keys.PageDown)
    # The boot order's first row has the focus.
    assert focused_row(window) == [order(window)[0]]


def test_tab_reaches_the_buttons_of_a_dialog(window):
    open_settings(window)
    press(window, keys.RightArrow)
    assert checked(window, "theme-picker") == ["Light"]
    button(window, "Reset all settings").invoke_accessible_default_action()
    # Cancel has the focus: Tab goes to Reset, Enter presses it.
    press(window, keys.Tab)
    press(window, keys.Return)
    assert wait_until(lambda: checked(window, "theme-picker") == ["Dark"])


def test_dragging_a_row_moves_it(window):
    open_settings(window)
    open_section(window, "Boot order")
    original = order(window)
    rows = window.query_descendants().match_id("SettingsScreen::order-row").find_all()
    first, third = rows[0].absolute_rect, rows[2].absolute_rect
    x = first.x + first.width / 3
    window.drag_and_drop(
        LogicalPosition(x, first.y + first.height / 2),
        LogicalPosition(x, third.y + third.height / 2),
    )
    assert wait_until(lambda: order(window) == [original[1], original[2], original[0], *original[3:]])


def test_reset_asks_first(window):
    open_settings(window)
    press(window, keys.RightArrow)
    button(window, "Reset all settings").invoke_accessible_default_action()
    button(window, "Cancel").invoke_accessible_default_action()
    assert checked(window, "theme-picker") == ["Light"]

    button(window, "Reset all settings").invoke_accessible_default_action()
    button(window, "Reset").invoke_accessible_default_action()
    assert checked(window, "theme-picker") == ["Dark"]


def test_up_down_go_from_setting_to_setting_across_sections(window):
    """↑ ↓ go to the previous or next setting (Tab goes control by control),
    into the next section after the last one and back."""
    open_settings(window)
    sections = ["Appearance", "Startup", "Language and input", "Boot order"]
    shown = lambda: [s for s in sections if button(window, s).accessible_item_selected]
    # Theme → accent: → changes the accent, not the theme.
    press(window, keys.DownArrow)
    press(window, keys.RightArrow)
    assert checked(window, "accent-picker") == ["Teal"]
    assert checked(window, "theme-picker") == ["Dark"]
    # Wallpaper, the resolution, the animations and their frames per second,
    # then Startup's first setting.
    for _ in range(4):
        press(window, keys.DownArrow)
    assert shown() == ["Appearance"]
    press(window, keys.DownArrow)
    wait_until(lambda: shown() == ["Startup"])
    press(window, keys.RightArrow)
    assert checked(window, "startup-picker") == ["Splash screen"]
    press(window, keys.LeftArrow)
    # ↑ from the first setting: Appearance's last one, then up to the theme.
    press(window, keys.UpArrow)
    wait_until(lambda: shown() == ["Appearance"])
    for _ in range(5):
        press(window, keys.UpArrow)
    press(window, keys.RightArrow)
    assert checked(window, "theme-picker") == ["Light"]
    assert checked(window, "accent-picker") == ["Teal"]
