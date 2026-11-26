//! GPU rendering for AstraTerm.
//!
//! Three pipelines cover everything the application draws, in this order:
//!
//! * a single instanced quad pipeline for backgrounds, selection, cursor, pane
//!   chrome, tab strip and overlays;
//! * an instanced textured pipeline for terminal images, one draw call per image
//!   texture, with a texture cache that keeps GPU memory bounded;
//! * glyphon (wgpu + cosmic-text) for glyphs, with a shaped-row cache so unchanged
//!   rows are never re-shaped. Glyphs are drawn last, so text stays on top of the
//!   images behind it.
//!
//! One frame is composed of up to two layers: the mosaic, and an overlay layer
//! drawn afterwards into the same target without clearing it. Within a layer the
//! order is quads, images, glyphs; between layers the whole layer is ordered, so
//! the glyph batch of the layer below cannot bleed through the background of the
//! layer above.
//!
//! The renderer knows nothing about panes, tabs or sessions: it consumes a [`Scene`]
//! that the application composes from snapshots.

pub mod paint;
pub mod quad;
pub mod text;
pub mod texture;

use std::sync::Arc;

use astraterm_config::schema::FontConfig;
use astraterm_core::{CellMetrics, Rect, Size};
use cosmic_text::FontSystem;
use glyphon::{Cache, SwashCache, TextAtlas, TextRenderer, Viewport};
use wgpu::{
    CommandEncoderDescriptor, CompositeAlphaMode, Device, DeviceDescriptor, Instance,
    InstanceDescriptor, LoadOp, MultisampleState, Operations, PresentMode, Queue,
    RenderPassColorAttachment, RenderPassDescriptor, RequestAdapterOptions, StoreOp, Surface,
    SurfaceColorSpace, SurfaceConfiguration, TextureFormat, TextureUsages, TextureViewDescriptor,
};
use winit::dpi::PhysicalSize;
use winit::window::Window;

pub use paint::{ImageItem, ImagePixels, PaintOptions, Quad, Scene, Span, TextItem};
pub use text::TextStyle;

/// Chooses the surface format, preferring plain 8-bit UNORM.
///
/// `Bgra8Unorm` first because it is what compositors present most efficiently;
/// sRGB and float formats are accepted as a fallback and handled by linearising the
/// scene, never by letting the theme colours drift.
pub fn pick_format(formats: &[TextureFormat]) -> TextureFormat {
    const PREFERENCE: [TextureFormat; 6] = [
        TextureFormat::Bgra8Unorm,
        TextureFormat::Rgba8Unorm,
        TextureFormat::Bgra8UnormSrgb,
        TextureFormat::Rgba8UnormSrgb,
        TextureFormat::Rgba16Float,
        TextureFormat::Rgba32Float,
    ];
    for candidate in PREFERENCE {
        if formats.contains(&candidate) {
            return candidate;
        }
    }
    formats
        .first()
        .copied()
        .unwrap_or(TextureFormat::Bgra8Unorm)
}

/// Whether a target format expects linear-light values.
pub fn target_expects_linear(format: TextureFormat) -> bool {
    format.is_srgb()
        || matches!(
            format,
            TextureFormat::Rgba16Float | TextureFormat::Rgba32Float
        )
}

/// The scissor rectangle for `rect`, clamped to a `size` surface.
///
/// `None` when nothing of the rectangle falls inside the surface: the layer it
/// describes is then not drawn at all, because `set_scissor_rect` must stay inside
/// the attachment for wgpu to accept it.
pub fn scissor_rect(rect: Rect, size: (u32, u32)) -> Option<(u32, u32, u32, u32)> {
    let (surface_width, surface_height) = size;
    let x = rect.x.max(0) as u32;
    let y = rect.y.max(0) as u32;
    if x >= surface_width || y >= surface_height {
        return None;
    }
    let width = rect.width.min(surface_width - x);
    let height = rect.height.min(surface_height - y);
    if width == 0 || height == 0 {
        return None;
    }
    Some((x, y, width, height))
}

/// Errors raised while bringing up or using the renderer.
#[derive(Debug, thiserror::Error)]
pub enum RendererError {
    #[error("no suitable GPU adapter was found: {0}")]
    Adapter(String),
    #[error("could not create the rendering device: {0}")]
    Device(String),
    #[error("could not create a drawing surface for the window: {0}")]
    Surface(String),
    #[error("the surface must be reconfigured: {0}")]
    SurfaceLost(String),
    #[error("could not prepare glyphs: {0}")]
    Prepare(String),
    #[error("could not render glyphs: {0}")]
    Render(String),
}

/// Renderer configuration derived from the application settings.
#[derive(Clone, Debug)]
pub struct RendererOptions {
    pub font: FontConfig,
    /// Display scale factor of the window.
    pub scale: f32,
    /// Whether the window is translucent, so the surface must be blended.
    pub transparent: bool,
    /// Prefer low power (integrated GPU) over maximum performance.
    pub low_power: bool,
}

/// The wgpu renderer and its caches.
pub struct Renderer {
    instance: Instance,
    surface: Surface<'static>,
    device: Device,
    queue: Queue,
    config: SurfaceConfiguration,
    format: TextureFormat,
    quads: quad::QuadPipeline,
    images: texture::TexturePipeline,
    textures: texture::TextureCache,
    /// Same pipelines again, for the overlay layer.
    ///
    /// Every pipeline keeps one instance buffer and draws all of it, so a layer
    /// that must be drawn on its own needs its own buffers. The image cache is
    /// still shared: it is prepared once per frame for both layers.
    overlay_quads: quad::QuadPipeline,
    overlay_images: texture::TexturePipeline,
    font_system: FontSystem,
    swash_cache: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    /// Glyphs of the overlay layer. glyphon's `prepare` concatenates every area it
    /// is given into one vertex buffer and `render` draws all of it, so a layer
    /// that must not be drawn with the mosaic gets its own renderer. The atlas,
    /// font system, swash cache, row cache and viewport stay shared.
    overlay_text: TextRenderer,
    rows: text::RowCache,
    /// Measured cluster advances that decide which clusters are grid-anchored.
    widths: text::ClusterWidths,
    style: TextStyle,
    size: Size,
    window: Arc<Window>,
    transparent: bool,
}

impl Renderer {
    /// Creates the surface, adapter, device and pipelines for `window`.
    ///
    /// Runs a blocking adapter/device request: this happens once at startup, before
    /// the event loop starts pumping terminal output, so it cannot stall a session.
    pub fn new(
        window: Arc<Window>,
        display: winit::event_loop::OwnedDisplayHandle,
        options: RendererOptions,
    ) -> Result<Self, RendererError> {
        let size = window.inner_size();
        let instance = Instance::new(InstanceDescriptor::new_with_display_handle(Box::new(
            display,
        )));
        let surface = instance
            .create_surface(window.clone())
            .map_err(|error| RendererError::Surface(error.to_string()))?;

        let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions {
            power_preference: if options.low_power {
                wgpu::PowerPreference::LowPower
            } else {
                wgpu::PowerPreference::HighPerformance
            },
            compatible_surface: Some(&surface),
            ..RequestAdapterOptions::default()
        }))
        .map_err(|error| RendererError::Adapter(error.to_string()))?;

        let (device, queue) =
            pollster::block_on(adapter.request_device(&DeviceDescriptor::default()))
                .map_err(|error| RendererError::Device(error.to_string()))?;

        let capabilities = surface.get_capabilities(&adapter);
        // Prefer a plain 8-bit UNORM target: configured colours are then used
        // verbatim. Float or sRGB targets expect linear light, so the scene is
        // linearised per frame for those.
        let format = pick_format(&capabilities.formats);

        // Transparency needs a premultiplied alpha mode; otherwise stay opaque and
        // let the theme background hide the compositor entirely.
        let alpha_mode = if options.transparent
            && capabilities
                .alpha_modes
                .contains(&CompositeAlphaMode::PreMultiplied)
        {
            CompositeAlphaMode::PreMultiplied
        } else {
            CompositeAlphaMode::Opaque
        };
        let transparent = alpha_mode != CompositeAlphaMode::Opaque;

        let config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: PresentMode::Fifo,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
            color_space: SurfaceColorSpace::Auto,
        };
        surface.configure(&device, &config);

        let quads = quad::QuadPipeline::new(&device, format);
        let images = texture::TexturePipeline::new(&device, format);
        let textures = texture::TextureCache::new(&device, texture::image_texture_format(format));
        let mut font_system = FontSystem::new();
        let style = TextStyle::new(&options.font, options.scale, &mut font_system);
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        // Glyph colour space has to match the target. `Accurate` makes glyphon convert
        // every glyph colour to linear light, which is right only when the target is itself
        // an sRGB format that encodes it back on write. On a plain UNORM target the bytes we
        // hand over are already sRGB-encoded — the same space the quad pipeline writes —
        // so converting here would darken text and flatten any gradient drawn with block
        // glyphs. `Web` passes the colour through and keeps text and backgrounds in one space.
        let color_mode = if format.is_srgb() {
            glyphon::ColorMode::Accurate
        } else {
            glyphon::ColorMode::Web
        };
        let mut atlas = TextAtlas::with_color_mode(&device, &queue, &cache, format, color_mode);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);
        let overlay_text =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);

        tracing::info!(
            format = ?format,
            srgb = format.is_srgb(),
            color_mode = ?color_mode,
            alpha = ?alpha_mode,
            adapter = adapter.get_info().name,
            backend = ?adapter.get_info().backend,
            device_type = ?adapter.get_info().device_type,
            "renderer ready"
        );

        let initial_size = Size::new(config.width, config.height);
        let overlay_quads = quad::QuadPipeline::new(&device, format);
        let overlay_images = texture::TexturePipeline::new(&device, format);

        Ok(Self {
            instance,
            surface,
            device,
            queue,
            config,
            format,
            quads,
            images,
            textures,
            overlay_quads,
            overlay_images,
            font_system,
            swash_cache,
            viewport,
            atlas,
            text_renderer,
            overlay_text,
            rows: text::RowCache::new(),
            widths: text::ClusterWidths::new(),
            style,
            size: initial_size,
            window,
            transparent,
        })
    }

    /// Surface format, exposed for diagnostics.
    pub fn format(&self) -> TextureFormat {
        self.format
    }

    /// Whether the surface is actually translucent.
    pub fn is_transparent(&self) -> bool {
        self.transparent
    }

    pub fn size(&self) -> Size {
        Size::new(self.size.width, self.size.height)
    }

    pub fn cell_metrics(&self) -> CellMetrics {
        self.style.cell
    }

    pub fn scale(&self) -> f32 {
        self.style.scale
    }

    /// Measurement service for painting: shaped cluster advances under the current
    /// font.
    ///
    /// The anchoring decision belongs to painting (it splits rows into segments) but
    /// needs the shaped advance, so the renderer lends its font system, style and
    /// advance cache to whoever composes a frame. One service is enough for a whole
    /// frame: the cache keeps repeated clusters to a map lookup.
    pub fn measure(&mut self) -> text::ClusterMeasurer<'_> {
        text::ClusterMeasurer::new(&mut self.font_system, &self.style, &mut self.widths)
    }

    /// Reacts to a window resize by reconfiguring the surface.
    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.config.width = size.width;
        self.config.height = size.height;
        self.size = Size::new(size.width, size.height);
        self.surface.configure(&self.device, &self.config);
    }

    /// Applies a new font configuration (live font changes, no restart).
    pub fn set_font(&mut self, font: &FontConfig) {
        self.style = TextStyle::new(font, self.style.scale, &mut self.font_system);
        self.rows.clear();
        self.widths.clear();
        self.window.request_redraw();
    }

    /// Applies a new display scale factor.
    pub fn set_scale(&mut self, scale: f32, font: &FontConfig) {
        self.style = TextStyle::new(font, scale, &mut self.font_system);
        self.rows.clear();
        self.widths.clear();
        self.window.request_redraw();
    }

    /// Whether the effective font family resolved to a real installed family.
    pub fn font_available(&self) -> bool {
        self.style.family_available(&self.font_system)
    }

    /// Draws one frame.
    ///
    /// `scene` is the mosaic: it owns the whole target and clears it. `overlay`, when
    /// present, is a second layer recorded into the same target with `LoadOp::Load`
    /// straight after the mosaic's glyphs, scissored to [`Scene::clip`]. It carries
    /// its own quad instances and its own glyph batch, which is the point: the
    /// mosaic's text has already been painted, so the overlay's background covers
    /// it instead of the other way round.
    ///
    /// When the surface is an sRGB target both layers' colours are converted to
    /// linear light first, so theme colours appear exactly as configured.
    pub fn render(&mut self, scene: &Scene, overlay: Option<&Scene>) -> Result<(), RendererError> {
        let mut scene_scratch = None;
        let mut overlay_scratch = None;
        let scene = for_target(scene, &mut scene_scratch, self.format, self.transparent);
        let overlay = overlay.map(|overlay| {
            for_target(overlay, &mut overlay_scratch, self.format, self.transparent)
        });
        let size = (self.config.width, self.config.height);
        self.viewport.update(
            &self.queue,
            glyphon::Resolution {
                width: size.0,
                height: size.1,
            },
        );

        let instances = quad_instances(scene);
        self.quads
            .upload(&self.device, &self.queue, &instances, size);
        if let Some(overlay) = overlay {
            let instances = quad_instances(overlay);
            self.overlay_quads
                .upload(&self.device, &self.queue, &instances, size);
        }

        // Images are uploaded only when their generation changed, and the cache
        // hands back exactly the instances it kept, so an evicted texture is not
        // drawn with stale pixels.
        //
        // Both layers share one `prepare` call: it releases every resident texture
        // that is not demanded, so preparing the layers separately would evict the
        // mosaic's textures and re-upload them on the next frame.
        let mut combined = Vec::new();
        let layered_images = overlay.filter(|overlay| !overlay.images.is_empty());
        let demands: &[ImageItem] = match layered_images {
            Some(overlay) => {
                combined.reserve(scene.images.len() + overlay.images.len());
                combined.extend_from_slice(&scene.images);
                combined.extend_from_slice(&overlay.images);
                &combined
            }
            None => &scene.images,
        };
        let prepared = self.textures.prepare(
            &self.device,
            &self.queue,
            self.images.texture_layout(),
            demands,
        );

        // The mosaic fills the front of the instance buffer; the overlay's images
        // follow it. Each layer draws from its own buffer, so their ranges are
        // rebased. Without an overlay every instance belongs to the mosaic, exactly
        // as it did before layers existed.
        let total = prepared.instances.len() as u32;
        let split = match layered_images {
            Some(_) => split_point(&prepared.batches, &scene.images),
            None => total,
        };
        self.images.upload(
            &self.device,
            &self.queue,
            &prepared.instances[..split as usize],
            size,
        );
        let mosaic_batches = resolve_batches(&self.textures, &prepared.batches, 0, split);
        let overlay_batches = match layered_images {
            Some(_) => {
                self.overlay_images.upload(
                    &self.device,
                    &self.queue,
                    &prepared.instances[split as usize..],
                    size,
                );
                resolve_batches(&self.textures, &prepared.batches, split, total)
            }
            None => Vec::new(),
        };

        self.rows.update(&mut self.font_system, &self.style, scene);
        if let Some(overlay) = overlay {
            self.rows
                .update(&mut self.font_system, &self.style, overlay);
        }
        let areas = self.rows.areas(scene);
        self.text_renderer
            .prepare(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                areas,
                &mut self.swash_cache,
            )
            .map_err(|error| RendererError::Prepare(error.to_string()))?;
        if let Some(overlay) = overlay {
            let areas = self.rows.areas(overlay);
            self.overlay_text
                .prepare(
                    &self.device,
                    &self.queue,
                    &mut self.font_system,
                    &mut self.atlas,
                    &self.viewport,
                    areas,
                    &mut self.swash_cache,
                )
                .map_err(|error| RendererError::Prepare(error.to_string()))?;
        }

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                return Err(RendererError::SurfaceLost("surface is outdated".to_owned()));
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.surface = self
                    .instance
                    .create_surface(self.window.clone())
                    .map_err(|error| RendererError::Surface(error.to_string()))?;
                self.surface.configure(&self.device, &self.config);
                return Err(RendererError::SurfaceLost("surface was lost".to_owned()));
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(RendererError::SurfaceLost("validation error".to_owned()));
            }
        };

        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor {
                label: Some("astraterm-frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("astraterm-pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(wgpu::Color {
                            r: scene.clear[0] as f64,
                            g: scene.clear[1] as f64,
                            b: scene.clear[2] as f64,
                            a: scene.clear[3] as f64,
                        }),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.quads.draw(&mut pass);
            self.images.draw(&mut pass, &mosaic_batches);
            self.text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
                .map_err(|error| RendererError::Render(error.to_string()))?;
        }

        // Second pass: the overlay layer, onto the frame the first pass already
        // painted, clipped to the box it owns. A layer whose clip falls entirely
        // outside the target is not recorded at all.
        if let Some(overlay) = overlay {
            let clip = match overlay.clip {
                Some(rect) => scissor_rect(rect, size),
                None => Some((0, 0, size.0, size.1)),
            };
            if let Some((x, y, width, height)) = clip {
                let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                    label: Some("astraterm-overlay-pass"),
                    color_attachments: &[Some(RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: Operations {
                            load: LoadOp::Load,
                            store: StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_scissor_rect(x, y, width, height);
                self.overlay_quads.draw(&mut pass);
                self.overlay_images.draw(&mut pass, &overlay_batches);
                self.overlay_text
                    .render(&self.atlas, &self.viewport, &mut pass)
                    .map_err(|error| RendererError::Render(error.to_string()))?;
            }
        }

        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
        self.atlas.trim();
        Ok(())
    }

    /// Resolves prepared image batches of one layer into draw calls.
    ///
    /// `from..to` is the layer's slice of the combined instance buffer; the ranges
    /// are rebased because each layer draws from its own buffer.
    /// Drops cached shaped rows (used after a theme change that alters colours).
    ///
    /// Measured cluster advances survive: they depend on the font, not on colour, and
    /// are keyed by the shaping style anyway.
    pub fn invalidate_text_cache(&mut self) {
        self.rows.clear();
    }
}

/// Resolves prepared image batches of one layer into draw calls.
///
/// `from..to` is the layer's slice of the combined instance buffer; the ranges are
/// rebased because each layer draws from its own buffer. A free function rather
/// than a method so the borrow stays on the texture cache, leaving the rest of the
/// renderer usable mutably while the batches are alive.
fn resolve_batches<'a>(
    textures: &'a texture::TextureCache,
    prepared: &[texture::BatchKey],
    from: u32,
    to: u32,
) -> Vec<texture::Batch<'a>> {
    prepared
        .iter()
        .filter(|batch| batch.range.start >= from && batch.range.end <= to)
        .filter_map(|batch| {
            textures
                .bind_group(batch.id)
                .map(|bind_group| texture::Batch {
                    bind_group,
                    range: (batch.range.start - from)..(batch.range.end - from),
                })
        })
        .collect()
}

/// The scene as the GPU must see it.
///
/// Converted into `scratch` for an sRGB target (linear light) or a translucent
/// surface (premultiplied clear), borrowed unchanged otherwise, so the common
/// opaque UNORM path never clones the scene.
fn for_target<'a>(
    scene: &'a Scene,
    scratch: &'a mut Option<Scene>,
    format: TextureFormat,
    transparent: bool,
) -> &'a Scene {
    if !target_expects_linear(format) && !transparent {
        return scene;
    }
    let mut converted = scene.clone();
    if target_expects_linear(format) {
        converted.linearize();
    }
    if transparent {
        converted.premultiply_clear();
    }
    scratch.insert(converted)
}

/// One instance per scene quad, in push order.
fn quad_instances(scene: &Scene) -> Vec<quad::Instance> {
    scene
        .quads
        .iter()
        .map(|quad| {
            quad::Instance {
                x: quad.rect.x as f32,
                y: quad.rect.y as f32,
                width: quad.rect.width as f32,
                height: quad.rect.height as f32,
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            }
            .with_color(quad.color)
        })
        .collect()
}

/// How many prepared image instances the mosaic layer owns.
///
/// `TextureCache::prepare` walks the image list in order, so the mosaic's images
/// fill the front of the instance buffer. Batch ranges are contiguous and ordered,
/// image identifiers are namespaced per session (`SessionImages` shifts the session
/// id into the high half) and the floating terminal always owns its own session, so
/// no batch straddles the two layers.
fn split_point(prepared: &[texture::BatchKey], mosaic: &[ImageItem]) -> u32 {
    prepared
        .iter()
        .filter(|batch| mosaic.iter().any(|image| image.id == batch.id))
        .map(|batch| batch.range.end)
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod format_tests {
    use super::*;
    use std::ops::Range;

    use texture::BatchKey;

    fn batch(id: u64, range: Range<u32>) -> BatchKey {
        BatchKey { id, range }
    }

    fn image(id: u64) -> ImageItem {
        ImageItem::new(
            id,
            Rect::new(0, 0, 4, 4),
            Arc::new(ImagePixels::new(1, 1, vec![0; 4]).expect("one pixel")),
            1,
        )
    }

    #[test]
    fn a_rect_inside_the_surface_keeps_its_bounds() {
        assert_eq!(
            scissor_rect(Rect::new(10, 20, 100, 50), (200, 100)),
            Some((10, 20, 100, 50))
        );
    }

    #[test]
    fn a_rect_hanging_over_the_surface_is_clamped_to_it() {
        assert_eq!(
            scissor_rect(Rect::new(-5, 90, 100, 50), (60, 100)),
            Some((0, 90, 60, 10))
        );
    }

    #[test]
    fn a_rect_outside_the_surface_never_becomes_a_scissor() {
        assert_eq!(scissor_rect(Rect::new(200, 0, 10, 10), (200, 100)), None);
        assert_eq!(scissor_rect(Rect::new(0, 200, 10, 10), (200, 100)), None);
        assert_eq!(scissor_rect(Rect::new(0, 0, 0, 10), (200, 100)), None);
        assert_eq!(scissor_rect(Rect::new(0, 0, 10, 0), (200, 100)), None);
    }

    #[test]
    fn the_split_point_is_the_end_of_the_mosaic_images() {
        let mosaic = [image(7), image(7)];
        let prepared = [batch(7, 0..2), batch(9, 2..3)];
        assert_eq!(split_point(&prepared, &mosaic), 2);
    }

    #[test]
    fn a_frame_without_mosaic_images_gives_every_instance_to_the_overlay() {
        let prepared = [batch(9, 0..3)];
        assert_eq!(split_point(&prepared, &[]), 0);
    }

    #[test]
    fn prefers_plain_unorm_formats() {
        let formats = [
            TextureFormat::Rgba16Float,
            TextureFormat::Bgra8UnormSrgb,
            TextureFormat::Bgra8Unorm,
        ];
        assert_eq!(pick_format(&formats), TextureFormat::Bgra8Unorm);
    }

    #[test]
    fn falls_back_to_srgb_when_that_is_all_there_is() {
        let formats = [TextureFormat::Bgra8UnormSrgb];
        assert_eq!(pick_format(&formats), TextureFormat::Bgra8UnormSrgb);
        assert!(target_expects_linear(TextureFormat::Bgra8UnormSrgb));
    }

    #[test]
    fn float_targets_expect_linear_light() {
        assert!(target_expects_linear(TextureFormat::Rgba16Float));
        assert!(!target_expects_linear(TextureFormat::Bgra8Unorm));
        assert!(!target_expects_linear(TextureFormat::Rgba8Unorm));
    }

    #[test]
    fn selection_is_stable_for_a_given_capability_list() {
        let formats = [TextureFormat::Rgba16Float, TextureFormat::Rgba8UnormSrgb];
        assert_eq!(pick_format(&formats), TextureFormat::Rgba8UnormSrgb);
        assert_eq!(pick_format(&formats), pick_format(&formats));
    }
}
