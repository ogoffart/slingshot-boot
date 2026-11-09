# Copyright © Olivier Goffart
# SPDX-License-Identifier: GPL-3.0-or-later

from slint_testing import (
    keys,
    LogicalPosition,
    PointerEventButton,
    PointerPressEvent,
    PointerReleaseEvent,
    wait_until,
)

from conftest import press


def open_customize(window):
    """Customize on the first card, a Linux one (it has the kernel options
    field): the focus starts on the firmware's default, which may not be."""
    press(window, keys.Home)
    press(window, "c")
    title = window.query_descendants().match_id("CustomizeDialog::title")
    wait_until(lambda: title.find_one().accessible_label)


def test_a_click_in_a_field_places_the_cursor(window):
    open_customize(window)
    field = window.query_descendants().match_id("CustomizeDialog::options-input").find_one()
    field.single_click(PointerEventButton.Left)
    for ch in "abc":
        press(window, ch)
    # A click at the start of the text.
    rect = field.absolute_rect
    start = LogicalPosition(rect.x + 1, rect.y + rect.height / 2)
    window.dispatch_event(PointerPressEvent(start, PointerEventButton.Left))
    window.dispatch_event(PointerReleaseEvent(start, PointerEventButton.Left))
    press(window, "X")
    value = field.query_descendants().match_id("TextField::input").find_one().accessible_value
    assert value == "Xabc"
