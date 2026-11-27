# Status

**Usable: a real terminal with native tabs and splits.** Version **1.3.0**. The
whole stack is implemented and covered by **755 automated tests**
(`cargo test --workspace`) plus manual verification on Wayland/Hyprland with
Neovim, zsh and the Omarchy prompt:

| Crate | Contents | State |
|---|---|---|
| `crates/core` | shared ids, cell metrics/geometry, application events | implemented |
| `crates/config` | one typed config model, TOML loader/writer, validation, atomic + watched config service, themes, keymap | implemented |
| `crates/workspace` | recursive split tree, tabs, focus traversal, layout math, window/tab metadata | implemented |
| `crates/pty` | `portable-pty` adapter with bounded queues and reader/writer/waiter threads | implemented |
| `crates/terminal` | `alacritty_terminal` adapter: snapshots, damage, selection, search, key/mouse/paste encoding, `OSC 133` marks | implemented |
| `crates/session` | session lifecycle, output coalescing with per-session budgets, cwd tracking | implemented |
| `crates/renderer` | wgpu quad pipeline + glyphon text, damaged-row shaping cache, retained canvas repainting only damaged bands | implemented |
| `crates/settings-ui` | native, keyboard-driven settings panel model and text projection | implemented |
| `crates/platform` | clipboard (`arboard`), URL/XDG path and font discovery without `sh -c` | implemented |
| `crates/app` | winit window and event loop, tabs/splits, keybinding dispatch, mouse selection, search, settings overlay, workspace restore, CLI | implemented |

Build, run and packaging instructions live in the [README](../README.md); this
page records the state and the numbers.

## What 1.1 to 1.3 added

- **A native clipboard, images included (1.2.2).** Copy and paste go through the
  `arboard` library — the Wayland data-control protocol with an X11 fallback —
  instead of a spawned `wl-copy`/`wl-paste`/`xclip`/`xsel`, so no helper process
  can block the window. Pasting an image writes the PNG to a temporary file and
  inserts its path, after validating it against a 16 MiB and 16-megapixel limit.
  `wl-clipboard` is no longer a runtime dependency. In 1.2.3 a `copy_on_select_target`
  key arrived, choosing whether the selection gesture writes the clipboard or the
  primary selection; a middle click pastes the primary.
- **A floating terminal that is a real workspace (1.2.0).** It opens in the
  directory of the pane you are in, holds its own split tree with dividers and
  per-pane sizes, and clicking a floating pane focuses it. It stays outside the
  mosaic: reflow, layout and the restored workspace never see it.
- **Directory inheritance (1.2.0).** A new tab starts where the active tab is; a
  floating pane starts where you opened it from. Both follow the pane's live
  directory, refreshed from `/proc`, so a `cd` in the parent is inherited rather
  than the directory the parent started in. An explicit `working_directory` path
  still wins.
- **Grid-anchored glyphs (1.2.0, 1.3.0).** A cell whose glyph does not advance
  exactly one cell — emoji, CJK, a fallback script — is placed on its grid column
  instead of pushing the rest of the row, which is what keeps a powerlevel10k
  prompt and a Neovim status line aligned; a ligature that spans several cells
  lands on its columns while still shaping as one glyph (1.3.0). Glyph colours
  also stopped being converted to linear light on a plain UNORM surface, so text
  is no longer drawn darker than the theme asks.
- **Typography controls (1.2.0).** `[font] integer_cell_width` snaps the cell
  advance to whole pixels, `[font] hinting` (off by default) keeps real glyph
  weight instead of snapping outlines, and `[font] solid_shades` paints `░▒▓` as a
  flat blend the way foot does.
- **Only the damaged rows are repainted (1.3.0).** The renderer composes into a
  retained canvas, repaints the bands that changed and blits them into the frame;
  an unchanged frame draws and presents nothing.
- **Shell integration (1.3.0).** `OSC 133` prompt and command marks are recorded
  and shown in the status bar, and `ctrl+shift+up` / `ctrl+shift+down` jump
  between prompts.
- **Input method (1.2.1, 1.3.0).** The candidate window is positioned at the
  painted cursor cell and follows it through output, keys, tab and pane switches,
  resize, scale and font changes, scrolling, clicks and focus changes; the
  composition in progress is drawn at the cursor and cleared on commit.
- **A configurable bell (1.3.0).** `[terminal] bell` chooses `"visual"` (the
  default), `"none"` or `"audible"`, which rings through a worker thread and
  degrades to the visual path when no sound backend exists.
- **Mouse granularity (1.3.0).** Double click selects a word using
  `terminal.word_delimiters`, triple click the line, and `Alt`+drag selects a
  rectangle whose copy keeps the rectangle's padding.
- **Image protocols (1.1.x, 1.3.0).** Kitty graphics sent in chunks (`m=1`) are
  reassembled and drawn, an image survives a resize instead of vanishing, and
  sixel `HLS` palettes are decoded with the same fidelity as RGB and grayscale.

Measured on the development machine (Intel UHD 630, Wayland, 2494×1396 window):
about 1 % idle CPU with one pane and roughly 129 MB RSS in a debug build. The
1.3.0 renderer repaints only damaged rows: on a 480×135-cell screen one changed
line redraws 1.6 % of the characters (2 of 135 bands), and damage tracking costs
87 µs against 2.2 ms of scene composition. The 1.2.2 row walk brought a 200×50
pane to 0.87 ms per frame for dense CJK, 0.28 ms for plain ASCII and 0.25 ms for
emoji, from 55.6 ms, 0.95 ms and 0.60 ms. See
[docs/PERFORMANCE.md](PERFORMANCE.md) for the measurement procedure and
[docs/ARCHITECTURE.md](ARCHITECTURE.md) for how the pieces fit together. Changes
are recorded in [CHANGELOG.md](../CHANGELOG.md).
