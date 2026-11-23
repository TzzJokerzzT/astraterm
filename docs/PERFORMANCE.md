# Performance

AstraTerm is designed to sleep when nothing happens and to bound every queue it
owns. This document records the design rules, the measurements taken on the
development machine, a reproducible profiling procedure, and the costs that are
still on the table.

Nothing here is enforced by a benchmark in CI: the repository contains no
benchmark harness or performance test. The numbers below are measurements, not
guarantees; reproduce them with the commands in each section.

## Design rules

| Rule | Implementation |
|---|---|
| **Event-driven redraw.** No frame loop, no polling. | PTY threads wake the event loop through an `EventLoopProxy`; a redraw happens only after output, input, resize, or a timer (`crates/app/src/app.rs`, `user_event`, `about_to_wait`). The idle control flow is `ControlFlow::Wait`. |
| **One timer, gated.** The cursor blink must not spin at frame rate. | `blink_due` (`crates/app/src/ui.rs`) fires at most once per `CURSOR_BLINK_PERIOD = 530 ms`, i.e. two toggles per full blink cycle, and never bursts after a stall. Blinking is off when there is no focused window, when an overlay is open, when `cursor_blink = false`, or when `low_power_mode = true`. |
| **Damage-tracked snapshots.** | `TerminalEngine::snapshot` reports `Snapshot::damaged_rows`, derived from `Term::damage()` and reset after collection (`crates/terminal/src/engine.rs`). |
| **Shaped-row cache.** | `text::RowCache` keeps one shaped `cosmic-text` buffer per `TextItem::id` and re-shapes only when the content hash or the font style key changes (`crates/renderer/src/text.rs`). Cache size is capped at `MAX_CACHED_ITEMS = 4096`, then cleared wholesale. |
| **Per-session output budget.** | `SessionManager::pump` gives each session up to `PUMP_BUDGET_BYTES = 256 KiB` or `PUMP_BUDGET_CHUNKS = 512` chunks per pass (`crates/session/src/manager.rs`). A flood is spread over several passes, so input and other panes stay responsive. |
| **Bounded PTY queues with backpressure.** | Output queue 64 chunks of at most 32 KiB (`READ_CHUNK`), input queue 1,024 messages. The reader's blocking send is the backpressure point: the child blocks instead of output being dropped (`crates/pty/src/lib.rs`). |
| **Bounded sessions.** | `SessionManager` defaults to `max_sessions = 64` and reports `SessionError::TooManySessions`. |
| **Bounded scrollback.** | `terminal.scrollback_lines` is validated to 100–1,000,000 and passed to the core as `scrolling_history`. |
| **One quad draw call.** | Every non-glyph rectangle in a frame is an instance of one pipeline, drawn in one instanced call (`crates/renderer/src/quad.rs`). |

Structural tests that pin these rules:

- `crates/pty`: `output_queue_is_bounded_and_never_loses_bytes` (a 400 KB burst
  arrives almost completely; the queue never drops chunks), `shutdown_terminates_a_long_running_child`.
- `crates/session`: `pump_budget_limits_work_per_pass` (a 2 MB flood must be split
  across pump passes; a pass may overshoot `PUMP_BUDGET_BYTES` by at most one read
  chunk, `astraterm_pty::READ_CHUNK_BYTES`).
- `crates/app/src/ui.rs`: `blink_gate_fires_twice_per_period`,
  `blink_gate_does_not_burst_after_a_long_stall`.
- `crates/renderer/src/text.rs`: `shaping_populates_the_cache_and_is_reused`,
  `changed_content_reshapes_the_row`.

## Timers

Everything is event-driven except two deliberate timers, both of which stop when
they are not needed:

| Timer | Period | Runs when |
|---|---|---|
| Cursor blink | 530 ms | the window is focused, blinking is enabled and low-power mode is off |
| Status bar readings | 1 s | `appearance.show_status_bar` is enabled |

With the status bar hidden there is no sampling at all: `/proc/stat` and
`/proc/meminfo` are read once per second only while the bar is on screen, and each
reading requests exactly one redraw.

## Measured numbers (development machine)

Machine: Intel UHD 630 (integrated GPU), Vulkan backend, Wayland session under
Hyprland, window 2494×1396. Both build profiles were measured: `dev`
(opt-level 1, dependencies opt-level 2) and `release`.

| Measurement | Debug | Release |
|---|---|---|
| Idle CPU, one pane, 10 s sample | **1 %** | **2 %** |
| Idle CPU before the blink-gate fix | **19 %** | not measured |
| Resident memory, one pane | **≈ 129 MB** | **≈ 129 MB** |

The ~129 MB figure is the whole process: wgpu/Mesa driver state, the glyphon
atlas, font database, and one terminal grid with the default 10,000-line
scrollback. It is not a per-pane cost, and it is dominated by the graphics driver
rather than by AstraTerm's own allocations.

The 1–2 % idle figure is dominated by the cursor blink: each blink toggles the
cursor and redraws one frame, i.e. two frames per 530 ms. Setting
`cursor_blink = false` (or `low_power_mode = true`) removes it entirely.

The 19 % → 1 % change is attributed to the blink gate. Without an explicit
schedule, a blink re-armed from the event loop turns idle time into frame-rate
work; `blink_due` computes the next deadline instead, and `about_to_wait` sleeps
until it. With the gate, the only idle work is one redraw every 530 ms while the
cursor blinks, or none at all when blinking is off.

These numbers are only comparable if you use the same build profile, window
size, compositor and GPU. A release build and a software rasteriser will differ.

### How to reproduce

Idle CPU from `/proc/<pid>/stat` tick sampling (`CLK_TCK` is normally 100):

```sh
pid=$(pgrep -n astraterm)
hz=$(getconf CLK_TCK)
ticks() { awk '{print $14 + $15}' "/proc/$pid/stat"; }

t0=$(ticks); sleep 10; t1=$(ticks)
awk -v a="$t0" -v b="$t1" -v hz="$hz" \
  'BEGIN { printf "idle CPU: %.1f%%\n", (b - a) * 100 / hz / 10 }'

grep -E 'VmRSS|VmHWM' "/proc/$pid/status"
```

Notes: fields 14 and 15 are `utime` and `stime` in clock ticks; the command name
in `/proc/<pid>/stat` is `astraterm`, which contains no spaces, so the field
numbers are stable. RSS is read once, so take several samples to distinguish
peak from steady state. `ps -o %cpu=,rss= -p "$pid"` gives a lifetime average,
which is not the same as a 10-second sample.

For GPU and memory pressure, run `intel_gpu_top` (or your vendor's tool) beside
the terminal and sample during the scenario. Frame pacing is not instrumented;
the renderer uses `PresentMode::Fifo` and `desired_maximum_frame_latency = 2`,
so record missed-frame impressions together with GPU busy percentage rather than
a frame time.

## Profiling scenarios

All of these are **manual** today. There is no automated performance test; the
scenario table is a procedure, not a test suite.

| Scenario | How to run | Record |
|---|---|---|
| Idle, one shell | Start `astraterm`, do not type, sample 10 s | CPU ticks (command above), RSS, GPU busy |
| Idle, 8 panes | Split to 8 panes (`Ctrl+Shift+D` / `Ctrl+Shift+E`), do not type | Same. Only the focused pane draws a cursor; verify idle CPU does not scale with pane count |
| Fast output | In one pane: `head -c 67108864 /dev/zero \| tr '\0' 'y'` | Wall time and bytes/s, CPU during the flood, RSS, whether typing in another pane still echoes, whether the flood pane finishes |
| Neovim, large file | `seq 1 200000 > /tmp/big.txt`, `nvim /tmp/big.txt`, then `Ctrl+D`, `Ctrl+U`, `G`, `gg`, `/pattern` | CPU spikes per operation, RSS, smoothness of scrolling, search response |
| Large scrollback | Set `scrollback_lines = 500000`, restart, run `seq 1 1000000`, scroll to top (`Ctrl+Shift+Home`) and back | RSS before/after filling, scroll responsiveness, memory after the buffer is full |
| Repeated resize | Drag the window edge and a split divider repeatedly (50+ times); also toggle zoom | CPU, RSS, PTY resize attempts, visual artefacts |
| HiDPI | Run the compositor at scale 2 for the output, restart AstraTerm (scale is read at startup and on `ScaleFactorChanged`) | Cell metrics, RSS, CPU; note that a scale change clears the shaped-row cache |
| GPU unavailable | Make the driver unavailable or force software: `VK_ICD_FILENAMES=/nonexistent LIBGL_ALWAYS_SOFTWARE=1 astraterm` | If no adapter is found, `Renderer::new` returns an error, the application logs "could not create the window; the GPU backend may be unavailable" and exits. There is no CPU rendering path of our own; if the driver stack exposes a software adapter, wgpu may still succeed with it |
| Transparency | Set `[window] opacity = 0.85`, restart (the surface is created translucent) | GPU blend cost, RSS, visual correctness |
| SSH | `ssh <host>` in a pane, or a profile with `command = ["ssh", "<host>"]`; run `dd if=/dev/zero bs=1M count=64 \| cat > /dev/null` and a full-screen TUI remotely | Input latency, throughput, CPU, responsiveness of other panes |

Metrics and how to obtain them:

- **CPU** — `/proc/<pid>/stat` sampling as above, or `perf stat -p "$pid" sleep 10`
  for cycles/instructions/context switches.
- **RSS / peak RSS** — `/proc/<pid>/status` (`VmRSS`, `VmHWM`).
- **Frame pacing** — `PresentMode::Fifo` means the compositor paces frames; watch
  `intel_gpu_top` and record visible stutter. There is no frame-time counter.
- **Input latency** — no instrumentation; record the subjective result plus
  whether keystrokes are dropped (`Session::input_drops` exists but is not shown
  in the UI).
- **Throughput** — bytes emitted divided by wall time until the prompt returns;
  `time` around the producer is a lower bound if the terminal falls behind.

## Known costs and limits

- **Full-frame composition and redraw on every damage event.** `Snapshot::damaged_rows`
  is computed but has no renderer consumer yet (`grep -rn damaged_rows crates/`
  finds only the terminal crate and its tests). Every redraw calls
  `WindowUi::compose`, which snapshots every visible pane and rebuilds all quads
  and text items for the whole window. There is no scissored/partial rendering.
  The row cache saves shaping, not composition.
- **A snapshot per visible pane per frame.** `WindowUi::compose` calls
  `Session::snapshot()` for each visible pane, and `TerminalEngine::snapshot`
  walks the renderable grid and collects damage every time, even if nothing
  changed. Idle cost is therefore tied to blink-driven redraws, not to damage.
- **One `glyphon` text area per row.** Each visible row (plus chrome rows) is a
  `TextArea`; `TextRenderer::prepare` runs for all of them each frame. Shaping is
  cached; atlas upload/prepare is not.
- **Linearisation allocates.** On sRGB or float surfaces, `Renderer::render`
  clones the scene and converts every colour (`Scene::linearize`) per frame. On
  the preferred plain UNORM formats this cost does not exist.
- **Quad buffer growth.** The instance buffer starts at 2,048 instances and grows
  to the next power of two; a scene larger than that reallocates once at the
  size boundary.
- **Text cache cap.** More than 4,096 distinct text items in one scene clears the
  whole shaped-row cache on the next update.
- **Memory scales with scrollback.** The core stores history as
  `scrollback_lines × columns` cells, each with a character, colours and flags.
  The default is 10,000 lines; 500,000 lines is roughly fifty times the history
  memory. Lower it on memory-constrained systems.
- **Bounded but non-trivial PTY buffers.** Each session can hold up to 64 output
  chunks (≈ 2 MiB at the 32 KiB read size) plus up to 1,024 queued input
  messages. With the default 64-session cap, worst-case buffering is bounded but
  noticeable.
- **No backpressure feedback in the UI.** A full input queue increments
  `Session::input_drops`; nothing surfaces it today, so a dropped keystroke is not
  visible to the user.
- **No idle FPS cap.** `performance.max_fps_when_idle` is accepted and validated
  but there is no idle redraw loop to cap; redraws are driven by events and the
  blink timer.
- **Search cost is linear in the visible screen.** The interactive search scans
  every visible row and character per keystroke (`find_in_snapshot`); the
  buffer-wide API (`crates/terminal/src/search.rs`) scans line by line and is not
  used by the overlay today.
