#!/bin/sh
# AstraTerm portable uninstaller.
#
# Removes exactly the files that ./install.sh creates, and nothing else:
#
#   $XDG_BIN_HOME/astraterm
#   $XDG_DATA_HOME/applications/astraterm.desktop
#   $XDG_DATA_HOME/icons/hicolor/<size>/apps/astraterm.png  (every installed size)
#
# Files that are already gone are skipped, so running this twice is safe.
# User configuration in $XDG_CONFIG_HOME/astraterm is never touched.

set -eu

if [ -z "${HOME:-}" ]; then
    printf 'error: HOME is not set; export HOME or set XDG_BIN_HOME and XDG_DATA_HOME\n' >&2
    exit 1
fi

bin_dir=${XDG_BIN_HOME:-"$HOME/.local/bin"}
data_dir=${XDG_DATA_HOME:-"$HOME/.local/share"}
applications_dir="$data_dir/applications"
icons_root="$data_dir/icons/hicolor"

removed=0

remove_file() {
    path=$1
    if [ -e "$path" ] || [ -L "$path" ]; then
        rm -f -- "$path"
        printf 'removed %s\n' "$path"
        removed=$((removed + 1))
    else
        printf 'skipped %s (not installed)\n' "$path"
    fi
}

remove_file "$bin_dir/astraterm"
remove_file "$applications_dir/astraterm.desktop"
for size in 32x32 48x48 64x64 128x128 256x256 512x512; do
    remove_file "$icons_root/$size/apps/astraterm.png"
done
# Remove the old placeholder scalable icon if a previous install created it.
remove_file "$icons_root/scalable/apps/astraterm.svg"

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$applications_dir" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t "$icons_root" >/dev/null 2>&1 || true
fi

printf '\nremoved %s file(s).\n' "$removed"
printf 'your configuration in %s/astraterm was left untouched.\n' \
    "${XDG_CONFIG_HOME:-"$HOME/.config"}"
