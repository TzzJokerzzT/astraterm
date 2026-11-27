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
| Kitty keyboard protocol | flags 1/2/4/8/16; `CSI ? u` query, `CSI = u` set, `CSI > u` push, `CSI < u` pop; encoder in `input.rs` | `keyboard_mode_query_is_answered_when_the_protocol_is_enabled`, `keyboard_mode_query_is_silent_when_the_protocol_is_disabled`, `push_set_and_pop_update_the_reported_flags`, `disambiguate_turns_ctrl_letters_into_escape_codes`, `event_types_are_reported_only_when_requested` |
| Cursor visibility and shape | `?25h/l`, DECSCUSR (`CSI n SP q`) | `cursor_visibility_and_shape_follow_decscusr` |
| OSC title | OSC 0 and OSC 2, BEL and ST terminators | `osc_title_is_reported_as_an_event` |
| Bell | BEL is reported as an event | `bell_is_reported` |
| Notifications (OSC 9, OSC 777) | `OSC 9;<body>` and `OSC 777;notify;<title>;<body>`, BEL or ST terminated; consumed before the VT parser, reported as a session event, the text sanitized and the desktop notification gated by `[notifications] on` | `osc_9_and_osc_777_reach_the_application_as_events`, `osc_9_with_a_bel_terminator_is_captured`, `osc_9_with_an_st_terminator_is_captured`, `osc_777_notify_carries_a_title_and_a_body`, `osc_777_with_another_subcommand_is_ignored`, `a_notification_split_into_one_byte_chunks_is_reassembled`, `a_foreign_osc_next_to_a_notification_stays_byte_identical` (`crates/terminal/src/image.rs`); `a_notification_sequence_becomes_a_session_event` (`crates/session/src/manager.rs`); `the_notification_policy_matrix_only_fires_when_it_should` (`crates/config/src/schema.rs`); `control_characters_are_removed_and_whitespace_is_collapsed`, `both_parts_are_cut_to_their_character_budget`, `multibyte_characters_are_never_split` (`crates/platform/src/notifications.rs`) |
| Scrollback | bounded history, scroll into history and back | `scrollback_holds_older_lines_and_scrolling_shows_them` |
| Reflow on resize | wrapped text reflows, cursor anchored | `resize_reflows_wrapped_text` |
| Wide characters | double-width cells with a spacer cell | `wide_characters_use_two_columns` |
| Combining marks | kept attached to the base cell | `combining_marks_stay_attached_to_their_base_cell` |
| Selection | simple, line and rectangle selection produce text | `selection_produces_the_expected_text`, `line_selection_includes_whole_lines` (`crates/terminal/tests/`); `a_block_selection_keeps_its_rectangle_while_the_drag_grows`, `the_range_handed_to_the_engine_copies_exactly_the_word` (`crates/app/src/word.rs`) |
| Shell integration (OSC 133) | `A`/`B`/`C`/`D` marks recorded per buffer row and carried in `Snapshot::marks`; the status bar reports the running command and its exit code | `every_mark_kind_reaches_the_snapshot_on_its_row`, `marks_stay_on_their_buffer_lines_when_the_view_scrolls`, `marks_whose_rows_were_trimmed_away_are_dropped`, `a_terminal_without_osc_133_produces_the_snapshot_it_always_did` (`crates/terminal/tests/shell_marks.rs`); `prompt::tests` (`crates/app/src/prompt.rs`) |
| Damage tracking | changed rows reported per snapshot | `damage_reports_rows_that_changed` |
| Buffer text | visible screen text and per-line text | `snapshot_text_round_trips_visible_lines`, `line_text_exposes_buffer_lines_for_search` |
| OSC 8 hyperlinks | destination carried beside the text in `CellSnapshot`, opened by Ctrl+click when `terminal.open_urls_on_click` is on; only the schemes the opener already accepts reach it | `an_osc8_hyperlink_reaches_the_snapshot`, `a_cell_without_a_hyperlink_carries_none`, `an_empty_uri_closes_the_link`, `an_opened_hyperlink_with_no_visible_text_links_no_cell`, `a_linked_cell_is_underlined_even_when_the_program_asked_for_no_style` (`crates/terminal/src/engine.rs`); `a_click_on_a_linked_cell_opens_the_hyperlink`, `a_click_on_an_unlinked_cell_still_opens_the_visible_url`, `an_unlinked_non_url_cell_opens_nothing`, `a_refused_scheme_never_reaches_the_opener` (`crates/app/src/ui.rs`) |
| OSC 52 | disabled by default | `osc52_is_disabled_by_default` |
| Images (opt-in) | kitty graphics, iTerm2 `OSC 1337;File=` and sixel captured before the VT parser; decode, anchoring and drawing behind `terminal.images` | `the_engine_keeps_the_text_and_hands_the_images_over`, `arbitrary_chunking_does_not_change_the_result` (`crates/terminal/tests/image_scanner.rs`); `png_over_max_dimension_is_rejected_before_expanding`, `kitty_wire_chunking_is_accepted_and_reassembled`, `iterm2_inline_zero_is_unsupported` (`crates/terminal/src/image_decode.rs`); `a_captured_kitty_image_is_anchored_and_drawn`, `images_are_discarded_without_decoding_when_the_option_is_off`, `a_partially_visible_image_is_clipped_instead_of_overflowing`, `an_anchor_the_resize_pushed_off_screen_keeps_its_image`, `a_resize_drops_and_counts_the_anchors_beyond_the_buffer` (`crates/app/src/ui.rs`) |
| Malformed sequences | never corrupt the grid | `malformed_escape_sequences_do_not_corrupt_the_grid` |

`OSC 9` and `OSC 777;notify;` are handled the same way as the image protocols:
`alacritty_terminal`'s `vte` dispatches an unknown OSC body to `unhandled`, which
only logs at debug level and offers no hook to the embedder, so the sequence has to
be captured in front of the parser or it is lost silently. A `777` with any other
subcommand is not a notification and stays byte-identical on its way to the
parser. Nothing here claims the rest of the desktop-notification ecosystem
(urgency, icons, actions); the emitted notification is plain title plus body.

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
| `kitty` (`disambiguate`, `report_event_types`, `report_alternate_keys`, `report_all_keys_as_esc`, `report_associated_text`) | Key encoding; release and repeat forwarding |

Shift+click bypasses mouse reporting so local text selection still works when an
application captures the mouse.

### Pointer: click granularity and the rectangle

The local selection belongs to the application, so these rules are AstraTerm's own:

| Gesture | Selection |
|---|---|
| Press and drag | Linear selection from the press to the pointer |
| Double click | The word under the pointer, delimited by `terminal.word_delimiters`. A click on a delimiter selects that one cell, whitespace and control characters always end a word, and a double-width glyph is never split. |
| Triple click | The whole line |
| **Alt+drag** | **Rectangle**: every row the drag covers contributes the same column range (`SelectionKind::Block`), and the copy is padded with blanks so the block keeps its shape. Add Shift when a program has mouse reporting on, so the drag stays local. |

Alt is the rectangle modifier everywhere the terminal owns the pointer, including
when an application reports mouse input and Shift is held to keep the drag local.
The linear selection, the copy-on-select gesture and the clipboard are untouched by
it: a drag without Alt is exactly the selection it always was, and with
`terminal.copy_on_select` off no selection text is built at all — the engine is
never asked to walk the selection.

### Shell integration, the status bar and the bell

`OSC 133;A|B|C|D` marks are recorded by the engine against the buffer row they
arrived on and exposed as `Snapshot::marks`. The status bar reads them through the
engine's own accessor and shows `running` while a command is in flight and
`exit <code>` (or `exit ?` when the shell sent no readable code) once it ends; a
program that never sends the sequence sees exactly the status bar it had before.
`jump-to-previous-prompt` (`Ctrl+Shift+Up`) and `jump-to-next-prompt`
(`Ctrl+Shift+Down`) scroll the focused pane to the nearest prompt outside the
viewport — backwards lands it on the top row, forwards on the bottom one — and both
do nothing at all when there is no prompt to move to, so a session without shell
integration is unaffected.
Nothing is recorded on the alternate screen, and a resize — which reflows the
buffer — clears the marks rather than leaving them pointing at unrelated rows.

`[terminal] bell` decides what `BEL` does: `none` drops it, `visual` (the default)
keeps the `bell: <title>` status message and flashes the focused pane's border, and
`audible` rings the speaker instead. The audible path never runs on the UI thread:
the helper is resolved from what exists on the machine at startup and rung on its
own worker. On a machine with no usable helper the bell degrades to the visual
answer instead of vanishing.

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

Source: `crates/terminal/src/input.rs`. Two encoders live there: the classic xterm
one documented first, and the Kitty keyboard protocol, used only while the
application has asked for it. Coordinates and modifiers follow the xterm
convention in both: modifier parameter `m = 1 + shift + 2*alt + 4*ctrl`.

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

### Kitty keyboard protocol

Advertised by default (`terminal.kitty_keyboard = true`). The emulation core
answers `CSI ? u` with the mode at the top of its stack and honours `CSI = u`
(set), `CSI > u` (push) and `CSI < u` (pop), so an application can switch the
protocol on, stack a mode and restore it on exit. The flags reach the UI and the
encoder through `TerminalEngine::modes().kitty`.

With no flags active the encoder emits exactly the xterm sequences above;
`xterm_encoding_is_unchanged_with_kitty_off` is the regression guard for that.
Under the flags, the encoding is `CSI code[:shifted:base][;m[:event]][;text] u`:

| Flag | Value | Effect on the encoder |
|---|---|---|
| disambiguate | 1 | `Ctrl`/`Alt` combinations, `Escape`, and modified `Enter`/`Tab`/`Backspace` switch to `CSI code u`, with a `;m` parameter when modifiers differ from none. Plain text and unmodified `Enter`/`Tab`/`Backspace` keep their bytes. |
| report_event_types | 2 | Appends `:1` (press), `:2` (repeat) or `:3` (release) to the modifier parameter. |
| report_alternate_keys | 4 | Emits `code:shifted:base` for a shifted character. |
| report_all_keys_as_esc | 8 | Text and the control keys (`Escape`, `Enter`, `Tab`, `Backspace`) use `CSI code u`; arrows, `Home`/`End`, `Insert`/`Delete`, `PageUp`/`PageDown` and the function keys keep their legacy sequences. |
| report_associated_text | 16 | Appends the text the key produced, as codepoints: `CSI code;m;text u`. |

Functional keys never use the protocol's fixed numbers (`57_348` for `Insert`
through `57_357` for `End`, `57_364` for `F1`), even under
`report_all_keys_as_esc`. Parsers that only know `CSI u` for text read that range
as codepoints in the Unicode Private Use Area: crossterm has no entry for
`57_348`–`57_357`, so `CSI 57352u` (Up) arrives as `Char('\u{e008}')` and the
application types a Nerd Font glyph instead of moving the cursor. This is what
broke `atuin search --interactive`, which pushes `CSI > 13 u` (disambiguate |
report_alternate_keys | report_all_keys_as_esc) and then painted that glyph in
its filter. foot and Ghostty keep the legacy sequence for these keys under the
same flags, so the encoder matches them;
`functional_keys_keep_their_legacy_form_under_all_keys_as_esc` pins the bytes.

Release and repeat are reported only under flag 2. The application forwards a
release to the PTY only while the focused session has `REPORT_EVENT_TYPES` on and
marks a repeated press as `Repeat` under the same condition; otherwise the
release is dropped and a repeat stays a press, so an xterm session still sees one
event per key (`releases_are_forwarded_only_with_report_event_types`). A
functional key carries the event type in its own parameter block —
`CSI 1;m:event A` for arrows and `Home`/`End`, `CSI n;m:event ~` for
`Insert`/`Delete`/`PageUp`/`PageDown`/`F5`+, `CSI 1;m:event P` for `F1`–`F4` —
and an unmodified press keeps the bare sequence (`CSI A`, `CSI 5~`), so
nothing changes for an application that only reads presses
(`functional_keys_report_events_in_their_legacy_form`). Text keys have no such
form: their release sends nothing rather than repeating the press bytes
(`a_text_release_never_repeats_the_text_bytes`).

One upstream detail: `CSI ? u` reports the top of the *push* stack, so an
application that only uses `CSI = u` (set, never push) queries `0` while its
flags are in fact active. Applications that push on entry and pop on exit, which
is the usual pattern, see the right answer.

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
12. **Layout characters and dead keys.** With a Spanish layout, `AltGr+1` writes
    `|`, `AltGr+4` writes `~`, `AltGr+2` writes `@` and `AltGr+3` writes `#`; the
    dead key `´` followed by `a` writes `á`. Verify with a program that shows the
    bytes, so an `ESC` prefix is visible:

    ```sh
    cat -v   # AltGr+1 must print |, never ^[|
    ```

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

- **Kitty keyboard protocol coverage is protocol-shaped, not exhaustive.** The
  flag is advertised by default, the five flags of the current protocol
  (1/2/4/8/16) are honoured, and the query/set/push/pop sequences are handled by
  the emulation core. What is missing is platform input the window layer does not
  deliver (see the composed-text entry below). `terminal.kitty_keyboard = false`
  restores the historical xterm-only behaviour: no `CSI ? u` answer, a held key
  is a repeated press, and a release sends nothing.
- **Composed text is delivered, not drawn.** The window opts into the platform
  input method (`window.set_ime_allowed(true)` with `ImePurpose::Terminal` in
  `crates/app/src/app.rs`), so the characters a layout or an input method
  composes arrive as `WindowEvent::Ime`: `Ime::Commit` is written to the focused
  pane as raw UTF-8, bypassing the shortcut and key-encoding paths. That covers
  dead keys (`´` then `a` gives `á`) and IME-based input (pinyin, kana), and it
  also makes `AltGr` a character rather than Meta: `|`, `~`, `@`, `#` and `\`
  on the Spanish layout are written as themselves instead of `ESC` + character,
  and a layout's third level no longer matches an `alt+1`-style binding. `crates/app/src/keys.rs`
  owns that decision (`altgr_composition`, `chord_modifiers`) and the guard that
  drops a commit repeating the text a key press just wrote
  (`commit_repeats_key`).

  The **candidate window is positioned at the cursor cell**. `Window::set_ime_cursor_area`
  is called with the physical rectangle of the terminal cursor cell, computed by
  `crates/app/src/ime.rs` from the same geometry the renderer paints it with: the
  pane rectangle without its focus border, plus the terminal padding, plus
  `column * cell width` and `row * cell height`. That covers mosaic panes and the
  floating terminal, a nonzero `appearance.padding_*`, a fractional cell advance
  (the painted cell is one whole pixel wider than the advance, and the area is that
  cell, never the glyph), and it is clamped into the pane when the cell would fall
  past its edge — the frames between a window resize and the session's own resize.
  It is recomputed from the events that move it (output, a key, a command, a click
  that moves the focus, a wheel scroll, a window resize, a scale or font change, a
  tab or pane switch, the floating terminal appearing or disappearing) and only sent
  when the rectangle differs from the last one, so it costs the pane rectangle the
  frame being drawn already needs and never requests a redraw of its own. Two moments
  re-send it unchanged, because the platform has forgotten it: a newly enabled input
  method and each preedit update.

  One position is one cell off. While the cursor sits on the trailing half of a wide
  glyph, the renderer paints the cursor on the glyph's own cell but the rectangle is
  computed from the grid column the engine reports (read without a full snapshot), so
  the candidate window is one cell to the right until the cursor moves off that cell.

  With **no cursor on screen** — the program turned `DECTCEM` off, or the cursor row
  is scrolled out of view — what is sent is a **zero-size rectangle at the pane's
  inner corner**. There is no cell to align to, winit has no "clear the area" call,
  and a zero-size rectangle is what tells an input method there is no caret: the
  candidate window then falls back to the platform's own placement instead of
  anchoring to a stale cell of the last composition, and the origin keeps that
  fallback inside the terminal rather than over the tab strip.

  What remains missing:
  - the **preedit** is not drawn. The text being composed (`´` waiting for a
    vowel, the pinyin buffer) is invisible until it is committed; there is no
    inline composition at the cursor and no reflected preedit anywhere else;
  - while an **overlay** (search, settings, rename, confirmation) is open, commits
    are dropped rather than typed into the overlay, because those panels read
    their own text from key events.
- **Ligatures are opt-in.** A row is shaped as whole strings within each grid-anchored
  segment (see the wide-character note below); glyph positions inside a segment come
  from the shaper. Cell metrics come from the shaped advance of `MMMMMMMMMM`
  (`crates/renderer/src/text.rs`), so grid alignment relies on the font's monospace
  advances. Because of that, `font.ligatures` defaults to `false` and disables
  `liga`/`calt` explicitly; turning it on hands the row to the font, which is fine for
  strictly monospaced ligature fonts (a ligature that occupies exactly two cells) and
  can drift for fonts whose substitutions change the advance.
- **Images are supported, with limits and only when enabled.**
  `terminal.images` defaults to `false`; while it is off, image sequences are
  removed from the stream and discarded without being decoded. With it on,
  `crates/terminal/src/image.rs` captures the three protocols before the VT parser
  sees them, `crates/terminal/src/image_decode.rs` decodes them, and
  `crates/app/src/ui.rs` anchors them to a buffer line and draws them between the
  pane background and the glyphs. What works:

  | Protocol | Accepted | Refused (`DecodeError::Unsupported`) |
  |---|---|---|
  | kitty graphics (APC `ESC _ G`) | `a=T` / `a=t` with `f=100` (one PNG) or `f=24`/`f=32` (raw RGB/RGBA with `s=` and `v=`) | chunked transfers (`m=1`), any other format, query/put/delete actions |
  | iTerm2 inline image (`OSC 1337;File=`) | `name=`, `width=`, `height=`, `preserveAspectRatio=` arguments (parsed; the display ones are ignored) with a base64 PNG body | `inline=0` (download-only) |
  | sixel (DCS `ESC P … q`) | RGB colour definitions (`#Pc;2;Pr;Pg;Pb`), raster attributes | HLS colour definitions (`#Pc;1;…`) |

  Limits, all enforced before any allocation: 8 MiB per payload, 4096 pixels on
  either axis, 2048x2048 pixels (16 MiB RGBA) per image, and a 64 MiB decoded
  budget per session with oldest-first eviction. An image occupies whole cells
  (`ceil(px / cell)` with the pane's font metrics) and is clipped to the pane, so
  it never overflows the grid or a neighbouring pane.

  What does **not** work, and why:

  - An image is anchored to the buffer line the cursor was on when it arrived.
    When the screen scrolls, the anchors move with the lines, so an image follows
    its line into the scrollback and disappears only when that line is dropped.
  - A **resize** never re-anchors, and the reason is in the core: reflow moves the
    numbers a line has (measured with a marker on line 4 of a pane that went from 20
    to 40 columns: it became line 3 and stayed there when the pane went back to 20),
    and `alacritty_terminal` exposes no old-to-new line map. An anchor can therefore
    end up one or more lines away from the text it belongs to after a resize. That is
    a limitation, not a silent drop: a resize **keeps every anchor whose line still
    exists in the buffer**, on screen or not. An image in the scrollback is culled
    while it is out of view and drawn again when the view is scrolled back to it. Only
    anchors whose line the reflow removed (truncated history, the alternate screen
    replacing the grid) are dropped, and those drops are counted in
    `SessionImages::dropped_anchors` rather than happening unnoticed.
  - Once the **scrollback is full** the core stops moving the oldest line number,
    so a very old image can stay pinned to a row instead of scrolling away.
  - Anchors are dropped when the **alternate screen** is entered or left, because
    the two grids have independent line numbers. A program that draws images on
    the alternate screen keeps them only while it stays there. Those anchors are
    counted like any other anchor whose line does not exist on the shown grid.
  - kitty image *placement* (`c=`, `r=`, `x=`, `y=`, `z=`, `p=`) is ignored: the
    image is drawn at the cursor cell with the size its pixels imply.
  - No delete/query actions (`a=d`, `a=q`), no animation, no multiple image
    chunks, and nothing is ever downloaded from a URL or written to a file.
  - There is no clip rectangle for images in the renderer, so an image that is
    only partly on screen is cropped through its texture coordinates; the visible
    part is correct, the cropped part is not drawn at all.
- **Search is plain substring.** `SearchState` / `find` / `count_matches`
  (`crates/terminal/src/search.rs`) and the interactive overlay (`find_in_snapshot`
  in `crates/app/src/ui.rs`) do substring matching with optional case folding.
  There is no regular-expression search. The overlay highlights matches on the
  visible screen; the buffer-wide forward/backward API exists in
  `crates/terminal/src/search.rs`.
- **Wide and fallback glyphs are grid-anchored.** `paint::row_text_item` splits a row
  into segments: a run of one-column cells in the primary face stays a single shaped
  string, and every cell that is wide or whose glyph may come from a fallback face
  (emoji and symbol blocks, `U+FE0F`) becomes its own item positioned at
  `column * cell_width` and clipped to the columns it owns plus one cell of slack. A
  fallback glyph whose advance differs from the reserved columns is therefore painted
  inside its own slot instead of pushing the rest of the row, which is what keeps a
  powerlevel10k prompt or a neovim status line aligned. The trade-off is that such a
  glyph can overflow by up to one cell: alignment is exact, clipping is not.
- **OSC 8 hyperlinks click, but do not yet look like links.** The destination
  reaches the UI in `CellSnapshot::hyperlink` (the model drops it when the span is
  closed with an empty URI), and with `terminal.open_urls_on_click` on, a
  Ctrl+click on a linked cell opens that destination instead of the text under
  the cursor; a cell without a link still opens the visible `http`/`https` URL.
  The destination is program-controlled, so `ui::link_target` screens it with
  `astraterm_platform::linux::is_safe_url` before the opener is called, and a
  refused scheme opens nothing — it does not fall back to the text either.
  What is missing is the styling: the whole visual indication today is the
  existing underline flag, which `convert_cell` sets for a linked cell whose
  program asked for no underline. A renderer-side treatment would give
  `paint::cell_style` and `paint::cell_underlined` a link colour from the theme
  (so the underline is drawn in it rather than in the foreground) and the UI a
  hand cursor when the pointer hovers a linked cell; neither touches the
  snapshot.
- **No bell sound/visual bell.** BEL becomes a status message and a `bell` flag,
  not an audible bell.
- **No scrollback search while output is flooding.** Search is computed on the
  current snapshot; there is no incremental index.
