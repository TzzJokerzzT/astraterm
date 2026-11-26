# AstraTerm — backlog de mejoras

Documento de trabajo, no una especificación. Cada entrada dice **qué falta**, **por qué
vale la pena**, **dónde vive el código hoy** y **cómo se verifica**, para que una sesión
futura pueda atacar una sin reconstruir todo el contexto.

Cuando se ataque una entrada, se abre su documento de feature en `odd/tasks/<nombre>.md`
con specs `S#` y slices, como se hizo con `astraterm-terminal-capabilities.md`.

## Estado actual

Rama `main`. **489 tests**, `cargo clippy --workspace --all-targets` sin advertencias,
`cargo fmt --check` limpio.

Versión publicada: **1.0.0** — tag `v1.0.0` (que apunta al commit `3806289`) y release
en https://github.com/TzzJokerzzT/astraterm/releases/tag/v1.0.0 con las notas del
`CHANGELOG.md`. La instalación por versión está verificada:
`./scripts/setup.sh --version v1.0.0` desde un checkout del tag devuelve
`ok: the working tree is exactly v1.0.0 and clean`.

Ya implementado y cerrado:

| Qué                                                  | Commit                          |
| ---------------------------------------------------- | ------------------------------- |
| Zoom persistente entre reinicios                     | `b9ec742`                       |
| Pegado asíncrono con límite de 8 MiB                 | `2a716b3`                       |
| Ligaduras opt-in (`font.ligatures`)                  | `7410eb6`                       |
| Kitty keyboard protocol completo                     | `a37ee4b`                       |
| Captura de imágenes antes del parser VT              | `662c512`                       |
| Decoders kitty / iTerm2 / sixel con límites          | `06e1f53`                       |
| Renderer texturizado con caché acotada               | `d9ab57d`                       |
| Cableado de imágenes + `terminal.images`             | `0cfe9c0`                       |
| AltGr como composición + IME commit                  | `c6848fa`                       |
| Teclas funcionales con forma legacy bajo flags Kitty | `7c3c56b`                       |
| Notificaciones OSC 9 / 777 + urgency hint            | `3ecf4b6`, `1a3db6d`, `63c2d7e` |
| Terminal flotante con ocultar/mostrar                | `894520b`                       |
| Flotante en su propia capa de render                 | `793f9dc`                       |
| Retroalimentación al guardar y recargar config       | `e015b3c`                       |
| Versión 1.0.0, `scripts/setup.sh` y `docs/INSTALL.md` | `f0c3502`                      |
| `--version <tag>` en el script de instalación        | `272b3d6`                       |
| Política de tags y releases                          | `215bbf2`                       |
| Guardas de `--version`: árbol sucio y repo contenedor | `2c18d32`, `3806289`            |
| Kitty graphics por chunks (`m=1`)                     | `e649cc5` (1.1.0)               |
| Escáner resincroniza tras un OSC malformado           | `dd3b534` (1.1.1)               |
| Mensajes y orden de `--version` + arnés               | `fix(scripts)` (1.1.2)          |
| Las imágenes sobreviven a un resize                   | `fix(app)` (1.1.4)              |

De la lista de mejoras B1–B18 de este documento, **solo las notificaciones OSC 9/777
y el kitty por chunks están hechas**. Todo lo demás sigue pendiente.

---

## Versionado y releases

La versión vive en `[workspace.package] version` (`Cargo.toml`) y la reporta
`astraterm --version`. Un test en `crates/app/src/cli.rs` la fija contra
`CARGO_PKG_VERSION` y contra la salida exacta del CLI, así que un bump a medias falla la
suite en lugar de publicar un número que no coincide con el binario.

**Estado**: **publicada**. Tag anotado `v1.0.0` sobre el commit `3806289`, release
`AstraTerm 1.0.0` con el cuerpo extraído de la entrada `[1.0.0]` del `CHANGELOG.md`.
Nada de este bloque queda pendiente salvo endurecer los mensajes (ver B19).

### Regla, a partir de 1.0.0

| Tipo de cambio                                      | Versión                           | Ejemplos                                                           |
| --------------------------------------------------- | --------------------------------- | ------------------------------------------------------------------ |
| Feature nueva                                       | MINOR (`1.1.0`)                   | terminal flotante, soporte de imágenes                             |
| Cambio visible en una feature existente, compatible | MINOR                             | nuevas opciones, nuevos valores de config, mensajes distintos      |
| Cambio que rompe un contrato ya consumido           | MAJOR (`2.0.0`)                   | quitar o renombrar una opción, cambiar el formato de configuración |
| Corrección que no cambia contratos                  | PATCH (`1.0.1`)                   | crash, una tecla mal codificada                                    |
| Solo documentación, tests o refactor interno        | sin bump hasta el próximo release | README, este backlog                                               |

La regla corta, que es la que hay que recordar: **si toca una feature de cara al
usuario, sube la versión**. Y si la rompe, sube el número mayor.

### Política de tags y releases

Decidido; no se vuelve a discutir en cada release:

- **Los tags llevan `v`**: `v1.0.0`, `v1.1.0`, `v2.0.0`. No se crean tags sin `v`.
  `scripts/setup.sh --version` normaliza lo que escriba el usuario y compara siempre
  contra la forma canónica `vX.Y.Z`.
- **Cada release publicada lleva las notas del `CHANGELOG.md`**: el cuerpo de la release
  es la entrada correspondiente del CHANGELOG (sus secciones `Added`, `Changed`, `Fixed`
  y su fecha), tal cual. No se escribe un resumen aparte a mano, porque en cuanto uno de
  los dos textos se actualiza quedan contradiciéndose.
- **El tag y la release son el mismo hecho**: ni tag sin release, ni release sin tag.
  Mientras no haya release publicada, `1.0.0` solo existe en el código.

### Checklist de release

En este orden, siempre:

1. `Cargo.toml`: `version`.
2. `Cargo.lock`: se regenera con `cargo build --release` **sin** `--locked` (con `--locked`
   falla a propósito, porque el bump invalida el lock). Después `cargo build --release
   --locked` debe completar sin tocar nada.
3. `packaging/linux/PKGBUILD`: `pkgver`.
4. `CHANGELOG.md`: entrada con secciones (`Added`, `Changed`, `Fixed`) y fecha real
   tomada de `date -I`, nunca inventada.
5. Verificación: `cargo test --workspace`, `cargo clippy --workspace --all-targets`,
   `cargo fmt --check`.
6. Commit `chore(release): x.y.z` y tag anotado `git tag -a vX.Y.Z -m "..."`.
7. `cargo build --release --locked` y `./scripts/setup.sh --dry-run`, para confirmar que
   las dependencias declaradas siguen siendo las reales. Añadir también
   `./scripts/setup.sh --dry-run --version vX.Y.Z`, que debe decir que el árbol ya está
   en ese tag.
8. Publicar la release con el tag `vX.Y.Z` y con las notas copiadas de la entrada del
   `CHANGELOG.md` para esa versión: esas notas son el cuerpo de la release, no un
   resumen nuevo.
9. Probar el camino del usuario **desde el tag**, no desde `main`. Con la rama por
delante del tag, `--version vX.Y.Z` se niega con código `4` y eso es correcto: el script
verifica y no mueve el checkout. El camino limpio es un worktree temporal:
   `git worktree add /tmp/astraterm-vX vX.Y.Z && cd /tmp/astraterm-vX && ./scripts/setup.sh --version vX.Y.Z --install`, y luego `git worktree remove /tmp/astraterm-vX`.

---

## Bloque A — Deuda con más valor por línea

### B3. `set_ime_cursor_area`

- **Por qué**: el IME ya funciona (commit compuesto), pero la ventana de candidatos
  aparece donde el compositor quiera en vez de junto al cursor.
- **Dónde**: `crates/app/src/app.rs`; se llama `set_ime_purpose` pero nunca
  `set_ime_cursor_area`. Hay que recalcularlo con la posición del cursor y en cada
  movimiento/resize.
- **Coste**: S.
- **Verificación**: manual, con un IME que muestre candidatos (fcitx5 en esta máquina).

### B4. Fuzzing del scanner de imágenes y notificaciones

- **Por qué**: es un parser nuevo alimentado por bytes de un host remoto, y ya apareció un
  caso de desincronización (ver B12).
- **Dónde**: `crates/terminal/src/image.rs`.
- **Qué**: `proptest` con dos invariantes: (1) sin secuencias de imagen/notificación, el
  passthrough es byte a byte idéntico, incluso con cortes arbitrarios; (2) ninguna entrada
  provoca panic ni crecimiento sin límite.
- **Coste**: S. Añade `proptest` como dev-dependency.

---

## Bloque B — Uso diario

### B5. Integración de shell (OSC 133)

- **Por qué**: marcas de prompt y de comando permiten saltar entre comandos, saber si el
  último falló, mostrar el comando en curso en la barra de estado y avisar al terminar.
  Es lo que convierte el emulador en un terminal usable a diario.
- **Dónde**: no existe nada. El parser VT descarta OSC desconocidos, así que hay que
  capturarlo en el scanner (`crates/terminal/src/image.rs`, donde ya viven OSC 9/777) y
  propagarlo como evento, igual que se hizo con las notificaciones
  (`engine.rs` → `core/src/events.rs` → `app.rs`).
- **Coste**: M. La UI (indicadores y navegación) es la mitad del trabajo.
- **Verificación**: `printf` de secuencias 133 en una sesión; test de que un comando
  fallido se reporta; prueba visual de la barra.

### B6. OSC 8 hyperlinks clicables

- **Por qué**: el core acepta OSC 8, pero `CellSnapshot` no lleva el destino: no se
  subrayan ni se pueden clicar. Ya existe detección de URL por click, así que es completar
  el camino.
- **Dónde**: `crates/terminal/src/snapshot.rs` (campo en `CellSnapshot`),
  `crates/terminal/src/engine.rs` (leerlo del `Cell`), `crates/app/src/ui.rs` (`url_at` y
  el hit testing).
- **Coste**: S/M.
- **Verificación**: `printf '\e]8;;https://example.com\e\\texto\e]8;;\e\\'` y click.

### B7. Copy-on-select a la selección primaria

- **Por qué**: en Linux es lo que todo el mundo espera; el servicio ya lo soporta.
- **Dónde**: `crates/app/src/app.rs` usa siempre `ClipboardSelection::Clipboard`
  (`:440`, `:1439`, `:1461`); `astraterm_platform::clipboard::Selection::Primary` existe y
  nadie lo usa.
- **Coste**: S. Falta decidir si el pegado con botón central entra en el mismo cambio.
- **Verificación**: seleccionar en AstraTerm y pegar con el botón central en otra app.

### B8. Selección por bloque, doble click por palabra, triple por línea

- **Por qué**: bloque rectangular es imprescindible para columnas alineadas (logs, `top`);
  doble click por palabra es lo que todo el mundo intenta por reflejo.
- **Dónde**: el tipo ya existe (`crates/terminal/src/snapshot.rs`, `is_block`) pero no hay
  gesto ni atajo en `crates/app/src/app.rs`.
- **Coste**: S/M.
- **Verificación**: tests de selección sobre un snapshot con texto alineado.

---

## Bloque C — Calidad de producto

### B9. Ligaduras correctas (shaping por runs)

- **Por qué**: `font.ligatures` entrega la fila entera al shaper y confía en que el avance
  de la ligadura sea exactamente N celdas. Si no lo es, las columnas se desplazan.
- **Dónde**: `crates/renderer/src/text.rs` (`shape`, `RowCache`, `style_key`). Hay que
  formar por tramos alineados a celdas y cachear por run.
- **Coste**: M/L. Es un cambio de renderer, no una opción más.
- **Verificación**: fuente con ligaduras cuyo avance no sea múltiplo exacto de celda; test
  de que las posiciones de columna siguen siendo las de la rejilla.

### B10. Búsqueda con regex e índice incremental

- **Por qué**: hoy es substring sobre el snapshot visible; buscar mientras el output inunda
  no existe.
- **Dónde**: `crates/terminal/src/search.rs` y el overlay en `crates/app/src/ui.rs`.
- **Coste**: M.
- **Verificación**: tests de regex con casos límite y de búsqueda con daño por fila.

### B11. Dibujar el preedit del IME

- **Por qué**: el commit compuesto ya se escribe, pero el preedit solo se tracea. Para
  composición compleja es escribir a ciegas.
- **Dónde**: `crates/app/src/app.rs` (`handle_ime`, rama `Ime::Preedit`).
- **Coste**: S/M. La decisión de diseño es dónde se pinta (celda del cursor vs overlay).
- **Verificación**: manual con fcitx5 y una dead key, y con japonés/Chino si hay IME.

### B12. Desincronización del scanner con OSC malformado _(hallazgo de review R3-004)_

- **Por qué**: un OSC 9 sin el `;` que termina en BEL deja bytes en passthrough y el estado
  en `Foreign`, así que deja de reconocer imágenes/notificaciones hasta el terminador
  siguiente. Fue introducido por la captura de notificaciones.
- **Dónde**: `crates/terminal/src/image.rs` (`step_osc`, `step_foreign`).
- **Coste**: S.
- **Verificación**: test con `ESC ] 9 BEL` seguido de una imagen válida.

### B13. Paletas sixel HLS

- **Por qué**: `#n;1;…` devuelve `Unsupported`; completitud del decoder.
- **Dónde**: `crates/terminal/src/image_decode.rs`.
- **Coste**: S.

### B14. Campana audible o visual configurable

- **Por qué**: BEL solo deja un mensaje de barra de 4 segundos.
- **Dónde**: `crates/session/src/manager.rs` escribe `session.bell`, y **nadie lo lee**:
  el flag está muerto. Además `crates/app/src/app.rs` pinta el mensaje para todas las
  ventanas.
- **Coste**: S. La decisión es sonido vs flash de borde, y si es por pane o global.
- **Verificación**: `printf '\a'` con cada modo configurado.

---

## Bloque D — Arquitectura y seguridad

### B15. Evitar el memcpy del passthrough

- **Por qué**: cada `process` copia el chunk completo (32 KiB) aunque no haya ni un ESC.
- **Dónde**: `crates/terminal/src/engine.rs` (`process`) y
  `crates/terminal/src/image.rs` (`ScanOutput::passthrough: Vec<u8>`). Un camino rápido
  cuando no hay `ESC` en el chunk elimina la copia en el caso normal.
- **Coste**: S.
- **Verificación**: test de que sin ESC el parser recibe el mismo slice, y medición de
  latencia input→paint antes/después.

### B16. Repintado por región dañada en GPU

- **Por qué**: se repinta la ventana entera en cada frame aunque solo cambie una fila. Ya
  existe damage tracking por filas para el shaping.
- **Dónde**: `crates/renderer/src/lib.rs` (render pass) y `paint.rs`.
- **Coste**: M.
- **Verificación**: perfil con un `yes` en una mitad de la pantalla; comparar GPU/CPU.

### B17. Límites de imagen configurables por sesión

- **Por qué**: `MAX_IMAGE_BYTES`, `MAX_PENDING_BYTES` y el presupuesto de 64 MiB de
  `ImageStore` son constantes sin calibrar contra memoria real de GPU.
- **Dónde**: `crates/terminal/src/image.rs`, `crates/terminal/src/engine.rs`
  (`EngineOptions`), `crates/config/src/schema.rs`.
- **Coste**: S.

### B18. OSC 52 con confirmación

- **Por qué**: hoy está apagado por seguridad (`allow_osc52: false` en
  `crates/session/src/lifecycle.rs`), lo cual es correcto pero deja la función inutilizable.
- **Dónde**: la ruta de respuesta existe en `crates/terminal/src/engine.rs` y
  `crates/session/src/manager.rs`; falta el diálogo por sesión y la política.
- **Coste**: S/M. Toca seguridad: requiere decisión explícita de UX y entrada en
  `docs/SECURITY.md`.

## Deuda de proceso (no es código)

### P1. Las reviews que se atascaban: causa encontrada, no era el provider

En la primera mitad del día, tres linajes quedaron sin veredicto porque la captura del
validador dirigido rechazaba el binding como obsoleto con `mutation_performed: none`.
Acusé al provider y **me equivoqué**: el problema era mi forma de invocar la secuencia
manual (pedir STATUS y luego reenviar un binding que en esa secuencia ya había cambiado).
Al usar la ruta de una sola llamada, `gentle_review_run`, el ciclo completo cerró limpio:
lineage `review-ecf2af1b425d3e2b` **aprobado** con la autoridad quemada, y antes el de
documentación (`review-1232d4ceb4439f5d`), también aprobado.

**Decisión tomada**: no hay defecto que reportar. La regla operativa queda en las notas de
entorno: para una review completa, `gentle_review_run` y no la secuencia manual de
capturas.

### P2. Hallazgos no bloqueantes sin arreglar

- **R2-002 / R3-003**: el doc dice que un cuerpo vacío tras sanear se descarta, y el código
  envía una notificación con cuerpo vacío. O se corrige el código, o el doc.
- **R2-001**: el comentario promete orden de stream entre eventos y notificaciones, y en un
  mismo chunk no se cumple.
- **R4-003**: un cuerpo de 8 MiB se convierte a `String` antes del recorte a 1024
  caracteres; el presupuesto real de memoria es mayor de lo que sugiere el límite.

### P3. `handle_key_release` no consulta el keymap

`crates/app/src/app.rs`: en una sesión con `REPORT_EVENT_TYPES` (nvim, por ejemplo), una
tecla consumida como atajo igual emite su release. Atuin no lo expone porque no pide ese
flag. Coste S, riesgo de romper atajos si se hace mal.

---

## Notas de entorno para quien siga

- **Actualizar el binario**: `cargo build --release --locked && ./scripts/setup.sh --install`
  (o `./packaging/linux/install.sh` a secas). El script comprueba dependencias, toolchain y
  fuentes antes de construir; nunca pide root ni toca la configuración. Cerrar las
  ventanas antes: los procesos vivos mantienen el binario viejo.
- **Instalar una release concreta**: `--version vX.Y.Z` **verifica y no mueve el
  checkout**. Con `main` por delante del tag se niega con código 4, y eso es correcto. El
  camino limpio es un worktree:
  `git worktree add /tmp/astraterm-vX vX.Y.Z && cd /tmp/astraterm-vX && ./scripts/setup.sh --version vX.Y.Z --install`,
  y luego `git worktree remove /tmp/astraterm-vX`.
- **Reviews nativas**: usar `gentle_review_run`, que ejecuta STATUS, captura, validación y
  reconocimiento en una llamada. La secuencia manual de capturas con bindings reenviados a
  mano acaba en `collectBinding ... stale` aunque el binding venga de un STATUS recién
  pedido: fue un error de invocación propio, no del provider.
- **Verificar una GUI sin tocar la config del usuario**: `--config /tmp/x/config.toml` y
  `--command <argv>`, luego `grim /tmp/x/shot.png` y mirar el PNG con la herramienta de
  lectura de imágenes.
- **Ver los bytes que emite el terminal**: `--command sh -c 'stty raw -echo; printf
"\033[>13u"; cat -v'` con las teclas inyectadas por `/dev/uinput`.
- **Nunca** `pkill -f <patrón>` si el patrón aparece en tu propia línea de comandos: mata
  el shell que lo ejecuta. Matar por PID.
- **`rm -rf` en `/tmp` puede estar bloqueado por política**: usar `rm -f` por archivo y
  `rmdir`.
