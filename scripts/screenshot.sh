#!/usr/bin/env bash
#
# Capture the running Beacon window to a PNG.
#
# Why this exists: UI work cannot be verified from logs. A window that opens,
# logs cleanly and renders nothing at all produces exactly the same output as a
# correct one. This grabs the actual pixels so a change can be looked at.
#
# Usage:
#   scripts/screenshot.sh [output.png]
#
# macOS only. Requires Screen Recording permission for whichever terminal runs
# it (System Settings -> Privacy & Security -> Screen Recording); without it
# the capture silently produces a desktop-wallpaper image instead.

set -euo pipefail

APP="${BEACON_APP_NAME:-beacon}"
OUT="${1:-/tmp/beacon-$(date +%Y%m%d-%H%M%S).png}"

if [[ "$(uname)" != "Darwin" ]]; then
    echo "screenshot.sh only works on macOS" >&2
    exit 1
fi

# CGWindowID is what `screencapture -l` wants. AppleScript's window id is a
# different, incompatible identifier, and the system python has no Quartz
# bindings -- so ask CoreGraphics directly.
window_id=$(swift - "$APP" <<'SWIFT' | head -1
import CoreGraphics
import Foundation

let target = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "beacon"
guard let list = CGWindowListCopyWindowInfo(
    [.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID
) as? [[String: Any]] else { exit(1) }

for window in list {
    guard let owner = window["kCGWindowOwnerName"] as? String, owner == target,
          let number = window["kCGWindowNumber"] as? Int,
          // Layer 0 is a normal application window. Shadows, tooltips and
          // panels live on other layers and would capture the wrong thing.
          (window["kCGWindowLayer"] as? Int) == 0
    else { continue }
    print(number)
}
SWIFT
)

if [[ -z "$window_id" ]]; then
    echo "no on-screen window found for '$APP' -- is it running?" >&2
    echo "  cargo run -p beacon" >&2
    exit 1
fi

# -l captures that window alone, so nothing else on screen ends up in the file.
# -x silences the shutter, -o drops the drop shadow.
screencapture -x -o -l "$window_id" "$OUT"

echo "$OUT"
