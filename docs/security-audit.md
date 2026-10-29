# Dependency advisory audit — 29 October 2026

Audited `Cargo.lock` with cargo-audit 0.22.2 and a freshly fetched RustSec database.
The [raw report](security-audit.json) is retained alongside the exact lockfile.

- Lockfile SHA-256: `fd8e2a1f4d0d8f4b8f0b6d61161583734b7e4d80cc5e309729f446d53c930315`.
- RustSec database commit: `40f5be6fb12ed93fc591bf4ab23b7d793b3f2239`.
- Known vulnerabilities: **0**.
- Informational warnings: **1**, `RUSTSEC-2024-0449`: `unsafe-libyaml` 0.2.11 is unmaintained.
- No advisories were ignored or suppressed.

The maintenance warning is inherited through `serde_yaml_ng`. There is no patched
release of `unsafe-libyaml`. Replacing the YAML parser requires compatibility
proof for duplicate/unknown keys, aliases, bounded input and existing configuration;
it is not hidden by a release-time dependency swap. Current input is limited to
1 MiB, rejects YAML anchors/aliases before parsing, and validates the user config.
This warning is not a reported exploitable vulnerability or a safety verdict.

Reproduce after installing cargo-audit:

```sh
cargo audit --json > docs/security-audit.json
```

Rerun after lockfile updates and before each release; an old report is not a
current audit. This check does not replace the filesystem/recovery and native
platform qualifications in [release.md](release.md).
