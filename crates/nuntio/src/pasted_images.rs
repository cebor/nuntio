//! Clipboard images saved as PNG files, so a paste can hand a program
//! their path (AI CLIs like Claude Code attach pasted image paths).

use std::fs::{DirBuilder, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use image::ImageEncoder;
use image::codecs::png::PngEncoder;

pub struct PastedImages {
    /// Created on the first save; removed by `remove_all`.
    dir: PathBuf,
    created: bool,
    count: u32,
}

impl PastedImages {
    pub fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self {
            // Pid and time make the name unique to this nuntio.
            dir: std::env::temp_dir().join(format!("nuntio-paste-{}-{nanos}", std::process::id())),
            created: false,
            count: 0,
        }
    }

    /// Save RGBA8 pixels as the next PNG; returns its path.
    pub fn save(&mut self, image: &arboard::ImageData) -> anyhow::Result<PathBuf> {
        let size = u32::try_from(image.width)
            .ok()
            .zip(u32::try_from(image.height).ok())
            .filter(|_| {
                image
                    .width
                    .checked_mul(image.height)
                    .and_then(|pixels| pixels.checked_mul(4))
                    == Some(image.bytes.len())
            });
        let Some((width, height)) = size else {
            anyhow::bail!(
                "clipboard image has {} bytes for {}x{} pixels",
                image.bytes.len(),
                image.width,
                image.height
            );
        };
        if !self.created {
            create_private_dir(&self.dir)
                .with_context(|| format!("failed to create {}", self.dir.display()))?;
            self.created = true;
        }
        self.count += 1;
        let path = self.dir.join(format!("image-{}.png", self.count));
        let file = File::create_new(&path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        let mut writer = BufWriter::new(file);
        PngEncoder::new(&mut writer)
            .write_image(&image.bytes, width, height, image::ExtendedColorType::Rgba8)
            .map_err(anyhow::Error::from)
            .and_then(|()| writer.flush().map_err(anyhow::Error::from))
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(path)
    }

    /// Delete the directory with every saved image (at exit).
    pub fn remove_all(&mut self) {
        if !self.created {
            return;
        }
        if let Err(err) = std::fs::remove_dir_all(&self.dir) {
            tracing::debug!("failed to remove {}: {err}", self.dir.display());
        }
        self.created = false;
    }
}

/// Create only the leaf, readable by this user alone where that's
/// possible: screenshots shouldn't be visible to others in a shared `/tmp`.
#[cfg(unix)]
fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    DirBuilder::new().mode(0o700).create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    DirBuilder::new().create(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(bytes: Vec<u8>) -> arboard::ImageData<'static> {
        arboard::ImageData {
            width: 2,
            height: 1,
            bytes: bytes.into(),
        }
    }

    #[test]
    fn saved_images_are_pngs_and_removed_at_exit() -> anyhow::Result<()> {
        let pixels = vec![255, 0, 0, 255, 0, 0, 255, 128];
        let mut images = PastedImages::new();
        let first = images.save(&image(pixels.clone()))?;
        let second = images.save(&image(pixels.clone()))?;
        assert_ne!(first, second);
        for path in [&first, &second] {
            assert_eq!(path.extension().and_then(|e| e.to_str()), Some("png"));
        }
        let decoded = image::open(&first)?.to_rgba8();
        assert_eq!(decoded.dimensions(), (2, 1));
        assert_eq!(decoded.into_raw(), pixels);

        assert!(images.save(&image(vec![0; 4])).is_err());

        images.remove_all();
        assert!(!first.parent().unwrap().exists());
        Ok(())
    }
}
