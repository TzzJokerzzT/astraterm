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
| T2 | S2.2, S2.3 | Pegado asíncrono (hilo de portapapeles) y payload grande sin truncar ni congelar | inline | — |
| T3 | S3.1–S3.3 | Ligaduras opt-in con `font.ligatures` y verificación de alineación | inline | — |
| T4 | S4.1–S4.4 | Kitty keyboard: flag, flags expuestos y encoder completo + docs | inline | — |
| T5 | S5.1, S5.3 | Scanner APC/OSC/DCS incremental con reensamble entre chunks + modelo de anclaje | inline | — |
| T6 | S5.1, S5.2 | Renderer: textura, sampler, UV y orden de dibujo; decoder PNG con límites | worker | — |
| T7 | S5.2 | Decoders kitty graphics y sixel, eviction de memoria y política de seguridad | worker | — |

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

L4 (decisiones):
- Se sigue el orden propuesto: zoom, ligaduras, Kitty keyboard, imágenes. El pegado entra en segundo lugar, antes de ligaduras, por frecuencia de uso declarada por el usuario.
- El pegado ya funciona (`ctrl+shift+v` → `AppCommand::Paste` → `App::paste` → `encode_paste`); lo que se corrige es el bloqueo del event loop al leer el portapapeles y el manejo de payloads grandes.
- La persistencia del zoom se guarda en el estado de la aplicación (`StatePaths`, `crates/platform/src/paths.rs`), no en el TOML del usuario: escribir config dispararía el watcher y realimentaría `ConfigChanged`.
- Imágenes queda como documento propio en la práctica: T5–T7 se ejecutan como slices separadas y con la superficie de seguridad resuelta antes de exponerla.
