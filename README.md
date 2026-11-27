# AstraTerm

AstraTerm is a native terminal emulator for Linux, written in Rust. It runs a
real PTY, emulates the terminal with a mature VT core, and draws its own window
with wgpu — no Electron, no webview, no browser engine. Tabs and split panes are
implemented by the application itself, so **tmux is not a runtime dependency**.

This is an entirely personal project. I wrote it because I wanted a terminal for
myself — one that behaves the way I work — and it is not a product: no roadmap
obligations, no support promises, no release schedule owed to anyone. It is
shared in case someone else finds it useful, and it stays shaped by what I need
from it.

Visual identity: dark, minimal, productivity-first. The default theme is
`midnight`; a light theme (`dawn`) is available as an option.

**Status:** [docs/STATUS.md](docs/STATUS.md) — the current version, the
crate-by-crate state and the measured numbers.

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

What the **build** needs:

- Rust **1.85+** (the workspace uses edition 2024).
- A C toolchain and `pkg-config`, for crates with C dependencies.
- Development headers for Wayland, `libxkbcommon`, fontconfig, FreeType and X11
  (the winit backend loads `libX11`).

What running the binary needs:

- Linux with Wayland or X11.
- GPU drivers for wgpu (Vulkan or OpenGL), and the Vulkan loader when Vulkan is
  the backend. Mesa covers most systems.
- A font: the default family is `JetBrainsMono Nerd Font`. `scripts/setup.sh`
  checks it with fontconfig, offers the package when it is missing and, if you
  decline, writes the family your machine already has; without it AstraTerm
  falls back to `JetBrains Mono`, `Symbols Nerd Font`, `Noto Color Emoji`,
  `DejaVu Sans Mono` and finally `monospace`.
- Optionally `notify-send` (libnotify) for desktop notifications: the save and
  reload notices, and `OSC 9`/`OSC 777` requests from applications. Without it a
  single warning is logged and the terminal still shows notifications in the
  status bar.

The clipboard needs nothing at runtime. It is linked into the binary through the
`arboard` crate, which reaches the compositor over the Wayland data-control
protocol and falls back to X11, so no `wl-copy`, `wl-paste`, `xclip` or `xsel`
process is spawned and **`wl-clipboard` is no longer a dependency**. The Rust
crates behind this are `arboard` and `png`, plus `proptest` for the tests only.

System packages:

```sh
# Arch Linux
sudo pacman -S --needed base-devel wayland libxkbcommon fontconfig freetype2 mesa \
    vulkan-icd-loader libx11 libnotify git
# plus a Nerd Font, which the bootstrap script offers only when fontconfig cannot
# see one: sudo pacman -S ttf-jetbrains-mono-nerd
# Debian / Ubuntu
sudo apt install build-essential pkg-config git libwayland-dev libxkbcommon-dev \
    libfontconfig1-dev libfreetype6-dev mesa-vulkan-drivers libgl1-mesa-dri libvulkan1 \
    libx11-dev libnotify-bin
```

The bootstrap script installs the same set for the detected distribution, asks
fontconfig for the default family instead of assuming it, and only then offers
the font package — which is what keeps distributions that ship the family
through another package (Omarchy's `ttf-jetbrains-mono-nerd-basic` provides it
and conflicts with it) out of a `pacman` conflict. Declining that offer makes the
script detect a family the machine already has and create
`~/.config/astraterm/config.toml` with it. Nothing is downloaded from the
network apart from the `rustup` installer, which is fetched only when you accept
the toolchain offer that appears when `cargo` is missing, and the binary is built
from the tree:

```sh
./scripts/setup.sh               # asks before installing packages, builds, installs
./scripts/setup.sh --no-install  # … builds only, leaving target/release/astraterm
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

The first run writes a documented default `config.toml`, then opens a shell in
`[terminal] working_directory` — `$HOME` by default — and draws its window.

The command line takes `--working-directory <DIR>`, `--profile <NAME>`,
`--command <ARGV>…` (an argument vector, never a shell string), `--title <TEXT>`,
`--config <PATH>`, `--new-window`, `--debug` and `--version`.

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
- a broken edit keeps the last-known-good configuration active.

Sections: `[window]`, `[terminal]`, `[font]`, `[appearance]`, `[workspace]`,
`[performance]`, `[notifications]`, `[keys]` and `[[profiles]]`. Every key, its
type, default, accepted values, whether it applies live or needs a new session,
and the keys that are accepted but not applied are documented in
[docs/CONFIGURATION.md](docs/CONFIGURATION.md), which is the full reference.

The keys a reader most often wants first:

- `[terminal] bell` — what a program's bell does: `"visual"` (the default: a
  status message and a border flash), `"none"`, or `"audible"`, which rings the
  speaker and falls back to the visual answer when no sound backend exists.
- `[terminal] copy_on_select_target` — whether the selection gesture writes
  `"clipboard"` (the default) or `"primary"`. The explicit copy always writes the
  regular clipboard.
- `[terminal] word_delimiters` — the characters that end a word for double-click
  selection; whitespace and control characters always end one.
- `[font] integer_cell_width` — snaps the cell advance to a whole pixel, which
  narrows the dark seams inside a gradient drawn with block glyphs, at the cost of
  a few columns.
- `[font] hinting` — off by default so thin ink keeps its weight and `░▒▓` keep
  the font's dither instead of turning into a hatch.
- `[font] solid_shades` — paints `░▒▓` as one flat blend per cell (25/50/75 %)
  instead of drawing the font's dither, the way foot does.
- `[performance] max_fps_when_idle` — upper bound on idle redraws per second
  (cursor blink, status-bar readings); `0` means no cap, and a redraw caused by
  output, input or a resize is never delayed by it.

The typography keys decide how text looks, so their units matter: `[font] size`
is in **logical pixels**, not points.

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
| Previous prompt / next prompt | `Ctrl+Shift+Up` / `Ctrl+Shift+Down` |
| Open settings | `Ctrl+,` |
| Reload configuration | `Ctrl+Shift+,` |
| Toggle status bar | `Ctrl+Shift+S` |
| Toggle floating terminal | `Ctrl+Shift+Grave` |
| New window | `Ctrl+Shift+N` |
| Send next key literally | `Ctrl+Shift+Enter` |
| Quit | `Ctrl+Shift+Q` |

The mouse is part of the same model. Double click selects the word under the
pointer (`[terminal] word_delimiters`), triple click selects the line, `Alt`+drag
selects a rectangle whose copy keeps its padding, and a middle click pastes the
primary selection. `Ctrl`+click opens a link only when
`[terminal] open_urls_on_click` is enabled (it is by default): the destination is
the cell's `OSC 8` hyperlink when it carries one, otherwise the visible text under
the pointer. Previous and next prompt jumps work where the shell emits `OSC 133`
marks.

Chord syntax in `[keys]`: lower-case tokens joined by `+`, for example
`ctrl+shift+t`, `alt+left`, `ctrl+pageup`, `ctrl+comma`, `ctrl+minus`,
`shift+pageup`. Named keys include `escape`, `enter`, `tab`, `backspace`,
`delete`, `insert`, `home`, `end`, `pageup`, `pagedown`, `left`, `right`, `up`,
`down`, `f1`–`f24`, plus `space`, `comma`, `period`, `minus`, `plus`, `equal`,
`slash`, `backslash`, `semicolon`, `quote`, `bracketleft`, `bracketright`,
`backtick` (or `grave`). Alternate chords are separated by `|`
(`increase-font-size = "ctrl+=|ctrl+plus"`); an empty value or `none` unbinds an
action.

## Packaging

Files in this repository:

| Path | Purpose |
|---|---|
| `packaging/desktop/astraterm.desktop` | freedesktop desktop entry (`Exec=astraterm %U`, `Icon=astraterm`, `StartupWMClass=astraterm`) |
| `packaging/linux/PKGBUILD` | Arch Linux package: builds with `cargo build --release --locked`, installs the binary, desktop entry, icon and `/etc/astraterm/config.toml` |
| `packaging/linux/install.sh` | POSIX `sh` installer for a portable build into the user's XDG directories (no root) |
| `scripts/setup.sh` | idempotent bootstrap: system dependencies, toolchain, an interactive font step, release build, optional install — see [docs/INSTALL.md](docs/INSTALL.md) |
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

To build a tagged release instead of the working tree, `scripts/setup.sh`
verifies the tag and leaves your checkout alone:

```sh
./scripts/setup.sh --version v1.3.0
```

It refuses unless `HEAD` is exactly that tag and the working tree is clean, never
runs `git checkout`/`git switch`, and prints the two remedies when the checkout is
elsewhere (see [docs/INSTALL.md](docs/INSTALL.md#installing-a-specific-release---version)).

Building an Arch package from a release tarball:

```sh
cd packaging/linux
makepkg -si
```

The `pkgver` in the `PKGBUILD` has to be bumped with each release; it still reads
`1.1.5`, so bump it before packaging 1.3.0. The file ships
`sha256sums=('SKIP')`; **fill in the real release tarball checksums before
publishing** (see the comment at the top of the file).

## Security posture

- **The clipboard is linked in, not spawned.** Copy and paste go through the
  `arboard` library over the Wayland data-control protocol, with an X11 fallback;
  no helper process is created, so nothing can block the UI thread waiting on a
  child and there is no `TMPDIR` for a helper to inherit.
- **A pasted image becomes a file path.** The clipboard image is validated
  against a 16 MiB and 16-megapixel limit, written exclusively into a temporary
  directory with a name built only from numbers, and its **path** is inserted into
  the session — never graphics-protocol bytes.
- **OSC 52 is off, and there is no way to turn it on.** Applications cannot read
  the clipboard and their writes never arrive: the engine is created with
  `Osc52::Disabled` and no configuration key changes that. This is a deliberate
  gap rather than a finished design — the application side would write such a
  request straight to the clipboard without asking, so the protection today is
  the disabled engine, not a confirmation prompt.
- **Links open only on an explicit `Ctrl`+click**, and only when
  `[terminal] open_urls_on_click` is on. The destination is the cell's `OSC 8`
  hyperlink when it carries one, screened by the platform allow-list, which
  accepts absolute `http://`, `https://`, `mailto:` and `ftp://` URLs; the visible
  text counts only when it starts with `http://` or `https://`. **A refused scheme
  opens nothing** — there is no fallback to the text under the pointer — and the
  opener runs `xdg-open` with one argument and no shell.
- **Images are opt-in and bounded.** `terminal.images` is off by default; with it
  off, captured sequences are dropped without decoding a byte. With it on, a
  payload is capped at 8 MiB, a decoded image at 4096 pixels per axis and
  2048×2048 pixels (16 MiB of RGBA), the declared size is checked before the
  pixel buffer is sized, the per-session store holds at most 64 MiB, and nothing
  is ever fetched from the network or written to a path a payload names.
- **SSH goes through the system OpenSSH client.** AstraTerm starts `ssh` with an
  argument vector, never a shell string, never disables host-key checking and
  **never stores SSH passwords or private keys** in its configuration. SSH
  profiles reference host aliases or explicit command vectors.
- **No shell string execution.** Commands are always launched with an argument
  vector; `sh -c` is never used to interpret a command.
- **No secret logging.** Terminal output, clipboard contents and configuration
  values are not written to logs.
- **Bounded resources.** PTY output queues, input queues, scrollback, image
  payloads and the session count are bounded, so a noisy process cannot exhaust
  memory or starve other panes.
- **Minimal `unsafe`.** Two places in the shipping code: the `SIGKILL` escalation
  that stops a child which ignores the polite close (`crates/pty/src/lib.rs`) and
  the `localtime_r` + `strftime` call that formats the status-bar clock in the
  user's timezone (`crates/platform/src/system.rs`). A counting global allocator
  in `crates/renderer/src/test_alloc.rs` exists only for the tests. See
  [docs/SECURITY.md](docs/SECURITY.md).

## Known limitations

- Copy and search text use the cell's base character only: a copied emoji keeps
  its glyph but loses its variation selectors and joiners, which the renderer does
  keep.
- `OSC 133` marks are cleared when the grid is resized, are not recorded while the
  alternate screen is up, and are dropped when one parser piece moves the buffer
  more than 64 lines, because the movement can no longer be measured.
- Ligatures are opt-in (`font.ligatures`, off by default) and images are opt-in
  (`terminal.images`, off by default because the payload is untrusted input).
- Image protocols cover kitty graphics (single chunk and chunked, PNG or raw
  RGB/RGBA), iTerm2 `OSC 1337;File=` and sixel, including HLS palettes. Kitty
  formats other than PNG and raw frames, kitty control messages other than
  `a=T`/`a=t`, and iTerm2 `inline=0` (save the file instead of showing it) are
  refused rather than approximated.
- Desktop notifications use `notify-send`; without libnotify the terminal still shows
  them in the status bar.
- The audible bell (`[terminal] bell = "audible"`) degrades to the visual answer
  when no sound backend exists, so on such a system the setting changes nothing
  audible.
- Search is plain substring matching (no regular expressions) and searches the
  buffer, not just the screen.
- Blur is compositor-specific: transparency works, blur needs a compositor rule
  (for example a Hyprland `blur` layer rule). The application never claims
  universal blur support.
- `/etc/astraterm/config.toml` is installed as documentation and a copy
  template; the loader reads only the per-user file.
- The icon set is generated from `assets/icons/logo.png`; regenerate it with
  `sh assets/icons/generate-icons.sh` after changing the artwork and commit the
  resulting PNGs.
- Settings that are declared but not yet consumed: `remember_geometry`,
  `appearance.style`, `appearance.show_window_controls`, `background_blur` (a
  compositor rule), `appearance.accent_color`, `font.fallbacks`,
  `gpu_acceleration` and per-profile `theme`/`font_size`. See
  [docs/CONFIGURATION.md](docs/CONFIGURATION.md).

## License

MIT (see the `license` field in `Cargo.toml`).

Copyright © Alexis Buelvas, author and copyright holder.

### Third-party components

AstraTerm is built on other people's work. Grouped by what they do, with the
licence each crate declares in its own `Cargo.toml` (versions as resolved by
`cargo metadata` in this tree); the full text of every licence ships with the
crate that declares it:

- **Rendering and window** — `wgpu` 30.0.2 (MIT OR Apache-2.0), `glyphon` 0.12.0
  (MIT OR Apache-2.0 OR Zlib), `cosmic-text` 0.19.0 (MIT OR Apache-2.0),
  `winit` 0.30.13 (Apache-2.0), `pollster` 0.4.0 (Apache-2.0/MIT), `bytemuck`
  1.25.2 (Zlib OR Apache-2.0 OR MIT).
- **Terminal core** — `alacritty_terminal` 0.26.0 (Apache-2.0),
  `portable-pty` 0.9.0 (MIT), `png` 0.18.1 (MIT OR Apache-2.0).
- **Configuration and plumbing** — `serde` 1.0.229 (MIT OR Apache-2.0),
  `serde_ignored` 0.1.14 (MIT OR Apache-2.0), `toml` 1.2.2+spec-1.1.0
  (MIT OR Apache-2.0), `notify` 9.0.0 (CC0-1.0), `thiserror` 2.0.21
  (MIT OR Apache-2.0), `anyhow` 1.0.105 (MIT OR Apache-2.0),
  `crossbeam-channel` 0.5.17 (MIT OR Apache-2.0), `tracing` 0.1.44 (MIT),
  `tracing-subscriber` 0.3.23 (MIT), `clap` 4.6.9 (MIT OR Apache-2.0).
- **Platform and system** — `arboard` 3.7.0 (MIT OR Apache-2.0), `libc` 0.2.191
  (MIT OR Apache-2.0).
- **Tooling and tests only** — `proptest` 1.12.0 (MIT OR Apache-2.0),
  `tempfile` 3.27.0 (MIT OR Apache-2.0).
