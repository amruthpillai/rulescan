#!/usr/bin/env python3
"""Regenerate the locked dependency inventory and bundled upstream notices."""
import json
from pathlib import Path
import subprocess
import tomllib

root = Path(__file__).resolve().parents[1]
metadata = json.loads(subprocess.check_output(
    ["rtk", "proxy", "cargo", "metadata", "--format-version", "1", "--locked"], cwd=root
))
locked = tomllib.loads((root / "Cargo.lock").read_text())
checksums = {(p["name"], p["version"]): p.get("checksum") for p in locked["package"]}
packages = []
notices = []
for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
    if package["source"] is None:
        continue
    directory = Path(package["manifest_path"]).parent
    files = sorted(p for p in directory.iterdir() if p.is_file()
                   and p.name.upper().startswith(("LICENSE", "COPYING", "NOTICE", "AUTHORS")))
    license_file = package.get("license_file")
    if license_file and (directory / license_file).is_file():
        files.append(directory / license_file)
    license_dir = directory / "LICENSES"
    if license_dir.is_dir():
        files.extend(p for p in sorted(license_dir.rglob("*")) if p.is_file())
    files = sorted(set(files))
    packages.append({
        "name": package["name"], "version": package["version"],
        "source": package["source"], "checksum": checksums.get((package["name"], package["version"])),
        "license": package["license"], "repository": package["repository"],
        "native_links": package.get("links"),
        "notice_files": [str(p.relative_to(directory)) for p in files],
        "declared_dependencies": sorted({p["name"] for p in package["dependencies"]}),
    })
    for path in files:
        notices.append(f"{'=' * 72}\n{package['name']} {package['version']} — {path.relative_to(directory)}\n"
                       f"{'=' * 72}\n{path.read_text(errors='replace')}\n")
(root / "docs/dependency-inventory.json").write_text(json.dumps({
    "source": "Cargo.lock and cargo metadata --locked; includes dev and target-specific dependencies",
    "advisory_audit": "Not run; cargo-audit is not installed. Required before release.",
    "packages": packages,
}, indent=2) + "\n")
(root / "docs/third-party-notices.txt").write_text("\n".join(notices))
print(f"Recorded {len(packages)} locked packages and {len(notices)} upstream notice files.")
