//! Textured image pipeline and the GPU texture cache.
//!
//! Images drawn by the terminal (kitty graphics, iTerm2, sixel) arrive as RGBA
//! bytes that a separate module has already decoded. This module owns the two
//! GPU-side responsibilities:
//!
//! * [`TexturePipeline`]: an instanced pipeline that draws one textured rectangle
//!   per image, above the solid quads and below the glyphs.
//! * [`TextureCache`]: uploads each image once per generation and keeps GPU memory
//!   bounded.
//!
//! The cache's eviction policy is a pure function, [`plan`], so the rules — what
//! enters, what leaves, what gets re-uploaded, what is dropped when the byte
//! budget is exceeded — are unit-tested without a window or a GPU.

use std::collections::HashMap;
use std::ops::Range;

use wgpu::{
    BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayout, BindGroupLayoutDescriptor,
    BindGroupLayoutEntry, BindingResource, BindingType, BlendState, Buffer, BufferDescriptor,
    BufferUsages, ColorTargetState, ColorWrites, Device, Extent3d, FragmentState, MultisampleState,
    Origin3d, PipelineLayoutDescriptor, PrimitiveState, PrimitiveTopology, Queue, RenderPass,
    RenderPipeline, RenderPipelineDescriptor, Sampler, SamplerBindingType, SamplerDescriptor,
    ShaderModuleDescriptor, ShaderSource, TexelCopyBufferLayout, TexelCopyTextureInfo, Texture,
    TextureAspect, TextureDescriptor, TextureDimension, TextureFormat, TextureSampleType,
    TextureUsages, TextureViewDescriptor, TextureViewDimension, VertexState,
};

use crate::paint::ImageItem;

/// Default GPU byte budget for terminal images.
pub const DEFAULT_BUDGET_BYTES: usize = 64 * 1024 * 1024;

/// One image the scene wants resident on the GPU this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Demand {
    pub id: u64,
    pub generation: u64,
    /// Bytes the GPU texture occupies (`width * height * 4`).
    pub bytes: usize,
}

/// Metadata of a texture that is already resident in the cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resident {
    pub id: u64,
    pub generation: u64,
    pub bytes: usize,
    /// Frame counter of the last frame that drew it.
    pub last_used: u64,
}

/// One texture the frame will draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Drawn {
    pub id: u64,
    pub generation: u64,
    /// Whether the pixels must be uploaded (new texture, or a new generation).
    pub upload: bool,
}

/// What the cache must do before drawing one frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Textures to draw, in first-use order.
    pub draw: Vec<Drawn>,
    /// Ids whose GPU texture must be destroyed.
    pub release: Vec<u64>,
    /// Demands that are not drawn this frame, and hold no GPU memory.
    pub dropped: Vec<u64>,
    /// Bytes resident after the plan is applied.
    pub bytes: usize,
}

/// Decides what the cache keeps, uploads, releases and drops for one frame.
///
/// Rules, in order:
///
/// 1. Demands are deduplicated by id: one texture per id, whether the scene draws
///    that image once or ten times. The largest generation wins, so the newest
///    pixels are the ones uploaded.
/// 2. A resident texture that no longer appears in the scene is released.
/// 3. A demand whose id is unchanged and whose generation is unchanged is reused;
///    a new id or a new generation is uploaded. A re-upload replaces the resident
///    entry in place, so it is not listed in [`Plan::release`].
/// 4. An image that is empty or larger than the whole budget can never fit and is
///    dropped without touching the GPU.
/// 5. If the resident bytes exceed the budget, the least recently used textures
///    are evicted — released and dropped — until the frame fits. Textures created
///    this frame count as used now, and ties break by ascending id.
pub fn plan(residents: &[Resident], demands: &[Demand], budget: usize, now: u64) -> Plan {
    let resident = |id: u64| residents.iter().find(|entry| entry.id == id).copied();

    let mut candidates: Vec<Candidate> = Vec::new();
    let mut dropped: Vec<u64> = Vec::new();
    for demand in demands {
        // An empty or oversized image never fits: no upload, no eviction.
        if demand.bytes == 0 || demand.bytes > budget {
            if !dropped.contains(&demand.id) {
                dropped.push(demand.id);
            }
            continue;
        }
        if dropped.contains(&demand.id) {
            continue;
        }
        match candidates
            .iter_mut()
            .find(|candidate| candidate.id == demand.id)
        {
            None => candidates.push(Candidate {
                id: demand.id,
                generation: demand.generation,
                bytes: demand.bytes,
                last_used: resident(demand.id).map(|entry| entry.last_used),
            }),
            Some(existing) => {
                // One texture per id: the newest pixels are the ones uploaded.
                if demand.generation > existing.generation {
                    existing.generation = demand.generation;
                    existing.bytes = demand.bytes;
                }
            }
        }
    }

    let mut release: Vec<u64> = residents
        .iter()
        .filter(|entry| !candidates.iter().any(|candidate| candidate.id == entry.id))
        .map(|entry| entry.id)
        .collect();

    let mut bytes: usize = candidates.iter().map(|candidate| candidate.bytes).sum();
    if bytes > budget {
        // Least recently used first; a texture created this frame was just used,
        // so it is evicted only after every older one.
        let mut order: Vec<usize> = (0..candidates.len()).collect();
        order.sort_by_key(|index| {
            let candidate = &candidates[*index];
            (candidate.last_used.unwrap_or(now), candidate.id)
        });
        for index in order {
            if bytes <= budget {
                break;
            }
            let candidate = &candidates[index];
            bytes -= candidate.bytes;
            dropped.push(candidate.id);
            if candidate.last_used.is_some() {
                release.push(candidate.id);
            }
        }
    }

    let draw: Vec<Drawn> = candidates
        .iter()
        .filter(|candidate| !dropped.contains(&candidate.id))
        .map(|candidate| Drawn {
            id: candidate.id,
            generation: candidate.generation,
            upload: resident(candidate.id).map(|entry| entry.generation)
                != Some(candidate.generation),
        })
        .collect();

    Plan {
        draw,
        release,
        dropped,
        bytes,
    }
}

/// A demanded image, after deduplication.
struct Candidate {
    id: u64,
    generation: u64,
    bytes: usize,
    /// Frame the id was last drawn, `None` for a texture that is not resident.
    last_used: Option<u64>,
}

/// Texture format used to upload image pixels for a given surface format.
///
/// The quad pipeline keeps its colours verbatim on a plain UNORM target and
/// linearises them for an sRGB or float target, so the *surface* decides whether
/// linear light is wanted. Images follow the same rule without touching the pixel
/// bytes: an sRGB texture view lets the GPU convert while sampling, which costs
/// nothing on the CPU and never copies the RGBA buffer again.
pub fn image_texture_format(target: TextureFormat) -> TextureFormat {
    if crate::target_expects_linear(target) {
        TextureFormat::Rgba8UnormSrgb
    } else {
        TextureFormat::Rgba8Unorm
    }
}

/// A textured rectangle instance.
///
/// Layout matches the WGSL struct: `x, y, width, height` then `u0, v0, u1, v1`.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Instance {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub u0: f32,
    pub v0: f32,
    pub u1: f32,
    pub v1: f32,
}

impl Instance {
    /// Builds an instance from a destination rectangle and a source UV range.
    pub fn new(x: f32, y: f32, width: f32, height: f32, uv: [f32; 4]) -> Self {
        Self {
            x,
            y,
            width,
            height,
            u0: uv[0],
            v0: uv[1],
            u1: uv[2],
            v1: uv[3],
        }
    }
}

const SHADER: &str = r#"
struct Globals {
    // Physical surface size in pixels.
    size: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;

@group(1) @binding(0) var image_texture: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;

struct Instance {
    @location(0) rect: vec4<f32>,  // x, y, width, height
    @location(1) uv: vec4<f32>,    // u0, v0, u1, v1
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32, instance: Instance) -> VertexOutput {
    // Two triangles per rectangle, expanded from the instance rectangle.
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 1.0),
    );
    let corner = corners[vertex_index];
    let pixel = instance.rect.xy + corner * instance.rect.zw;
    // Convert pixels to clip space, the same way the quad pipeline does.
    let ndc = vec2<f32>(
        pixel.x / globals.size.x * 2.0 - 1.0,
        1.0 - pixel.y / globals.size.y * 2.0,
    );
    var out: VertexOutput;
    out.position = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = instance.uv.xy + corner * (instance.uv.zw - instance.uv.xy);
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(image_texture, image_sampler, input.uv);
}
"#;

/// One draw call: a texture and the instance range that samples it.
pub struct Batch<'a> {
    pub bind_group: &'a BindGroup,
    pub range: Range<u32>,
}

/// Instanced pipeline that draws textured rectangles.
pub struct TexturePipeline {
    pipeline: RenderPipeline,
    globals: Buffer,
    globals_bind_group: BindGroup,
    texture_layout: BindGroupLayout,
    instances: Buffer,
    capacity: usize,
}

impl TexturePipeline {
    pub fn new(device: &Device, format: TextureFormat) -> Self {
        let shader = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("astraterm-image-shader"),
            source: ShaderSource::Wgsl(SHADER.into()),
        });

        let globals = device.create_buffer(&BufferDescriptor {
            label: Some("astraterm-image-globals"),
            size: 16,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let globals_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("astraterm-image-globals-layout"),
            entries: &[BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let globals_bind_group = device.create_bind_group(&BindGroupDescriptor {
            label: Some("astraterm-image-globals-bind-group"),
            layout: &globals_layout,
            entries: &[BindGroupEntry {
                binding: 0,
                resource: globals.as_entire_binding(),
            }],
        });

        // One bind group per texture, so each image is its own draw call.
        let texture_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("astraterm-image-texture-layout"),
            entries: &[
                BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: BindingType::Texture {
                        sample_type: TextureSampleType::Float { filterable: true },
                        view_dimension: TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: BindingType::Sampler(SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("astraterm-image-pipeline-layout"),
            bind_group_layouts: &[Some(&globals_layout), Some(&texture_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("astraterm-image-pipeline"),
            layout: Some(&layout),
            vertex: VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 16,
                            shader_location: 1,
                        },
                    ],
                })],
            },
            fragment: Some(FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(ColorTargetState {
                    format,
                    // Straight-alpha blending, exactly as the quad pipeline: the
                    // decoded pixels are not premultiplied.
                    blend: Some(BlendState::ALPHA_BLENDING),
                    write_mask: ColorWrites::ALL,
                })],
            }),
            primitive: PrimitiveState {
                topology: PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let capacity = 64;
        let instances = device.create_buffer(&BufferDescriptor {
            label: Some("astraterm-image-instances"),
            size: (capacity * std::mem::size_of::<Instance>()) as u64,
            usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            globals,
            globals_bind_group,
            texture_layout,
            instances,
            capacity,
        }
    }

    /// Layout of the per-texture bind group, used by [`TextureCache`].
    pub fn texture_layout(&self) -> &BindGroupLayout {
        &self.texture_layout
    }

    /// Uploads the frame's textured rectangles. Reallocates when needed.
    pub fn upload(
        &mut self,
        device: &Device,
        queue: &Queue,
        instances: &[Instance],
        size: (u32, u32),
    ) {
        queue.write_buffer(
            &self.globals,
            0,
            bytemuck::cast_slice(&[size.0 as f32, size.1 as f32, 0.0, 0.0]),
        );

        if instances.len() > self.capacity {
            let capacity = instances.len().next_power_of_two();
            self.instances = device.create_buffer(&BufferDescriptor {
                label: Some("astraterm-image-instances"),
                size: (capacity * std::mem::size_of::<Instance>()) as u64,
                usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.capacity = capacity;
        }

        if !instances.is_empty() {
            queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(instances));
        }
    }

    /// Draws the uploaded rectangles, one draw call per texture.
    pub fn draw(&self, pass: &mut RenderPass<'_>, batches: &[Batch<'_>]) {
        if batches.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.globals_bind_group, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        for batch in batches {
            if batch.range.is_empty() {
                continue;
            }
            pass.set_bind_group(1, batch.bind_group, &[]);
            pass.draw(0..6, batch.range.clone());
        }
    }
}

/// Instances to upload and the batch layout to draw them with.
#[derive(Debug, Default)]
pub struct Prepared {
    pub instances: Vec<Instance>,
    pub batches: Vec<BatchKey>,
}

/// A draw batch by texture id, so the renderer can resolve the bind group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchKey {
    pub id: u64,
    pub range: Range<u32>,
}

/// GPU textures for the images the scene currently draws.
pub struct TextureCache {
    entries: HashMap<u64, CachedTexture>,
    sampler: Sampler,
    format: TextureFormat,
    budget: usize,
    frame: u64,
}

struct CachedTexture {
    generation: u64,
    bytes: usize,
    last_used: u64,
    bind_group: BindGroup,
    /// Kept so the allocation outlives the view the bind group was built from.
    _texture: Texture,
}

impl TextureCache {
    pub fn new(device: &Device, format: TextureFormat) -> Self {
        Self::with_budget(device, format, DEFAULT_BUDGET_BYTES)
    }

    pub fn with_budget(device: &Device, format: TextureFormat, budget: usize) -> Self {
        // One sampler for every image: terminal images are scaled, so linear
        // filtering on both axes and clamped edges are what looks right.
        let sampler = device.create_sampler(&SamplerDescriptor {
            label: Some("astraterm-image-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            entries: HashMap::new(),
            sampler,
            format,
            budget,
            frame: 0,
        }
    }

    /// Bytes of GPU texture currently resident.
    pub fn resident_bytes(&self) -> usize {
        self.entries.values().map(|entry| entry.bytes).sum()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Bind group for one texture, `None` when it is not resident this frame.
    pub fn bind_group(&self, id: u64) -> Option<&BindGroup> {
        self.entries.get(&id).map(|entry| &entry.bind_group)
    }

    /// Applies the cache policy, uploading what changed and freeing what left.
    pub fn prepare(
        &mut self,
        device: &Device,
        queue: &Queue,
        layout: &BindGroupLayout,
        images: &[ImageItem],
    ) -> Prepared {
        let demands: Vec<Demand> = images
            .iter()
            .map(|image| Demand {
                id: image.id,
                generation: image.generation,
                bytes: image.pixels.byte_len(),
            })
            .collect();
        let residents: Vec<Resident> = self
            .entries
            .iter()
            .map(|(id, entry)| Resident {
                id: *id,
                generation: entry.generation,
                bytes: entry.bytes,
                last_used: entry.last_used,
            })
            .collect();

        self.frame = self.frame.wrapping_add(1);
        let decision = plan(&residents, &demands, self.budget, self.frame);

        for id in &decision.release {
            self.entries.remove(id);
        }
        for drawn in decision.draw.iter().filter(|drawn| drawn.upload) {
            let Some(image) = images
                .iter()
                .find(|image| image.id == drawn.id && image.generation == drawn.generation)
            else {
                continue;
            };
            if let Some(entry) = self.upload(device, queue, layout, image) {
                self.entries.insert(drawn.id, entry);
            }
        }

        let mut prepared = Prepared::default();
        for image in images {
            let Some(entry) = self.entries.get_mut(&image.id) else {
                continue;
            };
            if entry.generation != image.generation {
                continue;
            }
            entry.last_used = self.frame;
            let index = prepared.instances.len() as u32;
            prepared.instances.push(Instance::new(
                image.rect.x as f32,
                image.rect.y as f32,
                image.rect.width as f32,
                image.rect.height as f32,
                image.uv,
            ));
            match prepared.batches.last_mut() {
                // Adjacent items sharing a texture keep their scene order in one
                // batch; splitting on the id change preserves painter's order.
                Some(batch) if batch.id == image.id => batch.range.end = index + 1,
                _ => prepared.batches.push(BatchKey {
                    id: image.id,
                    range: index..index + 1,
                }),
            }
        }
        prepared
    }

    fn upload(
        &self,
        device: &Device,
        queue: &Queue,
        layout: &BindGroupLayout,
        image: &ImageItem,
    ) -> Option<CachedTexture> {
        let width = image.pixels.width;
        let height = image.pixels.height;
        if width == 0 || height == 0 {
            return None;
        }
        let texture = device.create_texture(&TextureDescriptor {
            label: Some("astraterm-image-texture"),
            size: Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: self.format,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: Origin3d::ZERO,
                aspect: TextureAspect::All,
            },
            &image.pixels.rgba,
            TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&BindGroupDescriptor {
            label: Some("astraterm-image-bind-group"),
            layout,
            entries: &[
                BindGroupEntry {
                    binding: 0,
                    resource: BindingResource::TextureView(&view),
                },
                BindGroupEntry {
                    binding: 1,
                    resource: BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        Some(CachedTexture {
            generation: image.generation,
            bytes: image.pixels.byte_len(),
            last_used: self.frame,
            bind_group,
            _texture: texture,
        })
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;

    fn demand(id: u64, generation: u64, bytes: usize) -> Demand {
        Demand {
            id,
            generation,
            bytes,
        }
    }

    fn resident(id: u64, generation: u64, bytes: usize, last_used: u64) -> Resident {
        Resident {
            id,
            generation,
            bytes,
            last_used,
        }
    }

    fn drawn(id: u64, generation: u64, upload: bool) -> Drawn {
        Drawn {
            id,
            generation,
            upload,
        }
    }

    #[test]
    fn a_new_image_is_uploaded() {
        let plan = plan(&[], &[demand(1, 1, 16)], 1024, 1);
        assert_eq!(plan.draw, vec![drawn(1, 1, true)]);
        assert!(plan.release.is_empty(), "nothing resident, nothing to free");
        assert!(plan.dropped.is_empty(), "the image fits the budget");
        assert_eq!(plan.bytes, 16);
    }

    #[test]
    fn a_resident_image_with_the_same_generation_is_reused() {
        let plan = plan(&[resident(1, 7, 16, 1)], &[demand(1, 7, 16)], 1024, 5);
        assert_eq!(plan.draw, vec![drawn(1, 7, false)]);
        assert!(
            plan.release.is_empty(),
            "reusing a texture must not free it"
        );
        assert_eq!(plan.bytes, 16);
    }

    #[test]
    fn a_new_generation_forces_a_reupload() {
        let plan = plan(&[resident(1, 7, 16, 1)], &[demand(1, 8, 16)], 1024, 5);
        assert_eq!(
            plan.draw,
            vec![drawn(1, 8, true)],
            "new pixels under the same id must be uploaded again"
        );
        assert!(
            plan.release.is_empty(),
            "the stale pixels are replaced, not kept alongside the new ones"
        );
        assert_eq!(plan.bytes, 16, "the new generation replaces the old size");
    }

    #[test]
    fn a_texture_that_leaves_the_scene_is_released() {
        let plan = plan(
            &[resident(2, 3, 16, 1), resident(4, 3, 16, 1)],
            &[demand(4, 3, 16)],
            1024,
            5,
        );
        assert_eq!(plan.release, vec![2], "the unused texture is freed");
        assert_eq!(plan.draw, vec![drawn(4, 3, false)]);
        assert_eq!(plan.bytes, 16);
    }

    #[test]
    fn exceeding_the_budget_evicts_the_least_recently_used() {
        let residents = [resident(1, 1, 16, 1), resident(2, 1, 16, 5)];
        let demands = [demand(1, 1, 16), demand(2, 1, 16)];
        let exact = plan(&residents, &demands, 32, 9);
        assert_eq!(exact.bytes, 32, "both textures fit exactly");

        let plan = plan(&residents, &demands, 31, 9);
        assert_eq!(
            plan.draw,
            vec![drawn(2, 1, false)],
            "the newer one survives"
        );
        assert_eq!(plan.release, vec![1], "the older texture is freed");
        assert_eq!(plan.dropped, vec![1], "and is not drawn");
        assert_eq!(plan.bytes, 16);
        assert!(plan.bytes <= 31);
    }

    #[test]
    fn the_budget_is_enforced_over_the_whole_resident_set() {
        let residents = [
            resident(1, 1, 16, 1),
            resident(2, 1, 16, 2),
            resident(3, 1, 16, 3),
        ];
        let demands = [demand(1, 1, 16), demand(2, 1, 16), demand(3, 1, 16)];
        let plan = plan(&residents, &demands, 16, 9);
        assert_eq!(plan.draw, vec![drawn(3, 1, false)]);
        assert_eq!(plan.release, vec![1, 2], "evicted oldest first");
        assert_eq!(plan.dropped, vec![1, 2]);
        assert_eq!(plan.bytes, 16);
    }

    #[test]
    fn an_image_larger_than_the_budget_is_never_uploaded() {
        let plan = plan(&[], &[demand(1, 1, 64)], 32, 1);
        assert!(plan.draw.is_empty(), "it cannot fit, so it is not drawn");
        assert_eq!(plan.dropped, vec![1]);
        assert!(plan.release.is_empty());
        assert_eq!(plan.bytes, 0);
    }

    #[test]
    fn empty_images_are_never_uploaded() {
        let plan = plan(&[], &[demand(1, 1, 0)], 1024, 1);
        assert!(plan.draw.is_empty());
        assert_eq!(plan.dropped, vec![1]);
        assert_eq!(plan.bytes, 0);
    }

    #[test]
    fn repeated_demands_share_one_texture() {
        let plan = plan(&[], &[demand(1, 1, 16), demand(1, 1, 16)], 1024, 1);
        assert_eq!(plan.draw, vec![drawn(1, 1, true)]);
        assert_eq!(plan.bytes, 16, "the texture is uploaded and counted once");
    }

    #[test]
    fn the_newest_generation_wins_when_one_id_is_demanded_twice() {
        let plan = plan(&[], &[demand(1, 3, 16), demand(1, 4, 16)], 1024, 1);
        assert_eq!(plan.draw, vec![drawn(1, 4, true)]);
        assert_eq!(plan.bytes, 16);
    }

    #[test]
    fn uploading_this_frame_never_evicts_older_textures_first() {
        // A new texture counts as used now, so a build-up of resident textures is
        // evicted before the frame's own upload is dropped.
        let residents = [resident(1, 1, 16, 1), resident(2, 1, 16, 2)];
        let demands = [demand(1, 1, 16), demand(2, 1, 16), demand(3, 1, 16)];
        let plan = plan(&residents, &demands, 32, 9);
        assert_eq!(
            plan.draw,
            vec![drawn(2, 1, false), drawn(3, 1, true)],
            "the freshly demanded image survives"
        );
        assert_eq!(plan.release, vec![1]);
        assert_eq!(plan.dropped, vec![1]);
        assert_eq!(plan.bytes, 32);
    }

    #[test]
    fn textures_follow_the_surface_format() {
        assert_eq!(
            image_texture_format(TextureFormat::Bgra8Unorm),
            TextureFormat::Rgba8Unorm,
            "a plain UNORM target takes the bytes verbatim, like quads do"
        );
        assert_eq!(
            image_texture_format(TextureFormat::Bgra8UnormSrgb),
            TextureFormat::Rgba8UnormSrgb,
            "a linear target lets the GPU convert while sampling"
        );
        assert_eq!(
            image_texture_format(TextureFormat::Rgba16Float),
            TextureFormat::Rgba8UnormSrgb
        );
    }
}
