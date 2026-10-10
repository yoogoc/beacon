import importlib.util
import json
import os
from pathlib import Path
import tempfile
import tomllib
import unittest
import plistlib
import shutil
import subprocess
import sys


spec = importlib.util.spec_from_file_location("release", Path(__file__).resolve().parents[1] / "release.py")
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReleasePlanTests(unittest.TestCase):
    def test_manual_builds_are_unique_ordered_and_not_latest(self):
        first = release.release_plan("0.2.0-dev.0", "workflow_dispatch", "refs/heads/main", "101")
        second = release.release_plan("0.2.0-dev.0", "workflow_dispatch", "refs/heads/main", "102")
        self.assertEqual(first["version"], "0.2.0-dev.101")
        self.assertEqual(second["tag"], "v0.2.0-dev.102")
        self.assertEqual(first["prerelease"], "true")
        self.assertEqual(first["make_latest"], "false")
        self.assertEqual(first["publish"], "false")
        self.assertEqual(first, release.release_plan("0.2.0-dev.0", "workflow_dispatch", "refs/heads/main", "101"))

    def test_manual_build_of_release_commit_uses_next_patch(self):
        plan = release.release_plan("0.2.0", "workflow_dispatch", "refs/heads/main", "103")
        self.assertEqual(plan["version"], "0.2.1-dev.103")
        self.assertEqual(plan["prerelease"], "true")

    def test_main_pushes_are_rejected(self):
        for version in ("0.2.0", "0.2.0-dev.0"):
            with self.subTest(version=version), self.assertRaisesRegex(ValueError, "only formal tags and manual packaging"):
                release.release_plan(version, "push", "refs/heads/main", "103")

    def test_formal_tag_is_latest_and_preserves_version(self):
        plan = release.release_plan("0.2.0", "push", "refs/tags/v0.2.0", "104")
        self.assertEqual(plan["version"], "0.2.0")
        self.assertEqual(plan["prerelease"], "false")
        self.assertEqual(plan["make_latest"], "true")
        self.assertEqual(plan["publish"], "true")

    def test_manual_runs_only_produce_artifacts_even_on_a_tag(self):
        for ref in ("refs/heads/feature", "refs/tags/v0.2.0"):
            plan = release.release_plan("0.2.0", "workflow_dispatch", ref, "105")
            self.assertEqual(plan["version"], "0.2.1-dev.105")
            self.assertEqual(plan["publish"], "false")

    def test_mismatched_or_non_formal_tags_are_rejected(self):
        for version, tag in (
            ("0.2.0", "v0.3.0"),
            ("0.2.0-dev.0", "v0.2.0"),
            ("0.2.0-dev.101", "v0.2.0-dev.101"),
            ("0.2.0", "v0.02.0"),
        ):
            with self.subTest(version=version, tag=tag), self.assertRaises(ValueError):
                release.release_plan(version, "push", f"refs/tags/{tag}", "106")

    def test_other_events_and_invalid_numbers_are_rejected(self):
        for event, ref, number in (
            ("push", "refs/heads/feature", "107"),
            ("pull_request", "refs/pull/1/merge", "107"),
            ("workflow_dispatch", "refs/heads/main", ""),
            ("workflow_dispatch", "refs/heads/main", "0"),
            ("workflow_dispatch", "refs/heads/main", "001"),
        ):
            with self.subTest(event=event, ref=ref, number=number), self.assertRaises(ValueError):
                release.release_plan("0.2.0", event, ref, number)


class WorkspaceVersionTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "Cargo.toml").write_text(
            '# Keep this comment.\n[workspace]\nmembers = ["crates/*"]\n\n'
            '[workspace.package]\nversion = "0.1.0"\nedition = "2024"\n', encoding="utf-8"
        )
        for name in ("beacon", "beacon-ui"):
            member = self.root / "crates" / name
            member.mkdir(parents=True)
            (member / "Cargo.toml").write_text(
                f'[package]\nname = "{name}"\nversion.workspace = true\n', encoding="utf-8"
            )
        with (self.root / "crates/beacon/Cargo.toml").open("a", encoding="utf-8") as file:
            file.write(
                '[package.metadata.packager]\nproduct-name = "Beacon"\nicons = ["../../assets/icon.png"]\n'
                '[package.metadata.packager.macos]\ninfo-plist-path = "../../assets/packaging/Info.plist"\n'
            )
        self.dependency = (
            '[[package]]\nname = "dependency"\nversion = "0.1.0"\n'
            'source = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "unchanged"\n'
        )
        (self.root / "Cargo.lock").write_text(
            '# Lockfile comment.\nversion = 4\n\n'
            '[[package]]\nname = "beacon"\nversion = "0.1.0"\n'
            'dependencies = ["beacon-ui 0.1.0"]\n\n'
            '[[package]]\nname = "beacon-ui"\nversion = "0.1.0"\n\n' + self.dependency,
            encoding="utf-8",
        )
        release.set_version(self.root, "0.1.0")

    def test_versions_and_qualified_references_change_without_dependency_updates(self):
        release.set_version(self.root, "0.2.0-dev.101")
        manifest = (self.root / "Cargo.toml").read_text(encoding="utf-8")
        lock = (self.root / "Cargo.lock").read_text(encoding="utf-8")
        self.assertTrue(manifest.startswith("# Keep this comment."))
        self.assertEqual(release.workspace_version(self.root), "0.2.0-dev.101")
        self.assertIn(self.dependency, lock)
        packages = tomllib.loads(lock)["package"]
        self.assertEqual([p["version"] for p in packages[:2]], ["0.2.0-dev.101"] * 2)
        self.assertEqual(packages[0]["dependencies"], ["beacon-ui 0.2.0-dev.101"])
        release.set_version(self.root, "0.2.0")
        self.assertEqual(release.workspace_version(self.root), "0.2.0")

    def test_same_version_is_idempotent(self):
        before = self.contents()
        release.set_version(self.root, "0.1.0")
        self.assertEqual(before, self.contents())

    def test_invalid_versions_leave_files_unchanged(self):
        for version in ("v0.2.0", "0.02.0", "0.2", "0.2.0-dev.01", "0.2.0-dev.1２", "0.2.0\nmalicious"):
            before = self.contents()
            with self.subTest(version=version), self.assertRaises(ValueError):
                release.set_version(self.root, version)
            self.assertEqual(before, self.contents())

    def test_macos_versions_are_numeric_and_builds_remain_ordered(self):
        for number, expected in ((101, "1.1.1"), (9999, "1.99.99"), (10000, "2.0.0")):
            release.set_version(self.root, f"0.2.0-dev.{number}", number)
            plist = plistlib.loads((self.root / "assets/packaging/Info.plist").read_bytes())
            self.assertEqual(plist["CFBundleShortVersionString"], "0.2.0")
            self.assertEqual(plist["CFBundleVersion"], expected)
            self.assertEqual(plist["BeaconVersion"], f"0.2.0-dev.{number}")
        before = self.contents()
        with self.assertRaises(ValueError):
            release.set_version(self.root, "0.2.0", 99_990_000)
        self.assertEqual(before, self.contents())

    def test_debian_config_preserves_metadata_and_places_prerelease_before_formal(self):
        target = "x86_64-unknown-linux-gnu"
        release.set_version(self.root, "0.2.0-dev.101")
        config = release.deb_config(self.root, target)
        self.assertEqual(config["version"], "0.2.0~dev.101")
        self.assertEqual(config["product-name"], "Beacon")
        self.assertEqual(config["target-triple"], target)
        self.assertEqual(config["binaries-dir"], str((self.root / "target" / target / "release").resolve()))
        self.assertEqual(config["icons"], [str((self.root / "assets/icon.png").resolve())])
        self.assertEqual(config["macos"]["info-plist-path"], str((self.root / "assets/packaging/Info.plist").resolve()))
        if shutil.which("dpkg"):
            subprocess.run(["dpkg", "--compare-versions", config["version"], "lt", "0.2.0"], check=True)
            subprocess.run(["dpkg", "--compare-versions", "0.2.0~dev.101", "lt", "0.2.0~dev.102"], check=True)
        release.set_version(self.root, "0.2.0")
        self.assertEqual(release.deb_config(self.root, target)["version"], "0.2.0")
        with self.assertRaises(ValueError):
            release.deb_config(self.root, "aarch64-apple-darwin")

    def test_cli_prepare_writes_outputs_and_invalid_tags_do_not_modify_files(self):
        environment = dict(os.environ, GITHUB_EVENT_NAME="workflow_dispatch", GITHUB_REF="refs/heads/main", GITHUB_RUN_NUMBER="101")
        output = self.root / "github-output"
        environment["GITHUB_OUTPUT"] = str(output)
        command = [sys.executable, str(spec.origin), "--root", str(self.root), "prepare"]
        result = subprocess.run(command, env=environment, capture_output=True, text=True, check=True)
        plan = json.loads(result.stdout)
        self.assertEqual(release.workspace_version(self.root), "0.1.1-dev.101")
        self.assertIn("make_latest=false\n", output.read_text())
        self.assertIn("publish=false\n", output.read_text())
        self.assertEqual(plan["tag"], "v0.1.1-dev.101")
        environment["GITHUB_EVENT_NAME"] = "push"
        environment["GITHUB_REF"] = "refs/tags/v0.1.0"
        before = self.contents()
        result = subprocess.run(command, env=environment, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not match", result.stderr)
        self.assertEqual(before, self.contents())

    def test_missing_or_inconsistent_lock_entries_leave_files_unchanged(self):
        lock_path = self.root / "Cargo.lock"
        original = lock_path.read_text(encoding="utf-8")
        for replacement in ("beacon-other", "beacon-ui"):
            lock_path.write_text(original.replace('name = "beacon"', f'name = "{replacement}"'), encoding="utf-8")
            before = self.contents()
            with self.subTest(replacement=replacement), self.assertRaises(ValueError):
                release.set_version(self.root, "0.2.0")
            self.assertEqual(before, self.contents())
        lock_path.write_text(original.replace('version = "0.1.0"', 'version = "0.1.1"', 1), encoding="utf-8")
        before = self.contents()
        with self.assertRaises(ValueError):
            release.set_version(self.root, "0.2.0")
        self.assertEqual(before, self.contents())

    def contents(self):
        return tuple((self.root / name).read_bytes() for name in ("Cargo.toml", "Cargo.lock", "assets/packaging/Info.plist"))


if __name__ == "__main__":
    unittest.main()
