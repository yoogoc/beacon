#!/usr/bin/env bash
set -euo pipefail

# Download completion does not guarantee that Xcode can execute the component
# yet. Probe the real compiler, including when it was already on the runner.
if xcrun --sdk macosx metal --version; then
  exit 0
fi

for attempt in 1 2 3; do
  printf 'Preparing Metal toolchain (attempt %s/3).\n' "$attempt"
  if ! xcodebuild -downloadComponent MetalToolchain; then
    printf '::warning::Metal toolchain download failed; checking availability before retrying.\n'
  fi

  # Discard tool lookups cached before the optional component was installed.
  xcrun --kill-cache || true
  for probe in 1 2 3 4 5 6; do
    if xcrun --sdk macosx metal --version; then
      exit 0
    fi
    if [ "$probe" -lt 6 ]; then
      sleep 5
    fi
  done
done

xcodebuild -showComponent MetalToolchain || true
printf '::error::Metal compiler is still unavailable after three preparation attempts; stopping before the build.\n'
exit 1
