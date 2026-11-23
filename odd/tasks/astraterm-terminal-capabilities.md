# AstraTerm — capacidades de terminal (zoom, pegado, ligaduras, Kitty keyboard, imágenes)

Rama: `feat/terminal-capabilities` (desde `feat/astraterm-mvp`). Cierra cada slice con un commit de unidad de trabajo.
Orden decidido en L4: zoom → pegado → ligaduras → Kitty keyboard → imágenes (esta última es un subsistema aparte).

## Specs

S1. Atajo de zoom en la terminal. Hoy ya existe y debe seguir funcionando: `ctrl+=` / `ctrl+plus`
    aumenta, `ctrl+minus` reduce, `ctrl+0` restablece, `ctrl+shift+z` hace zoom del pane.
    Paso 0.5, rango 4.0–96.0 (`crates/app/src/app.rs:1298-1308`, `crates/config/src/keys.rs:617-620`).
S1.1 El tamaño de fuente elegido sobrevive al reinicio de la aplicación. Hoy vive solo en
    `font_size_override` (memoria) y se pierde, porque `apply_config_value` no escribe en disco.

S2. Pego texto, comandos y contenido externo a la terminal; es una acción frecuente.
S2.1 Pegar desde el portapapeles del sistema con el atajo existente (`ctrl+shift+v`) sigue funcionando
    y sigue respetando bracketed paste (`encode_paste`).
S2.2 La lectura del portapapeles no bloquea el hilo de la interfaz. Hoy `App::paste`
    (`crates/app/src/app.rs:1271-1295`) ejecuta `wl-paste`/`xclip` de forma síncrona dentro del event loop.
S2.3 Un pegado grande no se trunca ni congela la interfaz; si hay un límite, es explícito y se avisa.
S2.4 Si el portapapeles no está disponible, se informa en la barra de estado (comportamiento actual, se mantiene).

S3. La terminal acepta ligaduras.
S3.1 Opción `font.ligatures` (bool, por defecto `false`) que habilita `liga` y `calt` en el shaping
    vía `Attrs::font_features` (`cosmic-text` 0.19, `attrs.rs:298`/`:377`).
S3.2 Con `font.ligatures = false` el render es idéntico al actual: ninguna fila cambia de métrica ni de posición.
S3.3 Con ligaduras activas la alineación de columnas y el cursor no se degradan; si no se puede
    garantizar con shaping por fila, la opción no se publica en ese estado y se documenta el límite.

S4. La terminal implementa Kitty keyboard protocol.
S4.1 `terminal.kitty_keyboard` (bool, por defecto `true`) activa `TermConfig::kitty_keyboard`
    (`alacritty_terminal` 0.26, `term/mod.rs:349-350`).
S4.2 La terminal responde `CSI ? u` con los flags activos y honra `CSI = u` (set), `CSI > u` (push)
    y `CSI < u` (pop).
S4.3 El encoder emite `CSI codepoint;shifted;base;mods[;text] u` solo bajo los flags que el programa
    pidió; sin flags activos emite exactamente las secuencias xterm actuales (sin regresión).
S4.4 Los eventos de release/repeat se reportan solo con `REPORT_EVENT_TYPES` activo.

S5. La terminal implementa protocolos de imagen (sixel, iTerm2 OSC 1337, kitty graphics).
S5.1 Primero el scanner + modelo de anclaje + límites, sin renderer; después texturas y decoders.
S5.2 Los bytes de imagen vienen de procesos no confiables (incluido un host remoto por SSH): dimensión
    y memoria acotadas, sin descarga por URL, y entrada en `docs/SECURITY.md`.
S5.3 Ninguna imagen rompe el grid ni la salida de texto normal: las secuencias malformadas se descartan.

## Tasks

| ID | Spec | Qué se hace | Route | Commit |
|---|---|---|---|---|
| T1 | S1.1 | Persistir el tamaño de fuente elegido en el estado de la app y aplicarlo al arrancar | inline | `b9ec742` |
| T2 | S2.2, S2.3 | Pegado asíncrono (hilo de portapapeles) y payload grande sin truncar ni congelar | inline | `2a716b3` |
| T3 | S3.1–S3.3 | Ligaduras opt-in con `font.ligatures` y verificación de alineación | inline | `7410eb6` |
| T4 | S4.1–S4.4 | Kitty keyboard: flag, flags expuestos y encoder completo + docs | inline + worker | `a37ee4b` |
| T5 | S5.1, S5.3 | Scanner APC/OSC/DCS incremental con reensamble entre chunks + modelo de anclaje | inline + worker | `662c512` |
| T6 | S5.2 | Decoders (kitty graphics, iTerm2, sixel) con límites de dimensión y memoria; puro y testeable sin GPU | worker | `06e1f53` |
| T7 | S5.1 | Renderer: ítems de imagen en `Scene`, pipeline texturizado, sampler/UV y caché de texturas con desalojo | worker | `d9ab57d` |
| T8 | S5.1, S5.2 | Cableado en la app (decode → anclaje → escena), opción `terminal.images` y `docs/SECURITY.md` | worker | `44af738` + `0cfe9c0` |

T5–T7 no empiezan hasta que T1–T4 estén cerrados y verificados: son subsistema, no feature.

## Log

L1 (pedido original, verbatim):
"Es posible que puedas implementar kitty keyboard protocol, para que acepta ligaturas e images protocols y tambien implementar un atajo para hacer zoom en la terminal . Pero antes de hacerlo que beneficios y costos traeria implementarlo"

L2 (evidencia, auditoría de factibilidad):
- Zoom ya existe: `ActionId::{IncreaseFontSize,DecreaseFontSize,ResetFontSize,ToggleZoom}` en `crates/config/src/keys.rs:379-382`, defaults en `:617-620`, lógica en `crates/app/src/app.rs:1298-1308`. El único hueco es la persistencia.
- Kitty keyboard: `alacritty_terminal` 0.26 ya parsea el protocolo (4 handlers gateados en `term/mod.rs:1276,1289,1309,1324`; report en `:1283-1284`) y expone los flags por `Term::mode()` (`term/mod.rs:81,86-110`). NO trae encoder de teclas (`event.rs:14-40`, solo `PtyWrite`): esa parte es nuestra.
- Ligaduras: `cosmic-text` 0.19 sí expone `Attrs::font_features` y el shaping las aplica con harfrust. El repo no las pasa (`crates/renderer/src/text.rs:151-183`). Riesgo real: las filas se forman completas y las métricas salen del avance de `MMMMMMMMMM` (`text.rs:203-213`), así que un avance de ligadura distinto de N celdas desalinea.
- Imágenes: cero soporte en el core (sin sixel/iTerm2/`_G`, features del crate solo `serde`). Choke point del stream: `crates/terminal/src/engine.rs:221-229`; chunks de 32 KiB en `crates/pty/src/lib.rs:25,415-421` obligan a reensamblar secuencias. Renderer: dos pipelines, un bind group de uniforms, ninguna `create_texture`/`create_sampler`/`write_texture` en `crates/renderer/src`.

L3 (pedido del usuario, verbatim):
"Sigue el orden y quiero poder pegar texto, comandos, ext externos a la terminal. Ya que es algo que hago muy frecuentemente"

L4 (cierre de T1–T4, evidencia):
- T1 `b9ec742`, T2 `2a716b3`, T3 `7410eb6`, T4 `a37ee4b`. Suite completa 380 tests, `cargo clippy --workspace --all-targets` sin advertencias, `cargo fmt --check` limpio.
- T4 delegado en parte a un worker por el backstop de contexto; su primer intento se detuvo porque la superficie de edición que le di omitía `crates/session/src/lifecycle.rs` (el verdadero sitio donde se construye `EngineOptions`, no `manager.rs`) y los dos tests de sesión que escriben `EngineSpec` como literal. Ampliada la superficie y completado.
- Defecto propio corregido en el camino: añadir `KeyPress.event` dejó `crates/app/src/keys.rs` sin compilar (construía el literal sin el campo) porque solo se verificó la crate `terminal` tras el cambio del encoder. El worker lo arregló dentro de su superficie.
- Caveat upstream documentado: `CSI ? u` reporta solo la cima de la pila de modos, así que una app que usa `CSI = u` sin push consulta `0` aunque sus flags estén activos; el encoder los honra igual.
- Límite conocido no testeable sin ventana: los modificadores del release salen de `self.mods`, así que si la plataforma limpia Shift antes del release, `Shift+A` puede liberarse como `a`.
L5 (decisiones de alcance, tomadas al autorizar; se registran con el cierre):
- Se sigue el orden propuesto: zoom, ligaduras, Kitty keyboard, imágenes. El pegado entra en segundo lugar, antes de ligaduras, por frecuencia de uso declarada por el usuario.
- El pegado ya funciona (`ctrl+shift+v` → `AppCommand::Paste` → `App::paste` → `encode_paste`); lo que se corrige es el bloqueo del event loop al leer el portapapeles y el manejo de payloads grandes.
- La persistencia del zoom se guarda en el estado de la aplicación (`StatePaths`, `crates/platform/src/paths.rs`), no en el TOML del usuario: escribir config dispararía el watcher y realimentaría `ConfigChanged`.
L6 (decisión de secuenciación, T5 cerrado):
- T5 cerrado en `662c512`: scanner + límites + modelo de anclaje, 9 tests nuevos, suite en 389 verde. El worker declaró que no pudo observar RED real (módulo nuevo) y lo sustituyó por dos mutaciones temporales revertidas; se acepta porque los tests muerden.
- Se invierte el orden de T6 y T7 respecto al plan inicial: primero los decoders (puros, testeables sin GPU) y al final las texturas y el cableado. Motivo: el decoder es donde vive el riesgo de seguridad (bytes no confiables de un host remoto) y puede verificarse sin ventana; la GPU no.
- Riesgos heredados de T5 que T7 debe resolver: remapeo de líneas en resize (`retain_anchored` hoy solo descarta), presupuesto de `ImageStore` sin calibrar (64 MiB por defecto) y constantes de límite que deberían pasar a `EngineOptions` si se quiere configurar por sesión.
- Coste conocido y aceptado: cada `process` copia el passthrough en un `Vec` nuevo (32 KiB por lectura de PTY), despreciable frente al parseo VT.

L7 (cierre de la feature, evidencia):
- T7 `d9ab57d`: pipeline texturizado dibujado entre fondo y glifos, píxeles en `Arc` con generación y `TextureCache` con política pura. El worker de este slice murió con `invalid_request` de infraestructura **después** de escribir el código; se revisó y se dio por bueno el trabajo ya presente en el árbol, no se relanzó a ciegas.
- T8 `44af738` (opción `terminal.images`, default `false`) + `0cfe9c0` (cableado completo).
- Verificación total: 436 tests, 0 fallos; `cargo clippy --workspace --all-targets` sin advertencias; `cargo fmt --check` limpio; `cargo build --release` ok.
- **Verificación visual real** (lo que ningún test cubre): binario release con `terminal.images = true` y `--config` bajo `/tmp`, imagen PNG 96×64 enviada como iTerm2 `OSC 1337;File=inline=1`, captura de pantalla con `grim`. Resultado: la imagen aparece en la rejilla, dentro del pane, con el texto todavía legible debajo. Artefactos temporales borrados.
- Límites que quedan fuera, documentados en `docs/TERMINAL_COMPATIBILITY.md`: transferencia por chunks de kitty (`m=1`), paletas sixel HLS e `inline=0` devuelven `Unsupported`; no hay reanclaje en resize (solo descarte); en scrollback saturado un ancla muy antigua puede quedarse fija; `compose_images` replica el padding de `paint::paint_pane` en local y hay que mantenerlos sincronizados si esa geometría cambia.
