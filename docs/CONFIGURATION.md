# Configuration

AstraTerm has exactly one configuration schema. TOML, the atomic writer, and the
native settings panel all operate on `astraterm_config::Config`
(`crates/config/src/schema.rs`), so the file and the panel cannot drift apart.

Every field has a default. A partial file is valid; missing keys keep their
built-in default. Unknown keys are reported as warnings and have no effect.

## Files and paths

| What | Path (default) | Notes |
|---|---|---|
| Configuration | `~/.config/astraterm/config.toml` | `$XDG_CONFIG_HOME/astraterm/config.toml` when that variable is an absolute path |
| Theme directory | `~/.config/astraterm/themes/` | Derived from the config file's parent, so `--config /srv/term/config.toml` uses `/srv/term/themes/` |
| State directory | `~/.local/state/astraterm/` | `$XDG_STATE_HOME/astraterm`, used for workspace metadata |
| Workspace metadata | `<state>/workspace.toml` | Tabs, split tree, per-pane cwd/profile, window size (the `label` field exists but is written as `None`) |
| Window geometry | `<state>/window.toml` | Defined by `StatePaths::geometry_file()` but not written today |

Resolution rules (`crates/config/src/lib.rs`, `crates/platform/src/paths.rs`):
`XDG_CONFIG_HOME` / `XDG_STATE_HOME` are honoured only when absolute; otherwise
`$HOME/.config` / `$HOME/.local/state` are used; if `HOME` is unset the code
falls back to `/tmp`. `--config <PATH>` replaces the configuration file path for
that run.

On first run, and only when `--config` was not given, the application writes a
documented default file (`defaults::ensure_config_file`, header plus canonical
TOML) so there is something to edit. A missing file is never an error: the
built-in defaults apply. A malformed file is reported and the previous
(last-known-good) configuration stays active.

The packaged `/etc/astraterm/config.toml` is documentation and a copy source. It
is **not** read by the application.

## Precedence

1. **Built-in defaults** — `Config::default()` in `schema.rs` and
   `default_keybindings()` in `keys.rs`.
2. **User TOML** — the file above, or `--config`.
   - Scalars and sections replace the defaults.
   - `[keys]` is merged per action: actions you do not mention keep their
     default chord. An empty value or `none` unbinds an action.
3. **Profile overrides** — `[[profiles]]` applies when a profile is used for a
   session.

Which profile fields override what (`crates/session/src/lifecycle.rs`,
`resolve_shell` / `session_spec`):

| Profile field | Effect |
|---|---|
| `command` | Complete argument vector; first element is the program. Takes precedence over `shell`/`args` |
| `shell` | Program override (ignored when `command` is set; `"auto"` or empty means the global shell) |
| `args` | Arguments used with `shell`, or with the global shell when `shell` is empty |
| `working_directory` | Overrides `[terminal] working_directory`; falls back to home when it no longer exists |
| `env` | Layered on top of the resolved environment, so it can override `TERM`, `COLORTERM`, `ASTRATERM` |
| `startup_command` | Typed into the shell after it starts (written to the PTY with a trailing `\r`), never interpreted by AstraTerm |
| `label` | Tab-strip title before the application sets an OSC title |
| `theme`, `font_size` | Validated and stored, but not applied to sessions today |

CLI precedence for the first window: `--working-directory` is passed as the
"current" directory used by the `current`/`inherit` working-directory policies
(with the default `home` policy it has no effect); `--command <ARGV…>` replaces
the program and arguments (also for restored panes); `--title` sets the first
pane and tab title; `--new-window` skips workspace restore. `--profile <name>`
selects the profile that seeds the first window's session
(`Application::create_window` resolves it against `[[profiles]]` and logs a
warning when the name does not exist).

## Field reference

Types are the Rust types in `schema.rs`; TOML uses the obvious mapping (bool,
integer, float, string, array, table).

### `[window]`

| Field | Type | Default | Validation / notes |
|---|---|---|---|
| `width` | u32 | `1280` | 200–16384 (error). Used when a window is created |
| `height` | u32 | `800` | 120–16384 (error). Used when a window is created |
| `opacity` | f32 | `1.0` | 0.1–1.0 (error); below 0.5 warns. Values below 1.0 request a transparent window at creation; it becomes translucent only when the surface supports a premultiplied alpha mode |
| `startup_maximized` | bool | `false` | Applied at window creation |
| `show_window_controls` | bool | `true` | Passed to winit as client-side decorations at creation |
| `remember_geometry` | bool | `true` | Accepted; not consumed today (window size is still stored in `workspace.toml`) |

### `[terminal]`

| Field | Type | Default | Validation / notes |
|---|---|---|---|
| `shell` | String | `"auto"` | Non-empty (error); `auto` resolves `$SHELL`, else `/bin/sh`. A value containing `/` must exist (error) |
| `shell_args` | array of String | `[]` | Passed as an argument vector, never through a shell |
| `term` | String | `"xterm-256color"` | Non-empty, no whitespace (error). Values outside the allowlist warn (see below) |
| `scrollback_lines` | usize | `10000` | 100–1,000,000 (error). Per session; needs a new session |
| `working_directory` | string or absolute path | `"home"` | See below. Needs a new session |
| `confirm_on_close` | bool | `true` | Prompts before closing a tab/pane with a live child, and before quitting with live sessions |
| `copy_on_select` | bool | `false` | Copies to the clipboard when a mouse selection ends |
| `open_urls_on_click` | bool | `true` | Enables Ctrl+click URL opening; the click is still required |
| `word_delimiters` | String | `" /\\()\"'-.,:;<>~!@#$%^&*\|+=[]{}~?"` | Accepted; not consumed today (semantic selection uses the terminal core's rules) |

`working_directory` values:

- `home` (or an empty value) — `$HOME`.
- `current` / `inherit` — the current working directory of the new pane's parent
  session, when known.
- a known policy-like absolute path (`/…`) — used if it is a directory, otherwise
  a warning is reported; a missing directory falls back to home when the session
  starts.
- anything else that is not absolute — error ("neither a known policy nor an
  absolute path").

The `$TERM` allowlist (`validation::WIDELY_AVAILABLE_TERMS`) is:
`xterm-256color`, `xterm`, `screen-256color`, `screen`, `tmux-256color`, `linux`,
`vt100`, `alacritty`, `foot`. A custom value is allowed but warns, because remote
hosts may not have its terminfo entry.

### `[font]`

| Field | Type | Default | Validation / notes |
|---|---|---|---|
| `family` | String | `"JetBrainsMono Nerd Font"` | Non-empty (error). When the family is not installed, cosmic-text resolves glyphs through its own fallback; `TextStyle::effective_family` reports `monospace` in that case |
| `size` | f32 | `13.0` | 4–96 (error). Logical points, multiplied by the display scale |
| `weight` | u16 | `400` | Multiple of 100, 100–900 (error) |
| `line_height` | f32 | `1.15` | 0.8–3.0 (error), multiplier of the font size |
| `letter_spacing` | f32 | `0.0` | −5–20 (error), logical pixels |
| `fallbacks` | array of String | see below | Accepted; not read today. cosmic-text falls back through the system font database |

Default `fallbacks`: `["JetBrains Mono", "Symbols Nerd Font",
"Noto Color Emoji", "DejaVu Sans Mono", "monospace"]`.

Font changes (family, size, weight, line height, letter spacing) apply live:
the renderer recomputes cell metrics and drops the shaped-row cache.

### `[appearance]`

| Field | Type | Default | Validation / notes |
|---|---|---|---|
| `theme` | String | `"midnight"` | Unknown theme warns and falls back to `midnight`. Built-ins: `midnight` (dark), `dawn` (light) |
| `background_blur` | bool | `false` | Accepted; no compositor blur request is made today. Warns when `opacity >= 1.0` |
| `padding_x` | u32 | `12` | >512 error, >64 warning (reported on `appearance.padding_x` for either axis). Logical pixels |
| `padding_y` | u32 | `10` | same rule as `padding_x` |
| `cursor` | `"block"`, `"beam"`, `"underline"`, `"hollow"` | `"block"` | Any other value is a TOML error. Applies live |
| `cursor_blink` | bool | `true` | Applies live; disabled by `performance.low_power_mode` |
| `style` | String | `"minimal-dark"` | Stored only; kept for forward compatibility |
| `show_status_bar` | bool | `false` | Applies live; `Ctrl+Shift+S` toggles and persists it |
| `show_window_controls` | bool | `false` | Accepted; the window uses `[window] show_window_controls` |
| `accent_color` | String | `"#8B9CFF"` | `#RGB` or `#RRGGBB` (error). Very dark values warn (<0.05 relative luminance). Validated but not applied today: the theme's `accent` colour is what the renderer uses |
| `tab_bar_position` | `"top"`, `"bottom"`, `"hidden"` | `"top"` | Any other value is a TOML error. Applies live |
| `compact_tabs` | bool | `false` | Smaller tab labels. Applies live |
| `divider_thickness` | u32 | `1` | 1–12 (error). Applies live |
| `pane_gap` | u32 | `2` | ≤24 (error). Applies live |

### `[workspace]`

| Field | Type | Default | Validation / notes |
|---|---|---|---|
| `restore_on_startup` | bool | `true` | Recreates the tab/split layout from `workspace.toml`. Processes are never restored |
| `close_pane_when_shell_exits` | bool | `true` | If false, a pane whose child exits stays visible until you close it |

### `[performance]`

| Field | Type | Default | Validation / notes |
|---|---|---|---|
| `gpu_acceleration` | bool | `true` | Accepted; not consumed today — the GPU renderer is always attempted |
| `low_power_mode` | bool | `false` | Requests the low-power GPU adapter and disables cursor blinking |
| `max_fps_when_idle` | u32 | `1` | ≤240 (error). Accepted; there is no idle redraw loop to cap |

### `[keys]`

`[keys]` maps an action name to one or more chords. Action names are exactly the
kebab-case names of `ActionId::ALL` (`crates/config/src/keys.rs`). An unknown
action name, an unparsable chord, or an unknown modifier is an **error** and
blocks saving from the panel. Two actions sharing a chord is a **warning**; the
alphabetically first action wins deterministically.

Chord syntax:

- Modifiers, case-insensitive: `ctrl`/`control`, `alt`/`option`/`opt`, `shift`,
  `super`/`meta`/`cmd`/`logo`/`win`.
- Key names: `escape`/`esc`, `enter`/`return`, `tab`, `backspace`/`bs`,
  `delete`/`del`, `insert`/`ins`, `home`, `end`, `pageup`/`pgup`/`prior`,
  `pagedown`/`pgdn`/`next`, `up`/`arrowup`, `down`/`arrowdown`,
  `left`/`arrowleft`, `right`/`arrowright`, `f1`…`f24`.
- Characters: any single character, stored lowercase (`ctrl+t`). Multi-character
  names: `space`, `comma`, `period`/`dot`, `minus`, `plus`, `equal`, `slash`,
  `backslash`, `semicolon`, `apostrophe`/`quote`, `bracketleft`,
  `bracketright`, `backtick`/`grave`.
- A trailing `+` means the plus key: `ctrl+` and `ctrl+plus` are the same chord.
- Alternatives are separated by `|`: `"ctrl+=|ctrl+plus"`. A comma is a key
  name, not a separator.
- An empty value (`""`) or `"none"` removes the default binding for that action.

Default bindings (`default_keybindings()`):

| Action | Chord | Action | Chord |
|---|---|---|---|
| `new-tab` | `ctrl+shift+t` | `next-tab` | `ctrl+pagedown` |
| `close-tab` | `ctrl+shift+alt+w` | `previous-tab` | `ctrl+pageup` |
| `rename-tab` | `ctrl+shift+r` | `tab-1`…`tab-9` | `alt+1`…`alt+9` |
| `new-split-horizontal` | `ctrl+shift+d` | `focus-left`/`right`/`up`/`down` | `alt+left`/`right`/`up`/`down` |
| `new-split-vertical` | `ctrl+shift+e` | `resize-left`/`right`/`up`/`down` | `ctrl+alt+left`/`right`/`up`/`down` |
| `close-pane` | `ctrl+shift+w` | `equalize-splits` | `ctrl+shift+0` |
| `toggle-zoom` | `ctrl+shift+z` | `increase-font-size` | `ctrl+=\|ctrl+plus` |
| `decrease-font-size` | `ctrl+minus` | `reset-font-size` | `ctrl+0` |
| `search` | `ctrl+shift+f` | `search-next` | `ctrl+g` |
| `search-previous` | `ctrl+shift+g` | `copy` | `ctrl+shift+c` |
| `paste` | `ctrl+shift+v` | `scroll-page-up` | `shift+pageup` |
| `scroll-page-down` | `shift+pagedown` | `scroll-to-top` | `ctrl+shift+home` |
| `scroll-to-bottom` | `ctrl+shift+end` | `open-settings` | `ctrl+comma` |
| `reload-config` | `ctrl+shift+comma` | `toggle-status-bar` | `ctrl+shift+s` |
| `new-window` | `ctrl+shift+n` | `send-literal-next` | `ctrl+shift+enter` |
| `quit` | `ctrl+shift+q` | | |

`send-literal-next` arms a one-shot mode: the next key bypasses the keymap and is
encoded for the PTY normally, which is how you send a chord the application
would otherwise capture.

### `[[profiles]]`

| Field | Type | Default | Notes |
|---|---|---|---|
| `name` | String | `""` | Required and unique (case-insensitive); empty or duplicate is an error |
| `shell` | String | `""` | Empty or `auto` means the global shell |
| `args` | array of String | `[]` | Arguments for `shell`; warning when both `command` and `args` are set (`command` wins) |
| `command` | array of String | `[]` | Full argument vector, e.g. `["ssh", "production"]` |
| `working_directory` | string or absolute path | unset | Same rules as `[terminal] working_directory` |
| `env` | table of String → String | `{}` | Layered over `TERM`/`COLORTERM`/`ASTRATERM`/`ASTRATERM_VERSION` |
| `startup_command` | String, optional | unset | Typed into the shell after startup |
| `theme` | String, optional | unset | Validated only; not applied today |
| `font_size` | f32, optional | unset | 4–96 when set (error); not applied today |
| `label` | String, optional | unset | Tab-strip label until an OSC title arrives |

A profile with `shell = "sh"` and an `args` entry `-c` produces a warning: prefer
`command` with an argument vector, because `-c` turns the next argument into a
shell string.

## Validation

`astraterm_config::validation::validate` is pure: it takes a `Config` plus the
known theme names and returns `ValidationReport { issues }`. Severity is
`Warning` or `Error`; `is_valid()` means "no error". The same rules run for TOML
input and for panel edits.

Validation issues are shown in the settings panel and logged at startup. Errors
block persistence; warnings do not.

Summary of rules:

| Field | Error | Warning |
|---|---|---|
| `window.width` | outside 200–16384 | — |
| `window.height` | outside 120–16384 | — |
| `window.opacity` | outside 0.1–1.0 | < 0.5 |
| `terminal.term` | empty or whitespace | not in the allowlist |
| `terminal.scrollback_lines` | outside 100–1,000,000 | — |
| `terminal.shell` | empty; absolute path missing | — |
| `terminal.working_directory` | unknown policy; relative path; missing `Path` | absolute path that is not a directory |
| `font.size` | outside 4–96 | — |
| `font.line_height` | outside 0.8–3.0 | — |
| `font.letter_spacing` | outside −5–20 | — |
| `font.weight` | not 100–900 in steps of 100 | — |
| `font.family` | empty | — |
| `appearance.padding_x` / `padding_y` | either > 512 | either > 64 |
| `appearance.divider_thickness` | outside 1–12 | — |
| `appearance.pane_gap` | > 24 | — |
| `appearance.accent_color` | cannot parse | relative luminance < 0.05 |
| `appearance.theme` | — | name not found (falls back to `midnight`) |
| `appearance.background_blur` | — | enabled while `window.opacity >= 1.0` |
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
     returns `SaveError::ExternalChange` (the file was changed by someone else);
  3. serialize the **canonical** form (`writer::to_toml_string`) and write it;
  4. on any persist error (external change or write failure), restore the previous
     in-memory configuration.
  `SaveOutcome::Unchanged` means the file already contained exactly this
  configuration; nothing was written.
- **Canonical form** means the file is regenerated from the typed model. The
  writer prepends a header that says so: **hand-written comments are not
  preserved across a save from the settings panel**. Comments and unknown keys
  exist only in the file; regenerating it drops them. Key bindings and profiles
  are still valid after a save, but their formatting is normalised.
- **Atomic write** (`writer::write_atomic`): create `.<name>.tmp` in the same
  directory, write, `sync_all`, `rename` over the target, then best-effort
  `sync_all` on the directory. A crash can leave the old file or the new one, not
  a truncated file.
- **External edits** are detected by a `notify` watcher on the config file's
  parent directory. The watcher thread only signals "something changed"; the
  application debounces for 250 ms and then calls
  `ConfigService::reload_if_changed`, which:
  - does nothing when the content hash is unchanged (`ReloadOutcome::Unchanged`);
  - applies a valid file (`Reloaded`);
  - keeps the previous configuration and reports the parse errors
    (`Invalid`) — the last-known-good configuration stays active;
  - resets to defaults when the file was deleted (`Reset`).
  `Ctrl+Shift+Comma` (`reload-config`) forces the same check manually.
- **No silent overwrite:** the panel refuses to save when the file changed since
  the service last loaded or reloaded it, and tells the user to reload first.
  (An external edit that has already been re-read and rejected as invalid becomes
  the new baseline, so a later save replaces it.)

## What applies live

`Application::apply_config` runs after a reload or a successful panel save. It
rebuilds the keymap, resolves the theme, pushes the configuration into every
window (`WindowUi::apply_config`: font and cell metrics, layout, theme, text-cache
invalidation), resizes PTY grids and refreshes titles.

Live without a restart:

- theme, cursor shape and blink, padding, pane gap, divider thickness, tab bar
  position, compact tabs, status bar;
- font family/size/weight/line height/letter spacing;
- all keybindings;
- `confirm_on_close`, `copy_on_select`, `open_urls_on_click`,
  `close_pane_when_shell_exits`;
- `performance.low_power_mode` (blinking and adapter preference for new windows).

`workspace.restore_on_startup` is read when the next window is created, so it
takes effect on the next start rather than immediately.

Requires a new session/window (`Field::requires_restart()` in
`crates/settings-ui/src/fields.rs`):

`terminal.term`, `terminal.scrollback_lines`, `terminal.working_directory`,
`terminal.shell`, `window.width`, `window.height`, `window.startup_maximized`,
`performance.gpu_acceleration`.

`terminal.shell_args` is not a panel field but also only affects new sessions,
and all `[[profiles]]` fields are applied when a profile-backed session is
created.

Additionally, but not in `requires_restart()`: `window.show_window_controls` and
whether the surface is created translucent are fixed when the window is created;
changing opacity afterwards only changes the scene alpha, not the surface.

Fields accepted by the schema but not consumed by today's code (setting them is
harmless, and they are marked here so nobody expects an effect):

| Field | Current status |
|---|---|
| `terminal.word_delimiters` | no consumer; double-click selection uses the terminal core's word rules |
| `window.remember_geometry` | no consumer; size is persisted in `workspace.toml` but restore does not apply it |
| `appearance.style` | stored only |
| `appearance.accent_color` | validated; the renderer uses the theme's `accent` colour |
| `appearance.show_window_controls` | unused; `[window] show_window_controls` is the one that is applied |
| `appearance.background_blur` | no blur request |
| `font.fallbacks` | no consumer; cosmic-text handles fallback |
| `performance.gpu_acceleration` | no consumer; the GPU renderer is always attempted |
| `performance.max_fps_when_idle` | no idle redraw loop to cap |
| `profiles[].theme`, `profiles[].font_size` | validated but not applied |

## Example configuration

Complete and valid against the schema and validation rules in this document.
Every section is present; `[keys]` lists the full default table, so you can see
the exact names and chords to edit.

```toml
[window]
width = 1440
height = 900
opacity = 1.0
startup_maximized = false
show_window_controls = true
remember_geometry = true

[terminal]
shell = "auto"
shell_args = []
term = "xterm-256color"
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
show_status_bar = false
show_window_controls = false
accent_color = "#8B9CFF"
tab_bar_position = "top"
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

To unbind a default action instead of rebinding it:

```toml
[keys]
rename-tab = "none"
```

Note on themes: built-ins are always available. Files in the theme directory use
the `Theme` schema (`name`, optional `light`, then the `Palette` fields, with
`ansi` as an array of 16 `#rrggbb` strings); see
`assets/themes/midnight.toml` for a complete, valid example. A user theme with
the same name as a built-in replaces it in the theme list, while name resolution
still prefers the built-in.
