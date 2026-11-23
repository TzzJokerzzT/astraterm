# AstraTerm — transparency, system status, theme editor, vertical tabs

Feature: `astraterm-polish`. Continues `odd/tasks/astraterm-terminal.md`.

## Specs

- **S1 — Transparent background.** «Quiero que la terminal pueda tener fondo con
  opacidad.» The window background (and only the background) must be translucent at
  the configured `[window] opacity`, live, without restarting; text and explicit
  cell colours stay fully opaque so legibility never depends on what is behind the
  window.
- **S2 — Status bar system information.** «Quiero que la barra de estado muestre de
  lado derecho => El consumo de la cpu, consumo de ram y hora.» When the status bar
  is enabled it shows CPU usage, RAM usage and the local time on the right-hand
  side; session information stays on the left. Reading `/proc` is cheap and must not
  become polling: at most one sample per second, and only while the status bar is
  visible.
- **S3 — Themes created from the settings panel.** «Quiero que puedas crear los temas
  desde el setting, nombrarlos y guardarlos.» From the native settings panel the user
  can adjust the theme colours, give the result a name and save it as a theme file in
  `~/.config/astraterm/themes/`, after which it becomes selectable and is selected.
  Changes preview live. Saving validates the name and the colours and reports errors
  in the panel instead of writing a broken file.
- **S4 — Tabs on the right.** «Quiero tambien seleccionar desde los settings que las
  pestañas aparezcan arriba o al costado derecho como en la terminal Warp.»
  `appearance.tab_bar_position` gains `right`: a vertical tab strip on the right edge
  with a configurable width, vertical hit testing, the same active-tab indicator and
  a terminal area that shrinks accordingly.

## Tasks

| ID | Spec | Work | Route |
|---|---|---|---|
| T1 | S1-S4 | Config schema, defaults and validation: `tab_bar_position = "right"`, `tab_strip_width`, status-bar segments, `clock_format` | inline |
| T2 | S2 | `astraterm-platform::system`: `/proc/stat` and `/proc/meminfo` parsers, CPU delta, civil-clock formatting, all unit-tested | inline |
| T3 | S1 | Renderer: translucent surface by default, scene alpha plumbing so only backgrounds are translucent | inline |
| T4 | S2, S4 | Window UI: vertical tab strip (layout, painting, hit testing), status bar left/right sections | inline |
| T5 | S3 | Theme editor in the settings panel: palette draft, live preview, name + save through the config service | inline |
| T6 | S1-S4 | Docs, README, runtime verification and work-unit commits | inline |

## Log

### L1 — user request (verbatim)

> 1. Quiero que la terminal pueda tener fondo con opacidad.
> 2. Quiero que la barra de estado muestre de lado derecho => El consumo de la cpu,
>    consumo de ram y hora
> 3. Quiero que puedas crear los temas desde el setting, nombrarlos y guardarlos
> 4. Quiero tambien seleccionar desde los settings que las pestañas aparezcan arriba
>    o al costado derecho como en la terminal Warp

### L2 — decisions taken

- Opacity is applied to the window background, pane background, tab strip and status
  bar; glyphs, cursor and explicit cell backgrounds stay opaque (Warp behaves the
  same way). The surface is created with premultiplied alpha whenever the compositor
  offers it, so changing opacity from the settings panel works live instead of
  requiring a new window.
- Blur remains a compositor rule (documented), never claimed by the application.
- Tabs on the right use a fixed logical width (`tab_strip_width`, default 180 px)
  rather than auto-sizing to the longest title: a strip that changes width while
  titles change would move the terminal on every command.
- The status bar samples `/proc` at most once per second and only while it is
  visible; when it is hidden there is no timer at all.
- Theme files are named from a sanitised theme name (`[a-z0-9-_]`, max 40 chars) and
  written atomically; the panel reports an invalid name or colour instead of saving.
- Clock formatting supports a small strftime-like subset (`%H %M %S %d %m %Y %b %a`)
  with everything else literal, implemented without adding a date dependency.

### L3 — runtime verification (Wayland/Hyprland, 2494x1396)

Verified with a throwaway configuration (`--config /tmp/astra-demo.toml`, removed
afterwards) so the user's own config was never touched:

| Feature | Evidence |
|---|---|
| Translucent background | `opacity = 0.78` shows the wallpaper through the terminal while text and the prompt stay opaque |
| Status bar right side | bottom row reads `CPU 11% · RAM 62% · 15:17`; the left side keeps the session title, cwd and grid size |
| Tabs on the right | vertical strip on the right edge with the active-tab indicator; the terminal width shrinks by `tab_strip_width` |
| Settings rows | Appearance lists `Status bar: CPU/RAM/clock`, `Clock format`, `Tab bar position = right`, `Tab strip width = 200` |
| Theme editor | `Colours & themes` shows `Theme / Edit theme colour / Colour value / Save theme as…`; changing the value to `#2a1030` previewed live, saving wrote `themes/midnight-custom.toml` and set `theme = "midnight-custom"` in the config |

345 automated tests cover the same behaviour without a window: chrome geometry and
hit testing (`crates/app/src/chrome.rs`), the `/proc` parsers and clock
(`crates/platform/src/system.rs`), the panel editor
(`crates/settings-ui/src/panel.rs`) and the save/select workflow
(`crates/config/tests/theme_workflow.rs`).

### L4 — documentation pass and a border regression caught by it

Writing the option reference exposed a real defect the earlier screenshots had not
reached: `paint_pane` drew the pane border as a **filled rectangle** covered by the
pane background. Painting the background at alpha 0 (which is what makes the
window translucent without compositing alpha twice) left the fill visible, so a
split pane became a solid accent-coloured block.

The border is now four thin edges painted opaque over a translucent background, so
chrome stays readable while the terminal surface fades. Covered by
`the_border_stays_opaque_when_the_pane_background_is_transparent`, and verified on
screen with a restored two-pane workspace at `opacity = 0.82`.

`docs/CONFIGURATION.md` was rewritten as the single reference: per-option
description and purpose (plus the panel row that edits it), recipes for opacity,
compositor blur (Hyprland/Omarchy, KDE, GNOME, Sway), the status bar, right-hand
tabs and theme creation. A verification script confirms the document covers every
field of `Config`, every `Profile` field, every `ActionId` and that each TOML block
in it parses.
