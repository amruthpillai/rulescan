# Screenshots

These are window captures of the real `rulescan` 0.1.0 release binary running its
keyboard dashboard in Ghostty on macOS. Nothing was drawn, mocked or edited after
capture.

| File | Shows |
| --- | --- |
| `dashboard.png` | Findings list with one `changed` and three `new` items, plus the approved-baseline diff for `AGENTS.md` |
| `review.png` | Review dialog with two approvals and one skill-bundle quarantine staged |
| `quarantine.png` | Quarantine view after applying the plan, with the recoverable skill bundle and restore guidance |

## How they were made

Run from the repository root (macOS; needs `tmux`, `/Applications/Ghostty.app` and
Screen Recording permission for `screencapture`):

```sh
cargo build --release
python3 scripts/capture-screenshots.py --binary target/release/rulescan --out docs/screenshots
```

What the script does:

1. It creates `/private/tmp/rulescan-demo` (mode 700, with an ownership marker) holding
   a synthetic workspace, a private state directory and a config that lists only that
   workspace in `scan.roots`. Explicit roots mean no home-directory agent folders are
   scanned. If the directory already exists without the marker, the script stops.
2. It approves `AGENTS.md`, `CLAUDE.md` and `.github/copilot-instructions.md` through
   the machine CLI (`plan approve --expected-sha256 …` then `apply`). Then it edits
   `AGENTS.md` and adds `GEMINI.md`, a Cursor rule and a Claude skill. All content is
   harmless text, and the URLs use the reserved `.invalid` domain.
3. It starts the TUI in a private tmux server (`tmux -S <demo>/tmux.sock`, status bar
   off) at 120×30 and sends keys with `tmux send-keys`. Before each step it checks the
   pane text, for example that the diff has loaded or that 3 decisions are staged.
4. It opens a separate Ghostty instance with
   `open -na Ghostty.app --args --config-default-files=false --config-file=<demo>/ghostty.conf`
   (Catppuccin Mocha theme, 14 pt). This instance attaches to the tmux session and
   ignores the user's Ghostty config. A small Swift helper finds that instance's
   CGWindowID, and `screencapture -x -o -l<id>` captures only that window.
5. When it finishes, it quits the TUI, stops that TUI process, the private tmux server
   and the Ghostty instance it started, and deletes `/private/tmp/rulescan-demo`. It
   touches no other window or process.

`--text-only` skips Ghostty and prints the same screens as plain text from tmux.
Hashes and quarantine IDs change from run to run.
