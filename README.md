# AstraTerm

AstraTerm is a native terminal emulator for Linux, written in Rust. It runs a
real PTY, emulates the terminal with a mature VT core, and draws its own window
with wgpu — no Electron, no webview, no browser engine. Tabs and split panes are
implemented by the application itself, so **tmux is not a runtime dependency**.

Visual identity: dark, minimal, productivity-first. The default theme is
`midnight`; a light theme (`dawn`) is available as an option.

## Status

**Usable MVP: a real terminal with native tabs and splits.** The whole stack is
implemented and covered by 489 automated tests plus manual verification on
Wayland/Hyprland with Neovim, zsh and the Omarchy prompt:

| Crate | Contents | State |
|---|---|---|
| `crates/core` | shared ids, cell metrics/geometry, application events | implemented |
| `crates/config` | one typed config model, TOML loader/writer, validation, atomic + watched config service, themes, keymap | implemented |
| `crates/workspace` | recursive split tree, tabs, focus traversal, layout math, window/tab metadata | implemented |
| `crates/pty` | `portable-pty` adapter with bounded queues and reader/writer/waiter threads | implemented |
| `crates/terminal` | `alacritty_terminal` adapter: snapshots, damage, selection, search, key/mouse/paste encoding | implemented |
| `crates/session` | session lifecycle, output coalescing with per-session budgets, cwd tracking | implemented |
| `crates/renderer` | wgpu quad pipeline + glyphon text, damaged-row shaping cache | implemented |
| `crates/settings-ui` | native, keyboard-driven settings panel model and text projection | implemented |
| `crates/platform` | clipboard, URL/XDG path and font discovery without `sh -c` | implemented |
| `crates/app` | winit window and event loop, tabs/splits, keybinding dispatch, mouse selection, search, settings overlay, workspace restore, CLI | implemented |

Build it with `cargo build --release` and run `./target/release/astraterm`.
Measured on the development machine (Intel UHD 630, Wayland, 2494×1396 window):
about 1 % idle CPU with one pane and roughly 129 MB RSS in a debug build. See
[docs/PERFORMANCE.md](docs/PERFORMANCE.md) for the measurement method and
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for how the pieces fit together.
Changes are recorded in [CHANGELOG.md](CHANGELOG.md).

## What AstraTerm is (and is not)

- A standalone graphical terminal emulator with its own window.
- A native multiplexer: tabs, splits, focus and pane lifecycle live in Rust.
- Configurable through TOML and a native settings panel that share one typed
  model.

It is **not** a terminal emulator built on Electron or a webview, and it does
**not** shell out to tmux for tabs or splits.

Rendering and desktop niceties that ship today:

- **Translucent background**: `[window] opacity` (0.1–1.0) applies live; only the
  background fades, text stays crisp. Blur is a compositor rule — see
  [docs/CONFIGURATION.md](docs/CONFIGURATION.md#blur-behind-the-window) for the
  Hyprland/Omarchy, KDE and GNOME steps.
- **Status bar** with session information on the left and **CPU, RAM and the clock**
  on the right, sampled from `/proc` once per second and only while it is visible.
- **Tabs on top, bottom, hidden, or on the right** as a Warp-style vertical strip
  with a configurable width.
- **Theme editor** in the settings panel: adjust the palette with live preview, name
  the result and save it to `~/.config/astraterm/themes/`.
- **Floating terminal**: `ctrl+shift+grave` opens one that floats over the layout with
  its own shell; hiding it keeps the session alive and shows a `float hidden` marker in
  the status bar.
- **Kitty keyboard protocol** (on by default), opt-in ligatures (`font.ligatures`),
  opt-in images (kitty graphics, iTerm2 `OSC 1337`, sixel) and **desktop notifications**
  from `OSC 9`/`OSC 777` with a window attention hint.

Every option is documented with what it does and when to change it in
[docs/CONFIGURATION.md](docs/CONFIGURATION.md#option-reference).

## Requirements

- Rust **1.85+** (the workspace uses edition 2024).
- Linux with Wayland or X11.
- GPU drivers for wgpu (Vulkan or OpenGL). Mesa is enough on most systems.
- A font: the default family is `JetBrainsMono Nerd Font`, falling back to
  `JetBrains Mono`, `Symbols Nerd Font`, `Noto Color Emoji`, `DejaVu Sans Mono`
  and finally `monospace`.

System packages:

```sh
# Arch Linux
sudo pacman -S --needed base-devel wayland libxkbcommon fontconfig freetype2 mesa \
    vulkan-icd-loader libx11 wl-clipboard libnotify git ttf-jetbrains-mono-nerd
# Debian / Ubuntu
sudo apt install build-essential pkg-config libwayland-dev libxkbcommon-dev \
    libfontconfig1-dev libfreetype6-dev mesa-vulkan-drivers libgl1-mesa-dri libvulkan1 \
    libx11-dev wl-clipboard libnotify-bin
```

The bootstrap script installs the same set for the detected distribution, checks
the toolchain and the font without downloading anything, and builds the binary:

```sh
./scripts/setup.sh            # asks before installing packages, then builds
./scripts/setup.sh --install  # … and installs it for the current user
```

[docs/INSTALL.md](docs/INSTALL.md) documents the requirements per distribution,
every flag of the script, and what it deliberately does not do.

## Build

```sh
git clone <repository> astraterm
cd astraterm
cargo build --release --locked
```

The binary lands in `target/release/astraterm`.

Quality gates used by the project:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build --release
```

## Run

```sh
./target/release/astraterm
```

> **Note:** until the `crates/app` shell is implemented the binary exits
> immediately. Everything below describes the intended behaviour of the shipped
> configuration and packaging assets, which are complete.

Installing it on `PATH` for the current user is covered by
[packaging](#packaging).

## Configuration

The configuration file is TOML, following the XDG Base Directory specification:

```text
${XDG_CONFIG_HOME:-~/.config}/astraterm/config.toml
```

A complete default file ships with the distribution packages as
`/etc/astraterm/config.toml`; copy it to your own path to customise. The
defaults are also compiled into the binary, so a missing or partial file is
always valid: missing keys keep their default value, and unknown keys are
reported as warnings instead of failing the load.

Guarantees of the configuration service (`crates/config/src/service.rs`):

- one typed model behind both the TOML interface and the native settings panel;
- values are validated before they are applied or persisted;
- saves are atomic (temporary file + rename), so a crash cannot truncate the file;
- an external edit is detected and reloaded; a save never silently overwrites
  newer changes made by another program;
- a broken edit keeps the last known-good configuration active.

Sections: `[window]`, `[terminal]`, `[font]`, `[appearance]`, `[workspace]`,
`[performance]`, `[keys]` and `[[profiles]]`.

Themes live under `${XDG_CONFIG_HOME:-~/.config}/astraterm/themes/*.toml`.
Exports of the two built-in themes are in [`assets/themes`](assets/themes)
(`midnight.toml`, `dawn.toml`); copy one there to use it as a starting point.
Select a theme with `[appearance] theme = "midnight"`.

## Keyboard shortcuts

These are the defaults from `default_keybindings()` in
`crates/config/src/keys.rs`; every one of them can be rebound in the `[keys]`
table of `config.toml`.

| Action | Shortcut |
|---|---|
| New tab | `Ctrl+Shift+T` |
| Close tab | `Ctrl+Shift+Alt+W` |
| Rename tab | `Ctrl+Shift+R` |
| Move tab left / right | `Ctrl+Shift+Alt+Left` / `Ctrl+Shift+Alt+Right` |
| Next tab | `Ctrl+PageDown` |
| Previous tab | `Ctrl+PageUp` |
| Go to tab 1–9 | `Alt+1` … `Alt+9` |
| Split horizontally | `Ctrl+Shift+D` |
| Split vertically | `Ctrl+Shift+E` |
| Close pane | `Ctrl+Shift+W` |
| Focus pane left/right/up/down | `Alt+Left` / `Alt+Right` / `Alt+Up` / `Alt+Down` |
| Resize pane left/right/up/down | `Ctrl+Alt+Left` / `Ctrl+Alt+Right` / `Ctrl+Alt+Up` / `Ctrl+Alt+Down` |
| Equalize splits | `Ctrl+Shift+0` |
| Zoom pane | `Ctrl+Shift+Z` |
| Increase font size | `Ctrl+=` or `Ctrl++` |
| Decrease font size | `Ctrl+-` |
| Reset font size | `Ctrl+0` |
| Search scrollback | `Ctrl+Shift+F` |
| Next match | `Ctrl+G` |
| Previous match | `Ctrl+Shift+G` |
| Copy selection | `Ctrl+Shift+C` |
| Paste | `Ctrl+Shift+V` |
| Scroll page up / down | `Shift+PageUp` / `Shift+PageDown` |
| Scroll to top / bottom | `Ctrl+Shift+Home` / `Ctrl+Shift+End` |
| Open settings | `Ctrl+,` |
| Reload configuration | `Ctrl+Shift+,` |
| Toggle status bar | `Ctrl+Shift+S` |
| New window | `Ctrl+Shift+N` |
| Send next key literally | `Ctrl+Shift+Enter` |
| Quit | `Ctrl+Shift+Q` |

Chord syntax in `[keys]`: lower-case tokens joined by `+`, for example
`ctrl+shift+t`, `alt+left`, `ctrl+pageup`, `ctrl+comma`, `ctrl+minus`,
`shift+pageup`. Named keys include `escape`, `enter`, `tab`, `backspace`,
`delete`, `insert`, `home`, `end`, `pageup`, `pagedown`, `left`, `right`, `up`,
`down`, `f1`–`f24`, plus `space`, `comma`, `period`, `minus`, `plus`, `equal`,
`slash`, `backslash`, `semicolon`, `quote`, `bracketleft`, `bracketright`,
`backtick`. Alternate chords are separated by `|`
(`increase-font-size = "ctrl+=|ctrl+plus"`); an empty value or `none` unbinds an
action.

## Packaging

Files in this repository:

| Path | Purpose |
|---|---|
| `packaging/desktop/astraterm.desktop` | freedesktop desktop entry (`Exec=astraterm %U`, `Icon=astraterm`, `StartupWMClass=astraterm`) |
| `packaging/linux/PKGBUILD` | Arch Linux package: builds with `cargo build --release --locked`, installs the binary, desktop entry, icon and `/etc/astraterm/config.toml` |
| `packaging/linux/install.sh` | POSIX `sh` installer for a portable build into the user's XDG directories (no root) |
| `scripts/setup.sh` | idempotent bootstrap: system dependencies, toolchain and font checks, release build, optional install — see [docs/INSTALL.md](docs/INSTALL.md) |
| `packaging/linux/uninstall.sh` | removes exactly the files `install.sh` created |
| `packaging/linux/config.toml` | the system-wide default configuration installed to `/etc/astraterm/config.toml` |
| `assets/icons/logo.png` | the AstraTerm logo; every icon size is generated from it |
| `assets/icons/astraterm.png`, `assets/icons/hicolor/*` | committed icon set (32–512 px, transparent) used by the installer, the package and the window icon |
| `assets/themes/midnight.toml`, `assets/themes/dawn.toml` | exports of the built-in themes |

Portable install for the current user, after `cargo build --release --locked`:

```sh
sh packaging/linux/install.sh
```

It installs:

- the binary to `${XDG_BIN_HOME:-~/.local/bin}/astraterm`,
- the desktop entry to `${XDG_DATA_HOME:-~/.local/share}/applications/astraterm.desktop`,
- the icon in every size it ships (32, 48, 64, 128, 256, 512) to
  `${XDG_DATA_HOME:-~/.local/share}/icons/hicolor/<size>/apps/astraterm.png`,
  and refreshes the desktop and icon caches.

After that, **AstraTerm appears in the Omarchy application menu (and every
freedesktop launcher) as “AstraTerm”, with its logo as the icon**. Launching from
the menu and launching the binary behave the same; the window sets the matching
`app_id`/`WM_CLASS` so the compositor groups it with the entry.

It refuses to run when the release binary is missing, never needs root, and
never touches your configuration. Remove it again with:

```sh
sh packaging/linux/uninstall.sh
```

Building an Arch package from a release tarball:

```sh
cd packaging/linux
makepkg -si
```

The `PKGBUILD` ships `sha256sums=('SKIP')`; **fill in the real release tarball
checksums before publishing** (see the comment at the top of the file).

## Security posture

- **OSC 52 is disabled by default.** Applications cannot read or write the system
  clipboard through escape sequences unless that is enabled explicitly; terminal
  escape sequences are treated as untrusted input.
- **SSH goes through the system OpenSSH client.** AstraTerm starts `ssh` with an
  argument vector, never a shell string, never disables host-key checking and
  **never stores SSH passwords or private keys** in its configuration. SSH
  profiles reference host aliases or explicit command vectors.
- **No shell string execution.** Commands are always launched with an argument
  vector; `sh -c` is never used to interpret a command.
- **No secret logging.** Terminal output, clipboard contents and configuration
  values are not written to logs.
- **Bounded resources.** PTY output queues, input queues and scrollback are
  bounded, so a noisy process cannot exhaust memory or starve other panes.
- **No automatic link opening.** URL detection only acts on an explicit user
  action.
- **Minimal `unsafe`.** Two places: the `SIGKILL` escalation that stops a child
  which ignores the polite close (`crates/pty/src/lib.rs`) and the `localtime_r` +
  `strftime` call that formats the status-bar clock in the user's timezone
  (`crates/platform/src/system.rs`). See [docs/SECURITY.md](docs/SECURITY.md).

## Known limitations

- IME preedit is not drawn: composed text arrives through the input method and is
  written to the pane, but the composition in progress is only traced, and the
  candidate window is not positioned at the cursor.
- Ligatures are opt-in (`font.ligatures`, off by default) and images are opt-in
  (`terminal.images`, off by default because the payload is untrusted input).
- Image protocols cover kitty single-chunk PNG and raw frames, iTerm2
  `OSC 1337;File=` and sixel. Kitty chunked transfers (`m=1`), HLS sixel palettes and
  iTerm2 `inline=0` are refused rather than approximated.
- Desktop notifications use `notify-send`; without libnotify the terminal still shows
  them in the status bar.
- Search is plain substring matching (no regular expressions) and searches the
  buffer, not just the screen.
- Wide glyphs are rendered followed by one filler space so later columns stay
  aligned; the glyph itself may slightly overflow its cell.
- OSC 52 (clipboard from the application) is disabled by default because escape
  sequences are untrusted input; enable it only if you need remote copy.
- Blur is compositor-specific: transparency works, blur needs a compositor rule
  (for example a Hyprland `blur` layer rule). The application never claims
  universal blur support.
- `/etc/astraterm/config.toml` is installed as documentation and a copy
  template; the loader reads only the per-user file.
- The icon set is generated from `assets/icons/logo.png`; regenerate it with
  `sh assets/icons/generate-icons.sh` after changing the artwork and commit the
  resulting PNGs.
- Settings that are declared but not yet consumed: `word_delimiters`,
  `remember_geometry`, `appearance.style`, `appearance.show_window_controls`,
  `background_blur` (a compositor rule), `font.fallbacks`, `gpu_acceleration`,
  `max_fps_when_idle` and per-profile `theme`/`font_size`. See
  [docs/CONFIGURATION.md](docs/CONFIGURATION.md).

## License

MIT (see the `license` field in `Cargo.toml`).
