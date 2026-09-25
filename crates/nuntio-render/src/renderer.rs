use std::collections::HashMap;
use std::fmt::Debug;
use std::rc::Rc;

use bytemuck::{Pod, Zeroable};
use nuntio_term::{CursorStyle, Rgb, Snapshot, SnapshotCell};
use unicode_width::UnicodeWidthChar;
use wgpu::rwh::{HasDisplayHandle, HasWindowHandle};

use crate::atlas::{Atlas, AtlasRegion, MIN_ATLAS_SIZE};
use crate::box_drawing;
use crate::font::{CellMetrics, FaceStyle, Fonts};
use crate::frame::{Frame, UiText};
use crate::gpu::{FrameStatus, GpuContext, GpuError};

const KIND_SOLID: u32 = 0;
const KIND_MASK: u32 = 1;
const KIND_COLOR: u32 = 2;
const KIND_ROUNDED: u32 = 3;
/// Upper bound for the mask atlas: 16 MiB at one byte per texel.
const MAX_MASK_ATLAS_SIZE: u32 = 4096;

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
    _pad: [f32; 2],
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

pub struct Renderer {
    gpu: GpuContext,
    fonts: Fonts,
    mask_atlas: Atlas,
    color_atlas: Atlas,
    glyphs: HashMap<GlyphKey, Rc<[Sprite]>>,
    pipeline: wgpu::RenderPipeline,
    /// Cuts the window's rounded corners out of the finished frame.
    cutout_pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniforms: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    instances: Vec<Instance>,
    /// Instances from here on are corner cutouts, drawn with their own
    /// pipeline.
    cutout_start: usize,
    /// Problem with the configured font, reported once.
    font_warning: Option<String>,
    /// The last frame didn't fit into the glyph atlas (already warned).
    atlas_overflow: bool,
}

impl Renderer {
    pub fn new<W>(
        window: W,
        width: u32,
        height: u32,
        scale_factor: f64,
        font_family: Option<String>,
        font_size: f32,
        transparent: bool,
    ) -> Result<Self, GpuError>
    where
        W: HasWindowHandle + HasDisplayHandle + Debug + Clone + Send + Sync + 'static,
    {
        let gpu = GpuContext::new(window, width, height, transparent)?;
        let (fonts, font_warning) = Fonts::new(font_family, font_size, scale_factor);
        let device = &gpu.device;

        // Large fonts on HiDPI screens need room for many glyph masks (one
        // byte per texel). Color glyphs (emoji) are rarer and cost four.
        let mask_size = device
            .limits()
            .max_texture_dimension_2d
            .clamp(MIN_ATLAS_SIZE, MAX_MASK_ATLAS_SIZE);
        let mask_atlas = Atlas::new(
            device,
            wgpu::TextureFormat::R8Unorm,
            "mask atlas",
            mask_size,
        );
        let color_atlas = Atlas::new(
            device,
            wgpu::TextureFormat::Rgba8Unorm,
            "color atlas",
            MIN_ATLAS_SIZE,
        );
        // Glyphs are drawn 1:1; nearest sampling keeps them sharp even when
        // a quad lands on a fractional position.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
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
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
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
        let create_pipeline = |label, entry_point, blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
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
                    entry_point: Some(entry_point),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: gpu.format(),
                        blend: Some(blend),
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
            })
        };
        let pipeline = create_pipeline("quad", "fs_main", wgpu::BlendState::ALPHA_BLENDING);
        // Scales what is already drawn by the fragment's alpha.
        let scale_by_alpha = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Zero,
            dst_factor: wgpu::BlendFactor::SrcAlpha,
            operation: wgpu::BlendOperation::Add,
        };
        let cutout_pipeline = create_pipeline(
            "corner cutout",
            "fs_cutout",
            wgpu::BlendState {
                color: scale_by_alpha,
                alpha: scale_by_alpha,
            },
        );

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
            cutout_pipeline,
            bind_group,
            uniforms,
            instance_buffer,
            instance_capacity,
            instances: Vec::new(),
            cutout_start: 0,
            font_warning,
            atlas_overflow: false,
        })
    }

    /// Warning about the font given to `new`, if any.
    pub fn take_font_warning(&mut self) -> Option<String> {
        self.font_warning.take()
    }

    /// Switch font family (`None` = system monospace). Returns a warning if
    /// the family isn't installed.
    pub fn set_font_family(&mut self, family: Option<String>) -> Option<String> {
        let warning = self.fonts.set_family(family);
        self.clear_glyphs();
        warning
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

    /// Draw a frame and present it.
    pub fn render(&mut self, frame: &Frame) -> FrameStatus {
        if self.build_instances(frame).is_err() {
            // The atlas filled up mid-frame: start over with an empty one.
            tracing::debug!("glyph atlas full, clearing");
            self.clear_glyphs();
            let fits = self.build_instances(frame).is_ok();
            // Warn once, not on every frame while it stays too full.
            if !fits && !self.atlas_overflow {
                tracing::warn!("screen content does not fit into the glyph atlas");
            }
            self.atlas_overflow = !fits;
        }
        let background = frame.background;

        let surface_texture = match self.gpu.acquire() {
            Ok(texture) => texture,
            Err(status) => return status,
        };
        let device = &self.gpu.device;
        let queue = &self.gpu.queue;

        let (width, height) = self.gpu.size();
        let uniforms = Uniforms {
            screen_size: [width as f32, height as f32],
            _pad: [0.0; 2],
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

        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("frame"),
        });
        {
            let [r, g, b, _] = rgba(background).map(f64::from);
            let a = if self.gpu.transparent() {
                f64::from(frame.background_opacity.clamp(0.0, 1.0))
            } else {
                1.0
            };
            // Blending keeps the target premultiplied, so start that way.
            let (r, g, b) = if self.gpu.premultiplied() {
                (r * a, g * a, b * a)
            } else {
                (r, g, b)
            };
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
                let (cutout, total) = (self.cutout_start as u32, self.instances.len() as u32);
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
                pass.draw(0..4, 0..cutout);
                if total > cutout {
                    pass.set_pipeline(&self.cutout_pipeline);
                    pass.draw(0..4, cutout..total);
                }
            }
        }
        queue.submit([encoder.finish()]);
        queue.present(surface_texture);
        FrameStatus::Presented
    }

    fn build_instances(&mut self, frame: &Frame) -> Result<(), AtlasFull> {
        self.instances.clear();
        for pane in frame.panes {
            self.push_pane(pane.snapshot, pane.x, pane.y)?;
            if pane.dim > 0.0 {
                let [x, y, width, height] = pane.area;
                let mut veil = Instance::solid(x, y, width, height, pane.snapshot.background);
                veil.color[3] = pane.dim.min(1.0);
                self.instances.push(veil);
            }
        }
        for r in frame.rects {
            let mut instance = Instance::solid(r.x, r.y, r.width, r.height, r.color);
            if r.radius > 0.0 {
                instance.kind = KIND_ROUNDED;
                instance.uv[0] = r.radius;
            }
            self.instances.push(instance);
        }
        for text in frame.texts {
            self.push_text(text)?;
        }
        self.cutout_start = self.instances.len();
        let radius = frame.corner_radius.round();
        if radius > 0.0 && self.gpu.transparent() {
            let (width, height) = self.gpu.size();
            let (right, bottom) = (width as f32 - radius, height as f32 - radius);
            for (x, y) in [(0.0, 0.0), (right, 0.0), (0.0, bottom), (right, bottom)] {
                let mut corner = Instance::solid(x, y, radius, radius, Rgb { r: 0, g: 0, b: 0 });
                corner.uv[0] = radius;
                self.instances.push(corner);
            }
        }
        Ok(())
    }

    fn push_text(&mut self, text: &UiText) -> Result<(), AtlasFull> {
        let cw = self.fonts.metrics().width as f32;
        let style = FaceStyle {
            bold: text.bold,
            italic: false,
        };
        let mut x = text.x;
        for c in text.text.chars() {
            let width = c.width().unwrap_or(0);
            if width == 0 {
                continue;
            }
            if c != ' ' {
                let sprites = self.glyph_sprites(GlyphKey::Char(c, style))?;
                self.push_sprites(&sprites, x, text.y, text.color);
            }
            x += width as f32 * cw;
        }
        Ok(())
    }

    fn push_sprites(&mut self, sprites: &[Sprite], x: f32, y: f32, color: Rgb) {
        for sprite in sprites {
            let r = sprite.region;
            let atlas = if sprite.color {
                &self.color_atlas
            } else {
                &self.mask_atlas
            };
            let texel = 1.0 / atlas.size() as f32;
            self.instances.push(Instance {
                pos: [x + sprite.x as f32, y + sprite.y as f32],
                size: [r.width as f32, r.height as f32],
                // Normalized: the atlases can differ in size.
                uv: [r.x as f32, r.y as f32, r.width as f32, r.height as f32].map(|v| v * texel),
                color: rgba(color),
                kind: if sprite.color { KIND_COLOR } else { KIND_MASK },
                _pad: [0; 3],
            });
        }
    }

    fn push_pane(&mut self, snapshot: &Snapshot, x0: f32, y0: f32) -> Result<(), AtlasFull> {
        let m = self.fonts.metrics();
        let (cw, ch) = (m.width as f32, m.height as f32);
        let origin = |column: usize, line: usize| (x0 + column as f32 * cw, y0 + line as f32 * ch);
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
                let sprites = self.sprites(cell)?;
                self.push_sprites(&sprites, x, y, fg);
            }
        }
        Ok(())
    }

    /// Look up or rasterize the glyphs for a cell.
    fn sprites(&mut self, cell: &SnapshotCell) -> Result<Rc<[Sprite]>, AtlasFull> {
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
        self.glyph_sprites(key)
    }

    /// Look up or rasterize the glyphs for a key.
    fn glyph_sprites(&mut self, key: GlyphKey) -> Result<Rc<[Sprite]>, AtlasFull> {
        if let Some(sprites) = self.glyphs.get(&key) {
            return Ok(sprites.clone());
        }

        let (text, style) = match &key {
            GlyphKey::Char(c, style) => (c.to_string(), *style),
            GlyphKey::Cluster(s, style) => (s.to_string(), *style),
        };
        let metrics = self.fonts.metrics();
        let queue = &self.gpu.queue;
        let mut sprites = Vec::new();

        // Box-drawing and block characters are drawn to fill the cell exactly.
        if let GlyphKey::Char(c, _) = key
            && let Some(mask) =
                box_drawing::rasterize(c, metrics.width, metrics.height, metrics.stroke)
        {
            let region = self
                .mask_atlas
                .insert(queue, metrics.width, metrics.height, &mask)
                .ok_or(AtlasFull)?;
            sprites.push(Sprite {
                x: 0,
                y: 0,
                region,
                color: false,
            });
        }

        let baseline = metrics.baseline as i32;
        let glyphs = if sprites.is_empty() {
            self.fonts.rasterize(&text, style)
        } else {
            Vec::new()
        };
        for glyph in glyphs {
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
        let sprites: Rc<[Sprite]> = sprites.into();
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
