#!/bin/sh
# AstraTerm portable installer.
#
# Installs a release build into the user's own XDG directories:
#
#   $XDG_BIN_HOME/astraterm                              -> the binary
#   $XDG_DATA_HOME/applications/astraterm.desktop        -> the desktop entry
#   $XDG_DATA_HOME/icons/hicolor/<size>/apps/astraterm.png -> the icon, every size
#
# The icon comes from `assets/icons/hicolor/`, which is generated from
# `assets/icons/logo.png` by `assets/icons/generate-icons.sh`, so every desktop
# environment finds the size it wants (launcher, dock, window switcher).
#
# It never needs root, never writes outside those directories, and never touches
# existing configuration in $XDG_CONFIG_HOME. Use ./uninstall.sh to remove
# exactly the files this script created.

set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root_dir=$(CDPATH= cd -- "$script_dir/../.." && pwd)

binary_source="$root_dir/target/release/astraterm"
desktop_source="$root_dir/packaging/desktop/astraterm.desktop"
icon_dir="$root_dir/assets/icons/hicolor"

if [ -z "${HOME:-}" ]; then
    printf 'error: HOME is not set; export HOME or set XDG_BIN_HOME and XDG_DATA_HOME\n' >&2
    exit 1
fi

bin_dir=${XDG_BIN_HOME:-"$HOME/.local/bin"}
data_dir=${XDG_DATA_HOME:-"$HOME/.local/share"}
applications_dir="$data_dir/applications"
icons_root="$data_dir/icons/hicolor"

if [ ! -f "$binary_source" ]; then
    printf 'error: %s does not exist\n' "$binary_source" >&2
    printf '       build it first: cargo build --release --locked\n' >&2
    exit 1
fi
if [ ! -f "$desktop_source" ]; then
    printf 'error: %s does not exist\n' "$desktop_source" >&2
    exit 1
fi
if [ ! -d "$icon_dir" ]; then
    printf 'error: %s does not exist; run assets/icons/generate-icons.sh\n' "$icon_dir" >&2
    exit 1
fi

mkdir -p "$bin_dir"
install -m 0755 "$binary_source" "$bin_dir/astraterm"
printf 'installed %s\n' "$bin_dir/astraterm"

mkdir -p "$applications_dir"
install -m 0644 "$desktop_source" "$applications_dir/astraterm.desktop"
printf 'installed %s\n' "$applications_dir/astraterm.desktop"

# Every generated size, so the launcher, the dock and the window switcher all pick
# a native-resolution icon instead of scaling one bitmap.
installed_icons=0
for source in "$icon_dir"/*/apps/astraterm.png; do
    [ -f "$source" ] || continue
    size_dir=$(basename "$(dirname "$(dirname "$source")")")
    target_dir="$icons_root/$size_dir/apps"
    mkdir -p "$target_dir"
    install -m 0644 "$source" "$target_dir/astraterm.png"
    printf 'installed %s\n' "$target_dir/astraterm.png"
    installed_icons=$((installed_icons + 1))
done
if [ "$installed_icons" -eq 0 ]; then
    printf 'error: no icon files found under %s\n' "$icon_dir" >&2
    exit 1
fi

# Refresh the desktop and icon caches when the helper tools are available. A
# failure here is never fatal: the caches are rebuilt by the desktop anyway.
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$applications_dir" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t "$icons_root" >/dev/null 2>&1 || true
fi
if command -v xdg-desktop-menu >/dev/null 2>&1; then
    # Some launchers only re-scan after this hint; ignore failures.
    xdg-desktop-menu forceupdate >/dev/null 2>&1 || true
fi

printf '\nAstraTerm installed: it now appears in the application menu as "AstraTerm".\n'
case ":${PATH:-}:" in
    *":$bin_dir:"*) ;;
    *)
        printf 'note: %s is not in PATH; add it to use `astraterm` directly.\n' "$bin_dir"
        ;;
esac
printf 'configuration lives in %s/config.toml and was not modified.\n' \
    "${XDG_CONFIG_HOME:-"$HOME/.config"}/astraterm"
