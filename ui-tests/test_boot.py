# Copyright © Olivier Goffart
# SPDX-License-Identifier: GPL-3.0-or-later

from slint_testing import keys, wait_until

from conftest import press


def cards(window):
    return window.query_descendants().match_id("BootScreen::card").find_all()


def labels(window):
    return [card.accessible_label for card in cards(window)]


def entries(window):
    """Title and subtitle: two entries can have the same title."""
    return [(card.accessible_label, card.accessible_description) for card in cards(window)]


def focused(window):
    return [card.accessible_label for card in cards(window) if card.accessible_item_selected]


def stay(window):
    """Any key stops the countdown: the simulator would start the default entry."""
    press(window, keys.Escape)


def test_left_right_move_the_focus(window):
    stay(window)
    names = labels(window)
    start = names.index(focused(window)[0])
    press(window, keys.RightArrow)
    assert focused(window) == [names[start + 1]]
    press(window, keys.LeftArrow)
    assert focused(window) == [names[start]]
    # Home and End: the first and the last.
    press(window, keys.End)
    assert focused(window) == [names[-1]]
    press(window, keys.Home)
    assert focused(window) == [names[0]]


def test_more_shows_the_other_entries_and_escape_comes_back(window):
    stay(window)
    main = entries(window)
    press(window, keys.End)
    press(window, keys.Return)
    assert wait_until(lambda: entries(window) != main)
    others = entries(window)
    assert others[0][0] == "Back"
    # The focus on the first entry after "Back".
    assert focused(window) == [others[1][0]]
    assert not set(others[1:]) & set(main[:-1]), "the main row's entries aren't repeated"
    press(window, keys.Escape)
    assert wait_until(lambda: entries(window) == main)
