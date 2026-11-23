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
use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, Weight};
use glyphon::{Color as GlyphonColor, TextArea, TextBounds};

use crate::paint::{Scene, TextItem};

/// Resolved shaping style for the terminal font.
#[derive(Clone, Debug)]
pub struct TextStyle {
    pub family: String,
    pub weight: Weight,
    pub letter_spacing: f32,
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

fn shape(font_system: &mut FontSystem, style: &TextStyle, item: &TextItem) -> Buffer {
    let mut buffer = Buffer::new(font_system, style.metrics);
    // The terminal decides where lines break, so the shaper must not wrap.
    buffer.set_size(Some(1.0e6), Some(style.metrics.line_height));

    let family = style.family.clone();
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
                ));
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
        .weight(style.weight);
    buffer.set_rich_text(span_refs, &default_attrs, Shaping::Advanced, None);
    buffer.shape_until_scroll(font_system, false);
    buffer
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
}
