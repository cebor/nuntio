use etagere::{AtlasAllocator, size2};

/// Glyph atlas side length that every wgpu backend supports.
pub const MIN_ATLAS_SIZE: u32 = 2048;

/// Gap between glyphs so sampling never bleeds into neighbours.
const PADDING: i32 = 1;

/// A region in an atlas texture, in texels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtlasRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// One texture plus a rectangle packer. Full atlases are cleared as a whole;
/// the glyph cache re-rasterizes on demand.
pub struct Atlas {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    allocator: AtlasAllocator,
    bytes_per_pixel: u32,
    size: u32,
}

impl Atlas {
    /// A square atlas `size` texels wide.
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, label: &str, size: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bytes_per_pixel = format
            .block_copy_size(None)
            .expect("uncompressed atlas format");
        Self {
            texture,
            view,
            allocator: AtlasAllocator::new(size2(size as i32, size as i32)),
            bytes_per_pixel,
            size,
        }
    }

    /// Side length in texels, to normalize texture coordinates.
    pub fn size(&self) -> u32 {
        self.size
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// Pack and upload an image. `None` means the atlas is full.
    pub fn insert(
        &mut self,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Option<AtlasRegion> {
        debug_assert_eq!(data.len(), (width * height * self.bytes_per_pixel) as usize);
        let alloc = self
            .allocator
            .allocate(size2(width as i32 + PADDING, height as i32 + PADDING))?;
        let region = AtlasRegion {
            x: alloc.rectangle.min.x as u32,
            y: alloc.rectangle.min.y as u32,
            width,
            height,
        };
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: region.x,
                    y: region.y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * self.bytes_per_pixel),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        Some(region)
    }

    pub fn clear(&mut self) {
        self.allocator.clear();
    }
}
