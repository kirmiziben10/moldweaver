//! Offscreen wgpu renderer for the 3D viewport. Renders into a texture that
//! egui shows as an image, so it does not depend on egui's render pass setup.

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use eframe::egui_wgpu;
use eframe::wgpu;
use glam::{Mat4, Vec3, Vec4};

const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    eye: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Object {
    model: [[f32; 4]; 4],
    tint: [f32; 4],
    /// x: lighting on (1) / off (0)
    params: [f32; 4],
}

/// Geometry uploaded to the GPU.
pub struct GpuMesh {
    vertices: wgpu::Buffer,
    count: u32,
    lines: bool,
}

/// One thing to draw this frame.
pub struct DrawItem {
    pub mesh: Arc<GpuMesh>,
    pub model: Mat4,
    pub tint: Vec4,
    pub lit: bool,
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    egui_renderer: Arc<egui::mutex::RwLock<egui_wgpu::Renderer>>,
    opaque: wgpu::RenderPipeline,
    transparent: wgpu::RenderPipeline,
    lines: wgpu::RenderPipeline,
    globals_buf: wgpu::Buffer,
    globals_bg: wgpu::BindGroup,
    object_layout: wgpu::BindGroupLayout,
    object_pool: Vec<(wgpu::Buffer, wgpu::BindGroup)>,
    samples: u32,
    target: Option<Target>,
    pub texture_id: Option<egui::TextureId>,
    pub adapter_info: String,
}

struct Target {
    size: [u32; 2],
    msaa: Option<wgpu::TextureView>,
    resolve: wgpu::TextureView,
    depth: wgpu::TextureView,
}

const SHADER: &str = r#"
struct Globals { view_proj: mat4x4<f32>, eye: vec4<f32> };
struct Object { model: mat4x4<f32>, tint: vec4<f32>, params: vec4<f32> };
@group(0) @binding(0) var<uniform> g: Globals;
@group(1) @binding(0) var<uniform> o: Object;

struct VIn { @location(0) pos: vec3<f32>, @location(1) normal: vec3<f32>, @location(2) color: vec4<f32> };
struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) world: vec3<f32>,
};

@vertex
fn vs_main(v: VIn) -> VOut {
    var out: VOut;
    let w = o.model * vec4<f32>(v.pos, 1.0);
    out.world = w.xyz;
    out.clip = g.view_proj * w;
    out.normal = (o.model * vec4<f32>(v.normal, 0.0)).xyz;
    out.color = v.color * o.tint;
    return out;
}

@fragment
fn fs_main(in: VOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    if (o.params.x < 0.5) {
        return in.color;
    }
    var n = normalize(in.normal);
    if (!front) { n = -n; }
    let view = normalize(g.eye.xyz - in.world);
    let key = normalize(vec3<f32>(0.35, -0.45, 0.82));
    let diffuse = 0.55 * max(dot(n, view), 0.0) + 0.35 * max(dot(n, key), 0.0) + 0.22;
    let h = normalize(view + key);
    let spec = 0.12 * pow(max(dot(n, h), 0.0), 32.0);
    // Back faces (inside of cut geometry) are darkened.
    var shade = diffuse;
    if (!front) { shade = shade * 0.55; }
    return vec4<f32>(in.color.rgb * shade + vec3<f32>(spec), in.color.a);
}
"#;

impl Renderer {
    pub fn new(rs: &egui_wgpu::RenderState) -> Renderer {
        let device = rs.device.clone();
        let queue = rs.queue.clone();
        let info = rs.adapter.get_info();
        let adapter_info = format!("{} ({:?})", info.name, info.backend);
        let samples = if rs
            .adapter
            .get_texture_format_features(COLOR_FORMAT)
            .flags
            .sample_count_supported(4)
            && rs
                .adapter
                .get_texture_format_features(DEPTH_FORMAT)
                .flags
                .sample_count_supported(4)
        {
            4
        } else {
            1
        };
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("moldweaver shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let uniform_layout = |label| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            })
        };
        let globals_layout = uniform_layout("globals");
        let object_layout = uniform_layout("object");
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("moldweaver layout"),
            bind_group_layouts: &[Some(&globals_layout), Some(&object_layout)],
            immediate_size: 0,
        });
        let make = |topology, blend: bool, depth_write: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("moldweaver pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Vertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x4],
                    }],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: COLOR_FORMAT,
                        blend: Some(if blend {
                            wgpu::BlendState::ALPHA_BLENDING
                        } else {
                            wgpu::BlendState::REPLACE
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(depth_write),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: samples,
                    ..Default::default()
                },
                multiview_mask: None,
                cache: None,
            })
        };
        let opaque = make(wgpu::PrimitiveTopology::TriangleList, false, true);
        let transparent = make(wgpu::PrimitiveTopology::TriangleList, true, false);
        let lines = make(wgpu::PrimitiveTopology::LineList, true, true);
        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buf.as_entire_binding(),
            }],
        });
        Renderer {
            device,
            queue,
            egui_renderer: rs.renderer.clone(),
            opaque,
            transparent,
            lines,
            globals_buf,
            globals_bg,
            object_layout,
            object_pool: Vec::new(),
            samples,
            target: None,
            texture_id: None,
            adapter_info,
        }
    }

    pub fn upload(&self, vertices: &[Vertex], lines: bool) -> Arc<GpuMesh> {
        use wgpu::util::DeviceExt;
        let data: &[u8] = if vertices.is_empty() {
            &[0u8; std::mem::size_of::<Vertex>()]
        } else {
            bytemuck::cast_slice(vertices)
        };
        let buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mesh"),
            contents: data,
            usage: wgpu::BufferUsages::VERTEX,
        });
        Arc::new(GpuMesh {
            vertices: buf,
            count: vertices.len() as u32,
            lines,
        })
    }

    fn ensure_target(&mut self, size: [u32; 2]) {
        if self.target.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        let tex = |label, format, samples, usage| {
            self.device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: size[0],
                        height: size[1],
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: samples,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let resolve = tex(
            "viewport color",
            COLOR_FORMAT,
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let msaa = (self.samples > 1)
            .then(|| tex("viewport msaa", COLOR_FORMAT, self.samples, wgpu::TextureUsages::RENDER_ATTACHMENT));
        let depth = tex("viewport depth", DEPTH_FORMAT, self.samples, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let mut r = self.egui_renderer.write();
        match self.texture_id {
            Some(id) => r.update_egui_texture_from_wgpu_texture(&self.device, &resolve, wgpu::FilterMode::Linear, id),
            None => self.texture_id = Some(r.register_native_texture(&self.device, &resolve, wgpu::FilterMode::Linear)),
        }
        self.target = Some(Target {
            size,
            msaa,
            resolve,
            depth,
        });
    }

    /// Render the scene into the viewport texture.
    pub fn render(&mut self, size: [u32; 2], view_proj: Mat4, eye: Vec3, background: [f64; 3], items: &[DrawItem]) {
        let size = [size[0].clamp(1, 8192), size[1].clamp(1, 8192)];
        self.ensure_target(size);
        let globals = Globals {
            view_proj: view_proj.to_cols_array_2d(),
            eye: eye.extend(1.0).to_array(),
        };
        self.queue.write_buffer(&self.globals_buf, 0, bytemuck::bytes_of(&globals));
        while self.object_pool.len() < items.len() {
            let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("object"),
                size: std::mem::size_of::<Object>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("object"),
                layout: &self.object_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buf.as_entire_binding(),
                }],
            });
            self.object_pool.push((buf, bg));
        }
        for (item, (buf, _)) in items.iter().zip(&self.object_pool) {
            let o = Object {
                model: item.model.to_cols_array_2d(),
                tint: item.tint.to_array(),
                params: [if item.lit { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0],
            };
            self.queue.write_buffer(buf, 0, bytemuck::bytes_of(&o));
        }
        let target = self.target.as_ref().expect("target");
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let (view, resolve) = match &target.msaa {
                Some(m) => (m, Some(&target.resolve)),
                None => (&target.resolve, None),
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("viewport"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: resolve,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: background[0],
                            g: background[1],
                            b: background[2],
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &target.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &self.globals_bg, &[]);
            // Opaque first, then lines, then transparent (back to front).
            let mut order: Vec<usize> = (0..items.len()).collect();
            let key = |i: usize| {
                let it = &items[i];
                if it.mesh.lines {
                    1
                } else if it.tint.w < 0.999 {
                    2
                } else {
                    0
                }
            };
            order.sort_by_key(|&i| key(i));
            let depth_of = |i: usize| (items[i].model.w_axis.truncate() - eye).length();
            let first_t = order.iter().position(|&i| key(i) == 2).unwrap_or(order.len());
            order[first_t..].sort_by(|&a, &b| depth_of(b).total_cmp(&depth_of(a)));
            for i in order {
                let it = &items[i];
                if it.mesh.count == 0 {
                    continue;
                }
                let pipeline = match key(i) {
                    0 => &self.opaque,
                    1 => &self.lines,
                    _ => &self.transparent,
                };
                pass.set_pipeline(pipeline);
                pass.set_bind_group(1, &self.object_pool[i].1, &[]);
                pass.set_vertex_buffer(0, it.mesh.vertices.slice(..));
                pass.draw(0..it.mesh.count, 0..1);
            }
        }
        self.queue.submit([encoder.finish()]);
    }
}
