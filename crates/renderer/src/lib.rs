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
//! The renderer knows nothing about panes, tabs or sessions: it consumes a [`Scene`]
//! that the application composes from snapshots.

pub mod paint;
pub mod quad;
pub mod text;
pub mod texture;

use std::sync::Arc;

use astraterm_config::schema::FontConfig;
use astraterm_core::{CellMetrics, Size};
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
    font_system: FontSystem,
    swash_cache: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    rows: text::RowCache,
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
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);

        tracing::info!(
            format = ?format,
            srgb = format.is_srgb(),
            alpha = ?alpha_mode,
            adapter = adapter.get_info().name,
            backend = ?adapter.get_info().backend,
            device_type = ?adapter.get_info().device_type,
            "renderer ready"
        );

        let initial_size = Size::new(config.width, config.height);

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
            font_system,
            swash_cache,
            viewport,
            atlas,
            text_renderer,
            rows: text::RowCache::new(),
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
        self.window.request_redraw();
    }

    /// Applies a new display scale factor.
    pub fn set_scale(&mut self, scale: f32, font: &FontConfig) {
        self.style = TextStyle::new(font, scale, &mut self.font_system);
        self.rows.clear();
        self.window.request_redraw();
    }

    /// Whether the effective font family resolved to a real installed family.
    pub fn font_available(&self) -> bool {
        self.style.family_available(&self.font_system)
    }

    /// Draws one frame.
    ///
    /// When the surface is an sRGB target the scene colours are converted to linear
    /// light first, so theme colours appear exactly as configured.
    pub fn render(&mut self, scene: &Scene) -> Result<(), RendererError> {
        let mut converted;
        let scene = if target_expects_linear(self.format) || self.transparent {
            converted = scene.clone();
            if target_expects_linear(self.format) {
                converted.linearize();
            }
            if self.transparent {
                converted.premultiply_clear();
            }
            &converted
        } else {
            scene
        };
        let size = (self.config.width, self.config.height);
        self.viewport.update(
            &self.queue,
            glyphon::Resolution {
                width: size.0,
                height: size.1,
            },
        );

        let instances: Vec<quad::Instance> = scene
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
            .collect();
        self.quads
            .upload(&self.device, &self.queue, &instances, size);

        // Images are uploaded only when their generation changed, and the cache
        // hands back exactly the instances it kept, so an evicted texture is not
        // drawn with stale pixels.
        let prepared = self.textures.prepare(
            &self.device,
            &self.queue,
            self.images.texture_layout(),
            &scene.images,
        );
        self.images
            .upload(&self.device, &self.queue, &prepared.instances, size);

        self.rows.update(&mut self.font_system, &self.style, scene);
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
            let batches: Vec<texture::Batch<'_>> = prepared
                .batches
                .iter()
                .filter_map(|batch| {
                    self.textures
                        .bind_group(batch.id)
                        .map(|bind_group| texture::Batch {
                            bind_group,
                            range: batch.range.clone(),
                        })
                })
                .collect();
            self.images.draw(&mut pass, &batches);
            self.text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
                .map_err(|error| RendererError::Render(error.to_string()))?;
        }

        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
        self.atlas.trim();
        Ok(())
    }

    /// Drops cached shaped rows (used after a theme change that alters colours).
    pub fn invalidate_text_cache(&mut self) {
        self.rows.clear();
    }
}

#[cfg(test)]
mod format_tests {
    use super::*;

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
