#!/usr/bin/env bash
# AstraTerm bootstrap: from a fresh clone to a running binary.
#
# What it does, in order:
#
#   1. detects the package manager (pacman, apt, dnf) and either installs or
#      prints the exact dependency set for that distribution;
#   2. checks the Rust toolchain (1.85+, edition 2024); when cargo is missing it
#      offers the rustup installation instead of installing one behind your
#      back. That offer is the only download this script makes besides an
#      explicitly accepted `git fetch --tags`;
#   3. checks the font: the default family (JetBrainsMono Nerd Font) is asked of
#      fontconfig, never assumed from the distribution. When it is missing the
#      script offers the distribution's own font package, and when that offer is
#      declined it offers to create ~/.config/astraterm/config.toml with a
#      family this machine already has. The Nerd Font is deliberately absent
#      from the dependency list: on a distribution that ships the family through
#      another package (Omarchy ships ttf-jetbrains-mono-nerd-basic, which
#      provides and conflicts with ttf-jetbrains-mono-nerd), installing it by
#      name would offer to remove the distribution's own package;
#   4. checks the optional desktop notifications (`notify-send`); the clipboard
#      is linked into the binary;
#   5. builds with `cargo build --release --locked`;
#   6. installs it for the current user by default, through
#      `packaging/linux/install.sh`: the binary in your XDG bin directory and the
#      desktop entry plus every icon size in your XDG data directory, so the
#      terminal is ready to use both from PATH and from the application menu.
#      `--no-install` skips this step.
#
# It never overwrites an existing ~/.config/astraterm/config.toml (it only
# offers to create one when there is none), never runs the build as root, never
# installs a Rust toolchain unless that offer was accepted, never adds a
# third-party repository and never downloads anything except the rustup
# installer, each after an explicit yes. Every step is a check plus an
# idempotent command, so running it again on a prepared machine changes nothing.
#
# Usage:
#   ./scripts/setup.sh [--dry-run] [--yes] [--no-install] [--no-build] [--version <tag>]
#
#   --dry-run        print every command that would change the system or the
#                    tree and execute none of them. Read-only probes (`command
#                    -v`, `cargo --version`, `fc-list`) still run so that the
#                    plan matches this machine; they change nothing.
#   --yes            do not ask for confirmation before installing packages or
#                    the Rust toolchain, and accept the font offers (intended
#                    for scripted setups; still asks sudo for the password)
#   --install        still accepted, and a no-op: installing is what the script
#                    does by default
#   --no-install     do not install the binary, the desktop entry or the icons;
#                    leave the build in target/release
#   --no-build       skip `cargo build --release --locked`
#   --version <tag>  build a specific release (`v1.0.0` or `1.0.0`). The script
#                    verifies that HEAD is exactly that tag and that the working
#                    tree is clean, and refuses otherwise; it never runs
#                    `git checkout`/`git switch` or fetches tags in --dry-run.
#
# Exit codes:
#   0  success, nothing left to do (declining the package installation is not
#      an error: the step is skipped and the build still runs)
#   2  usage error or wrong user (root)
#   3  the Rust toolchain is missing (installation declined or failed) or older
#      than 1.85
#   4  the requested `--version` tag is missing, HEAD is not at that tag, or the
#      working tree has changes
#   other  the exit status of the command that failed (cargo, pacman, ...)
#
# See docs/INSTALL.md for the requirements per distribution.

set -euo pipefail

readonly MIN_RUST_MINOR=85
# The official rustup installer; the only download besides `git fetch --tags`.
readonly RUSTUP_INSTALLER_URL="https://sh.rustup.rs"
readonly DEFAULT_FONT_FAMILY="JetBrainsMono Nerd Font"
# The package that carries the default family in the distributions that package
# it at all. Only ever installed after an explicit offer: see
# offer_nerd_font_install.
readonly NERD_FONT_PACKAGE="ttf-jetbrains-mono-nerd"

readonly EXIT_OK=0
readonly EXIT_USAGE=2
readonly EXIT_TOOLCHAIN=3
readonly EXIT_VERSION=4

dry_run=0
assume_yes=0
# The point of the script is a usable terminal, so the install runs by default.
install_binary=1
# Set by install_for_user once the files really are in the XDG directories, so the
# closing summary can tell an install from a skipped one. A failed installer never
# reaches it: the failure propagates and stops the script.
install_done=0
do_build=1
# Canonical `vX.Y.Z` tag requested with --version; empty means "build the tree".
requested_version=""

root_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

usage() {
    # Quoted heredoc: the option descriptions contain backticks, and an
    # unquoted one would run them as a command substitution.
    cat <<'EOF'
setup.sh - build AstraTerm from a fresh clone and install its dependencies

Usage:
  ./scripts/setup.sh [--dry-run] [--yes] [--no-install] [--no-build] [--version <tag>]

Options:
  --dry-run        print every command that would change the system or the tree and
                   execute none of them. Read-only probes (command -v, cargo
                   --version, fc-list, pacman -T) still run so the plan matches
                   this machine.
  --yes, -y        do not ask for confirmation before installing packages or the
                   Rust toolchain with rustup, and accept the font offer and the
                   fallback configuration offer without prompting
  --install        do nothing extra: installing is the default. Kept so existing
                   invocations keep working
  --no-install     do not install the binary, the desktop entry and the icons; leave
                   the build in target/release
  --no-build       skip the release build
  --version <tag>  build a specific release (v1.0.0, or 1.0.0). The tag must exist,
                   HEAD must be exactly at it and the working tree must be clean;
                   the script stops otherwise and never runs git checkout/git
                   switch itself.
  -h, --help       show this help

Exit codes: 0 success, 2 usage error or root, 3 the Rust toolchain is missing
            (declined or failed) or older than 1.85, 4 the --version tag is
            missing, HEAD is not at it, or the working tree has changes;
            a failing command exits with its own status. Declining the package
            installation, the font offer or the fallback configuration is not a
            failure.

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

# "1 commit" / "3 commits", for the --version messages.
commit_count() {
    if [ "$1" -eq 1 ]; then
        printf '1 commit'
    else
        printf '%s commits' "$1"
    fi
}

# Being at the tag is not the whole promise: uncommitted or untracked files would
# still be compiled, and an untracked Cargo configuration or build script changes
# the binary as effectively as an edit does. A status that cannot be read is not
# evidence of a clean tree either. Called once HEAD has been shown to be at the
# requested tag, so the message can only ever blame the tree, never the tag.
require_clean_tree() {
    local dirty
    if ! dirty=$(git -C "$root_dir" status --porcelain 2>&1); then
        fail "$EXIT_VERSION" \
            "git status failed, so the working tree cannot be shown to be clean." \
            "$dirty" \
            "Repair the repository (a corrupt index, for example) and re-run."
    fi
    if [ -n "$dirty" ]; then
        fail "$EXIT_VERSION" \
            "HEAD is $requested_version, but the working tree has changes:" \
            "$dirty" \
            "Building now would install code that is not that release: what gets" \
            "compiled has to be exactly what was published. Commit, stash or remove" \
            "them and re-run:" \
            "  git -C $root_dir status --short"
    fi
}

set_requested_version() {
    local raw=${1-} tag
    if [ -z "$raw" ]; then
        fail "$EXIT_USAGE" \
            "--version needs a tag: --version v1.0.0" \
            "Run ./scripts/setup.sh --help for the accepted forms."
    fi
    case $raw in
    v*) tag=$raw ;;
    *) tag="v$raw" ;;
    esac
    # Releases are tagged vX.Y.Z, so everything downstream compares the
    # canonical form and `--version 1.0.0` means the same as `--version v1.0.0`.
    if [[ ! $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        fail "$EXIT_USAGE" \
            "invalid tag for --version: '$raw'" \
            "Releases are tagged vX.Y.Z (for example v1.0.0). The leading 'v' is" \
            "optional on the command line, but the tag itself is compared as vX.Y.Z."
    fi
    requested_version=$tag
}

parse_args() {
    while [ "$#" -gt 0 ]; do
        case $1 in
        --dry-run) dry_run=1 ;;
        --yes | -y) assume_yes=1 ;;
        --install) install_binary=1 ;;
        --no-install) install_binary=0 ;;
        --no-build) do_build=0 ;;
        --version)
            shift
            set_requested_version "${1-}"
            ;;
        --version=*) set_requested_version "${1#--version=}" ;;
        -h | --help) usage; exit "$EXIT_OK" ;;
        *) fail "$EXIT_USAGE" "unknown argument: $1 (try --help)" ;;
        esac
        shift
    done
}

check_requested_version() {
    if [ -z "$requested_version" ]; then
        return 0
    fi

    # Set when `git fetch --tags` was actually attempted, so the "tag still
    # missing" message can tell a declined prompt from a failed one.
    local fetch_attempted=0

    step "Requested version: $requested_version"

    if ! command -v git >/dev/null 2>&1; then
        fail "$EXIT_VERSION" \
            "--version needs git to verify the tag, and git was not found." \
            "Install git (it is in the package list above) and re-run this script."
    fi

    # `rev-parse --git-dir` walks up to an enclosing repository, so a tarball
    # unpacked inside another checkout would pass this guard and then be verified
    # against that other repository. The top level has to be this directory.
    local top_level
    top_level=$(git -C "$root_dir" rev-parse --show-toplevel 2>/dev/null || true)
    if [ -z "$top_level" ] \
        || [ "$(cd "$top_level" && pwd -P)" != "$(cd "$root_dir" && pwd -P)" ]; then
        fail "$EXIT_VERSION" \
            "$root_dir is not a git checkout." \
            "--version $requested_version installs a tagged release, and without a git" \
            "checkout there is no tag to verify. Clone the repository instead of" \
            "unpacking a tarball:" \
            "  git clone <repository> astraterm"
    fi

    if ! git -C "$root_dir" rev-parse -q --verify "refs/tags/$requested_version" >/dev/null 2>&1; then
        info "the tag $requested_version is not in this clone"
        if [ "$dry_run" -eq 1 ]; then
            info "would run: git fetch --tags"
            info "a real run asks before that (it uses the network) and verifies again afterwards"
            fail "$EXIT_VERSION" \
                "cannot verify $requested_version: the tag is missing locally and" \
                "--dry-run never touches the network." \
                "Fetch the tags yourself, or re-run without --dry-run to be offered" \
                "\`git fetch --tags\`:" \
                "  git fetch --tags"
        fi
        # Declining a fetch and a fetch that was tried and failed are different
        # facts, and the message below has to say which one happened: after a
        # declined prompt nothing was attempted, so blaming the fetch would be false.
        if confirm "fetch tags from the remote? (uses the network)"; then
            fetch_attempted=1
            if ! run git -C "$root_dir" fetch --tags; then
                fail "$EXIT_VERSION" \
                    "\`git fetch --tags\` failed, so $requested_version cannot be verified." \
                    "Fix access to the remote (or fetch the tags yourself) and re-run."
            fi
        else
            info "skipped \`git fetch --tags\`"
        fi
        if ! git -C "$root_dir" rev-parse -q --verify "refs/tags/$requested_version" >/dev/null 2>&1; then
            if [ "$fetch_attempted" -eq 1 ]; then
                fail "$EXIT_VERSION" \
                    "tag $requested_version does not exist." \
                    "\`git fetch --tags\` ran and brought the tags up to date, so the" \
                    "remote does not publish that tag either." \
                    "Check the version you asked for against the available tags:" \
                    "  git tag -l 'v*'"
            fi
            fail "$EXIT_VERSION" \
                "tag $requested_version does not exist." \
                "The tags were not fetched: you declined, so fetching them was never" \
                "attempted and this clone may simply predate the tag." \
                "Run \`git fetch --tags\` yourself (that uses the network) and re-run," \
                "or check the version you asked for against the available tags:" \
                "  git tag -l 'v*'"
        fi
    fi

    # The order matters: HEAD is compared with the tag *before* the tree is
    # inspected, so a dirty tree at another commit is never reported as "the tag
    # is here with changes" when the tag is not in the tree at all.
    local head_tag head_commit tag_commit
    head_tag=$(git -C "$root_dir" describe --tags --exact-match HEAD 2>/dev/null || true)
    head_commit=$(git -C "$root_dir" rev-parse --verify HEAD)
    tag_commit=$(git -C "$root_dir" rev-parse --verify "refs/tags/$requested_version^{commit}")
    if [ "$head_tag" = "$requested_version" ] || [ "$tag_commit" = "$head_commit" ]; then
        # `git describe` returns one tag arbitrarily when a commit carries
        # several, so a tag that points at HEAD also satisfies the request.
        require_clean_tree
        ok "the working tree is exactly $requested_version and clean"
        return 0
    fi

    # HEAD is somewhere else. Say how far: `git rev-list --count A..B` counts
    # the commits reachable from B but not from A, so the two counts describe the
    # gap without moving anything.
    local head_short ahead behind distance description
    head_short=$(git -C "$root_dir" rev-parse --short HEAD)
    ahead=$(git -C "$root_dir" rev-list --count "refs/tags/$requested_version..HEAD" 2>/dev/null || true)
    behind=$(git -C "$root_dir" rev-list --count "HEAD..refs/tags/$requested_version" 2>/dev/null || true)
    distance=""
    if [[ $ahead =~ ^[0-9]+$ && $behind =~ ^[0-9]+$ ]] && [ $((ahead + behind)) -gt 0 ]; then
        if [ "$ahead" -gt 0 ] && [ "$behind" -gt 0 ]; then
            distance="$(commit_count "$ahead") after $requested_version and $(commit_count "$behind") before it, so the history diverged"
        elif [ "$ahead" -gt 0 ]; then
            distance="$(commit_count "$ahead") after $requested_version"
        else
            distance="$(commit_count "$behind") before $requested_version"
        fi
    fi
    if [ -n "$distance" ]; then
        description="HEAD is $distance ($head_short)"
    else
        description="HEAD is $head_short, which is not an exact tag"
    fi
    if [ -n "$head_tag" ]; then
        description="$description, tagged $head_tag"
    fi

    fail "$EXIT_VERSION" \
        "this checkout is not at $requested_version: $description." \
        "Building here would install a binary that is not $requested_version, so the" \
        "script stops. It never changes your checkout; moving the tree is your call." \
        "Either move this checkout to the release and build there:" \
        "  git checkout $requested_version" \
        "  ./scripts/setup.sh --version $requested_version --install" \
        "or build the release in a temporary worktree, leaving this tree untouched:" \
        "  git worktree add /tmp/astraterm-$requested_version $requested_version" \
        "  /tmp/astraterm-$requested_version/scripts/setup.sh --version $requested_version --install"
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
        # build tools, Wayland/X11 + font/Vulkan runtime libraries, desktop
        # notifications and git. The clipboard needs no package: it is linked
        # into the binary (arboard). The Nerd Font is *not* here on purpose:
        # `pacman -S --needed ttf-jetbrains-mono-nerd` matches on the package
        # name, not on what an installed package provides, and Omarchy's
        # ttf-jetbrains-mono-nerd-basic provides that name while conflicting
        # with it, so this list would offer to remove the distribution's own
        # font. The font step below asks fontconfig whether the family exists
        # and only then offers the package.
        packages=(
            base-devel wayland libxkbcommon fontconfig freetype2 mesa
            vulkan-icd-loader libx11 libnotify git
        )
        install_cmd=(pacman -S --needed)
        # `--yes` has to mean non-interactive end to end: pacman asks for its own
        # transaction confirmation, which would block a scripted or CI run.
        if [ "$assume_yes" -eq 1 ]; then
            install_cmd+=(--noconfirm)
        fi
        ;;
    apt)
        # Debian and Ubuntu. The Nerd Font is not packaged here; the font step
        # below points at the closest packaged family and at a manual install.
        packages=(
            build-essential pkg-config git libwayland-dev libxkbcommon-dev
            libfontconfig1-dev libfreetype6-dev libvulkan1 mesa-vulkan-drivers
            libgl1-mesa-dri libx11-dev libnotify-bin
        )
        install_cmd=(apt-get install -y --no-install-recommends)
        ;;
    dnf)
        # Fedora. Same note about the Nerd Font as for apt.
        packages=(
            gcc gcc-c++ make pkgconf-pkg-config git wayland-devel
            libxkbcommon-devel fontconfig-devel freetype-devel vulkan-loader
            mesa-vulkan-drivers libX11-devel libnotify
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

# Runs the official rustup installer. `-y` answers rustup's own prompts because
# this script already asked, and it accepts rustup's default of appending
# ~/.cargo/bin to the shell profile: that is exactly what running the pipeline
# printed in the instructions by hand does, so a later shell finds cargo too.
install_rustup() {
    curl --proto '=https' --tlsv1.2 -sSf "$RUSTUP_INSTALLER_URL" | sh -s -- -y
}

offer_rust_install() {
    # Offers the toolchain installation and returns 0 only when cargo runs
    # afterwards, so the caller can keep going instead of assuming success.
    if ! command -v curl >/dev/null 2>&1; then
        warn "curl is not installed, so the rustup installer cannot be fetched."
        info "install curl with your package manager (it is in the list above), or"
        info "install Rust yourself and re-run this script:"
        info "  curl --proto '=https' --tlsv1.2 -sSf $RUSTUP_INSTALLER_URL | sh"
        return 1
    fi

    # Spelled out here rather than left to confirm(): in a dry run the answer is
    # never a real yes, so the plan is printed and nothing is fetched.
    if [ "$dry_run" -eq 1 ]; then
        printf '  would ask: install the Rust toolchain with rustup? [y/N]\n'
        printf '  would run: curl --proto '\''=https'\'' --tlsv1.2 -sSf %s | sh -s -- -y\n' \
            "$RUSTUP_INSTALLER_URL"
        printf '  would run: export PATH="$HOME/.cargo/bin:$PATH"\n'
        info "this is the only download the script makes; a real run asks first"
        return 1
    fi

    if ! confirm "install the Rust toolchain with rustup? it downloads from $RUSTUP_INSTALLER_URL"; then
        info "skipped the toolchain installation"
        return 1
    fi

    info "this fetches the rustup installer and puts the toolchain in ~/.cargo"
    if ! install_rustup; then
        warn "the rustup installer failed (see the output above)."
        return 1
    fi

    # rustup writes to ~/.cargo/bin and only edits the shell profile for new
    # shells, so this run has to be told before the version check and the build
    # can see it.
    if [ -d "$HOME/.cargo/bin" ]; then
        PATH="$HOME/.cargo/bin:$PATH"
        export PATH
    fi

    if ! command -v cargo >/dev/null 2>&1; then
        warn "rustup finished but cargo is still not on PATH."
        info "open a new shell and re-run this script."
        return 1
    fi
    ok "cargo $(cargo --version | awk '{print $2}') installed"
    return 0
}

check_toolchain() {
    step "Rust toolchain"

    if ! command -v cargo >/dev/null 2>&1; then
        warn "cargo was not found."
        info "AstraTerm needs Rust 1.85+ (edition 2024)."
        if ! offer_rust_install; then
            fail "$EXIT_TOOLCHAIN" \
                "the Rust toolchain is still missing, so nothing can be built." \
                "Install Rust 1.85+ yourself and re-run this script:" \
                "  curl --proto '=https' --tlsv1.2 -sSf $RUSTUP_INSTALLER_URL | sh" \
                "or install your distribution's rust package, then open a new shell."
        fi
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
            "This script does not update an existing toolchain for you."
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
    local distro=$1
    case $distro in
    arch)
        info "  sudo pacman -S $NERD_FONT_PACKAGE"
        info "  (a distribution that already ships the family through another package,"
        info "  such as Omarchy's ttf-jetbrains-mono-nerd-basic, needs nothing)"
        ;;
    debian)
        info "  Debian and Ubuntu package JetBrains Mono, the fallback family, without"
        info "  the patched Nerd glyphs; there is no package for the Nerd Font itself:"
        info "  sudo apt install fonts-jetbrains-mono"
        info "  and for the icons, download a Nerd Font from https://www.nerdfonts.com/font-downloads,"
        info "  unpack it into ~/.local/share/fonts and run: fc-cache -f"
        ;;
    fedora)
        info "  Fedora packages JetBrains Mono, the fallback family, without the patched"
        info "  Nerd glyphs: sudo dnf install jetbrains-mono-fonts"
        info "  for the icons, either enable the atim/nerd-fonts copr yourself (this"
        info "  script adds no repositories) or unpack a Nerd Font release into"
        info "  ~/.local/share/fonts and run: fc-cache -f"
        ;;
    *)
        info "  install $DEFAULT_FONT_FAMILY with your package manager, or unpack a"
        info "  Nerd Font release into ~/.local/share/fonts and run: fc-cache -f"
        ;;
    esac
}

font_aliases() {
    # One family name per line from every face fontconfig knows. `fc-list` prints
    # the aliases of a face comma-separated ("JetBrainsMono Nerd Font,JetBrainsMono
    # NF"), so they are split and trimmed before anything is compared.
    fc-list : family 2>/dev/null | tr ',' '\n' | sed 's/^[[:space:]]*//; s/[[:space:]]*$//' || true
}

family_installed() {
    # Exact, case-insensitive match against one alias. A substring test would call
    # "JetBrainsMono Nerd Font Mono" a match for "JetBrainsMono Nerd Font" and
    # report the default as present when only its sibling is.
    local wanted=${1,,} alias
    while IFS= read -r alias; do
        [ -n "$alias" ] || continue
        if [ "${alias,,}" = "$wanted" ]; then
            return 0
        fi
    done < <(font_aliases)
    return 1
}

detect_fallback_family() {
    # The family this machine can actually render. A different installed Nerd Font
    # keeps the prompts' icons working, so it wins over a plain monospaced family.
    local aliases family=""
    aliases=$(font_aliases)

    if [ -n "$aliases" ]; then
        family=$(printf '%s\n' "$aliases" | grep -i -m1 'mono.*nerd font' || true)
        if [ -z "$family" ]; then
            family=$(printf '%s\n' "$aliases" | grep -i -m1 'nerd font' || true)
        fi
    fi
    if [ -z "$family" ]; then
        # Whatever fontconfig answers for the generic alias, then the alias itself
        # as a last resort: cosmic-text resolves "monospace" through the database.
        family=$(fc-match -f '%{family[0]}' monospace 2>/dev/null || true)
    fi
    if [ -z "$family" ]; then
        family="monospace"
    fi
    printf '%s' "$family"
}

nerd_capability_satisfied() {
    # `pacman -T` prints only the requirements that are *not* satisfied, so an
    # empty answer means an installed package already provides the capability.
    # Omarchy's ttf-jetbrains-mono-nerd-basic is exactly that: it provides
    # ttf-jetbrains-mono-nerd and conflicts with the packaged one, so asking
    # pacman for the package by name would offer to remove the font the
    # distribution ships.
    command -v pacman >/dev/null 2>&1 || return 1
    local missing
    missing=$(pacman -T "$NERD_FONT_PACKAGE" 2>/dev/null || true)
    [ -z "$missing" ]
}

font_config_path() {
    printf '%s/astraterm/config.toml' "${XDG_CONFIG_HOME:-$HOME/.config}"
}

offer_nerd_font_install() {
    # Returns 0 when the default family ends up visible to fontconfig, 1 when it
    # has to stay missing.
    local distro=$1 sudo_cmd
    local -a cmd=()

    case $distro in
    arch)
        if nerd_capability_satisfied; then
            info "an installed package already provides $NERD_FONT_PACKAGE"
            info "(Omarchy's ttf-jetbrains-mono-nerd-basic provides it and conflicts"
            info "with the packaged one), so pacman is left alone."
            return 1
        fi
        sudo_cmd=$(sudo_prefix)
        if [ -z "$sudo_cmd" ]; then
            warn "sudo is not installed, so the font has to be installed by hand."
            font_hint "$distro"
            return 1
        fi
        cmd=("$sudo_cmd" pacman -S --needed)
        # `--yes` has to mean non-interactive end to end here too: pacman asks its
        # own transaction confirmation, which would block a scripted or CI run.
        if [ "$assume_yes" -eq 1 ]; then
            cmd+=(--noconfirm)
        fi
        cmd+=("$NERD_FONT_PACKAGE")
        ;;
    *)
        # apt and dnf at most package the plain JetBrains Mono family, which does
        # not carry the Nerd glyphs, and the Nerd Font itself only comes from a
        # third-party repository. This script adds none, so there is nothing to
        # offer: say what to do instead of pretending a package would fix it.
        info "this distribution has no packaged $DEFAULT_FONT_FAMILY:"
        font_hint "$distro"
        return 1
        ;;
    esac

    if [ "$dry_run" -eq 1 ]; then
        printf '  would ask: install %s with %s? [Y/n]\n' "$NERD_FONT_PACKAGE" "${cmd[1]}"
        printf '  would run:'
        printf ' %q' "${cmd[@]}"
        printf '\n'
        info "declining it falls through to the family this machine already has"
        return 1
    fi

    if ! confirm "install $NERD_FONT_PACKAGE with ${cmd[1]}?"; then
        info "skipped the font installation"
        return 1
    fi

    if ! run "${cmd[@]}"; then
        warn "the font installation failed (see the output above)."
        return 1
    fi
    if family_installed "$DEFAULT_FONT_FAMILY"; then
        ok "$DEFAULT_FONT_FAMILY found"
    else
        warn "the package is installed but $DEFAULT_FONT_FAMILY is not visible to fontconfig yet."
        info "log out and back in (or run: fc-cache -f) and re-run this script."
    fi
    return 0
}

offer_fallback_config() {
    # The family is only half the story: the grid and the shaper are built from
    # the family the configuration names, so a machine without the default has to
    # be told about the one it has. An existing configuration is never touched.
    local family=${1//\"/} path
    path=$(font_config_path)

    if [ -e "$path" ]; then
        info "your configuration already exists and is never modified:"
        info "  $path"
        info "to use it, set this there:"
        info "  [font]"
        info "  family = \"$family\""
        return 0
    fi

    if [ "$dry_run" -eq 1 ]; then
        printf '  would ask: create %s with font.family = "%s"? [Y/n]\n' "$path" "$family"
        return 0
    fi

    if ! confirm "create $path with font.family = \"$family\"?"; then
        info "left the default in place: set [font] family in $path when you want it"
        return 0
    fi

    # A file with one section is a valid configuration: every other key of
    # FontConfig keeps its default through #[serde(default)].
    local dir=${path%/*}
    if ! mkdir -p -- "$dir" 2>/dev/null; then
        warn "could not create $dir"
        info "set it yourself in some config.toml: [font] family = \"$family\""
        return 0
    fi
    if ! {
        printf '# Written by scripts/setup.sh: %s was not installed, so this file\n' \
            "$DEFAULT_FONT_FAMILY"
        printf '# names a family this machine already has. Every other key keeps its\n'
        printf '# default; see docs/CONFIGURATION.md for the rest.\n'
        printf '[font]\nfamily = "%s"\n' "$family"
    } >"$path" 2>/dev/null; then
        warn "could not write $path"
        info "set it yourself: [font] family = \"$family\""
        return 0
    fi
    ok "$path"
    info "restart AstraTerm to use it"
}

check_font() {
    local distro=$1 fallback
    step "Font"

    if ! command -v fc-list >/dev/null 2>&1; then
        warn "fontconfig (fc-list) is not installed, so the font cannot be checked."
        font_hint "$distro"
        return 0
    fi

    if family_installed "$DEFAULT_FONT_FAMILY"; then
        ok "$DEFAULT_FONT_FAMILY found"
        return 0
    fi

    warn "$DEFAULT_FONT_FAMILY not found: text falls back to another family and glyphs may be missing."

    if offer_nerd_font_install "$distro"; then
        return 0
    fi

    fallback=$(detect_fallback_family)
    info "family already installed on this machine: $fallback"
    case ${fallback,,} in
    *"nerd font"*) ;;
    *) info "it has no Nerd glyphs, so prompt icons can still be missing" ;;
    esac
    offer_fallback_config "$fallback"
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
    # The build is the only step that can be skipped, so a missing binary is the one
    # case where the install has nothing to do. It is reported as the skip it is —
    # in a dry run too, so the printed plan never promises an install that a real
    # run would not perform — instead of letting the installer fail on a missing
    # file. It stays a skip, not a failure: the build step already reported that.
    if [ ! -x "$root_dir/target/release/astraterm" ]; then
        if [ "$dry_run" -eq 1 ]; then
            info "would skip the install: target/release/astraterm does not exist yet"
        else
            warn "target/release/astraterm is missing, so nothing was installed."
            info "drop --no-build (or build it) and re-run this script."
        fi
        return 0
    fi
    info "packaging/linux/install.sh writes the binary, the desktop entry and every"
    info "icon size to your XDG directories; it needs no root and never touches"
    info "~/.config/astraterm."
    # Called directly, never behind an `if`: a failing installer has to stop the
    # script with its own status. Read as a condition it would look like "nothing to
    # do", and the closing summary would report success for a failed install.
    run sh "$root_dir/packaging/linux/install.sh"
    install_done=1
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

    # Before any package or build step: a wrong version must not install
    # anything, and this is also where a `--version` request is verified.
    check_requested_version

    manager=$(detect_manager || true)
    if [ -n "$manager" ]; then
        install_system_packages "$manager" "$distro_id"
    else
        warn "no supported package manager found (pacman, apt, dnf)."
        info "install these capabilities with your own package manager:"
        info "  Rust 1.85+, a C toolchain, pkg-config; wayland + libxkbcommon,"
        info "  fontconfig + freetype2, a Vulkan ICD (Mesa is enough), libX11;"
        info "  libnotify, git, and a Nerd Font (default: $DEFAULT_FONT_FAMILY)"
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
    elif [ "$install_done" -eq 1 ]; then
        if [ -n "$requested_version" ]; then
            info "astraterm $requested_version is installed: run \`astraterm\` or pick \"AstraTerm\" from the application menu"
        else
            info "astraterm is installed: run \`astraterm\` or pick \"AstraTerm\" from the application menu"
        fi
    elif [ "$install_binary" -eq 1 ]; then
        info "nothing was installed; run this script without --no-build to build it first"
    else
        info "not installed (--no-install): run ./target/release/astraterm, or re-run without --no-install"
    fi
    return "$EXIT_OK"
}

main "$@"
