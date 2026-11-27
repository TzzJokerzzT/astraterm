//! Text shaping, font metrics and the per-item glyph buffer cache.
//!
//! Rows are shaped once and re-shaped only when their content or style changes,
//! which is what keeps scrolling and idle redraws cheap: glyphon re-uploads glyph
//! quads from its atlas each frame, but cosmic-text layout is skipped.
//!
//! The `FontSystem` lives next to the cache (not inside it) so the renderer can hold
//! shaped buffers and the mutable font system at the same time.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use astraterm_config::schema::FontConfig;
use astraterm_core::CellMetrics;
use cosmic_text::{
    Attrs, Buffer, CacheKeyFlags, Family, FeatureTag, FontFeatures, FontSystem, Metrics, Shaping,
    Style, Weight,
};
use glyphon::{Color as GlyphonColor, TextArea, TextBounds};

use crate::paint::{ANCHOR_TOLERANCE, Scene, TextItem};

/// Resolved shaping style for the terminal font.
#[derive(Clone, Debug)]
pub struct TextStyle {
    pub family: String,
    pub weight: Weight,
    pub letter_spacing: f32,
    /// Ligatures requested by configuration.
    pub ligatures: bool,
    /// Hint glyph outlines to the pixel grid, as requested by configuration.
    pub hinting: bool,
    pub metrics: Metrics,
    pub cell: CellMetrics,
    pub scale: f32,
    /// Whether a single printable ASCII cell in the primary face is known to
    /// advance exactly one column; see [`TextStyle::ascii_is_one_column`].
    pub ascii_is_one_column: bool,
    /// Hash of the inputs that affect shaping; changing it invalidates buffers.
    pub key: u64,
}

impl TextStyle {
    /// Builds a style from configuration and measures its cell metrics.
    pub fn new(font: &FontConfig, scale: f32, font_system: &mut FontSystem) -> Self {
        let scale = scale.max(0.1);
        let (metrics, cell) = measure_style(font_system, font, scale);
        let mut style = Self {
            family: font.family.clone(),
            weight: Weight(font.weight),
            letter_spacing: font.letter_spacing,
            ligatures: font.ligatures,
            hinting: font.hinting,
            metrics,
            cell,
            scale,
            ascii_is_one_column: false,
            key: style_key(font, scale),
        };
        style.ascii_is_one_column = measure_ascii_is_one_column(font_system, &style);
        style
    }

    /// Whether the configured family is installed.
    pub fn family_available(&self, font_system: &FontSystem) -> bool {
        font_system.db().faces().any(|face| {
            face.families
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(&self.family))
        })
    }

    /// Family actually used for shaping: falls back to `monospace` when missing.
    pub fn effective_family(&self, font_system: &FontSystem) -> &str {
        if self.family_available(font_system) {
            &self.family
        } else {
            "monospace"
        }
    }
}

struct CachedBuffer {
    buffer: Buffer,
    hash: u64,
    style_key: u64,
}

/// Upper bound on cached rows; beyond this the cache is dropped wholesale.
const MAX_CACHED_ITEMS: usize = 4096;

/// Cache of shaped text items keyed by their stable identifier.
#[derive(Default)]
pub struct RowCache {
    rows: HashMap<u64, CachedBuffer>,
}

impl RowCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn clear(&mut self) {
        self.rows.clear();
    }

    /// Shapes every item that changed since the last call.
    pub fn update(&mut self, font_system: &mut FontSystem, style: &TextStyle, scene: &Scene) {
        if self.rows.len() > MAX_CACHED_ITEMS {
            self.rows.clear();
        }
        for item in &scene.texts {
            let hash = item.content_hash();
            let rebuild = match self.rows.get(&item.id) {
                Some(cached) => cached.hash != hash || cached.style_key != style.key,
                None => true,
            };
            if !rebuild {
                continue;
            }
            let buffer = shape(font_system, style, item);
            self.rows.insert(
                item.id,
                CachedBuffer {
                    buffer,
                    hash,
                    style_key: style.key,
                },
            );
        }
    }

    /// Text areas ready to be handed to glyphon.
    pub fn areas<'a>(&'a self, scene: &'a Scene) -> Vec<TextArea<'a>> {
        let mut areas = Vec::with_capacity(scene.texts.len());
        for item in &scene.texts {
            let Some(cached) = self.rows.get(&item.id) else {
                continue;
            };
            areas.push(TextArea {
                buffer: &cached.buffer,
                left: item.position.0,
                top: item.position.1,
                scale: 1.0,
                bounds: TextBounds {
                    left: item.clip.x,
                    top: item.clip.y,
                    right: item.clip.right(),
                    bottom: item.clip.bottom(),
                },
                default_color: GlyphonColor::rgb(255, 255, 255),
                custom_glyphs: &[],
            });
        }
        areas
    }
}

/// Maximum on cached cluster advances.
///
/// The cache evicts its oldest measurements when it reaches this count instead of
/// dropping everything, so the bound has to be larger than the number of distinct
/// clusters a screen can hold: a screen wider than the bound reshapes its whole row
/// set on every frame no matter how the cache evicts. A 4K display at the default
/// font holds about 60 000 cells, so 64 Ki rows cover it. An entry is a shared
/// cluster string plus a float, so even a cache full of distinct CJK clusters stays
/// a few megabytes.
const MAX_CACHED_CLUSTERS: usize = 1 << 16;

/// Measured advances of grapheme clusters, keyed by shaping style and cluster text.
///
/// [`crate::paint`] decides which clusters get their own grid-anchored segment, and
/// that decision has to come from the shaped advance instead of a codepoint list: a
/// fallback script no list would cover drifts exactly like an emoji does (measured
/// with the shipped default config, devanagari `क` advances 1.280 cells while the
/// grid reserves one).
///
/// Measurement is per cluster, so a script whose advances depend on neighbouring
/// clusters — Arabic joining is the obvious case — is measured approximately, as if
/// every cluster stood alone. The limitation is accepted: a cluster measured wider
/// than the columns it owns is anchored, which costs it its own segment and nothing
/// else, so the grid stays exact even where the advance is approximate.
///
/// The cache is bounded but never wiped: at the bound it evicts the measurements
/// inserted longest ago, so a screen that churns through more clusters than the old
/// clear-at-capacity behaviour held keeps the recent ones resident instead of
/// reshaping everything on the next frame.
pub struct ClusterWidths {
    /// One map per shaping style, so a font or scale change leaves stale entries
    /// behind without ever reading them back.
    ///
    /// The key is a shared `Arc<str>`, so looking one up costs no allocation and the
    /// eviction queue below can hold another reference to it without copying text.
    widths: HashMap<u64, HashMap<Arc<str>, f32>>,
    /// Keys in insertion order, so a full cache evicts the oldest measurement.
    order: VecDeque<(u64, Arc<str>)>,
    /// Entries across every style, for bounding.
    len: usize,
    /// Entries kept before the oldest is evicted.
    capacity: usize,
    /// [`Self::advance`] calls, so a test can assert a row never reached the cache.
    lookups: u64,
    /// Clusters actually shaped, so a test can assert a cached advance was reused.
    measurements: u64,
}

impl Default for ClusterWidths {
    fn default() -> Self {
        Self::with_capacity(MAX_CACHED_CLUSTERS)
    }
}

impl ClusterWidths {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cache that evicts its oldest measurement past `capacity` entries.
    ///
    /// The bound is a seam: production uses [`MAX_CACHED_CLUSTERS`], and the tests
    /// use a handful of entries so filling the cache past its bound stays cheap.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            widths: HashMap::new(),
            order: VecDeque::new(),
            len: 0,
            // A cache that keeps nothing would measure every cluster of every frame.
            capacity: capacity.max(1),
            lookups: 0,
            measurements: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Entries the cache keeps before evicting the oldest.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Advance lookups served, cached or not.
    pub fn lookups(&self) -> u64 {
        self.lookups
    }

    /// Advances actually shaped, one per cache miss.
    pub fn measurements(&self) -> u64 {
        self.measurements
    }

    pub fn clear(&mut self) {
        self.widths.clear();
        self.order.clear();
        self.len = 0;
    }

    /// Advance of `cluster` in device pixels, shaped with `style`'s attributes.
    ///
    /// Cached per style and cluster, so a row re-walked every frame costs one map
    /// lookup per cluster instead of one shape.
    pub fn advance(
        &mut self,
        font_system: &mut FontSystem,
        style: &TextStyle,
        cluster: &str,
    ) -> f32 {
        self.lookups += 1;
        if let Some(width) = self
            .widths
            .get(&style.key)
            .and_then(|widths| widths.get(cluster))
        {
            return *width;
        }
        let width = measure_cluster(font_system, style, cluster);
        self.measurements += 1;
        self.insert(style.key, cluster, width);
        width
    }

    /// Stores one measurement, evicting the oldest entries until it fits.
    ///
    /// Eviction never drops the whole cache: the entries inserted most recently
    /// survive, which is what keeps a screen whose distinct clusters exceed the bound
    /// from reshaping every cluster again on the next frame.
    fn insert(&mut self, style_key: u64, cluster: &str, width: f32) {
        while self.len >= self.capacity {
            let Some((evicted_style, evicted)) = self.order.pop_front() else {
                break;
            };
            let removed = self
                .widths
                .get_mut(&evicted_style)
                .and_then(|widths| widths.remove(evicted.as_ref()))
                .is_some();
            if removed {
                self.len -= 1;
            }
        }
        let key: Arc<str> = Arc::from(cluster);
        let replaced = self
            .widths
            .entry(style_key)
            .or_default()
            .insert(Arc::clone(&key), width)
            .is_some();
        self.order.push_back((style_key, key));
        if !replaced {
            self.len += 1;
        }
    }
}

/// Everything [`crate::paint::paint_pane`] needs to measure a cluster.
///
/// The renderer owns the font system, the shaping style and the cache as three
/// separate fields, so this borrows all three for the duration of one frame's
/// painting. Bundling them keeps the painting API at one extra parameter and keeps
/// the borrow inside the renderer: a scene is composed through `&mut Renderer`, so
/// paint never has to reshape a whole row to learn how wide one cluster is.
pub struct ClusterMeasurer<'a> {
    font_system: &'a mut FontSystem,
    style: &'a TextStyle,
    widths: &'a mut ClusterWidths,
}

impl<'a> ClusterMeasurer<'a> {
    pub fn new(
        font_system: &'a mut FontSystem,
        style: &'a TextStyle,
        widths: &'a mut ClusterWidths,
    ) -> Self {
        Self {
            font_system,
            style,
            widths,
        }
    }

    /// Advance of `cluster` in device pixels under the terminal's shaping attributes.
    pub fn advance(&mut self, cluster: &str) -> f32 {
        self.widths.advance(self.font_system, self.style, cluster)
    }

    /// Whether one printable ASCII cell in the primary face is known to advance
    /// exactly the column the grid reserves for it.
    ///
    /// [`crate::paint`] anchors a cluster when its measured advance disagrees with
    /// its columns, so when this holds the walk can treat such a cell as unanchored
    /// without measuring it or consulting the cache at all (B19).
    pub fn ascii_is_one_column(&self) -> bool {
        self.style.ascii_is_one_column
    }

    /// Advance lookups this measurer served, cached or not.
    pub fn lookups(&self) -> u64 {
        self.widths.lookups()
    }

    /// Advances this measurer actually shaped.
    pub fn measurements(&self) -> u64 {
        self.widths.measurements()
    }
}

/// Shapes one cluster with exactly the attributes [`shape`] gives a regular span.
///
/// Family, weight, style, ligature features and non-zero letter spacing all match,
/// so the measured advance is the advance the painted segment will use. Bold and
/// italic cells are measured at the base weight and upright: the model reserves the
/// same columns for them, and a face whose bold advance differs is anchored as if it
/// were the regular one.
fn measure_cluster(font_system: &mut FontSystem, style: &TextStyle, text: &str) -> f32 {
    let mut buffer = Buffer::new(font_system, style.metrics);
    buffer.set_size(Some(1.0e6), Some(style.metrics.line_height));
    let family = style.family.clone();
    let mut attrs = Attrs::new()
        .family(Family::Name(&family))
        .weight(style.weight)
        .cache_key_flags(cache_key_flags(style.hinting))
        .font_features(font_features(style.ligatures));
    if style.letter_spacing.abs() > f32::EPSILON {
        attrs = attrs.letter_spacing(style.letter_spacing);
    }
    buffer.set_text(text, &attrs, Shaping::Advanced, None);
    buffer.shape_until_scroll(font_system, false);
    // A cluster never carries a line break, so the first run is the whole of it.
    buffer
        .layout_runs()
        .next()
        .map(|run| run.line_w)
        .unwrap_or(0.0)
}

/// Whether every printable ASCII cell advances exactly one column in this style.
///
/// The row walk anchors a cluster whose measured advance disagrees with the columns
/// the grid reserved for it (S8), and that measurement is what a plain row used to
/// pay per cell. For a monospace primary face no ASCII cell ever disagrees, so the
/// walk can skip the measurement — but only once it has been *checked*: a
/// proportional family configured as the terminal font drifts on its narrowest
/// characters exactly like a fallback script does, and anchoring those is existing
/// behaviour. Each cell is therefore measured the way the walk would measure it, once
/// per style instead of once per cell per frame.
fn measure_ascii_is_one_column(font_system: &mut FontSystem, style: &TextStyle) -> bool {
    let mut buffer = [0u8; 4];
    (' '..='~').all(|c| {
        let advance = measure_cluster(font_system, style, c.encode_utf8(&mut buffer));
        (advance - style.cell.width).abs() <= ANCHOR_TOLERANCE
    })
}

fn shape(font_system: &mut FontSystem, style: &TextStyle, item: &TextItem) -> Buffer {
    let mut buffer = Buffer::new(font_system, style.metrics);
    // The terminal decides where lines break, so the shaper must not wrap.
    buffer.set_size(Some(1.0e6), Some(style.metrics.line_height));

    let family = style.family.clone();
    let features = font_features(style.ligatures);
    let spans: Vec<(String, Attrs<'_>)> = item
        .spans
        .iter()
        .map(|span| {
            let mut attrs = Attrs::new()
                .family(Family::Name(&family))
                .weight(if span.bold {
                    Weight::BOLD
                } else {
                    style.weight
                })
                .style(if span.italic {
                    Style::Italic
                } else {
                    Style::Normal
                })
                .color(cosmic_text::Color::rgba(
                    (span.color[0] * 255.0) as u8,
                    (span.color[1] * 255.0) as u8,
                    (span.color[2] * 255.0) as u8,
                    (span.color[3] * 255.0) as u8,
                ))
                .cache_key_flags(cache_key_flags(style.hinting))
                .font_features(features.clone());
            if style.letter_spacing.abs() > f32::EPSILON {
                attrs = attrs.letter_spacing(style.letter_spacing);
            }
            (span.text.clone(), attrs)
        })
        .collect();

    let span_refs: Vec<(&str, Attrs<'_>)> = spans
        .iter()
        .map(|(text, attrs)| (text.as_str(), attrs.clone()))
        .collect();
    let default_attrs = Attrs::new()
        .family(Family::Name(&family))
        .weight(style.weight)
        .cache_key_flags(cache_key_flags(style.hinting))
        .font_features(features);
    buffer.set_rich_text(span_refs, &default_attrs, Shaping::Advanced, None);
    buffer.shape_until_scroll(font_system, false);
    buffer
}

/// OpenType features that decide whether ligatures reach the shaper.
///
/// Both directions are stated explicitly: the shaper enables `liga` and `calt` on
/// its own, so a terminal that never touched features still gets whatever the font
/// substitutes. Spelling the value out makes the configuration decide, not the
/// shaper's defaults.
pub fn font_features(ligatures: bool) -> FontFeatures {
    let mut features = FontFeatures::new();
    if ligatures {
        features.enable(FeatureTag::STANDARD_LIGATURES);
        features.enable(FeatureTag::CONTEXTUAL_ALTERNATES);
    } else {
        features.disable(FeatureTag::STANDARD_LIGATURES);
        features.disable(FeatureTag::CONTEXTUAL_ALTERNATES);
        features.disable(FeatureTag::DISCRETIONARY_LIGATURES);
    }
    features
}

/// Glyph-cache flags that decide how the rasteriser treats the outline.
///
/// cosmic-text hints by default — `swash.rs` builds its scaler with
/// `hint(!flags.contains(DISABLE_HINTING))` — so an unhinted terminal has to say so
/// on every attribute it shapes with: the flag travels from `Attrs` into the
/// `CacheKey` of each laid-out glyph, and that key is what glyphon's atlas uses to
/// rasterise it. Setting it in one shaping path only would show one texture for a
/// measured advance and another for the painted glyph.
fn cache_key_flags(hinting: bool) -> CacheKeyFlags {
    if hinting {
        CacheKeyFlags::empty()
    } else {
        CacheKeyFlags::DISABLE_HINTING
    }
}

/// Shaping metrics and cell metrics for a font configuration.
///
/// With `integer_cell_width` off this is the historical measurement, untouched.
///
/// With it on, the grid width is rounded to a whole pixel *and the font is stretched
/// to match*: rounding the grid alone would leave the shaper advancing the fractional
/// width, and every row would walk away from the grid again, which is the drift
/// [`crate::paint`] anchors rows to prevent. The font is therefore measured, scaled by
/// `target / advance`, and measured again at the adjusted size; both the shaping
/// metrics and the cell come from that second measurement, with the cell width set to
/// the target exactly. The stretch is why the option is opt-in: it enlarges the font
/// and costs a column or two on a wide pane.
fn measure_style(
    font_system: &mut FontSystem,
    font: &FontConfig,
    scale: f32,
) -> (Metrics, CellMetrics) {
    let natural = measure_cell(font_system, font, scale);
    if !font.integer_cell_width {
        let metrics = Metrics::new(font.size * scale, (font.size * font.line_height) * scale);
        return (metrics, natural);
    }
    // `measure_cell` never reports less than one pixel, so the factor is finite.
    let target = natural.width.round().max(1.0);
    let mut cell = measure_cell(font_system, font, scale * (target / natural.width));
    cell.width = target;
    // `cell.baseline` is the adjusted font size, and `Renderer::cell_metrics()` along
    // with the app's grid computation read the baseline as the size, so the shaping
    // metrics take their size from it too.
    let metrics = Metrics::new(cell.baseline, cell.baseline * font.line_height);
    (metrics, cell)
}

/// Measures the cell advance, line height and baseline for a font configuration.
///
/// Falls back to a conservative fixed cell when the font cannot be measured, so a
/// missing font still produces a usable (if plain) grid.
pub fn measure_cell(font_system: &mut FontSystem, font: &FontConfig, scale: f32) -> CellMetrics {
    let size = (font.size * scale).max(1.0);
    let line_height = (size * font.line_height).round().max(1.0);
    let metrics = Metrics::new(size, line_height);
    let mut buffer = Buffer::new(font_system, metrics);
    buffer.set_size(Some(4096.0), Some(line_height));
    let attrs = Attrs::new()
        .family(Family::Name(&font.family))
        .weight(Weight(font.weight))
        .cache_key_flags(cache_key_flags(font.hinting));
    // Ten ASCII cells: dividing the shaped width averages out rounding.
    buffer.set_text("MMMMMMMMMM", &attrs, Shaping::Advanced, None);
    buffer.shape_until_scroll(font_system, false);

    // Only the first shaped run is measured: the string has no line breaks.
    let advance = buffer
        .layout_runs()
        .next()
        .map(|run| run.line_w / 10.0)
        .filter(|width| *width > 1.0);

    let width = advance.unwrap_or((size * 0.6).max(4.0)).max(1.0);
    CellMetrics {
        width,
        height: line_height as u32,
        baseline: size,
    }
}

fn style_key(font: &FontConfig, scale: f32) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    font.family.hash(&mut hasher);
    font.size.to_bits().hash(&mut hasher);
    font.weight.hash(&mut hasher);
    font.line_height.to_bits().hash(&mut hasher);
    font.letter_spacing.to_bits().hash(&mut hasher);
    font.ligatures.hash(&mut hasher);
    font.integer_cell_width.hash(&mut hasher);
    font.hinting.hash(&mut hasher);
    scale.to_bits().hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use astraterm_core::Rect;

    fn font() -> FontConfig {
        FontConfig {
            family: "monospace".to_owned(),
            size: 13.0,
            ..Default::default()
        }
    }

    fn scene_with(id: u64, text: &str) -> Scene {
        let mut scene = Scene::default();
        scene.push_text(TextItem::plain(
            id,
            (0.0, 0.0),
            text,
            [1.0; 4],
            Rect::new(0, 0, 500, 20),
        ));
        scene
    }

    #[test]
    fn cell_metrics_are_positive_and_scale_up() {
        let mut system = FontSystem::new();
        let normal = measure_cell(&mut system, &font(), 1.0);
        assert!(normal.width >= 1.0);
        assert!(normal.height >= 13);
        let hidpi = measure_cell(&mut system, &font(), 2.0);
        assert!(hidpi.width >= normal.width);
        assert!(hidpi.height >= normal.height);
    }

    #[test]
    fn shaping_populates_the_cache_and_is_reused() {
        let mut system = FontSystem::new();
        let style = TextStyle::new(&font(), 1.0, &mut system);
        let mut cache = RowCache::new();
        let scene = scene_with(1, "hello");
        cache.update(&mut system, &style, &scene);
        assert_eq!(cache.len(), 1);
        let areas = cache.areas(&scene);
        assert_eq!(areas.len(), 1);
        cache.update(&mut system, &style, &scene);
        assert_eq!(cache.len(), 1, "unchanged rows must not be reshaped");
    }

    #[test]
    fn changed_content_reshapes_the_row() {
        let mut system = FontSystem::new();
        let style = TextStyle::new(&font(), 1.0, &mut system);
        let mut cache = RowCache::new();
        cache.update(&mut system, &style, &scene_with(7, "first"));
        let before = cache.rows.get(&7).map(|cached| cached.hash).unwrap();
        cache.update(&mut system, &style, &scene_with(7, "second"));
        let after = cache.rows.get(&7).map(|cached| cached.hash).unwrap();
        assert_ne!(before, after);
    }

    #[test]
    fn style_change_invalidates_cached_rows() {
        let mut system = FontSystem::new();
        let mut style = TextStyle::new(&font(), 1.0, &mut system);
        let mut cache = RowCache::new();
        let scene = scene_with(1, "abc");
        cache.update(&mut system, &style, &scene);
        let bigger = FontConfig {
            size: 20.0,
            ..font()
        };
        style = TextStyle::new(&bigger, 1.0, &mut system);
        cache.update(&mut system, &style, &scene);
        assert_eq!(
            cache.rows.get(&1).map(|cached| cached.style_key),
            Some(style.key)
        );
    }

    #[test]
    fn empty_items_are_not_shaped() {
        let mut system = FontSystem::new();
        let style = TextStyle::new(&font(), 1.0, &mut system);
        let mut cache = RowCache::new();
        let mut scene = Scene::default();
        scene.push_text(TextItem::plain(
            3,
            (0.0, 0.0),
            "",
            [1.0; 4],
            Rect::new(0, 0, 10, 10),
        ));
        cache.update(&mut system, &style, &scene);
        assert!(cache.areas(&scene).is_empty());
    }

    #[test]
    fn ligature_features_are_explicit_in_both_directions() {
        let enabled = font_features(true);
        assert!(contains(&enabled, FeatureTag::STANDARD_LIGATURES, 1));
        assert!(contains(&enabled, FeatureTag::CONTEXTUAL_ALTERNATES, 1));

        let disabled = font_features(false);
        assert!(contains(&disabled, FeatureTag::STANDARD_LIGATURES, 0));
        assert!(contains(&disabled, FeatureTag::CONTEXTUAL_ALTERNATES, 0));
        assert!(contains(&disabled, FeatureTag::DISCRETIONARY_LIGATURES, 0));
    }

    #[test]
    fn ligatures_are_off_by_default() {
        assert!(!FontConfig::default().ligatures);
    }

    #[test]
    fn the_style_carries_the_configured_ligature_choice() {
        let mut system = FontSystem::new();
        let off = TextStyle::new(&font(), 1.0, &mut system);
        let on = TextStyle::new(
            &FontConfig {
                ligatures: true,
                ..font()
            },
            1.0,
            &mut system,
        );
        assert!(!off.ligatures);
        assert!(on.ligatures);
        // The cache key must change, or toggling ligatures keeps stale rows.
        assert_ne!(off.key, on.key);
    }

    fn contains(features: &FontFeatures, tag: FeatureTag, value: u32) -> bool {
        features
            .features
            .iter()
            .any(|feature| feature.tag == tag && feature.value == value)
    }

    #[test]
    fn missing_family_falls_back_to_monospace() {
        let mut system = FontSystem::new();
        let font = FontConfig {
            family: "Definitely Not Installed Font".to_owned(),
            ..font()
        };
        let style = TextStyle::new(&font, 1.0, &mut system);
        assert!(!style.family_available(&system));
        assert_eq!(style.effective_family(&system), "monospace");
    }

    /// The shipped default font, whose advances the renderer measures in production.
    fn mono_font() -> FontConfig {
        FontConfig::default()
    }

    /// The shipped default font with whole-pixel cells requested.
    fn integer_width_font() -> FontConfig {
        FontConfig {
            integer_cell_width: true,
            ..mono_font()
        }
    }

    #[test]
    fn integer_cell_width_lands_the_cell_on_a_whole_pixel() {
        let mut system = FontSystem::new();
        let style = TextStyle::new(&integer_width_font(), 1.0, &mut system);
        assert_eq!(
            style.cell.width.fract(),
            0.0,
            "the toggle exists so a block glyph's edge lands on a pixel boundary, not at {}",
            style.cell.width
        );
        assert!(style.cell.width >= 1.0);
        // The grid reads the baseline as the font size, so shaping and the grid must
        // still agree on it after the font is stretched.
        assert_eq!(style.cell.baseline, style.metrics.font_size);
    }

    #[test]
    fn integer_cell_width_keeps_the_shaped_advance_on_the_grid() {
        let mut system = FontSystem::new();
        let style = TextStyle::new(&integer_width_font(), 1.0, &mut system);
        let mut cache = RowCache::new();
        let scene = scene_with(11, "MMMMMMMMMM");
        cache.update(&mut system, &style, &scene);
        let buffer = &cache
            .rows
            .get(&11)
            .expect("the row was shaped into the cache")
            .buffer;
        let run = buffer.layout_runs().next().expect("one shaped run");
        // Ten ASCII cells: the grid advances `cell.width` per column, so the painted
        // run has to advance exactly that much or the grid and the glyphs drift apart.
        let advance = run.line_w / 10.0;
        assert!(
            (advance - style.cell.width).abs() <= 0.01,
            "shaped advance {advance} against cell width {}",
            style.cell.width
        );
    }

    #[test]
    fn without_the_option_the_cell_width_is_the_measured_one() {
        let mut system = FontSystem::new();
        let font = mono_font();
        assert!(!font.integer_cell_width, "the option is off by default");
        let style = TextStyle::new(&font, 1.0, &mut system);
        let measured = measure_cell(&mut system, &font, 1.0);
        assert_eq!(
            style.cell, measured,
            "off, the cell is exactly what measure_cell reports"
        );
        assert_eq!(style.metrics.font_size, font.size);
        assert_eq!(
            style.metrics.line_height,
            font.size * font.line_height,
            "off, the row height formula is unchanged"
        );
    }

    #[test]
    fn the_style_key_tracks_the_integer_cell_width_choice() {
        let mut system = FontSystem::new();
        let off = TextStyle::new(&mono_font(), 1.0, &mut system);
        let on = TextStyle::new(&integer_width_font(), 1.0, &mut system);
        assert_ne!(
            off.key, on.key,
            "toggling the option has to invalidate shaped rows"
        );
    }

    fn hinted_font() -> FontConfig {
        FontConfig {
            hinting: true,
            ..mono_font()
        }
    }

    #[test]
    fn hinting_is_off_by_default() {
        assert!(!FontConfig::default().hinting);
    }

    #[test]
    fn the_style_key_tracks_the_hinting_choice() {
        let mut system = FontSystem::new();
        let off = TextStyle::new(&mono_font(), 1.0, &mut system);
        let on = TextStyle::new(&hinted_font(), 1.0, &mut system);
        assert!(!off.hinting);
        assert!(on.hinting);
        assert_ne!(
            off.key, on.key,
            "toggling hinting has to invalidate shaped rows"
        );
    }

    /// Every shaped glyph carries the flag into the atlas key, so the rasteriser
    /// the renderer reaches through glyphon is the one the configuration asked for.
    fn glyph_cache_flags(font: &FontConfig, text: &str) -> Vec<CacheKeyFlags> {
        let mut system = FontSystem::new();
        let style = TextStyle::new(font, 1.0, &mut system);
        let mut cache = RowCache::new();
        cache.update(&mut system, &style, &scene_with(21, text));
        cache
            .rows
            .get(&21)
            .expect("the row was shaped into the cache")
            .buffer
            .layout_runs()
            .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.cache_key_flags))
            .collect()
    }

    #[test]
    fn hinting_off_marks_the_glyph_cache_key_unhinted() {
        let unhinted = glyph_cache_flags(&mono_font(), "Mn");
        assert!(!unhinted.is_empty(), "the row shaped at least one glyph");
        for flags in &unhinted {
            assert!(
                flags.contains(CacheKeyFlags::DISABLE_HINTING),
                "an unhinted style must not reach a hinted glyph: {flags:?}"
            );
        }

        let hinted = glyph_cache_flags(&hinted_font(), "Mn");
        assert!(!hinted.is_empty());
        for flags in &hinted {
            assert!(
                !flags.contains(CacheKeyFlags::DISABLE_HINTING),
                "a hinted style keeps cosmic-text's own hinting: {flags:?}"
            );
        }
    }

    #[test]
    fn unhinted_whole_pixel_cells_keep_the_shaped_advance_on_the_grid() {
        // The reporter's configuration: `integer_cell_width = true` shapes at a
        // fractional ppem, and unhinting must not move the advance off the cell.
        let mut system = FontSystem::new();
        let font = integer_width_font();
        assert!(
            !font.hinting,
            "the shipped default combined with whole-pixel cells"
        );
        let style = TextStyle::new(&font, 1.0, &mut system);
        let mut cache = RowCache::new();
        cache.update(&mut system, &style, &scene_with(22, "MMMMMMMMMM"));
        let run = cache
            .rows
            .get(&22)
            .expect("the row was shaped into the cache")
            .buffer
            .layout_runs()
            .next()
            .expect("one shaped run");
        let advance = run.line_w / 10.0;
        assert!(
            (advance - style.cell.width).abs() <= 0.01,
            "shaped advance {advance} against cell width {}",
            style.cell.width
        );
    }

    #[test]
    fn toggling_hinting_does_not_move_the_measured_cell() {
        let mut system = FontSystem::new();
        let unhinted = measure_cell(&mut system, &mono_font(), 1.0);
        let hinted = measure_cell(&mut system, &hinted_font(), 1.0);
        assert_eq!(
            unhinted, hinted,
            "the advance comes from unhinted glyph metrics, so the toggle only changes \
             rasterisation"
        );
    }

    #[test]
    fn a_cluster_is_measured_with_the_shaping_attributes() {
        let mut system = FontSystem::new();
        let style = TextStyle::new(&mono_font(), 1.0, &mut system);
        let mut widths = ClusterWidths::new();
        let ascii = {
            let mut measurer = ClusterMeasurer::new(&mut system, &style, &mut widths);
            measurer.advance("M")
        };
        assert!(
            (ascii - style.cell.width).abs() < 0.5,
            "a monospace glyph advances one cell: {ascii} against {}",
            style.cell.width
        );
        assert_eq!(widths.len(), 1);
    }

    #[test]
    fn a_fallback_script_measures_wider_than_its_cell() {
        // S8: the advance a whitelist could never predict. U+0915 DEVANAGARI LETTER
        // KA is not in any emoji or symbol block, yet it does not fit its column.
        let mut system = FontSystem::new();
        let style = TextStyle::new(&mono_font(), 1.0, &mut system);
        let mut widths = ClusterWidths::new();
        let mut measurer = ClusterMeasurer::new(&mut system, &style, &mut widths);
        let devanagari = measurer.advance("\u{0915}");
        assert!(
            devanagari - style.cell.width > 0.5,
            "devanagari drifts past the half-pixel tolerance: {devanagari} against {}",
            style.cell.width
        );
    }

    #[test]
    fn measuring_the_same_cluster_twice_shapes_it_once() {
        let mut system = FontSystem::new();
        let style = TextStyle::new(&mono_font(), 1.0, &mut system);
        let mut widths = ClusterWidths::new();
        let first = {
            let mut measurer = ClusterMeasurer::new(&mut system, &style, &mut widths);
            measurer.advance("a")
        };
        let second = {
            let mut measurer = ClusterMeasurer::new(&mut system, &style, &mut widths);
            measurer.advance("a")
        };
        assert_eq!(first, second, "a cached measurement is reused verbatim");
        assert_eq!(widths.len(), 1, "the second lookup came from the cache");
    }

    #[test]
    fn cluster_widths_are_keyed_by_style_and_can_be_bounded() {
        let mut system = FontSystem::new();
        let mut widths = ClusterWidths::new();
        for size in [13.0, 20.0] {
            let style = TextStyle::new(
                &FontConfig {
                    size,
                    ..mono_font()
                },
                1.0,
                &mut system,
            );
            let mut measurer = ClusterMeasurer::new(&mut system, &style, &mut widths);
            measurer.advance("M");
        }
        assert_eq!(widths.len(), 2, "each style keeps its own measurement");
        assert!(!widths.is_empty());
        widths.clear();
        assert!(widths.is_empty(), "the cache can be dropped wholesale");
    }

    /// Distinct one-character clusters, enough to overflow a small cache.
    fn clusters(count: usize) -> Vec<String> {
        (0..count)
            .map(|index| {
                let code = char::from(b'a' + (index % 26) as u8);
                let mut cluster = String::new();
                cluster.push(code);
                cluster.push_str(&format!("{index}"));
                cluster
            })
            .collect()
    }

    #[test]
    fn the_cluster_cache_evicts_the_oldest_instead_of_wiping_itself() {
        // B19: reaching the bound used to clear every style map, so a screen with more
        // distinct clusters than the bound reshaped all of them on the next frame.
        let capacity = 8;
        let mut system = FontSystem::new();
        let style = TextStyle::new(&mono_font(), 1.0, &mut system);
        let mut widths = ClusterWidths::with_capacity(capacity);
        let clusters = clusters(capacity + 4);
        for cluster in &clusters {
            let mut measurer = ClusterMeasurer::new(&mut system, &style, &mut widths);
            measurer.advance(cluster);
            assert!(
                widths.len() <= capacity,
                "the cache outgrew its bound with {} entries after {cluster:?}",
                widths.len()
            );
        }
        assert_eq!(widths.len(), capacity, "the cache is full, not emptied");

        let shaped = widths.measurements();
        for cluster in &clusters[4..] {
            let mut measurer = ClusterMeasurer::new(&mut system, &style, &mut widths);
            measurer.advance(cluster);
        }
        assert_eq!(
            widths.measurements(),
            shaped,
            "the {} most recently inserted clusters are still resident",
            clusters.len() - 4
        );

        // The four oldest measurements were the ones evicted, and evicting one made
        // room for exactly one.
        let mut measurer = ClusterMeasurer::new(&mut system, &style, &mut widths);
        measurer.advance(&clusters[0]);
        assert_eq!(
            widths.measurements(),
            shaped + 1,
            "the oldest cluster had to be shaped again"
        );
        assert_eq!(widths.len(), capacity, "still bounded after the eviction");
    }

    #[test]
    fn the_production_bound_covers_the_cells_of_a_4k_screen() {
        // A bound smaller than the screen's distinct clusters reshapes every one of
        // them on every frame however it evicts, so it has to hold a whole 4K screen
        // at the smallest layout the defaults allow.
        let columns = 3840 / 8;
        let rows = 2160 / 16;
        assert!(
            ClusterWidths::new().capacity() >= columns * rows,
            "{} entries against {columns}x{rows} cells",
            MAX_CACHED_CLUSTERS
        );
    }

    #[test]
    fn plain_ascii_is_only_skipped_when_it_really_fits_one_column() {
        // The row walk skips the measurement of an ASCII cell because this check says
        // the advance is already known, so the check has to come from measurement
        // rather than from an assumption about the family.
        let mut system = FontSystem::new();
        let monospace = TextStyle::new(&mono_font(), 1.0, &mut system);
        assert!(
            monospace.ascii_is_one_column,
            "the shipped default font fills one column per ASCII cell"
        );
        let spaced = TextStyle::new(
            &FontConfig {
                letter_spacing: 3.0,
                ..mono_font()
            },
            1.0,
            &mut system,
        );
        assert!(
            !spaced.ascii_is_one_column,
            "letter spacing widens every ASCII cell past its column, so it must be \
             measured as before"
        );
    }
}
