import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "prepare-metal-toolchain.sh"


@unittest.skipIf(os.name == "nt", "Tests use POSIX command stubs for the macOS runner")
class MetalToolchainTests(unittest.TestCase):
    def run_preparation(self, *, available_probe=1, successful_download=1, available_after_download=0):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            state_path = root / "state.json"
            state_path.write_text(json.dumps({"calls": [], "downloads": 0, "probes": 0}))
            stub = f"#!{sys.executable}\n" + textwrap.dedent("""\
                import json
                import os
                from pathlib import Path
                import sys

                path = Path(os.environ["BEACON_METAL_TEST_STATE"])
                state = json.loads(path.read_text())
                command = Path(sys.argv[0]).name
                args = sys.argv[1:]
                state["calls"].append([command, *args])
                status = 0
                if command == "xcrun" and args == ["--sdk", "macosx", "metal", "--version"]:
                    state["probes"] += 1
                    ready = (state["probes"] >= int(os.environ["BEACON_METAL_TEST_PROBE"])
                             and state["downloads"] >= int(os.environ["BEACON_METAL_TEST_DOWNLOAD"]))
                    status = 0 if ready else 1
                    print("Apple metal version test" if ready else "missing Metal Toolchain")
                elif command == "xcodebuild" and args == ["-downloadComponent", "MetalToolchain"]:
                    state["downloads"] += 1
                    status = 0 if state["downloads"] >= int(os.environ["BEACON_METAL_TEST_SUCCESS"]) else 1
                    print("Done downloading" if status == 0 else "Download failed")
                elif command == "xcodebuild" and args == ["-showComponent", "MetalToolchain"]:
                    print("Component diagnostics")
                elif command == "xcrun" and args == ["--kill-cache"]:
                    pass
                elif command == "sleep" and args == ["5"]:
                    pass
                else:
                    raise RuntimeError(f"Unexpected command: {command} {args}")
                path.write_text(json.dumps(state))
                sys.exit(status)
                """)
            for name in ("xcrun", "xcodebuild", "sleep"):
                command = root / name
                command.write_text(stub)
                command.chmod(0o755)
            environment = dict(
                os.environ,
                PATH=str(root) + os.pathsep + os.environ["PATH"],
                BEACON_METAL_TEST_STATE=str(state_path),
                BEACON_METAL_TEST_PROBE=str(available_probe),
                BEACON_METAL_TEST_SUCCESS=str(successful_download),
                BEACON_METAL_TEST_DOWNLOAD=str(available_after_download),
            )
            result = subprocess.run(
                ["bash", str(SCRIPT)], env=environment, capture_output=True, text=True, timeout=15,
            )
            return result, json.loads(state_path.read_text())

    def test_available_compiler_does_not_download_or_wait(self):
        result, state = self.run_preparation()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(state["calls"], [["xcrun", "--sdk", "macosx", "metal", "--version"]])

    def test_download_completion_waits_for_the_compiler_to_become_usable(self):
        result, state = self.run_preparation(available_probe=4)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(state["downloads"], 1)
        self.assertEqual(state["probes"], 4)
        self.assertEqual(state["calls"].count(["sleep", "5"]), 2)
        self.assertLess(
            state["calls"].index(["xcrun", "--kill-cache"]),
            state["calls"].index(["sleep", "5"]),
        )

    def test_successful_but_unusable_download_is_retried(self):
        result, state = self.run_preparation(available_after_download=2)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(state["downloads"], 2)

    def test_failed_download_is_retried(self):
        result, state = self.run_preparation(successful_download=2, available_after_download=2)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(state["downloads"], 2)
        self.assertIn("::warning::Metal toolchain download failed", result.stdout)

    def test_permanently_missing_compiler_fails_with_diagnostics_and_a_retry_limit(self):
        result, state = self.run_preparation(available_probe=1000)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(state["downloads"], 3)
        self.assertEqual(state["probes"], 19)
        self.assertEqual(state["calls"].count(["sleep", "5"]), 15)
        self.assertIn(["xcodebuild", "-showComponent", "MetalToolchain"], state["calls"])
        self.assertIn("::error::Metal compiler is still unavailable", result.stdout)


if __name__ == "__main__":
    unittest.main()
