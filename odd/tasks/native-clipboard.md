# Native clipboard with image paste

Replace the `wl-copy`/`wl-paste` subprocess backend with a native library, so no clipboard operation can block the UI thread, and let a copied image be pasted into the terminal.

## Specs

S1. Every clipboard read and write goes through a native library (chosen: `arboard`), never a spawned helper. No clipboard operation may block the UI thread: a missing, slow or unresponsive clipboard must not freeze the window, and there is no child process to wait for or reap.

S2. A clipboard write is triggered by the gesture that completes a selection (mouse release) and by the explicit copy command, never by the motion events of a drag, and never when the selected text did not change.

S3. Pasting when the clipboard holds an image writes the PNG to a temporary file and inserts that path into the terminal, so any program can consume it. Only image MIME the library reports is accepted; anything else falls back to text paste. The temporary file is created in the session temp directory with a name that cannot collide, and the existing `TMPDIR` handling stays correct.

S4. Clipboard content is untrusted input: image payloads obey a size limit, an oversized or undecodable payload is refused with a message instead of being written, and nothing is written outside the temp directory.

S5. Behaviour is unchanged for text paste and copy: same content, same encoding, same trim rules, and the same failure messages, minus the helper process.

## Tasks

T1 — S1, S2, S5 — swap the backend, remove the subprocess code, keep the gesture rules — commit 1
T2 — S3, S4 — image paste as a temp-file path, with limits and messages — commit 2

## Log

L1. «Podemos cambiar el wl-copy por otra dependencia mas estable y que me permita pegar imagenes en la terminal y que no genere este tipo de errores»

L2. Confirmed cause of the hang reported after 1.2.1: with `copy_on_select = true` a drag spawned one `wl-copy` per selection event and the UI thread waited on the child; explicit copy (`ctrl+shift+c`) hung the same way, which proved the wait, not the trigger, is the primary defect. Disabling `copy_on_select` removed the symptom.

L3. Scope chosen by the user: `arboard` for text and images, and a copied image pasted as a temporary file path rather than a kitty graphics payload. The kitty route is a possible later task, not part of this one.

L4. Ordering: the stopgap fix that takes the wait off the UI thread lands first and touches the same file, so the backend swap starts only after it is committed. Two writers in `crates/platform/src/clipboard.rs` at once is the collision this document exists to prevent.

L5. Helpers removed (`crates/platform/src/clipboard.rs`): `wl-copy`/`wl-paste`/`xclip`/`xsel`, `Backend::{WlClipboard, Xclip, Xsel}`, the `which` lookup, the argument-vector builders, `run_helper` with its grace period and detached reaper thread, `describe_failure` (helper stderr) and the helper-only `TMPDIR` repair are gone. `Clipboard::get`/`set` are replaced by `Clipboard::read` (returns `Paste::Text` or `Paste::Image`), and the process tests went with the code: helper exit status, helper that never exits, helper that dies before reading, and the argument-vector test. The writer's queue/slot/gate tests stay — that shape is the app's contract and is unchanged.

L6. `arboard` reaches the clipboard over Wayland data-control and falls back to X11; the writer keeps one handle alive on its worker thread because on X11 that handle owns the connection that answers other applications. Text paste is read text-first, so text still wins when a clipboard offers both, and the remaining `TMPDIR` rule now only decides where a pasted image is written: a `TMPDIR` that no longer exists falls back to `/tmp` instead of failing the paste.

L7. Doc follow-up carried by the 1.2.2 release: the same change rewrote `docs/INSTALL.md`, `docs/ARCHITECTURE.md` and `docs/SECURITY.md`, so no page names `wl-clipboard`, `wl-copy`, `wl-paste`, `xclip` or `xsel` any more — install says the clipboard needs nothing at runtime, architecture describes the `arboard` route and `Backend::Unavailable`, and security says the service spawns no helper. `CHANGELOG.md` records the backend swap and the image paste under 1.2.2. `scripts/setup.sh` stopped installing and requiring `wl-clipboard` in the follow-up release commit.
