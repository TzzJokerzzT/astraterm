# AstraTerm — native Rust terminal emulator (Linux)

Feature: `astraterm-terminal`. Source specification:
`astraterm_requirements_and_ai_agent_prompt.md` (the user's document, at the
repository parent directory).

## Specs

- **S1 — Real terminal, native window.** «Standalone native graphical terminal
  emulator», Linux-only, «its own graphical window, launch real shell processes,
  emulate terminal protocols correctly, and provide native workspace management».
  «Do not use Electron, Chromium, browser-based rendering, or a webview.»
- **S2 — Terminal core, not a text widget.** «Use a mature terminal emulation core
  such as `alacritty_terminal`, behind an internal adapter.» «Do not fake terminal
  functionality with a text widget.»
- **S3 — PTY.** «Use a real PTY implementation such as `portable-pty`.» «Never
  perform blocking PTY I/O on the UI thread.» «Do not silently drop terminal
  output.»
- **S4 — Native multiplexer.** «Implement tabs, splits, focus, and pane/session
  lifecycle natively in Rust; tmux must not be an internal runtime dependency.»
  «Use a recursive layout tree rather than a fixed grid.»
- **S5 — Hybrid UI.** «Keep the terminal workspace minimal and native; provide a
  polished graphical settings experience without embedding a webview.»
  «Avoid a permanent sidebar consuming workspace width.»
- **S6 — One configuration model.** «Provide TOML configuration and a native
  graphical settings panel backed by one shared typed configuration model.» «Use
  atomic writes (temporary file + rename) to avoid corrupting the config.» «TOML
  edits made externally are detected and reloaded safely.» «never silently
  overwrite newer user changes».
- **S7 — Dark, minimalist, productivity-first.** «Dark theme is the default;
  provide a light theme as an option, not as the primary identity.» «Blur off by
  default; transparency configurable.» «Status bar off by default.»
- **S8 — Configurable shortcuts.** «All shortcuts must be user-configurable, with
  sensible defaults», including `Ctrl+Shift+T`, `Ctrl+Shift+W`, `Ctrl+PageDown`,
  `Ctrl+PageUp`, `Ctrl+Shift+D`, `Ctrl+Shift+E`, `Alt+Arrow`, `Ctrl+=`, `Ctrl+-`,
  `Ctrl+0`, `Ctrl+Shift+F`, `Ctrl+Shift+C`, `Ctrl+Shift+V`, `Ctrl+,`,
  `Ctrl+Shift+N`. «Provide a way to pass a key sequence through to the
  shell/application.»
- **S9 — SSH through OpenSSH.** «MVP approach: use the system OpenSSH client.»
  «Never store SSH passwords or private keys in the terminal's configuration.»
  «Do not disable SSH host-key checking.»
- **S10 — Performance and resources.** «Low idle cost: no continuous polling or
  unnecessary redraws; sleep when there is no work.» «prefer dirty-row/cell
  rendering and damage tracking.» «Bound scrollback and internal queues.»
  «A noisy pane must not starve input or rendering in other panes.»
- **S11 — Terminal correctness.** Must support 16/256/24-bit colour, alternate
  screen, bracketed paste, mouse reporting, application cursor/keypad modes,
  cursor styles, OSC title, Unicode and wide characters, scrollback, resize while
  full-screen applications run. «Do not claim Neovim compatibility based only on
  the ability to launch `nvim`.»
- **S12 — Terminfo policy.** «Do not advertise a custom `$TERM` identifier unless
  its terminfo entry is installed»; default `xterm-256color`.
- **S13 — Security.** «Never execute shell strings through `sh -c` when an
  argument vector can be used.» «Do not log terminal output, secrets, or clipboard
  data.» «Treat OSC sequences and terminal escape sequences as untrusted input.»
  «Avoid unsafe Rust unless required and documented.»
- **S14 — Quality gates.** `cargo fmt`, `cargo clippy --workspace --all-targets
  --all-features -- -D warnings`, `cargo test --workspace`, release build.
- **S15 — Packaging.** Desktop entry, icon, Linux packaging, documentation
  (`docs/ARCHITECTURE.md`, `docs/CONFIGURATION.md`,
  `docs/TERMINAL_COMPATIBILITY.md`, `docs/PERFORMANCE.md`, `docs/SECURITY.md`).

## Tasks

| ID | Spec | Work | Route | Commit |
|---|---|---|---|---|
| T1 | S1 | Cargo workspace scaffold, shared ids/geometry/events (`crates/core`) | inline | — |
| T2 | S6 | Typed configuration model, TOML loader/writer, validation, atomic service (`crates/config`) | inline | — |
| T3 | S4 | Split tree, tabs, focus traversal, layout math, workspace metadata (`crates/workspace`) | inline | — |
| T4 | S3 | PTY adapter with bounded queues and reader/writer/waiter threads (`crates/pty`) | inline | — |
| T5 | S2, S11 | `TerminalEngine` adapter, snapshot/damage, selection, search, key/mouse encoding (`crates/terminal`) | inline | — |
| T6 | S3, S4, S10 | Session manager: lifecycle, output coalescing, budgets, cwd tracking (`crates/session`) | inline | — |
| T7 | S1, S10 | wgpu/glyphon renderer: grid text, cursor, selection, pane chrome, damage-aware row buffers (`crates/renderer`) | inline | — |
| T8 | S5, S7 | Native settings panel model + rendering without a webview (`crates/settings-ui`) | inline | — |
| T9 | S1, S5, S8 | Application: winit window, keybindings, tabs/splits, clipboard, search, CLI (`crates/app`) | inline | — |
| T10 | S9 | SSH profiles through OpenSSH, URL detection, workspace restore | inline | — |
| T11 | S15 | Documentation set and Linux packaging | delegated | — |
| T12 | S14 | Quality gates: fmt, clippy, tests, release build | inline | — |

## Log

### L1 — user request (verbatim)

> @astraterm_requirements_and_ai_agent_prompt.md implementa este documento acorde a
> todo lo que hay dentro. Si hay alguna duda hazmelo saber

### L2 — environment evidence (2026-07 machine state)

- Rust 1.99.1 (Arch/Omarchy), Wayland session (`wayland-1`), X11 `:0` also
  available.
- System libraries present: wayland-client, xkbcommon, fontconfig, freetype2, egl,
  libdecor, vulkan loader. `nvim` and `tmux` installed for compatibility tests.
- Dependency versions validated as mutually compatible: `winit 0.30.13`,
  `wgpu 30.0.2`, `glyphon 0.12.0`, `cosmic-text 0.19.0`,
  `alacritty_terminal 0.26.0`, `portable-pty 0.9.0`, `toml 1`, `notify 9`.

### L3 — decisions taken where the specification left a choice

- `Ctrl+Shift+W` closes the focused pane (and the tab when it is the last pane),
  matching the specification's combined "Close tab/pane" row. Closing a whole tab
  with several panes is bound to `Ctrl+Shift+Alt+W`, so no two actions share a
  chord by default.
- `ExitReason::Signal` carries the platform signal name (that is what
  `portable-pty` reports), not a number.
- Scrollback search is plain substring matching with optional case folding; regex
  is out of scope for this milestone and documented as such.
- OSC 52 (clipboard read/write from the application) is disabled by default
  because terminal escape sequences are untrusted input (S13).
- The graphical settings panel is a native overlay rendered with the same GPU
  pipeline: no GUI framework, no webview (S5).
