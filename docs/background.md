# Background monitoring

RuleScan can watch your configured roots and record when agent instructions
appear or change. The watcher **detects changes after they are written**. It is
not enforcement: an agent may already have read a file before RuleScan notices
it. A monitor that runs as your user is not a security boundary against other
programs running as the same user.

The watcher only observes. It never approves, quarantines, or restores anything.
Review with `rulescan`, or use `plan` and `apply`.

## Foreground watch

```sh
rulescan watch --foreground                     # roots from config, else --root
rulescan watch --foreground --root ~/work/repo
rulescan watch --foreground --once              # reconcile once, exit with the scan exit code
rulescan --format jsonl watch --foreground      # one JSON event per line on stdout
```

What happens:

1. Takes `state/monitor.lock`. Only one watcher can run per state directory.
   This lock is separate from the coordinator lock that scans and actions use.
2. Runs `recover` to settle interrupted quarantine or restore operations.
3. Runs a full reconciliation. Every candidate is rehashed through the normal
   scan. Coverage is reported as current only after this step.
4. Watches each root recursively with native events (FSEvents, inotify, or
   ReadDirectoryChangesW through `notify`, symlinks not followed). It also
   watches each root's parent without recursion, so deleting, renaming, or
   replacing the root is noticed.
5. Treats each event as a hint. Events for a root are debounced (default
   `watch.debounce: 500ms`) and then the whole root is rescanned. Constant
   activity cannot delay a rescan forever: after `max(10 × debounce, 5s)` the
   root is rescanned anyway.
6. Runs a full reconciliation every `watch.reconcile_every` (default `6h`).

Events inside RuleScan's own state, data, and cache directories are dropped, so
RuleScan's own writes never trigger rescans.

### Degraded coverage

- The event queue holds 1024 events. If it overflows, or the OS reports that
  events were lost, every root is marked for a full rescan.
- If a native watch cannot be created for a root (for example, the watch limit
  is reached or the filesystem does not support events), that root falls back to
  a full rescan every `watch.poll_every` (default `30s`). The fallback is listed
  in health as `polling_fallbacks` and the event kind is `degraded`.
- If a scan is blocked by another RuleScan operation, the root stays queued and
  is retried.

Ctrl-C or SIGTERM stops an idle watcher within about 250 ms. A scan already in
progress stops at its next cancellation check.

## Events (JSONL)

Get the schema with `rulescan schema --kind event`. Each line is one
`WatchEvent`:

| Field | Meaning |
| --- | --- |
| `schema_version` | Event format version (currently 1) |
| `sequence` | Persistent, increasing cursor. It is shared with the audit event log and survives restarts. |
| `kind` | `started`, `reconciled`, `degraded`, `stopped` |
| `trigger` | `startup`, `native_event`, `overflow`, `periodic`, `poll_fallback`, `cancel` |
| `completion` | `completed` only when coverage was complete with no errors |
| `coverage`, `profile_version`, `policy_version` | Same meaning as in scan output |
| `changes` | Artifact observations since the previous event: kind, id, SHA-256, escaped path, category, state. At most 500 per event. |
| `health` | The same snapshot saved in the `monitor_health` setting |
| `exit_code` | Set on `--once` (scan exit code) and on `stopped` |

Events never contain file contents. Paths and error text are escaped and
error text is length-limited. A change is reported once per artifact and digest.
Repeated saves of the same bytes do not create new findings. Quiet rescans with
the same status refresh the heartbeat but emit no event.

## Health

The watcher saves `monitor_health` in the database at startup, around each
reconciliation, and every 15 seconds. The record holds: pid, version,
executable, status (`starting`, `reconciling`, `watching`, `degraded`,
`stopped`), heartbeat time, watched roots, polling fallbacks, last complete
reconciliation, last error, and queue size (roots waiting for a rescan).

## Startup integration

```sh
rulescan service install            # preview only: shows the file, program, and arguments
rulescan service install --apply    # write and register it
rulescan service start | stop | status | logs | uninstall
```

- Install requires `scan.roots` in the config file. A background watcher never
  scans its working directory by accident.
- The integration runs
  `<absolute rulescan> --no-input --format text --config <absolute config> [--state-dir <dir>] watch --foreground`.
  It does not depend on a shell or `PATH`. If you move the executable, install
  again. `status` reports `up_to_date: false` when the installed file differs
  from what would be generated now.
- Every generated file carries the marker `Managed by RuleScan service install`.
  RuleScan only replaces or removes files with that marker. It refuses to
  replace any other file at that path, and never follows a symlink there.
- Native commands get their arguments as separate values, never as a shell
  string. Their output is escaped and limited to 4 KiB, and they time out after
  20 seconds.
- `uninstall` removes only the integration. Your config, state, approvals, and
  quarantine stay in place.

`status` reports three separate things:

- `installed`: a RuleScan-owned integration exists.
- `running`: the native supervisor reports the process running.
- `healthy`: running, a heartbeat within 3 intervals, status `watching`, a
  complete reconciliation recorded, and no polling fallbacks.

`logs` returns the last 200 audit events from SQLite: sequence, time, kind,
artifact id, and digest. It never returns content or paths. It also reports
where the native log is kept.

### macOS: LaunchAgent

- File: `~/Library/LaunchAgents/dev.rulescan.watch.plist`
- `ProgramArguments` array, `RunAtLoad`, `KeepAlive` only after a failed exit,
  `ThrottleInterval` 30 s, background process type, low-priority I/O,
  working directory set to the state directory.
- stdout and stderr go to `state/service/watch.log` (text summaries only). The
  watcher empties this file when it grows past 1 MiB.
- Managed with `launchctl bootstrap|bootout|kickstart|print gui/<uid>`. `stop`
  unloads the agent until `start` or your next login.
- Protected folders such as Downloads or Documents may need privacy permission
  for background execution. A denied folder shows up as a coverage gap.

### Linux: systemd user unit

- File: `$XDG_CONFIG_HOME/systemd/user/rulescan-watch.service`
- `ExecStart` with each argument quoted, and `%`/`$` escaped. `Restart=on-failure`,
  `RestartSec=30`, `StartLimitBurst=5` within 600 s. Logs go to the journal:
  `journalctl --user -u rulescan-watch.service`.
- Managed with `systemctl --user`. RuleScan never enables lingering, so the
  service runs only while you are logged in, unless you turn on lingering
  yourself.
- With no user systemd manager, use `rulescan watch --foreground` or schedule
  `rulescan scan` yourself.

### Windows: Scheduled Task

- Task `\RuleScan\Watch`, registered from `state\service\rulescan-watch.xml`
  (UTF-16). All fields are XML-escaped. Arguments follow the
  `CommandLineToArgvW` quoting rules.
- Logon trigger for the current user only. `InteractiveToken`, `LeastPrivilege`,
  no stored password, no elevation.
- `MultipleInstancesPolicy=IgnoreNew`, `ExecutionTimeLimit=PT0S` (no runtime
  limit), it runs on battery and keeps running when the computer switches to
  battery, restarts up to 5 times every minute after failure.
- Managed with `schtasks.exe /Create /Run /End /Query /Delete`. Task output is
  not captured. Use `service logs`.

## Validation status

| Area | macOS | Linux | Windows |
| --- | --- | --- | --- |
| Unit tests: template, escaping, ownership checks, preview writes nothing | passed | compiled on macOS only | compiled on macOS only |
| Foreground watch: native event triggers rescan, `--once`, single-instance lock, cancel time | passed (FSEvents, unit tests) | pending | pending |
| Generated LaunchAgent passes `plutil -lint` | passed | n/a | n/a |
| Real install, start, stop, login, reboot, uninstall | **pending: not run** | pending | pending |
| Sleep and resume, watch-limit exhaustion, network volumes | pending | pending | pending |

During development nothing was installed, started, or stopped on any machine.
The `launchctl`, `systemctl`, and `schtasks` paths have only been reviewed as
code. Windows task status matching uses English `Running` text and needs
native validation on other locales.

Integration updates refuse in-place replacement: explicitly uninstall and reinstall
to select a relocated executable/config path. Existing state and quarantine stay
intact. Installation uses exclusive creation even if a definition appears during
the write. Native supervisor lifecycle validation is still pending.
