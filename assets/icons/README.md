# AstraTerm icons

The application icon comes from `logo.png` — the AstraTerm emblem: a dark terminal
window with a `>_` prompt, an orbit and a star.

## Files

| Path | Purpose |
|---|---|
| `logo.png` | The source artwork (landscape PNG on a white background). |
| `generate-icons.sh` | Regenerates every derived icon from `logo.png`. |
| `astraterm.png` | Canonical square 512 px icon, embedded into the binary for the window icon. |
| `hicolor/<size>/apps/astraterm.png` | The icon set that desktop environments read (32, 48, 64, 128, 256, 512). |

## Regenerating

```bash
sh assets/icons/generate-icons.sh
```

The script needs ImageMagick (`magick`). It removes the *connected* outer white
background by flood-filling from the four corners rather than deleting every white
pixel — the prompt glyph inside the window is white and must survive — then trims
and centres the artwork on a square canvas with a small margin, so the icon looks
the same in a launcher, a dock and a window switcher.

Commit the regenerated PNGs: they are what the installer and the package ship, and
the 512 px one is compiled into the binary.

## How the icon reaches the desktop

- `packaging/desktop/astraterm.desktop` declares `Icon=astraterm` and
  `StartupWMClass=astraterm`.
- The window sets the same identity (`app_id` on Wayland, `WM_CLASS` on X11) and
  its icon at startup, so the compositor associates the window with the entry
  instead of showing a generic placeholder.
- `packaging/linux/install.sh` copies every `hicolor/<size>` directory and refreshes
  the icon cache, which is what makes the entry appear in the Omarchy menu and the
  dock with its icon.
