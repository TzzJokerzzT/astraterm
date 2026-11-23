//! Solid-colour quad pipeline.
//!
//! Everything that is not glyph text — backgrounds, selection, cursor, pane
//! borders, tab strip, overlays — is drawn as instanced rectangles by this single
//! pipeline, in one draw call per frame.

use wgpu::{
    BindGroupLayout, BlendState, Buffer, BufferDescriptor, BufferUsages, ColorTargetState,
    ColorWrites, Device, FragmentState, MultisampleState, PipelineLayoutDescriptor, PrimitiveState,
    PrimitiveTopology, Queue, RenderPass, RenderPipeline, RenderPipelineDescriptor,
    ShaderModuleDescriptor, ShaderSource, TextureFormat, VertexState,
};

/// A rectangle instance.
///
/// Layout matches the WGSL struct: `x, y, width, height` then `r, g, b, a`.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Instance {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Instance {
    /// Applies the straight-alpha colour onto the instance.
    pub fn with_color(mut self, color: [f32; 4]) -> Self {
        self.r = color[0];
        self.g = color[1];
        self.b = color[2];
        self.a = color[3];
        self
    }
}

const SHADER: &str = r#"
struct Globals {
    // Physical surface size in pixels.
    size: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;

struct Instance {
    @location(0) rect: vec4<f32>,   // x, y, width, height
    @location(1) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
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
    // Convert pixels to clip space.
    let ndc = vec2<f32>(
        pixel.x / globals.size.x * 2.0 - 1.0,
        1.0 - pixel.y / globals.size.y * 2.0,
    );
    var out: VertexOutput;
    out.position = vec4<f32>(ndc, 0.0, 1.0);
    out.color = instance.color;
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return input.color;
}
"#;

/// Instanced quad renderer.
pub struct QuadPipeline {
    pipeline: RenderPipeline,
    bind_group_layout: BindGroupLayout,
    bind_group: wgpu::BindGroup,
    instances: Buffer,
    capacity: usize,
    count: u32,
    globals: Buffer,
}

impl QuadPipeline {
    pub fn new(device: &Device, format: TextureFormat) -> Self {
        let shader = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("astraterm-quad-shader"),
            source: ShaderSource::Wgsl(SHADER.into()),
        });

        let globals = device.create_buffer(&BufferDescriptor {
            label: Some("astraterm-quad-globals"),
            size: 16,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("astraterm-quad-bind-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("astraterm-quad-bind-group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals.as_entire_binding(),
            }],
        });

        let layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("astraterm-quad-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("astraterm-quad-pipeline"),
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

        let capacity = 2048;
        let instances = device.create_buffer(&BufferDescriptor {
            label: Some("astraterm-quad-instances"),
            size: (capacity * std::mem::size_of::<Instance>()) as u64,
            usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            bind_group_layout,
            bind_group,
            instances,
            capacity,
            count: 0,
            globals,
        }
    }

    /// Uploads the frame's rectangles. Reallocates the instance buffer when needed.
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
                label: Some("astraterm-quad-instances"),
                size: (capacity * std::mem::size_of::<Instance>()) as u64,
                usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.capacity = capacity;
        }

        if !instances.is_empty() {
            queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(instances));
        }
        self.count = instances.len() as u32;
    }

    /// Draws the uploaded rectangles.
    pub fn draw(&self, pass: &mut RenderPass<'_>) {
        if self.count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        pass.draw(0..6, 0..self.count);
    }

    /// Layout of the quad bind group, exposed for pipeline composition.
    pub fn layout(&self) -> &BindGroupLayout {
        &self.bind_group_layout
    }
}
