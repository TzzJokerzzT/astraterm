# 1.3.0: the remaining backlog, and honest idle control

Closes the rest of the backlog that has functional value, plus the one configuration key that never did anything.

## Specs

S1. **B5 — shell integration (OSC 133).** The engine records the prompt and command marks a shell
emits (`A` prompt start, `B` prompt end, `C` command start, `D` command finished with an optional
exit code) and exposes them in the snapshot; the app can jump to the previous and next prompt, and
the status bar shows whether a command is running and its last exit code. OSC 133 that is never
sent leaves everything exactly as it is today.

S2. **B8 — block selection and click granularity.** A rectangular selection kind alongside the
existing linear one; double click selects a word using the configured `word_delimiters`, triple
click selects the line. Remaining behaviour of the linear selection, the clipboard and the
copy-on-select gesture is unchanged.

S3. **B9 — ligatures that stay on the grid.** With `font.ligatures` enabled, a ligature that spans
more than one cell is shaped as one run and still lands on its cells, instead of shifting the rest
of the row. With ligatures off, nothing changes.

S4. **B11 — the IME preedit is drawn.** The composition in progress is painted at the cursor and
reflects replacements and deletions; committing clears it. No preedit means no visual change.

S5. **B13 — sixel colour fidelity.** The HLS palette forms used by sixel are decoded with the same
fidelity as the RGB and grayscale forms; an unrepresentable palette falls back without panicking.

S6. **B14 — a configurable bell.** `[terminal] bell` chooses `"none"`, `"visual"` (a border flash) or
`"audible"`; the default keeps today's behaviour (BEL becomes a status message).

S7. **B16 — damaged-region repaint.** Only the rows that changed since the last frame are rebuilt
and drawn; a frame with no damage draws nothing. What is on screen must be identical to a full
repaint, which is what the tests must prove.

S8. **B21 — per-cell payload cost.** The OSC 8 destination is stored once per row/run instead of
cloned into every cell, and the renderer's fast path for one-column ASCII consults the same
continuation decision as the measured path instead of duplicating it.

S9. **B22 — caches and retained memory.** `RowCache` evicts its oldest entry instead of clearing
everything at its bound, the cluster and row ceilings are as high as a real screen needs, and with
`copy_on_select` disabled no selection text is built at all.

S10. **Idle frame budget.** `[performance] max_fps_when_idle` is honoured: it caps idle redraws per
second, `0` means no cap, and it never delays a redraw caused by output, input or a resize.

## Tasks, in waves over disjoint surfaces

T1 — S3, S7, S8 (renderer half), S9 (renderer half) — `crates/renderer/` — wave 1
T2 — S1 (engine half), S2 (engine half), S5, S8 (terminal half) — `crates/terminal/` — wave 1
T3 — S1 (UI half), S2 (UI half), S4, S6 (UI half), S9 (app half), S10 — `crates/app/` — wave 2
T4 — S6 (config half) — `crates/config/`, `crates/settings-ui/` — wave 2
T5 — release 1.3.0 — version, changelog, docs

Wave 2 starts only when wave 1 has landed, because every app-side task reads something wave 1
produced.

## Log

L1. «Implementemos B5, B8, B9, B11, B13, B14, B16, B21, B22 y arreglar max_fps_when_idle. Luego
subamos la version 1.3.0 que seria el release final. Para mi ya que la pienso usar en otros equipos.»

L2. Scope warning given before starting: of these ten, only B16, B21, B22 and the idle budget change
performance; the rest are features. The terminal will be more complete, not cheaper than foot.

L3. Waves decided because nearly every item touches `crates/app` or `crates/renderer`; two writers in
one crate is the collision that already cost a documentation rewrite earlier in this project.

L4. T1 landed on `crates/renderer/` (S3, S7, S8 renderer half, S9 renderer half). Decisions and
numbers, all measured on this machine.

S3 — measured first: the shipped font (`JetBrainsMono Nerd Font`) substitutes `->`, `=>`, `==`,
`<--` and the rest *per cell*, each glyph advancing exactly one cell, so no installed font can
reproduce the defect. The fixture `src/testdata/grid_ligature.ttf` (2.5 kB, generated with
fontTools, recipe in `src/testdata/README.md`) adds one substitution whose advance is 1000 units
where its two cells are 1200: shaped as one run, `x` lands 13.0 px and 20.8 px where the grid wants
15.6 and 23.4. The existing grid-anchoring rule handles it one level up and needed no new
mechanism: `paint` flags a text item as a grid run when its text is one ASCII byte per column
(`TextItem::on_grid`), and `text::ligature_breaks` splits such a run at every glyph whose shaped
advance disagrees with the columns it covers, placing each piece at `position + offset * cell_width`
with the same `column_clip` an anchored cluster gets. The substitution is not broken: the piece that
starts with it still shapes it as one glyph. Fonts whose substitutions already fill their columns,
and every non-ASCII run, are shaped exactly as before, and with `font.ligatures` off nothing runs at
all. Proven by `paint::tests::a_ligature_that_spans_cells_keeps_the_cells_after_it_on_the_grid`
(RED at 13.0/20.8, green at 15.6/23.4 with the ligature still one glyph over two columns),
`paint::tests::with_ligatures_off_the_row_is_shaped_exactly_as_before`, and four pure splitter tests
in `text::tests` for the shapes the fixture cannot make.

S7 — the renderer receives a composed `Scene`, never a `Snapshot`, so the damage is derived from the
scene it already holds: `damage::Bands` digests every instruction into the strip of one text row it
touches, `damage::plan` names the strips whose digest changed, and the frame is painted into a
retained canvas strip by strip before being copied into the frame. `Snapshot::damaged_rows` was not
used and nothing is missing from the snapshot for this: it carries cell damage, while the scene also
carries the cursor, the selection, the search highlight and the image placement, all of which move
without a cell changing, and all four are covered by tests in `damage::tests`. Measured on a
480x135-cell screen (3744x2025 px, 135 items, 64 321 characters): an unchanged frame now prepares
0 areas, uploads nothing and presents nothing; one changed line damages 2 of 135 strips (rows are
offset 1 px from the absolute strip grid, so a row can straddle one boundary), prepares 3 of 135
items and 1005 of 64 321 characters (1.6 %), against every item and every character before. The
tracking itself costs 87 µs per frame against 2.2 ms of scene composition. An unchanged full repaint
is proved equal to the damaged sequence by
`damage::tests::a_damaged_repaint_holds_exactly_what_a_full_repaint_holds` (eight edits: text,
scroll, cursor blink on and off, selection, highlight, an image arriving and moving), and
`the_overlay_layer_is_repainted_with_the_mosaic_band` for the second layer.

S8 renderer half — one continuation decision, `paint::continues_cluster`, is now consulted by
`cluster_at` and by `plain_cell`, so the spacer/hidden/joiner/regional-indicator order exists once.
`paint::tests::both_row_walks_continue_a_cluster_in_the_same_place` runs ten awkward rows through
both walks with the same font and asserts the items are identical (the gate, not the font, chooses
the path).

The GPU side of S7 was checked on a real device outside the repository (Intel UHD 630, Vulkan,
headless wgpu probe driving the same public pipelines the renderer uses): a canvas cleared red, one
strip of rows 2..4 cleared to blue by a replace-blended quad under a scissor with a green quad drawn
into it, and the canvas copied into the frame. Read back: rows 0 and 5 stayed red, the strip came out
blue with the green quad inside it, and canvas and frame were byte-for-byte identical — which is the
pattern the strip repaint and the blit rely on.

S9 renderer half — `RowCache` evicts by insertion order instead of clearing at its bound, and the
bound is 32 768: a 4K screen at 8x16 device pixels is 480x135 cells, and a screen of nothing but
wide clusters puts every second cell in a segment of its own, 240 per row and 32 400 items. A
reshaped item keeps its place in the queue, so a clock cannot push its neighbours out.
`text::tests::the_row_cache_evicts_the_oldest_instead_of_wiping_itself` (RED on the old
clear-at-bound behaviour), `the_row_bound_holds_a_screen_of_wide_clusters`, and the existing
`the_production_bound_covers_the_cells_of_a_4k_screen` for the cluster cache. Splitting an item into
pieces exposed one latent hazard and closed it: the cache key is content, so an item that only moved
kept its buffer, and a piece that cached absolute coordinates would have been painted where the pane
used to be. Pieces now store offsets from the item and take positions and clips from the scene, with
`text::tests::a_moved_item_keeps_its_shaped_buffer_and_its_new_position` pinning it (RED with a
cached position).

Suite: `cargo test --workspace` 701 passed, 0 failed; `cargo clippy --workspace --all-targets` 0
warnings. `crates/terminal` was being edited by T2 throughout; no failure there was attributable to
this task.
