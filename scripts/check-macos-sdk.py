#!/usr/bin/env python3
"""Check the SDK and deployment target recorded in a macOS executable."""

import argparse
from pathlib import Path
import re
import subprocess


def version(value):
    parts = tuple(map(int, value.split(".")))
    return parts + (0,) * (3 - len(parts))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--sdk-version", required=True)
    parser.add_argument("--deployment-target", default="11.0")
    args = parser.parse_args()
    try:
        result = subprocess.run(
            ["xcrun", "vtool", "-show-build", str(args.binary.resolve())],
            capture_output=True, text=True, check=True,
        )
        print(result.stdout, end="")
        for field, expected in (("sdk", args.sdk_version), ("minos", args.deployment_target)):
            values = re.findall(rf"^\s*{field}\s+(\d+(?:\.\d+)*)\s*$", result.stdout, re.MULTILINE)
            if not values or any(version(value) != version(expected) for value in values):
                parser.exit(1, f"macOS {field}: expected {expected}, found {values or 'no build metadata'}\n")
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"macOS SDK verification failed: {error}\n")


if __name__ == "__main__":
    main()
