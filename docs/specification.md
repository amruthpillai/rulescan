# RuleScan product contract

The command is `rulescan`. The supplied logo is retained verbatim in `assets/`.
The initial application covers local discovery, review, approved baselines,
quarantine/restore, headless commands, foreground monitoring and generated user
startup integrations. It does not automatically approve or quarantine anything.

The base command opens the dashboard only when stdin and stdout are terminals,
output mode is automatic and `--no-input` is absent. Scanning is read-only with
respect to scanned roots. Private inventory records may update. First use never
means trust-on-first-use. Current workspace plus disclosed existing user agent
folders are the interactive default. A piped invocation uses configured roots or
the current directory. Downloads and broad home scans require a scope choice.

The README key map is the default interaction contract. Scans, previews and
action application run off the input thread. The list retains row identity;
staging waits for the current scan to finish. Selection applies only to visible
filtered rows. File contents, paths and diagnostics are escaped for terminal
safety. Preview bytes are bounded and diff calculation has a deadline. Terminal
state is restored on cancellation, errors and panic. Staged actions are displayed
with paths, action, and SHA-256 before Enter applies them. Outcomes are per-item.

Exact snapshot approval registers an artifact, never future contents of its
workspace. File bytes, path, root identity, executable bits, profile/policy
revision, imports and bundle manifests participate. New resources or removals
invalidate bundles. Explicit exclusions are visible coverage decisions.
Permissive trust and automatic quarantine are intentionally absent.

JSON snapshots and JSONL monitor events have schema version 1. Content is omitted
unless explicitly requested and is labeled untrusted. Command and schema output
share the same Rust models. `schema --kind commands` exports the Clap command
tree. JSON is reserved for stdout; ordinary diagnostics use stderr. Raw content
is never sent to a service or analyzer. Private current-baseline snapshots are
bounded to 256 KiB each / 64 MiB total. Quarantine is never automatically purged.

Research and release gates from the source plan remain separate: three-platform
native execution, 5–8 developer usability sessions, fault injection under disk
full/locked/network volumes, native login/reboot/background permissions, published
packages and signing, and measured latency/CPU targets. Coverage adapters for
installed agent versions, complex settings/import syntax, a daemon IPC broker,
notifications, automatic profile updates, analyzers and MCP are future work.
The first release uses a shared advisory coordination lock instead of adding an
IPC transport; both foreground and monitor mutations serialize through it.

No result proves that instructions are benign, that all agent-specific loading
semantics were resolved, or that a running agent has not already consumed a file.
