#!/usr/bin/env python3
"""Assemble the signed updater manifest from completed packaging artifacts."""
import argparse
import base64
import json
from pathlib import Path
import re
from urllib.parse import quote

TARGETS = {
    "macos-arm64": ("macos-aarch64", [("app", "*.app.tar.gz")]),
    "macos-amd64": ("macos-x86_64", [("app", "*.app.tar.gz")]),
    "windows-arm64": ("windows-aarch64", [("nsis", "*-setup.exe")]),
    "windows-amd64": ("windows-x86_64", [("nsis", "*-setup.exe")]),
    "linux-arm64": ("linux-aarch64", [("appimage", "*.AppImage"), ("deb", "*.deb")]),
    "linux-amd64": ("linux-x86_64", [("appimage", "*.AppImage"), ("deb", "*.deb")]),
}


def manifest(directory, repository, version, require_all=True):
    if not re.fullmatch(r"[\w.-]+/[\w.-]+", repository):
        raise ValueError("invalid GitHub repository")
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:-dev\.\d+)?", version):
        raise ValueError("invalid update version")
    platforms = {}
    filenames = set()
    for name, (platform, formats) in TARGETS.items():
        folder = directory / f"beacon-{name}"
        if not folder.is_dir():
            if require_all:
                raise ValueError(f"missing platform artifacts: {name}")
            continue
        for fmt, pattern in formats:
            files = list(folder.glob(pattern))
            if len(files) != 1 or not files[0].is_file():
                raise ValueError(f"expected one {fmt} package for {name}, found {len(files)}")
            package = files[0]
            if package.name in filenames:
                raise ValueError("release asset filenames must be unique")
            filenames.add(package.name)
            signature_path = package.with_name(package.name + ".sig")
            # This is already base64 text written by cargo-packager. Do not
            # encode it again. Cryptographic verification is a separate step.
            signature = signature_path.read_text(encoding="utf-8").strip()
            decoded = base64.b64decode(signature, validate=True)
            if not decoded.startswith(b"untrusted comment:"):
                raise ValueError(f"invalid cargo-packager signature: {signature_path}")
            size = package.stat().st_size
            if not 0 < size <= 2 * 1024**3:
                raise ValueError(f"invalid package size: {package}")
            platforms[f"{platform}-{fmt}"] = {
                "url": f"https://github.com/{repository}/releases/download/v{version}/{quote(package.name, safe='')}",
                "signature": signature,
                "size": size,
                "format": fmt,
            }
    if not platforms:
        raise ValueError("no signed update packages found")
    return {"version": version, "platforms": platforms}


def stage_mac_archive(directory, platform, version, require_signed=True):
    if platform not in ("macos-arm64", "macos-amd64"):
        raise ValueError("macOS archive requires a macOS platform")
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:-dev\.\d+)?", version):
        raise ValueError("invalid update version")
    # .app is always named Beacon.app, so both architectures would otherwise
    # upload Beacon.app.tar.gz. Renaming preserves the signed archive bytes.
    archive = directory / "Beacon.app.tar.gz"
    signature = directory / "Beacon.app.tar.gz.sig"
    if not archive.is_file():
        if require_signed:
            raise ValueError("missing signed macOS application archive")
        return
    if not signature.is_file():
        raise ValueError("missing macOS archive signature")
    name = f"Beacon_{version}_{platform}.app.tar.gz"
    archive.replace(directory / name)
    signature.replace(directory / (name + ".sig"))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    feed = commands.add_parser("manifest")
    feed.add_argument("--artifacts", type=Path, required=True)
    feed.add_argument("--repository", required=True)
    feed.add_argument("--version", required=True)
    feed.add_argument("--output", type=Path, required=True)
    feed.add_argument("--allow-missing", action="store_true")
    stage = commands.add_parser("stage-macos")
    stage.add_argument("--directory", type=Path, required=True)
    stage.add_argument("--platform", required=True)
    stage.add_argument("--version", required=True)
    stage.add_argument("--unsigned", action="store_true")
    args = parser.parse_args()
    if args.command == "stage-macos":
        stage_mac_archive(args.directory, args.platform, args.version, not args.unsigned)
    else:
        args.output.write_text(json.dumps(manifest(args.artifacts, args.repository, args.version, not args.allow_missing), indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
