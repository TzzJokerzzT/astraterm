#!/usr/bin/env bash
# Repeatable checks for the `--version` paths and the font and toolchain steps
# of scripts/setup.sh.
#
# Every case builds a throwaway git repository in `mktemp -d` with a stub
# `Cargo.toml` and a copy of `scripts/setup.sh`, then runs that copy. Nothing is
# compiled, no network is used (the fixtures have no remote at all) and the real
# checkout is never touched: the script only reads the real one to copy it.
#
# The font cases replace `fc-list`/`fc-match` inside the fixture with stubs, so
# the step is decided by the fixture and never by the fonts of the machine
# running the tests. `XDG_CONFIG_HOME` points into the fixture too: the fallback
# configuration this script may create can never land in the real `~/.config`.
#
# Usage:
#   ./scripts/tests/version-check.sh
#
# Exit codes: 0 every case passed, 1 at least one failed.
#
# The version cases run with `--dry-run --no-build`, so the only things that
# execute outside the version check are read-only probes (`cargo --version`,
# `fc-list`, `command -v`); the successful case therefore needs cargo on PATH to
# reach exit 0, exactly like a real run would. The font case that has to write a
# file uses `--yes` (never asks, so it cannot block on a tty) together with stub
# `sudo` and `pacman` commands that only print their arguments.

set -uo pipefail
# Not `set -e`: a failing check is the expected outcome of most cases.

readonly SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
readonly REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
readonly SETUP=$REPO_ROOT/scripts/setup.sh

tmp_dir=""
total=0
passed=0
failed=0
last_code=0
last_output=""

cleanup() {
    if [ -n "$tmp_dir" ] && [ -d "$tmp_dir" ]; then
        rm -rf -- "$tmp_dir"
    fi
}
trap cleanup EXIT

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

# The fixtures are the only thing this script may write to; assert it before any
# `git` command touches a path, so a mistake in a path cannot reach the checkout.
assert_inside_tmp() {
    local path=$1 resolved
    resolved=$(CDPATH= cd -- "$path" && pwd -P) || die "cannot resolve $path"
    case $resolved in
    "$tmp_dir"/*) ;;
    *) die "refusing to touch $resolved: it is not inside $tmp_dir" ;;
    esac
}

# make_repo <name>: prints the path of a fresh fixture repository.
#
# History: commit "base" tagged v1.0.0, then commit "second" tagged v2.0.0, so a
# case can stand at either tag, one commit after v1.0.0 or one before v2.0.0.
make_repo() {
    local repo=$tmp_dir/$1
    mkdir -p "$repo/scripts"
    printf '[package]\nname = "astraterm"\nversion = "1.1.1"\nedition = "2021"\n' \
        >"$repo/Cargo.toml"
    cp "$SETUP" "$repo/scripts/setup.sh"
    chmod +x "$repo/scripts/setup.sh"
    assert_inside_tmp "$repo"

    # A local identity and no signing: the fixture must not depend on the
    # machine's git config (nor on a gpg key that is not there).
    git -C "$repo" init -q --initial-branch=main
    git -C "$repo" config user.name "AstraTerm harness"
    git -C "$repo" config user.email "harness@example.invalid"
    git -C "$repo" config commit.gpgsign false
    git -C "$repo" config tag.gpgSign false
    git -C "$repo" add Cargo.toml scripts/setup.sh
    git -C "$repo" commit -q -m "base"
    git -C "$repo" tag -a v1.0.0 -m "v1.0.0"
    printf 'second\n' >"$repo/NOTES.md"
    git -C "$repo" add NOTES.md
    git -C "$repo" commit -q -m "second"
    git -C "$repo" tag -a v2.0.0 -m "v2.0.0"

    printf '%s\n' "$repo"
}

# Detach the fixture at a commit without printing git's advice; the real
# checkout is never involved (assert_inside_tmp guards every fixture path).
fixture_checkout() {
    local repo=$1 ref=$2
    assert_inside_tmp "$repo"
    git -C "$repo" -c advice.detachedHead=false checkout -q --detach "$ref"
}

# invoke <repo> <setup-args>...: runs the fixture's copy of setup.sh, capturing
# both streams and the exit code. The caller's git environment must not leak in,
# the fixture's `bin/` (when a case created one) decides which fontconfig and
# package commands the copy sees, and `XDG_CONFIG_HOME` keeps any configuration
# the font step writes inside the fixture.
invoke() {
    local repo=$1
    shift
    last_output=$(
        cd "$repo" || exit 1
        env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE \
            GIT_TERMINAL_PROMPT=0 GIT_ASKPASS= \
            XDG_CONFIG_HOME="$repo/config" \
            XDG_BIN_HOME="$repo/xdg/bin" \
            XDG_DATA_HOME="$repo/xdg/data" \
            PATH="$repo/bin:$PATH" \
            "$repo/scripts/setup.sh" "$@" </dev/null 2>&1
    )
    last_code=$?
}

# hide_commands: extra command names the next invoke_without_cargo leaves out of
# the fixture PATH, on top of cargo. Reset it to empty after that call.
hide_commands=""

# build_minbin <repo> <hide>: prints a directory of symlinks to every executable
# this machine resolves except `cargo` and <hide>. Dropping whole PATH entries
# would not do: cargo lives in /usr/bin here, next to bash and git.
build_minbin() {
    local repo=$1 hide=$2 minbin dir f name
    local -a entries=()
    minbin=$repo/minbin${hide:+-no-$hide}
    mkdir -p "$minbin"
    assert_inside_tmp "$minbin"
    IFS=: read -r -a entries <<<"$PATH"
    for dir in "${entries[@]}"; do
        [ -d "$dir" ] || continue
        for f in "$dir"/*; do
            # An unmatched glob stays literal; a dangling symlink still owns its name.
            [ -e "$f" ] || [ -L "$f" ] || continue
            name=${f##*/}
            if [ "$name" = cargo ] || { [ -n "$hide" ] && [ "$name" = "$hide" ]; }; then
                continue
            fi
            if [ -e "$minbin/$name" ] || [ -L "$minbin/$name" ]; then
                continue
            fi
            ln -s "$f" "$minbin/$name"
        done
    done
    printf '%s' "$minbin"
}

# invoke_without_cargo <repo> <setup-args>...: same as invoke, on a PATH that
# resolves every tool the machine has except `cargo` (and $hide_commands), so the
# copy runs as on a machine with no toolchain. HOME points into the fixture and
# stdin is /dev/null: an accepted offer installs into the fixture instead of the
# real ~/.cargo, and a prompt can never block the suite — which is what makes the
# decline case deterministic even when the suite is started from a terminal.
invoke_without_cargo() {
    local repo=$1 minbin
    shift
    mkdir -p "$repo/home"
    assert_inside_tmp "$repo/home"
    minbin=$(build_minbin "$repo" "$hide_commands")
    last_output=$(
        cd "$repo" || exit 1
        env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE \
            GIT_TERMINAL_PROMPT=0 GIT_ASKPASS= \
            HOME="$repo/home" \
            XDG_CONFIG_HOME="$repo/config" \
            XDG_BIN_HOME="$repo/xdg/bin" \
            XDG_DATA_HOME="$repo/xdg/data" \
            PATH="$repo/bin:$minbin" \
            "$repo/scripts/setup.sh" "$@" </dev/null 2>&1
    )
    last_code=$?
}

# font_stub <repo> [family-line]...: installs an `fc-list` that reports exactly
# these family lines and an `fc-match` that answers with the last of them, so a
# case decides what the font step sees instead of inheriting the fonts of the
# machine that runs the tests. A line may carry comma-separated aliases, the way
# `fc-list` prints them.
font_stub() {
    local repo=$1
    shift
    local family
    mkdir -p "$repo/bin"
    assert_inside_tmp "$repo/bin"
    {
        printf '#!/bin/sh\n'
        for family in "$@"; do
            printf "printf '%%s\\\\n' '%s'\n" "$family"
        done
        printf 'exit 0\n'
    } >"$repo/bin/fc-list"
    {
        printf '#!/bin/sh\n'
        if [ "$#" -gt 0 ]; then
            printf "printf '%%s' '%s'\n" "$family"
        else
            printf "printf '%%s' 'monospace'\n"
        fi
        printf 'exit 0\n'
    } >"$repo/bin/fc-match"
    chmod +x "$repo/bin/fc-list" "$repo/bin/fc-match"
}

# noop_stub <repo>: `sudo` and `pacman` that only report their arguments. The
# package step therefore succeeds without touching the machine, and `pacman -T`
# answers "satisfied" — the Omarchy case, where an installed package already
# provides the Nerd Font and nothing may be installed.
noop_stub() {
    local repo=$1
    mkdir -p "$repo/bin"
    assert_inside_tmp "$repo/bin"
    printf '#!/bin/sh\necho "(stub sudo) $*"\nexit 0\n' >"$repo/bin/sudo"
    printf '#!/bin/sh\nexit 0\n' >"$repo/bin/pacman"
    chmod +x "$repo/bin/sudo" "$repo/bin/pacman"
}

# install_stub <repo>: the same `sudo`, and a `pacman` whose `-T` reports the
# capability as missing, so the font step really offers the package. Nothing is
# executed: `sudo` only echoes the command line it was given.
install_stub() {
    local repo=$1
    mkdir -p "$repo/bin"
    assert_inside_tmp "$repo/bin"
    printf '#!/bin/sh\necho "(stub sudo) $*"\nexit 0\n' >"$repo/bin/sudo"
    {
        printf '#!/bin/sh\n'
        printf 'case "$1" in\n'
        printf -- '-T) printf "%%s\\n" "ttf-jetbrains-mono-nerd"; exit 1 ;;\n'
        printf 'esac\n'
        printf 'exit 0\n'
    } >"$repo/bin/pacman"
    chmod +x "$repo/bin/sudo" "$repo/bin/pacman"
}

# rustup_stub <repo> <mode>: a `curl` that never reaches the network. `ok`
# leaves the cargo the real installer would have produced in the fixture HOME,
# `fail` exits like a refused download, `empty` succeeds without installing
# anything — the case where rustup claims success and changes nothing.
rustup_stub() {
    local repo=$1 mode=$2
    mkdir -p "$repo/bin"
    assert_inside_tmp "$repo/bin"
    case $mode in
    ok)
        cat >"$repo/bin/curl" <<'STUB'
#!/bin/sh
mkdir -p "$HOME/.cargo/bin"
cat >"$HOME/.cargo/bin/cargo" <<'CARGO'
#!/bin/sh
echo "cargo 1.90.0 (fake 2025-01-01)"
CARGO
chmod +x "$HOME/.cargo/bin/cargo"
exit 0
STUB
        ;;
    fail) printf '#!/bin/sh\nexit 7\n' >"$repo/bin/curl" ;;
    empty) printf '#!/bin/sh\nexit 0\n' >"$repo/bin/curl" ;;
    esac
    chmod +x "$repo/bin/curl"
}

# expect <case-name> <expected-code> [pattern]...
#
# A pattern is a substring the combined output must contain; a pattern prefixed
# with `!` is a substring it must not contain.
expect() {
    local name=$1 code=$2
    shift 2
    local -a problems=()
    local pattern line

    total=$((total + 1))
    if [ "$last_code" -ne "$code" ]; then
        problems+=("exit code $last_code, expected $code")
    fi
    for pattern in "$@"; do
        case $pattern in
        "!"*)
            if [[ $last_output == *"${pattern#!}"* ]]; then
                problems+=("output must not contain '${pattern#!}'")
            fi
            ;;
        *)
            if [[ $last_output != *"$pattern"* ]]; then
                problems+=("output must contain '$pattern'")
            fi
            ;;
        esac
    done

    if [ ${#problems[@]} -eq 0 ]; then
        passed=$((passed + 1))
        printf 'PASS  %s (exit %s)\n' "$name" "$last_code"
        return 0
    fi
    failed=$((failed + 1))
    printf 'FAIL  %s (exit %s)\n' "$name" "$last_code"
    local problem
    for problem in "${problems[@]}"; do
        printf '        - %s\n' "$problem"
    done
    printf '        --- output ---\n'
    while IFS= read -r line; do
        printf '        %s\n' "$line"
    done <<<"$last_output"
    printf '        --------------\n'
}

# expect_no_file <case-name> <path>: a dry run wrote nothing where nothing should be.
expect_no_file() {
    local name=$1 path=$2
    total=$((total + 1))
    if [ -e "$path" ]; then
        failed=$((failed + 1))
        printf 'FAIL  %s (file exists: %s)\n' "$name" "$path"
        return 1
    fi
    passed=$((passed + 1))
    printf 'PASS  %s (not written)\n' "$name"
}

# expect_file <case-name> <path> [line]...: the file exists and holds each line;
# a line prefixed with `!` must not appear.
expect_file() {
    local name=$1 path=$2
    shift 2
    local -a problems=()
    local line problem

    total=$((total + 1))
    if [ ! -f "$path" ]; then
        failed=$((failed + 1))
        printf 'FAIL  %s (no file at %s)\n' "$name" "$path"
        return 1
    fi
    for line in "$@"; do
        case $line in
        "!"*)
            if grep -qxF -- "${line#!}" "$path"; then
                problems+=("file must not contain '${line#!}'")
            fi
            ;;
        *)
            if ! grep -qxF -- "$line" "$path"; then
                problems+=("file must contain '$line'")
            fi
            ;;
        esac
    done

    if [ ${#problems[@]} -eq 0 ]; then
        passed=$((passed + 1))
        printf 'PASS  %s\n' "$name"
        return 0
    fi
    failed=$((failed + 1))
    printf 'FAIL  %s\n' "$name"
    for problem in "${problems[@]}"; do
        printf '        - %s\n' "$problem"
    done
    return 1
}

main() {
    [ -f "$SETUP" ] || die "cannot find scripts/setup.sh next to this script"
    if [ "$(id -u)" -eq 0 ]; then
        die "run this harness as a normal user: setup.sh refuses to run as root"
    fi

    tmp_dir=$(mktemp -d) || die "cannot create a temporary directory"
    tmp_dir=$(CDPATH= cd -- "$tmp_dir" && pwd -P) || die "cannot resolve $tmp_dir"
    case $tmp_dir in
    "$REPO_ROOT" | "$REPO_ROOT"/*)
        die "$tmp_dir is inside the checkout: put the temporary directory elsewhere"
        ;;
    esac
    printf 'fixtures: %s\n\n' "$tmp_dir"

    local repo

    # 1. HEAD exactly at the tag: the only success path.
    repo=$(make_repo at-tag)
    fixture_checkout "$repo" v1.0.0
    invoke "$repo" --dry-run --no-build --version v1.0.0
    expect "HEAD at the tag" 0 \
        "the working tree is exactly v1.0.0 and clean"

    # 2. HEAD one commit after the tag: the message teaches the distance and
    #    offers both remedies instead of only "not an exact tag".
    repo=$(make_repo after)
    invoke "$repo" --dry-run --no-build --version v1.0.0
    expect "HEAD one commit after the tag" 4 \
        "this checkout is not at v1.0.0" \
        "1 commit after v1.0.0" \
        "git checkout v1.0.0" \
        "git worktree add /tmp/astraterm-v1.0.0 v1.0.0" \
        "/tmp/astraterm-v1.0.0/scripts/setup.sh --version v1.0.0" \
        "!which is not an exact tag"

    # 3. HEAD one commit before the tag.
    repo=$(make_repo before)
    fixture_checkout "$repo" HEAD~1
    invoke "$repo" --dry-run --no-build --version v2.0.0
    expect "HEAD one commit before the tag" 4 \
        "1 commit before v2.0.0" \
        "git worktree add /tmp/astraterm-v2.0.0 v2.0.0"

    # 4. Diverged history: HEAD carries a commit the tag does not have, and the
    #    tag carries one HEAD does not have.
    repo=$(make_repo diverged)
    assert_inside_tmp "$repo"
    git -C "$repo" -c advice.detachedHead=false checkout -q -b side v1.0.0
    printf 'side\n' >"$repo/side.txt"
    git -C "$repo" add side.txt
    git -C "$repo" commit -q -m "side"
    invoke "$repo" --dry-run --no-build --version v2.0.0
    expect "diverged history" 4 \
        "1 commit after v2.0.0 and 1 commit before it, so the history diverged" \
        "git worktree add /tmp/astraterm-v2.0.0 v2.0.0"

    # 5. The ordering itself (S3.2): a dirty tree at a commit that is *not* the
    #    tag must be reported as "not at that version", never as "the tag is
    #    here with changes".
    repo=$(make_repo dirty-at-other-commit)
    printf 'local only\n' >"$repo/local-only.txt"
    invoke "$repo" --dry-run --no-build --version v1.0.0
    expect "dirty tree at another commit blames the checkout, not the tree" 4 \
        "this checkout is not at v1.0.0" \
        "1 commit after v1.0.0" \
        "!is checked out, but the working tree has changes"

    # 6. A modified tracked file at the tag: the failure is about the tree, not
    #    about the tag being absent.
    repo=$(make_repo dirty-tracked)
    fixture_checkout "$repo" v2.0.0
    printf '# local edit\n' >>"$repo/Cargo.toml"
    invoke "$repo" --dry-run --no-build --version v2.0.0
    expect "modified tracked file at the tag" 4 \
        "HEAD is v2.0.0, but the working tree has changes" \
        "Cargo.toml" \
        "Commit, stash or remove" \
        "!this checkout is not at v2.0.0" \
        "!does not exist"

    # 7. An untracked file at the tag is a dirty tree too.
    repo=$(make_repo dirty-untracked)
    fixture_checkout "$repo" v2.0.0
    : >"$repo/stray-untracked.txt"
    invoke "$repo" --dry-run --no-build --version v2.0.0
    expect "untracked file at the tag" 4 \
        "HEAD is v2.0.0, but the working tree has changes" \
        "stray-untracked.txt" \
        "!this checkout is not at v2.0.0"

    # 8. A missing tag under --dry-run fails without touching the network: the
    #    fetch is only described, and the fixture has no remote to reach anyway.
    repo=$(make_repo missing-tag)
    invoke "$repo" --dry-run --no-build --version v0.0.9
    expect "missing tag with --dry-run" 4 \
        "would run: git fetch --tags" \
        "--dry-run never touches the network" \
        "!running: git fetch" \
        "!tag v0.0.9 does not exist"

    # 9. `--version` without a value is a usage error, not a version failure.
    repo=$(make_repo no-value)
    invoke "$repo" --dry-run --no-build --version
    expect "--version without a value" 2 \
        "--version needs a tag" \
        "Run ./scripts/setup.sh --help"

    # 10. Unchanged behaviour: without --version nothing is verified, so a dirty
    #    tree is not a failure.
    repo=$(make_repo no-version)
    printf '# local edit\n' >>"$repo/Cargo.toml"
    invoke "$repo" --dry-run --no-build
    expect "no --version: the version check is skipped" 0 \
        "!Requested version"

    # 11. Unchanged behaviour: the leading `v` stays optional on success.
    repo=$(make_repo short-form)
    fixture_checkout "$repo" v2.0.0
    invoke "$repo" --dry-run --no-build --version 2.0.0
    expect "--version without the leading v" 0 \
        "the working tree is exactly v2.0.0 and clean"

    # 12. The default family is reported and nothing is offered: the font step is
    #     decided by fontconfig, never by the distribution. A machine that already
    #     has the family is never asked to install a package for it, which is what
    #     keeps Omarchy's ttf-jetbrains-mono-nerd-basic out of a pacman conflict.
    repo=$(make_repo font-present)
    font_stub "$repo" \
        'JetBrainsMono Nerd Font,JetBrainsMono NF' \
        'JetBrainsMono Nerd Font Mono,JetBrainsMono NFM'
    invoke "$repo" --dry-run --no-build
    expect "font present: reported, nothing offered" 0 \
        "ok: JetBrainsMono Nerd Font found" \
        "!would ask: create" \
        "!would ask: install" \
        "!not found: text falls back"

    # 13. The sibling family is not the default one: the alias match has to be
    #     exact where a substring test over `fc-list` would have called it present.
    repo=$(make_repo font-sibling-only)
    font_stub "$repo" 'JetBrainsMono Nerd Font Mono,JetBrainsMono NFM'
    invoke "$repo" --dry-run --no-build
    expect "only the Mono sibling installed" 0 \
        "JetBrainsMono Nerd Font not found" \
        "family already installed on this machine: JetBrainsMono Nerd Font Mono" \
        "would ask: create"
    expect_no_file "a dry run writes no configuration" \
        "$repo/config/astraterm/config.toml"

    # 14. No font at all: the fallback is what fontconfig answers for the generic
    #     monospace alias, and the icon warning is spelled out.
    repo=$(make_repo font-none)
    font_stub "$repo"
    invoke "$repo" --dry-run --no-build
    expect "no font at all: monospace fallback" 0 \
        "JetBrainsMono Nerd Font not found" \
        "family already installed on this machine: monospace" \
        "it has no Nerd glyphs, so prompt icons can still be missing" \
        "would ask: create"

    # 15. --yes accepts the fallback configuration, and the file it writes is the
    #     one the fixture's XDG_CONFIG_HOME names: the real ~/.config is never a
    #     candidate here.
    repo=$(make_repo font-fallback-config)
    font_stub "$repo"
    noop_stub "$repo"
    invoke "$repo" --yes --no-build
    expect "the fallback configuration is created" 0 \
        "ok: $repo/config/astraterm/config.toml"
    expect_file "the written configuration names the installed family" \
        "$repo/config/astraterm/config.toml" \
        "[font]" \
        'family = "monospace"'

    # 16. An existing configuration is never modified, whatever the font step
    #     would have offered.
    printf '[font]\nfamily = "Mine"\n' >"$repo/config/astraterm/config.toml"
    invoke "$repo" --yes --no-build
    expect "an existing configuration is left alone" 0 \
        "your configuration already exists and is never modified"
    expect_file "the existing family survives" \
        "$repo/config/astraterm/config.toml" \
        'family = "Mine"' \
        '!family = "monospace"'

    # 17. `--yes` stays non-interactive end to end: once the script's own question
    #     is auto-accepted, pacman's own transaction confirmation must not appear.
    repo=$(make_repo font-install-noconfirm)
    font_stub "$repo"
    install_stub "$repo"
    invoke "$repo" --yes --no-build
    expect "--yes installs the font package without pacman prompting" 0 \
        "(stub sudo) pacman -S --needed --noconfirm ttf-jetbrains-mono-nerd"

    # 18. A machine without cargo is not a dead end: the toolchain step offers the
    #     rustup installation, and a dry run prints the plan without fetching
    #     anything, so no case in this file touches the network.
    repo=$(make_repo toolchain-missing-dry-run)
    font_stub "$repo" 'JetBrainsMono Nerd Font'
    invoke_without_cargo "$repo" --dry-run --no-build
    expect "missing toolchain: the rustup offer is planned" 3 \
        "cargo was not found" \
        "would ask: install the Rust toolchain with rustup?" \
        "would run: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs" \
        "!running: curl"

    # 19. Declining it is not a silent success: stdin is /dev/null here, which
    #     answers no, and the script says what to do and exits 3.
    invoke_without_cargo "$repo" --no-build
    expect "missing toolchain: declining still exits 3" 3 \
        "stdin is not a terminal: assuming no" \
        "skipped the toolchain installation" \
        "the Rust toolchain is still missing" \
        "!running: curl"

    # 20. An accepted install: the stub leaves what rustup would have installed,
    #     and the very same run has to see it — otherwise the build would run
    #     without a toolchain.
    repo=$(make_repo toolchain-install)
    font_stub "$repo" 'JetBrainsMono Nerd Font'
    noop_stub "$repo"
    rustup_stub "$repo" ok
    invoke_without_cargo "$repo" --yes --no-build
    expect "missing toolchain: an accepted install serves the same run" 0 \
        "ok: cargo 1.90.0 installed" \
        "this fetches the rustup installer and puts the toolchain in ~/.cargo" \
        "skipped (--no-build)"

    # 21. A failing installer is reported as a failure, never as an install.
    repo=$(make_repo toolchain-install-fails)
    font_stub "$repo" 'JetBrainsMono Nerd Font'
    noop_stub "$repo"
    rustup_stub "$repo" fail
    invoke_without_cargo "$repo" --yes --no-build
    expect "missing toolchain: a failed install exits 3" 3 \
        "the rustup installer failed" \
        "!installed"

    # 22. rustup succeeded and left nothing on PATH: a failure with its own
    #     message, and no crash on the directory that was never created.
    repo=$(make_repo toolchain-install-empty)
    font_stub "$repo" 'JetBrainsMono Nerd Font'
    noop_stub "$repo"
    rustup_stub "$repo" empty
    invoke_without_cargo "$repo" --yes --no-build
    expect "missing toolchain: an empty install is reported" 3 \
        "rustup finished but cargo is still not on PATH" \
        "the Rust toolchain is still missing"

    # 23. No curl: the offer cannot be honoured, and it says so instead of
    #     offering a download it could not perform.
    repo=$(make_repo toolchain-no-curl)
    font_stub "$repo" 'JetBrainsMono Nerd Font'
    hide_commands=curl
    invoke_without_cargo "$repo" --dry-run --no-build
    hide_commands=""
    expect "missing toolchain: without curl the offer explains itself" 3 \
        "curl is not installed" \
        "!would ask: install the Rust toolchain"

    # 24. The point of the script: after one run the terminal is usable from PATH
    #     and from the application menu. The fixture carries the release binary the
    #     build would have produced, and the XDG directories the install writes to
    #     are the fixture's own, never the real ~/.local.
    repo=$(make_repo install-default)
    font_stub "$repo" 'JetBrainsMono Nerd Font'
    noop_stub "$repo"
    mkdir -p "$repo/target/release" "$repo/packaging" "$repo/assets/icons"
    printf '#!/bin/sh\nexit 0\n' >"$repo/target/release/astraterm"
    chmod +x "$repo/target/release/astraterm"
    cp -r "$REPO_ROOT/packaging/." "$repo/packaging/"
    cp -r "$REPO_ROOT/assets/icons/hicolor" "$repo/assets/icons/"
    invoke "$repo" --no-build
    expect "the default run installs for the user" 0 \
        "installed $repo/xdg/bin/astraterm" \
        "installed $repo/xdg/data/applications/astraterm.desktop" \
        "installed $repo/xdg/data/icons/hicolor/128x128/apps/astraterm.png" \
        "it now appears in the application menu" \
        "is installed: run" \
        "pick \"AstraTerm\" from the application menu"

    # 25. --no-install leaves the build in the tree and touches no XDG directory.
    repo=$(make_repo install-skipped)
    font_stub "$repo" 'JetBrainsMono Nerd Font'
    noop_stub "$repo"
    invoke "$repo" --no-build --no-install
    expect "--no-install leaves the tree alone" 0 \
        "not installed (--no-install)" \
        "!it now appears in the application menu"
    expect_no_file "--no-install writes no binary" "$repo/xdg/bin/astraterm"

    # 26. A failing installer is a failure: the script stops with the installer's
    #     own status instead of falling through to the closing summary, which would
    #     report a success that never happened.
    repo=$(make_repo install-fails)
    font_stub "$repo" 'JetBrainsMono Nerd Font'
    noop_stub "$repo"
    mkdir -p "$repo/target/release" "$repo/packaging/linux"
    printf '#!/bin/sh\nexit 0\n' >"$repo/target/release/astraterm"
    chmod +x "$repo/target/release/astraterm"
    printf '#!/bin/sh\nexit 9\n' >"$repo/packaging/linux/install.sh"
    invoke "$repo" --no-build
    expect "a failing installer stops the script" 9 \
        "running: sh $repo/packaging/linux/install.sh" \
        "!nothing was installed" \
        "!==> Done"

    printf '\n%d checks: %d passed, %d failed\n' "$total" "$passed" "$failed"
    if [ "$failed" -ne 0 ]; then
        exit 1
    fi
    return 0
}

main "$@"
