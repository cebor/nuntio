use std::fmt::Debug;

use thiserror::Error;
use wgpu::rwh::{HasDisplayHandle, HasWindowHandle};

#[derive(Debug, Error)]
pub enum GpuError {
    #[error("failed to create surface: {0}")]
    Surface(#[from] wgpu::CreateSurfaceError),
    #[error("no suitable GPU adapter: {0}")]
    Adapter(#[from] wgpu::RequestAdapterError),
    #[error("failed to create device: {0}")]
    Device(#[from] wgpu::RequestDeviceError),
    #[error("surface is not supported by the adapter")]
    UnsupportedSurface,
}

/// Outcome of a frame, so the caller can decide whether to redraw again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameStatus {
    Presented,
    /// Frame skipped (timeout, reconfigured); try again right away.
    Skipped,
    /// Nothing can be drawn now (window occluded); wait for the next event
    /// instead of retrying, which would spin the event loop.
    Paused,
    /// The surface is gone and the context must be rebuilt.
    Lost,
}

pub struct GpuContext {
    surface: wgpu::Surface<'static>,
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    /// The surface blends with what is behind the window.
    transparent: bool,
    /// The last frame was suboptimal; reconfigure once it has been presented.
    reconfigure: bool,
}

impl GpuContext {
    /// Create a context for `window`. `width`/`height` are in physical pixels.
    /// With `transparent`, the surface keeps the alpha channel if the
    /// platform supports it (the window must be created transparent too).
    pub fn new<W>(window: W, width: u32, height: u32, transparent: bool) -> Result<Self, GpuError>
    where
        W: HasWindowHandle + HasDisplayHandle + Debug + Clone + Send + Sync + 'static,
    {
        let instance = wgpu::Instance::new(
            wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(window.clone())),
        );
        let surface = instance.create_surface(window)?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))?;
        tracing::info!(adapter = ?adapter.get_info(), "selected GPU adapter");

        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("nuntio"),
                required_limits:
                    wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
                ..Default::default()
            }))?;

        let mut config = surface
            .get_default_config(&adapter, width.max(1), height.max(1))
            .ok_or(GpuError::UnsupportedSurface)?;
        // Colors in the config/themes are sRGB values; blending in a non-sRGB
        // target keeps them exact, like other terminals do.
        // Only the plain 8-bit variant of the preferred format: the first
        // non-sRGB format offered may be a float or 10-bit HDR format,
        // which changes how colors come out.
        let caps = surface.get_capabilities(&adapter);
        let linear = config.format.remove_srgb_suffix();
        if caps.formats.contains(&linear) {
            config.format = linear;
        } else {
            tracing::debug!(format = ?config.format, "no non-sRGB variant, blending in sRGB");
        }
        let alpha_mode = [
            wgpu::CompositeAlphaMode::PreMultiplied,
            wgpu::CompositeAlphaMode::PostMultiplied,
        ]
        .into_iter()
        .find(|mode| caps.alpha_modes.contains(mode));
        let transparent = transparent && alpha_mode.is_some();
        if transparent {
            config.alpha_mode = alpha_mode.expect("checked above");
        } else if alpha_mode.is_none() {
            tracing::debug!(modes = ?caps.alpha_modes, "no transparent surface available");
        }
        config.present_mode = wgpu::PresentMode::AutoVsync;
        config.desired_maximum_frame_latency = 1;
        surface.configure(&device, &config);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            transparent,
            reconfigure: false,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn transparent(&self) -> bool {
        self.transparent
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// Get the next surface texture, or the reason to skip this frame.
    pub(crate) fn acquire(&mut self) -> Result<wgpu::SurfaceTexture, FrameStatus> {
        if std::mem::take(&mut self.reconfigure) {
            self.surface.configure(&self.device, &self.config);
        }
        match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => Ok(frame),
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                // Configuring while the frame is alive panics; do it next time.
                self.reconfigure = true;
                Ok(frame)
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                Err(FrameStatus::Skipped)
            }
            wgpu::CurrentSurfaceTexture::Lost => Err(FrameStatus::Lost),
            wgpu::CurrentSurfaceTexture::Timeout => Err(FrameStatus::Skipped),
            wgpu::CurrentSurfaceTexture::Occluded => Err(FrameStatus::Paused),
            other => {
                tracing::warn!(?other, "failed to acquire a frame");
                Err(FrameStatus::Paused)
            }
        }
    }
}
