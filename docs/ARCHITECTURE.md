# Architecture

AstraTerm is a native Linux terminal emulator built from one Cargo workspace. It
opens its own window with `winit`, draws with `wgpu` + `glyphon`, owns a real PTY
through `portable-pty`, and emulates the terminal with `alacritty_terminal`
behind a small adapter. It never embeds a browser engine, a webview or tmux.

## Layers

```
                    ┌──────────────────────────────────────┐
                    │ crates/app  (winit event loop)        │
                    │ Application · WindowUi · key dispatch │
                    └───────┬───────────────┬──────────────┘
            creates and     │               │  composes a Scene
            drives          │               ▼
                    ┌───────▼──────┐  ┌─────────────────────┐
                    │ workspace    │  │ renderer (wgpu)     │
                    │ tabs/splits  │  │ quads + glyphon     │
                    │ layout/focus │  └─────────────────────┘
                    └──────────────┘
                            ▲
                    pane → session binding
                            │
                    ┌───────┴──────────────────────────────┐
                    │ session                              │
                    │ SessionManager · pump · budgets      │
                    └───────┬───────────────┬──────────────┘
                            │               │
                    ┌───────▼──────┐  ┌─────▼──────────────┐
                    │ terminal     │  │ pty                │
                    │ TerminalEngine│ │ reader/writer/waiter│
                    │ over alacritty│ │ 3 threads          │
                    └──────────────┘  └────────────────────┘

  cross-cutting: config (one typed model) · settings-ui (panel over that model)
                 platform (clipboard, URLs, XDG paths, fonts) · core (ids,
                 geometry, events)
```

The workspace does **not** depend on sessions and the session layer does **not**
depend on the UI. The application is the only place that knows about every
subsystem: it binds a `PaneId` to a `SessionId`, routes input, and turns
snapshots into a scene.

## Crates

All public entry points below are the re-exports of each `src/lib.rs` (the app
crate is a binary with modules). Dependency versions are pinned in the root
`Cargo.toml`.

### `crates/core` — vocabulary

Purpose: stable identifiers, geometry primitives and inter-subsystem events.
Dependencies: none (optional `serde` feature).

Entry points: `PaneId`, `TabId`, `SessionId`, `WindowId`, `ProfileId`
(`ids.rs`, monotonic process-local counters with `from_raw`/`raw` for workspace
restore); `Rect`, `Size`, `CellMetrics`, `Padding`, `SplitAxis`
(`geometry.rs`); `AppCommand`, `AppEvent`, `SessionEvent`, `ExitReason`,
`FocusDirection` (`events.rs`).

Notes:

- `SplitAxis::Horizontal` splits along the horizontal axis, producing two panes
  stacked vertically. `SplitAxis::Vertical` produces side-by-side panes.
- `CellMetrics::grid_size` returns at least `1x1`; the PTY is never resized to a
  zero grid.

### `crates/config` — one typed configuration model

Purpose: the single schema shared by TOML, the atomic writer and the settings
panel.

Entry points: `Config` and its sections (`schema.rs`), `Keymap`/`Chord`/
`ActionId`/`default_keybindings` (`keys.rs`), `Palette`/`Rgb`/`Color`
(`palette.rs`), `load_file`/`parse_str` (`loader.rs`), `validate`/
`ValidationReport`/`ConfigIssue` (`validation.rs`), `ConfigService`/`SaveOutcome`
(`service.rs`), `write_atomic`/`to_toml_string` (`writer.rs`), `Theme`/
`available_themes`/`resolve_theme` (`themes.rs`), `default_config_path`/
`config_dir`/`themes_dir`/`state_dir` (`lib.rs`).

Notes:

- Every field has a `#[serde(default)]`, so a partial file is valid.
- Unknown keys are collected with `serde_ignored` and reported as warnings, not
  errors.
- `ConfigService` is the only writer. Validation runs before a value is applied
  or persisted.

### `crates/workspace` — native multiplexer model

Purpose: windows, tabs, the recursive split tree, focus and layout math. Pure
data and pure functions: no window, GPU or PTY.

Entry points: `SplitNode` (`tree.rs`: `split_at`, `remove_pane`, `adjust_ratio`,
`equalize`, `swap_panes`, `split_at_point`, `panes`), `Tab` (`tab.rs`),
`WindowState` (`window.rs`), `LayoutConfig` + `Placement` (`lib.rs`,
`layout.rs`: `compute_layout`, `compute_dividers`, `split_extent`),
`focus::nearest_in_direction`/`next_pane`, `Pane` (`pane.rs`), and the
persistence snapshots (`snapshot.rs`: `snapshot_window`, `restore_window`,
`WorkspaceSnapshot`).

Notes:

- Layout is a binary tree with a ratio per split, clamped to `MIN_RATIO = 0.1` /
  `MAX_RATIO = 0.9`. `split_extent` guarantees the two child extents sum exactly
  to the available space, so panes tile without seams.
- Focus traversal is geometric, not tree-order: `focus::nearest_in_direction`
  scores candidates by pixel distance so `Alt+Left` means "visually left".
- `Tab::zoomed` temporarily shows a single pane; splitting clears zoom.

### `crates/pty` — PTY and process management

Purpose: one PTY per session with non-blocking access from the UI.

Entry points: `PtySpec`, `PtyHandle`, `PtyError`, `spawn`, `spawn_with_waker`,
`Waker`.

Design (all in `crates/pty/src/lib.rs`):

- `spawn_with_waker` opens the PTY, builds the child with `CommandBuilder`
  (program + argument vector, never a shell string), and starts three threads:
  - **reader** — reads up to `READ_CHUNK = 32 KiB`, sends into a bounded output
    queue of `OUTPUT_QUEUE = 64` chunks, then calls the waker. The send blocks,
    which is deliberate backpressure: the child blocks instead of output being
    dropped.
  - **writer** — owns the PTY master and drains a bounded input queue of
    `INPUT_QUEUE = 1024` messages (`Data`, `Resize`, `Shutdown`). Resizes are
    applied from this thread.
  - **waiter** — waits for the child, publishes `ExitReason` exactly once and
    wakes the event loop.
- `PtyHandle::write` uses `try_send`: a full queue returns `false` so the caller
  sees backpressure. `write_blocking` is used for small mandatory writes
  (protocol replies, startup input).
- `PtyHandle::shutdown` asks the writer to close the PTY (SIGHUP), waits
  `KILL_GRACE = 400 ms`, then calls the child killer and waits up to two seconds.
- If `TERM`/`COLORTERM` are absent from the spec, the builder adds
  `xterm-256color` and `truecolor`.

### `crates/terminal` — terminal engine adapter

Purpose: the only place that touches `alacritty_terminal`.

Entry points: `TerminalEngine` (`engine.rs`: `new`, `process`, `resize`,
`snapshot`, `scroll_display`, `scroll_to`, `modes`, `drain_events`,
`update_selection`, `selection_text`, `line_text`, `line_range`,
`encode_color_reply`), `EngineOptions`, `TerminalEvent`, `SelectionKind`;
`snapshot.rs` types (`Snapshot`, `RowSnapshot`, `CellSnapshot`, `ModeFlags`,
`CursorSnapshot`, ...); `input.rs` (`encode_key`, `encode_mouse`, `encode_paste`,
`KeyPress`, `TermKey`, `MouseEvent`); `search.rs` (`find`, `count_matches`,
`SearchState`).

Notes:

- `TerminalEngine::process` advances a `vte::ansi::Processor`; `snapshot` walks
  the renderable grid, reports the cursor, the selection and `damaged_rows`
  (from `Term::damage()`, reset after collection).
- `EngineOptions::allow_osc52` defaults to `false` and the session layer
  hard-codes it off; `kitty_keyboard` is `false` in the `Term` config.
- The engine is synchronous and single-threaded. Bytes arrive from the session
  layer; a snapshot is taken when the UI asks for one.

### `crates/session` — session lifecycle and pumping

Purpose: bind one PTY process to one terminal engine and one pane.

Entry points: `SessionManager` (`manager.rs`: `create`, `pump`, `close`,
`shutdown_all`, `reap_exited`, `set_waker`, `for_pane`), `Session`, `PumpReport`,
`SessionError`; `SessionSpec`/`session_spec`/`resolve_shell` (`lifecycle.rs`).

Notes:

- `SessionManager::pump` gives every session one budget pass:
  `PUMP_BUDGET_BYTES = 256 KiB` or `PUMP_BUDGET_CHUNKS = 512` chunks, whichever
  is reached first. Leftover output stays in the bounded PTY queue.
- A pump pass stops once the byte budget is reached, so it can overshoot its budget
  by at most one read chunk (`astraterm_pty::READ_CHUNK_BYTES`); `PumpReport`
  reports the exact number of bytes that reached the engine.
- Window chrome geometry (strip position, status bar, tab hit testing, label
  fitting) lives in `crates/app/src/chrome.rs` as pure functions, so layout rules are
  unit-tested without a window.
- The status bar samples `/proc/stat` and `/proc/meminfo` through
  `astraterm_platform::system::SystemMonitor` at most once per second, and only
  while the bar is visible; the clock is formatted through the C library so the
  local timezone and locale are honoured.
- Opacity is painted, not composited twice: the scene clear carries the configured
  alpha (premultiplied for the surface) and the pane background is left transparent,
  so a translucent window never compounds alpha into looking opaque.
- `pump` also translates engine events (title, bell, OSC 52 store, colour
  queries, direct PTY replies) into `SessionEvent`s or `PumpReport` fields, and
  reports child exit exactly once. A colour query (OSC 4/10/11) becomes
  `SessionEvent::ColorQuery`; the application resolves it against the active
  theme and answers through `SessionManager::reply` with
  `TerminalEngine::encode_color_reply`.
- The working directory is read from `/proc/<pid>/cwd` at most every
  `500 ms` (`CWD_REFRESH_INTERVAL`).
- `session_spec` resolves shell, arguments, environment (`TERM`, `COLORTERM`,
  `ASTRATERM`, `ASTRATERM_VERSION`, profile `env`), working directory and
  scrollback. Profile `startup_command` is carried as typed input, never as
  `argv`.

### `crates/renderer` — wgpu + glyphon

Purpose: turn a `Scene` into a frame. The renderer knows nothing about panes,
tabs or sessions.

Entry points: `Renderer` (`lib.rs`: `new`, `render`, `resize`, `set_font`,
`set_scale`, `cell_metrics`, `invalidate_text_cache`), `RendererOptions`,
`RendererError`, `pick_format`, `target_expects_linear`, `scissor_rect`;
`paint.rs` (`Scene`, `Quad`, `TextItem`, `Span`, `paint_pane`,
`paint_cell_backgrounds`, `resolve`, `srgb_to_linear`, `rgba`); `quad.rs`
(`QuadPipeline`); `text.rs` (`TextStyle`, `RowCache`, `measure_cell`).

Notes:

- `quad.rs` is a single instanced pipeline: six vertices per rectangle, one
  instance per quad, one draw call per frame. Instances start at 2048 and the
  buffer grows to the next power of two when needed.
- A frame carries up to two layers. `Renderer::render` takes the mosaic and an
  optional overlay; the mosaic clears the target, the overlay is recorded with
  `LoadOp::Load` and scissored to `Scene::clip`. Each pipeline keeps one instance
  buffer and draws all of it, and glyphon's `prepare` concatenates every area
  into one vertex buffer that `render` draws whole, so the overlay layer owns a
  second quad pipeline, a second image pipeline and a second `TextRenderer`. The
  image cache, atlas, font system, swash cache, shaped-row cache and viewport stay
  shared.
- `text.rs` shapes rows with cosmic-text and caches the shaped `Buffer` per
  `TextItem::id`. A cached row is reused while its content hash and the font
  style key are unchanged.
- `paint.rs` is pure: it has no GPU or window dependency and is unit-tested.

### `crates/settings-ui` — native settings panel

Purpose: a keyboard-driven settings model and its plain-text projection. No
webview, no GUI toolkit.

Entry points: `SettingsPanel` (`panel.rs`: `new`, `handle_key`, `lines`,
`set_message`, `set_issues`, `on_saved`, `revert`, `draft`), `Section`,
`PanelKey`, `PanelAction`, `PanelLine`, `PanelStyle`; `Field`, `FieldKind`
(`fields.rs`).

Notes:

- The panel edits a draft `Config` and never writes files. A save returns
  `PanelAction::Save(Box<Config>)`; the application persists it through
  `ConfigService`.
- The Keyboard and Profiles sections are intentionally empty: shortcuts and
  profiles are TOML-only (`[keys]`, `[[profiles]]`).
- The projection is drawn by the application with the same quad/text renderer as
  the terminal.

### `crates/platform` — Linux integration

Purpose: clipboard, URL opening, XDG state paths and font discovery without
`sh -c`.

Entry points: `Clipboard` (`clipboard.rs`: `detect`, `set`, `get`, `backend`),
`open_url`/`is_safe_url`/`is_wayland`/`is_x11` (`linux.rs`), `StatePaths`
(`paths.rs`: `workspace_file`, `geometry_file`, `ensure`), `list_families`/
`has_family` (`fonts.rs`).

Notes:

- Clipboard backends are `wl-copy`/`wl-paste`, then `xclip`, then `xsel`; with
  none installed the service reports `Backend::Unavailable` instead of failing.
- `open_url` runs `xdg-open <url>` with a single argument after validating the
  scheme. It is only called from an explicit Ctrl+click on a detected http(s)
  URL.
- `StatePaths` derives from `astraterm_config::state_dir()`
  (`$XDG_STATE_HOME/astraterm`, default `~/.local/state/astraterm`).
  `geometry_file()` exists but is not written today; window size is stored in
  `workspace.toml` instead.

### `crates/app` — the application

Purpose: winit event loop, command dispatch, input translation, scene
composition, CLI.

Entry points: the `astraterm` binary (`main.rs`), `Application`
(`app.rs`: `new`, `dispatch`, plus the `ApplicationHandler` implementation),
`UserEvent`, `Cli` (`cli.rs`), `keys::{chord_of, key_press, command_for}`
(`keys.rs`), `WindowUi`/`Overlay`/`blink_due`/`find_in_snapshot`/`url_at`
(`ui.rs`).

Notes:

- `Application::new` loads the config, writes a default file on first run (only
  when `--config` was not given), starts the config watcher thread, builds the
  keymap and detects the clipboard backend.
- `WindowUi` owns the renderer, the workspace, pane metadata and pointer state
  for one window. It never owns sessions.
- `WindowUi` also owns at most one optional floating terminal (`Floating`), which
  lives outside the split tree: the mosaic layout and the reflow never see it, it
  keeps its own session, and it is composed into the overlay layer, above the
  panes and below the modal overlays.
- `window.set_ime_allowed(true)` with `ImePurpose::Terminal`: composed text arrives as
  `WindowEvent::Ime` and is written to the focused pane. Preedit is traced, not drawn.
- Only one window is created in `resumed`; further windows come from the
  `NewWindow` command.

## Event flow

There is no polling loop. Output wakes the event loop, and a frame is drawn only
when something requested a redraw.

```
child process
   │ bytes
   ▼
pty reader thread ── bounded output queue ──► waker (Arc<dyn Fn>)
                                                   │ EventLoopProxy::send_event
                                                   ▼
                                     UserEvent::Wake (winit user event)
                                                   │
                                     Application::user_event
                                                   ▼
                                     Application::pump
                                       └─ SessionManager::pump
                                            ├─ try_recv_output → engine.process
                                            ├─ drain_events → SessionEvent
                                            └─ PumpReport { bytes, exits, replies }
                                                   ▼
                                     handle_session_event
                                       ├─ Output → refresh cwd
                                       ├─ Title  → session.title, window title
                                       ├─ Bell   → status message
                                       └─ Exited → close pane/tab (per config)
                                                   ▼
                                     window.request_redraw()
                                                   ▼
                                     RedrawRequested → WindowUi::compose
                                                   ▼
                                     Renderer::render (present)
```

The PTY crate calls the waker after every queued chunk and after the child
exits. `Application::resumed` installs the waker with
`SessionManager::set_waker`, and the waker closes over the `EventLoopProxy`. A
blocked reader means the queue is full; once the UI pumps, the reader unblocks
and wakes the loop again. A quiet terminal therefore sleeps in
`ControlFlow::Wait` with no timer.

The only timers are:

- the cursor blink (`blink_due`, only while blinking is enabled, the window is
  focused and no overlay is open), and
- the config-watcher debounce (`UserEvent::ConfigChanged` arms a 250 ms
  deadline; `about_to_wait` performs the reload when it expires).

## Threading rules

| Rule | Where it is enforced |
|---|---|
| No PTY read/write on the UI thread | `crates/pty/src/lib.rs`: reader/writer threads; the UI only calls `try_recv_output`, `try_send`/`send` on channels |
| Output is never dropped | reader uses a blocking `send` into a bounded queue (`OUTPUT_QUEUE = 64`); the child is throttled by the kernel |
| Queues are bounded | output 64 chunks, input 1024 messages, exit status 4 |
| One noisy pane cannot starve others | `SessionManager::pump` applies `PUMP_BUDGET_BYTES`/`PUMP_BUDGET_CHUNKS` per session per pass |
| Input backpressure is visible | `PtyHandle::write` returns `false`; `Session::write` counts `input_drops` |
| No orphan children | `PtyHandle::shutdown` (close PTY, 400 ms grace, kill, wait), `SessionManager::shutdown_all` and its `Drop` |
| Cross-thread wakeups are messages | PTY waker and config watcher send `UserEvent` through the event-loop proxy; no shared mutable UI state |

One nuance: `Session::write_urgent` uses `PtyHandle::write_blocking`, a blocking
**channel send** (not PTY I/O). It is used for startup input and for direct
protocol replies (the bytes collected from `TerminalEvent::PtyReply`) and can wait
if the 1,024-message input queue is full.

## Rendering pipeline

1. **Compose (pure).** `WindowUi::compose` builds a `Frame`: the mosaic `Scene`
   (clear colour, quads and text items) and, when the floating terminal or a modal
   overlay is on screen, the overlay `Scene` that is drawn after it.
   `paint::paint_pane` paints one pane: border, background, selection, cell
   backgrounds/underlines, one text item per visible row, and the cursor. The tab
   strip, status bar, search bar, confirmation dialog and settings panel are
   composed the same way. Text item ids are stable
   (`(pane_index + 1) * 0x1000 + row` for terminal rows, fixed ranges for
   chrome), which is what makes the shaped-row cache effective.
2. **Colour-space normalisation.** If the surface format expects linear light,
   `Scene::linearize` converts clear, quad and span colours with
   `srgb_to_linear`; alpha is untouched. Both layers are converted.
3. **Quads.** `Scene::quads` becomes `quad::Instance`s and is uploaded to the
   layer's instance buffer; `QuadPipeline::draw` issues one instanced draw call.
4. **Text.** `RowCache::update` re-shapes only items whose content hash or font
   style key changed, then `areas` produces one `glyphon::TextArea` per text
   item. `TextRenderer::prepare` uploads glyphs into the atlas and
   `TextRenderer::render` draws them.
5. **Present.** The mosaic is recorded in one render pass: clear, quad draw,
   image draws, glyph draw. A present overlay is recorded in a second pass over
   the same target with `LoadOp::Load` and `set_scissor_rect` on its `Scene::clip`,
   in the same order. Because the mosaic's glyph draw finished before the overlay
   pass begins, the overlay's background covers the mosaic's text instead of the
   text painting over it. The frame is submitted and presented with
   `PresentMode::Fifo`; the atlas is trimmed after each frame. A lost/outdated
   surface is reconfigured; `Timeout`/`Occluded` frames are skipped without an
   error.

Damage handling today: `TerminalEngine::snapshot` reports `damaged_rows`, but the
renderer has no consumer for it yet. Every redraw composes the whole frame; the
row cache prevents re-shaping unchanged rows, but quads are rebuilt and the text
areas are re-prepared each frame. See `docs/PERFORMANCE.md`.

### Colour space and surface format

Themes are authored in sRGB 8-bit (`Rgb`, `#rrggbb`). The renderer prefers a
plain 8-bit UNORM surface so those values can be written verbatim:

`pick_format` preference order: `Bgra8Unorm`, `Rgba8Unorm`,
`Bgra8UnormSrgb`, `Rgba8UnormSrgb`, `Rgba16Float`, `Rgba32Float`, else the
first reported format.

`target_expects_linear` is true for sRGB formats and float formats. For those,
the scene is linearised per frame (a clone plus conversion, which costs an
allocation) so colours do not drift. Without this conversion an sRGB target
would wash the UI out.

Transparency: when `window.opacity < 1.0` the renderer requests
`CompositeAlphaMode::PreMultiplied` if the surface supports it, otherwise it
stays opaque. The scene clear alpha is `window.opacity`; text and chrome are
painted opaque so they stay readable. Window transparency is chosen at surface
creation, so changing opacity below 1.0 only takes full effect in a new window.

## Dependency graph

Verified from each crate's `Cargo.toml`:

| Crate | Depends on (workspace crates) |
|---|---|
| `core` | — |
| `config` | `core` |
| `workspace` | `core` |
| `pty` | `core` |
| `terminal` | `core` |
| `session` | `core`, `config`, `pty`, `terminal` |
| `renderer` | `core`, `config`, `terminal` |
| `settings-ui` | `core`, `config` |
| `platform` | `core`, `config` |
| `app` | all of the above |

The graph is a DAG. Rules that keep it that way:

- Nothing below `app` may depend on `app`, `renderer` or `winit` (except the
  renderer itself, which depends on `winit` only for `Window`, `PhysicalSize` and
  `OwnedDisplayHandle`).
- Upstream terminal types (`alacritty_terminal`) appear only in `crates/terminal`
  (and in `crates/renderer`'s dev-dependencies for tests). Everything else speaks
  the adapter types.
- The workspace has no knowledge of sessions or PTYs; the application binds
  `PaneId` → `SessionId`.
- New shared vocabulary belongs in `core`, which has no dependencies.
