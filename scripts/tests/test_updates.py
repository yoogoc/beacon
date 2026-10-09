import base64
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("updates", Path(__file__).resolve().parents[1] / "updates.py")
updates = importlib.util.module_from_spec(spec)
spec.loader.exec_module(updates)


class UpdateManifestTests(unittest.TestCase):
    def fixture(self, root, name="beacon-linux-amd64", filename="Beacon_0.2.8_amd64.AppImage"):
        directory = root / name
        directory.mkdir(exist_ok=True)
        package = directory / filename
        package.write_bytes(b"package")
        package.with_name(filename + ".sig").write_text(base64.b64encode(b"untrusted comment: fixture\nsignature").decode())
        return package

    def test_complete_release_requires_every_platform(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, "missing platform"):
                updates.manifest(Path(directory), "yoogoc/beacon", "0.2.8")

    def test_partial_development_manifest_keeps_formats_architecture_and_signature(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            package = self.fixture(root)
            self.fixture(root, filename="Beacon_0.2.8_amd64.deb")
            result = updates.manifest(root, "yoogoc/beacon", "0.2.8", False)
            image = result["platforms"]["linux-x86_64-appimage"]
            self.assertEqual(image["size"], 7)
            self.assertEqual(image["signature"], package.with_name(package.name + ".sig").read_text())
            self.assertEqual(image["url"], "https://github.com/yoogoc/beacon/releases/download/v0.2.8/Beacon_0.2.8_amd64.AppImage")
            self.assertEqual(result["platforms"]["linux-x86_64-deb"]["format"], "deb")

    def test_missing_or_invalid_signatures_prevent_publication(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            package = self.fixture(root)
            signature = package.with_name(package.name + ".sig")
            signature.unlink()
            with self.assertRaises(FileNotFoundError):
                updates.manifest(root, "yoogoc/beacon", "0.2.8", False)
            signature.write_text(base64.b64encode(b"not a signature").decode())
            with self.assertRaisesRegex(ValueError, "invalid cargo-packager"):
                updates.manifest(root, "yoogoc/beacon", "0.2.8", False)

    def test_ambiguous_packages_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.fixture(root)
            self.fixture(root, filename="Other.AppImage")
            with self.assertRaisesRegex(ValueError, "expected one"):
                updates.manifest(root, "yoogoc/beacon", "0.2.8", False)

    def test_macos_archives_are_unique_without_changing_signed_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for platform in ("macos-arm64", "macos-amd64"):
                folder = root / platform
                folder.mkdir()
                (folder / "Beacon.app.tar.gz").write_bytes(b"signed archive")
                (folder / "Beacon.app.tar.gz.sig").write_text("original signature")
                updates.stage_mac_archive(folder, platform, "0.2.8")
                archive = folder / f"Beacon_0.2.8_{platform}.app.tar.gz"
                self.assertEqual(archive.read_bytes(), b"signed archive")
                self.assertEqual(archive.with_name(archive.name + ".sig").read_text(), "original signature")
                self.assertFalse((folder / "Beacon.app.tar.gz").exists())

    def test_complete_signed_manifest_has_all_eight_installation_formats(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name, (_, formats) in updates.TARGETS.items():
                for fmt, pattern in formats:
                    filename = pattern.replace("*", name)
                    self.fixture(root, "beacon-" + name, filename)
            result = updates.manifest(root, "yoogoc/beacon", "0.2.8")
            self.assertEqual(len(result["platforms"]), 8)
