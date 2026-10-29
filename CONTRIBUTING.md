# Contributing

Use Rust 1.89+ and a local filesystem for state. Keep CLI/TUI/watch callers on the
shared engine. All unknown or unstable filesystem cases should become review-only
or explicit incomplete coverage; never weaken a guard to make a fixture pass.

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --locked --release
```

Tests create harmless temporary workspaces and private state. Nothing installs
a service or executes discovered content. Unit tests exercise table state and a
Ratatui TestBackend; integration tests invoke the real CLI with piped IO. Native
mutation tests use actual moves and metadata on Unix. Windows approval/mutation tests are
gated until the native backend is qualified; inventory-only behavior is tested separately; compilation alone grants no support.

To add a candidate convention, extend `discovery`, retain a stable rule ID,
associate a profile, add ignored/hidden/global-root fixtures and update the source
coverage matrix. To change semantics, bump `PROFILE_VERSION` or the manifest/
policy version. Do not infer installed-agent behavior from a filename or silently
parse arbitrary executable configuration. An adapter must bound depth, entries,
bytes and runtime, expose references and coverage gaps, and execute no imports.

No analyzer API ships yet. An eventual analyzer is advisory and separately
enabled; it cannot approve its own untrusted input. Do not add provider credentials,
network uploads or executable plugins to discovery. A concrete integration and
data-flow review must precede such a feature.

Filesystem changes need real temporary-filesystem tests for stale snapshots,
no-overwrite, concurrency and recovery. Add native target fixtures for ACLs,
reparse points, case, permissions and metadata. Keep mock-only tests out of claims
about actual OS operations. Check the release matrix for unsupported scenarios.

Fuzz configuration and terminal preview handling with cargo-fuzz/nightly:

```sh
cargo fuzz run config_and_preview -- -max_len=1048576 -rss_limit_mb=256 -timeout=2
```

Fuzzing is deliberately outside the normal workspace. The harness bounds config
and preview input; the runtime flags bound memory and time. An extended fuzz run is
a release gate, not a result implied by checking the harness into source control.

Regenerate machine contracts with `python3 scripts/update-schemas.py` after building
the binary, or `rulescan schema --kind KIND`; schema snapshots
must match the current types. Regenerate dependency notices after changing the
lockfile. Do not change the supplied logo or introduce telemetry.
