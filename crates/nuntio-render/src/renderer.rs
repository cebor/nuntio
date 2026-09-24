use std::collections::HashMap;
use std::fmt::Debug;

use bytemuck::{Pod, Zeroable};
use nuntio_term::{CursorStyle, Rgb, Snapshot, SnapshotCell};
use wgpu::rwh::{HasDisplayHandle, HasWindowHandle};

use crate::atlas::{ATLAS_SIZE, Atlas, AtlasRegion};
use crate::font::{CellMetrics, FaceStyle, Fonts};
use crate::gpu::{FrameStatus, GpuContext, GpuError};

const KIND_SOLID: u32 = 0;
const KIND_MASK: u32 = 1;
const KIND_COLOR: u32 = 2;

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct Instance {
    pos: [f32; 2],
    size: [f32; 2],
    uv: [f32; 4],
    color: [f32; 4],
    kind: u32,
    _pad: [u32; 3],
}

impl Instance {
    fn solid(x: f32, y: f32, width: f32, height: f32, color: Rgb) -> Self {
        Self {
            pos: [x, y],
            size: [width, height],
            uv: [0.0; 4],
            color: rgba(color),
            kind: KIND_SOLID,
            _pad: [0; 3],
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    screen_size: [f32; 2],
    atlas_size: [f32; 2],
}

fn rgba(c: Rgb) -> [f32; 4] {
    [
        c.r as f32 / 255.0,
        c.g as f32 / 255.0,
        c.b as f32 / 255.0,
        1.0,
    ]
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum GlyphKey {
    Char(char, FaceStyle),
    /// A base character with combining marks.
    Cluster(Box<str>, FaceStyle),
}

/// A rasterized glyph placed in an atlas.
#[derive(Debug, Clone, Copy)]
struct Sprite {
    /// Offset from the cell's top-left corner.
    x: i32,
    y: i32,
    region: AtlasRegion,
    color: bool,
}

struct AtlasFull;

/// Where on the surface the terminal grid is drawn, in physical pixels.
#[derive(Debug, Clone, Copy, Default)]
pub struct Viewport {
    pub x: u32,
    pub y: u32,
}

pub struct Renderer {
    gpu: GpuContext,
    fonts: Fonts,
    mask_atlas: Atlas,
    color_atlas: Atlas,
    glyphs: HashMap<GlyphKey, Box<[Sprite]>>,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniforms: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    instances: Vec<Instance>,
}

impl Renderer {
    pub fn new<W>(
        window: W,
        width: u32,
        height: u32,
        scale_factor: f64,
        font_family: Option<String>,
        font_size: f32,
    ) -> Result<Self, GpuError>
    where
        W: HasWindowHandle + HasDisplayHandle + Debug + Clone + Send + Sync + 'static,
    {
        let gpu = GpuContext::new(window, width, height)?;
        let fonts = Fonts::new(font_family, font_size, scale_factor);
        let device = &gpu.device;

        let mask_atlas = Atlas::new(device, wgpu::TextureFormat::R8Unorm, "mask atlas");
        let color_atlas = Atlas::new(device, wgpu::TextureFormat::Rgba8Unorm, "color atlas");
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("quad"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture_entry(1),
                texture_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::include_wgsl!("quad.wgsl"));
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("quad"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("quad"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2,
                        1 => Float32x2,
                        2 => Float32x4,
                        3 => Float32x4,
                        4 => Uint32,
                    ],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: gpu.format(),
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let instance_capacity = 4096;
        let instance_buffer = create_instance_buffer(device, instance_capacity);
        let bind_group = create_bind_group(
            device,
            &bind_group_layout,
            &uniforms,
            &mask_atlas,
            &color_atlas,
            &sampler,
        );

        Ok(Self {
            gpu,
            fonts,
            mask_atlas,
            color_atlas,
            glyphs: HashMap::new(),
            pipeline,
            bind_group,
            uniforms,
            instance_buffer,
            instance_capacity,
            instances: Vec::new(),
        })
    }

    pub fn cell_metrics(&self) -> CellMetrics {
        self.fonts.metrics()
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.gpu.resize(width, height);
    }

    /// Re-rasterize for a new font size or scale factor (e.g. monitor change).
    pub fn set_font_size(&mut self, font_size: f32, scale_factor: f64) -> CellMetrics {
        let metrics = self.fonts.set_size(font_size, scale_factor);
        self.clear_glyphs();
        metrics
    }

    fn clear_glyphs(&mut self) {
        self.glyphs.clear();
        self.mask_atlas.clear();
        self.color_atlas.clear();
    }

    /// Draw a terminal snapshot and present it.
    pub fn render(&mut self, snapshot: &Snapshot, viewport: Viewport) -> FrameStatus {
        if self.build_instances(snapshot, viewport).is_err() {
            // The atlas filled up mid-frame: start over with an empty one.
            tracing::debug!("glyph atlas full, clearing");
            self.clear_glyphs();
            if self.build_instances(snapshot, viewport).is_err() {
                tracing::warn!("screen content does not fit into the glyph atlas");
            }
        }

        let frame = match self.gpu.acquire() {
            Ok(frame) => frame,
            Err(status) => return status,
        };
        let device = &self.gpu.device;
        let queue = &self.gpu.queue;

        let (width, height) = self.gpu.size();
        let uniforms = Uniforms {
            screen_size: [width as f32, height as f32],
            atlas_size: [ATLAS_SIZE as f32; 2],
        };
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&uniforms));

        if self.instances.len() > self.instance_capacity {
            self.instance_capacity = self.instances.len().next_power_of_two();
            self.instance_buffer = create_instance_buffer(device, self.instance_capacity);
        }
        queue.write_buffer(
            &self.instance_buffer,
            0,
            bytemuck::cast_slice(&self.instances),
        );

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("frame"),
        });
        {
            let bg = snapshot.background;
            let [r, g, b, a] = rgba(bg).map(f64::from);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("terminal"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            if !self.instances.is_empty() {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
                pass.draw(0..4, 0..self.instances.len() as u32);
            }
        }
        queue.submit([encoder.finish()]);
        queue.present(frame);
        FrameStatus::Presented
    }

    fn build_instances(
        &mut self,
        snapshot: &Snapshot,
        viewport: Viewport,
    ) -> Result<(), AtlasFull> {
        self.instances.clear();
        let m = self.fonts.metrics();
        let (cw, ch) = (m.width as f32, m.height as f32);
        let origin = |column: usize, line: usize| {
            (
                viewport.x as f32 + column as f32 * cw,
                viewport.y as f32 + line as f32 * ch,
            )
        };
        let cursor = snapshot.cursor;
        let block_cursor = cursor.filter(|c| c.style == CursorStyle::Block);

        // Backgrounds, merged into horizontal runs of equal color.
        for line in 0..snapshot.lines {
            let mut column = 0;
            while column < snapshot.columns {
                let bg = snapshot.cell(column, line).bg;
                let start = column;
                while column < snapshot.columns && snapshot.cell(column, line).bg == bg {
                    column += 1;
                }
                if bg != snapshot.background {
                    let (x, y) = origin(start, line);
                    let width = (column - start) as f32 * cw;
                    self.instances.push(Instance::solid(x, y, width, ch, bg));
                }
            }
        }

        // Cursor shapes sit on top of the background, below the text.
        if let Some(c) = cursor {
            let (x, y) = origin(c.column, c.line);
            let width = if c.wide { cw * 2.0 } else { cw };
            let stroke = m.stroke.max(1) as f32;
            let beam = (m.stroke as f32 * 2.0).max(2.0);
            let rects: &[(f32, f32, f32, f32)] = match c.style {
                CursorStyle::Block => &[(x, y, width, ch)],
                CursorStyle::Beam => &[(x, y, beam, ch)],
                CursorStyle::Underline => &[(x, y + ch - beam, width, beam)],
                CursorStyle::HollowBlock => &[
                    (x, y, width, stroke),
                    (x, y + ch - stroke, width, stroke),
                    (x, y, stroke, ch),
                    (x + width - stroke, y, stroke, ch),
                ],
            };
            for &(x, y, w, h) in rects {
                self.instances.push(Instance::solid(x, y, w, h, c.color));
            }
        }

        // Text and decorations.
        for line in 0..snapshot.lines {
            for column in 0..snapshot.columns {
                let cell = snapshot.cell(column, line);
                let under_block =
                    block_cursor.is_some_and(|c| c.line == line && c.column == column);
                // Text under a block cursor takes the cell background for contrast.
                let fg = if under_block { cell.bg } else { cell.fg };
                let (x, y) = origin(column, line);
                let width = if cell.style.wide { cw * 2.0 } else { cw };

                if cell.style.underline {
                    let y = y + m.underline_y as f32;
                    self.instances
                        .push(Instance::solid(x, y, width, m.stroke as f32, fg));
                }
                if cell.style.strikeout {
                    let y = y + m.strikeout_y as f32;
                    self.instances
                        .push(Instance::solid(x, y, width, m.stroke as f32, fg));
                }
                if cell.is_blank() {
                    continue;
                }
                for sprite in self.sprites(cell)?.iter() {
                    let r = sprite.region;
                    self.instances.push(Instance {
                        pos: [x + sprite.x as f32, y + sprite.y as f32],
                        size: [r.width as f32, r.height as f32],
                        uv: [r.x as f32, r.y as f32, r.width as f32, r.height as f32],
                        color: rgba(fg),
                        kind: if sprite.color { KIND_COLOR } else { KIND_MASK },
                        _pad: [0; 3],
                    });
                }
            }
        }
        Ok(())
    }

    /// Look up or rasterize the glyphs for a cell.
    fn sprites(&mut self, cell: &SnapshotCell) -> Result<Box<[Sprite]>, AtlasFull> {
        let style = FaceStyle {
            bold: cell.style.bold,
            italic: cell.style.italic,
        };
        let key = match &cell.zerowidth {
            None => GlyphKey::Char(cell.c, style),
            Some(marks) => GlyphKey::Cluster(
                std::iter::once(cell.c)
                    .chain(marks.iter().copied())
                    .collect(),
                style,
            ),
        };
        if let Some(sprites) = self.glyphs.get(&key) {
            return Ok(sprites.clone());
        }

        let text = match &key {
            GlyphKey::Char(c, _) => c.to_string(),
            GlyphKey::Cluster(s, _) => s.to_string(),
        };
        let baseline = self.fonts.metrics().baseline as i32;
        let queue = &self.gpu.queue;
        let mut sprites = Vec::new();
        for glyph in self.fonts.rasterize(&text, style) {
            let atlas = if glyph.color {
                &mut self.color_atlas
            } else {
                &mut self.mask_atlas
            };
            let region = atlas
                .insert(queue, glyph.width, glyph.height, &glyph.data)
                .ok_or(AtlasFull)?;
            sprites.push(Sprite {
                x: glyph.left,
                y: baseline - glyph.top,
                region,
                color: glyph.color,
            });
        }
        let sprites: Box<[Sprite]> = sprites.into();
        self.glyphs.insert(key, sprites.clone());
        Ok(sprites)
    }
}

fn create_instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("instances"),
        size: (capacity * size_of::<Instance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniforms: &wgpu::Buffer,
    mask_atlas: &Atlas,
    color_atlas: &Atlas,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("quad"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(mask_atlas.view()),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(color_atlas.view()),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}
