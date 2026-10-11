#!/bin/sh
# Regenerate macos/AppIcon.icns from macos/icon.svg. Uses qlmanage
# (QuickLook) to rasterize the SVG — no extra dependencies — then
# iconutil to fold the PNGs into an icns.
set -eu
cd "$(dirname "$0")"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

for size in 16 32 128 256 512; do
    qlmanage -t -s "$size" -o "$WORK" icon.svg >/dev/null
    mv "$WORK/icon.svg.png" "$WORK/icon_${size}x${size}.png"
    qlmanage -t -s $((size * 2)) -o "$WORK" icon.svg >/dev/null
    mv "$WORK/icon.svg.png" "$WORK/icon_${size}x${size}@2x.png"
done

mkdir AppIcon.iconset
cp "$WORK"/*.png AppIcon.iconset/
iconutil -c icns AppIcon.iconset
rm -rf AppIcon.iconset
echo "wrote $(pwd)/AppIcon.icns"
