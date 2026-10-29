<img src="assets/rulescan-logo.png" alt="RuleScan" width="600">

Review the instructions your coding agents can find. Approve exact snapshots,
notice changes, and quarantine files or skill bundles with a recovery record.

RuleScan is a local Rust CLI with a keyboard dashboard. Discovery, hashing, review,
and monitoring work offline. No account, model, API key, or telemetry is required.
**A matching filename is a candidate, not a verdict that its contents are safe.**

## Install

No Rust toolchain is needed for the prebuilt binaries.

**macOS and Linux**

```sh
curl -fsSL https://github.com/amruthpillai/rulescan/releases/latest/download/install.sh | sh
```

**Windows — PowerShell**

```powershell
irm https://github.com/amruthpillai/rulescan/releases/latest/download/install.ps1 | iex
```

The installers select your platform, verify the archive against the release's
SHA-256 checksums, and install for the current user without administrator access.
They do not register a background service or change agent settings. Unix installs
into `~/.local/bin`; Windows installs into `%LOCALAPPDATA%\Programs\RuleScan` and
adds that directory to your user PATH. If `~/.local/bin` is not already on your
PATH, add that directory to your shell PATH. Then run `rulescan`.

To pin a release or choose a different directory, download and inspect the installer
before running it. Unix accepts `RULESCAN_VERSION=v0.1.0` and
`RULESCAN_INSTALL_DIR`; PowerShell accepts `-Version v0.1.0` and `-InstallDir`.
For an upgrade with a background monitor running, stop it with `rulescan service
stop`, rerun the installer, then start it with `rulescan service start`. The
installer preserves configuration, approvals and quarantine.

### Download a binary directly

[All release assets and SHA256SUMS](https://github.com/amruthpillai/rulescan/releases/latest).
The initial release provides these archives:

| Platform | CPU | Download |
| --- | --- | --- |
| macOS 15+ | Apple Silicon | [tar.gz](https://github.com/amruthpillai/rulescan/releases/download/v0.1.0/rulescan-v0.1.0-aarch64-apple-darwin.tar.gz) |
| macOS 15+ | Intel x64 | [tar.gz](https://github.com/amruthpillai/rulescan/releases/download/v0.1.0/rulescan-v0.1.0-x86_64-apple-darwin.tar.gz) |
| Linux | x64 | [tar.gz](https://github.com/amruthpillai/rulescan/releases/download/v0.1.0/rulescan-v0.1.0-x86_64-unknown-linux-musl.tar.gz) |
| Linux | ARM64 | [tar.gz](https://github.com/amruthpillai/rulescan/releases/download/v0.1.0/rulescan-v0.1.0-aarch64-unknown-linux-musl.tar.gz) |
| Windows | x64 | [zip](https://github.com/amruthpillai/rulescan/releases/download/v0.1.0/rulescan-v0.1.0-x86_64-pc-windows-msvc.zip) |

Download the matching archive and `SHA256SUMS` from the same release. Verify its
hash with `shasum -a 256` (macOS), `sha256sum` (Linux), or `Get-FileHash -Algorithm
SHA256` (PowerShell). Extract it with `tar -xzf ARCHIVE.tar.gz` or Windows' **Extract
All**, then put `rulescan` / `rulescan.exe` in a directory on your PATH. Keep the
included `THIRD_PARTY_NOTICES.txt` with the installed binary.

For example, after extracting the macOS or Linux archive:

```sh
mkdir -p "$HOME/.local/bin"
install -m 755 rulescan "$HOME/.local/bin/rulescan"
install -m 644 THIRD_PARTY_NOTICES.txt "$HOME/.local/bin/rulescan-notices.txt"
"$HOME/.local/bin/rulescan" --version
```

Linux archives use musl and do not require a particular glibc version. Binaries are
unsigned; macOS notarization and Windows code signing are not provided. **Windows
currently supports inventory and inspection only; approval, quarantine and restore
remain disabled pending native identity/ACL qualification.** Full limitations and
validation evidence are in [the release checklist](docs/release.md).

### Build from source

Requires Rust 1.89 or newer; binary installation above does not.

```sh
cargo build --locked --release
./target/release/rulescan
```

## Screenshots

Captured from the real macOS terminal UI using a harmless demo workspace.

**Review findings alongside an approved-baseline diff.**

![RuleScan dashboard showing changed instructions and their approved-baseline diff](docs/screenshots/dashboard.png)

**Stage decisions, then review exact targets and content hashes before applying.**

![RuleScan action review with approvals and a recoverable skill-bundle quarantine](docs/screenshots/review.png)

**Inspect retained quarantine records and preview a restore.**

![RuleScan quarantine view showing a retained skill and restore guidance](docs/screenshots/quarantine.png)

[Capture method and reproduction instructions](docs/screenshots/README.md).

## Review instructions

The dashboard starts with the current workspace and existing user agent folders
when no roots are configured. It discloses those roots, scans without changing
instructions, and leaves Downloads opt-in. Press `w` to choose scopes and explicitly
save an initial configuration. In a pipe, the base command produces a read-only
summary and never adds global folders automatically.

| Key | Action |
| --- | --- |
| `j` / `k`, arrows | Navigate |
| Space | Select a row |
| `/`, `f` | Search; filter by status, agent, category, workspace or bundle |
| Enter, `d` | Detail; toggle content/diff |
| `a`, `x`, `u` | Stage approval, stage quarantine, undo a staged choice |
| `c`, Enter | Review the exact action queue, then apply |
| `r`, `Q` | Refresh; open quarantine and stage a restore |
| Tab, `:`, `?`, `w` | Focus pane, command palette, help, scopes/settings |
| Esc, `q` / Ctrl-C | Back; quit with a staged-action warning |

Use `--plain` for a linear terminal review and `--no-color` or `NO_COLOR` for text
labels without color. No repository-defined pager, editor, hook, or MCP server runs.

For scripts and agents, every decision uses the same engine:

```sh
rulescan scan --root /path/to/workspace --format json --no-input
rulescan findings list --status new,changed --format json
rulescan inspect ARTIFACT_ID --content --diff --format json
rulescan plan approve --id ARTIFACT_ID --expected-sha256 OBSERVED_DIGEST --format json
rulescan plan show PLAN_ID --format json
rulescan apply PLAN_ID --no-input --format json
rulescan check --workspace /path/to/workspace --format json

rulescan plan quarantine --id ARTIFACT_ID --expected-sha256 OBSERVED_DIGEST
rulescan apply PLAN_ID --no-input
rulescan quarantine list --format json
rulescan restore QUARANTINE_ID --to /unused/destination --no-input
rulescan apply RESTORE_PLAN_ID --no-input
```

`restore` creates a reviewable plan; it never moves a payload until `apply`.
`--no-input` suppresses interaction and **does not grant approval**. Batch planning
accepts repeated `--id` and `--expected-sha256` pairs in corresponding order.

```sh
rulescan config init
rulescan config validate
rulescan config show
rulescan patterns test --path 'nested/AGENTS.md'
rulescan schema --kind output
rulescan completions zsh
rulescan doctor
rulescan recover
rulescan watch --foreground --root /path/to/workspace --format jsonl
rulescan service install                # Preview; needs configured roots
rulescan service install --apply        # Explicit per-user startup installation
rulescan service status
rulescan run --workspace /path/to/workspace -- your-agent arguments
```

The `run` wrapper passes arguments directly without a shell. It returns 125 when
preflight blocks execution, otherwise preserves child execution/exit behavior.
It does not freeze the filesystem or eliminate check-to-read races.

| Exit | Meaning |
| --- | --- |
| 0 | Selected operation completed; scans have no actionable findings |
| 1 | Scan completed with review required |
| 2 | Invocation or configuration error |
| 3 | Incomplete coverage, taking precedence over findings |
| 4 | Stale snapshot, action conflict or partial conflicting batch |
| 5 | Operational failure |
| 130 | Interrupted scan |

Built-in profiles cover AGENTS/Codex, Claude, Gemini, Copilot, Cursor, skills,
and known hook/MCP settings. Custom basenames, relative globs and bundle markers
extend discovery through [a private YAML config](docs/configuration.md).
Hidden directories and repository-ignored dependencies remain in scope.
Local `@path` directives, including cycles and outside-root references, are tracked
conservatively; installed-agent loading behavior is not inferred from discovery.

Quarantine uses exclusive same-volume native moves on macOS/Linux. It preserves
the recoverable original, records interrupted stages, disables stored file execution,
and refuses restore collisions. Links, special files, hard links, shared settings,
special permission bits, and cross-volume moves stay read-only. Windows approval and file
mutations are deliberately disabled until its native identity/ADS/ACL gate passes.
See [recovery](docs/recovery.md) before manually moving private payloads.

The monitor detects changes **after writes**. It uses native events plus full
reconciliation and visible polling fallbacks. A same-user agent or malware can
alter RuleScan's files; this is not a sandbox or a privilege boundary.

Development and the remaining research gates are described in
[contributing](CONTRIBUTING.md), [architecture](docs/architecture.md),
[coverage](docs/discovery.md), [background operation](docs/background.md), and
[the product contract](docs/specification.md). The supplied research plan is
retained as [source material](docs/source-plan.md), including its original
placeholder command name. It is not a list of completed validation claims.

Actual local checks and outstanding gates are recorded in [validation](docs/validation.md).
