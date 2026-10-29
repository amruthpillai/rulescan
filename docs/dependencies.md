# Dependency and code-reuse inventory

`Cargo.lock` pins exact crate versions and registry checksums. The complete direct,
transitive, development and platform-specific inventory (including licenses,
repositories, native link names and dependency declarations) is generated in
`dependency-inventory.json`. `third-party-notices.txt` preserves packaged upstream
license/notice texts. Regenerate both with `python3 scripts/dependency-inventory.py`
after a deliberate dependency update. No security-product source was copied.

| Adopted library | Owner responsibility / alternative considered |
| --- | --- |
| Ratatui / Crossterm | Stateful terminal rendering/events, cleanup and test backend |
| Clap / clap_complete | One command model, validation, help, shell completions |
| Walkdir | Explicit unignored traversal, no directory link following |
| Globset | Compiled custom rules with deliberate cross-platform semantics |
| Notify | Native event hints; reconciliation remains authoritative |
| Rusqlite / libsqlite3-sys bundled | Local transactions and approved snapshots; no remote database |
| SHA2 / hex | Raw-byte SHA-256 and lossless path encoding |
| Serde / serde_json / serde_yaml_ng / Schemars | Versioned, strict config and generated machine schemas |
| Similar | Bounded diff, 200ms calculation deadline |
| Directories | Native per-user paths |
| UUID | Opaque action/payload identifiers |
| Libc | Native no-follow directory operations, exclusive rename, Unix identity |
| Ctrlc | Cancellation and termination handling |
| Tempfile / Proptest | Real filesystem fixtures and property tests |
| Xattr | Native metadata preservation verification (test-only) |

File locking uses Rust's standard library (Rust 1.89+) instead of another lock
crate. Quarantine does not depend on OS Trash retention. No executable plugin,
analyzer, MCP bridge or network-client dependency is included. Inspirations in the
source research plan are workflow references, not code imports or audited products.

Bundled SQLite includes native C build steps; libc exposes platform calls;
watcher backends have native OS dependencies. Inventory `native_links` and the
locked target graph must be rechecked for each release. Cargo feature resolution
and compatible versions are proven by a locked build on a target, not this table.

Update process: review the lockfile diff, upstream licenses/notices, release notes
and advisories; regenerate inventory; run format/lint/integration/native gates;
retest filesystem and service semantics when related dependencies change. Profile
behavior changes require an explicit revision bump and reapproval. Workflow
actions are pinned to inspected remote commit IDs, rather than mutable tags.

Security advisory audit is **not verified**: cargo-audit is not installed in the
development environment. Run an up-to-date RustSec audit before public packaging.
The inventory is a license/dependency record, not a legal compatibility opinion.
Project license selection and binary redistribution review remain maintainer gates.
