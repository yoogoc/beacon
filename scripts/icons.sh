#!/usr/bin/env bash
# Regenerate every icon in assets/app-icon/ from beacon-icon.svg.
#
# Each raster is rendered from the SVG at its final size rather than downscaled
# from the 1024, which is what keeps the 16 and 32 px versions legible: the lamp
# and the heptagon ring are tuned to survive at those sizes, and a bicubic
# downscale of the big one smears both.
#
# Needs rsvg-convert (librsvg) and magick (ImageMagick). The .icns step needs
# iconutil, which only exists on macOS -- run this on a Mac, or leave beacon.icns
# alone.
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
icons="$here/assets/app-icon"
svg="$icons/beacon-icon.svg"

render() { rsvg-convert -w "$1" -h "$1" "$svg" -o "$2"; }

# Linux's hicolor theme takes the loose PNGs.
for size in 16 32 64 128 256 512 1024; do
    render "$size" "$icons/icon-$size.png"
done

if command -v iconutil >/dev/null 2>&1; then
    set="$(mktemp -d)/beacon.iconset"
    mkdir -p "$set"
    render 16   "$set/icon_16x16.png"
    render 32   "$set/icon_16x16@2x.png"
    render 32   "$set/icon_32x32.png"
    render 64   "$set/icon_32x32@2x.png"
    render 128  "$set/icon_128x128.png"
    render 256  "$set/icon_128x128@2x.png"
    render 256  "$set/icon_256x256.png"
    render 512  "$set/icon_256x256@2x.png"
    render 512  "$set/icon_512x512.png"
    render 1024 "$set/icon_512x512@2x.png"
    iconutil -c icns "$set" -o "$icons/beacon.icns"
else
    echo "iconutil not found; leaving beacon.icns as it is" >&2
fi

# 24 and 48 are in the .ico but not in the PNG set: Windows picks between them
# in list views, and nothing else asks for those sizes.
tmp=$(mktemp -d)
for size in 16 24 32 48 64 128 256; do
    render "$size" "$tmp/$size.png"
done
magick "$tmp/16.png" "$tmp/24.png" "$tmp/32.png" "$tmp/48.png" \
       "$tmp/64.png" "$tmp/128.png" "$tmp/256.png" "$icons/beacon.ico"

echo "wrote $(ls -1 "$icons" | wc -l | tr -d ' ') files to assets/app-icon/"
