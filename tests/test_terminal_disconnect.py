#!/usr/bin/env python3
"""Native Unix TUI shutdown regression; uses only an isolated tmux server."""
import argparse
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import subprocess
import tempfile
import time
import unittest

BINARY = Path(__file__).resolve().parents[1] / "target/debug/rulescan"


@unittest.skipUnless(os.name == "posix" and shutil.which("tmux"), "requires Unix and tmux")
class TerminalDisconnect(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="rulescan-disconnect-")
        self.base = Path(self.temporary.name).resolve()
        workspace = self.base / "workspace"
        workspace.mkdir()
        (workspace / "AGENTS.md").write_text("Harmless terminal shutdown fixture.\n")
        config = self.base / "config.yaml"
        config.write_text("version: 1\nscan:\n  roots: [" + json.dumps(str(workspace)) + "]\n")
        self.socket = self.base / "tmux.sock"
        self.pid = None
        self.addCleanup(self.clean)
        command = "exec " + shlex.join([str(BINARY), "--state-dir", str(self.base / "private"), "--config", str(config)])
        self.tmux("new-session", "-d", "-s", "demo", "-x", "120", "-y", "30", command)
        self.pid = int(self.tmux("display-message", "-p", "-t", "demo:0.0", "#{pane_pid}"))
        self.wait_screen("Harmless terminal shutdown fixture.")

    def tmux(self, *arguments):
        return subprocess.check_output(["tmux", "-S", str(self.socket), *arguments], text=True, stderr=subprocess.DEVNULL)

    def alive(self):
        if self.pid is None:
            return False
        status = subprocess.run(["ps", "-ww", "-p", str(self.pid), "-o", "stat=,command="], capture_output=True, text=True)
        description = status.stdout.strip()
        return (status.returncode == 0 and not description.startswith("Z")
                and str(BINARY) in description and str(self.base) in description)

    def wait_screen(self, expected):
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            screen = self.tmux("capture-pane", "-p", "-t", "demo:0.0")
            if expected in screen:
                return
            time.sleep(0.05)
        self.fail("TUI did not display " + repr(expected))

    def assert_exits(self):
        deadline = time.monotonic() + 3
        while self.alive() and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertFalse(self.alive(), "TUI remained running after terminal shutdown/termination")

    def clean(self):
        if self.alive():
            os.kill(self.pid, signal.SIGKILL)
        subprocess.run(["tmux", "-S", str(self.socket), "kill-server"], capture_output=True)
        self.temporary.cleanup()

    def test_terminal_eof_does_not_leave_a_spinning_orphan(self):
        self.tmux("kill-server")
        self.assert_exits()

    def test_external_termination_does_not_wait_for_staged_confirmation(self):
        self.tmux("send-keys", "-t", "demo:0.0", "a")
        self.wait_screen("1 staged")
        self.tmux("send-keys", "-t", "demo:0.0", "C-c")
        self.wait_screen("staged decisions have not been applied")
        self.assertTrue(self.alive(), "raw Ctrl-C must retain the staged-action warning")
        os.kill(self.pid, signal.SIGTERM)
        self.assert_exits()
        result = subprocess.check_output([str(BINARY), "--state-dir", str(self.base / "private"),
                                          "findings", "list", "--format", "json"])
        self.assertTrue(all(artifact["state"] == "new" for artifact in json.loads(result)["data"]),
                        "termination must not apply staged decisions")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=BINARY)
    arguments, remaining = parser.parse_known_args()
    BINARY = arguments.binary.resolve()
    unittest.main(argv=[__file__, *remaining])
