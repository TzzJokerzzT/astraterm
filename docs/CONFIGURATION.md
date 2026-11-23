# Configuration

AstraTerm reads one configuration file and exposes every option twice: in TOML
(the portable source of truth) and in the native settings panel (`Ctrl+,`), which
edits the same typed model and writes the TOML back. This page documents every
option: what it does, what it is for, and where to change it.

Sections below:

- [Files and paths](#files-and-paths) — where files live, and which file wins.
- [Recipes](#recipes) — opacity, blur, status bar, right-hand tabs, themes.
- [Option reference](#option-reference) — every option, with description and purpose.
- [Keyboard actions](#keys) — the whole key map.
- [Profiles](#profiles) — named launch presets.
- [Validation](#validation) · [Save policy](#save-policy) · [What applies live](#what-applies-live)
- [Example configuration](#example-configuration) — complete and valid.
- [Options accepted but not applied](#options-accepted-but-not-applied)

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
4. Command-line flags for the first window: `--working-directory`, `--profile`,
   `--command`, `--title` and `--config`.

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
`clock_format` is strftime-like (`%H %M %S %d %m %Y %b %a`; anything else is
literal) and uses your timezone and locale.

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

Types are the Rust types in `crates/config/src/schema.rs`; TOML maps them to
bool, integer, float, string, array and table. "Panel" names the settings row that
edits the option, when there is one.

### `[window]` — the application window

| Option | Default | What it does | When you would change it | Panel |
|---|---|---|---|---|
| `width` | `1280` | Initial window width in logical pixels (200–16384). | Your monitor is wider than the default, or you want a compact window. Read when a window is created. | Window width |
| `height` | `800` | Initial window height in logical pixels (120–16384). | Same as `width`. Read when a window is created. | Window height |
| `opacity` | `1.0` | Alpha of the painted background (0.1–1.0), applied live. Text stays opaque. | You want the wallpaper or the windows behind to show through. Pair it with compositor blur. | Window opacity |
| `startup_maximized` | `false` | Asks the compositor to open the window maximized. | You always work maximized. Read when a window is created. | Start maximized |
| `show_window_controls` | `true` | Lets the compositor draw client-side decorations (title bar, close button). | You prefer server-side decorations or a bare window, or your compositor draws its own. Read when a window is created. | — |
| `remember_geometry` | `true` | Accepted but not applied today: the last size is stored in `workspace.toml`, and restore recreates tabs/splits without re-applying geometry. | You expected the window to reopen at its old size. | Remember window geometry |

### `[terminal]` — shell, emulation and session behaviour

| Option | Default | What it does | When you would change it | Panel |
|---|---|---|---|---|
| `shell` | `"auto"` | Program to run. `auto` resolves `$SHELL` and falls back to `/bin/sh`; an absolute path must exist. | You want zsh/fish/nu regardless of `$SHELL`, or a specific binary. Needs a new session. | Shell |
| `shell_args` | `[]` | Argument vector passed to the shell (for example `["-l"]` for a login shell). Never interpreted by a shell. | You want a login shell, a custom rc file, or a `--norc` start. Needs a new session. | — |
| `term` | `"xterm-256color"` | `$TERM` advertised to applications. Values outside the widely-available list warn, because remote hosts may lack the terminfo entry. | Only when you know the target has the entry installed (it breaks `nvim`, `less` and remote shells otherwise). Needs a new session. | `$TERM` |
| `images` | `false` | Decodes and draws images sent by the running program (kitty graphics, iTerm2 `OSC 1337`, sixel), anchored to the grid. Off, the sequences are captured and discarded. | You preview images inline (`yazi`, `chafa`, notebooks) and accept that the terminal parses attacker-controlled image data. Needs a new session. | — |
| `kitty_keyboard` | `true` | Advertises the Kitty keyboard protocol: `CSI ? u` is answered with the active flags and `CSI = u` (set), `CSI > u` (push) and `CSI < u` (pop) are honoured, so applications can ask for unambiguous encodings, alternate keys, associated text and key release/repeat events. Off, applications fall back to the xterm encodings. | An application misbehaves once it detects the protocol, or you want the historical xterm-only encoding everywhere. Needs a new session. | — |
| `scrollback_lines` | `10000` | Lines of history kept per session; bounded memory. | Long build logs or long agent sessions. Higher values cost memory per pane. Needs a new session. | Scrollback lines |
| `working_directory` | `"home"` | Directory a new session starts in: `home`, `current`/`inherit` (the parent pane's directory), or an absolute path. | You always start in a project directory, or want new splits to follow the current one. Needs a new session. | Working directory |
| `confirm_on_close` | `true` | Asks before closing a tab/pane with a live child, and before quitting with live sessions. | You trust yourself with `Ctrl+Shift+W` and want it instant. | Confirm before closing |
| `copy_on_select` | `false` | Copies to the clipboard as soon as a mouse selection ends. | You live in a copy/paste workflow and dislike `Ctrl+Shift+C`. | Copy on select |
| `open_urls_on_click` | `true` | Enables Ctrl+click on an `http(s)` URL to open it with `xdg-open`. A click is always required; nothing opens on its own. | You want to disable URL opening entirely (for example on shared screens). | Open URLs on click |
| `word_delimiters` | `" /\\()\"'-.,:;<>~!@#$%^&*\|+=[]{}~?"` | Accepted but not applied today: double-click word selection uses the terminal core's rules. | You expected to tune double-click selection. | Word delimiters |

### `[font]` — typography

| Option | Default | What it does | When you would change it | Panel |
|---|---|---|---|---|
| `family` | `"JetBrainsMono Nerd Font"` | Primary font family. When it is not installed, glyphs are resolved through the system fallback (the renderer reports `monospace`). | Your font of choice, or Nerd Font glyphs look wrong. Applies live. | Font family |
| `size` | `13.0` | Font size in logical points, multiplied by the display scale. | Denser output or better readability, especially on HiDPI. Applies live (`Ctrl+=`/`Ctrl+-` exist too). | Font size |
| `weight` | `400` | Font weight, 100–900 in steps of 100. | You want a heavier or lighter face (variable fonts). Applies live. | Font weight |
| `line_height` | `1.15` | Multiplier applied to the font size to get the row height. | More air between lines, or more rows on screen. Applies live. | Line height |
| `letter_spacing` | `0.0` | Extra tracking in logical pixels (−5 to 20). | Wide fonts that feel cramped, or a tighter grid. Applies live. | Letter spacing |
| `ligatures` | `false` | Enables the font's `liga` and `calt` features. Off, they are explicitly disabled so the grid never depends on a glyph substitution. | You use a font with programming ligatures (`!=`, `->`, `=>`) and want them drawn. Applies live. | — |
| `fallbacks` | see below | Accepted but not read today: cosmic-text resolves missing glyphs through the system font database. | You expected to control the fallback chain. | — |

Default `fallbacks`: `["JetBrains Mono", "Symbols Nerd Font", "Noto Color Emoji",
"DejaVu Sans Mono", "monospace"]`.

### `[appearance]` — colours, chrome and cursor

| Option | Default | What it does | When you would change it | Panel |
|---|---|---|---|---|
| `theme` | `"midnight"` | Active palette. Built-ins are `midnight` (dark) and `dawn` (light); a user theme in the themes directory wins on a name clash. An unknown name warns and falls back to `midnight`. | You want a different palette, including ones you create in the panel. Applies live. | Theme |
| `background_blur` | `false` | Documentation flag for compositor blur; no request is made. Warns while `opacity >= 1.0`, where blur could not be visible anyway. | You are noting that you enabled blur in the compositor. See [Recipes](#blur-behind-the-window). | Background blur |
| `padding_x` | `12` | Horizontal inner padding of every pane, in logical pixels (>512 error, >64 warns). | Tighter or more generous breathing room around the grid. Applies live. | Horizontal padding |
| `padding_y` | `10` | Vertical inner padding of every pane (same limits). | Same as `padding_x`. Applies live. | Vertical padding |
| `cursor` | `"block"` | Cursor shape: `block`, `beam`, `underline`, `hollow`. | Your preference, or to match the shape applications ask for. Applies live. | Cursor shape |
| `cursor_blink` | `true` | Blinks the cursor (two toggles per 530 ms). | You dislike blinking, or want to save the idle redraws. `low_power_mode` also disables it. | Cursor blinks |
| `style` | `"minimal-dark"` | Stored only, for forward compatibility. | Nothing today; kept so a future style set can read it. | — |
| `show_status_bar` | `false` | Shows the bottom bar with session info on the left and CPU/RAM/clock on the right. | You want system readings while you work. `Ctrl+Shift+S` toggles it. Applies live. | Status bar |
| `status_bar_cpu` | `true` | CPU segment of the bar, from `/proc/stat`. | Hide it on a shared machine or if you do not care about CPU. | Status bar: CPU |
| `status_bar_ram` | `true` | RAM segment, from `/proc/meminfo`. | Memory pressure matters more to you, or you want it gone. | Status bar: RAM |
| `status_bar_clock` | `true` | Clock segment, in your timezone and locale. | You already have a clock in the bar/compositor. | Status bar: clock |
| `clock_format` | `"%H:%M"` | strftime-like format for that segment (`%H %M %S %d %m %Y %b %a`; other text is literal). Non-empty, ≤64 characters. | You want seconds, a date, or 12-hour time (`%I:%M %p`). | Clock format |
| `show_window_controls` | `false` | Accepted but unused: `[window] show_window_controls` is the one that is applied. | You edited the wrong one. | — |
| `accent_color` | `"#8B9CFF"` | Validated but not applied today: the renderer uses the theme's `accent` colour. Very dark values warn. | You want the accent changed → edit it in the theme editor instead. | Accent colour |
| `tab_bar_position` | `"top"` | Where the tabs sit: `top`, `right` (vertical Warp-style strip), `bottom` or `hidden`. | Screen real estate: right-side tabs suit wide/ultrawide monitors, hidden suits `Alt+1…9` users. Applies live. | Tab bar position |
| `tab_strip_width` | `180` | Width of the vertical strip in logical pixels, 120–480. Only used when the tabs are on the right. | Titles are being truncated, or the strip is stealing too much width. | Tab strip width |
| `compact_tabs` | `false` | Smaller tab labels with less padding. | Many tabs in one window. Applies live. | Compact tabs |
| `divider_thickness` | `1` | Thickness of the line between splits, in device pixels (1–12). | You want the layout to read more clearly. Applies live. | Divider thickness |
| `pane_gap` | `2` | Gap between panes as a whole, in device pixels (≤24). | A more spacious tiling look. Applies live. | Pane gap |

### `[workspace]` — what happens on start and when a shell exits

| Option | Default | What it does | When you would change it | Panel |
|---|---|---|---|---|
| `restore_on_startup` | `true` | Recreates the tabs, split tree and each pane's working directory from `workspace.toml`. Processes are never restored: every pane starts a fresh shell. | You prefer a single clean tab on start. Read when the window is created. | Restore layout on startup |
| `close_pane_when_shell_exits` | `true` | Closing a pane when its child exits. With `false`, the pane keeps its final screen until you close it. | You want to read a crashed command's output without losing the pane. | Close pane when shell exits |

### `[performance]`

| Option | Default | What it does | When you would change it | Panel |
|---|---|---|---|---|
| `gpu_acceleration` | `true` | Accepted but not applied today: the GPU renderer is always attempted and the app reports a startup failure instead of silently degrading. | You expected a software fallback. | GPU acceleration |
| `low_power_mode` | `false` | Asks for the low-power GPU adapter on new windows and disables cursor blinking. | Battery use, or a fan that never stops. | Low power mode |
| `max_fps_when_idle` | `1` | Accepted but not applied today: there is no idle redraw loop to cap. | You expected to throttle idle redraws. | Max idle FPS |

### `[keys]`

`[keys]` maps an **action name** to one or more chords. See
[Keyboard actions](#keys-1) for every action, its default chord and what it does.
An unknown action name, an unparsable chord or an unknown modifier is an error;
two actions sharing a chord is a warning, and the alphabetically first action
keeps it.

### `[[profiles]]` — named launch presets

| Field | Default | What it does | When you would change it | Panel |
|---|---|---|---|---|
| `name` | required | Identifier used by `--profile <name>` and by the panel; must be unique and non-empty. | Always. | Profiles (TOML only) |
| `shell` | `""` | Shell override; empty or `auto` uses `terminal.shell`. | A profile with a different shell. | — |
| `args` | `[]` | Arguments for that shell, as a vector. | Login shell, custom rc file. | — |
| `command` | `[]` | Alternative to a shell: an **argument vector** executed directly (for example `["ssh", "production"]`). No shell interprets it. | SSH hosts, one-shot tools, TUIs. | — |
| `working_directory` | unset | Per-profile directory (same values as `terminal.working_directory`). | Project-specific panes. | — |
| `env` | `{}` | Extra environment variables layered over the inherited environment. | `PROJECT=…`, `SSH_AUTH_SOCK=…`, per-profile secrets-free context. | — |
| `startup_command` | unset | Text typed into the shell once it starts (not executed through `sh -c`). | `nvim .`, `lazygit`, `ssh host`. | — |
| `label` | unset | Label shown in the tab strip until the application sets a title. | Distinguish `ssh production` from `ssh staging`. | — |
| `theme` | unset | Validated but not applied today. | You expected per-profile themes. | — |
| `font_size` | unset | Validated but not applied today. | You expected per-profile font sizes. | — |

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
and friends are synonyms. Several chords share one action with `|`:

```toml
[keys]
increase-font-size = "ctrl+=|ctrl+plus"
rename-tab = "none"            # an empty value or `none` unbinds the action
```

## Validation

`astraterm_config::validation::validate` is pure: it takes a `Config` plus the
known theme names and returns `ValidationReport { issues }`. Severity is
`Warning` or `Error`; `is_valid()` means "no error". The same rules run for TOML
input and for panel edits. Errors block persistence; warnings do not.

| Field | Error | Warning |
|---|---|---|
| `window.width` / `window.height` | outside 200–16384 / 120–16384 | — |
| `window.opacity` | outside 0.1–1.0 | < 0.5 |
| `terminal.term` | empty or whitespace | not in the allowlist |
| `terminal.scrollback_lines` | outside 100–1,000,000 | — |
| `terminal.shell` | empty; absolute path missing | — |
| `terminal.working_directory` | unknown policy; relative path; missing directory | absolute path that does not exist |
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

## What applies live

`Application::apply_config` runs after a reload or a successful panel save: it
rebuilds the keymap, resolves the theme, pushes the configuration into every
window (font metrics, layout, palette, text-cache invalidation), resizes PTY
grids and refreshes titles.

Live without a restart: theme and palette, cursor shape/blink, padding, pane gap,
divider thickness, tab bar position, strip width, compact tabs, status bar and its
segments, clock format, font family/size/weight/line height/letter spacing,
opacity, all keybindings, `confirm_on_close`, `copy_on_select`,
`open_urls_on_click`, `close_pane_when_shell_exits`, `low_power_mode`.

Requires a new session or window (`Field::requires_restart()`):
`terminal.term`, `terminal.scrollback_lines`, `terminal.working_directory`,
`terminal.shell`, `window.width`, `window.height`, `window.startup_maximized`,
`performance.gpu_acceleration`. `terminal.shell_args` and every `[[profiles]]`
field apply when a session is created, and `workspace.restore_on_startup` is read
when the next window appears.

## Example configuration

Complete and valid against this document, with the full default `[keys]` table so
you can see the exact names and chords to edit.

```toml
[window]
width = 1440
height = 900
opacity = 0.92
startup_maximized = false
show_window_controls = true
remember_geometry = true

[terminal]
shell = "auto"
shell_args = []
term = "xterm-256color"
kitty_keyboard = true
scrollback_lines = 20000
working_directory = "home"
confirm_on_close = true
copy_on_select = false
open_urls_on_click = true
word_delimiters = " /\\()\"'-.,:;<>~!@#$%^&*|+=[]{}~?"

[font]
family = "JetBrainsMono Nerd Font"
size = 13.5
weight = 400
line_height = 1.2
letter_spacing = 0.0
fallbacks = [
    "JetBrains Mono",
    "Symbols Nerd Font",
    "Noto Color Emoji",
    "DejaVu Sans Mono",
    "monospace",
]

[appearance]
theme = "midnight"
background_blur = false
padding_x = 12
padding_y = 10
cursor = "block"
cursor_blink = true
style = "minimal-dark"
show_status_bar = true
status_bar_cpu = true
status_bar_ram = true
status_bar_clock = true
clock_format = "%H:%M"
show_window_controls = false
accent_color = "#8B9CFF"
tab_bar_position = "top"
tab_strip_width = 180
compact_tabs = false
divider_thickness = 1
pane_gap = 2

[workspace]
restore_on_startup = true
close_pane_when_shell_exits = true

[performance]
gpu_acceleration = true
low_power_mode = false
max_fps_when_idle = 1

[keys]
close-pane = "ctrl+shift+w"
close-tab = "ctrl+shift+alt+w"
copy = "ctrl+shift+c"
decrease-font-size = "ctrl+minus"
equalize-splits = "ctrl+shift+0"
focus-down = "alt+down"
focus-left = "alt+left"
focus-right = "alt+right"
focus-up = "alt+up"
increase-font-size = "ctrl+=|ctrl+plus"
new-split-horizontal = "ctrl+shift+d"
new-split-vertical = "ctrl+shift+e"
new-tab = "ctrl+shift+t"
new-window = "ctrl+shift+n"
next-tab = "ctrl+pagedown"
open-settings = "ctrl+comma"
paste = "ctrl+shift+v"
previous-tab = "ctrl+pageup"
quit = "ctrl+shift+q"
reload-config = "ctrl+shift+comma"
rename-tab = "ctrl+shift+r"
move-tab-left = "ctrl+shift+alt+left"
move-tab-right = "ctrl+shift+alt+right"
reset-font-size = "ctrl+0"
resize-down = "ctrl+alt+down"
resize-left = "ctrl+alt+left"
resize-right = "ctrl+alt+right"
resize-up = "ctrl+alt+up"
scroll-page-down = "shift+pagedown"
scroll-page-up = "shift+pageup"
scroll-to-bottom = "ctrl+shift+end"
scroll-to-top = "ctrl+shift+home"
search = "ctrl+shift+f"
search-next = "ctrl+g"
search-previous = "ctrl+shift+g"
send-literal-next = "ctrl+shift+enter"
tab-1 = "alt+1"
tab-2 = "alt+2"
tab-3 = "alt+3"
tab-4 = "alt+4"
tab-5 = "alt+5"
tab-6 = "alt+6"
tab-7 = "alt+7"
tab-8 = "alt+8"
tab-9 = "alt+9"
toggle-status-bar = "ctrl+shift+s"
toggle-zoom = "ctrl+shift+z"

# A login shell in the home directory, inheriting the global terminal settings.
[[profiles]]
name = "work"
shell = "/bin/zsh"
args = ["-l"]
working_directory = "home"
label = "work"

# Runs the system OpenSSH client directly. No shell interprets the vector, and no
# keys or passwords are stored here (see docs/SECURITY.md).
[[profiles]]
name = "production"
command = ["ssh", "production"]
label = "prod"

# A shell with extra environment and one command typed after startup.
[[profiles]]
name = "dev"
working_directory = "/tmp"
env = { EDITOR = "nvim", RUST_BACKTRACE = "1" }
startup_command = "nvim ."
# Accepted and validated, but not applied to sessions today:
theme = "dawn"
font_size = 14.0
```

## Options accepted but not applied

Setting these is harmless and validated, but today nothing reads them; they are
listed so nobody expects an effect:

| Field | Current status |
|---|---|
| `window.remember_geometry` | size is stored in `workspace.toml`, restore does not re-apply it |
| `terminal.word_delimiters` | double-click selection uses the terminal core's rules |
| `appearance.style` | stored only |
| `appearance.accent_color` | validated; the renderer uses the theme's `accent` |
| `appearance.show_window_controls` | `[window] show_window_controls` is the applied one |
| `appearance.background_blur` | documentation only; blur is a compositor rule |
| `font.fallbacks` | cosmic-text resolves fallbacks through the system database |
| `performance.gpu_acceleration` | the GPU renderer is always attempted |
| `performance.max_fps_when_idle` | there is no idle redraw loop to cap |
| `profiles[].theme`, `profiles[].font_size` | validated, not applied |

## See also

- [docs/ARCHITECTURE.md](ARCHITECTURE.md) — how the layers fit and where each
  option is consumed.
- [docs/TERMINAL_COMPATIBILITY.md](TERMINAL_COMPATIBILITY.md) — key/mouse encoding,
  `$TERM` policy and the Neovim checklist.
- [docs/PERFORMANCE.md](PERFORMANCE.md) — timers, sampling cadence and measured
  idle numbers.
