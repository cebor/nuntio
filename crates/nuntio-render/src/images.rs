//! The atlas of inline images and where each uploaded image is in it.

use std::collections::{HashMap, HashSet};

use nuntio_term::ImagePiece;

use crate::atlas::{Atlas, AtlasRegion, MIN_ATLAS_SIZE};

/// Starting size of the image atlas (4 MiB); grows up to
/// `MAX_IMAGE_ATLAS_SIZE` (256 MiB) when inline images need it.
const INITIAL_IMAGE_ATLAS_SIZE: u32 = 1024;
const MAX_IMAGE_ATLAS_SIZE: u32 = 8192;

/// Inline images in an atlas texture, premultiplied RGBA. A region is only
/// valid for the texture it was uploaded to, so the cache forgets all of
/// them whenever the atlas is cleared, grown or replaced.
pub(crate) struct ImageCache {
    atlas: Atlas,
    /// Where each uploaded image is, by [`TermImage::uid`](nuntio_term::TermImage::uid).
    regions: HashMap<u64, AtlasRegion>,
    /// Scratch: the uids of the images in the frame being prepared.
    live: HashSet<u64>,
    /// Some image of the last frame didn't fit; for logging it once.
    overflow: bool,
}

impl ImageCache {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let atlas = Atlas::new(
            device,
            wgpu::TextureFormat::Rgba8Unorm,
            "image atlas",
            INITIAL_IMAGE_ATLAS_SIZE,
            device
                .limits()
                .max_texture_dimension_2d
                .clamp(MIN_ATLAS_SIZE, MAX_IMAGE_ATLAS_SIZE),
        );
        Self {
            atlas,
            regions: HashMap::new(),
            live: HashSet::new(),
            overflow: false,
        }
    }

    pub(crate) fn atlas(&self) -> &Atlas {
        &self.atlas
    }

    /// Where the image `uid` is in the atlas, if it has been uploaded.
    pub(crate) fn region(&self, uid: u64) -> Option<AtlasRegion> {
        self.regions.get(&uid).copied()
    }

    /// Upload the images of `pieces` that aren't in the atlas yet. `true` if
    /// the atlas texture was replaced, so that what binds it has to be
    /// rebuilt.
    pub(crate) fn upload<'a>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pieces: impl Iterator<Item = &'a ImagePiece> + Clone,
    ) -> bool {
        let mut replaced = false;
        if pieces.clone().next().is_none() {
            // Give the memory back that a large image needed.
            if self.atlas.size() > INITIAL_IMAGE_ATLAS_SIZE
                && self.atlas.reset(device, INITIAL_IMAGE_ATLAS_SIZE)
            {
                self.regions.clear();
                replaced = true;
            }
            self.overflow = false;
            return replaced;
        }
        self.live.clear();
        self.live
            .extend(pieces.clone().map(|piece| piece.image.uid));
        let mut cleared = false;
        'pass: loop {
            let mut skipped = false;
            for piece in pieces.clone() {
                let image = &piece.image;
                if self.regions.contains_key(&image.uid) {
                    continue;
                }
                if let Some(region) =
                    self.atlas
                        .insert(queue, image.width, image.height, &image.rgba)
                {
                    self.regions.insert(image.uid, region);
                    continue;
                }
                let max = self.atlas.max_size();
                if image.width + 1 > max || image.height + 1 > max {
                    skipped = true;
                } else if !cleared && self.regions.keys().any(|uid| !self.live.contains(uid)) {
                    // Images no longer on screen hold space: start over
                    // before spending more memory. Without any, clearing
                    // would re-upload the same images every frame.
                    self.atlas.clear();
                    self.regions.clear();
                    cleared = true;
                    continue 'pass;
                } else if self.atlas.grow(device) {
                    self.regions.clear();
                    replaced = true;
                    continue 'pass;
                } else {
                    skipped = true;
                }
            }
            if skipped && !self.overflow {
                tracing::warn!("inline images do not fit into the image atlas");
            }
            self.overflow = skipped;
            break;
        }
        replaced
    }
}
