# Terminal compatibility

AstraTerm runs a real VT/xterm emulator (`alacritty_terminal` 0.26) behind
`crates/terminal`. This document states what is implemented, how it is tested,
and what is deliberately missing. Do not claim broader compatibility than this
file and `crates/terminal/tests/engine_compat.rs` support.

## Protocol surface

The executable matrix is `crates/terminal/tests/engine_compat.rs`. Each row below
names the test that covers it.

| Capability | Details | Test |
|---|---|---|
| Plain text and cursor motion | CR/LF, cursor position | `plain_text_lands_in_the_grid`, `newlines_and_carriage_returns_move_the_cursor` |
| 16 colours | SGR 30–37 / 90–97 mapped to palette slots 0–15 | `ansi_16_colours_are_resolved_to_palette_slots` |
| 256 colours | SGR `38;5;n` / `48;5;n` preserved as indices | `ansi_256_colours_are_preserved` |
| Truecolour | SGR `38;2;r;g;b` / `48;2;r;g;b` preserved as RGB | `true_colour_is_preserved` |
| Attributes | bold, italic, underline, strikethrough, dim, inverse, hidden | `background_and_attributes_are_reported` |
| Erase and scroll | `ED`, scrolling regions used by line feeds | `erase_and_scroll_operations_work` |
| Alternate screen | `1049h` / `1049l`, primary content restored | `alternate_screen_preserves_the_primary_content` |
| Bracketed paste | mode `2004`, encoding of pasted text | `bracketed_paste_and_application_cursor_modes_are_tracked`, `bracketed_paste_encoding_follows_application_mode` |
| Mouse reporting modes | `1000`, `1002`, `1003`, `1006` tracked; SGR and X10 encoding in `input.rs` tests | `mouse_reporting_modes_are_tracked`, `sgr_mouse_encoding_is_one_based`, `classic_mouse_encoding_uses_offset_bytes` |
| Application cursor keys | mode `DECCKM` (`?1h`) changes arrow encoding to SS3 | `bracketed_paste_and_application_cursor_modes_are_tracked` |
| Cursor visibility and shape | `?25h/l`, DECSCUSR (`CSI n SP q`) | `cursor_visibility_and_shape_follow_decscusr` |
| OSC title | OSC 0 and OSC 2, BEL and ST terminators | `osc_title_is_reported_as_an_event` |
| Bell | BEL is reported as an event | `bell_is_reported` |
| Scrollback | bounded history, scroll into history and back | `scrollback_holds_older_lines_and_scrolling_shows_them` |
| Reflow on resize | wrapped text reflows, cursor anchored | `resize_reflows_wrapped_text` |
| Wide characters | double-width cells with a spacer cell | `wide_characters_use_two_columns` |
| Combining marks | kept attached to the base cell | `combining_marks_stay_attached_to_their_base_cell` |
| Selection | simple and line selection produce text | `selection_produces_the_expected_text`, `line_selection_includes_whole_lines` |
| Damage tracking | changed rows reported per snapshot | `damage_reports_rows_that_changed` |
| Buffer text | visible screen text and per-line text | `snapshot_text_round_trips_visible_lines`, `line_text_exposes_buffer_lines_for_search` |
| OSC 52 | disabled by default | `osc52_is_disabled_by_default` |
| Malformed sequences | never corrupt the grid | `malformed_escape_sequences_do_not_corrupt_the_grid` |

Supporting unit tests live in `crates/terminal/src/input.rs` (encoding),
`crates/terminal/src/search.rs` (search) and `crates/terminal/src/engine.rs`
through the snapshot types.

### Modes the UI also honours

Beyond the grid, the application reads these mode flags from
`TerminalEngine::modes()`:

| Mode flag | Used for |
|---|---|
| `app_cursor`, `app_keypad` | Key encoding |
| `bracketed_paste` | Pasting |
| `mouse_click`, `mouse_drag`, `mouse_motion`, `sgr_mouse`, `utf8_mouse` | Mouse forwarding instead of local selection/scroll |
| `alt_screen` | Status bar indicator |
| `show_cursor` | Cursor painting |
| `focus_reporting` | `ESC[I` / `ESC[O` on window focus changes |

Shift+click bypasses mouse reporting so local text selection still works when an
application captures the mouse.

## `$TERM` policy

- Default: `TERM=xterm-256color`, `COLORTERM=truecolor`, plus `ASTRATERM=1` and
  `ASTRATERM_VERSION`.
- The PTY builder adds `TERM`/`COLORTERM` only if the spec does not already
  contain them (`crates/pty/src/lib.rs`).
- `validation.rs` keeps an allowlist of terminfo entries that exist on virtually
  every system (`WIDELY_AVAILABLE_TERMS`): `xterm-256color`, `xterm`,
  `screen-256color`, `screen`, `tmux-256color`, `linux`, `vt100`, `alacritty`,
  `foot`.
- A custom `terminal.term` is allowed but produces a **warning**, not an error:
  the local machine may have the entry installed while a remote host reached over
  SSH does not, and then colours/keys degrade in confusing ways. AstraTerm never
  advertises a custom identifier silently; the value always comes from the
  configuration.

## Key encoding

Source: `crates/terminal/src/input.rs`. The encoder emits xterm-compatible
sequences; there is no Kitty keyboard protocol. Coordinates and modifiers follow
the xterm convention: modifier parameter `m = 1 + shift + 2*alt + 4*ctrl`.

### Printable keys and modifiers

| Input | Bytes |
|---|---|
| Printable character | UTF-8 text of the character |
| Shift + printable | The shifted character when the platform reports the unshifted one (`shifted_character`), otherwise the character as reported |
| Alt + key | `ESC` prefix (0x1b) before the encoded key |
| Ctrl + `a`…`z` | 0x01…0x1a |
| Ctrl + `[` | 0x1b |
| Ctrl + `\` | 0x1c |
| Ctrl + `]` | 0x1d |
| Ctrl + `^` | 0x1e |
| Ctrl + `_` | 0x1f |
| Ctrl + `@`, Ctrl + Space, Ctrl + `2` | 0x00 |
| Ctrl + `3` | 0x1b |
| Ctrl + `/` | 0x1f |
| Ctrl + other character | No control mapping: the character itself is sent (with Alt prefix if set) |

### Named keys

| Key | Normal | Modified (`m > 1`) |
|---|---|---|
| Up / Down / Right / Left | `CSI A` / `B` / `C` / `D` | `CSI 1;m A` etc. |
| Up/down/left/right in application cursor mode, no modifiers | `SS3 A` / `B` / `C` / `D` (`ESC O …`) | — (modifiers force CSI form) |
| Home | `CSI H` | `CSI 1;m H` |
| End | `CSI F` | `CSI 1;m F` |
| Insert | `CSI 2~` | `CSI 2;m~` |
| Delete | `CSI 3~` | `CSI 3;m~` |
| PageUp | `CSI 5~` | `CSI 5;m~` |
| PageDown | `CSI 6~` | `CSI 6;m~` |
| Enter | `CR` (0x0d) | Alt+Enter = `ESC CR` |
| Tab | `HT` (0x09) | Shift+Tab = `CSI Z` |
| Backspace | `DEL` (0x7f) | Ctrl+Backspace = 0x08 |
| Escape | `ESC` | Alt+Escape = `ESC ESC` |

Function keys:

| Key | Normal | Modified |
|---|---|---|
| F1 / F2 / F3 / F4 | `SS3 P` / `Q` / `R` / `S` | `CSI 1;m P` etc. |
| F5 | `CSI 15~` | `CSI 15;m~` |
| F6 | `CSI 17~` | `CSI 17;m~` |
| F7 | `CSI 18~` | `CSI 18;m~` |
| F8 | `CSI 19~` | `CSI 19;m~` |
| F9 | `CSI 20~` | `CSI 20;m~` |
| F10 | `CSI 21~` | `CSI 21;m~` |
| F11 | `CSI 23~` | `CSI 23;m~` |
| F12 | `CSI 24~` | `CSI 24;m~` |

The encoder also knows F13–F20 (codes 25, 26, 28, 29, 31, 32, 33, 34); F21 and
above produce no bytes.

### Paste

`encode_paste` normalises CRLF and LF to CR, then:

- bracketed paste mode on: `CSI 200~` + text + `CSI 201~`;
- otherwise: the normalised text.

### Mouse

`encode_mouse` returns nothing unless a mouse reporting mode is enabled. Motion
requires `1002` or `1003`.

| Mode | Encoding |
|---|---|
| SGR (`1006`) | `CSI < code ; column+1 ; row+1 M` for press/motion, `m` for release |
| UTF-8 (`1005`) | `CSI M` + `32+code` + UTF-8 encoded 1-based column and row |
| Classic X10 | `CSI M` + `32+code` + `32+column` + `32+row`, coordinates clamped to 223 |

`code` is the button (left 0, middle 1, right 2, none 3) plus modifier bits
(shift 4, alt 8, ctrl 16), plus 3 for an X10 release and plus 32 for motion.
Wheel events are sent by the application as buttons 64 (up) and 65 (down) using
the same SGR/X10 wire format. With no reporting mode, the wheel scrolls the local
scrollback by three lines per line of reported scroll delta.

Focus reporting (`1004`) sends `ESC[I` on focus in and `ESC[O` on focus out.

## Manual acceptance procedure

The automated tests cover the parser and encoder. The following procedure is the
manual complement; run it after changing the terminal adapter, input encoding or
rendering. Use a real terminal multiplexer-like workload, not just `nvim`
launching successfully.

### Neovim

```sh
nvim /tmp/astraterm-acceptance.txt
```

Prepare a file containing: plain ASCII, `你好世界`, `e` + combining acute, an emoji,
Nerd Font glyphs (for example `U+F015` or an icon from your prompt), and at least
300 lines of text.

Check, in this order:

1. **Colours.** Set `:set termguicolors` and `:syntax on`; open a file with
   syntax highlighting. 16-, 256- and truecolour groups must all render. Compare
   the same file in another terminal as a reference.
2. **Cursor shape.** DECSCUSR styles per mode (`:set guicursor`); insert/normal/
   replace should change shape (block/beam/underline).
3. **Navigation.** Arrows in normal and insert mode; `Home`/`End`,
   `PageUp`/`PageDown`; `F1`…`F12` (for example `:help`, `:set number!`).
4. **Modifiers.** `Ctrl` combinations (`Ctrl+W`, `Ctrl+N`, `Ctrl+P`), `Alt`
   combinations (`<M-j>` style mappings), `Shift+Tab`.
5. **Mouse.** `:set mouse=a`; click to move the cursor, drag to select, scroll
   with the wheel (SGR reporting). Then hold Shift and click: local selection
   must win.
6. **Splits and floating windows.** `:vsplit`, `:split`, open a floating window
   (for example `:lua vim.lsp.buf.hover()` with an LSP attached, or
   `:help` in a split). Borders and backgrounds must not smear.
7. **Completion.** `:` + `Tab` completion and insert-mode completion popups.
8. **Alternate screen.** Open `:help` (or any full-screen TUI), then leave it:
   the shell content underneath must be intact, including scrollback.
9. **Resize while running.** Resize the window and drag a split divider. Then
   `:echo &columns &lines` and confirm the values match the visible grid. Redraw
   with `Ctrl+L` and check for stale rows.
10. **Copy/paste.** Select with the mouse, `Ctrl+Shift+C`, paste into another
    application. Paste multi-line text with `Ctrl+Shift+V` into insert mode: no
    auto-indent cascade (bracketed paste), and CR line endings.
11. **Unicode and fonts.** The wide characters occupy two columns, combining
    marks stay on their base, emoji and Nerd Font glyphs are not clipped. A wide
    glyph may overflow its cell by a pixel; the column after it must remain
    aligned.

### tmux

```sh
tmux new -s acceptance
```

- status line colours and the `$TERM` seen inside (`echo $TERM`, expect
  `screen-256color` or `tmux-256color` as configured by tmux);
- `Ctrl+B %` and `Ctrl+B "` splits, pane borders, pane switching;
- `set -g mouse on`, then click/scroll/drag; without it the wheel scrolls the
  local history;
- resize the outer window while tmux is running: panes must reflow;
- copy mode (`Ctrl+B [`) with arrows/PageUp/PageDown.

### `less` and `htop`

```sh
seq 1 100000 > /tmp/lines.txt
less /tmp/lines.txt      # PageUp/PageDown, arrows, G/g, q restores the shell
htop                     # F1-F10, arrows, mouse, colour, CPU meters
```

For `less`, after quitting, the shell screen and scrollback must be intact
(alternate screen). For `htop`, function keys must work and the layout must
survive a resize.

Record, for each case: colour correctness, cursor shape, key responses, mouse
behaviour, redraw artefacts, and whether resizing needs a manual `Ctrl+L`.

## Known gaps

State these plainly; they are not bugs in the emulation core but in the adapter
or the application.

- **No Kitty keyboard protocol.** `TermConfig::kitty_keyboard` is `false` and the
  encoder emits xterm sequences only. Applications that probe for the Kitty
  protocol get no support and fall back.
- **No IME preedit.** `window.set_ime_allowed(false)` in `crates/app/src/app.rs`.
  CJK and other IME-based input cannot compose text; only characters delivered as
  key events work.
- **No image protocols.** There is no Sixel or Kitty graphics parsing/rendering
  path; images sent to the terminal are not displayed.
- **No ligature support.** Rows are shaped as whole strings and glyph positions
  come from the shaper; AstraTerm does not implement per-cell glyph placement or
  ligature-aware cells. Cell metrics come from the shaped advance of
  `MMMMMMMMMM` (`crates/renderer/src/text.rs`), so grid alignment relies on the
  font's monospace advances, not on ligature handling.
- **Search is plain substring.** `SearchState` / `find` / `count_matches`
  (`crates/terminal/src/search.rs`) and the interactive overlay (`find_in_snapshot`
  in `crates/app/src/ui.rs`) do substring matching with optional case folding.
  There is no regular-expression search. The overlay highlights matches on the
  visible screen; the buffer-wide forward/backward API exists in
  `crates/terminal/src/search.rs`.
- **Wide-character rendering.** `paint::row_text_item` appends a filler space
  after a wide glyph so following glyphs keep their exact column, which means the
  wide glyph itself may overflow into that filler/neighbouring cell. Alignment is
  preserved; pixel-perfect clipping is not.
- **No OSC 8 hyperlink styling.** The embedded core accepts OSC 8, but
  `CellSnapshot` has no hyperlink field, so links are not underlined or clickable
  and only the visible URL text can be opened.
- **No bell sound/visual bell.** BEL becomes a status message and a `bell` flag,
  not an audible bell.
- **No scrollback search while output is flooding.** Search is computed on the
  current snapshot; there is no incremental index.
