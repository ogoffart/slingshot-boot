# Copyright © Olivier Goffart
# SPDX-License-Identifier: GPL-3.0-or-later

import os
import shutil
import subprocess
import time
from pathlib import Path

import pytest
import slint_testing
from slint_testing import KeyPressedEvent, KeyReleasedEvent, keys

ROOT = Path(__file__).resolve().parent.parent
# Its own target directory: the debug info and the feature would otherwise
# rebuild the normal simulator each time.
TARGET = ROOT / "target" / "ui-tests"


@pytest.fixture(scope="session")
def simulator() -> Path:
    """The simulator, built with system testing support."""
    env = dict(os.environ, SLINT_EMIT_DEBUG_INFO="1")
    subprocess.run(
        ["cargo", "build", "--features", "system-testing", "--target-dir", str(TARGET)],
        cwd=ROOT,
        env=env,
        check=True,
    )
    return TARGET / "debug" / "slingshot"


@pytest.fixture(scope="session")
def display_env(tmp_path_factory):
    """The environment for the simulator: a headless Weston when there is no display."""
    if os.environ.get("WAYLAND_DISPLAY") or os.environ.get("DISPLAY") or not shutil.which("weston"):
        yield {}
        return
    runtime_dir = tmp_path_factory.mktemp("wayland")
    runtime_dir.chmod(0o700)
    weston = subprocess.Popen(
        ["weston", "--backend=headless", "--socket=wayland-0", "--width=1600", "--height=1000"],
        env=dict(os.environ, XDG_RUNTIME_DIR=str(runtime_dir)),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    socket = runtime_dir / "wayland-0"
    deadline = time.monotonic() + 10
    while not socket.exists():
        if time.monotonic() > deadline or weston.poll() is not None:
            weston.kill()
            pytest.fail("headless weston did not start")
        time.sleep(0.05)
    # An absolute path: the client needs no XDG_RUNTIME_DIR.
    yield {"WAYLAND_DISPLAY": str(socket)}
    weston.terminate()
    weston.wait()


@pytest.fixture
def window(simulator, display_env):
    """The simulator's window, in a fresh simulator for each test."""
    env = dict(os.environ, **display_env)
    with slint_testing.Application([str(simulator), "simulate"], env=env) as aut:
        yield aut.first_window


def press(window, key: str, alt: bool = False, shift: bool = False):
    """Presses and releases `key`, with Alt or Shift held down."""
    held = [k for k, down in ((keys.Alt, alt), (keys.Shift, shift)) if down]
    for k in held:
        window.dispatch_event(KeyPressedEvent(k))
    window.dispatch_event(KeyPressedEvent(key))
    window.dispatch_event(KeyReleasedEvent(key))
    for k in reversed(held):
        window.dispatch_event(KeyReleasedEvent(k))
