# AstraTerm — endurecer imágenes y versionado (post 1.0.0)

Tres entradas del backlog, en el orden pedido: B1, B12 y B19. Cada una es un slice con su
propio commit y su propio bump de versión, según la política de
`odd/tasks/astraterm-backlog.md`.

## Specs

S1. **Kitty graphics por chunks (`m=1`)**, de B1.
S1.1 El decoder acepta una imagen troceada: cada APC trae `m=1` salvo el último, que trae
    `m=0` o lo omite, y todos comparten el mismo `i=<id>`. El resultado es el mismo
    `DecodedImage` que produce hoy un único chunk.
S1.2 Varias transferencias pueden intercalarse sin mezclarse: los chunks de dos ids
    distintos se acumulan por separado y cada una se decodifica al recibir su final.
S1.3 Los buffers en vuelo están acotados: un tope por transferencia, un tope agregado y
    un número máximo de transferencias vivas. Al excederlos se descarta la más antigua y
    se cuenta; ninguna entrada sin terminar puede crecer sin límite.
S1.4 Un chunk final sin transferencia previa, o un id ausente cuando hace falta, devuelve
    `DecodeError`, nunca un resultado parcial.
S1.5 Lo que hoy devuelve `Unsupported` sigue devolviéndolo: formatos raw distintos de
    24/32, HLS sixel e iTerm2 `inline=0`.

S2. **El scanner no se desincroniza con un OSC malformado**, de B12.
S2.1 Tras un `ESC ] 9 BEL` (introducer de notificación sin el `;`) el scanner vuelve a
    reconocer imágenes y notificaciones en la secuencia siguiente.
S2.2 Los bytes de esa secuencia malformada salen en el passthrough byte a byte, como hoy.
S2.3 La invariante de siempre se mantiene: una entrada sin secuencias de imagen o
    notificación llega al parser idéntica, con cualquier corte de chunks.

S3. **Mensajes y orden de `--version`**, de B19.
S3.1 Si HEAD está por delante del tag pedido, el error dice cuántos commits lo separan y
    ofrece el worktree o el `git checkout`, en vez de solo `which is not an exact tag`.
S3.2 La comprobación de árbol sucio corre **después** de comparar HEAD con el tag, para no
    afirmar que el tag está en el árbol cuando no lo está.
S3.3 `docs/INSTALL.md` documenta que el árbol debe estar limpio, no solo el tag.
S3.4 Si el usuario declina el `git fetch --tags`, el mensaje lo dice: no se intentó.
S3.5 Las rutas de `--version` quedan cubiertas por comprobaciones repetibles (matriz con
    tag temporal o arnés de shell), no solo por prueba manual.

S4. **Versionado por slice.**
S4.1 B1 es feature: `1.1.0` (MINOR).
S4.2 B12 y B19 son correcciones sin cambio de contrato: `1.1.1` y `1.1.2` (PATCH).
S4.3 Cada bump actualiza `Cargo.toml`, `PKGBUILD`, el test de `cli.rs` y `CHANGELOG.md`, en
    ese orden, y la suite debe pasar antes de commitear.

## Tasks

| ID | Spec | Qué se hace | Route | Commit |
|---|---|---|---|---|
| T1 | S1 | Decoder con estado de transferencias, cableado en la app y topes | worker | `e649cc5` (1.1.0) |
| T2 | S2 | Corrección del estado del scanner al cerrar un OSC ajeno | worker | `dd3b534` (1.1.1) |
| T3 | S3 | Mensajes, orden de comprobaciones y doc de `--version` | worker | `0bdefec` (1.1.2) |
| T4 | S4 | Bumps 1.1.0, 1.1.1 y 1.1.2 con CHANGELOG | inline | `78a690d`, `a8fb211`, `5808930` |

## Log

L1 (pedido del usuario, verbatim):
"Comencemos con la B1, B12 y por ultimo la B19"

L2 (contexto de partida):
- Base: `main` en `5286182`, versión 1.0.0 publicada (tag `v1.0.0`), 489 tests.
- B1 está en `crates/terminal/src/image_decode.rs`: `decode_kitty` devuelve
  `DecodeError::Unsupported` al ver `m=1`.
- B12 está en `crates/terminal/src/image.rs`: `step_osc`/`step_foreign`.
- B19 está en `scripts/setup.sh` (`check_requested_version`) y `docs/INSTALL.md`.

L3 (cierre, evidencia):
- **T1** `e649cc5` + release `78a690d` → `1.1.0`. Decoder con estado: chunks por `i=`,
  decodificación con la cabecera del primero, topes 8 MiB/transferencia, 16 MiB agregados,
  4 ids vivos. `decode` devuelve `Option<DecodedImage>` porque un chunk en vuelo no es ni
  imagen ni error.
- **T2** `dd3b534` + release `a8fb211` → `1.1.1`. El bug se reprodujo en rojo antes de
  tocar código: no era solo el BEL, también el `ESC \`. El terminador entraba en el prefijo
  candidato y se volcaba, dejando el escáner en `Foreign` esperando otro terminador.
- **T3** `0bdefec` + release `5808930` → `1.1.2`. Arnés `scripts/tests/version-check.sh`:
  11 casos, **5 pasan y 6 fallan sobre la base** y 11/11 sobre el árbol corregido.
- Verificación final: **501 tests**, `cargo clippy --workspace --all-targets` sin
  advertencias, `cargo fmt --check` limpio, `astraterm --version` → `1.1.2`, y
  `cargo build --release --locked` sin tocar el lockfile.
- Los tres releases están **sin taggear**: `v1.1.0`, `v1.1.1` y `v1.1.2` los decide y
  publica el usuario.
