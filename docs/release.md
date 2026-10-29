# Native packaging and release qualification

This is an unpublished development package. The initial CI definition builds,
formats, lints, tests and packages binaries on native macOS, Linux and Windows
GitHub runners. CI has not been dispatched or observed from this local checkout.
Its build artifacts are unsigned development binaries, not public releases.

Install a local development binary with `cargo install --path . --locked`. Startup
integration is a separate explicit `service install` preview / `--apply` operation.
The base command never registers login items. See `background.md` for generated
native definitions. Uninstall preserves approvals, configuration and quarantine.

Choose the project license, release names and tested minimum OS versions before
distribution. Generate notices/inventory, run a fresh RustSec advisory audit, and
include notices alongside each binary. Public artifacts need checksums, source
revision, build/target metadata, install instructions and an explicit compatibility
matrix. Sign/notarize macOS and sign Windows packages with maintainer-managed
credentials only after the native tests below. No credentials are embedded here.

## Gates

- [ ] Fresh exact-source CI on Linux, macOS and Windows; native ARM execution before
      declaring ARM support, and a separate musl build/test before portability claims.
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
