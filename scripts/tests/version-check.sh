#!/usr/bin/env bash
# Repeatable checks for the `--version` paths of scripts/setup.sh.
#
# Every case builds a throwaway git repository in `mktemp -d` with a stub
# `Cargo.toml` and a copy of `scripts/setup.sh`, then runs that copy. Nothing is
# compiled, no network is used (the fixtures have no remote at all) and the real
# checkout is never touched: the script only reads the real one to copy it.
#
# Usage:
#   ./scripts/tests/version-check.sh
#
# Exit codes: 0 every case passed, 1 at least one failed.
#
# The cases run with `--dry-run --no-build`, so the only things that execute
# outside the version check are read-only probes (`cargo --version`, `fc-list`,
# `command -v`); the successful case therefore needs cargo on PATH to reach
# exit 0, exactly like a real run would.

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
# both streams and the exit code. The caller's git environment must not leak in.
invoke() {
    local repo=$1
    shift
    last_output=$(
        cd "$repo" || exit 1
        env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE \
            GIT_TERMINAL_PROMPT=0 GIT_ASKPASS= \
            "$repo/scripts/setup.sh" "$@" 2>&1
    )
    last_code=$?
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

    printf '\n%d checks: %d passed, %d failed\n' "$total" "$passed" "$failed"
    if [ "$failed" -ne 0 ]; then
        exit 1
    fi
    return 0
}

main "$@"
