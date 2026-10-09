"""Signed fixtures must survive Git's Windows line-ending conversion unchanged."""
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
PACKAGE = Path("crates/beacon-updater/tests/fixtures/package")


class SignedFixtureCheckoutTests(unittest.TestCase):
    def test_windows_checkout_preserves_the_signed_payload(self):
        original = (ROOT / PACKAGE).read_bytes()
        with tempfile.TemporaryDirectory() as directory:
            checkout = Path(directory)
            payload = checkout / PACKAGE
            payload.parent.mkdir(parents=True)
            payload.write_bytes(original)
            shutil.copyfile(ROOT / ".gitattributes", checkout / ".gitattributes")

            def git(*args):
                subprocess.run(
                    ["git", *args], cwd=checkout, check=True, capture_output=True
                )

            git("init", "--quiet")
            git("config", "core.autocrlf", "true")
            git("config", "core.eol", "crlf")
            git("add", ".gitattributes", PACKAGE.as_posix())
            payload.unlink()
            git("checkout-index", "--force", "--", PACKAGE.as_posix())
            self.assertEqual(payload.read_bytes(), original)


if __name__ == "__main__":
    unittest.main()
