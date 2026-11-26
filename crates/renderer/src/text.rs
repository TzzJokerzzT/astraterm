//! Text shaping, font metrics and the per-item glyph buffer cache.
//!
//! Rows are shaped once and re-shaped only when their content or style changes,
//! which is what keeps scrolling and idle redraws cheap: glyphon re-uploads glyph
//! quads from its atlas each frame, but cosmic-text layout is skipped.
//!
//! The `FontSystem` lives next to the cache (not inside it) so the renderer can hold
//! shaped buffers and the mutable font system at the same time.

use std::collections::HashMap;

use astraterm_config::schema::FontConfig;
use astraterm_core::CellMetrics;
use cosmic_text::{
    Attrs, Buffer, Family, FeatureTag, FontFeatures, FontSystem, Metrics, Shaping, Style, Weight,
};
use glyphon::{Color as GlyphonColor, TextArea, TextBounds};

use crate::paint::{Scene, TextItem};

/// Resolved shaping style for the terminal font.
#[derive(Clone, Debug)]
pub struct TextStyle {
    pub family: String,
    pub weight: Weight,
    pub letter_spacing: f32,
    /// Ligatures requested by configuration.
    pub ligatures: bool,
    pub metrics: Metrics,
    pub cell: CellMetrics,
    pub scale: f32,
    /// Hash of the inputs that affect shaping; changing it invalidates buffers.
    pub key: u64,
}

impl TextStyle {
    /// Builds a style from configuration and measures its cell metrics.
    pub fn new(font: &FontConfig, scale: f32, font_system: &mut FontSystem) -> Self {
        let scale = scale.max(0.1);
        let metrics = Metrics::new(font.size * scale, (font.size * font.line_height) * scale);
        let cell = measure_cell(font_system, font, scale);
        Self {
            family: font.family.clone(),
            weight: Weight(font.weight),
            letter_spacing: font.letter_spacing,
            ligatures: font.ligatures,
            metrics,
            cell,
            scale,
            key: style_key(font, scale),
        }
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

/// Upper bound on cached cluster advances; beyond this the cache is dropped
/// wholesale, the way [`RowCache`] bounds itself.
const MAX_CACHED_CLUSTERS: usize = 4096;

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
#[derive(Default)]
pub struct ClusterWidths {
    /// One map per shaping style, so a font or scale change leaves stale entries
    /// behind without ever reading them back.
    widths: HashMap<u64, HashMap<String, f32>>,
    /// Entries across every style, for bounding.
    len: usize,
}

impl ClusterWidths {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn clear(&mut self) {
        self.widths.clear();
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
        if let Some(width) = self
            .widths
            .get(&style.key)
            .and_then(|widths| widths.get(cluster))
        {
            return *width;
        }
        let width = measure_cluster(font_system, style, cluster);
        if self.len >= MAX_CACHED_CLUSTERS {
            self.clear();
        }
        self.widths
            .entry(style.key)
            .or_default()
            .insert(cluster.to_owned(), width);
        self.len += 1;
        width
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
        .weight(Weight(font.weight));
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
}
