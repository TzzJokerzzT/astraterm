# AstraTerm icons

`astraterm.svg` is a **placeholder** icon: a dark terminal window with an
accent-coloured (`#8B9CFF`) prompt glyph on the theme background. The maintainer
is expected to replace it with the final artwork. Keep the replacement a valid,
standalone SVG with `viewBox="0 0 128 128"`, no external references and no
embedded raster images, so the packaging steps below keep working unchanged.

## Rasterising to PNG

Freedesktop icon themes look for a PNG in `hicolor/128x128/apps/`, so the
packaging assets ship a 128x128 PNG generated from this SVG. Rasterise it with
`rsvg-convert` (package `librsvg` on Arch, `librsvg2-bin` on Debian) from this
directory:

```sh
rsvg-convert -w 128 -h 128 astraterm.svg -o astraterm.png
```

Inkscape works too, if you already have it:

```sh
inkscape astraterm.svg --export-type=png --export-width=128 --export-height=128 --export-filename=astraterm.png
```

`astraterm.png` is a build artefact and is not committed; `packaging/linux/install.sh`,
`packaging/linux/PKGBUILD` and `.gitignore` all expect it to be absent from the
repository. They either rasterise the SVG on the fly or, when `rsvg-convert` is
not available, install the SVG into `hicolor/scalable/apps/`.
