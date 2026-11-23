#!/bin/sh
# AstraTerm portable installer.
#
# Installs a release build into the user's own XDG directories:
#
#   $XDG_BIN_HOME/astraterm                        -> the binary
#   $XDG_DATA_HOME/applications/astraterm.desktop  -> the desktop entry
#   $XDG_DATA_HOME/icons/hicolor/.../astraterm.*   -> the icon
#
# It never needs root, never writes outside those directories, and never
# touches existing configuration in $XDG_CONFIG_HOME. Use ./uninstall.sh to
# remove exactly the files this script created.

set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root_dir=$(CDPATH= cd -- "$script_dir/../.." && pwd)

binary_source="$root_dir/target/release/astraterm"
desktop_source="$root_dir/packaging/desktop/astraterm.desktop"
icon_svg_source="$root_dir/assets/icons/astraterm.svg"
icon_png_source="$root_dir/assets/icons/astraterm.png"

if [ -z "${HOME:-}" ]; then
    printf 'error: HOME is not set; export HOME or set XDG_BIN_HOME and XDG_DATA_HOME\n' >&2
    exit 1
fi

bin_dir=${XDG_BIN_HOME:-"$HOME/.local/bin"}
data_dir=${XDG_DATA_HOME:-"$HOME/.local/share"}
applications_dir="$data_dir/applications"
icon_png_dir="$data_dir/icons/hicolor/128x128/apps"
icon_scalable_dir="$data_dir/icons/hicolor/scalable/apps"

if [ ! -f "$binary_source" ]; then
    printf 'error: %s does not exist\n' "$binary_source" >&2
    printf '       build it first: cargo build --release --locked\n' >&2
    exit 1
fi
if [ ! -f "$desktop_source" ]; then
    printf 'error: %s does not exist\n' "$desktop_source" >&2
    exit 1
fi
if [ ! -f "$icon_svg_source" ]; then
    printf 'error: %s does not exist\n' "$icon_svg_source" >&2
    exit 1
fi

mkdir -p "$bin_dir"
install -m 0755 "$binary_source" "$bin_dir/astraterm"
printf 'installed %s\n' "$bin_dir/astraterm"

mkdir -p "$applications_dir"
install -m 0644 "$desktop_source" "$applications_dir/astraterm.desktop"
printf 'installed %s\n' "$applications_dir/astraterm.desktop"

# Prefer a prebuilt PNG, otherwise rasterise the SVG, otherwise fall back to the
# scalable SVG slot that icon themes resolve just as well.
if [ -f "$icon_png_source" ]; then
    mkdir -p "$icon_png_dir"
    install -m 0644 "$icon_png_source" "$icon_png_dir/astraterm.png"
    printf 'installed %s\n' "$icon_png_dir/astraterm.png"
elif command -v rsvg-convert >/dev/null 2>&1; then
    tmp_png=$(mktemp "${TMPDIR:-/tmp}/astraterm-icon.XXXXXX")
    trap 'rm -f -- "$tmp_png"' EXIT HUP INT TERM
    rsvg-convert -w 128 -h 128 "$icon_svg_source" -o "$tmp_png"
    mkdir -p "$icon_png_dir"
    install -m 0644 "$tmp_png" "$icon_png_dir/astraterm.png"
    rm -f -- "$tmp_png"
    trap - EXIT HUP INT TERM
    printf 'installed %s\n' "$icon_png_dir/astraterm.png"
else
    mkdir -p "$icon_scalable_dir"
    install -m 0644 "$icon_svg_source" "$icon_scalable_dir/astraterm.svg"
    printf 'installed %s\n' "$icon_scalable_dir/astraterm.svg"
    printf 'note: rsvg-convert not found, so the scalable SVG icon was installed\n'
    printf '      instead of a 128x128 PNG (install librsvg to change that)\n'
fi

# Refresh the desktop and icon caches when the helper tools are available. A
# failure here is never fatal: the caches are rebuilt by the desktop anyway.
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$applications_dir" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t "$data_dir/icons/hicolor" >/dev/null 2>&1 || true
fi

printf '\nAstraTerm installed.\n'
case ":${PATH:-}:" in
    *":$bin_dir:"*) ;;
    *)
        printf 'note: %s is not in PATH; add it to use `astraterm` directly.\n' "$bin_dir"
        ;;
esac
printf 'configuration lives in %s/config.toml and was not modified.\n' \
    "${XDG_CONFIG_HOME:-"$HOME/.config"}/astraterm"
