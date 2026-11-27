//! Damaged-region tracking: which strips of the target a frame has to rebuild.
//!
//! The renderer is handed one composed [`Scene`] per frame, so the damage it can act
//! on is the difference between the frame it is drawing and the frame it drew last: a
//! frame whose scene is unchanged draws nothing, and a frame that changed rebuilds only
//! the strips the change reaches (S7).
//!
//! The target is cut into horizontal *bands*, one text row each. Every drawing
//! instruction of every layer is hashed into the band it starts in — the whole
//! instruction, not the part of it inside the band — so an instruction that changed in
//! any way damages every band it touches. That is the conservative direction: a band is
//! rebuilt more often than it strictly had to be, never less, which is what makes a
//! damaged repaint indistinguishable from a full one.
//!
//! The digest is derived from the composed scene rather than from
//! [`astraterm_terminal::snapshot::Snapshot::damaged_rows`], because the scene is what
//! the renderer receives and the scene is also the only thing that carries the changes
//! the snapshot does not call damage: the cursor, the selection, search highlights and
//! image placement all move without a cell changing. No field is missing from the
//! snapshot for this; the scene already carries all of it.
//!
//! `Retained` (test builds only) is the same rule on the CPU: it holds the instructions
//! each band was last painted with, and a damaged repaint replaces exactly the bands the
//! damage names. The tests compare it against a full rebuild of the same scenes, which is
//! how "the output is identical to a full repaint" is proved without a GPU.

use std::ops::Range;

use astraterm_core::Rect;

use crate::paint::{ImageItem, Quad, Scene, TextItem};

/// Geometry a frame is drawn with.
///
/// Any change to it changes what every band holds, so a frame recorded with different
/// geometry damages the whole target: a resize, a font change that moves the row height,
/// or a clear colour that is no longer the one already in the target.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub size: (u32, u32),
    /// Height of one band, which is the height of one text row.
    pub band_height: u32,
    /// Colour the mosaic clears the target with, in target space.
    pub clear: [f32; 4],
}

impl Geometry {
    pub fn new(size: (u32, u32), band_height: u32, clear: [f32; 4]) -> Self {
        Self {
            size,
            band_height: band_height.max(1),
            clear,
        }
    }

    /// Bands the target is cut into.
    pub fn band_count(&self) -> usize {
        let height = self.size.1.max(1) as usize;
        height.div_ceil(self.band_height as usize)
    }

    /// Rectangle one band covers, full width.
    pub fn band_rect(&self, band: u32) -> Rect {
        let height = self.band_height;
        let top = band.saturating_mul(height);
        let bottom = (top + height).min(self.size.1);
        Rect::new(0, top as i32, self.size.0, bottom.saturating_sub(top))
    }

    /// Bands a rectangle reaches, clamped to the target.
    fn band_span(&self, rect: Rect) -> Range<usize> {
        if rect.is_empty() {
            return 0..0;
        }
        let top = rect.y.max(0) as u32;
        let bottom = (rect.bottom().max(0) as u32).min(self.size.1);
        if bottom <= top {
            return 0..0;
        }
        let height = self.band_height;
        let first = (top / height) as usize;
        let end = ((bottom - 1) / height + 1) as usize;
        first..end.min(self.band_count())
    }
}

/// Per-band digests of one frame's layers.
#[derive(Clone, Debug, PartialEq)]
pub struct Bands {
    geometry: Geometry,
    digests: Vec<u64>,
}

impl Bands {
    /// Digests the frame's instructions per band.
    pub fn record(scene: &Scene, overlay: Option<&Scene>, geometry: Geometry) -> Self {
        let mut digests = vec![OFFSET; geometry.band_count()];
        hash_layer(&mut digests, geometry, scene, LAYER_MOSAIC, None);
        if let Some(overlay) = overlay {
            hash_layer(&mut digests, geometry, overlay, LAYER_OVERLAY, overlay.clip);
        }
        Self { geometry, digests }
    }

}

/// Strips of the target a frame must rebuild, in device pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Damage {
    geometry: Geometry,
    bands: Vec<u32>,
    full: bool,
}

impl Damage {
    /// A frame that changed nothing: nothing is rebuilt and nothing is drawn.
    pub fn nothing(geometry: Geometry) -> Self {
        Self {
            geometry,
            bands: Vec::new(),
            full: false,
        }
    }

    /// Every band, for a first frame, a resize or any other change of geometry.
    pub fn everything(geometry: Geometry) -> Self {
        Self {
            geometry,
            bands: Vec::new(),
            full: true,
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.full && self.bands.is_empty()
    }

    pub fn is_full(&self) -> bool {
        self.full
    }

    /// Damaged bands, ascending.
    pub fn bands(&self) -> &[u32] {
        &self.bands
    }

    /// Rectangle one damaged band covers.
    pub fn band_rect(&self, band: u32) -> Rect {
        self.geometry.band_rect(band)
    }

    /// Whether anything in `rect` has to be rebuilt.
    pub fn touches(&self, rect: Rect) -> bool {
        if self.full {
            return true;
        }
        let span = self.geometry.band_span(rect);
        self.bands
            .iter()
            .any(|band| span.contains(&(*band as usize)))
    }
}

/// The bands that differ between two frames.
///
/// A frame with no previous recording, or one drawn with different geometry, is damaged
/// everywhere: there is no earlier pixel to keep.
pub fn plan(previous: Option<&Bands>, current: &Bands) -> Damage {
    let geometry = current.geometry;
    let Some(previous) = previous else {
        return Damage::everything(geometry);
    };
    if previous.geometry != geometry || previous.digests.len() != current.digests.len() {
        return Damage::everything(geometry);
    }
    let bands = previous
        .digests
        .iter()
        .zip(&current.digests)
        .enumerate()
        .filter(|(_, (before, now))| before != now)
        .map(|(band, _)| band as u32)
        .collect();
    Damage {
        geometry,
        bands,
        full: false,
    }
}

/// What the target holds, as the instructions that produced each band.
///
/// A damaged repaint replaces the instructions of the damaged bands with the current
/// frame's, and leaves every other band alone: it is the CPU mirror of what the
/// renderer's canvas does with a clear quad, a scissor and the frame's geometry, and it
/// exists so the equivalence can be asserted without a GPU.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub struct Retained {
    geometry: Geometry,
    bands: Vec<Vec<Instruction>>,
}

/// One drawing instruction, clipped to the band it was drawn in.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub enum Instruction {
    Quad(Quad),
    Image(ImageItem),
    Text(TextItem),
}

#[cfg(test)]
impl Retained {
    /// A target with nothing drawn in it yet.
    pub fn blank(geometry: Geometry) -> Self {
        Self {
            geometry,
            bands: vec![Vec::new(); geometry.band_count()],
        }
    }

    /// Repaints the damaged bands from `scene` and `overlay`, keeping the rest.
    pub fn repaint(&mut self, scene: &Scene, overlay: Option<&Scene>, damage: &Damage) {
        if damage.is_full() {
            for band in 0..self.bands.len() as u32 {
                self.bands[band as usize] = instructions(scene, overlay, self.geometry.band_rect(band));
            }
            return;
        }
        for band in damage.bands() {
            let Some(slot) = self.bands.get_mut(*band as usize) else {
                continue;
            };
            *slot = instructions(scene, overlay, self.geometry.band_rect(*band));
        }
    }

    /// Digest of what the target holds, over every band in order.
    ///
    /// Two targets with the same digest hold the same pixels as far as a scene can say
    /// so, which is what the tests compare.
    pub fn digest(&self) -> u64 {
        let mut digest = OFFSET;
        for band in 0..self.bands.len() {
            digest = mix(digest, self.band_digest(band));
        }
        digest
    }

    /// Digest of one band, so a failure names the strip that differs.
    pub fn band_digest(&self, band: usize) -> u64 {
        let mut digest = OFFSET;
        for instruction in self.bands.get(band).map(Vec::as_slice).unwrap_or(&[]) {
            digest = mix(digest, hash_instruction(instruction));
        }
        digest
    }
}

/// The target a full repaint of these layers produces.
#[cfg(test)]
pub fn full_repaint(scene: &Scene, overlay: Option<&Scene>, geometry: Geometry) -> Retained {
    let mut target = Retained::blank(geometry);
    target.repaint(scene, overlay, &Damage::everything(geometry));
    target
}

/// The instructions one band holds after a repaint of `scene` and `overlay`.
///
/// The mosaic paints the whole target; the overlay is scissored to its own box, so only
/// the part of the band inside that box receives its instructions.
#[cfg(test)]
fn instructions(scene: &Scene, overlay: Option<&Scene>, band: Rect) -> Vec<Instruction> {
    let mut out = Vec::new();
    collect(scene, band, &mut out);
    if let Some(overlay) = overlay {
        let visible = match overlay.clip {
            Some(clip) => intersect(clip, band),
            None => Some(band),
        };
        if let Some(visible) = visible {
            collect(overlay, visible, &mut out);
        }
    }
    out
}

/// Appends every instruction of `scene` that lands inside `region`, clipped to it.
#[cfg(test)]
fn collect(scene: &Scene, region: Rect, out: &mut Vec<Instruction>) {
    for quad in &scene.quads {
        if let Some(rect) = intersect(quad.rect, region) {
            out.push(Instruction::Quad(Quad {
                rect,
                color: quad.color,
            }));
        }
    }
    for image in &scene.images {
        if let Some(clipped) = clip_image(image, region) {
            out.push(Instruction::Image(clipped));
        }
    }
    for text in &scene.texts {
        if let Some(clip) = intersect(text.clip, region) {
            let mut clipped = text.clone();
            clipped.clip = clip;
            out.push(Instruction::Text(clipped));
        }
    }
}

/// An image clipped to `region`, with its source rectangle narrowed the same way.
#[cfg(test)]
fn clip_image(image: &ImageItem, region: Rect) -> Option<ImageItem> {
    let rect = intersect(image.rect, region)?;
    let (width, height) = (image.rect.width as f32, image.rect.height as f32);
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let (left, right) = (
        (rect.x - image.rect.x) as f32 / width,
        (rect.right() - image.rect.x) as f32 / width,
    );
    let (top, bottom) = (
        (rect.y - image.rect.y) as f32 / height,
        (rect.bottom() - image.rect.y) as f32 / height,
    );
    let mut clipped = image.clone();
    clipped.rect = rect;
    clipped.uv = [
        image.uv[0] + left * (image.uv[2] - image.uv[0]),
        image.uv[1] + top * (image.uv[3] - image.uv[1]),
        image.uv[0] + right * (image.uv[2] - image.uv[0]),
        image.uv[1] + bottom * (image.uv[3] - image.uv[1]),
    ];
    Some(clipped)
}

/// Overlap of two rectangles, `None` when they do not touch.
pub(crate) fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let left = a.x.max(b.x);
    let top = a.y.max(b.y);
    let right = a.right().min(b.right());
    let bottom = a.bottom().min(b.bottom());
    if right <= left || bottom <= top {
        return None;
    }
    Some(Rect::new(
        left,
        top,
        (right - left) as u32,
        (bottom - top) as u32,
    ))
}

/// Markers that keep the two layers' contributions apart.
const LAYER_MOSAIC: u64 = 0x1;
const LAYER_OVERLAY: u64 = 0x2;

/// FNV-1a: no dependency, no state, and enough of a digest to tell two frames' bands
/// apart.
const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const PRIME: u64 = 0x0000_0100_0000_01b3;

fn mix(hash: u64, value: u64) -> u64 {
    (hash ^ value).wrapping_mul(PRIME)
}

fn mix_bytes(hash: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(hash, |hash, byte| mix(hash, *byte as u64))
}

fn mix_f32(hash: u64, value: f32) -> u64 {
    mix(hash, value.to_bits() as u64)
}

fn hash_rect(rect: Rect) -> u64 {
    let mut hash = mix(OFFSET, rect.x as u32 as u64);
    hash = mix(hash, rect.y as u32 as u64);
    hash = mix(hash, rect.width as u64);
    mix(hash, rect.height as u64)
}

fn hash_quad(quad: &Quad) -> u64 {
    let mut hash = mix(hash_rect(quad.rect), 0x11);
    for channel in quad.color {
        hash = mix_f32(hash, channel);
    }
    hash
}

fn hash_text(text: &TextItem) -> u64 {
    let mut hash = mix(hash_rect(text.clip), 0x22);
    hash = mix(hash, text.id);
    hash = mix_f32(hash, text.position.0);
    hash = mix_f32(hash, text.position.1);
    hash = mix(hash, text.on_grid as u64);
    for span in &text.spans {
        hash = mix_bytes(hash, span.text.as_bytes());
        hash = mix(hash, span.bold as u64);
        hash = mix(hash, span.italic as u64);
        for channel in span.color {
            hash = mix_f32(hash, channel);
        }
    }
    hash
}

fn hash_image(image: &ImageItem) -> u64 {
    let mut hash = mix(hash_rect(image.rect), 0x33);
    hash = mix(hash, image.id);
    hash = mix(hash, image.generation);
    for value in image.uv {
        hash = mix_f32(hash, value);
    }
    hash
}

#[cfg(test)]
fn hash_instruction(instruction: &Instruction) -> u64 {
    match instruction {
        Instruction::Quad(quad) => hash_quad(quad),
        Instruction::Image(image) => hash_image(image),
        Instruction::Text(text) => hash_text(text),
    }
}

/// Mixes one layer's instructions into the band digests.
///
/// `clip` is the scissor the layer is drawn with: the overlay is confined to its own
/// box, so neither the box nor an instruction it hides can change a band outside it.
fn hash_layer(
    digests: &mut [u64],
    geometry: Geometry,
    layer: &Scene,
    marker: u64,
    clip: Option<Rect>,
) {
    let target = Rect::new(0, 0, geometry.size.0, geometry.size.1);
    let region = match clip {
        Some(clip) => match intersect(clip, target) {
            Some(region) => region,
            None => return,
        },
        None => target,
    };
    for band in geometry.band_span(region) {
        digests[band] = mix(digests[band], marker);
        // The scissor is part of what the band shows.
        digests[band] = mix(digests[band], hash_rect(region));
    }
    let mut visit = |rect: Rect, hash: u64| {
        let Some(rect) = intersect(rect, region) else {
            return;
        };
        for band in geometry.band_span(rect) {
            digests[band] = mix(digests[band], hash);
        }
    };
    for quad in &layer.quads {
        visit(quad.rect, hash_quad(quad));
    }
    for image in &layer.images {
        visit(image.rect, hash_image(image));
    }
    for text in &layer.texts {
        // A text item's own clip is the strip it paints into.
        visit(text.clip, hash_text(text));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use astraterm_config::palette::Rgb;

    use super::*;
    use crate::paint::{ImagePixels, TextItem};

    /// Five bands of ten device pixels.
    fn geometry() -> Geometry {
        Geometry::new((100, 50), 10, [0.0, 0.0, 0.0, 1.0])
    }

    const BANDS: usize = 5;

    fn row_item(row: usize, text: &str) -> TextItem {
        let top = row as i32 * geometry().band_height as i32;
        TextItem::plain(
            row as u64,
            (0.0, top as f32),
            text,
            [1.0, 1.0, 1.0, 1.0],
            Rect::new(0, top, 100, geometry().band_height),
        )
    }

    /// A screen of `rows` rows of text plus the cursor block on the first row.
    fn screen(rows: usize, text: &str) -> Scene {
        let mut scene = Scene::with_clear(Rgb::new(0, 0, 0), 1.0);
        for row in 0..rows {
            scene.push_text(row_item(row, text));
        }
        scene
    }

    fn cursor_scene(scene: &mut Scene, row: usize) {
        scene.push_quad(
            Rect::new(8, row as i32 * 10, 8, 10),
            Rgb::new(200, 200, 200),
            1.0,
        );
    }

    fn image(id: u64, x: i32, y: i32) -> ImageItem {
        let pixels = Arc::new(ImagePixels::new(2, 2, vec![255; 16]).expect("2x2 pixels"));
        ImageItem::new(id, Rect::new(x, y, 20, 20), pixels, 1)
    }

    #[test]
    fn the_first_frame_damages_everything_and_a_repeat_damages_nothing() {
        let geometry = geometry();
        let scene = screen(BANDS, "hello");
        let first = Bands::record(&scene, None, geometry);
        assert!(plan(None, &first).is_full(), "nothing was drawn yet");

        let again = Bands::record(&scene, None, geometry);
        let damage = plan(Some(&first), &again);
        assert!(
            damage.is_empty(),
            "an unchanged frame draws nothing: {:?}",
            damage.bands()
        );
    }

    #[test]
    fn one_changed_row_damages_only_its_band() {
        let geometry = geometry();
        let before = Bands::record(&screen(BANDS, "hello"), None, geometry);
        let mut scene = screen(BANDS, "hello");
        scene.texts[2] = row_item(2, "changed");
        let after = Bands::record(&scene, None, geometry);
        let damage = plan(Some(&before), &after);
        assert_eq!(damage.bands(), &[2], "only the row that changed");
    }

    #[test]
    fn a_cursor_blink_damages_the_cursors_row() {
        // The cursor is a quad painted over the cell it sits on. Blinking is not cell
        // damage, so it has to reach the plan on its own.
        let geometry = geometry();
        let off = screen(BANDS, "hello");
        let mut on = off.clone();
        cursor_scene(&mut on, 2);
        let before = Bands::record(&off, None, geometry);
        let after = Bands::record(&on, None, geometry);
        let damage = plan(Some(&before), &after);
        assert_eq!(damage.bands(), &[2], "the cursor's own row");
    }

    #[test]
    fn a_selection_highlight_damages_the_rows_it_covers() {
        let geometry = geometry();
        let scene = screen(BANDS, "hello");
        let before = Bands::record(&scene, None, geometry);
        let mut selected = scene.clone();
        for row in 1..3 {
            selected.push_quad_rgba(
                Rect::new(0, row * 10, 50, 10),
                [0.2, 0.3, 0.8, 1.0],
            );
        }
        let after = Bands::record(&selected, None, geometry);
        assert_eq!(plan(Some(&before), &after).bands(), &[1, 2]);
    }

    #[test]
    fn a_search_highlight_damages_the_row_it_marks() {
        let geometry = geometry();
        let scene = screen(BANDS, "hello");
        let before = Bands::record(&scene, None, geometry);
        let mut searched = scene.clone();
        searched.push_quad_rgba(Rect::new(24, 30, 16, 10), [0.9, 0.8, 0.1, 0.65]);
        let after = Bands::record(&searched, None, geometry);
        assert_eq!(plan(Some(&before), &after).bands(), &[3]);
    }

    #[test]
    fn an_image_that_moved_damages_where_it_was_and_where_it_is() {
        let geometry = geometry();
        let mut scene = screen(BANDS, "hello");
        scene.push_image(image(1, 10, 10));
        let before = Bands::record(&scene, None, geometry);

        let mut moved = screen(BANDS, "hello");
        moved.push_image(image(1, 10, 30));
        let after = Bands::record(&moved, None, geometry);
        assert_eq!(
            plan(Some(&before), &after).bands(),
            &[1, 2, 3, 4],
            "rows 1..3 (the old placement) and 3..5 (the new one)"
        );
    }

    #[test]
    fn a_clear_colour_change_damages_every_band() {
        let geometry = geometry();
        let before = Bands::record(&screen(BANDS, "hello"), None, geometry);
        let mut darker = screen(BANDS, "hello");
        darker.clear = [0.1, 0.1, 0.1, 1.0];
        let changed = Geometry::new(
            geometry.size,
            geometry.band_height,
            darker.clear,
        );
        let after = Bands::record(&darker, None, changed);
        assert!(plan(Some(&before), &after).is_full());
    }

    #[test]
    fn a_resize_damages_every_band() {
        let before = Bands::record(&screen(BANDS, "hello"), None, geometry());
        let taller = Geometry::new((100, 80), 10, [0.0, 0.0, 0.0, 1.0]);
        let after = Bands::record(&screen(BANDS, "hello"), None, taller);
        assert!(plan(Some(&before), &after).is_full());
    }

    #[test]
    fn damage_touches_only_the_rectangles_its_bands_cover() {
        let geometry = geometry();
        let mut damage = Damage::nothing(geometry);
        assert!(!damage.touches(Rect::new(0, 0, 100, 50)));
        damage.bands = vec![0, 3];
        assert!(damage.touches(Rect::new(0, 0, 100, 10)), "band zero");
        assert!(!damage.touches(Rect::new(0, 10, 100, 10)), "band one");
        assert!(damage.touches(Rect::new(0, 30, 100, 10)), "band three");
        assert!(
            !damage.touches(Rect::new(0, 100, 100, 10)),
            "outside the target"
        );
        assert!(Damage::everything(geometry).touches(Rect::new(0, 30, 1, 1)));
    }

    #[test]
    fn a_damaged_repaint_holds_exactly_what_a_full_repaint_holds() {
        // S7: the point of the whole module. A sequence of edits — text, scroll, cursor
        // blink, selection, highlight, an image moving — repainted band by band has to
        // leave the target holding exactly what a full rebuild of the last scene holds.
        let geometry = geometry();
        let mut frames: Vec<Scene> = Vec::new();

        let mut text = screen(BANDS, "hello");
        frames.push(text.clone());
        text.texts[1] = row_item(1, "changed");
        frames.push(text.clone());
        // Scroll: every row moves up and a new row arrives at the bottom.
        let mut scrolled = screen(BANDS, "hello");
        for row in 0..BANDS {
            scrolled.texts[row] = row_item(row, if row < 4 { "scrolled" } else { "new" });
        }
        frames.push(scrolled.clone());
        // Cursor blink on.
        let mut blinking = scrolled.clone();
        cursor_scene(&mut blinking, 2);
        frames.push(blinking.clone());
        // Cursor blink off again.
        frames.push(scrolled.clone());
        // Selection over two rows.
        let mut selected = scrolled.clone();
        for row in 0..2 {
            selected.push_quad_rgba(
                Rect::new(0, row * 10, 60, 10),
                [0.2, 0.3, 0.8, 1.0],
            );
        }
        frames.push(selected.clone());
        // Search highlight.
        let mut searched = selected.clone();
        searched.push_quad_rgba(Rect::new(20, 20, 16, 10), [0.9, 0.8, 0.1, 0.65]);
        frames.push(searched.clone());
        // An image arriving, then moving.
        let mut with_image = searched.clone();
        with_image.push_image(image(7, 10, 10));
        frames.push(with_image.clone());
        with_image.images[0] = image(7, 10, 20);
        frames.push(with_image);

        let mut retained = Retained::blank(geometry);
        let mut previous: Option<Bands> = None;
        for (index, scene) in frames.iter().enumerate() {
            let bands = Bands::record(scene, None, geometry);
            let damage = plan(previous.as_ref(), &bands);
            if index > 0 {
                assert!(
                    !damage.is_full(),
                    "edit {index} rebuilt everything instead of its bands"
                );
            }
            retained.repaint(scene, None, &damage);
            let full = full_repaint(scene, None, geometry);
            for band in 0..BANDS {
                assert_eq!(
                    retained.band_digest(band),
                    full.band_digest(band),
                    "band {band} after edit {index}"
                );
            }
            assert_eq!(retained.digest(), full.digest(), "after edit {index}");
            previous = Some(bands);
        }
    }

    #[test]
    fn the_overlay_layer_is_repainted_with_the_mosaic_band() {
        // The overlay is drawn over the mosaic, so a band it changes has to bring the
        // mosaic back as well: the model repaints the whole band, both layers.
        let geometry = geometry();
        let mosaic = screen(BANDS, "hello");
        let mut overlay = Scene {
            clip: Some(Rect::new(0, 10, 100, 20)),
            ..Scene::default()
        };
        overlay.push_quad(Rect::new(0, 10, 100, 20), Rgb::new(9, 9, 9), 0.9);
        overlay.push_quad(Rect::new(0, 0, 10, 10), Rgb::new(9, 9, 9), 0.9);

        let before = Bands::record(&mosaic, None, geometry);
        let after = Bands::record(&mosaic, Some(&overlay), geometry);
        let damage = plan(Some(&before), &after);
        assert_eq!(damage.bands(), &[1, 2], "the overlay's own box");

        let mut retained = Retained::blank(geometry);
        retained.repaint(&mosaic, None, &Damage::everything(geometry));
        retained.repaint(&mosaic, Some(&overlay), &damage);
        let full = full_repaint(&mosaic, Some(&overlay), geometry);
        assert_eq!(retained.digest(), full.digest());
    }
}
