# Development validation — 29 October 2026

Host: Apple M1 Max (MacBookPro18,4), ARM64, macOS 27.0.1, APFS. These observations
apply to this local checkout. No commit, hosted CI, public release or installed
startup integration is implied.

| Proof | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed on native macOS |
| `cargo test --locked --all-targets` | 64 tests passed across seven target suites |
| `cargo build --locked --release --bins` | Passed; local ARM64 development executable |
| `cargo check --manifest-path fuzz/Cargo.toml` | Harness compiled; extended fuzzing not run |
| `python3 scripts/native-smoke.py` | Native create/root-replacement detection, monotonic events, shutdown, 80×24 PTY review/apply, terminal restoration passed |
| Generated LaunchAgent preview + `plutil -lint` | Syntax passed; nothing registered/started |
| Schema snapshots | Compared against current executable in contract tests |
| Dependency inventory | 269 locked packages, 455 packaged upstream notice files retained |

The filesystem tests operate on real temporary files: exact raw-byte approval,
same-size/time content change, bundle additions/removals/permissions, imported
dependencies and cycles, excluded/private scopes, root replacement, safe previews,
stale plans, no-overwrite restore, hard links, source/payload ambiguity, metadata
preservation, interrupted prepared/moved recovery, and SQLite-trigger-injected
terminal-commit failure. Property tests exercise display escaping, native path
encoding and separator/globstar behavior. APFS rejects invalid UTF-8 filenames;
lossless byte encoding is tested without creating such an entry on this host.

An independent review reproduced two import-approval bugs during development:
skipped/over-budget references retaining partial fingerprints, and existing
candidate nodes stopping inherited import traversal. Both were corrected with
regression tests. Batch actions now revalidate bundles/dependencies per item;
terminal quarantine/restoration state, inventory, approval revocation and audit
metadata commit in one SQLite transaction. This review is not an external audit.

Windows-target Rust code and tests also passed Clippy type checking on this Mac:

```sh
LIBSQLITE3_SYS_USE_PKG_CONFIG=1 PKG_CONFIG_ALLOW_CROSS=1 \
  cargo clippy --locked --target x86_64-pc-windows-msvc --all-targets -- -D warnings
```

This check uses host SQLite metadata to bypass a Windows native C compiler during
type checking. It **does not build/link or execute a Windows binary**, validate
Windows SQLite packaging, or prove any native ACL/filesystem/service behavior.
Windows approvals and active-file mutations remain disabled pending qualification.

The benchmark generated 100,000 harmless files and found all 1,000 candidates
across 101,101 visited entries with complete coverage. Release-build wall time
was 1,105 ms on the first pass and 1,096 ms warm. Caches were not controlled;
CPU, peak memory, first-result latency and UI percentiles were not measured.
See `benchmark-results.json` and `benchmarks.md` for reproducibility and limits.

## Not verified

Native Linux/Windows execution, hosted CI, minimum supported OS versions, startup
install/login/reboot/upgrade/uninstall and macOS background privacy permissions,
Windows identity/ACL/ADS/sharing cases, full-disk/fsync/process-kill fault injection
at every stage, cross-volume removal, extended fuzz campaigns, large-tree/event
storm/idle CPU measurements, user usability sessions, vulnerability advisories,
release licensing, signed/notarized installers and public distribution remain
release gates in `release.md`. `cargo-audit` is not installed in this environment.
