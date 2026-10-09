use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::ops::Range;
use std::rc::Rc;

use bytemuck::{Pod, Zeroable};
use nuntio_term::{
    CursorStyle, ImagePiece, MAX_ZEROWIDTH, Rgb, Snapshot, SnapshotCell, UnderlineStyle,
};
use unicode_width::UnicodeWidthChar;
use wgpu::rwh::{HasDisplayHandle, HasWindowHandle};

use crate::atlas::{Atlas, AtlasRegion, MIN_ATLAS_SIZE};
use crate::font::{CellMetrics, FaceStyle, Fonts};
use crate::frame::{Frame, Rect, UiRect, UiText};
use crate::gpu::{FrameStatus, GpuContext, GpuError, GpuOptions, oom_checked};
use crate::{box_drawing, decoration};

const KIND_SOLID: u32 = 0;
const KIND_MASK: u32 = 1;
const KIND_COLOR: u32 = 2;
const KIND_ROUNDED: u32 = 3;
const KIND_IMAGE: u32 = 4;
const KIND_IMAGE_SCALED: u32 = 5;
/// Upper bound for the mask atlas: 16 MiB at one byte per texel.
const MAX_MASK_ATLAS_SIZE: u32 = 4096;
/// Starting size of the mask atlas (1 MiB); enough for ASCII plus some
/// symbols at common font sizes, doubles when full.
const INITIAL_MASK_ATLAS_SIZE: u32 = 1024;
/// Starting size of the color atlas (1 MiB at four bytes per texel); stays
/// that small unless emoji show up.
const INITIAL_COLOR_ATLAS_SIZE: u32 = 512;
/// Starting size of the image atlas (4 MiB); grows up to
/// `MAX_IMAGE_ATLAS_SIZE` (256 MiB) when inline images need it.
const INITIAL_IMAGE_ATLAS_SIZE: u32 = 1024;
const MAX_IMAGE_ATLAS_SIZE: u32 = 8192;

/// Consecutive overflowing frames between two clears of the glyph atlas.
const OVERFLOW_RETRY_FRAMES: u32 = 32;

/// Most clusters (characters with combining marks) kept; the cache starts
/// over beyond that.
const MAX_CLUSTERS: usize = 4096;

/// Whether the atlas may be cleared in a frame that follows
/// `overflow_frames` overflowing ones.
fn may_clear_atlas(overflow_frames: u32) -> bool {
    overflow_frames.is_multiple_of(OVERFLOW_RETRY_FRAMES)
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct Instance {
    pos: [f32; 2],
    size: [f32; 2],
    uv: [f32; 4],
    /// Images: their atlas region (x0, y0, x1, y1), normalized.
    color: [f32; 4],
    kind: u32,
    _pad: [u32; 3],
}

impl Instance {
    fn solid_rect(rect: Rect, color: Rgb) -> Self {
        Self::solid(rect.x, rect.y, rect.width, rect.height, color)
    }

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

    fn fill(rect: Rect, color: [f32; 4]) -> Self {
        let mut i = Self::solid_rect(rect, Rgb { r: 0, g: 0, b: 0 });
        i.color = color;
        i
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

/// The color a cleared or filled default background is stored as: with
/// `transparent` its alpha is the window opacity, premultiplied if the
/// surface expects that.
fn base_color(background: Rgb, opacity: f32, transparent: bool, premultiplied: bool) -> [f32; 4] {
    let [r, g, b, _] = rgba(background);
    let a = if transparent {
        opacity.clamp(0.0, 1.0)
    } else {
        1.0
    };
    if premultiplied {
        [r * a, g * a, b * a, a]
    } else {
        [r, g, b, a]
    }
}

/// The instances of one pane, clipped to its area.
struct PaneBatch {
    /// Whether the first instance is a fill of `area`, drawn with the fill pipeline.
    fill: bool,
    range: Range<u32>,
    area: Rect,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum GlyphKey {
    Char(char, FaceStyle),
    /// A character of small UI text.
    Small(char, FaceStyle),
    /// A patterned underline, one cell wide.
    Underline(UnderlineStyle),
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

/// The atlas that ran out of room.
#[derive(Debug, Clone, Copy)]
enum AtlasFull {
    Mask,
    Color,
}

/// Cached sprites of one cluster, one slot per size/bold/italic combination.
type ClusterSlots = [Option<Rc<[Sprite]>>; 8];

/// A frame read back by [`Renderer::capture`].
#[cfg(feature = "capture")]
#[derive(Debug, Clone)]
pub struct Capture {
    pub width: u32,
    pub height: u32,
    /// Rows of RGBA pixels, top to bottom; premultiplied if the window is
    /// transparent.
    pub rgba: Vec<u8>,
}

pub struct Renderer {
    gpu: GpuContext,
    fonts: Fonts,
    mask_atlas: Atlas,
    color_atlas: Atlas,
    /// Inline images, straight RGBA.
    image_atlas: Atlas,
    /// Where each uploaded image is, by [`TermImage::uid`](nuntio_term::TermImage::uid).
    image_regions: HashMap<u64, AtlasRegion>,
    /// Scratch: the uids of the images in the frame being prepared.
    image_uids: HashSet<u64>,
    glyphs: HashMap<GlyphKey, Rc<[Sprite]>>,
    /// Glyphs of characters with combining marks, by text and a slot for
    /// each size/bold/italic combination.
    clusters: HashMap<Box<str>, ClusterSlots>,
    /// Scratch buffer for building cluster text without allocating.
    cluster_text: String,
    pipeline: wgpu::RenderPipeline,
    /// Fills a pane whose default background differs from the clear color.
    fill_pipeline: wgpu::RenderPipeline,
    /// Cuts the window's rounded corners out of the finished frame.
    cutout_pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    /// Kept to rebuild `bind_group` when an atlas texture grows.
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Linear filtering for images that are drawn at another scale than their bitmap.
    image_sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    instances: Vec<Instance>,
    /// The instances of each pane and the area they are clipped to, so
    /// glyphs wider than their cell don't reach into the next pane.
    pane_batches: Vec<PaneBatch>,
    /// Instances from here on are UI (bars, overlays), drawn unclipped.
    ui_start: usize,
    /// Instances from here on are corner cutouts, drawn with their own
    /// pipeline.
    cutout_start: usize,
    /// Problem with the configured font, reported once.
    font_warning: Option<String>,
    /// Consecutive frames in which glyphs didn't fit into the atlas and
    /// were left out.
    overflow_frames: u32,
    /// Glyphs that don't fit into the atlas stay blank for this frame.
    skip_missing: bool,
    /// The last frame had more instances than the GPU buffer holds (already
    /// warned).
    instances_truncated: bool,
    /// The last frame's images didn't fit into the image atlas (already
    /// warned).
    image_overflow: bool,
}

impl Renderer {
    pub fn new<W>(
        window: W,
        width: u32,
        height: u32,
        scale_factor: f64,
        font_family: Option<String>,
        font_size: f32,
        options: GpuOptions,
    ) -> Result<Self, GpuError>
    where
        W: HasWindowHandle + HasDisplayHandle + Debug + Clone + Send + Sync + 'static,
    {
        // Without any font there is nothing to draw: say so before the GPU
        // is set up.
        let (fonts, font_warning) = Fonts::new(font_family, font_size, scale_factor)?;
        let gpu = GpuContext::new(window, width, height, options)?;
        let device = &gpu.device;

        // Large fonts on HiDPI screens need room for many glyph masks (one
        // byte per texel). Color glyphs (emoji) are rarer and cost four.
        // Both start small and double when full, up to these maximums.
        let mask_max = device
            .limits()
            .max_texture_dimension_2d
            .clamp(MIN_ATLAS_SIZE, MAX_MASK_ATLAS_SIZE);
        let mask_atlas = Atlas::new(
            device,
            wgpu::TextureFormat::R8Unorm,
            "mask atlas",
            INITIAL_MASK_ATLAS_SIZE.min(mask_max),
            mask_max,
        );
        let color_atlas = Atlas::new(
            device,
            wgpu::TextureFormat::Rgba8Unorm,
            "color atlas",
            INITIAL_COLOR_ATLAS_SIZE,
            MIN_ATLAS_SIZE,
        );
        let image_atlas = Atlas::new(
            device,
            wgpu::TextureFormat::Rgba8Unorm,
            "image atlas",
            INITIAL_IMAGE_ATLAS_SIZE,
            device
                .limits()
                .max_texture_dimension_2d
                .clamp(MIN_ATLAS_SIZE, MAX_IMAGE_ATLAS_SIZE),
        );
        // Glyphs are drawn 1:1; nearest sampling keeps them sharp even when
        // a quad lands on a fractional position.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let image_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("image sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let (bind_group_layout, pipeline, fill_pipeline, cutout_pipeline) =
            create_pipelines(device, gpu.format());

        let instance_capacity = MIN_INSTANCE_CAPACITY;
        let instance_buffer = create_instance_buffer(device, instance_capacity);
        let bind_group = create_bind_group(
            device,
            &bind_group_layout,
            &uniforms,
            &mask_atlas,
            &color_atlas,
            &image_atlas,
            [&sampler, &image_sampler],
        );

        Ok(Self {
            gpu,
            fonts,
            mask_atlas,
            color_atlas,
            image_atlas,
            image_regions: HashMap::new(),
            image_uids: HashSet::new(),
            glyphs: HashMap::new(),
            clusters: HashMap::new(),
            cluster_text: String::new(),
            pipeline,
            fill_pipeline,
            cutout_pipeline,
            bind_group,
            bind_group_layout,
            sampler,
            image_sampler,
            uniforms,
            instance_buffer,
            instance_capacity,
            instances: Vec::new(),
            pane_batches: Vec::new(),
            ui_start: 0,
            cutout_start: 0,
            font_warning,
            overflow_frames: 0,
            skip_missing: false,
            instances_truncated: false,
            image_overflow: false,
        })
    }

    /// Give up the window's surface, so that a new renderer can be created
    /// for the same window. Frames report `Lost` until then, or until
    /// `restore_surface`.
    pub fn release_surface(&mut self) {
        self.gpu.release_surface();
    }

    /// Take the window's surface back after `release_surface`, when no new
    /// renderer could be created. `window` is the one this renderer was
    /// created for.
    pub fn restore_surface<W>(&mut self, window: W) -> Result<(), GpuError>
    where
        W: HasWindowHandle + HasDisplayHandle + Debug + Send + Sync + 'static,
    {
        self.gpu.restore_surface(window)
    }

    /// Drawing happens on the CPU (a software adapter).
    pub fn software(&self) -> bool {
        self.gpu.software()
    }

    /// A software renderer could be created for the window (see
    /// `GpuContext::offers_software`).
    pub fn offers_software(&self) -> bool {
        self.gpu.offers_software()
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

    /// Cell metrics of small UI text ([`UiText::small`]).
    pub fn small_cell_metrics(&self) -> CellMetrics {
        self.fonts.small_metrics()
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
        self.clusters.clear();
        self.mask_atlas.clear();
        self.color_atlas.clear();
        self.overflow_frames = 0;
    }

    /// Grow the atlas that ran full and drop all glyphs, which pointed into
    /// the old texture. `false` if it is already at its maximum size.
    fn grow_atlas(&mut self, full: AtlasFull) -> bool {
        let atlas = match full {
            AtlasFull::Mask => &mut self.mask_atlas,
            AtlasFull::Color => &mut self.color_atlas,
        };
        if !atlas.grow(&self.gpu.device) {
            return false;
        }
        self.clear_glyphs();
        self.rebuild_bind_group();
        true
    }

    /// Draw a frame and present it. A frame that isn't presented (the
    /// window is occluded, the surface or device is lost) uploads nothing.
    pub fn render(&mut self, frame: &Frame) -> FrameStatus {
        let surface_texture = match self.gpu.acquire() {
            Ok(texture) => texture,
            Err(status) => return status,
        };
        self.prepare(frame);
        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        self.encode_pass(&mut encoder, &view, frame);
        self.gpu.queue.submit([encoder.finish()]);
        self.gpu.queue.present(surface_texture);
        self.gpu.presented();
        FrameStatus::Presented
    }

    /// Draw a frame into an offscreen texture of the window's size and read
    /// it back, as the window would show it. Doesn't need the surface, so
    /// it also works while the window is covered or minimized.
    #[cfg(feature = "capture")]
    pub fn capture(&mut self, frame: &Frame) -> Result<Capture, GpuError> {
        if self.gpu.lost() {
            return Err(GpuError::Capture("GPU device lost".into()));
        }
        self.prepare(frame);
        let device = &self.gpu.device;
        let (width, height) = self.gpu.size();
        let format = self.gpu.format();
        let bgra = match format {
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => true,
            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => false,
            other => return Err(GpuError::Capture(format!("unsupported format {other:?}"))),
        };
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let padded_row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let (texture, buffer) = oom_checked(device, || {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("capture"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("capture"),
                size: u64::from(padded_row) * u64::from(height),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            (texture, buffer)
        })
        .ok_or_else(|| GpuError::Capture("out of memory".into()))?;
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("capture"),
        });
        self.encode_pass(&mut encoder, &view, frame);
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: None,
                },
            },
            size,
        );
        self.gpu.queue.submit([encoder.finish()]);

        let (tx, rx) = std::sync::mpsc::channel();
        buffer.map_async(wgpu::MapMode::Read, .., move |result| {
            let _ = tx.send(result);
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|err| GpuError::Capture(err.to_string()))?;
        rx.recv()
            .map_err(|err| GpuError::Capture(err.to_string()))?
            .map_err(|err| GpuError::Capture(err.to_string()))?;
        let rgba = {
            let data = buffer
                .get_mapped_range(..)
                .map_err(|err| GpuError::Capture(err.to_string()))?;
            unpad_rows(&data, width, height, padded_row, bgra)
        };
        buffer.unmap();
        Ok(Capture {
            width,
            height,
            rgba,
        })
    }

    /// Lay out the frame's instances and upload them with the uniforms.
    fn prepare(&mut self, frame: &Frame) {
        self.upload_images(frame);
        // While the atlas stays too small for the screen, clearing it again
        // and again would re-rasterize everything every frame: only now and
        // then, and draw what fits in between.
        let streak = self.overflow_frames;
        let mut cleared = !may_clear_atlas(streak);
        self.skip_missing = false;
        loop {
            let Err(full) = self.build_instances(frame) else {
                self.overflow_frames = if self.skip_missing {
                    self.overflow_frames.saturating_add(1)
                } else {
                    0
                };
                break;
            };
            // Grow before clearing, so the atlas settles at the size the
            // session's glyphs need instead of re-rasterizing every frame.
            if self.grow_atlas(full) {
                cleared = true;
                continue;
            }
            if !cleared {
                // The atlas filled up mid-frame: start over with an empty one.
                tracing::debug!("glyph atlas full, clearing");
                self.clear_glyphs();
                cleared = true;
                continue;
            }
            // Warn once, not on every frame while it stays too full.
            if streak == 0 {
                tracing::warn!("screen content does not fit into the glyph atlas");
            }
            // Glyphs that don't fit stay blank; everything else is drawn.
            self.skip_missing = true;
        }

        let (width, height) = self.gpu.size();
        let uniforms = Uniforms {
            screen_size: [width as f32, height as f32],
            _pad: [0.0; 2],
        };
        self.gpu
            .queue
            .write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&uniforms));

        let limit = max_instances(self.gpu.device.limits().max_buffer_size);
        let mut truncated = self.limit_instances(limit);
        let wanted = next_capacity(self.instance_capacity, self.instances.len(), limit);
        if wanted != self.instance_capacity {
            match try_create_instance_buffer(&self.gpu.device, wanted) {
                Some(buffer) => {
                    self.instance_buffer = buffer;
                    self.instance_capacity = wanted;
                }
                // No memory to grow into: draw what the old buffer holds.
                None if wanted > self.instance_capacity => {
                    truncated |= self.limit_instances(self.instance_capacity);
                }
                None => {}
            }
        }
        if truncated && !self.instances_truncated {
            tracing::warn!("frame has more instances than the GPU buffer limit; truncated");
        }
        self.instances_truncated = truncated;
        self.gpu.queue.write_buffer(
            &self.instance_buffer,
            0,
            bytemuck::cast_slice(&self.instances),
        );
    }

    /// Cut the layout to `limit` instances. Returns whether it was cut.
    fn limit_instances(&mut self, limit: usize) -> bool {
        truncate_layout(
            &mut self.instances,
            limit,
            &mut self.pane_batches,
            &mut self.ui_start,
            &mut self.cutout_start,
        )
    }

    /// Bind the current atlas textures, after one was replaced.
    fn rebuild_bind_group(&mut self) {
        self.bind_group = create_bind_group(
            &self.gpu.device,
            &self.bind_group_layout,
            &self.uniforms,
            &self.mask_atlas,
            &self.color_atlas,
            &self.image_atlas,
            [&self.sampler, &self.image_sampler],
        );
    }

    /// Upload the frame's inline images that aren't in the image atlas yet.
    fn upload_images(&mut self, frame: &Frame) {
        if frame.panes.iter().all(|p| p.snapshot.images.is_empty()) {
            // Give the memory back that a large image needed.
            if self.image_atlas.size() > INITIAL_IMAGE_ATLAS_SIZE
                && self
                    .image_atlas
                    .reset(&self.gpu.device, INITIAL_IMAGE_ATLAS_SIZE)
            {
                self.image_regions.clear();
                self.rebuild_bind_group();
            }
            self.image_overflow = false;
            return;
        }
        self.image_uids.clear();
        self.image_uids.extend(
            frame
                .panes
                .iter()
                .flat_map(|p| &p.snapshot.images)
                .map(|piece| piece.image.uid),
        );
        let mut cleared = false;
        'pass: loop {
            let mut skipped = false;
            for piece in frame.panes.iter().flat_map(|p| &p.snapshot.images) {
                let image = &piece.image;
                if self.image_regions.contains_key(&image.uid) {
                    continue;
                }
                let queue = &self.gpu.queue;
                if let Some(region) =
                    self.image_atlas
                        .insert(queue, image.width, image.height, &image.rgba)
                {
                    self.image_regions.insert(image.uid, region);
                    continue;
                }
                let max = self.image_atlas.max_size();
                if image.width + 1 > max || image.height + 1 > max {
                    skipped = true;
                } else if !cleared
                    && self
                        .image_regions
                        .keys()
                        .any(|uid| !self.image_uids.contains(uid))
                {
                    // Images no longer on screen hold space: start over
                    // before spending more memory. Without any, clearing
                    // would re-upload the same images every frame.
                    self.image_atlas.clear();
                    self.image_regions.clear();
                    cleared = true;
                    continue 'pass;
                } else if self.image_atlas.grow(&self.gpu.device) {
                    self.image_regions.clear();
                    self.rebuild_bind_group();
                    continue 'pass;
                } else {
                    skipped = true;
                }
            }
            if skipped && !self.image_overflow {
                tracing::warn!("inline images do not fit into the image atlas");
            }
            self.image_overflow = skipped;
            break;
        }
    }

    /// Record the render pass of a `prepare`d frame into `view`.
    fn encode_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        frame: &Frame,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("terminal"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(self.clear_color(frame)),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        if self.instances.is_empty() {
            return;
        }
        let (width, height) = self.gpu.size();
        let ui = self.ui_start as u32;
        let (cutout, total) = (self.cutout_start as u32, self.instances.len() as u32);
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        for batch in &self.pane_batches {
            // A pane outside the window has nothing to show.
            if let Some([x, y, w, h]) = scissor(batch.area, (width, height)) {
                pass.set_scissor_rect(x, y, w, h);
                let mut range = batch.range.clone();
                if batch.fill {
                    pass.set_pipeline(&self.fill_pipeline);
                    pass.draw(0..4, range.start..range.start + 1);
                    range.start += 1;
                    pass.set_pipeline(&self.pipeline);
                }
                pass.draw(0..4, range);
            }
        }
        pass.set_scissor_rect(0, 0, width, height);
        pass.draw(0..4, ui..cutout);
        if total > cutout {
            pass.set_pipeline(&self.cutout_pipeline);
            pass.draw(0..4, cutout..total);
        }
    }

    /// The frame's background, as the color the pass starts from.
    fn clear_color(&self, frame: &Frame) -> wgpu::Color {
        let c = base_color(
            frame.background,
            frame.background_opacity,
            self.gpu.transparent(),
            self.gpu.premultiplied(),
        );
        wgpu::Color {
            r: f64::from(c[0]),
            g: f64::from(c[1]),
            b: f64::from(c[2]),
            a: f64::from(c[3]),
        }
    }

    fn build_instances(&mut self, frame: &Frame) -> Result<(), AtlasFull> {
        self.instances.clear();
        self.pane_batches.clear();
        // Not reached yet; stale indices would point into the last frame.
        self.ui_start = usize::MAX;
        self.cutout_start = usize::MAX;
        for pane in frame.panes {
            let start = self.instances.len() as u32;
            // A pane whose default background differs from the clear color is
            // filled first, so `push_pane` can skip runs equal to it.
            let fill = pane.snapshot.background != frame.background;
            if fill {
                let color = base_color(
                    pane.snapshot.background,
                    frame.background_opacity,
                    self.gpu.transparent(),
                    self.gpu.premultiplied(),
                );
                self.instances.push(Instance::fill(pane.area, color));
            }
            self.push_pane(pane.snapshot, pane.x, pane.y)?;
            if pane.dim > 0.0 {
                let mut veil = Instance::solid_rect(pane.area, pane.snapshot.background);
                veil.color[3] = pane.dim.min(1.0);
                self.instances.push(veil);
            }
            let end = self.instances.len() as u32;
            self.pane_batches.push(PaneBatch {
                fill,
                range: start..end,
                area: pane.area,
            });
        }
        self.ui_start = self.instances.len();
        self.push_ui(frame.rects, frame.texts)?;
        self.push_ui(frame.popup_rects, frame.popup_texts)?;
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

    /// UI rectangles, then the text over them.
    fn push_ui(&mut self, rects: &[UiRect], texts: &[UiText]) -> Result<(), AtlasFull> {
        for r in rects {
            let mut instance = Instance::solid(r.x, r.y, r.width, r.height, r.color);
            if r.radius > 0.0 {
                instance.kind = KIND_ROUNDED;
                instance.uv[0] = r.radius;
            }
            self.instances.push(instance);
        }
        for text in texts {
            self.push_text(text)?;
        }
        Ok(())
    }

    fn push_text(&mut self, text: &UiText) -> Result<(), AtlasFull> {
        let metrics = if text.small {
            self.fonts.small_metrics()
        } else {
            self.fonts.metrics()
        };
        let cw = metrics.width as f32;
        let style = FaceStyle {
            bold: text.bold,
            italic: false,
        };
        let mut x = text.x;
        let mut chars = text.text.chars().peekable();
        while let Some(c) = chars.next() {
            let width = c.width().unwrap_or(0);
            if width == 0 {
                continue;
            }
            let mut marks = std::mem::take(&mut self.cluster_text);
            marks.clear();
            marks.push(c);
            let mut mark_count = 0;
            while let Some(&mark) = chars.peek() {
                if mark.width() != Some(0) {
                    break;
                }
                // Consume every mark, but keep only as many as a cell may have.
                if mark_count < MAX_ZEROWIDTH {
                    marks.push(mark);
                    mark_count += 1;
                }
                chars.next();
            }
            let has_marks = marks.len() > c.len_utf8();
            let sprites = if has_marks {
                Some(self.cluster_sprites(&marks, style, text.small))
            } else if c != ' ' {
                let key = if text.small {
                    GlyphKey::Small(c, style)
                } else {
                    GlyphKey::Char(c, style)
                };
                Some(self.glyph_sprites(key))
            } else {
                None
            };
            self.cluster_text = marks;
            if let Some(sprites) = sprites {
                self.push_sprites(&sprites?, x, text.y, text.color);
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

        // Images cover the background, below the cursor and text.
        for piece in &snapshot.images {
            let Some(&region) = self.image_regions.get(&piece.image.uid) else {
                continue;
            };
            let (x, y) = origin(piece.column, piece.line);
            self.instances.push(Instance {
                pos: [x, y],
                size: [piece.columns as f32 * cw, ch],
                uv: image_uv(region, self.image_atlas.size(), piece),
                color: image_bounds(region, self.image_atlas.size()),
                kind: if region.width as usize == piece.image.columns * m.width as usize
                    && region.height as usize == piece.image.lines * m.height as usize
                {
                    KIND_IMAGE
                } else {
                    KIND_IMAGE_SCALED
                },
                _pad: [0; 3],
            });
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

                if let Some(style) = cell.style.underline {
                    // The cursor's contrast color wins over a colored underline.
                    let color = match cell.underline_color {
                        Some(color) if !under_block => color,
                        _ => fg,
                    };
                    let cells = if cell.style.wide { 2 } else { 1 };
                    self.push_underline(style, x, y, cells, color)?;
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

    /// Draw an underline below `cells` cells starting at the cell corner
    /// `x`, `y`.
    fn push_underline(
        &mut self,
        style: UnderlineStyle,
        x: f32,
        y: f32,
        cells: u32,
        color: Rgb,
    ) -> Result<(), AtlasFull> {
        let m = self.fonts.metrics();
        let stroke = m.stroke.max(1) as f32;
        let width = (cells * m.width) as f32;
        let top = y + m.underline_y as f32;
        match style {
            UnderlineStyle::Single => {
                self.instances
                    .push(Instance::solid(x, top, width, stroke, color));
            }
            UnderlineStyle::Double => {
                // Two lines one stroke apart, moved up if the cell is too short.
                let top = top.min(y + m.height as f32 - 3.0 * stroke);
                for top in [top, top + 2.0 * stroke] {
                    self.instances
                        .push(Instance::solid(x, top, width, stroke, color));
                }
            }
            UnderlineStyle::Curly | UnderlineStyle::Dotted | UnderlineStyle::Dashed => {
                let sprites = self.glyph_sprites(GlyphKey::Underline(style))?;
                for i in 0..cells {
                    self.push_sprites(&sprites, x + (i * m.width) as f32, y, color);
                }
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
        match &cell.zerowidth {
            None => self.glyph_sprites(GlyphKey::Char(cell.c, style)),
            Some(marks) => {
                let mut text = std::mem::take(&mut self.cluster_text);
                text.clear();
                text.push(cell.c);
                text.extend(marks.iter().copied());
                let result = self.cluster_sprites(&text, style, false);
                self.cluster_text = text;
                result
            }
        }
    }

    /// Look up or rasterize the glyphs of a base character with combining
    /// marks, without allocating on a hit.
    fn cluster_sprites(
        &mut self,
        text: &str,
        style: FaceStyle,
        small: bool,
    ) -> Result<Rc<[Sprite]>, AtlasFull> {
        let slot = usize::from(small) * 4 + usize::from(style.bold) * 2 + usize::from(style.italic);
        if let Some(sprites) = self
            .clusters
            .get(text)
            .and_then(|slots| slots[slot].as_ref())
        {
            return Ok(sprites.clone());
        }
        let sprites: Rc<[Sprite]> = match self.font_sprites(text, style, small) {
            Ok(sprites) => sprites.into(),
            // Blank for this frame; not cached, so it is tried again later.
            Err(_) if self.skip_missing => return Ok(Rc::from(Vec::new())),
            Err(full) => return Err(full),
        };
        // Only the map is cleared, never the atlas: regions already used by
        // this frame's instances must stay valid.
        if self.clusters.len() >= MAX_CLUSTERS {
            self.clusters.clear();
        }
        self.clusters.entry(text.into()).or_default()[slot] = Some(sprites.clone());
        Ok(sprites)
    }

    /// Look up or rasterize the glyphs for a key.
    fn glyph_sprites(&mut self, key: GlyphKey) -> Result<Rc<[Sprite]>, AtlasFull> {
        if let Some(sprites) = self.glyphs.get(&key) {
            return Ok(sprites.clone());
        }
        let built = match &key {
            GlyphKey::Char(c, style) => self.char_sprites(*c, *style, false),
            GlyphKey::Small(c, style) => self.char_sprites(*c, *style, true),
            GlyphKey::Underline(style) => self.underline_sprites(*style),
        };
        let sprites: Rc<[Sprite]> = match built {
            Ok(sprites) => sprites.into(),
            // Blank for this frame; not cached, so it is tried again later.
            Err(_) if self.skip_missing => return Ok(Rc::from(Vec::new())),
            Err(full) => return Err(full),
        };
        self.glyphs.insert(key, sprites.clone());
        Ok(sprites)
    }

    /// Box-drawing and block characters are drawn to fill the cell exactly,
    /// like a few symbols that fonts rarely cover (see `box_drawing`); all
    /// others come from the font.
    fn char_sprites(
        &mut self,
        c: char,
        style: FaceStyle,
        small: bool,
    ) -> Result<Vec<Sprite>, AtlasFull> {
        let metrics = if small {
            self.fonts.small_metrics()
        } else {
            self.fonts.metrics()
        };
        let Some(mask) = box_drawing::rasterize(c, metrics.width, metrics.height, metrics.stroke)
        else {
            return self.font_sprites(&c.to_string(), style, small);
        };
        let region = self
            .mask_atlas
            .insert(&self.gpu.queue, metrics.width, metrics.height, &mask)
            .ok_or(AtlasFull::Mask)?;
        Ok(vec![Sprite {
            x: 0,
            y: 0,
            region,
            color: false,
        }])
    }

    /// Rasterize `text` with the font into the atlases.
    fn font_sprites(
        &mut self,
        text: &str,
        style: FaceStyle,
        small: bool,
    ) -> Result<Vec<Sprite>, AtlasFull> {
        let baseline = if small {
            self.fonts.small_metrics().baseline
        } else {
            self.fonts.metrics().baseline
        } as i32;
        let mut sprites = Vec::new();
        for glyph in self.fonts.rasterize(text, style, small) {
            let atlas = if glyph.color {
                &mut self.color_atlas
            } else {
                &mut self.mask_atlas
            };
            let region = atlas
                .insert(&self.gpu.queue, glyph.width, glyph.height, &glyph.data)
                .ok_or(if glyph.color {
                    AtlasFull::Color
                } else {
                    AtlasFull::Mask
                })?;
            sprites.push(Sprite {
                x: glyph.left,
                y: baseline - glyph.top,
                region,
                color: glyph.color,
            });
        }
        Ok(sprites)
    }

    /// Rasterize a patterned underline into the mask atlas, placed at the
    /// font's underline position but kept inside the cell.
    fn underline_sprites(&mut self, style: UnderlineStyle) -> Result<Vec<Sprite>, AtlasFull> {
        let m = self.fonts.metrics();
        let Some(mask) = decoration::rasterize(style, m.width, m.stroke) else {
            return Ok(Vec::new());
        };
        let region = self
            .mask_atlas
            .insert(&self.gpu.queue, mask.width, mask.height, &mask.data)
            .ok_or(AtlasFull::Mask)?;
        let y = m.underline_y.min(m.height.saturating_sub(mask.height));
        Ok(vec![Sprite {
            x: 0,
            y: y as i32,
            region,
            color: false,
        }])
    }
}

/// The scissor rectangle (x, y, width, height) for an `area` on a target of
/// `size`: whole pixels that cover the area, inside the target. `None` if
/// nothing of it is visible.
fn scissor(area: Rect, (width, height): (u32, u32)) -> Option<[u32; 4]> {
    let Rect {
        x,
        y,
        width: w,
        height: h,
    } = area;
    let clamp = |v: f32, max: u32| (v.max(0.0) as u32).min(max);
    let (left, top) = (clamp(x.floor(), width), clamp(y.floor(), height));
    let (right, bottom) = (clamp((x + w).ceil(), width), clamp((y + h).ceil(), height));
    (right > left && bottom > top).then(|| [left, top, right - left, bottom - top])
}

/// Tightly packed RGBA rows from a texture copy whose rows are
/// `padded_row` bytes long, swapping red and blue for BGRA textures.
#[cfg(feature = "capture")]
fn unpad_rows(data: &[u8], width: u32, height: u32, padded_row: u32, bgra: bool) -> Vec<u8> {
    let row = width as usize * 4;
    let mut rgba = Vec::with_capacity(row * height as usize);
    for line in data.chunks(padded_row as usize).take(height as usize) {
        rgba.extend_from_slice(&line[..row]);
    }
    if bgra {
        for pixel in rgba.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
    }
    rgba
}

/// The layout of the quad shader's bindings, and its three pipelines: one
/// that draws the quads, one that fills a pane's own background (replacing
/// what is there, like the clear), one that cuts the window's corners out.
fn create_pipelines(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
) -> (
    wgpu::BindGroupLayout,
    wgpu::RenderPipeline,
    wgpu::RenderPipeline,
    wgpu::RenderPipeline,
) {
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
            texture_entry(4),
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 5,
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
                    format,
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
    // Replaces what is there, like the clear: a pane's own default background.
    let fill_pipeline = create_pipeline("pane fill", "fs_main", wgpu::BlendState::REPLACE);
    let cutout_pipeline = create_pipeline(
        "corner cutout",
        "fs_cutout",
        wgpu::BlendState {
            color: scale_by_alpha,
            alpha: scale_by_alpha,
        },
    );
    (bind_group_layout, pipeline, fill_pipeline, cutout_pipeline)
}

fn create_instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("instances"),
        size: (capacity * size_of::<Instance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Smallest instance buffer, in instances.
const MIN_INSTANCE_CAPACITY: usize = 4096;

/// Most instances a buffer of `max_buffer_size` bytes can hold.
fn max_instances(max_buffer_size: u64) -> usize {
    usize::try_from(max_buffer_size / size_of::<Instance>() as u64).unwrap_or(usize::MAX)
}

/// The capacity the instance buffer should have for `len` instances: it
/// grows to the next power of two (at most `limit`) and shrinks when fewer
/// than a quarter is used, so one huge frame doesn't pin its memory.
fn next_capacity(capacity: usize, len: usize, limit: usize) -> usize {
    if len > capacity {
        len.next_power_of_two().min(limit)
    } else if capacity > MIN_INSTANCE_CAPACITY && len < capacity / 4 {
        (len.max(1).next_power_of_two() * 2)
            .max(MIN_INSTANCE_CAPACITY)
            .min(capacity)
    } else {
        capacity
    }
}

/// Cut the layout to `limit` instances, keeping the pane batches and the
/// UI/cutout starts inside it. Returns whether anything was cut.
fn truncate_layout(
    instances: &mut Vec<Instance>,
    limit: usize,
    batches: &mut Vec<PaneBatch>,
    ui_start: &mut usize,
    cutout_start: &mut usize,
) -> bool {
    if instances.len() <= limit {
        return false;
    }
    instances.truncate(limit);
    let end = u32::try_from(limit).unwrap_or(u32::MAX);
    batches.retain(|batch| batch.range.start < end);
    for batch in batches.iter_mut() {
        batch.range.end = batch.range.end.min(end);
    }
    *ui_start = (*ui_start).min(limit);
    *cutout_start = (*cutout_start).min(limit);
    true
}

/// Like `create_instance_buffer`, but `None` if the GPU is out of memory.
fn try_create_instance_buffer(device: &wgpu::Device, capacity: usize) -> Option<wgpu::Buffer> {
    oom_checked(device, || create_instance_buffer(device, capacity))
}

fn create_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniforms: &wgpu::Buffer,
    mask_atlas: &Atlas,
    color_atlas: &Atlas,
    image_atlas: &Atlas,
    // The nearest sampler for glyphs and pixel-exact images, then the linear one for scaled images.
    [sampler, image_sampler]: [&wgpu::Sampler; 2],
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
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(image_atlas.view()),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::Sampler(image_sampler),
            },
        ],
    })
}

/// The atlas coordinates of the part of an image a piece shows, normalized:
/// x, y, width, height.
fn image_uv(region: AtlasRegion, atlas_size: u32, piece: &ImagePiece) -> [f32; 4] {
    let image = &piece.image;
    let sx = region.width as f32 / image.columns as f32;
    let sy = region.height as f32 / image.lines as f32;
    let size = atlas_size as f32;
    [
        (region.x as f32 + piece.image_column as f32 * sx) / size,
        (region.y as f32 + piece.image_line as f32 * sy) / size,
        piece.columns as f32 * sx / size,
        sy / size,
    ]
}

/// An image's whole atlas region, normalized: x0, y0, x1, y1. Scaled
/// sampling stays inside it.
fn image_bounds(region: AtlasRegion, atlas_size: u32) -> [f32; 4] {
    let size = atlas_size as f32;
    [
        region.x as f32 / size,
        region.y as f32 / size,
        (region.x + region.width) as f32 / size,
        (region.y + region.height) as f32 / size,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_bounds_are_the_normalized_region() {
        let region = AtlasRegion {
            x: 10,
            y: 20,
            width: 30,
            height: 40,
        };
        assert_eq!(image_bounds(region, 100), [0.1, 0.2, 0.4, 0.6]);
    }

    #[test]
    fn base_color_matches_the_clear() {
        let red = Rgb { r: 255, g: 0, b: 0 };
        assert_eq!(base_color(red, 0.5, true, true), [0.5, 0.0, 0.0, 0.5]);
        assert_eq!(base_color(red, 0.5, true, false), [1.0, 0.0, 0.0, 0.5]);
        assert_eq!(base_color(red, 0.5, false, true), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(base_color(red, 2.0, true, true), [1.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn image_pieces_map_to_their_part_of_the_region() {
        let image = std::sync::Arc::new(nuntio_term::TermImage {
            uid: 1,
            columns: 4,
            lines: 2,
            width: 40,
            height: 40,
            rgba: Box::new([]),
        });
        let piece = ImagePiece {
            line: 0,
            column: 0,
            columns: 3,
            image,
            image_column: 1,
            image_line: 1,
        };
        let region = AtlasRegion {
            x: 10,
            y: 20,
            width: 40,
            height: 40,
        };
        let uv = image_uv(region, 100, &piece);
        let expected = [0.2, 0.4, 0.3, 0.2];
        assert!(
            uv.iter().zip(expected).all(|(a, b)| (a - b).abs() < 1e-6),
            "{uv:?}"
        );
    }

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn scissors_cover_the_area_inside_the_target() {
        let size = (800, 600);
        assert_eq!(
            scissor(rect(10.0, 20.0, 300.0, 200.0), size),
            Some([10, 20, 300, 200])
        );
        // Fractional edges round outwards.
        assert_eq!(
            scissor(rect(10.5, 20.5, 100.0, 100.0), size),
            Some([10, 20, 101, 101])
        );
        // Cut to the target.
        assert_eq!(
            scissor(rect(-5.0, 500.0, 900.0, 200.0), size),
            Some([0, 500, 800, 100])
        );
        // Nothing visible.
        assert_eq!(scissor(rect(850.0, 0.0, 100.0, 100.0), size), None);
        assert_eq!(scissor(rect(0.0, 0.0, 0.0, 100.0), size), None);
    }

    #[test]
    fn next_capacity_grows_shrinks_and_clamps() {
        const L: usize = 1 << 20;
        assert_eq!(next_capacity(4096, 5000, L), 8192);
        assert_eq!(next_capacity(4096, 5000, 6000), 6000);
        assert_eq!(next_capacity(4096, 4096, L), 4096);
        assert_eq!(next_capacity(8192, 2000, L), 4096);
        assert_eq!(next_capacity(8192, 3000, L), 8192);
        assert_eq!(max_instances(256 << 20), 4_194_304);
    }

    #[test]
    fn truncate_layout_keeps_ranges_consistent() {
        let batch = |range: Range<u32>| PaneBatch {
            fill: false,
            range,
            area: rect(0.0, 0.0, 1.0, 1.0),
        };
        let layout = || {
            (
                vec![Instance::solid(0.0, 0.0, 1.0, 1.0, Rgb { r: 0, g: 0, b: 0 }); 34],
                vec![batch(0..10), batch(10..20), batch(20..30)],
            )
        };
        let ranges =
            |batches: &[PaneBatch]| batches.iter().map(|b| b.range.clone()).collect::<Vec<_>>();

        let (mut instances, mut batches) = layout();
        let (mut ui_start, mut cutout_start) = (30, 34);
        assert!(truncate_layout(
            &mut instances,
            15,
            &mut batches,
            &mut ui_start,
            &mut cutout_start
        ));
        assert_eq!(ranges(&batches), [0..10, 10..15]);
        assert_eq!((instances.len(), ui_start, cutout_start), (15, 15, 15));

        let (mut instances, mut batches) = layout();
        let (mut ui_start, mut cutout_start) = (30, 34);
        assert!(truncate_layout(
            &mut instances,
            20,
            &mut batches,
            &mut ui_start,
            &mut cutout_start
        ));
        assert_eq!(ranges(&batches), [0..10, 10..20]);
        assert_eq!((instances.len(), ui_start, cutout_start), (20, 20, 20));

        let (mut instances, mut batches) = layout();
        let (mut ui_start, mut cutout_start) = (30, 34);
        assert!(!truncate_layout(
            &mut instances,
            34,
            &mut batches,
            &mut ui_start,
            &mut cutout_start
        ));
        assert_eq!(ranges(&batches), [0..10, 10..20, 20..30]);
    }

    #[test]
    fn atlas_clearing_is_rationed_while_overflowing() {
        for frames in [0, 32, 64] {
            assert!(may_clear_atlas(frames), "{frames}");
        }
        for frames in [1, 31, 33] {
            assert!(!may_clear_atlas(frames), "{frames}");
        }
    }

    #[cfg(feature = "capture")]
    #[test]
    fn captured_rows_lose_their_padding() {
        // Two rows of one pixel, padded to 8 bytes.
        let data = [1, 2, 3, 4, 0, 0, 0, 0, 5, 6, 7, 8, 0, 0, 0, 0];
        assert_eq!(unpad_rows(&data, 1, 2, 8, false), [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(unpad_rows(&data, 1, 2, 8, true), [3, 2, 1, 4, 7, 6, 5, 8]);
    }
}
