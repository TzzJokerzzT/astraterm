# Changelog

All notable changes to AstraTerm are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.2.3] - 2026-11-26

### Added

- `[terminal] copy_on_select_target` chooses where the selection gesture writes:
  `"clipboard"` (the default) or `"primary"`. The explicit copy always writes the
  regular clipboard.
- Middle-click pastes the primary selection, through the same threaded read as the
  normal paste, so it never blocks the window; an empty selection shows the usual
  message.

### Changed

- The cursor shape belongs to the program. A `DECSCUSR` request from an editor is
  now painted as asked, and `[appearance] cursor` is only the default for programs
  that never ask. Before this the terminal's setting hid the editor's cursor
  entirely, so a beam in insert mode and a block in normal mode never appeared. The
  override path was removed rather than disabled, and the change reaches panes
  created after it.

### Performance

- The PTY reader no longer copies every read: the buffer is handed over and
  recycled, so a steady session copies nothing and allocates almost never (1.00 ->
  0.0004 allocations per chunk, measured over 190 MB). Bytes, order, chunk
  boundaries, backpressure and the drop accounting are unchanged.

### Fixed

- Turning `copy_on_select` off releases the remembered selection instead of keeping
  it for the rest of the session.

## [1.2.2] - 2026-11-26

### Added

- Ctrl+click opens an OSC 8 hyperlink: the destination travels in `CellSnapshot`,
  so a click opens what the program linked rather than the text under the cursor.
  A refused scheme opens nothing — there is no fallback to the visible text — and a
  cell without a hyperlink keeps the previous behaviour of opening a detected URL.

### Changed

- The clipboard backend is `arboard` instead of a spawned helper: no `wl-copy`,
  `wl-paste`, `xclip` or `xsel` process is created, so copy and paste can no longer
  block the UI thread waiting on a child. `wl-clipboard` is no longer a runtime
  dependency.
- Pasting when the clipboard holds an image writes the PNG to a temporary file and
  inserts its path, so any program can open it. The payload is validated against a
  16 MiB and 16 megapixel limit, and the file is created exclusively with a name
  built only from numbers, so a shared temp directory is safe.

### Performance

- The row walk no longer allocates per cell or looks up an advance for a plain
  one-column ASCII cell, and the cluster cache evicts its oldest entry instead of
  wiping itself at the bound, which now covers a 4K screen. Measured per frame on a
  200x50 pane: dense CJK 55.6 ms -> 0.87 ms, plain ASCII 0.95 ms -> 0.28 ms, emoji
  0.60 ms -> 0.25 ms. Scene digests over five content scenarios are identical
  before and after, so anchoring and glyph placement did not change.

## [1.2.1] - 2026-11-26

### Added

- The input method's candidate window is positioned at the terminal cursor: the
  painted cursor cell, in physical pixels, recomputed from the events that can
  move it — output, keys, tab or pane switches, resize, scale and font changes,
  scrolling, clicks, showing or hiding the floating terminal, and the window
  regaining focus — and sent only when it changes. A hidden cursor sends a
  zero-size area at the pane's inner corner, which is how an input method is told
  there is no caret.
- The image and notification scanner is now property-tested: byte-exact
  passthrough with arbitrary chunk boundaries, and no panic or unbounded growth on
  arbitrary, truncated and interleaved input.

### Fixed

- An unterminated sequence that hit the pending-byte bound swallowed the rest of
  the current read instead of resyncing at the bound, so whether the text after an
  abandoned sequence reached the parser depended on where the read was cut. The
  bytes past the bound are text again, which is what already happened for every
  later chunk. Found by the new property tests.
- `astraterm --version` no longer has a test that hard-codes the version: it now
  compares the binary against the workspace version, so a bump cannot leave the
  suite red.

## [1.2.0] - 2026-11-26

### Added

- The floating terminal inherits the directory of the pane it is opened from, and a
  new tab inherits the active tab's live directory: both follow the pane's real
  directory, refreshed from `/proc`, so a `cd` in the parent is inherited. An
  explicit `working_directory` path still wins, and the policy applies after the
  parent's directory.
- Splits created while the floating terminal has the keyboard land inside the
  floating terminal, which now holds its own layout with per-pane scene ids, sizes,
  hit testing, dividers and focus. Clicking a floating pane focuses it, the focus
  keys traverse the float and stop at its edges, and hiding then showing it restores
  the pane that was active. The mosaic, its reflow and its persistence never see it.
- `[font] integer_cell_width` stretches the font so the cell advance is a whole
  pixel, which shrinks the dark seam between the steps of a gradient drawn with
  block glyphs from 12 units to 3-4. Costs a few columns.
- `[font] hinting`, off by default, rasterises glyphs unhinted. Hinted one-pixel
  dither landed on uneven boundaries and hinted thin ink lost covered pixels, which
  is what made `▓▒░` look like a coarse hatch and the `·` filler look sparse.
- `[font] solid_shades`, off by default, paints `░▒▓` as one flat blend of the
  foreground over the background at 25/50/75 %, emulating foot's
  `box-drawing-solid-shades` and suppressing the font's own dither.

### Fixed

- Glyph colours are no longer converted to linear light on a plain UNORM surface,
  which had been drawing every glyph darker than the theme asked: a 256-colour 238
  filler dot measured 22 instead of 68, and block-glyph gradients collapsed from
  118 to 46. Text and backgrounds now share one colour space and match foot.
- Wide and fallback glyphs are anchored to the grid, so a prompt or a status line
  drawn with powerline and nerd glyphs no longer drifts after an emoji or a CJK
  glyph. Anchoring is decided by the measured advance, not by a list of codepoints,
  so a fallback script such as devanagari is covered too.
- `CellSnapshot` carries the zero-width continuations of a cell, so `❤️`, `1️⃣` and
  `👨‍👩‍👧` reach the shaper whole instead of losing their selectors and joiners, and
  clusters the snapshot preserves are no longer split between items.
- A session starts with the winsize derived from the real window rectangle and a
  refused winsize is retried, instead of beginning at a hardcoded 80x24.
- A wide cell with no following spacer no longer swallows the cell after it, and a
  hidden region indicator no longer absorbs the visible one or lends it its style.
- `docs/CONFIGURATION.md` documents every key with its type, default, accepted
  values and effect, and four schema comments that contradicted the implementation
  were corrected.

## [1.1.5] - 2026-11-26

### Fixed

- A kitty image sent as a single chunk that spells out `m=0` is decoded again.
  It was refused as a malformed final chunk, which the previous decoder accepted.
- A chunk that continues a transfer no longer evicts a sibling. With four
  transfers in flight, a valid continuation used to drop another image.
- The compatibility table names the tests that exist, and the feature document no
  longer lists the resize work as pending.

## [1.1.4] - 2026-11-26

### Fixed

- Images no longer disappear when the window is resized. Only anchors whose line
  has left the buffer are dropped, and those are counted; an image that merely
  scrolled out of view survives and comes back. The anchor can still end up a line
  or more off after a reflow, because `alacritty_terminal` renumbers lines and
  exposes no old-to-new map: the guarantee is that the image does not vanish.

## [1.1.3] - 2026-11-26

### Fixed

- Copying no longer asks to install `wl-clipboard` when it is installed. The
  helper inherited a `TMPDIR` pointing at a directory that had gone away, so
  `wl-copy` died with `mkdtemp: No such file or directory` and the terminal
  blamed the installation. A `TMPDIR` that is not a directory is now dropped for
  the helper, which falls back to `/tmp`, and a failure quotes the helper's own
  message instead of a generic one. Paste reports the same distinction.

## [1.1.2] - 2026-11-26

### Changed

- `setup.sh --version` now explains instead of only refusing. When the checkout
  is not at the requested tag the error names the distance (`4 commits after
  v1.0.0`, `2 before`, or diverged) and prints both remedies: check out the tag,
  or build it in a temporary worktree. The script still never moves your tree.
- The clean-tree requirement is documented, and it is now checked **after**
  confirming the tag, so a modified file at another commit is no longer reported
  as if the tag were checked out. A declined `git fetch --tags` says so instead
  of claiming the fetch failed.

### Added

- `scripts/tests/version-check.sh`: eleven repeatable checks over throwaway git
  repositories covering every `--version` outcome, including the skip when the
  flag is absent.

## [1.1.1] - 2026-11-26

### Fixed

- A malformed OSC (for example `ESC ] 9 BEL`, a notification introducer without
  its `;`) no longer stops the scanner from recognising images and
  notifications. The terminator was being swallowed along with the rejected
  prefix, so the scanner waited for another one and every sequence in between
  was missed. Applies to both BEL and ST terminated sequences.

## [1.1.0] - 2026-11-26

### Added

- Kitty graphics sent in chunks (`m=1`) are reassembled and drawn. Clients that
  split an image across several escape sequences (`kitten icat`, `yazi`, `chafa`
  in kitty mode) now work instead of being refused.
- In-flight transfers are bounded: a per-transfer and aggregate byte budget, at
  most four live transfers, and the oldest one dropped when the budget runs out.
  A final chunk without a transfer, or a chunk without an image id, is an error
  rather than a silently truncated image.

## [1.0.0] - 2026-11-23

First public version: a native terminal emulator for Linux whose window, tabs
and splits are implemented in Rust on top of wgpu, with no Electron, webview or
tmux in the stack.

### Added

- Native terminal with real PTY sessions, scrollback and search.
- Tabs and recursive split panes managed by the application, plus a floating
  terminal that can be hidden and brought back.
- Themes, including the built-in `midnight` and `dawn` palettes and a
  keyboard-driven theme editor with live preview.
- Native settings panel that edits the same typed configuration model as the
  TOML file, with atomic saves and live reload of external edits.
- Kitty keyboard protocol support, so applications that ask for it receive
  unambiguous key events.
- Optional ligatures and optional inline images (kitty graphics protocol,
  iTerm2 `OSC 1337;File=`, sixel), both off by default and bounded in memory.
- Desktop notifications from the application through `OSC 9` and `OSC 777`.
- Clipboard integration: copy/paste of selections and bracketed paste, with
  application-driven clipboard access through `OSC 52` disabled by default.
- Save and reload notifications for the configuration.
- Command-line interface (`--working-directory`, `--profile`, `--command`,
  `--title`, `--config`, `--new-window`, `--debug`) and `--version`.
- Packaging: Arch `PKGBUILD`, a freedesktop desktop entry with a committed icon
  set in six sizes, and a rootless `packaging/linux/install.sh` /
  `uninstall.sh` pair for a per-user install.
- `scripts/setup.sh`, an idempotent bootstrap that installs the system
  dependencies for the detected distribution, checks the Rust toolchain and the
  default Nerd Font without downloading anything, builds with
  `cargo build --release --locked` and optionally installs the result.
  Documented in [`docs/INSTALL.md`](docs/INSTALL.md).

### Changed

- The project is versioned `1.0.0`; `astraterm --version` reports the version of
  the workspace package.

[Unreleased]: https://example.invalid/astraterm/compare/1.0.0...HEAD
[1.0.0]: https://example.invalid/astraterm/releases/tag/1.0.0
