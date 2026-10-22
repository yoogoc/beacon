import contextlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


SCRIPT = Path(__file__).resolve().parents[1] / "retry-macos-packaging.py"
spec = importlib.util.spec_from_file_location("macos_packaging", SCRIPT)
packaging = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packaging)


class MacOSPackagingTests(unittest.TestCase):
    def run_packager(self, outcomes):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            state = root / "state.json"
            state.write_text(json.dumps({"calls": [], "outcomes": outcomes}))
            program = root / "fake packager.py"
            program.write_text(
                "import json, pathlib, sys\n"
                "path = pathlib.Path(sys.argv[1])\n"
                "state = json.loads(path.read_text())\n"
                "index = len(state['calls'])\n"
                "state['calls'].append(sys.argv[2:])\n"
                "path.write_text(json.dumps(state))\n"
                "code, text = state['outcomes'][min(index, len(state['outcomes']) - 1)]\n"
                "print(text, file=sys.stderr)\n"
                "sys.exit(code)\n"
            )
            output = io.StringIO()
            with contextlib.redirect_stdout(output), mock.patch.object(packaging.time, "sleep") as sleep:
                code = packaging.package([sys.executable, str(program), str(state), "argument with spaces"])
            return code, json.loads(state.read_text())["calls"], output.getvalue(), sleep.call_args_list

    def test_success_runs_once_and_preserves_arguments(self):
        code, calls, output, sleeps = self.run_packager([(0, "Packaged Beacon.dmg")])
        self.assertEqual(code, 0)
        self.assertEqual(calls, [["argument with spaces"]])
        self.assertIn("Packaged Beacon.dmg", output)
        self.assertEqual(sleeps, [])

    def test_busy_image_creation_retries_and_keeps_both_logs(self):
        code, calls, output, sleeps = self.run_packager([
            (1, "hdiutil: create failed - Resource busy"),
            (0, "Packaged Beacon.dmg"),
        ])
        self.assertEqual(code, 0)
        self.assertEqual(len(calls), 2)
        self.assertEqual(sleeps, [mock.call(5)])
        self.assertIn("hdiutil: create failed", output)
        self.assertIn("Packaged Beacon.dmg", output)

    def test_persistent_busy_error_keeps_failure_and_stops_after_three_attempts(self):
        code, calls, output, sleeps = self.run_packager([(1, "\x1b[31mhdiutil: create failed - Resource busy\x1b[0m")])
        self.assertEqual(code, 1)
        self.assertEqual(len(calls), 3)
        self.assertEqual(sleeps, [mock.call(5), mock.call(10)])
        self.assertIn("::error::DMG creation is still busy", output)

    def test_other_packaging_errors_fail_without_retry(self):
        for message in (
            "codesign failed: Resource busy",
            "hdiutil: create failed - Permission denied",
            "hdiutil: detach failed - Resource busy",
            "Signing key is invalid",
        ):
            with self.subTest(message=message):
                code, calls, output, sleeps = self.run_packager([(7, message)])
                self.assertEqual(code, 7)
                self.assertEqual(len(calls), 1)
                self.assertEqual(sleeps, [])
                self.assertIn(message, output)

    def test_success_with_a_busy_message_does_not_retry(self):
        code, calls, _, sleeps = self.run_packager([(0, "hdiutil: create failed - Resource busy\nRecovered")])
        self.assertEqual(code, 0)
        self.assertEqual(len(calls), 1)
        self.assertEqual(sleeps, [])

    def test_missing_command_is_an_error(self):
        result = subprocess.run([sys.executable, "-B", str(SCRIPT), "--"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("supply the packaging command", result.stderr)

    def test_cli_executes_the_supplied_command(self):
        result = subprocess.run([sys.executable, "-B", str(SCRIPT), "--", sys.executable, "-c", "print('packaged')"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "packaged\n")


if __name__ == "__main__":
    unittest.main()
