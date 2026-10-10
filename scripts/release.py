#!/usr/bin/env python3
"""Prepare release versions without changing dependency resolutions (Python 3.11+)."""

import argparse
import json
import os
from pathlib import Path
import plistlib
import re
import tomllib


VERSION = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-dev\.(0|[1-9][0-9]*))?")


def validate_version(version):
    if not VERSION.fullmatch(version):
        raise ValueError("version must be X.Y.Z or X.Y.Z-dev.N, without leading zeroes")
    return version


def workspace_version(root):
    manifest = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    return validate_version(manifest["workspace"]["package"]["version"])


def set_version(root, version, build_number=0):
    """Update inherited workspace versions and macOS metadata, preserving dependencies."""
    validate_version(version)
    if not 0 <= build_number < 99_990_000:
        raise ValueError("build number is outside the macOS bundle version range")
    manifest_path = root / "Cargo.toml"
    manifest_text = manifest_path.read_text(encoding="utf-8")
    manifest = tomllib.loads(manifest_text)
    old_version = workspace_version(root)
    members = set()
    for pattern in manifest["workspace"]["members"]:
        for member in root.glob(pattern):
            package = tomllib.loads((member / "Cargo.toml").read_text(encoding="utf-8"))["package"]
            if package.get("version") == {"workspace": True}:
                members.add(package["name"])
    if not members:
        raise ValueError("no packages inherit the workspace version")

    lock_path = root / "Cargo.lock"
    lock_text = lock_path.read_text(encoding="utf-8")
    blocks = re.split(r"(?m)(?=^\[\[package\]\]$)", lock_text)
    found = set()
    for index, block in enumerate(blocks):
        if not block.startswith("[[package]]"):
            continue
        package = tomllib.loads(block)["package"][0]
        if package["name"] not in members or "source" in package:
            continue
        if package["name"] in found or package["version"] != old_version:
            raise ValueError(f"inconsistent lockfile version for {package['name']}")
        found.add(package["name"])
        blocks[index], count = re.subn(
            r'(?m)^version = "[^"\n]+"$', f'version = "{version}"', block, count=1
        )
        if count != 1:
            raise ValueError(f"cannot update lockfile version for {package['name']}")
    if found != members:
        raise ValueError(f"workspace packages missing from Cargo.lock: {sorted(members - found)}")

    new_lock_text = "".join(blocks)
    # Cargo may qualify dependency references when packages share a name.
    for name in members:
        new_lock_text = new_lock_text.replace(f'"{name} {old_version}"', f'"{name} {version}"')
    section = re.search(r"(?ms)^\[workspace\.package\][^\n]*\n.*?(?=^\[|\Z)", manifest_text)
    if section is None:
        raise ValueError("cannot find workspace.package")
    new_section, count = re.subn(
        r'(?m)^(version\s*=\s*)"[^"\n]+"',
        lambda match: match[1] + f'"{version}"',
        section[0],
        count=1,
    )
    if count != 1:
        raise ValueError("cannot find workspace.package.version")
    new_manifest_text = manifest_text[:section.start()] + new_section + manifest_text[section.end():]
    # Validate both complete documents before either file is written.
    tomllib.loads(new_manifest_text)
    tomllib.loads(new_lock_text)
    # Apple requires numeric bundle versions. Keep the full SemVer in the app
    # and a custom plist key, with the CI run encoded as a 4.2.2-digit number.
    plist = plistlib.dumps({
        "CFBundleShortVersionString": version.split("-")[0],
        "CFBundleVersion": f"{1 + build_number // 10000}.{build_number // 100 % 100}.{build_number % 100}",
        "BeaconVersion": version,
    })
    plist_path = root / "assets" / "packaging" / "Info.plist"
    plist_path.parent.mkdir(parents=True, exist_ok=True)
    manifest_path.write_text(new_manifest_text, encoding="utf-8")
    lock_path.write_text(new_lock_text, encoding="utf-8")
    plist_path.write_bytes(plist)


def release_plan(version, event, ref, run_number):
    validate_version(version)
    if event == "push" and ref.startswith("refs/tags/"):
        tag = ref.removeprefix("refs/tags/")
        if not re.fullmatch(r"v(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", tag):
            raise ValueError("formal releases require a vX.Y.Z tag")
        if tag != f"v{version}":
            raise ValueError(f"tag {tag} does not match Cargo version {version}; run release.py set first")
        prerelease = False
    elif event == "workflow_dispatch":
        if not re.fullmatch(r"[1-9][0-9]*", run_number):
            raise ValueError("GITHUB_RUN_NUMBER must be a positive integer")
        base = version.split("-")[0]
        if "-" not in version:
            major, minor, patch = map(int, base.split("."))
            base = f"{major}.{minor}.{patch + 1}"
        version = f"{base}-dev.{run_number}"
        tag = f"v{version}"
        prerelease = True
    else:
        raise ValueError("only formal tags and manual packaging are supported")
    return {
        "version": version,
        "tag": tag,
        "name": f"Beacon {version}",
        "prerelease": str(prerelease).lower(),
        "make_latest": str(not prerelease).lower(),
        "publish": str(event == "push").lower(),
    }


def deb_config(root, target):
    """Use Debian's prerelease ordering while retaining SemVer in the binary."""
    if target not in ("x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"):
        raise ValueError("Debian packaging requires a supported Linux target")
    manifest_path = root / "crates/beacon/Cargo.toml"
    package = tomllib.loads(manifest_path.read_text(encoding="utf-8"))["package"]
    config = package["metadata"]["packager"]
    config["name"] = package["name"]
    config["version"] = workspace_version(root).replace("-dev.", "~dev.")
    config["target-triple"] = target
    output = str((root / "target" / target / "release").resolve())
    config["out-dir"] = output
    config["binaries-dir"] = output
    config["icons"] = [str((manifest_path.parent / icon).resolve()) for icon in config["icons"]]
    macos = config.get("macos", {})
    if "info-plist-path" in macos:
        macos["info-plist-path"] = str((manifest_path.parent / macos["info-plist-path"]).resolve())
    return config


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("set", help="set the next development target or formal release version").add_argument("version")
    commands.add_parser("prepare", help="generate and apply the version for a GitHub Actions run")
    deb = commands.add_parser("deb-config", help="write a Debian-specific cargo-packager configuration")
    deb.add_argument("--target", required=True)
    deb.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        if args.command == "set":
            set_version(args.root, args.version, int(os.environ.get("GITHUB_RUN_NUMBER", "0")))
            print(f"Workspace version: {args.version}")
        elif args.command == "deb-config":
            config = deb_config(args.root, args.target)
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(json.dumps(config, indent=2) + "\n", encoding="utf-8")
            print(f"Debian package version: {config['version']}")
        else:
            plan = release_plan(
                workspace_version(args.root),
                os.environ.get("GITHUB_EVENT_NAME", ""),
                os.environ.get("GITHUB_REF", ""),
                os.environ.get("GITHUB_RUN_NUMBER", ""),
            )
            set_version(args.root, plan["version"], int(os.environ.get("GITHUB_RUN_NUMBER", "0")))
            if output := os.environ.get("GITHUB_OUTPUT"):
                with Path(output).open("a", encoding="utf-8") as file:
                    for key, value in plan.items():
                        file.write(f"{key}={value}\n")
            print(json.dumps(plan, indent=2))
    except (ValueError, KeyError, OSError) as error:
        parser.exit(1, f"release: {error}\n")


if __name__ == "__main__":
    main()
