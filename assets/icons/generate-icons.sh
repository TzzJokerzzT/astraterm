#!/bin/sh
# Generates the AstraTerm icon set from assets/icons/logo.png.
#
# The source logo is a landscape PNG on a white background. A global white removal
# would also erase the white `>_` prompt inside the window, so the background is
# flood-filled from the four corners instead: only the connected outer region
# becomes transparent.
#
# Requirements: ImageMagick (`magick`) and, optionally, `rsvg-convert`.
# Usage: run from anywhere: `sh assets/icons/generate-icons.sh`
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
source_png="$here/logo.png"
sizes="32 48 64 128 256 512"

if [ ! -f "$source_png" ]; then
    echo "missing source logo: $source_png" >&2
    exit 1
fi
if ! command -v magick >/dev/null 2>&1; then
    echo "ImageMagick (magick) is required to generate the icons" >&2
    exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# 1. Make the connected outer background transparent.
width=$(magick identify -format '%w' "$source_png")
height=$(magick identify -format '%h' "$source_png")
right=$((width - 1))
bottom=$((height - 1))
magick "$source_png" -alpha set -fuzz 14% -fill none \
    -draw "alpha 0,0 floodfill" \
    -draw "alpha $right,0 floodfill" \
    -draw "alpha 0,$bottom floodfill" \
    -draw "alpha $right,$bottom floodfill" \
    "$work/transparent.png"

# 2. Trim the empty border and centre it on a square canvas with a small margin so
#    the icon looks the same in a launcher, a dock and a window switcher.
magick "$work/transparent.png" -trim +repage "$work/trimmed.png"
side=$(magick identify -format '%[fx:max(w,h)*1.12]' "$work/trimmed.png")
side=$(printf '%.0f' "$side")
magick "$work/trimmed.png" -background none -gravity center \
    -extent "${side}x${side}" "$work/square.png"

# 3. Canonical 512 px icon plus the hicolor sizes that desktop environments read.
magick "$work/square.png" -resize 512x512 "$here/astraterm.png"
for size in $sizes; do
    dir="$here/hicolor/${size}x${size}/apps"
    mkdir -p "$dir"
    magick "$work/square.png" -resize "${size}x${size}" "$dir/astraterm.png"
    echo "wrote $dir/astraterm.png"
done
echo "wrote $here/astraterm.png (512 px)"
