# Cursor shape precedence: the program decides, the configuration defaults

An editor that switches its cursor per mode must be able to do so; today the terminal's own setting hides it.

## Specs

S1. The shape a program requests with `DECSCUSR` (`CSI Ps SP q`) is the shape painted, for every pane and for the floating terminal. A program that asks for a beam in insert mode and a block in normal mode gets exactly that.

S2. `[appearance] cursor` and `[appearance] cursor_blink` become the **default**: they decide the shape when no program has asked for one, including at startup before any program writes to the terminal. Their accepted values, names and documented effect stay, and the effect text now says it is a default.

S3. Nothing else about the cursor changes: its position, the block's cell coverage, the hollow style's outline, the search and selection highlight interaction, and the cursor's suppression when the program hides it (`DECTCEM`) behave as they do today.

S4. The renderer no longer overrides the shape the engine reports. The override path is removed rather than left disabled, so there is one source of truth and the next change cannot reintroduce the precedence by accident.

S5. A program that asks for a shape the configuration cannot express still works: the engine's full set (block, beam, underline, hollow block, hidden) is carried through, not just the two the old option allowed.

## Tasks

T1 — S1, S2, S3, S5 — carry the full configured style into the engine and drop the override — commit 1
T2 — S4 — remove the override plumbing from `PaintOptions` and its call sites — commit 1

## Log

L1. «Uso neovim como editor y resulta que el shape del cursor se antepone al de neovim y me confunde… hay una forma de que neovim use su propio cursor sin tener que heredar el de la terminal.»

L2. Measured cause: the engine already seeds `default_cursor_style` from the configuration and already maps `CursorShape` to `CursorVisual` (`crates/terminal/src/engine.rs:201-205`, `:414-418`), so a program's `DECSCUSR` was recorded correctly. The app then overrode it unconditionally: `crates/app/src/ui.rs:1121` and `:1226` pass `cursor_override: Some(self.cursor_style())`, and `crates/renderer/src/paint.rs:1058` resolves `options.cursor_override.unwrap_or(cursor.shape)`. The override could therefore only ever hide the program.

L3. User decision: the program always wins and the configuration is a default. The alternative — keeping a forced mode behind an extra key — was offered and not chosen, because it would add configuration surface for a case nobody asked for.

L4. Carrying the style (S2, S5): `EngineOptions` dropped `hollow_cursor: bool` for
`cursor_shape: CursorShape` plus `cursor_blink: bool`, and `TerminalEngine::new`
seeds `default_cursor_style` from both. `CursorShape` is re-exported by
`astraterm-terminal`, so no new enum and no new dependency. The configuration
values map one to one: `block`→`Block`, `beam`→`Beam`, `underline`→`Underline`,
`hollow`→`HollowBlock`; the map is an exhaustive `match` in
`crates/session/src/lifecycle.rs`, so a future configuration value without an
engine equivalent fails to compile instead of silently painting a block. Every
value the configuration accepts has an equivalent; `Hidden` stays reachable only
through `DECSCUSR`.

L5. Where the configuration meets the engine: `EngineSpec` gained the same two
fields and `build_spec` fills them from `config.appearance`, because `EngineSpec`
and `SessionSpec::engine_options` are the only configuration-to-engine bridge. The
two session integration tests that build `EngineSpec` literally
(`shutdown_no_orphan.rs`, `output_accounting.rs`) gained the two fields with the
old default (`Block`, blinking) — the smallest change that keeps them compiling
without pretending to test the cursor.

L6. Why `cursor` is no longer "applies live": no public setter updates a live
engine's `default_cursor_style`, and the app never owns an engine (`SessionManager`
does), so a reload reaches the panes created after it. The docs say exactly that.
`crates/settings-ui/src/fields.rs` still does not list `Field::CursorStyle` under
`requires_restart()`; that file was outside the edit surface, so the mismatch is
reported rather than fixed here.

L7. Program ids, for the record: the emulator maps `DECSCUSR 0` to "no program
style", which falls back to the configured default; `1`/`2` are block, `3`/`4`
underline, `5`/`6` beam, odd ids blinking and even ids steady. The blink half only
seeds the engine's style; the painted 530 ms phase still follows
`appearance.cursor_blink` through `WindowUi::should_blink`, and no other cursor
geometry changed.

L8. Override removal (S4): `PaintOptions::cursor_override`
(`crates/renderer/src/paint.rs`), its two `compose` call sites in
`crates/app/src/ui.rs`, `WindowUi::cursor_style()` and the `unwrap_or` resolution
in `paint_cursor` are gone, so `cursor.shape` from the snapshot is the only input
to the painted shape.
