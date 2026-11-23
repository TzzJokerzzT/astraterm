# Security

A terminal is a program launcher. It starts shells and arbitrary commands with
the privileges of the user who runs it. AstraTerm does not sandbox them and does
not claim to; the security work is about not adding new attack surface on top of
that, and about treating everything that arrives from a program as untrusted
input.

## Threat model

| Input | Trust | Why |
|---|---|---|
| The user's keyboard, mouse and explicit commands | trusted | They act with the user's intent |
| Terminal output and escape sequences | **untrusted** | Any program, file or remote host can emit them |
| OSC 52 clipboard requests | **untrusted** | A remote process could read or overwrite the user's clipboard |
| OSC 0/2 titles, bells, colour queries | untrusted | Titles and bells are displayed/reported; colour queries are answered with theme colours only; nothing from them is executed |
| Status bar readings (`/proc/stat`, `/proc/meminfo`) | trusted, kernel-provided | Read locally, parsed with bounds, never logged; no process names or command lines are read |
| Clipboard contents pasted by the user | untrusted | Pasted text may contain control characters; it is sent as input, not evaluated by AstraTerm |
| Configuration file (`config.toml`, theme files) | trusted, user-owned | It selects shell, argument vectors and environment; anyone who can write it can run code as the user |
| SSH sessions | untrusted output, user-authorised command | The remote side runs commands with the user's account on that host |
| Dependencies (`wgpu`, `winit`, `alacritty_terminal`, …) | reviewed, not audited here | They may contain `unsafe`; see the audit workflow |

Non-goals: protecting the user from a command they deliberately run, sandboxing
child processes, privilege separation, or defending against a compromised
compositor/GPU driver.

## Implemented guarantees

Each guarantee is tied to the code that enforces it.

### No `sh -c` for configuration, CLI or profile commands

AstraTerm builds a program plus an argument vector and gives it to the PTY:
`PtySpec { program, args, .. }` (`crates/pty/src/lib.rs`) and
`session_spec` / `resolve_shell` (`crates/session/src/lifecycle.rs`). The CLI
keeps `--command` as a vector (`crates/app/src/cli.rs`, `trailing_var_arg`), and
the application copies `command[0]` to the program and the rest to arguments
(`crates/app/src/app.rs`). A profile's `command` vector is used verbatim.

The only `-c` handling in the product is a validation **warning** when a profile
uses `shell = "sh"` with `args = ["-c"]`, recommending `command` instead
(`crates/config/src/validation.rs`). Tests use `-c` to run fixtures; that is not
product code.

`startup_command` is not executed by AstraTerm: it is written to the PTY as
keystrokes with a trailing carriage return, so the interactive shell interprets
it exactly as if the user had typed it (`SessionManager::create`).

### No secrets are stored

The schema (`crates/config/src/schema.rs`) has no password, token or private-key
fields. Profiles contain a name, a shell/command vector, a working directory,
environment variables, a startup command and a label. SSH credentials stay with
OpenSSH and its agent.

Two fields are free-form user text and could contain a secret if the user puts
one there: `profiles[].env` and `profiles[].startup_command`. That is the user's
choice in a user-owned file; AstraTerm never writes terminal content, clipboard
content or environment variables into the configuration or the state directory.
Workspace metadata stores only tab layout, working directory and profile name
(`PaneMeta` in `crates/workspace/src/snapshot.rs`; its `label` field is present
but always serialized as `None`).

### `$TERM` is never silently custom

The default is `xterm-256color` with `COLORTERM=truecolor`
(`EngineOptions`/`session_spec`, and the PTY builder's fallback env). Values
outside `WIDELY_AVAILABLE_TERMS` in `crates/config/src/validation.rs` are
reported as warnings, so a user can intentionally use a locally installed custom
terminfo entry, but the application never invents or advertises one on its own.

### OSC 52 is disabled by default

`EngineOptions::allow_osc52` defaults to `false`; `session_spec` hard-codes
`allow_osc52: false`, and the engine sets `Term::config.osc52 = Osc52::Disabled`
(`crates/terminal/src/engine.rs`). The compatibility test
`osc52_is_disabled_by_default` feeds an OSC 52 write and asserts no
`ClipboardStore` event appears. There is currently **no configuration field**
that turns it on: `EngineSpec.allow_osc52` exists, but nothing sets it to true.
If it were enabled, the engine would allow copy requests only
(`Osc52::OnlyCopy`), and the application would write through the platform
clipboard service.

### URL opening is restricted and gesture-driven

`crates/platform/src/linux.rs`:

- `is_safe_url` accepts only `http://`, `https://`, `mailto:` and `ftp://`,
  requires a non-empty remainder, and bounds the length to 9–4096 bytes;
- `open_url` runs `xdg-open` with one argument, `stdin`/`stdout`/`stderr` set to
  `null`, and returns an error for anything else.

The application only calls it from an explicit Ctrl+click on a cell, and only
when `terminal.open_urls_on_click = true`
(`Application::handle_mouse_input` → `try_open_url`). Cell text recognition
(`ui::url_at`) accepts only `http://` and `https://`, so the UI path is narrower
than the platform function. There is no automatic link opening, no shell
involvement and no `file://`/`javascript:` handling. A URL is never launched from
an OSC 8 hyperlink because hyperlinks are not styled or activated at all.

### Scrollback and queues are bounded

- `terminal.scrollback_lines` is validated to 100–1,000,000
  (`crates/config/src/validation.rs`) and passed as `scrolling_history`.
- PTY output queue: 64 chunks (`OUTPUT_QUEUE`), read size 32 KiB
  (`READ_CHUNK`), blocking send as backpressure.
- PTY input queue: 1,024 messages (`INPUT_QUEUE`).
- One pump pass parses at most `PUMP_BUDGET_BYTES = 256 KiB` or
  `PUMP_BUDGET_CHUNKS = 512` chunks per session.
- At most 64 sessions by default (`SessionManager::new` sets `max_sessions = 64`);
  beyond that `SessionError::TooManySessions` is returned.

Backpressure is preferred over dropping: the reader blocks instead of discarding
output. Input can still be refused when its queue is full; `Session::write`
counts that in `input_drops` rather than pretending it was delivered.

### No logging of terminal content, clipboard or environment

`tracing` is initialised in `crates/app/src/main.rs` (`RUST_LOG` wins; `--debug`
enables debug logs for the application crates). All call sites were reviewed:

- startup and shutdown, config load/save/watch failures, workspace
  persistence failures, GPU/window creation failures, session spawn/recreate
  failures, invalid `workspace.toml`;
- config validation messages (`issue.describe()`), which can contain configured
  values such as a shell path or profile name;
- at debug level: shortcuts (chord and action), split requests, frame retry
  errors.

No call site logs PTY bytes, terminal snapshots, clipboard contents, pasted text,
environment variables or SSH data. The clipboard service writes and reads via
stdin/stdout of the helper process with logging disabled (`Stdio::null()`), and
never prints the content.

### Child processes are terminated on shutdown

- `PtyHandle::request_shutdown` (`crates/pty/src/lib.rs`) asks the writer thread to
  close the PTY master. Closing it is a *polite* close, not a guarantee: the reader
  thread holds a duplicate of the descriptor and a child may ignore SIGHUP.
- `PtyHandle::kill_hard` escalates with `SIGKILL` sent to the child's **process
  group** (`libc::kill(-pid, SIGKILL)`; the child is a session leader, so its
  process group id equals its pid). That also terminates grandchildren a shell may
  have spawned, which is what prevents orphans. It is the only `unsafe` block in
  the workspace, is a few lines long, and cannot violate memory safety.
- The hard kill is **guarded by the child's identity**: `PtyHandle` records the
  kernel start time from `/proc/<pid>/stat` at spawn, and refuses to signal when
  the pid is gone, is a zombie, or now belongs to a different process. Without that
  guard a reaped pid could be recycled and an unrelated process group killed.
- `PtyHandle::shutdown` is bounded: polite close, one `KILL_GRACE = 400 ms` wait,
  hard kill, short reap. `PtyHandle::drop` performs the same escalation so a dropped
  handle cannot leak a child.
- `SessionManager::shutdown_all` requests a polite close for every session, waits a
  **single shared** 400 ms grace period (not one per session), hard-kills whatever
  is still alive, and reaps the exit statuses. It is called by
  `Application::finish` (quit / last window closed), by
  `ApplicationHandler::exiting`, and from `SessionManager`'s `Drop`.
- `SessionManager::close` removes a session and shuts it down with the same bounded
  path; closing a pane, tab or window goes through it.

A grandchild that deliberately calls `setsid` to leave the terminal's process group
is outside what any terminal can reclaim, exactly as with any other terminal
emulator.

## SSH posture

- AstraTerm uses the **system OpenSSH client**; there is no embedded SSH
  implementation, no key parsing, no password handling and no host-key policy of
  its own. Host-key verification, `known_hosts`, agent and key selection are
  entirely OpenSSH's.
- A profile runs SSH as an argument vector:

  ```toml
  [[profiles]]
  name = "production"
  command = ["ssh", "production"]
  label = "prod"
  ```

  `ssh` is resolved through `PATH` like any child process; `production` is the
  host alias from the user's `~/.ssh/config`. AstraTerm never adds `-o`
  `StrictHostKeyChecking=...` or any other SSH option.
- `profiles[].env` is passed to the child, so `SSH_AUTH_SOCK` can be supplied if
  the session does not inherit it. That is configuration, not credential storage:
  the socket path is not a key.
- The remote `$TERM` is whatever the configured `terminal.term` is, usually
  `xterm-256color`; a warning is emitted for entries remote hosts may lack.

## Unsafe code policy

The workspace contains exactly **two** `unsafe` sites, both with `// SAFETY:`
comments:

- `crates/pty/src/lib.rs` (`PtyHandle::kill_hard`): two `libc::kill(2)` calls that
  escalate to `SIGKILL` when a child ignores the polite close, guarded by the child's
  `/proc` start time so a recycled pid is never signalled.
- `crates/platform/src/system.rs` (`format_clock`): `localtime_r` + `strftime` on a
  stack buffer, so the status-bar clock honours the user's timezone and locale
  instead of reimplementing `TZif` parsing. The format string is length-capped and
  NUL-checked before it reaches C.

There are no `unsafe fn`, `unsafe impl` or `unsafe trait` declarations. New
`unsafe` code must be justified in review, kept minimal, and documented with a
`// SAFETY:` comment.

Dependencies may use `unsafe` internally (wgpu, winit, alacritty_terminal,
portable-pty, crossbeam-channel). That code is outside this workspace's policy
and is covered by the dependency review below, not by an FFI boundary of our own.

## Dependencies and audit workflow

Direct dependencies are deliberately few and each one replaces a large amount of
security-sensitive code that would otherwise be ours: `winit` (windowing),
`wgpu` + `glyphon` + `cosmic-text` (GPU and text), `alacritty_terminal` (VT
parser, grid, scrollback, selection), `portable-pty` (PTY and process spawning),
`serde`/`toml`/`serde_ignored` (configuration), `notify` (file watching),
`crossbeam-channel` (bounded queues), `clap` (CLI), `tracing` (logging),
`thiserror`/`anyhow` (errors), `bytemuck`/`pollster` (GPU plumbing).

Workflow:

- **Audit.** `cargo audit` is *not* run in CI: the repository has no CI
  configuration. Run it manually after dependency updates:

  ```sh
  cargo install cargo-audit   # once
  cargo audit
  ```

  `Cargo.lock` is part of the repository, so audits and builds are reproducible.
- **Version updates.** Prefer `cargo update -p <crate>` with a passing test run;
  do not bump major versions casually, especially in the wgpu/winit/glyphon stack
  where API changes are large.
- **License review.** The workspace declares `license = "MIT"`. Dependency
  licenses are reviewed by hand when a dependency is added or updated; no
  automated license checker is configured.
- **Adding a dependency.** Justify it against an existing dependency, check its
  maintenance status and license, and keep it behind the crate that needs it
  (`crates/*/Cargo.toml`), not in the root unless it is shared.

## Files and permissions

| Path | Content | Notes |
|---|---|---|
| `~/.config/astraterm/config.toml` | the whole configuration, including shell/command vectors | User-owned. Written atomically (temp file + `fsync` + rename). No explicit mode is set, so it inherits the user's umask; it is not expected to contain secrets |
| `~/.config/astraterm/themes/*.toml` | theme palettes | Read-only input; parsed as TOML, no code execution |
| `~/.local/state/astraterm/workspace.toml` | tab layout, cwd, profile names (the `label` field is written as `None`) | User-owned; lets a same-user process learn directories, nothing more |
| `/etc/astraterm/config.toml` | packaged defaults and documentation | Installed by the distribution package as a backup file, **never read by the application**; only `--config` or the per-user file can supply configuration |

AstraTerm installs no setuid binary, requests no capabilities and runs no daemon.
`install.sh` writes only under `$XDG_BIN_HOME` / `$XDG_DATA_HOME` and never
touches existing configuration. The PKGBUILD installs the binary, desktop entry,
icon and `/etc` documentation file.

Configuration is trusted precisely because it is user-owned: an attacker who can
write it can already run programs as the user, so the atomic writer and conflict
detection in `ConfigService` are about preventing accidental corruption and lost
edits, not about a privilege boundary.

## Reporting a vulnerability

Report security issues privately to the maintainers at
`maintainers@astraterm.invalid` (placeholder — replace this address with a real
maintainer contact before publishing). Include the AstraTerm version
(`astraterm --version`), the distribution, the session type (Wayland/X11), and a
reproduction. Please do not open a public issue for a vulnerability before a fix
is available.
