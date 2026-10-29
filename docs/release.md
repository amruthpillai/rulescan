# Native packaging and release qualification

RuleScan binaries are distributed through [GitHub releases](https://github.com/amruthpillai/rulescan/releases).
The [release workflow](releasing.md) builds and tests five native targets, verifies
the packaged executables and installers, and assembles a draft with checksums.
Publication is explicit after inspecting that draft. The source package is not
published to crates.io.

Install a local development binary with `cargo install --path . --locked`. Startup
integration is a separate explicit `service install` preview / `--apply` operation.
The base command never registers login items. See `background.md` for generated
native definitions. Uninstall preserves approvals, configuration and quarantine.

Artifacts retain dependency notices, checksums and exact source/target manifests.
The [advisory audit](security-audit.md) records the audited lockfile and its one
maintenance warning. Project-level open-source licensing remains unselected.
Initial binaries are unsigned: macOS notarization and Windows signing are not
provided. No signing credentials are embedded or requested by the installers.
The README distinguishes available binary targets from unqualified operations.

## Gates

- [x] Fresh exact-source CI on Linux, macOS and Windows; native ARM execution before
      declaring ARM support, and a separate musl build/test before portability claims.
      See [binary release validation](release-validation.md) for source and native-run evidence.
- [ ] Windows file identity, owner-only ACLs, reparse-point handling, ADS preservation,
      locked files, OneDrive/UNC behavior and no-replace moves before enabling approvals or mutation.
- [ ] Native macOS Terminal versus LaunchAgent privacy permission tests, including
      Downloads/Documents and the signed executable identity.
- [ ] Native systemd user session, missing manager, watch exhaustion, bind mounts,
      network/offline roots and current distribution plus Ubuntu LTS qualification.
- [ ] Scheduled Task logon, restart, battery behavior and localized status; confirm
      non-elevated identity/no password and explicit single-instance runtime settings.
- [ ] Install/start/stop/login/logout/reboot/sleep/resume/upgrade/uninstall under an
      ordinary user. Confirm executable relocation is diagnosed and state retained.
- [ ] Terminate after every durable file/DB stage; inject fsync/full-disk/sharing
      failures and concurrent editor writes; verify no unique recoverable copy is lost.
- [ ] Validate bounded preview/parser fuzzing with recorded corpus/runtime limits.
- [ ] Benchmark 100k/large dependency tree, 10k TUI rows and controlled event bursts;
      record hardware, OS, filesystem, cache state, coverage and supported features.
- [ ] Observe 5–8 new developers finding/reviewing/staging/applying/restoring on the
      three OS families, including 80-column, no-color and accessible plain flow.
- [ ] Audit dependency notices/advisories, storage privacy/retention and same-user
      messaging; validate installed/signed artifacts separately from development builds.

Deferred features have no stub security claim: permissive trust, automatic
quarantine, cross-volume removal, executable plugins, semantic analyzers,
notifications, agent-version/settings loading adapters, MCP and automatic profile
updates. IPC transport is also deferred in favor of one shared coordination lock.
Native correctness gates determine release scope rather than a speculative date.
