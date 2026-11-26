#!/usr/bin/env python3
"""Package the app-store feature's universal binary for App Store Connect."""
import argparse
import base64
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import plistlib
import re
import ssl
import subprocess
import tomllib


ROOT = Path(__file__).resolve().parents[1]


def run(*args, **kwargs):
    return subprocess.run([str(arg) for arg in args], check=True, **kwargs)


def identities(environment):
    names = ("APPLE_TEAM_ID", "APP_STORE_APP_IDENTITY", "APP_STORE_INSTALLER_IDENTITY")
    missing = [name for name in names if not environment.get(name)]
    if missing:
        raise ValueError("Missing App Store configuration: " + ", ".join(missing))
    team, app, installer = (environment[name] for name in names)
    if not re.fullmatch(r"[A-Z0-9]{10}", team):
        raise ValueError("APPLE_TEAM_ID must contain ten uppercase letters or digits")
    if not app.startswith(("Apple Distribution:", "3rd Party Mac Developer Application:")):
        raise ValueError("APP_STORE_APP_IDENTITY must be an App Store application identity")
    if not installer.startswith("3rd Party Mac Developer Installer:"):
        raise ValueError("APP_STORE_INSTALLER_IDENTITY must be a Mac Installer Distribution identity")
    if not all(name.endswith(f"({team})") for name in (app, installer)):
        raise ValueError("Both App Store identities must belong to APPLE_TEAM_ID")
    app_id = environment.get("APP_STORE_APP_ID", "")
    if app_id and not re.fullmatch(r"[1-9][0-9]*", app_id):
        raise ValueError("APP_STORE_APP_ID must be the numeric App Store Connect app ID")
    return team, app, installer


def bundle_identifier(default, environment):
    identifier = environment.get("APP_STORE_BUNDLE_ID") or default
    if len(identifier) > 255 or not re.fullmatch(r"[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+", identifier):
        raise ValueError("APP_STORE_BUNDLE_ID must be a valid reverse-DNS bundle identifier")
    return identifier


def load_provisioning_profile(path, environment):
    """Read an Apple-signed profile without exposing its contents in command logs."""
    if path is not None:
        contents = Path(path).read_bytes()
    else:
        encoded = environment.get("APP_STORE_PROVISIONING_PROFILE")
        if not encoded:
            raise ValueError("A Mac App Store provisioning profile is required for TestFlight; provide --profile or APP_STORE_PROVISIONING_PROFILE")
        try:
            contents = base64.b64decode(encoded, validate=True)
        except ValueError as error:
            raise ValueError("APP_STORE_PROVISIONING_PROFILE must contain valid Base64") from error
    decoded = run("security", "cms", "-D", input=contents, capture_output=True).stdout
    return contents, plistlib.loads(decoded)


def provisioning_entitlements(profile, team, identifier, now=None):
    """Require a current Mac App Store profile for every store build, including TestFlight."""
    if "OSX" not in profile.get("Platform", []):
        raise ValueError("The provisioning profile must target macOS")
    now = now or datetime.now(timezone.utc)
    expiry = profile.get("ExpirationDate")
    if not isinstance(expiry, datetime) or expiry.replace(tzinfo=timezone.utc) <= now:
        raise ValueError("The App Store provisioning profile is expired or has no expiry")
    entitlements = profile.get("Entitlements", {})
    app_id = f"{team}.{identifier}"
    if profile.get("TeamIdentifier") != [team] or entitlements.get("com.apple.application-identifier") != app_id:
        raise ValueError("The provisioning profile must match the exact team and bundle identifier")
    if entitlements.get("com.apple.developer.team-identifier") != team:
        raise ValueError("The provisioning profile team entitlement does not match")
    if profile.get("ProvisionedDevices") or profile.get("ProvisionsAllDevices") or entitlements.get("get-task-allow") or entitlements.get("com.apple.security.get-task-allow"):
        raise ValueError("Use a Mac App Store distribution profile, not a development or Developer ID profile")
    return {
        "com.apple.application-identifier": app_id,
        "com.apple.developer.team-identifier": team,
    }


def packager_config(root, output):
    manifest = root / "crates/beacon/Cargo.toml"
    package = tomllib.loads(manifest.read_text(encoding="utf-8"))["package"]
    config = package["metadata"]["packager"]
    config["identifier"] = bundle_identifier(config["identifier"], os.environ)
    version = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["package"]["version"]
    if not re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", version):
        raise ValueError("App Store Connect packaging requires a formal X.Y.Z version")
    config.update({
        "name": package["name"], "version": version,
        # Select the macOS packaging backend. The input executable contains both architectures.
        "target-triple": "aarch64-apple-darwin",
        "out-dir": str(output), "binaries-dir": str(output),
    })
    config["icons"] = [str((manifest.parent / icon).resolve()) for icon in config["icons"]]
    macos = config["macos"]
    macos.pop("signing-identity", None)
    macos.pop("entitlements", None)
    macos["info-plist-path"] = str((manifest.parent / macos["info-plist-path"]).resolve())
    # Signing follows profile embedding and uses the job's isolated keychain.
    return config


def package(root, output, keychain, profile):
    team, app_identity, installer_identity = identities(os.environ)
    config = packager_config(root, output)
    profile_contents, decoded_profile = load_provisioning_profile(profile, os.environ)
    profile_entitlements = provisioning_entitlements(decoded_profile, team, config["identifier"])
    certificate = run("security", "find-certificate", "-c", app_identity, "-p", keychain,
                      capture_output=True, text=True).stdout
    if ssl.PEM_cert_to_DER_cert(certificate) not in decoded_profile.get("DeveloperCertificates", []):
        raise ValueError("The provisioning profile does not authorize APP_STORE_APP_IDENTITY; regenerate it with the current distribution certificate")
    binary = output / "beacon"
    architectures = run("lipo", "-archs", binary, capture_output=True, text=True).stdout.split()
    if set(architectures) != {"arm64", "x86_64"}:
        raise ValueError("App Store binary must contain both arm64 and x86_64 architectures")
    config_path = output / "packager.json"
    config_path.write_text(json.dumps(config), encoding="utf-8")
    run("cargo", "packager", "--config", config_path, "--formats", "app", cwd=root)
    app = output / "Beacon.app"
    info = plistlib.loads((app / "Contents/Info.plist").read_bytes())
    if info.get("CFBundleIdentifier") != config["identifier"] or info.get("CFBundleShortVersionString") != config["version"]:
        raise ValueError("Packaged app identifier or version does not match the release")
    if info.get("NSHumanReadableCopyright") != config["copyright"]:
        raise ValueError("Packaged app copyright does not match the packaging configuration")
    entitlements = plistlib.loads((root / "assets/packaging/app-store.entitlements").read_bytes())
    entitlements.update(profile_entitlements)
    embedded_profile = app / "Contents/embedded.provisionprofile"
    embedded_profile.write_bytes(profile_contents)
    entitlements_path = output / "app-store.entitlements"
    entitlements_path.write_bytes(plistlib.dumps(entitlements))
    run("codesign", "--force", "--sign", app_identity, "--keychain", keychain,
        "--entitlements", entitlements_path, app)
    run("codesign", "--verify", "--deep", "--strict", "--verbose=2",
        "--test-requirement", f'=anchor apple generic and certificate leaf[subject.OU] = "{team}"', app)
    signed = run("codesign", "--display", "--entitlements", ":-", app, capture_output=True).stdout
    if plistlib.loads(signed) != entitlements:
        raise ValueError("Signed application entitlements do not match the App Store configuration")
    if embedded_profile.read_bytes() != profile_contents:
        raise ValueError("The signed application is missing its expected provisioning profile")
    pkg = output / f'Beacon_{config["version"]}_universal_app-store.pkg'
    run("productbuild", "--component", app, "/Applications", "--sign", installer_identity,
        "--keychain", keychain, pkg)
    signature = run("pkgutil", "--check-signature", pkg, capture_output=True, text=True).stdout
    if installer_identity not in signature:
        raise ValueError("Installer signature does not match APP_STORE_INSTALLER_IDENTITY")
    print(signature)
    print(f"App Store package: {pkg}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("preflight", "package"))
    parser.add_argument("--output", type=Path, default=ROOT / "target/app-store")
    parser.add_argument("--keychain", type=Path)
    parser.add_argument("--profile", type=Path)
    args = parser.parse_args()
    try:
        team, _, _ = identities(os.environ)
        if args.command == "preflight":
            config = packager_config(ROOT, args.output.resolve())
            _, profile = load_provisioning_profile(args.profile, os.environ)
            provisioning_entitlements(profile, team, config["identifier"])
        if args.command == "package":
            if not args.keychain:
                raise ValueError("--keychain is required for signing")
            package(ROOT, args.output.resolve(), args.keychain.resolve(), args.profile)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"App Store packaging failed: {error}\n")


if __name__ == "__main__":
    main()
