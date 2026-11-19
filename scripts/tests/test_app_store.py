import importlib.util
from datetime import datetime, timedelta, timezone
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import os

spec = importlib.util.spec_from_file_location("app_store", Path(__file__).resolve().parents[1] / "app-store.py")
app_store = importlib.util.module_from_spec(spec)
spec.loader.exec_module(app_store)


class AppStoreTests(unittest.TestCase):
    def setUp(self):
        self.team = "ABCDEFGHIJ"
        self.environment = {
            "APPLE_TEAM_ID": self.team,
            "APP_STORE_APP_IDENTITY": f"Apple Distribution: Example ({self.team})",
            "APP_STORE_INSTALLER_IDENTITY": f"3rd Party Mac Developer Installer: Example ({self.team})",
        }
        self.now = datetime(2026, 11, 18, tzinfo=timezone.utc)
        self.profile = {
            "ExpirationDate": self.now + timedelta(days=30),
            "TeamIdentifier": [self.team],
            "Entitlements": {
                "com.apple.application-identifier": f"{self.team}.dev.beacon.Beacon",
                "com.apple.developer.team-identifier": self.team,
            },
        }

    def test_distribution_identities_are_required_and_team_must_match(self):
        app_store.identities(self.environment)
        self.environment["APP_STORE_APP_IDENTITY"] = f"3rd Party Mac Developer Application: Example ({self.team})"
        app_store.identities(self.environment)
        for name, value in (
            ("APP_STORE_APP_IDENTITY", f"Developer ID Application: Example ({self.team})"),
            ("APP_STORE_APP_IDENTITY", "Apple Distribution: Example (OTHERTTEAM)"),
            ("APP_STORE_INSTALLER_IDENTITY", f"Developer ID Installer: Example ({self.team})"),
            ("APPLE_TEAM_ID", ""),
        ):
            invalid = dict(self.environment, **{name: value})
            with self.subTest(name=name, value=value), self.assertRaises(ValueError):
                app_store.identities(invalid)

    def test_profile_matches_bundle_and_team(self):
        result = app_store.provisioning_entitlements(self.profile, self.team, "dev.beacon.Beacon", self.now)
        self.assertEqual(result, self.profile["Entitlements"])
        for team, identifier in (("OTHERTTEAM", "dev.beacon.Beacon"), (self.team, "other.bundle")):
            with self.subTest(team=team, identifier=identifier), self.assertRaises(ValueError):
                app_store.provisioning_entitlements(self.profile, team, identifier, self.now)

    def test_store_identifier_is_configurable_without_changing_standalone_identifier(self):
        self.assertEqual(app_store.bundle_identifier("dev.beacon.Beacon", {}), "dev.beacon.Beacon")
        self.assertEqual(app_store.bundle_identifier("dev.beacon.Beacon", {"APP_STORE_BUNDLE_ID": "top.yoogo.Beacon"}), "top.yoogo.Beacon")
        for identifier in ("invalid", "top.yoogo/Beacon", "top..Beacon", "top.yoogo.Beacon\n"):
            with self.subTest(identifier=identifier), self.assertRaises(ValueError):
                app_store.bundle_identifier("dev.beacon.Beacon", {"APP_STORE_BUNDLE_ID": identifier})
        with self.assertRaisesRegex(ValueError, "numeric"):
            app_store.identities(dict(self.environment, APP_STORE_APP_ID="account@example.com"))

    def test_expired_development_and_developer_id_profiles_are_rejected(self):
        for change in (
            {"ExpirationDate": self.now - timedelta(seconds=1)},
            {"ProvisionedDevices": ["device-id"]},
            {"ProvisionsAllDevices": True},
            {"Entitlements": dict(self.profile["Entitlements"], **{"get-task-allow": True})},
            {"Entitlements": dict(self.profile["Entitlements"], **{"com.apple.security.get-task-allow": True})},
        ):
            with self.subTest(change=change), self.assertRaises(ValueError):
                app_store.provisioning_entitlements(dict(self.profile, **change), self.team, "dev.beacon.Beacon", self.now)

    def test_store_config_uses_formal_version_and_separate_output(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "crates/beacon/Cargo.toml"
            manifest.parent.mkdir(parents=True)
            manifest.write_text('[package]\nname = "beacon"\n[package.metadata.packager]\nidentifier = "dev.beacon.Beacon"\ncopyright = "© 2026 Beacon Team"\nicons = ["../../assets/icon.icns"]\n[package.metadata.packager.macos]\ninfo-plist-path = "../../assets/packaging/Info.plist"\nsigning-identity = "Developer ID Application: Example"\n', encoding="utf-8")
            workspace = root / "Cargo.toml"
            workspace.write_text('[workspace.package]\nversion = "1.4.60"\n')
            output = root / "target/app-store"
            config = app_store.packager_config(root, output)
            self.assertEqual(config["version"], "1.4.60")
            self.assertEqual(config["binaries-dir"], str(output))
            self.assertEqual(config["copyright"], "© 2026 Beacon Team")
            self.assertNotIn("signing-identity", config["macos"])
            self.assertEqual(config["icons"], [str((root / "assets/icon.icns").resolve())])
            with patch.dict(os.environ, {"APP_STORE_BUNDLE_ID": "top.yoogo.Beacon"}):
                store = app_store.packager_config(root, output)
                self.assertEqual(store["identifier"], "top.yoogo.Beacon")
            self.assertIn('identifier = "dev.beacon.Beacon"', manifest.read_text())
            workspace.write_text('[workspace.package]\nversion = "1.4.60-dev.123"\n')
            with self.assertRaisesRegex(ValueError, "formal"):
                app_store.packager_config(root, output)


if __name__ == "__main__":
    unittest.main()
