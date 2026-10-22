#!/usr/bin/env python3
"""Retry macOS packaging only when hdiutil cannot create an image due to EBUSY."""

import argparse
import re
import subprocess
import sys
import time


ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
BUSY_CREATE = re.compile(r"hdiutil:\s*create failed\s*-\s*Resource busy\b")
RETRY_DELAYS = (5, 10)


def package(command):
    for attempt in range(len(RETRY_DELAYS) + 1):
        try:
            process = subprocess.Popen(
                command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                text=True, encoding="utf-8", errors="replace",
            )
        except OSError as error:
            print(f"Could not start packaging: {error}", file=sys.stderr)
            return 1
        output = []
        with process:
            for line in process.stdout:
                output.append(line)
                print(line, end="", flush=True)
            code = process.wait()
        if code == 0 or not BUSY_CREATE.search(ANSI.sub("", "".join(output))):
            return code
        if attempt == len(RETRY_DELAYS):
            print("::error::DMG creation is still busy after 3 packaging attempts.", flush=True)
            return code
        delay = RETRY_DELAYS[attempt]
        print(
            f"::warning::hdiutil could not create the DMG (Resource busy). "
            f"Retrying packaging in {delay}s ({attempt + 2}/3).",
            flush=True,
        )
        time.sleep(delay)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        parser.error("supply the packaging command after --")
    return package(command)


if __name__ == "__main__":
    sys.exit(main())
