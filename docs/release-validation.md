# Binary release validation

Release: [RuleScan v0.1.1](https://github.com/amruthpillai/rulescan/releases/tag/v0.1.1).
Published 29 October 2026 from source
[`b47cd677164ad4878e6ff71893b1387866bd3974`](https://github.com/amruthpillai/rulescan/commit/b47cd677164ad4878e6ff71893b1387866bd3974).

## Hosted release checks

The separate [standard native CI run 40837339838](https://github.com/amruthpillai/rulescan/actions/runs/40837339838)
passed on Linux, macOS and Windows at the same release source.

[Release workflow run 40837367831](https://github.com/amruthpillai/rulescan/actions/runs/40837367831)
passed all five native target jobs before assembling the draft:

| Target | Native host | Result |
| --- | --- | --- |
| x86_64-unknown-linux-musl | Ubuntu 22.04 x64 | Passed |
| aarch64-unknown-linux-musl | Ubuntu 24.04 ARM64 | Passed |
| aarch64-apple-darwin | macOS 15 Apple Silicon | Passed |
| x86_64-apple-darwin | macOS 15 Intel | Passed |
| x86_64-pc-windows-msvc | Windows Server 2022 x64 | Passed |

Each job ran formatting, Clippy, all Rust test targets, five packaging tests,
optimized compilation, execution of the extracted archive's binary (version and
JSON schema), and the native installer against that actual archive. Unix jobs
also passed two real tmux terminal-close/external-cancellation regressions and six
installer cases. Windows PowerShell 5.1 passed installation, upgrade, checksum and
duplicate rejection, destination-directory refusal and archive traversal rejection.
Linux ELF checks confirmed no program interpreter or shared-library dependencies.
The actual Linux x64 archive also passed version/schema execution in a plain
Ubuntu 22.04 container without musl installed (emulated x64 on this ARM Mac);
this supplements the native x64 runner checks.

Earlier hosted attempts exposed lints, Windows preview path separators, the musl
renameat2 wrapper/linker configuration, and PowerShell fixture scoping/null-string
conversion. These were fixed before publication. A minimal Ubuntu 22.04 x64
container reproduced the linker crash with a hello-world program; removing the
forced musl-gcc Rust linker produced a working static PIE. The musl compiler is
still used for bundled C dependencies. The successful run rebuilt every target
from one source commit; artifacts from failed runs were not reused.

The initial v0.1.0 release was also built and published with these gates. Its final
anonymous check exposed an exhausted GitHub REST API quota; v0.1.1 removes that
dependency from Windows latest-version resolution. The original assets were not
replaced. Windows tests cover both Windows PowerShell and modern PowerShell
response shapes.

## Downloaded release and anonymous installation

The draft was downloaded and inspected before publication. All eight assets were
present: five archives, two installer scripts and SHA256SUMS. All seven checksummed
assets matched. Every archive contained exactly four regular files; each manifest
matched the release version, target, source commit and executable SHA-256. Bundled
README and third-party notice text matched the tested source (Windows checkout
uses CRLF line endings); both installer scripts matched the source bytes exactly.
The downloaded Apple Silicon executable returned the expected version and schema.

After publication, anonymous HTTPS downloads verified the latest release redirect,
all eight assets, README and three screenshot PNGs. Downloaded bytes matched the
inspected draft/committed screenshots. The README's Unix one-command installer
installed into a temporary user-owned directory without GitHub credentials, and
the installed executable returned `rulescan 0.1.1`. No user shell profile, normal
installation directory, configuration or startup integration was changed.

## Local checks and remaining limits

Local development validation on ARM64 macOS during 0.1.0 preparation included
the checks below. The 64 Rust tests were rerun for 0.1.1; the hosted checks above
bind the final binaries to their exact source.

- 64 Rust tests, formatting, Clippy and the optimized build.
- Five deterministic packaging/manifest/member validation tests.
- Six Unix installer cases with synthetic packages and a real native archive.
- Two real tmux terminal-EOF/external-termination regressions; raw Ctrl-C warnings
  and exact cooked-terminal restoration remain verified.
- Native watcher and 80-column PTY review/apply smoke checks.
- Three genuine Ghostty window captures of the locally built 0.1.0 UI against
  synthetic data; 0.1.1 changes installation/version metadata, not this UI.
- [RustSec audit](security-audit.md): zero known vulnerabilities and one
  informational unmaintained YAML-parser warning, without suppression. The
  lockfile SHA-256 still matches the recorded audit.

Binary availability does not qualify Windows approvals/mutations, background
service install/login/reboot/upgrade behavior, signing/notarization, or adversarial
filesystem cases. Those limits remain explicit in the README and [release
checklist](release.md). The public one-command installer was executed on macOS;
the public Windows and Linux commands were also executed on their native hosted
runners. [Public installer run 40838356832](https://github.com/amruthpillai/rulescan/actions/runs/40838356832)
passed all three native jobs using the exact README commands, the latest public
installer scripts, and the expected installed version `0.1.1`. No checkout or
GitHub authentication was used by these jobs.
