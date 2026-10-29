#!/usr/bin/env python3
"""Create a flat, checksummable binary archive for a qualified native target."""
import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import re
import stat
import tarfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]
TARGETS = frozenset({
    "x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl",
    "aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-pc-windows-msvc",
})


def package(target, version, commit, binary, output_dir):
    if target not in TARGETS:
        raise ValueError("unsupported release target")
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?", version):
        raise ValueError("version must be a semantic version without the v prefix")
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("commit must be a full lowercase Git SHA-1")
    binary = Path(binary)
    if not stat.S_ISREG(binary.lstat().st_mode):
        raise ValueError("binary must be a regular file, not a link or directory")
    binary_bytes = binary.read_bytes()
    if not binary_bytes:
        raise ValueError("binary is empty")
    executable = "rulescan.exe" if target.endswith("-windows-msvc") else "rulescan"
    manifest = {
        "schema_version": 1,
        "version": version,
        "target": target,
        "commit": commit,
        "binary": executable,
        "binary_sha256": hashlib.sha256(binary_bytes).hexdigest(),
    }
    members = [
        (executable, binary_bytes, 0o755),
        ("README.md", (ROOT / "README.md").read_bytes(), 0o644),
        ("THIRD_PARTY_NOTICES.txt", (ROOT / "docs/third-party-notices.txt").read_bytes(), 0o644),
        ("manifest.json", (json.dumps(manifest, indent=2) + "\n").encode(), 0o644),
    ]
    output_dir = Path(output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    extension = "zip" if executable.endswith(".exe") else "tar.gz"
    destination = output_dir / f"rulescan-v{version}-{target}.{extension}"
    # Exclusive creation makes reruns explicit; never silently replace an asset.
    with destination.open("xb") as output:
        try:
            if extension == "zip":
                with zipfile.ZipFile(output, "w") as archive:
                    for name, data, mode in members:
                        info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
                        info.create_system = 3
                        info.external_attr = (stat.S_IFREG | mode) << 16
                        archive.writestr(info, data, compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)
            else:
                with gzip.GzipFile(filename="", mode="wb", fileobj=output, mtime=0) as compressed:
                    with tarfile.open(mode="w", fileobj=compressed, format=tarfile.PAX_FORMAT) as archive:
                        for name, data, mode in members:
                            info = tarfile.TarInfo(name)
                            info.size = len(data)
                            info.mode = mode
                            info.mtime = 0
                            archive.addfile(info, io.BytesIO(data))
        except BaseException:
            output.close()
            destination.unlink(missing_ok=True)
            raise
    return destination


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True, choices=sorted(TARGETS))
    parser.add_argument("--version", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    args = parser.parse_args()
    try:
        path = package(args.target, args.version, args.commit, args.binary, args.output_dir)
    except (ValueError, OSError) as error:
        parser.error(str(error))
    print(path)


if __name__ == "__main__":
    main()
