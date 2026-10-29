#!/usr/bin/env python3
"""Unix-only, isolated native watcher and PTY review smoke. Installs no service."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import pty
import selectors
import signal
import sqlite3
import struct
import subprocess
import tempfile
import termios
import time

parser = argparse.ArgumentParser()
parser.add_argument("--binary", type=Path, default=Path("target/debug/rulescan"))
args = parser.parse_args()
binary = args.binary.resolve()

with tempfile.TemporaryDirectory(prefix="rulescan-smoke-") as temporary:
    base = Path(temporary).resolve()
    root = base / "workspace"
    root.mkdir()
    state = base / "private"
    prefix = [str(binary), "--state-dir", str(state)]
    watcher = subprocess.Popen(prefix + ["watch", "--foreground", "--root", str(root), "--format", "jsonl"],
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    events = []
    selector = selectors.DefaultSelector()
    selector.register(watcher.stdout, selectors.EVENT_READ)

    def wait_event(predicate, timeout=15):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if selector.select(timeout=0.2):
                line = watcher.stdout.readline()
                if not line:
                    raise RuntimeError("watcher stopped: " + watcher.stderr.read().decode(errors="replace"))
                event = json.loads(line)
                events.append(event)
                if predicate(event):
                    return event
        raise TimeoutError("watcher event did not arrive")

    try:
        started = wait_event(lambda event: event["kind"] == "started")
        assert started["completion"] == "completed", started
        assert not started["health"]["polling_fallbacks"], started
        (root / "AGENTS.md").write_text("Harmless native watcher fixture.\n")
        changed = wait_event(lambda event: any(change["state"] == "new" for change in event["changes"]))
        assert changed["trigger"] == "native_event", changed
        root.rename(base / "old-workspace")
        root.mkdir()
        (root / "CLAUDE.md").write_text("Harmless replacement root.\n")
        replaced = wait_event(lambda event: any(change["path"] and change["path"]["display"].endswith("CLAUDE.md") for change in event["changes"]))
        assert replaced["completion"] == "completed", replaced
    finally:
        watcher.send_signal(signal.SIGINT)
        watcher.communicate(timeout=10)
        selector.close()
    assert watcher.returncode == 0, watcher.returncode
    assert all(a["sequence"] < b["sequence"] for a, b in zip(events, events[1:])), events

    # A different state layout keeps the PTY test independent of watcher inventory.
    tui_state = base / "tui-private"
    config = base / "tui.yaml"
    config.write_text("version: 1\nscan:\n  roots: [" + json.dumps(str(root)) + "]\n")
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
    before = termios.tcgetattr(slave)
    process = subprocess.Popen([str(binary), "--state-dir", str(tui_state), "--config", str(config)],
                               stdin=slave, stdout=slave, stderr=slave,
                               env={**os.environ, "TERM": "xterm-256color", "NO_COLOR": "1"})
    captured = bytearray()
    ready = selectors.DefaultSelector()
    ready.register(master, selectors.EVENT_READ)

    def pump(seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if ready.select(timeout=0.05):
                chunk=os.read(master,65536)
                captured.extend(chunk)
                if b"\x1b[6n" in chunk:
                    os.write(master,b"\x1b[1;1R")

    try:
        deadline = time.monotonic() + 10
        scanned=False
        database=tui_state / "state/rulescan.sqlite3"
        while not scanned and time.monotonic() < deadline:
            pump(0.1)
            if database.exists():
                try:
                    with sqlite3.connect(database.as_uri()+"?mode=ro",uri=True) as connection:
                        scanned=connection.execute("SELECT COUNT(*) FROM scans").fetchone()[0]>0
                except sqlite3.OperationalError:
                    pass
        assert scanned and b"rulescan" in captured.lower(), "TUI did not finish scanning: " + repr(bytes(captured[:500]))
        pump(0.5)  # Permit the asynchronous safe preview to arrive.
        os.write(master, b"a")
        pump(0.2)
        os.write(master, b"c")
        pump(0.2)
        os.write(master, b"\r")
        pump(1.5)
        os.write(master, b"q")
        pump(0.3)
        process.wait(timeout=10)
        assert process.returncode == 0, "TUI did not exit normally"
        # macOS sets transient PENDIN status when restoring cooked input. Exercise
        # real canonical buffering, which settles that status, before comparing
        # every saved terminal setting without masking any configuration bits.
        cooked = selectors.DefaultSelector()
        try:
            cooked.register(slave, selectors.EVENT_READ)
            probe = b"rulescan-terminal-restoration"
            os.write(master, probe)
            assert not cooked.select(timeout=0.05), "terminal input is not canonical"
            os.write(master, b"\n")
            assert cooked.select(timeout=1), "cooked terminal input did not arrive"
            assert os.read(slave, len(probe) + 2) == probe + b"\n", "cooked input changed"
        finally:
            cooked.close()
        after = termios.tcgetattr(slave)
        assert before == after, "terminal settings were not restored"
        checked = subprocess.run([str(binary), "--state-dir", str(tui_state), "check", "--workspace", str(root), "--format", "json"],
                                 capture_output=True, check=False)
        assert checked.returncode == 0, checked.stdout.decode(errors="replace")
        data = json.loads(checked.stdout)
        assert data["data"][0]["state"] == "approved", data
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
        ready.close()
        os.close(master)
        os.close(slave)

    print(json.dumps({"watch_native_change": "passed", "watch_root_replacement": "passed",
                      "watch_clean_shutdown": "passed", "watch_sequence": "passed",
                      "tui_80x24_review_apply": "passed", "terminal_restoration": "passed",
                      "service_installed": False}, indent=2))
