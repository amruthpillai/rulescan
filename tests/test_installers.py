#!/usr/bin/env python3
"""Run the actual Unix installer with isolated release downloads and destinations."""
import argparse
import hashlib
import io
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest

parser = argparse.ArgumentParser()
parser.add_argument("--archive", type=Path, help="Use a native packaged executable for the install smoke")
parser.add_argument("--version", default="0.1.0")
arguments, unittest_arguments = parser.parse_known_args()
INSTALLER = Path(__file__).resolve().parents[1] / "scripts/install.sh"
TAG = "v" + arguments.version.removeprefix("v")
HOST_OS = platform.system()
HOST_ARCH = platform.machine()
HOST_TARGET = {
    ("Darwin", "arm64"): "aarch64-apple-darwin",
    ("Darwin", "aarch64"): "aarch64-apple-darwin",
    ("Darwin", "x86_64"): "x86_64-apple-darwin",
    ("Linux", "x86_64"): "x86_64-unknown-linux-musl",
    ("Linux", "aarch64"): "aarch64-unknown-linux-musl",
    ("Linux", "arm64"): "aarch64-unknown-linux-musl",
}.get((HOST_OS, HOST_ARCH))


@unittest.skipUnless(os.name == "posix" and HOST_TARGET, "Unix installer native test")
class Installers(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="rulescan-installer-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.release = self.root / "release"
        self.release.mkdir()
        self.bin = self.root / "with spaces/bin"
        self.tools = self.root / "tools"
        self.tools.mkdir()
        self.asset = f"rulescan-{TAG}-{HOST_TARGET}.tar.gz"
        self.archive = self.release / self.asset
        if arguments.archive:
            if arguments.archive.name != self.asset:
                self.fail(f"Expected native archive {self.asset}, received {arguments.archive.name}")
            shutil.copyfile(arguments.archive, self.archive)
        else:
            self.make_archive()
        self.checksum()
        self.env = {
            **os.environ,
            "PATH": str(self.tools) + os.pathsep + os.environ.get("PATH", ""),
            "RULESCAN_INSTALL_DIR": str(self.bin),
            "RULESCAN_VERSION": TAG,
            "FIXTURE_RELEASE": str(self.release),
            "FIXTURE_TAG": TAG,
            "FIXTURE_OS": HOST_OS,
            "FIXTURE_ARCH": HOST_ARCH,
            "CURL_LOG": str(self.root / "urls.txt"),
        }
        self.tool("curl", r'''
import os, pathlib, shutil, sys
args=sys.argv[1:]; url=args[-1]
with open(os.environ['CURL_LOG'],'a') as log: log.write(url+'\n')
base='https://github.com/amruthpillai/rulescan'
if url==base+'/releases/latest':
    print(base+'/releases/tag/'+os.environ['FIXTURE_TAG'],end='')
elif url.startswith(base+'/releases/download/'+os.environ['FIXTURE_TAG']+'/'):
    name=url.rsplit('/',1)[1]
    shutil.copyfile(pathlib.Path(os.environ['FIXTURE_RELEASE'])/name,args[args.index('--output')+1])
else: raise SystemExit('unexpected network URL: '+url)
''')
        self.tool("uname", "import os,sys; print(os.environ['FIXTURE_OS' if sys.argv[1]=='-s' else 'FIXTURE_ARCH'])")
        self.tool("sysctl", "print('0')")

    def tool(self, name, source):
        path = self.tools / name
        path.write_text(f"#!{sys.executable}\n" + source)
        path.chmod(0o755)

    def make_archive(self, special=None):
        with tarfile.open(self.archive, "w:gz") as archive:
            files = {
                "rulescan": f"#!/bin/sh\nprintf 'rulescan {TAG[1:]}\\n'\n".encode(),
                "README.md": b"Harmless installer fixture.\n",
                "THIRD_PARTY_NOTICES.txt": b"Upstream notices must remain installed.\n",
                "manifest.json": b"{}\n",
            }
            for name, content in files.items():
                info = tarfile.TarInfo(name)
                info.size = len(content)
                info.mode = 0o755 if name == "rulescan" else 0o644
                archive.addfile(info, io.BytesIO(content))
            if special:
                info = tarfile.TarInfo(special)
                if special == "rulescan":
                    info.size = 1
                    archive.addfile(info, io.BytesIO(b"x"))
                elif special == "README.md":
                    info.type = tarfile.SYMTYPE
                    info.linkname = "/outside-target"
                    archive.addfile(info)
                else:
                    info.size = 1
                    archive.addfile(info, io.BytesIO(b"x"))

    def checksum(self):
        self.hash = hashlib.sha256(self.archive.read_bytes()).hexdigest()
        (self.release / "SHA256SUMS").write_text(f"{self.hash}  {self.asset}\n")

    def install(self, *, success=True):
        result = subprocess.run(["sh", str(INSTALLER)], env=self.env, capture_output=True, text=True)
        if success:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def test_install_and_explicit_upgrade_keep_notices(self):
        self.install()
        binary = self.bin / "rulescan"
        self.assertEqual(subprocess.check_output([binary, "--version"], text=True).strip(), f"rulescan {TAG[1:]}")
        self.assertTrue(binary.stat().st_mode & 0o111)
        self.assertTrue((self.bin / "rulescan-notices.txt").read_text())
        binary.write_bytes(b"previous executable")
        self.install()
        self.assertEqual(subprocess.check_output([binary, "--version"], text=True).strip(), f"rulescan {TAG[1:]}")

    def test_latest_is_resolved_once_before_tag_specific_downloads(self):
        self.env.pop("RULESCAN_VERSION")
        self.install()
        urls = Path(self.env["CURL_LOG"]).read_text().splitlines()
        self.assertEqual(urls[0], "https://github.com/amruthpillai/rulescan/releases/latest")
        self.assertEqual(len(urls), 3)
        self.assertTrue(all(f"/releases/download/{TAG}/" in url for url in urls[1:]))

    def test_corrupt_and_duplicate_checksums_preserve_existing_binary(self):
        self.bin.mkdir(parents=True)
        binary = self.bin / "rulescan"
        binary.write_bytes(b"do not replace")
        for checksums in [f"{'0' * 64}  {self.asset}\n", f"{self.hash}  {self.asset}\ninvalid  {self.asset}\n"]:
            with self.subTest(checksums=checksums[:10]):
                (self.release / "SHA256SUMS").write_text(checksums)
                self.install(success=False)
                self.assertEqual(binary.read_bytes(), b"do not replace")
                self.assertFalse((self.bin / "rulescan-notices.txt").exists())

    def test_verified_archive_rejects_links_duplicates_and_traversal(self):
        for special in ["README.md", "rulescan", "../escape"]:
            with self.subTest(entry=special):
                self.make_archive(special)
                self.checksum()
                result = self.install(success=False)
                self.assertIn("Archive", result.stderr)
                self.assertFalse((self.bin / "rulescan").exists())
                self.assertFalse((self.root / "escape").exists())

    def test_existing_link_or_directory_is_never_replaced(self):
        self.bin.mkdir(parents=True)
        victim = self.root / "victim"
        victim.write_bytes(b"unrelated")
        binary = self.bin / "rulescan"
        binary.symlink_to(victim)
        self.install(success=False)
        self.assertTrue(binary.is_symlink())
        self.assertEqual(victim.read_bytes(), b"unrelated")
        binary.unlink()
        binary.mkdir()
        self.install(success=False)
        self.assertTrue(binary.is_dir())

    def test_unsupported_platform_and_invalid_tag_fail_before_download(self):
        self.env["FIXTURE_ARCH"] = "i686"
        self.install(success=False)
        self.assertFalse(Path(self.env["CURL_LOG"]).exists())
        self.env["FIXTURE_ARCH"] = HOST_ARCH
        self.env["RULESCAN_VERSION"] = "v0.1.0/../../other"
        self.install(success=False)
        self.assertFalse(Path(self.env["CURL_LOG"]).exists())


if __name__ == "__main__":
    unittest.main(argv=[sys.argv[0], *unittest_arguments], verbosity=2)
