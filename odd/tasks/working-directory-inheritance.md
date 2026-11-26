# Working directory inheritance and a floating terminal that can hold panes

The floating terminal and a new tab must start where the user is, and the floating terminal must be
able to hold more than one pane.

## Specs

S1. Floating terminal inherits the directory it is opened from, verbatim: «cuando abra la terminal
flotante herede la ruta desde la cual se esta abriendo, decir que si estoy
/home/alex_buelvas/project la terminal flotante tambien abra desde esa ruta».

S2. The floating terminal can hold more than one pane and more than one tab, verbatim: «que pueda en
la terminal flotante abrir otros paneles o pestañas».

S3. A new tab inherits the directory of the pane it is opened from, verbatim: «cuando habra otra
pestaña en la terminal herede la ruta del padre».

S4. Resolution rule for the two creation paths of S1 and S3: the parent pane's **live** working
directory wins over the `[terminal] working_directory` policy, and an explicit
`WorkingDirectory::Path` still wins over both. The live directory is `Session::cwd`
(`crates/session/src/manager.rs`, refreshed from `/proc/<pid>/cwd`), not `Pane::cwd`, which is
written once at creation and never refreshed. Splits already read the live value; they must keep
doing so.

S5. Nothing about the mosaic layout changes: the floating terminal stays outside the split tree,
never participates in reflow, and its panes are not persisted as part of the mosaic workspace until
S2 requires it.

## Tasks

T1 — S1, S4 — floating terminal resolves the focused pane's live cwd — commit 9
T2 — S3, S4 — a new tab resolves the focused pane's live cwd instead of `None` — commit 9
T3 — S2 — the floating terminal owns its own tab set with splits — commit 10

## Log

L1. «Hay 2 cosas que quiero implementar tambien 1 cuando abra la terminal flotante herede la ruta
desde la cual se esta abriendo, decir que si estoy /home/alex_buelvas/project la terminal flotante
tambien abra desde esa ruta y tambien que pueda en la terminal flotante abrir otros paneles o
pestañas. Y cuando habra otra pestaña en la terminal herede la ruta del padre»

L2. Mapped before writing anything. `open_floating` already reads the focused mosaic pane's `cwd`
(`app.rs:1462-1465`) but that value is dead: `resolve_directory` returns `Some` for every policy, so
the trailing `.or(cwd)` in `session_spec` (`lifecycle.rs:112-117`) never runs and the default policy
`home` wins. It also has no live-cwd fallback, unlike `split_pane` (`app.rs:1357-1361`). `open_tab`
passes `None` for the directory (`app.rs:1309-1310`), so a new tab can never inherit. The floating
terminal is a single `Floating { pane, session, visible }` (`ui.rs:156-165`) painted at one rect with
one scene-id base `0x20_0000`; `SplitPane` and `NewTab` are explicitly routed to the mosaic even
while the float has focus (`app.rs:1009-1016`, `:985`). Tests that pin current behaviour:
`ui.rs:2394` `floating_tests`, `ui.rs:2444` `upper_layer_tests`, `chrome.rs:293`/`313`,
`keys.rs:801`, `lifecycle.rs:235`/`245`. No test covers `open_floating` or `open_tab` cwd.

L3. T1 and T2 landed as `session_spec_inheriting` plus `inherited_directory` in
`crates/session/src/lifecycle.rs`, one place for the S4 precedence (explicit `Path` > parent's live
directory > policy), with `open_floating` and `open_tab` passing the focused pane's live cwd.
`session_spec` keeps its previous semantics byte for byte, proven by a 105-case differential over
7 policies × 3 directories × 5 profile shapes on copies of HEAD. RED was observed before the
change: the two new tests resolved to `$HOME` instead of the parent's directory.

Two decisions recorded for a later reader: the rule also applies to a *profile* policy, so a
profile setting `working_directory = "home"` no longer forces home for these two paths; and a split
under the default `home` policy still starts in `$HOME` while a new tab inherits, because S4 scoped
the change to the floating terminal and to tabs and asked to keep splits as they are. Making that
uniform is a one-line change if the asymmetry ever bothers anyone.
