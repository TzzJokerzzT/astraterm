# Test fixtures

## `grid_ligature.ttf`

The font the ligature tests shape with (S3). Two kilobytes, generated with
`fontTools`; it holds one property no installed font in CI has: a substitution whose
advance is *not* the width of the cells it stands for.

* Every printable ASCII character (`U+0020`..`U+007E`) is a one-column glyph of
  advance 600 units at 1000 units per em, with the same empty outline. The font is
  therefore monospace exactly like the shipped default (`cell width = 7.8 px` at
  size 13).
* `liga` substitutes `uni002D uni003E` (`->`) with `f_arrow`, a single glyph whose
  advance is 1000 units. Two cells are 1200, so a row that shapes `->` as one glyph
  drifts 200 units (2.6 px) to the left of where the grid expects it, and every
  glyph after it moves with it. With ligatures off the same row shapes four glyphs
  and lands exactly on the columns.

The font is generated, not hand-edited. Rebuilding it takes this script and
`python3 -m pip install fonttools` (nothing in the build or the test run depends on
Python):

```python
from fontTools.fontBuilder import FontBuilder
from fontTools.feaLib.builder import addOpenTypeFeaturesFromString
from fontTools.pens.ttGlyphPen import TTGlyphPen

ADVANCE = 600
names = []
order = [".notdef"]
for code in range(0x20, 0x7F):
    name = "uni%04X" % code
    order.append(name)
    names.append((code, name))
order.append("f_arrow")

fb = FontBuilder(1000, isTTF=True)
fb.setupGlyphOrder(order)
fb.setupCharacterMap(dict(names))
glyphs = {}
metrics = {}
for name in order:
    pen = TTGlyphPen(None)
    glyphs[name] = pen.glyph()
    metrics[name] = (ADVANCE, 0)
metrics["f_arrow"] = (1000, 0)
fb.setupGlyf(glyphs)
fb.setupHorizontalMetrics(metrics)
fb.setupHorizontalHeader(ascent=800, descent=-200)
fb.setupNameTable(
    {
        "familyName": "AstraGrid Test",
        "styleName": "Regular",
        "uniqueFontIdentifier": "AstraGrid Test Regular 1.0",
        "fullName": "AstraGrid Test Regular",
        "psName": "AstraGridTest-Regular",
        "version": "Version 1.0",
    }
)
fb.setupOS2(sTypoAscender=800, sTypoDescender=-200, usWinAscent=800, usWinDescent=200)
fb.setupPost()
addOpenTypeFeaturesFromString(
    fb.font, "feature liga { sub uni002D uni003E by f_arrow; } liga;"
)
fb.save("grid_ligature.ttf")
```
