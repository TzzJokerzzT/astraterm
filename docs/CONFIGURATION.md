# Configuration

AstraTerm reads one configuration file and exposes every option twice: in TOML
(the portable source of truth) and in the native settings panel (`Ctrl+,`), which
edits the same typed model and writes the TOML back. This page documents every
option: what it does, what it is for, and where to change it.

Sections below:

- [Files and paths](#files-and-paths) — where files live, and which file wins.
- [Recipes](#recipes) — opacity, blur, status bar, right-hand tabs, themes.
- [Option reference](#option-reference) — every option, field by field: type,
  default, accepted values and effect.
- [Keyboard actions](#keyboard-actions) — the whole key map.
- [Profiles](#profiles) — named launch presets (in the option reference).
- [Validation](#validation) · [Save policy](#save-policy) · [What applies live](#what-applies-live)
- [Complete example](#complete-example) — a valid file that sets every section.
- [Gotchas](#gotchas) — inert keys, clamps, dependent defaults and units.

## Files and paths

| Path | Contents |
|---|---|
| `~/.config/astraterm/config.toml` | The configuration file (`$XDG_CONFIG_HOME` is honoured) |
| `~/.config/astraterm/themes/*.toml` | User themes, including the ones the panel saves |
| `~/.local/state/astraterm/workspace.toml` | Saved tab/split layout metadata |

- `--config <path>` uses a different configuration file for this run.
- The first run writes a documented default file so there is something to edit.
- `/etc/astraterm/config.toml`, shipped by the package, is a **copy template**:
  the loader never reads it.
- `Ctrl+Shift+Comma` (`reload-config`) re-reads the file after an external edit;
  otherwise a file watcher does it automatically. `Ctrl+Shift+R` renames the tab.

## Precedence

1. Built-in defaults (`crates/config/src/schema.rs`).
2. The user TOML file, layered on top of the defaults: a section or field you
   omit keeps its default, and unknown keys are reported as warnings.
3. `[[profiles]]` overrides, applied only to sessions created from that profile:
   `shell`, `args`, `command`, `working_directory`, `env`, `startup_command`,
   `label`, and (validated but not applied today) `theme` and `font_size`.
4. Command-line flags: `--config` picks the file; `--working-directory`,
   `--profile`, `--command` and `--title` apply to windows that start a fresh
   workspace (the first window, or one opened with `new-window`), not to a
   restored layout and not to tabs opened later with `new-tab`.

Saving from the panel always writes the *merged* result, so anything you set in
the TOML survives a panel save even if the panel never showed it.

## Recipes

### Translucent background

Opacity belongs to the application, applies live, and only affects the
background: text, the cursor and explicit cell colours stay opaque, so contrast
never depends on what is behind the window.

```toml
[window]
opacity = 0.85   # 0.1 = almost transparent, 1.0 = solid
```

Or open the settings panel with `Ctrl+,` → **Appearance** → **Window opacity** and
use `←`/`→`. The change is visible immediately; nothing restarts.

Requirements and limits:

- The window is created with a premultiplied-alpha surface so opacity can change
  without a new window. If the compositor offers no alpha mode, the surface stays
  opaque and the value has no visible effect (the app keeps working).
- Only the terminal background is painted with that alpha. Glyphs, cell
  backgrounds, pane borders, the tab strip and the status bar stay opaque, so
  chrome remains readable over any wallpaper.
- Values below `0.5` warn: text on a busy wallpaper stops being comfortable to
  read. `0.1` is the minimum accepted.

### Blur behind the window

Blur is a **compositor** capability: the terminal only makes its background
translucent and the compositor decides whether to blur what is behind it. AstraTerm
therefore never claims blur support — it depends on your desktop:

| Desktop | How to get blur |
|---|---|
| **Hyprland / Omarchy** | Blur is global: `decoration.blur.enabled = true`. On Omarchy the **Glass** control in the bar writes `~/.config/hypr/glass.lua`; enable blur there. Verify with `hyprctl getoption decoration:blur:enabled` (prints `bool: true` when active), then set `[window] opacity` below `1.0`. |
| **KDE Plasma** | System Settings → Desktop Effects → *Blur* on, plus a window rule for AstraTerm with *Force blur* if your theme requires it. |
| **GNOME** | No per-window blur built in; the *Blur my Shell* extension can provide it. |
| **Sway / wlroots** | Upstream Sway has no blur; some forks do. |
| **X11 without a compositor** | No blur, and transparency only if a compositing manager runs. |

Two rules of thumb:

- The background must be translucent **somewhere**. `[window] opacity = 1.0`
  gives the compositor nothing to blur; `appearance.background_blur = true` warns
  about exactly that.
- Compositor opacity **multiplies** with AstraTerm's own. On Omarchy, Glass
  applies `0.76` to every window through the `default-opacity` tag, so an
  AstraTerm window with `opacity = 0.85` ends up at roughly `0.65`. To let
  AstraTerm's value be the only one, exclude it from that tag in
  `~/.config/hypr/glass.lua`:

```lua
-- Keep AstraTerm's own opacity authoritative (class matches the app_id/WM_CLASS).
o.window({ class = "^astraterm$" }, { tag = "-default-opacity" })
```

`appearance.background_blur` is accepted for documentation and warns when it
cannot show; it never sends a request the compositor would ignore.

### Status bar with system readings

```toml
[appearance]
show_status_bar = true   # or Ctrl+Shift+S
status_bar_cpu = true
status_bar_ram = true
status_bar_clock = true
clock_format = "%H:%M"
```

The bar keeps session information (title, directory, grid size, `ALT`/`SEL`,
search indicator) on the left and shows `CPU 12% · RAM 61% · 15:13` on the right.
CPU and memory come from `/proc/stat` and `/proc/meminfo`, sampled **once per
second and only while the bar is visible** — hidden means no timer at all.
`clock_format` is a `strftime` format string evaluated by the C library in your
timezone and locale, so the usual specifiers (`%H %M %S %d %m %Y %b %a`, also
`%I %p`, …) work and other text is literal.

### Tabs on the right (Warp style)

```toml
[appearance]
tab_bar_position = "right"   # top | right | bottom | hidden
tab_strip_width = 200        # 120-480, device-independent pixels
```

Also reachable from the panel: **Appearance** → **Tab bar position** (cycles
top → right → bottom → hidden) and **Tab strip width**. On the right the tabs
stack vertically with the active indicator on the left edge, long titles are
ellipsised, and the terminal area narrows by `tab_strip_width`.

### Creating a theme from the panel

1. `Ctrl+,` → **Colours & themes**.
2. **Theme** picks a base; changing it loads that palette into the editor.
3. **Edit theme colour** selects the entry (background, foreground, accent,
   cursor, cursor text, selection, focused border, tab bar).
4. **Colour value** → `Enter` → type `#rrggbb` → `Enter`. Every change previews
   live; leaving the panel without saving discards it.
5. **Save theme as…** → `Enter` on the suggested name (`<theme>-custom`) or type
   your own → `Enter`.

The file is written atomically to `~/.config/astraterm/themes/<name>.toml`, the
theme list refreshes, `appearance.theme` is set to it and persisted, and the panel
shows the path. Names are sanitised (`Mi Tema` → `mi-tema`, max 40 characters);
`midnight` and `dawn` are reserved. An invalid name or colour is reported in the
panel and nothing is written.

## Option reference

Every key the loader accepts, grouped by TOML table. The columns are the same for
every table:

- **Key** — the TOML key as written in the file.
- **Type** — the TOML type of the value (the loader maps it to the Rust type in
  `crates/config/src/schema.rs`): bool, integer, float, string, array of strings
  or table. A value of the wrong TOML type is a parse error, not a warning.
- **Default** — the built-in default, in TOML syntax, used when the key or its
  whole table is absent.
- **Accepted values** — what is accepted. Ranges and rules come from
  `crates/config/src/validation.rs`: **error** means the configuration does not
  validate (it still loads and runs, but a panel save is refused), **warning**
  means it loads and is reported without blocking anything.
- **Effect** — what the value does in the running application, and whether it
  applies live or needs a new session/window.

A key that is present but not listed here is reported as a warning with its
dotted path and the text "unknown setting; it has no effect".

### `[window]`

The application window.

| Key | Type | Default | Accepted values | Effect |
|---|---|---|---|---|
| `width` | integer | `1280` | 200–16384; outside is an error | Width of a new window in logical pixels (multiplied by the display scale). Read in `create_window`, so a window has to be created; the compositor may still pick another size. |
| `height` | integer | `800` | 120–16384; outside is an error | Height of a new window in logical pixels. Read when a window is created. |
| `opacity` | float | `1.0` | 0.1–1.0; outside is an error, below 0.5 warns | Alpha of the painted terminal background. Only the background is translucent: glyphs, cell backgrounds, borders, tab strip and status bar stay opaque. Clamped to 0.1–1.0 when painted, so an out-of-range value still paints. Applies live. |
| `startup_maximized` | bool | `false` | `true`, `false` | Asks the compositor to map the new window maximized. Read when a window is created. |
| `show_window_controls` | bool | `true` | `true`, `false` | Requests window decorations (title bar, close button), drawn by the compositor or window manager. Read when a window is created. |
| `remember_geometry` | bool | `true` | `true`, `false` | **Accepted but not applied.** No code reads it. The last window size is written into `workspace.toml`, but restore creates the window from `width`/`height` below and ignores the saved size. |

### `[terminal]`

The shell, emulation and session behaviour.

| Key | Type | Default | Accepted values | Effect |
|---|---|---|---|---|
| `shell` | string | `"auto"` | `"auto"`, a program name, or an absolute path; empty is an error, and a value containing `/` must exist on disk | Program started for a new session. `auto` uses `$SHELL` when it is set and exists, otherwise `/bin/sh`; a name without `/` is looked up in `PATH`. Needs a new session. |
| `shell_args` | array of strings | `[]` | any strings | Arguments passed after the shell program name, one argv element each, never interpreted by a shell. A profile's `args` replace them for that profile's sessions. Needs a new session. |
| `term` | string | `"xterm-256color"` | non-empty and whitespace-free (otherwise an error); anything outside the widely available list warns: `xterm-256color`, `xterm`, `screen-256color`, `screen`, `tmux-256color`, `linux`, `vt100`, `alacritty`, `foot` | Value of `$TERM` for the child. Needs a new session. |
| `kitty_keyboard` | bool | `true` | `true`, `false` | Advertises and honours the Kitty keyboard protocol: `CSI ? u` is answered with the active flags, `CSI = u` / `CSI > u` / `CSI < u` set, push and pop them, and keys are encoded in the kitty form while flags are active. Off, keys use the xterm encodings. Needs a new session. |
| `images` | bool | `false` | `true`, `false` | Decodes and draws images sent as kitty graphics, iTerm2 `OSC 1337` or sixel, anchored to the grid. Off, the sequences are still captured by the scanner but dropped without being decoded. Read on every output pump, so it applies live; turning it off drops already-decoded images. |
| `scrollback_lines` | integer | `10000` | 100–1,000,000; outside is an error | History lines kept per session (bounded memory per pane). Fixed when the session is created. |
| `working_directory` | string | `"home"` | `"home"`, `"current"`, `"inherit"`, `""`, or an absolute path | Directory a new session starts in, in this precedence: an absolute path is used as written and outranks everything; otherwise a pane created from another pane — a new tab or the floating terminal — starts in that pane's live directory, so a `cd` in the parent is inherited; otherwise the policy applies: `home`/`""` = `$HOME`, `current`/`inherit` = the directory of the pane the session is created from (or `--working-directory`). A relative value that is not a policy is an error; a configured directory that does not exist warns and the session falls back to `$HOME`, then to the pane's directory. Needs a new session. |
| `confirm_on_close` | bool | `true` | `true`, `false` | Asks before closing a pane or tab whose child is still running, and before quitting with live sessions. Read at the moment of the action, so it applies live. |
| `copy_on_select` | bool | `false` | `true`, `false` | Copies the selection to the system clipboard (the same one `copy` fills) as soon as a mouse selection ends. Applies live. |
| `open_urls_on_click` | bool | `true` | `true`, `false` | Enables Ctrl+click on a detected `http`/`https` URL, which opens it with `xdg-open`; a plain click never opens anything. Applies live. |
| `word_delimiters` | string | `" /\\()\"'-.,:;<>~!@#$%^&*\|+=[]{}~?"` | any string | **Accepted but not applied.** Double-click word selection uses the terminal core's own rules; no code reads this value. |

### `[font]`

Typography. Every key here applies live.

| Key | Type | Default | Accepted values | Effect |
|---|---|---|---|---|
| `family` | string | `"JetBrainsMono Nerd Font"` | non-blank (blank is an error) | Primary font family handed to the shaper. If the family is not installed, cosmic-text resolves the request through the system font database (the same mechanism that supplies missing glyphs). Applies live. |
| `size` | float | `13.0` | 4–96; outside is an error | Font size in **logical pixels**, not points, multiplied by the display scale factor to get physical pixels. The row height is `size * line_height`. A zoom set with `increase-font-size`/`decrease-font-size` is stored in `preferences.toml` and overrides this value until `reset-font-size`. |
| `weight` | integer | `400` | 100–900 in steps of 100; anything else is an error | Weight requested from the shaper; bold cells still use weight 700. |
| `line_height` | float | `1.15` | 0.8–3.0; outside is an error | Row height as a multiple of `size`, rounded to whole pixels. |
| `letter_spacing` | float | `0.0` | −5–20; outside is an error | Extra tracking between shaped clusters, in pixels. Unlike `size`, it is **not** multiplied by the display scale. |
| `ligatures` | bool | `false` | `true`, `false` | Enables the font's `liga` and `calt` features. Off, both are explicitly disabled so a cell never depends on a glyph substitution. |
| `integer_cell_width` | bool | `false` | `true`, `false` | Makes the cell advance a whole number of pixels by rasterising the font at the slightly larger size whose advance rounds to that width, so the grid and the shaper agree. On, block glyphs (`█ ▓ ▒ ░`) no longer antialias their edge against the pixel grid, which removes the dark seam between the steps of a gradient drawn with them. Off, the measured advance is used as is. Applies live. |
| `fallbacks` | array of strings | `["JetBrains Mono", "Symbols Nerd Font", "Noto Color Emoji", "DejaVu Sans Mono", "monospace"]` | any strings | **Accepted but not read.** Missing glyphs are resolved by cosmic-text through the system font database; this list is never passed to the shaper. |

### `[appearance]`

Colours, chrome and cursor.

| Key | Type | Default | Accepted values | Effect |
|---|---|---|---|---|
| `theme` | string | `"midnight"` | any built-in or user theme name, matched case-insensitively; a name that resolves to nothing warns and falls back to `midnight` | Active palette. Built-ins are `midnight` (dark) and `dawn` (light); user themes come from `~/.config/astraterm/themes/*.toml`. Built-ins resolve first, so a user theme reusing the name `midnight` or `dawn` is listed in the panel but the built-in palette is the one painted. Applies live. |
| `background_blur` | bool | `false` | `true`, `false` | **Accepted but not applied.** Blur is a compositor rule and no request is sent. Validation warns while `[window] opacity >= 1.0`, where blur could not be visible anyway. |
| `padding_x` | integer | `12` | 0–512; above 512 is an error, above 64 warns | Horizontal inner padding of every pane, in logical pixels (scaled by the display scale). Both padding keys report under the field name `appearance.padding_x`. Applies live. |
| `padding_y` | integer | `10` | 0–512; above 512 is an error, above 64 warns | Vertical inner padding of every pane, same rules as `padding_x`. Applies live. |
| `cursor` | string | `"block"` | `"block"`, `"beam"`, `"underline"`, `"hollow"` (lowercase exactly; anything else is a parse error) | Shape painted at the cursor of the focused pane; it overrides a shape the running program requests (`DECSCUSR`). `hollow` is an outlined block. Applies live. |
| `cursor_blink` | bool | `true` | `true`, `false` | Toggles the cursor every 530 ms. Disabled while `[performance] low_power_mode` is on and while a panel or overlay is open. Applies live. |
| `style` | string | `"minimal-dark"` | any string | **Accepted but not applied.** Stored only, for forward compatibility; no code reads it. |
| `show_status_bar` | bool | `false` | `true`, `false`; warns when it is on and all three `status_bar_*` keys are `false` | Shows the bottom bar: title, directory, grid size, `ALT`/`SEL`, search state and transient messages on the left; the enabled segments on the right. `toggle-status-bar` flips it and persists the result. Applies live. |
| `status_bar_cpu` | bool | `true` | `true`, `false` | Adds `CPU nn%` from `/proc/stat`. Sampled once per second and only while the bar is visible. Applies live. |
| `status_bar_ram` | bool | `true` | `true`, `false` | Adds `RAM nn%` from `/proc/meminfo`, with the same sampling. Applies live. |
| `status_bar_clock` | bool | `true` | `true`, `false` | Adds the clock segment (see `clock_format`). Applies live. |
| `clock_format` | string | `"%H:%M"` | non-empty and at most 64 characters (otherwise an error) | Format string for the clock segment, evaluated by the C library `strftime` in your timezone and locale: the standard specifiers (`%H`, `%M`, `%S`, `%d`, `%m`, `%Y`, `%b`, `%a`, `%I`, `%p`, …) work and other text is literal. At runtime an empty, NUL-containing or longer-than-63-byte format falls back to `%H:%M`. Applies live. |
| `show_window_controls` | bool | `false` | `true`, `false` | **Accepted but not applied.** `[window] show_window_controls` is the key the window reads; this one only exists in the model. |
| `accent_color` | string | `"#8B9CFF"` | `#RGB` or `#RRGGBB` (3 or 6 hex digits, `#` optional); unparsable is an error, relative luminance below 0.05 warns | **Validated but not applied.** The renderer paints the theme's `accent`; change the accent in the theme editor instead. |
| `tab_bar_position` | string | `"top"` | `"top"`, `"right"`, `"bottom"`, `"hidden"` (lowercase exactly; anything else is a parse error) | Where the tab strip is drawn. `right` is a vertical Warp-style strip whose width is `tab_strip_width`. Applies live. |
| `tab_strip_width` | integer | `180` | 120–480; outside is an error | Width of the vertical tab strip in logical pixels (scaled), used only when `tab_bar_position = "right"`. Also clamped to the window width minus 160 device pixels so the terminal keeps a usable area. Applies live. |
| `compact_tabs` | bool | `false` | `true`, `false` | Smaller tab labels with less padding and no profile hint. Applies live. |
| `divider_thickness` | integer | `1` | 1–12; outside is an error | Thickness of the line between two splits, in **device** pixels (not scaled by the display scale factor). Applied as `max(1)`. Applies live. |
| `pane_gap` | integer | `2` | 0–24; above 24 is an error | Gap reserved around each pane, in **device** pixels (not scaled). Applies live. |

### `[workspace]`

What happens on start and when a shell exits.

| Key | Type | Default | Accepted values | Effect |
|---|---|---|---|---|
| `restore_on_startup` | bool | `true` | `true`, `false` | Recreates the tabs, split tree, per-pane working directory and profile from `workspace.toml`; every pane starts a fresh shell (processes are never restored). A missing or invalid file, or `false`, starts with one fresh tab. Read when a window is created. |
| `close_pane_when_shell_exits` | bool | `true` | `true`, `false` | With `true`, a pane whose child exits is removed and its tab closes when it was the tab's only pane — except the very last pane of the window, which stays on screen with its final output. With `false` every pane keeps its final screen until you close it. Read when the child exits, so it applies live. |

### `[performance]`

| Key | Type | Default | Accepted values | Effect |
|---|---|---|---|---|
| `gpu_acceleration` | bool | `true` | `true`, `false` | **Accepted but not applied.** The GPU renderer is always attempted; a failure is reported as a startup error instead of silently degrading. |
| `low_power_mode` | bool | `false` | `true`, `false` | Requests the low-power GPU adapter (`wgpu` `PowerPreference::LowPower`) for a new window and disables cursor blinking. The adapter choice is fixed when the window is created; the blink part applies live. |
| `max_fps_when_idle` | integer | `1` | 0–240; above 240 is an error | **Accepted but not applied.** The schema documents `0` as "unlimited on demand", but there is no idle redraw loop, so no code reads it. |

### `[notifications]`

Programs ask for a notification with `OSC 9;<body>` or
`OSC 777;notify;<title>;<body>` (the sequences used by `notify-send` wrappers,
tmux, `llvm` builds and most "tell me when this finishes" scripts). AstraTerm
consumes both before the VT parser, shows the text in the status bar, and — under
the policy below — files a desktop notification through `notify-send`.

| Key | Type | Default | Accepted values | Effect |
|---|---|---|---|---|
| `on` | string | `"unfocused"` | `"unfocused"`, `"always"`, `"never"` (lowercase exactly; anything else is a parse error) | When a desktop notification is filed, for `OSC 9`/`OSC 777` and for AstraTerm's own save/reload notices. `unfocused` files one only while the window showing that session does not have focus; `always` files it even while you watch; `never` only shows the status-bar message. It also gates the window attention hint. Read per notification, so it applies live. |

The policy also gates the window attention hint (the compositor's "urgent" mark):
with `never` nothing is raised, not even the hint.

AstraTerm's own save and reload notices use the same policy. A successful panel
save reports `settings saved` (or `theme saved` from **Save theme as…**), and a
file reloaded from disk reports `config reloaded`; while the settings panel is
open the notice is painted above the dimmed status bar, which the panel would
otherwise cover. A failed save reports the real error (`could not save settings:
…`) instead of a generic message. When the window does not have focus the notice
is also filed as a desktop notification, and an identical notice from one burst
of file events is shown and notified once.

The text is untrusted terminal output. Control characters are removed, whitespace
is collapsed, the title is cut to 120 characters and the body to 1024, and the
result is passed to `notify-send` as an argument vector — never through a shell.
A missing `notify-send` (or no notification daemon) degrades silently: the status
bar still reports the message. See `docs/SECURITY.md`.

```toml
[notifications]
# "unfocused" (default), "always" or "never".
on = "unfocused"
```

### `[keys]`

A table that maps an **action name** to one or more chords.

| Key | Type | Default | Accepted values | Effect |
|---|---|---|---|---|
| any action name from the list below | string | the chord in the list below | one chord, or several separated by `\|`; `""` or `"none"` unbinds the action | Replaces the chord(s) bound to that action. An unknown action name, an unknown modifier, an unknown key, or a value that contains no chord at all is an error; two actions claiming the same chord is a warning, and the alphabetically first action keeps it. The keymap is rebuilt on every reload and panel save, so this applies live. |

The value must be a string — an array of chords is a parse error. Action names are
case-sensitive, and a `[keys]` table is **merged** with the defaults: every action
you do not mention keeps its default chord, so `[keys]` holds only your changes.
`""` and `"none"` (case-insensitive) are the exception: they remove the binding
instead of adding one.

Chord syntax: modifiers `ctrl` (`control`), `alt` (`option`, `opt`), `shift` and
`super` (`meta`, `cmd`, `logo`, `win`) in any order, then the key. A key is one
character (`t`, `1`, `=`, `,`, `+`), a named key — `escape`/`esc`,
`enter`/`return`, `tab`, `backspace`/`bs`, `delete`/`del`, `insert`/`ins`,
`home`, `end`, `pageup`/`pgup`/`prior`, `pagedown`/`pgdn`/`next`,
`up`/`arrowup`, `down`/`arrowdown`, `left`/`arrowleft`, `right`/`arrowright`,
`f1`–`f24` — or one of the spelled-out punctuation keys `space`, `comma`,
`period`/`dot`, `minus`, `plus`, `equal`, `slash`, `backslash`, `semicolon`,
`apostrophe`/`quote`, `bracketleft`, `bracketright`, `backtick`/`grave`. A
trailing `+` means the plus key itself (`ctrl+` = `ctrl+plus`). `|` separates
alternative chords for one action:

```toml
[keys]
increase-font-size = "ctrl+=|ctrl+plus"   # either chord triggers the action
rename-tab = "none"                       # an empty value or "none" unbinds it
```

Every accepted action name with its default chord (48 in total):

| Action | Default chord | Action | Default chord |
|---|---|---|---|
| `new-tab` | `ctrl+shift+t` | `equalize-splits` | `ctrl+shift+0` |
| `close-tab` | `ctrl+shift+alt+w` | `toggle-zoom` | `ctrl+shift+z` |
| `rename-tab` | `ctrl+shift+r` | `increase-font-size` | `ctrl+=\|ctrl+plus` |
| `next-tab` | `ctrl+pagedown` | `decrease-font-size` | `ctrl+minus` |
| `previous-tab` | `ctrl+pageup` | `reset-font-size` | `ctrl+0` |
| `move-tab-left` | `ctrl+shift+alt+left` | `search` | `ctrl+shift+f` |
| `move-tab-right` | `ctrl+shift+alt+right` | `search-next` | `ctrl+g` |
| `tab-1` | `alt+1` | `search-previous` | `ctrl+shift+g` |
| `tab-2` | `alt+2` | `copy` | `ctrl+shift+c` |
| `tab-3` | `alt+3` | `paste` | `ctrl+shift+v` |
| `tab-4` | `alt+4` | `scroll-page-up` | `shift+pageup` |
| `tab-5` | `alt+5` | `scroll-page-down` | `shift+pagedown` |
| `tab-6` | `alt+6` | `scroll-to-top` | `ctrl+shift+home` |
| `tab-7` | `alt+7` | `scroll-to-bottom` | `ctrl+shift+end` |
| `tab-8` | `alt+8` | `open-settings` | `ctrl+comma` |
| `tab-9` | `alt+9` | `reload-config` | `ctrl+shift+comma` |
| `new-split-horizontal` | `ctrl+shift+d` | `toggle-status-bar` | `ctrl+shift+s` |
| `new-split-vertical` | `ctrl+shift+e` | `new-window` | `ctrl+shift+n` |
| `close-pane` | `ctrl+shift+w` | `toggle-floating` | `ctrl+shift+grave` |
| `focus-left` | `alt+left` | `send-literal-next` | `ctrl+shift+enter` |
| `focus-right` | `alt+right` | `quit` | `ctrl+shift+q` |
| `focus-up` | `alt+up` | | |
| `focus-down` | `alt+down` | | |
| `resize-left` | `ctrl+alt+left` | | |
| `resize-right` | `ctrl+alt+right` | | |
| `resize-up` | `ctrl+alt+up` | | |
| `resize-down` | `ctrl+alt+down` | | |

What each action does is in [Keyboard actions](#keyboard-actions).

### `[[profiles]]`

An array of tables: one `[[profiles]]` block per named launch preset. A profile
is used when it is named with `--profile <name>` or when a restored pane was
created from it; there is no implicit default profile.

| Key | Type | Default | Accepted values | Effect |
|---|---|---|---|---|
| `name` | string | `""` — which validation rejects, so it is effectively required | non-blank and unique (compared case-insensitively) | Identifier used by `--profile <name>`, by the panel and in the saved workspace. |
| `shell` | string | `""` | `""`, `"auto"`, a program name or an absolute path | Shell override for this profile's sessions. Empty or `auto` uses `[terminal] shell`, and `args` below then applies to that shell instead. |
| `args` | array of strings | `[]` | any strings | Arguments for the profile's shell. With an empty `shell` they are passed to the global shell (replacing `terminal.shell_args`). Ignored when `command` is set, which warns. |
| `command` | array of strings | `[]` | a non-empty array is an argument vector: first element the program, the rest its arguments | Runs that vector directly instead of a shell; no shell interprets it. `sh -c` patterns warn. |
| `working_directory` | string | unset | same values as `[terminal] working_directory` | Directory for this profile's sessions; unset falls back to `[terminal] working_directory`. An explicit path here also outranks the directory a pane inherits from its parent. |
| `env` | table of strings | `{}` | any string keys and values | Extra environment variables for this profile's sessions, merged after the inherited environment and after the variables AstraTerm sets (`TERM`, `COLORTERM`, `ASTRATERM`, `ASTRATERM_VERSION`), so a profile can override `TERM`. |
| `startup_command` | string | unset | any string | Text typed into the PTY as keystrokes once the shell has started; never executed through `sh -c`. Useful for `nvim .`, `lazygit` or `ssh host`. |
| `theme` | string | unset | any string | **Accepted but not applied.** Validated only for being a string; no code reads it. |
| `font_size` | float | unset | 4–96; outside is an error | **Accepted but not applied.** Validated, then ignored. |
| `label` | string | unset | any string | Tab title shown until the program sets a title, for example the SSH host alias. |

## Keyboard actions

Every action is configurable in `[keys]`. Defaults:

| Action | Default chord | What it does |
|---|---|---|
| `new-tab` | `ctrl+shift+t` | Opens a tab with a new shell |
| `close-tab` | `ctrl+shift+alt+w` | Closes the whole tab, asking first when a child is running |
| `rename-tab` | `ctrl+shift+r` | Renames the active tab (empty title returns to the derived one) |
| `next-tab` | `ctrl+pagedown` | Goes to the next tab, wrapping around |
| `previous-tab` | `ctrl+pageup` | Goes to the previous tab, wrapping around |
| `move-tab-left` | `ctrl+shift+alt+left` | Moves the active tab one position left |
| `move-tab-right` | `ctrl+shift+alt+right` | Moves the active tab one position right |
| `tab-1 … tab-9` | `alt+1 … alt+9` | Jumps to that tab index |
| `new-split-horizontal` | `ctrl+shift+d` | Splits the focused pane into stacked panes |
| `new-split-vertical` | `ctrl+shift+e` | Splits the focused pane into side-by-side panes |
| `close-pane` | `ctrl+shift+w` | Closes the focused pane; the last pane closes its tab |
| `focus-left/right/up/down` | `alt+left/right/up/down` | Moves focus to the pane in that direction |
| `resize-left/right/up/down` | `ctrl+alt+left/right/up/down` | Moves the divider of the focused pane by 2% |
| `equalize-splits` | `ctrl+shift+0` | Resets every divider in the tab to 50% |
| `toggle-zoom` | `ctrl+shift+z` | Temporarily maximises the focused pane |
| `increase-font-size` | `ctrl+= or ctrl+plus` | Grows the font by 0.5 pt, live; the level survives a restart |
| `decrease-font-size` | `ctrl+minus` | Shrinks the font by 0.5 pt, live; the level survives a restart |
| `reset-font-size` | `ctrl+0` | Returns to `font.size` and forgets the zoom level |
| `search` | `ctrl+shift+f` | Opens scrollback search for the focused pane |
| `search-next` | `ctrl+g` | Jumps to the next match |
| `search-previous` | `ctrl+shift+g` | Jumps to the previous match |
| `copy` | `ctrl+shift+c` | Copies the current selection to the clipboard |
| `paste` | `ctrl+shift+v` | Pastes, honouring bracketed paste; refuses payloads over 8 MiB |
| `scroll-page-up` | `shift+pageup` | Scrolls the viewport one page into the scrollback |
| `scroll-page-down` | `shift+pagedown` | Scrolls one page back towards the prompt |
| `scroll-to-top` | `ctrl+shift+home` | Scrolls to the oldest line |
| `scroll-to-bottom` | `ctrl+shift+end` | Returns to the live prompt |
| `open-settings` | `ctrl+comma` | Opens the settings panel |
| `reload-config` | `ctrl+shift+comma` | Re-reads the configuration file |
| `toggle-status-bar` | `ctrl+shift+s` | Shows/hides the status bar and persists it |
| `new-window` | `ctrl+shift+n` | Opens another window in the same process |
| `toggle-floating` | `ctrl+shift+grave` | Creates, hides or shows the window's floating terminal; hiding keeps its shell running |
| `send-literal-next` | `ctrl+shift+enter` | Sends the next key to the application instead of the keymap |
| `quit` | `ctrl+shift+q` | Quits, asking first when sessions are running |

A chord that is not in the table goes straight to the application (the shell). When
an application needs a chord that *is* configured, or an action shadows a shortcut
you want in the shell, either change the binding or press `send-literal-next`
(`Ctrl+Shift+Enter`) followed by the key: that key bypasses the keymap once.

Chord syntax: modifiers `ctrl`, `alt`, `shift`, `super` (`meta`, `cmd`, `logo`,
`win` are synonyms), then the key. `comma`, `period`, `minus`, `plus`, `space`,
`equal`, `slash`, `backslash`, `semicolon`, `apostrophe`, `bracketleft`,
`bracketright` and `backtick` name punctuation; `arrowleft`/`left`, `pageup`/`pgup`
and friends are synonyms. Several chords share one action with `|`. The complete
grammar, including every alias, is in [`[keys]`](#keys):

```toml
[keys]
increase-font-size = "ctrl+=|ctrl+plus"
rename-tab = "none"            # an empty value or `none` unbinds the action
```

## Validation

`astraterm_config::validation::validate` is pure: it takes a `Config` plus the
known theme names and returns `ValidationReport { issues }`. Severity is
`Warning` or `Error`; `is_valid()` means "no error". The same rules run for TOML
input and for panel edits. Errors block persistence; warnings do not. A file with
errors is still loaded and used — the errors are logged and the panel refuses to
save until they are fixed.

| Field | Error | Warning |
|---|---|---|
| `window.width` / `window.height` | outside 200–16384 / 120–16384 | — |
| `window.opacity` | outside 0.1–1.0 | < 0.5 |
| `terminal.term` | empty, whitespace | not in the allowlist |
| `terminal.scrollback_lines` | outside 100–1,000,000 | — |
| `terminal.shell` | empty; a value containing `/` that does not exist | — |
| `terminal.working_directory` | unknown policy; relative path that is not a policy | an absolute path that is not an existing directory (reported as the field `working_directory`) |
| `font.size` / `line_height` / `letter_spacing` | outside 4–96 / 0.8–3.0 / −5–20 | — |
| `font.weight` | not 100–900 in steps of 100 | — |
| `font.family` | empty | — |
| `appearance.padding_x` / `padding_y` | either > 512 | either > 64 |
| `appearance.divider_thickness` / `pane_gap` | outside 1–12 / > 24 | — |
| `appearance.tab_strip_width` | outside 120–480 | — |
| `appearance.clock_format` | empty or > 64 characters | — |
| `appearance.show_status_bar` | — | visible with all three segments disabled |
| `appearance.accent_color` | cannot parse | relative luminance < 0.05 |
| `appearance.theme` | — | name not found (falls back to `midnight`) |
| `appearance.background_blur` | — | enabled while `opacity >= 1.0` |
| `performance.max_fps_when_idle` | > 240 | — |
| `keys` | unknown action, bad chord/modifier | duplicate chord |
| `profiles` | empty/duplicate name, bad `font_size`, bad `working_directory` | `command` and `args` both set; `sh -c` pattern |

Unknown TOML keys are not in this list: the loader reports each one as a warning
with the dotted path and the text "unknown setting; it has no effect".

## Save policy

`ConfigService` (`crates/config/src/service.rs`) is the only writer.

- **Panel save** calls `ConfigService::replace(config)`:
  1. validate; on any error return `SaveError::Invalid(errors)` — neither memory
     nor disk changes;
  2. compare the file hash with the hash recorded at load/save time; a difference
     returns `SaveError::ExternalChange` (someone else changed the file);
  3. serialize the **canonical** form and write it atomically;
  4. on any persist error, restore the previous in-memory configuration.
- **Canonical form** means the file is regenerated from the typed model, so
  hand-written comments are not preserved across a panel save.
- **Atomic write**: temporary file in the same directory, `sync_all`, `rename`
  over the target, then a best-effort directory sync. A crash leaves either the
  old or the new file, never a truncated one.
- **External edits** are detected by a watcher (debounced 250 ms) and reloaded
  through `reload_if_changed`: unchanged content does nothing, a valid file is
  applied, an invalid one keeps the last known-good configuration and reports the
  parse error, and a deleted file resets to defaults.
- **No silent overwrite**: the panel refuses to save when the file changed since it
  was last loaded and tells you to reload first.
- **Feedback**: every save and reload reports its result in the status bar (a
  failure names the real error), and with the window unfocused the same notice
  becomes a desktop notification under `notifications.on`. An identical notice
  repeated by a burst of file events is shown once.

## What applies live

`Application::apply_config` runs after a reload or a successful panel save: it
rebuilds the keymap, resolves the theme, pushes the configuration into every
window (font metrics, layout, palette, text-cache invalidation), resizes PTY
grids and refreshes titles.

Live without a restart: theme and palette, cursor shape/blink, padding, pane gap,
divider thickness, tab bar position, strip width, compact tabs, status bar and its
segments, clock format, font family/size/weight/line height/letter spacing,
opacity, `terminal.images`, all keybindings, `confirm_on_close`, `copy_on_select`,
`open_urls_on_click`, `close_pane_when_shell_exits`, `notifications.on`.

Requires a new session or window (`Field::requires_restart()`):
`terminal.term`, `terminal.scrollback_lines`, `terminal.working_directory`,
`terminal.shell`, `window.width`, `window.height`, `window.startup_maximized`,
`performance.gpu_acceleration`. `terminal.shell_args` and every `[[profiles]]`
field apply when a session is created, and `workspace.restore_on_startup` is read
when the next window appears. `performance.low_power_mode` picks the GPU adapter
when a window is created and stops the cursor blink live.

## Complete example

Valid against this document, setting at least one key from every table. Every key
has a one-line comment; keys that are absent keep their defaults.

```toml
[window]
width = 1440                # initial width, logical pixels (200-16384)
height = 900                # initial height, logical pixels (120-16384)
opacity = 0.92              # 0.1-1.0; only the background becomes translucent
startup_maximized = false   # ask the compositor to open maximized
show_window_controls = true # let the compositor draw the decorations
remember_geometry = true    # accepted; restore does not re-apply the size

[terminal]
shell = "auto"              # "auto" = $SHELL when it exists, else /bin/sh
shell_args = ["-l"]         # login shell; never interpreted by a shell
term = "xterm-256color"     # $TERM; values outside the allowlist warn
kitty_keyboard = true       # advertise the Kitty keyboard protocol
images = false              # decode images from terminal output (opt-in)
scrollback_lines = 20000    # history lines per session (100-1000000)
working_directory = "home"  # "home", "current"/"inherit" or an absolute path
confirm_on_close = true     # ask before closing a pane with a live child
copy_on_select = false      # copy as soon as a mouse selection ends
open_urls_on_click = true   # Ctrl+click on a URL opens it with xdg-open
word_delimiters = " /\\()\"'-.,:;<>~!@#$%^&*|+=[]{}~?"  # accepted; not applied

[font]
family = "JetBrainsMono Nerd Font"  # primary font family
size = 13.5                 # logical pixels, not points (4-96)
weight = 400                # 100-900 in steps of 100
line_height = 1.2           # row height as a multiple of size (0.8-3.0)
letter_spacing = 0.0        # extra tracking in pixels (-5 to 20)
ligatures = false           # enable the font's liga/calt features
integer_cell_width = false  # stretch the font so a cell is a whole pixel wide
fallbacks = ["JetBrains Mono", "Noto Color Emoji", "monospace"]  # accepted; not read

[appearance]
theme = "midnight"          # built-in or user theme, case-insensitive
background_blur = false     # accepted; blur is a compositor rule
padding_x = 12              # horizontal pane padding, logical pixels (max 512)
padding_y = 10              # vertical pane padding, logical pixels (max 512)
cursor = "block"            # block | beam | underline | hollow
cursor_blink = true         # 530 ms toggle; off in low-power mode
style = "minimal-dark"      # accepted; stored only
show_status_bar = true      # bottom bar with session info and readings
status_bar_cpu = true       # CPU segment from /proc/stat
status_bar_ram = true       # RAM segment from /proc/meminfo
status_bar_clock = true     # clock segment
clock_format = "%H:%M"      # strftime format, at most 64 characters
show_window_controls = false # accepted; [window] is the applied key
accent_color = "#8B9CFF"    # validated; the theme's accent is what paints
tab_bar_position = "top"    # top | right | bottom | hidden
tab_strip_width = 200       # right strip width, logical pixels (120-480)
compact_tabs = false        # smaller tab labels with less padding
divider_thickness = 1       # split divider, device pixels (1-12)
pane_gap = 2                # gap around each pane, device pixels (0-24)

[workspace]
restore_on_startup = true           # rebuild tabs/splits from workspace.toml
close_pane_when_shell_exits = true  # close a pane when its child exits

[performance]
gpu_acceleration = true     # accepted; the GPU renderer is always attempted
low_power_mode = false      # low-power adapter plus no cursor blink
max_fps_when_idle = 1       # accepted; there is no idle redraw loop

[notifications]
on = "unfocused"            # unfocused | always | never

[keys]
# Only the bindings that differ from the defaults; every other action keeps its
# default chord because the table is merged with the built-in defaults.
close-tab = "ctrl+shift+alt+w"                      # chord for one action
rename-tab = "ctrl+shift+r"                         # another chord
decrease-font-size = "ctrl+minus|ctrl+shift+minus"  # `|` separates alternatives
toggle-floating = "none"                            # empty or "none" unbinds

[[profiles]]
name = "work"                    # identifier used by --profile work
shell = "/bin/zsh"               # overrides [terminal] shell
args = ["-l"]                    # arguments for that shell
working_directory = "home"       # same values as terminal.working_directory
env = { EDITOR = "nvim" }        # extra environment for this profile
startup_command = "nvim ."       # typed into the shell after it starts
label = "work"                   # tab title until the program sets one

[[profiles]]
name = "prod"                    # a second profile
command = ["ssh", "production"]  # argv run directly, no shell involved
label = "prod"                   # tab title
theme = "dawn"                   # accepted; not applied
font_size = 14.0                 # accepted; not applied
```

## Gotchas

### Accepted but not applied

These keys parse, load and validate (mostly), and change nothing:

| Key | What happens instead |
|---|---|
| `window.remember_geometry` | no code reads it; the size saved in `workspace.toml` is ignored on restore |
| `terminal.word_delimiters` | double-click selection uses the terminal core's rules |
| `font.fallbacks` | cosmic-text resolves missing glyphs through the system font database |
| `appearance.background_blur` | documentation only; no compositor request is sent |
| `appearance.style` | stored only |
| `appearance.show_window_controls` | `[window] show_window_controls` is the one the window reads |
| `appearance.accent_color` | validated; the theme's `accent` is what paints |
| `performance.gpu_acceleration` | the GPU renderer is always attempted; a failure is an error, not a fallback |
| `performance.max_fps_when_idle` | there is no idle redraw loop to cap |
| `profiles[].theme`, `profiles[].font_size` | validated (`font_size` 4–96), then ignored |

### Values that are clamped

- `[window] opacity` is clamped to 0.1–1.0 when the frame is painted, so an
  out-of-range value paints the nearest valid one (and is a validation error).
- `[appearance] tab_strip_width` is clamped to the window width minus 160 device
  pixels while the tabs are on the right, so a 480-pixel strip cannot squeeze the
  terminal to nothing in a small window.
- `[appearance] divider_thickness` is applied as `max(1)`, so `0` paints a 1 px
  divider (and is a validation error).
- `[font] size` set by the zoom keys is clamped to 4–96 by the runtime preference
  and, while set, overrides `font.size` until `reset-font-size`; a non-finite
  stored value is discarded.
- Text padding is scaled and then subtracted from the pane rectangle
  saturating, so a 512-pixel padding on a small window leaves zero columns rather
  than a panic.

### Defaults that depend on other settings

- `[keys]` is merged with the built-in defaults: a table that renames one action
  keeps every other default chord, so you never copy the whole list. `""` and
  `"none"` are the exception — they remove a binding.
- `terminal.working_directory = "current"` / `"inherit"` resolve against the pane
  the session is created from, so the effective directory depends on where you
  are when the tab or split opens. `--working-directory` supplies that directory
  for windows that start a fresh workspace; with the default `home` policy it is
  not used.
- A profile's `shell` and `args` interact: an empty or `auto` `shell` means
  `[terminal] shell`, and the profile's `args` then replace `terminal.shell_args`.
  Setting `command` ignores both and warns if `args` is also set.
- `appearance.tab_strip_width` only means anything when
  `tab_bar_position = "right"`.
- `appearance.status_bar_cpu`, `status_bar_ram` and `status_bar_clock` only mean
  anything while `show_status_bar = true`.
- `appearance.background_blur` warns based on `window.opacity`, because blur
  cannot show through an opaque window.
- An unknown `appearance.theme` falls back to `midnight`, whatever name you wrote.
- `performance.low_power_mode` disables the cursor blink regardless of
  `appearance.cursor_blink`.
- `notifications.on = "never"` also suppresses the window attention hint.

### Pixels, not points

`[font] size` is in **logical pixels**, not points: the renderer multiplies it by
the display scale factor and hands pixels to cosmic-text. Most other terminals
write font sizes in points (1 pt = 1/72 inch ≈ 1.333 px at 96 dpi), so a config
that says `size = 11` in a point-based terminal is roughly `size = 14.7` here.
On a 2× HiDPI display `size = 13` becomes 26 physical pixels.

The units are not uniform across the file:

- `width`, `height`, `padding_x`, `padding_y`, `tab_strip_width` and `font.size`
  are logical pixels and are multiplied by the display scale.
- `divider_thickness` and `pane_gap` are **device** pixels and are used as
  written, so they look thinner on a HiDPI display.
- `letter_spacing` is in pixels and is not scaled either.
- `font.line_height` is a multiplier, not a length.

Copying a config from another terminal is the most common reason a font or a
padding value "looks wrong".

### Whole-pixel cells

`font.integer_cell_width = true` stretches the font so the cell advance is a
whole pixel. Rounding the grid width alone would not do: the grid would step 11
pixels per column while the shaper still advanced 10.8, and every row would drift
away from the grid again. Instead the font is measured, scaled by
`round(advance) / advance`, and measured again, and the shaping metrics come from
that second measurement.

The visible cost is the size. With the shipped default config the advance rises
from 7.8 px to 8 px, which is the same font at about 13.33 instead of 13, so text
is very slightly larger and slightly fewer columns fit: a 2490 px pane goes from
319 columns to 311. The row height is rounded, so it usually does not change.

## See also

- [docs/ARCHITECTURE.md](ARCHITECTURE.md) — how the layers fit and where each
  option is consumed.
- [docs/TERMINAL_COMPATIBILITY.md](TERMINAL_COMPATIBILITY.md) — key/mouse encoding,
  `$TERM` policy and the Neovim checklist.
- [docs/PERFORMANCE.md](PERFORMANCE.md) — timers, sampling cadence and measured
  idle numbers.
