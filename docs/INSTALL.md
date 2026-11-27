# Installation

Two ways in: run the bootstrap script, or install the packages yourself and
build. Both end with the same binary, `target/release/astraterm`.

```sh
git clone <repository> astraterm
cd astraterm
./scripts/setup.sh              # asks before installing packages, then builds
./scripts/setup.sh --install    # … and puts the binary on your PATH
```

## What `scripts/setup.sh` does

In order, on every run:

1. **System packages.** Detects `pacman`, `apt-get` or `dnf`, prints the exact
   package set for that distribution and asks before installing it. The install
   command is idempotent (`pacman -S --needed`, `apt-get install`,
   `dnf install`), so a second run on a prepared machine changes nothing. It
   never refreshes package lists on Arch and never starts a partial upgrade; on
   Debian/Ubuntu it runs `apt-get update` right before the install, because apt
   needs current lists to resolve the packages.
2. **Rust toolchain.** Requires `cargo` and reports the toolchain version.
   AstraTerm needs **Rust 1.85+** (edition 2024). If cargo is missing or older,
   the script prints the `rustup` instructions and exits with code `3`; it never
   downloads or installs a toolchain on its own. It also checks that `cc` and
   `pkg-config` are present, because crates with C dependencies (`fontconfig`,
   `freetype`, `wayland`) need them.
3. **Font.** Checks that the default family `JetBrainsMono Nerd Font` is visible
   to fontconfig and prints the per-distribution way to install it. It never
   downloads font files.
4. **Desktop integration.** Checks `notify-send` (libnotify). It is a runtime
   feature — save and reload notifications are lost without it — so a missing
   one is a warning, not a failure. Clipboard copy and paste need no helper
   program.
5. **Build.** Runs `cargo build --release --locked` from the repository root.
6. **Install (optional).** With `--install`, runs
   [packaging/linux/install.sh](#installing-for-the-current-user).

With `--version <tag>` a version check runs before step 1 and stops the script
if the checkout is not at that release; see
[Installing a specific release](#installing-a-specific-release---version).

### Flags

| Flag | Effect |
|---|---|
| *(none)* | Check, ask before installing packages, build |
| `--dry-run` | Print every command that would change the system or the tree and execute none of them |
| `--yes`, `-y` | Do not ask for confirmation before installing packages (`sudo` still asks for your password) |
| `--install` | After a successful build, install the binary, desktop entry and icons for the current user |
| `--no-build` | Skip `cargo build --release --locked` |
| `--version <tag>` | Build a specific release (`v1.0.0`, or `1.0.0`). The tag must exist, `HEAD` must be exactly at it and the working tree must be clean; the script stops with code `4` otherwise and never changes your checkout |
| `--help` | Show the help text |

### `--dry-run` semantics

`--dry-run` prints each mutating step prefixed with `would run:` and executes
nothing: no packages, no build, no files copied. Read-only probes still run —
`command -v`, `cat /etc/os-release`, `cargo --version`, `fc-list : family` — so
the printed plan matches the machine you are on. Use it to review the exact
`pacman`/`apt-get`/`dnf` command before letting the script touch the system.

With `--version`, `--dry-run` never uses the network: if the tag is missing it
prints `would run: git fetch --tags` and exits with code `4`, because a real run
would first ask for confirmation and then fetch. It also reports when the tag
exists but `HEAD` is somewhere else (with the distance to the tag), and when the
working tree is dirty, without moving anything.

### Installing a specific release (`--version`)

Releases are tagged `vX.Y.Z`. `--version` accepts the tag with or without the
leading `v` (`--version v1.0.0` and `--version 1.0.0` are the same request) and
compares against the canonical `vX.Y.Z` form. Anything else — `--version`
without a value, or a value that is not `X.Y.Z` — is a usage error with code
`2`.

The script **verifies, it does not mutate your checkout**. It never runs
`git checkout`, `git switch` or anything else that moves the working tree,
because moving the tree (and the branch you are on) is the user's decision, not
a side effect of an installer. What it does instead:

1. Confirms it runs inside a git checkout (`git rev-parse --git-dir`); a tarball
   without `.git` is rejected.
2. Checks that the tag exists locally. If it does not, a real run offers
   `git fetch --tags` (that is network, and it is asked for explicitly);
   `--dry-run` only prints it. If the tag still does not exist, the script
   exits with code `4`.
3. Requires `git describe --tags --exact-match HEAD` to report exactly that tag
   (or, equivalently, that the tag points at `HEAD`). This is what guarantees
   that the binary built and installed here is the release you asked for. If it
   does not match, the script exits with code `4`, says how far `HEAD` is from
   the tag ("`HEAD` is 3 commits after `v1.0.0`", "… before …", or "the history
   diverged") and prints the two ways to get to the release: `git checkout
   v1.0.0`, or a temporary worktree built from the tag, which leaves your tree
   untouched.
4. Only then requires a **clean** working tree: no modified tracked files and no
   untracked files either. A commit is not enough on its own — what gets
   compiled has to be exactly what was published, and an uncommitted edit, a
   stray file or a local `.cargo/config.toml` would change the binary without
   changing the tag. This check runs *after* the tag comparison, so a dirty tree
   at another commit is reported as "not at that version", not as "the tag is
   here with changes". If it fails the script exits with code `4` and lists the
   offending paths.
5. Only then continues with the normal flow (checks, build, `--install`).

The intended flow, with the checkout step owned by you:

```sh
cd astraterm
git checkout v1.0.0
./scripts/setup.sh --version v1.0.0 --install
```

Preview it first with `--dry-run --version v1.0.0`, which uses no network.

### Exit codes

| Code | Meaning |
|---|---|
| `0` | Success; nothing left to do. Declining the package installation is not a failure: the step is skipped and the build still runs |
| `2` | Usage error, unknown flag, `--version` without a value, or the script was started as root |
| `3` | Rust toolchain missing, or older than 1.85 |
| `4` | The `--version` tag does not exist, the checkout is not a git checkout, `HEAD` is not at that tag, or the working tree has changes |
| other | The exit status of the command that failed (`cargo`, `pacman`, `apt-get`, `dnf`) |

The script refuses to run as root: compile as your own user, and let it ask
`sudo` for the package step only. It never writes to
`~/.config/astraterm/config.toml` or to the themes directory, never enables
third-party repositories (such as the Nerd Fonts COPR), and never installs
anything from the network apart from distribution packages.

## Requirements

AstraTerm is a native window: it needs a C toolchain and `pkg-config` to build,
Wayland/X11 client libraries, fontconfig and freetype for text, and a working
Vulkan or OpenGL driver for wgpu.

| Capability | Arch (`pacman`) | Debian/Ubuntu (`apt`) | Fedora (`dnf`) |
|---|---|---|---|
| C toolchain + `pkg-config` | `base-devel` | `build-essential pkg-config` | `gcc gcc-c++ make pkgconf-pkg-config` |
| Wayland client | `wayland` | `libwayland-dev` | `wayland-devel` |
| Keyboard handling | `libxkbcommon` | `libxkbcommon-dev` | `libxkbcommon-devel` |
| Fontconfig | `fontconfig` | `libfontconfig1-dev` | `fontconfig-devel` |
| FreeType | `freetype2` | `libfreetype6-dev` | `freetype-devel` |
| X11 (winit backend) | `libx11` | `libx11-dev` | `libX11-devel` |
| OpenGL / Vulkan driver | `mesa` | `mesa-vulkan-drivers libgl1-mesa-dri` | `mesa-vulkan-drivers` |
| Vulkan loader | `vulkan-icd-loader` | `libvulkan1` | `vulkan-loader` |
| Notifications | `libnotify` | `libnotify-bin` | `libnotify` |
| VCS | `git` | `git` | `git` |
| Font | `ttf-jetbrains-mono-nerd` | see [Font](#font) | see [Font](#font) |

On a vendor GPU driver (NVIDIA, AMD proprietary) the distribution's own Vulkan
driver package replaces `mesa`; keep exactly one ICD for the GPU you use. Check
that the ICD is visible before blaming the build:

```sh
vulkaninfo --summary    # or: ls /usr/share/vulkan/icd.d/
```

## Font

The default family is `JetBrainsMono Nerd Font`. If it is missing, AstraTerm
falls back to `JetBrains Mono`, `Symbols Nerd Font`, `Noto Color Emoji`,
`DejaVu Sans Mono` and finally `monospace`, so it always renders — but the
powerline and icon glyphs used by modern prompts will be missing.

- **Arch:** `sudo pacman -S ttf-jetbrains-mono-nerd`
- **Debian/Ubuntu:** `sudo apt install fonts-jetbrains-mono` provides the
  *JetBrains Mono* fallback. For the Nerd Font variant, download it from
  <https://www.nerdfonts.com/font-downloads>, unpack it into
  `~/.local/share/fonts` and run `fc-cache -f`.
- **Fedora:** `sudo dnf install jetbrains-mono-fonts` provides the fallback.
  The Nerd Font variant comes from the `atim/nerd-fonts` COPR; enable it
  yourself if you want it — the setup script does not add repositories.

Any monospaced font works today: set it with `[font] family` in
`~/.config/astraterm/config.toml`, for example

```toml
[font]
family = "JetBrains Mono"
```

## Clipboard and notifications

The clipboard needs nothing at runtime: copy and paste go through the `arboard`
library, which talks to the compositor directly, so `Ctrl+Shift+C`/`Ctrl+Shift+V`
work without a helper program.

Notifications are a runtime feature, not a build dependency:

- `libnotify` (`notify-send`) shows the desktop notification when the
  configuration is saved or reloaded, and it is also the fallback for
  application notifications; the terminal's own `OSC 9`/`OSC 777` notifications
  use it.

## Building without the script

```sh
cargo build --release --locked
./target/release/astraterm
```

`Cargo.lock` is committed, so `--locked` is the reproducible build and is what
the `PKGBUILD` uses too. The release profile uses thin LTO and strips debug
information; expect a first build of several minutes.

## Installing for the current user

```sh
cargo build --release --locked
sh packaging/linux/install.sh
```

That installs the binary to `${XDG_BIN_HOME:-~/.local/bin}/astraterm`, the
desktop entry to
`${XDG_DATA_HOME:-~/.local/share}/applications/astraterm.desktop`, and the icon
in every shipped size to
`${XDG_DATA_HOME:-~/.local/share}/icons/hicolor/<size>/apps/astraterm.png`, then
refreshes the desktop and icon caches. No root, no configuration changes.
Remove it again with `sh packaging/linux/uninstall.sh`.

`./scripts/setup.sh --install` runs exactly this script after the build.

## Arch package

```sh
cd packaging/linux
makepkg -si
```

The `PKGBUILD` installs the binary to `/usr/bin`, the desktop entry, every icon
size and the documented default `/etc/astraterm/config.toml` (a copy template;
the loader only reads the per-user file). It ships `sha256sums=('SKIP')`, so fill
in the real checksums before publishing a package.

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| `error: cargo was not found` / `cargo 1.8x is too old` | Install Rust 1.85+ with rustup (`rustup update stable`), open a new shell, re-run |
| `error: do not run this script as root` | Run it as your user; it calls `sudo` itself for package installation |
| Build fails in `fontconfig-sys` / `freetype-sys` | `pkg-config` or the `-dev` packages are missing |
| Build fails in `wayland-sys` / `xkbcommon` | Install the Wayland and libxkbcommon development packages |
| Window opens but the text is boxes | No usable font: install a Nerd Font or set `[font] family` |
| `failed to create surface` / no adapter | No Vulkan ICD or OpenGL driver for this GPU; install `mesa` (or the vendor driver) and check `vulkaninfo` |
| Clipboard does nothing | The session offers no clipboard: neither the Wayland data-control protocol nor an X11 display is available |
| No save/reload notification | `libnotify` / `notify-send` is missing |
| `error: the tag vX.Y.Z is not in this clone` | The clone predates the tag: `git fetch --tags`, or run without `--version` to build the tree as it is |
| `error: this checkout is not at vX.Y.Z` | `HEAD` is elsewhere: the message says how many commits ahead/behind and prints both fixes — `git checkout vX.Y.Z` yourself, or `git worktree add /tmp/astraterm-vX.Y.Z vX.Y.Z` and build there; the script deliberately moves nothing |
| `error: HEAD is vX.Y.Z, but the working tree has changes` | You are on the right tag but have modified or untracked files; what gets compiled must be exactly what was published. Commit, stash or remove them and re-run |
| `error: … is not a git checkout` | You are in an unpacked tarball, so no tag can be verified; clone the repository |

Diagnostics live behind `--debug` (`RUST_LOG=astraterm=debug`).
