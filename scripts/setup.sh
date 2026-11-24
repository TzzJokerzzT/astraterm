#!/usr/bin/env bash
# AstraTerm bootstrap: from a fresh clone to a running binary.
#
# What it does, in order:
#
#   1. detects the package manager (pacman, apt, dnf) and either installs or
#      prints the exact dependency set for that distribution;
#   2. checks the Rust toolchain (1.85+, edition 2024) and refuses to continue
#      instead of downloading a toolchain behind your back;
#   3. checks the default font family (JetBrainsMono Nerd Font) and the optional
#      desktop integration tools (`notify-send`, `wl-copy`/`wl-paste`);
#   4. builds with `cargo build --release --locked`;
#   5. with `--install`, runs `packaging/linux/install.sh` to put the binary,
#      the desktop entry and the icons in your XDG directories.
#
# It never writes to ~/.config/astraterm, never runs the build as root, never
# installs a Rust toolchain on its own, and never downloads fonts or other
# files from the network. Every step is a check plus an idempotent command, so
# running it again on a prepared machine changes nothing.
#
# Usage:
#   ./scripts/setup.sh [--dry-run] [--yes] [--install] [--no-build]
#
#   --dry-run    print every command that would change the system or the tree
#                and execute none of them. Read-only probes (`command -v`,
#                `cargo --version`, `fc-list`) still run so that the plan
#                matches this machine; they change nothing.
#   --yes        do not ask for confirmation before installing packages
#                (intended for scripted setups; still asks sudo for the password)
#   --install    after a successful build, install the binary for the current
#                user with packaging/linux/install.sh
#   --no-build   skip `cargo build --release --locked`
#
# Exit codes:
#   0  success, nothing left to do (declining the package installation is not
#      an error: the step is skipped and the build still runs)
#   2  usage error or wrong user (root)
#   3  Rust toolchain missing or older than 1.85
#   other  the exit status of the command that failed (cargo, pacman, ...)
#
# See docs/INSTALL.md for the requirements per distribution.

set -euo pipefail

readonly MIN_RUST_MINOR=85
readonly DEFAULT_FONT_FAMILY="JetBrainsMono Nerd Font"

readonly EXIT_OK=0
readonly EXIT_USAGE=2
readonly EXIT_TOOLCHAIN=3

dry_run=0
assume_yes=0
install_binary=0
do_build=1

root_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

usage() {
    # Quoted heredoc: the option descriptions contain backticks, and an
    # unquoted one would run them as a command substitution.
    cat <<'EOF'
setup.sh - build AstraTerm from a fresh clone and install its dependencies

Usage:
  ./scripts/setup.sh [--dry-run] [--yes] [--install] [--no-build]

Options:
  --dry-run    print every command that would change the system or the tree and
               execute none of them. Read-only probes (command -v, cargo --version,
               fc-list) still run so the plan matches this machine.
  --yes, -y    do not ask for confirmation before installing packages
  --install    after a successful build, install the binary for the current user
               with packaging/linux/install.sh
  --no-build   skip the release build
  -h, --help   show this help

Exit codes: 0 success, 2 usage error or root, 3 Rust toolchain missing or
            older than 1.85; a failing command exits with its own status.
            Declining the package installation is not a failure.

Documentation: docs/INSTALL.md
EOF
}

step() {
    printf '\n==> %s\n' "$*"
}

info() {
    printf '    %s\n' "$*"
}

ok() {
    printf '  ok: %s\n' "$*"
}

warn() {
    printf 'warning: %s\n' "$*" >&2
}

fail() {
    local code=$1
    shift
    printf 'error: %s\n' "$1" >&2
    shift
    local line
    for line in "$@"; do
        printf '       %s\n' "$line" >&2
    done
    exit "$code"
}

# Run a command that changes something. In --dry-run mode it is only printed.
run() {
    if [ "$dry_run" -eq 1 ]; then
        printf '  would run:'
        printf ' %q' "$@"
        printf '\n'
        return 0
    fi
    printf '  running:'
    printf ' %q' "$@"
    printf '\n'
    "$@"
}

# Ask a yes/no question. `--yes` answers yes, a non-interactive stdin answers no.
confirm() {
    local question=$1
    if [ "$assume_yes" -eq 1 ]; then
        return 0
    fi
    if [ "$dry_run" -eq 1 ]; then
        printf '  would ask: %s [y/N]\n' "$question"
        return 0
    fi
    if [ ! -t 0 ]; then
        info "stdin is not a terminal: assuming no (re-run with --yes to confirm)"
        return 1
    fi
    local answer
    printf '%s [y/N] ' "$question"
    read -r answer || answer=""
    case $answer in
    [yY] | [yY][eE][sS]) return 0 ;;
    *) return 1 ;;
    esac
}

parse_args() {
    while [ "$#" -gt 0 ]; do
        case $1 in
        --dry-run) dry_run=1 ;;
        --yes | -y) assume_yes=1 ;;
        --install) install_binary=1 ;;
        --no-build) do_build=0 ;;
        -h | --help) usage; exit "$EXIT_OK" ;;
        *) fail "$EXIT_USAGE" "unknown argument: $1 (try --help)" ;;
        esac
        shift
    done
}

require_normal_user() {
    if [ "$(id -u)" -eq 0 ]; then
        fail "$EXIT_USAGE" \
            "do not run this script as root: run it as your user." \
            "It asks sudo for the package installation only, and cargo must not" \
            "write root-owned files into target/."
    fi
}

detect_distro() {
    # Prints "<ID> <ID_LIKE>"; either field may be empty. /etc/os-release is
    # sourced in a subshell so nothing leaks into this script's environment.
    local id="" like=""
    if [ -r /etc/os-release ]; then
        # shellcheck source=/etc/os-release
        id=$(. /etc/os-release && printf '%s' "${ID:-}")
        like=$(. /etc/os-release && printf '%s' "${ID_LIKE:-}")
    fi
    printf '%s %s' "${id:-unknown}" "$like"
}

distro_family() {
    # Collapse the ID/ID_LIKE pair into the family the script knows how to talk
    # about, so an Arch derivative such as Omarchy gets Arch instructions.
    case " $1 $2 " in
    *" arch "* | *" archarm "*) printf 'arch' ;;
    *" debian "* | *" ubuntu "*) printf 'debian' ;;
    *" fedora "* | *" rhel "*) printf 'fedora' ;;
    *) printf 'unknown' ;;
    esac
}

detect_manager() {
    local candidate
    for candidate in pacman apt-get dnf; do
        if command -v "$candidate" >/dev/null 2>&1; then
            case $candidate in
            pacman) printf 'pacman' ;;
            apt-get) printf 'apt' ;;
            dnf) printf 'dnf' ;;
            esac
            return 0
        fi
    done
    return 1
}

sudo_prefix() {
    if command -v sudo >/dev/null 2>&1; then
        printf 'sudo'
    fi
}

install_system_packages() {
    local manager=$1
    local distro=$2

    # One explicit list per distribution: every name below exists in that
    # distribution's own repositories. Nothing here is a guess.
    local -a packages=()
    local -a install_cmd=()
    case $manager in
    pacman)
        # build tools, Wayland/X11 + font/Vulkan runtime libraries, clipboard,
        # desktop notifications, git, and the default Nerd Font family.
        packages=(
            base-devel wayland libxkbcommon fontconfig freetype2 mesa
            vulkan-icd-loader libx11 wl-clipboard libnotify git
            ttf-jetbrains-mono-nerd
        )
        install_cmd=(pacman -S --needed)
        ;;
    apt)
        # Debian and Ubuntu. The Nerd Font is not packaged here; the font check
        # below points at the closest packaged family and at a manual install.
        packages=(
            build-essential pkg-config git libwayland-dev libxkbcommon-dev
            libfontconfig1-dev libfreetype6-dev libvulkan1 mesa-vulkan-drivers
            libgl1-mesa-dri libx11-dev wl-clipboard libnotify-bin
        )
        install_cmd=(apt-get install -y --no-install-recommends)
        ;;
    dnf)
        # Fedora. Same note about the Nerd Font as for apt.
        packages=(
            gcc gcc-c++ make pkgconf-pkg-config git wayland-devel
            libxkbcommon-devel fontconfig-devel freetype-devel vulkan-loader
            mesa-vulkan-drivers libX11-devel wl-clipboard libnotify
        )
        install_cmd=(dnf install -y)
        ;;
    *)
        return 1
        ;;
    esac

    step "System packages ($distro via $manager)"

    local sudo_cmd
    sudo_cmd=$(sudo_prefix)
    if [ -z "$sudo_cmd" ]; then
        warn "sudo is not installed, so these packages have to be installed by hand."
        info "run as root:"
        if [ "$manager" = apt ]; then
            info "  apt-get update"
        fi
        info "  ${install_cmd[*]} ${packages[*]}"
        return 0
    fi

    info "packages: ${packages[*]}"

    if [ "$dry_run" -eq 0 ] && ! confirm "install these packages with $manager?"; then
        info "skipped the package installation"
        info "install them yourself and re-run this script; the build step still follows"
        return 0
    fi

    # `--needed` on pacman is what makes this idempotent: installed packages are
    # left alone. No `-Sy` either: a partial upgrade on Arch is not something a
    # setup script may start. apt-get and dnf are idempotent by design.
    if [ "$manager" = apt ]; then
        run "$sudo_cmd" apt-get update
    fi
    run "$sudo_cmd" "${install_cmd[@]}" "${packages[@]}"
}

check_toolchain() {
    step "Rust toolchain"

    if ! command -v cargo >/dev/null 2>&1; then
        fail "$EXIT_TOOLCHAIN" \
            "cargo was not found." \
            "AstraTerm needs Rust 1.85+ (edition 2024). Install it yourself with:" \
            "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" \
            "or your distribution's rust package, then open a new shell and re-run" \
            "this script. This script never installs a toolchain on its own."
    fi

    local version major minor
    version=$(cargo --version | awk '{print $2}')
    if [[ ! $version =~ ^[0-9]+\.[0-9]+ ]]; then
        fail "$EXIT_TOOLCHAIN" \
            "could not read the Rust version from \`cargo --version\` (got '$version')." \
            "Check that cargo runs correctly, or reinstall it with rustup."
    fi
    major=${version%%.*}
    minor=${version#*.}
    minor=${minor%%.*}

    if [ "$major" -lt 1 ] || { [ "$major" -eq 1 ] && [ "$minor" -lt "$MIN_RUST_MINOR" ]; }; then
        fail "$EXIT_TOOLCHAIN" \
            "cargo $version is too old: AstraTerm needs Rust 1.85+ (edition 2024)." \
            "Update it yourself with:" \
            "  rustup update stable" \
            "This script never downloads a toolchain on its own."
    fi
    ok "cargo $version"

    if command -v rustc >/dev/null 2>&1; then
        ok "rustc $(rustc --version | awk '{print $2}')"
    fi

    local tool
    for tool in cc pkg-config; do
        if command -v "$tool" >/dev/null 2>&1; then
            ok "$tool found"
        else
            warn "$tool not found: the build needs a C toolchain and pkg-config."
            info "they ship in the package list above (base-devel / build-essential / gcc + pkgconf-pkg-config)"
        fi
    done
}

font_hint() {
    local family=$1
    info "AstraTerm defaults to $DEFAULT_FONT_FAMILY and falls back to JetBrains Mono,"
    info "Symbols Nerd Font, Noto Color Emoji, DejaVu Sans Mono and finally monospace."
    info "install a Nerd Font with one of:"
    case $family in
    arch)
        info "  sudo pacman -S ttf-jetbrains-mono-nerd"
        ;;
    debian)
        info "  sudo apt install fonts-jetbrains-mono     # JetBrains Mono, the fallback family"
        info "  or download a Nerd Font from https://www.nerdfonts.com/font-downloads"
        info "  and unpack it into ~/.local/share/fonts, then run: fc-cache -f"
        ;;
    fedora)
        info "  sudo dnf install jetbrains-mono-fonts      # JetBrains Mono, the fallback family"
        info "  or enable the Nerd Fonts copr: atim/nerd-fonts"
        ;;
    *)
        info "  install $DEFAULT_FONT_FAMILY with your package manager, or unpack a"
        info "  Nerd Font release into ~/.local/share/fonts and run: fc-cache -f"
        ;;
    esac
    info "any monospaced font works today if you set [font] family in config.toml"
}

check_font() {
    local family=$1
    step "Font"

    if ! command -v fc-list >/dev/null 2>&1; then
        warn "fontconfig (fc-list) is not installed, so the font cannot be checked."
        font_hint "$family"
        return 0
    fi

    # `grep -q` would close the pipe early and, with `pipefail`, make this test
    # fail on a machine that does have the font. Collect first, then match.
    local families
    families=$(fc-list : family 2>/dev/null || true)
    if [[ $families == *"$DEFAULT_FONT_FAMILY"* ]]; then
        ok "$DEFAULT_FONT_FAMILY found"
    else
        warn "$DEFAULT_FONT_FAMILY not found: text falls back to another family and glyphs may be missing."
        font_hint "$family"
    fi
}

check_desktop_tools() {
    step "Desktop integration"

    local missing=0
    if command -v notify-send >/dev/null 2>&1; then
        ok "notify-send found (desktop notifications)"
    else
        warn "notify-send not found (libnotify): save/reload notifications are lost."
        info "package: libnotify (Arch) / libnotify-bin (Debian, Ubuntu) / libnotify (Fedora)"
        missing=1
    fi

    if command -v wl-copy >/dev/null 2>&1 && command -v wl-paste >/dev/null 2>&1; then
        ok "wl-copy and wl-paste found (clipboard)"
    else
        warn "wl-copy/wl-paste not found (wl-clipboard): clipboard copy/paste on Wayland will not work."
        info "package: wl-clipboard"
        missing=1
    fi

    if [ "$missing" -eq 1 ]; then
        info "these are runtime features, not build dependencies: the build below still works without them"
    fi
}

build_binary() {
    step "Build"
    info "cargo build --release --locked (in $root_dir)"
    if [ "$dry_run" -eq 1 ]; then
        run cargo build --release --locked
        return 0
    fi
    (cd "$root_dir" && run cargo build --release --locked)
    if [ -x "$root_dir/target/release/astraterm" ]; then
        ok "target/release/astraterm"
    else
        warn "the build reported success but target/release/astraterm is missing"
    fi
}

install_for_user() {
    step "Install for the current user"
    info "packaging/linux/install.sh writes to your XDG bin/data directories and"
    info "never touches ~/.config/astraterm."
    run sh "$root_dir/packaging/linux/install.sh"
}

main() {
    parse_args "$@"
    require_normal_user

    if [ ! -f "$root_dir/Cargo.toml" ]; then
        fail "$EXIT_USAGE" "$root_dir does not look like the AstraTerm repository (no Cargo.toml)"
    fi

    local distro_id distro_like family manager
    read -r distro_id distro_like <<<"$(detect_distro)"
    family=$(distro_family "$distro_id" "$distro_like")

    step "AstraTerm setup"
    info "repository: $root_dir"
    info "distribution: $distro_id${distro_like:+ (like $distro_like)}"
    if [ "$dry_run" -eq 1 ]; then
        info "dry run: nothing that changes the system or the tree is executed"
    fi

    manager=$(detect_manager || true)
    if [ -n "$manager" ]; then
        install_system_packages "$manager" "$distro_id"
    else
        warn "no supported package manager found (pacman, apt, dnf)."
        info "install these capabilities with your own package manager:"
        info "  Rust 1.85+, a C toolchain, pkg-config; wayland + libxkbcommon,"
        info "  fontconfig + freetype2, a Vulkan ICD (Mesa is enough), libX11;"
        info "  wl-clipboard, libnotify, git, and a Nerd Font (default: $DEFAULT_FONT_FAMILY)"
        info "see docs/INSTALL.md for the mapping per distribution"
    fi

    check_toolchain
    check_font "$family"
    check_desktop_tools

    if [ "$do_build" -eq 1 ]; then
        build_binary
    else
        step "Build"
        info "skipped (--no-build)"
    fi

    if [ "$install_binary" -eq 1 ]; then
        install_for_user
    fi

    step "Done"
    if [ "$dry_run" -eq 1 ]; then
        info "dry run finished: no package was installed, nothing was built or copied"
    elif [ "$do_build" -eq 1 ] && [ "$install_binary" -eq 1 ]; then
        info "astraterm is installed; run \`astraterm\` or pick it from the application menu"
    elif [ "$do_build" -eq 1 ]; then
        info "run ./target/release/astraterm, or re-run this script with --install"
    else
        info "nothing was built; re-run without --no-build to build the binary"
    fi
    return "$EXIT_OK"
}

main "$@"
