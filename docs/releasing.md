# Building and publishing binaries

`Release binaries` builds and tests five native targets:

| Asset target | Native runner |
| --- | --- |
| x86_64-unknown-linux-musl | Ubuntu 22.04 x64 |
| aarch64-unknown-linux-musl | Ubuntu 24.04 ARM64 |
| aarch64-apple-darwin | macOS 15 Apple Silicon |
| x86_64-apple-darwin | macOS 15 Intel |
| x86_64-pc-windows-msvc | Windows Server 2022 x64 |

The release workflow pins Rust 1.95.0 and Python 3.12. Linux binaries use musl and
bundled SQLite, and their tests execute on the same CPU architecture as the build.
Both macOS builds set their deployment floor to macOS 15.0.
This does not certify every Linux distribution/kernel or every supported OS version.
Windows currently offers inventory/inspection only; approving baselines and active
file mutations remain disabled until its native identity and ACL gates pass.

Each native job checks formatting, runs Clippy and all Cargo targets' tests, builds
the release executable, creates its archive, then extracts and executes that exact
archive's binary for version and JSON-schema checks. A failed target prevents the
draft-release job. The relevant installer is also exercised against each native
archive (including Windows PowerShell 5.1). A successful build is not
service/login-session qualification.

Archives are named `rulescan-vVERSION-TARGET.tar.gz` (Unix) or `.zip` (Windows).
Each is flat and contains exactly the executable, `README.md`,
`THIRD_PARTY_NOTICES.txt`, and `manifest.json`. The manifest records package version,
target, full source commit and the executable SHA-256. Archive metadata timestamps,
ownership and modes are fixed; the tool does not claim identical compiled binaries
across build hosts or Rust/linker versions. Upstream license notices are preserved.

## Prepare a release

Update `Cargo.toml`'s package version and `docs/release-notes.md`, review the changes,
run local checks and push the intended source. The tag must equal `v` plus the
package version. Use the workflow UI or GitHub CLI to request a draft:

```sh
gh workflow run release.yml --ref main -f tag=v0.1.0
```

The workflow binds all builds and manifests to its exact checked-out commit. An
existing tag is accepted only when it resolves to that source; a release/draft
already using the tag is never overwritten. Dispatch does not create a tag before
the native builds pass. The final job uses the tested source as the draft's tag
target; GitHub may defer creating a new tag until the draft is published. Pushing
an already-created `v*` tag also runs the workflow, with tag existence required.

The complete draft contains five archives, `install.sh`, `install.ps1` and
`SHA256SUMS`. Checksums cover all archives and both installer scripts. They detect
corruption and bind downloads to that release; they are not an independent signing
identity. The initial binaries are unsigned and not notarized. Signed installers,
native startup qualification and the remaining filesystem gates are documented in
`release.md` and must not be inferred from this workflow.

## Verify and publish

Inspect the exact-source workflow results, download the draft assets with an
authenticated `gh release download`, verify `SHA256SUMS`, and exercise installation
into a temporary user-owned directory. Keep the notices with distributed binaries.
After reviewing the draft and its platform limitations, publish it explicitly:

```sh
gh release edit v0.1.0 --draft=false --latest
```

Verify unauthenticated release/download URLs and run the documented installation
command after publication. The workflow deliberately stops at a draft; a tag push
does not immediately make unreviewed artifacts public. Do not move a released tag
or replace published assets. For a changed binary or installer, publish a new
package version. Project crate publication remains disabled (`publish = false`).

The packaging helper is also usable locally after a release build:

```sh
python3 scripts/package-release.py --target aarch64-apple-darwin \
  --version 0.1.0 --commit FULL_COMMIT_SHA \
  --binary target/release/rulescan --output-dir dist
python3 -m unittest discover -s tests -p test_package_release.py
```

Windows binaries link the C runtime statically (`+crt-static`); Unix Linux
archives use musl. This avoids making the installer install a compiler, Rust or
a separate Visual C++ runtime. Native packaged execution is still required.
