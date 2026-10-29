# Binary release validation

Target release: `v0.1.0`. The [release workflow](releasing.md) builds all five
targets and creates a private draft only after native tests, packaged execution
and native installer smoke checks pass. The draft is inspected before publication.

Completed locally on ARM64 macOS:

- 64 Rust tests, formatting, Clippy and the optimized build.
- Five deterministic packaging/manifest/member validation tests.
- Six Unix installer cases with synthetic packages and the real macOS archive:
  install/upgrade, immutable latest selection, checksums, archive validation,
  unsupported targets and destination link/directory refusal.
- Two real tmux regressions for terminal EOF and external termination; normal
  raw Ctrl-C warnings and exact cooked-terminal restoration are preserved.
- Three genuine Ghostty window captures of the running TUI against synthetic data.
- [RustSec audit](security-audit.md): zero known vulnerabilities and one
  informational unmaintained YAML-parser warning, retained without suppression.

The first hosted run found newer-toolchain lints; the second passed Linux/macOS
and found a Windows bundle-preview separator issue. Those findings were fixed
before the binary release. Terminal capture also uncovered a Crossterm EOF spin;
its existing EOF-safe backend is enabled and verified by real terminal regressions.
Native release and anonymous-install results are
recorded here after the release workflow completes.

Binary availability does not qualify Windows approvals/mutations, background
service install/login/reboot/upgrade behavior, signing/notarization, or adversarial
filesystem cases. Those limits remain explicit in the README and [release
checklist](release.md). No startup integration was installed during local validation.
