# Grid-anchored glyph placement and PTY winsize coherence

Measured diagnosis: the renderer positions glyphs from the shaper's advances while the grid
positions backgrounds, so any glyph whose advance differs from `columns * cell_width` shifts the
rest of its row; and a session's PTY winsize can stay at the pre-map value.

## Specs

S1. The reported symptoms, verbatim from the user: «si uso la terminal foot el tema P10K se ve
complementamente diferente a como se ve a como se ve en astraterm y tambien si abro neovim el
lualine esta alineado, pero en astraterm se no. Parece que las cosas estuvieran desalineadas.»

S2. A painted text segment's origin must be `origin_x + column * cell_width`, where `column` is the
grid column of its first cell. Rows must be split into segments at every cell whose painted advance
is not guaranteed to equal `columns * cell_width`, so a wide or fallback glyph can never push the
cells after it. Measured facts that motivate the split: `U+23F1` (⏱) reserves 1 model column and
paints 2.076 cells; `U+1F4C1` (📁) reserves 2 and paints 2.076; `U+4F60` (你) reserves 2 and paints
1.667.

S3. The filler space that `row_text_item` pushes after a wide cell must be removed: the anchored
segment of the wide glyph already occupies its two columns.

S4. An anchored segment must be clipped to the cell rectangle of the columns it owns plus one
cell of slack, so an over-wide fallback glyph is not cut in half while the bleed stays bounded to
the single neighbouring cell.

S5. A session must never paint grid `cols x rows` while its PTY winsize says something else.
Sessions are created at the grid derived from the current window rectangle, not `80, 24`, and a
failed PTY resize must be observable rather than silently discarded.

S6. No regression for the common row: a row made only of 1-column cells in the primary font must
still produce one text item with one span per style run, exactly as before.

S7. A segment boundary must never fall inside a grapheme cluster. A variation selector (U+FE0E,
U+FE0F), a zero-width joiner sequence, a skin-tone modifier (U+1F3FB-U+1F3FF), a keycap sequence, a
tag sequence, a combining mark, or a pair of regional indicators belongs to the anchored segment of
its base character, so the shaper still receives the cluster whole. Valid input such as `❤️`,
`👍🏽`, `👨‍👩‍👧`, `1️⃣` and `🇪🇸` must not be split across items.

S8. The decision to anchor a cell must come from measurement, not from a hardcoded list of
codepoints: a cluster is anchored when its shaped advance disagrees with the columns the model
reserved for it. The emoji/symbol whitelist does not cover a fallback script, so `क` still drifts
at 1.280 cells per glyph while the grid reserves one.

## Tasks

T1 — S2, S3, S4 — renderer: grid-anchored segments, segment ids in the text cache, per-segment clip — commit 1
T2 — S5 — session/app: initial winsize from the real rectangle and observable PTY resize — commit 2
T3 — S6 — differential check on plain rows, docs in `docs/TERMINAL_COMPATIBILITY.md` — commit 3
T4 — S7 — one anchored segment per grapheme cluster; the whitelist may only decide the base — commit 4
T5 — S8 — replace the whitelist with a measured split driven by the shaped advance — commit 5

## Log

L1. «Hay algo que quiero que arregles y es lo siguiente si uso la terminal foot el tema P10K se ve
complementamente diferente a como se ve a como se ve en astraterm y tambien si abro neovim el
lualine esta alineado, pero en astraterm se no. Parece que las cosas estuvieran desalineadas. Se que
no es problema de tema ni de lualine. Podemos revisar que esta pasando o por que esta pasando eso,
puedes ejecutar las terminales tomar capturas para tener mas contexto»

L2. Diagnosis measured with two temporary Rust probes (font resolution through cosmic-text and model
widths through the terminal engine), deleted afterwards; the repo was clean at that point.
Ruled out: PTY resize convergence (WINCH 33x114 -> 62x227 reached the grid), and font resolution
(every nerd/powerline/box/nerd-plane codepoint of p10k and lualine resolves to JetBrainsMono Nerd
Font with `advance == cell_width == 10.800`). Confirmed: emoji-font fallbacks and wide glyphs paint
at an advance the grid did not reserve; the PTY starts at the pre-map size (measured `33 114` for an
already `227x62` window; `app.rs:383,462` create sessions with `80, 24`) and `manager.rs:120`
discards the PTY resize result behind a guard that only compares engine state; `font.fallbacks` is
dead configuration never passed to the shaper.
Not defects, but they change how the prompt looks: `[window] opacity = 0.7` against foot's
`alpha=0.99`, and font size units (astraterm `size = 18.0` in pixels vs foot `size=9` in points).

L3. Scope decision by the user: «Arreglo dirigido (recomendado)» — anchor the painted advance to the
model's column width for emoji/wide glyphs and synchronize the initial PTY size. The per-cell
placement redesign and the emoji-only and PTY-only alternatives were offered and not chosen.

L4. Provider refuter finding R3-001 (critical, introduced): deciding per cell splits multi-codepoint
grapheme clusters, so `❤️`, `👍🏽` and ZWJ sequences reach the shaper broken. Independent
verifier confirmed S2-S6 otherwise and measured the residual S8 gap (devanagari `क` at 1.280x) and
the S4 wording mismatch. User decisions, verbatim: «1. ¿Cómo cierro R3-001 y el hueco de S2? —
Medir y partir (recomendado)» and «2. S4: ¿alineo la spec con el código o el código con la spec? —
Ajustar el texto de la spec». S4 therefore now documents the one-cell slack, S7 records the cluster
invariant, and S8 records that the anchoring decision must be measured rather than listed.
