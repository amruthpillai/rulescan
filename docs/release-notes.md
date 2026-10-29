RuleScan 0.1.1 keeps Windows installation independent of GitHub's anonymous REST API quota by resolving the latest release through its public redirect.

This patch rebuilds all five native binary targets from the same source. RuleScan's binary distribution, introduced in 0.1.0, includes:

- Keyboard dashboard and JSON CLI for discovering and reviewing local agent instructions.
- Exact-content approvals, bundle manifests, local import tracking, and recoverable same-volume quarantine/restore on macOS/Linux.
- Native event monitoring with reconciliation and explicit per-user startup integration.
- Native-built Apple Silicon, Intel macOS, Linux x64/ARM64 (musl) and Windows x64 archives.
- Shell and PowerShell installers, SHA256SUMS, source-commit manifests and bundled dependency notices. No Rust installation is needed.

Install on macOS/Linux:

```sh
curl -fsSL https://github.com/amruthpillai/rulescan/releases/latest/download/install.sh | sh
```

Install on Windows with PowerShell:

```powershell
irm https://github.com/amruthpillai/rulescan/releases/latest/download/install.ps1 | iex
```

This is an initial 0.x release. Windows provides inventory/inspection only;
approvals and filesystem mutations remain disabled. macOS builds target macOS 15+.
Binaries are unsigned and not notarized. Service login/reboot/permission and
adversarial filesystem qualification remain separate from native build/test checks.

RuleScan detects unexpected instructions and content changes. It does not prove
instructions are benign, sandbox an agent, or prevent reads before a watcher fires.
See the README, recovery runbook and release qualification notes before enabling
background operation. No startup integration is installed by the binary installers.
