import hashlib
import importlib.util
import json
from pathlib import Path
import stat
import tarfile
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("package_release", ROOT / "scripts/package-release.py")
package_release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package_release)


class ReleaseArchiveTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.binary = self.directory / "built-command"
        self.binary.write_bytes(b"binary fixture\x00with exact bytes\n")

    def build(self, target, output="dist", **overrides):
        arguments = {"target": target, "version": "0.1.0", "commit": "a" * 40,
                     "binary": self.binary, "output_dir": self.directory / output}
        arguments.update(overrides)
        return package_release.package(**arguments)

    def assert_manifest(self, manifest, target, binary):
        self.assertEqual(manifest["version"], "0.1.0")
        self.assertEqual(manifest["target"], target)
        self.assertEqual(manifest["commit"], "a" * 40)
        self.assertEqual(manifest["binary"], binary)
        self.assertEqual(manifest["binary_sha256"], hashlib.sha256(self.binary.read_bytes()).hexdigest())

    def test_unix_archive_is_flat_regular_executable_and_keeps_notices(self):
        target = "aarch64-apple-darwin"
        path = self.build(target)
        self.assertEqual(path.name, f"rulescan-v0.1.0-{target}.tar.gz")
        with tarfile.open(path) as archive:
            self.assertEqual(archive.getnames(), ["rulescan", "README.md", "THIRD_PARTY_NOTICES.txt", "manifest.json"])
            self.assertTrue(all(member.isfile() for member in archive.getmembers()))
            self.assertEqual(archive.getmember("rulescan").mode, 0o755)
            self.assertEqual(archive.extractfile("rulescan").read(), self.binary.read_bytes())
            self.assertEqual(archive.extractfile("THIRD_PARTY_NOTICES.txt").read(), (ROOT / "docs/third-party-notices.txt").read_bytes())
            self.assert_manifest(json.load(archive.extractfile("manifest.json")), target, "rulescan")

    def test_windows_zip_marks_only_flat_regular_entries(self):
        target = "x86_64-pc-windows-msvc"
        path = self.build(target)
        with zipfile.ZipFile(path) as archive:
            self.assertEqual(archive.namelist(), ["rulescan.exe", "README.md", "THIRD_PARTY_NOTICES.txt", "manifest.json"])
            for entry in archive.infolist():
                self.assertTrue(stat.S_ISREG(entry.external_attr >> 16))
                self.assertFalse(entry.is_dir())
            self.assertEqual(archive.read("rulescan.exe"), self.binary.read_bytes())
            self.assert_manifest(json.loads(archive.read("manifest.json")), target, "rulescan.exe")

    def test_archive_bytes_do_not_depend_on_output_path_or_wall_clock(self):
        for target in ["x86_64-unknown-linux-musl", "x86_64-pc-windows-msvc"]:
            first = self.build(target, "first-" + target).read_bytes()
            second = self.build(target, "second-" + target).read_bytes()
            self.assertEqual(first, second)

    def test_existing_assets_are_not_overwritten(self):
        path = self.build("x86_64-apple-darwin")
        before = path.read_bytes()
        with self.assertRaises(FileExistsError):
            self.build("x86_64-apple-darwin")
        self.assertEqual(path.read_bytes(), before)

    def test_invalid_metadata_and_non_files_create_no_archive(self):
        for arguments in [{"version": "../bad"}, {"commit": "short"}, {"target": "unsupported"},
                          {"binary": self.directory}]:
            with self.subTest(arguments=arguments), self.assertRaises(ValueError):
                self.build(arguments.get("target", "aarch64-unknown-linux-musl"),
                           **{key: value for key, value in arguments.items() if key != "target"})
        self.assertFalse((self.directory / "dist").exists())


if __name__ == "__main__":
    unittest.main()
