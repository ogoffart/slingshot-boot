# Copyright © Olivier Goffart
# SPDX-License-Identifier: GPL-3.0-or-later

# Turning off when idle, in a simulator that does it after 20 s.

import os

import pytest
import slint_testing
from slint_testing import keys, wait_until

from conftest import press


@pytest.fixture
def window(simulator, display_env):
    env = dict(os.environ, **display_env)
    with slint_testing.Application([str(simulator), "simulate", "--power-off-after", "20"], env=env) as aut:
        yield aut.first_window


def shown(window, start):
    return any(e.accessible_label.startswith(start) for e in window.query_descendants().find_all())


def test_idle_counts_down_then_turns_off(window):
    # A key: no countdown to the default entry, the idle time starts.
    press(window, keys.DownArrow)
    # The last 15 s are counted down.
    assert wait_until(lambda: shown(window, "Turning off in"), timeout=12)
    if os.environ.get("SLINGSHOT_SHOT"):
        with open(os.environ["SLINGSHOT_SHOT"], "wb") as f:
            f.write(window.grab_window_as_png())
    # A key stays on, and starts again. Not acting on what's behind: Enter
    # doesn't start the entry under the warning.
    press(window, keys.Return)
    assert wait_until(lambda: not shown(window, "Turning off in"))
    assert not shown(window, "Simulator: would")
    assert wait_until(lambda: shown(window, "Shutting down"), timeout=30)
