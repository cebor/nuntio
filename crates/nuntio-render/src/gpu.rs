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
    #[error("failed to configure the surface: {0}")]
    Configure(String),
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
        let mut failure = None;
        // A system with several GPUs may offer an adapter that can't present
        // to the window after all; fall back to the next one.
        for adapter in adapters(&instance, &surface)? {
            let info = adapter.get_info();
            match setup(&adapter, &surface, width, height, transparent) {
                Ok((device, queue, config, transparent)) => {
                    tracing::info!(adapter = ?info, "selected GPU adapter");
                    let context = Self {
                        surface,
                        device,
                        queue,
                        config,
                        transparent,
                        reconfigure: false,
                    };
                    #[cfg(target_os = "macos")]
                    context.match_srgb();
                    return Ok(context);
                }
                Err(err) => {
                    tracing::warn!(adapter = ?info, "GPU adapter unusable: {err}");
                    failure = Some(err);
                }
            }
        }
        Err(failure.unwrap_or(GpuError::UnsupportedSurface))
    }

    fn configure(&self) {
        self.surface.configure(&self.device, &self.config);
        #[cfg(target_os = "macos")]
        self.match_srgb();
    }

    /// Have macOS convert the frames from sRGB, which the theme colors are,
    /// to the display's colors. wgpu leaves the Metal layer without a color
    /// space, and then the values go to the display unconverted: sRGB red
    /// shows as the more saturated Display P3 red on Mac displays.
    #[cfg(target_os = "macos")]
    fn match_srgb(&self) {
        use objc2_core_graphics::{CGColorSpace, kCGColorSpaceSRGB};

        // SAFETY: the layer is only read and given a color space; the
        // surface stays owned by wgpu.
        let Some(surface) = (unsafe { self.surface.as_hal::<wgpu::hal::api::Metal>() }) else {
            return;
        };
        // SAFETY: a constant CoreGraphics provides.
        let srgb = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }));
        surface.render_layer().lock().setColorspace(srgb.as_deref());
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.configure();
    }

    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn transparent(&self) -> bool {
        self.transparent
    }

    /// The compositor expects color premultiplied by alpha.
    pub fn premultiplied(&self) -> bool {
        self.config.alpha_mode == wgpu::CompositeAlphaMode::PreMultiplied
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// Get the next surface texture, or the reason to skip this frame.
    pub(crate) fn acquire(&mut self) -> Result<wgpu::SurfaceTexture, FrameStatus> {
        if std::mem::take(&mut self.reconfigure) {
            self.configure();
        }
        match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => Ok(frame),
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                // Configuring while the frame is alive panics; do it next time.
                self.reconfigure = true;
                Ok(frame)
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.configure();
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

/// The adapters that claim to support `surface`, best first: those named by
/// `WGPU_ADAPTER_NAME`, then the one wgpu picks for the power preference
/// (`WGPU_POWER_PREF`, low power by default), then the rest.
fn adapters(
    instance: &wgpu::Instance,
    surface: &wgpu::Surface<'_>,
) -> Result<Vec<wgpu::Adapter>, GpuError> {
    let preferred = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference:
            wgpu::PowerPreference::from_env().unwrap_or(wgpu::PowerPreference::LowPower),
        compatible_surface: Some(surface),
        ..Default::default()
    }));
    let mut adapters: Vec<_> =
        pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()))
            .into_iter()
            .filter(|adapter| adapter.is_surface_supported(surface))
            .collect();
    for adapter in &adapters {
        tracing::debug!(adapter = ?adapter.get_info(), "GPU adapter available");
    }
    let preferred = match preferred {
        Ok(adapter) => Some(adapter.get_info()),
        // Nothing else to try either.
        Err(err) if adapters.is_empty() => return Err(err.into()),
        Err(_) => None,
    };
    let wanted = std::env::var("WGPU_ADAPTER_NAME")
        .ok()
        .map(|name| name.to_lowercase());
    let rank = |adapter: &wgpu::Adapter| {
        let info = adapter.get_info();
        if wanted
            .as_deref()
            .is_some_and(|name| info.name.to_lowercase().contains(name))
        {
            0
        } else if preferred.as_ref() == Some(&info) {
            1
        } else {
            2
        }
    };
    adapters.sort_by_key(rank);
    if let Some(name) = &wanted
        && adapters.first().is_none_or(|adapter| rank(adapter) != 0)
    {
        tracing::warn!("no GPU adapter matches WGPU_ADAPTER_NAME={name:?}");
    }
    Ok(adapters)
}

/// Create a device on `adapter` and configure `surface` for it. Errors
/// instead of panicking if the surface turns out not to work with it.
fn setup(
    adapter: &wgpu::Adapter,
    surface: &wgpu::Surface<'_>,
    width: u32,
    height: u32,
    transparent: bool,
) -> Result<(wgpu::Device, wgpu::Queue, wgpu::SurfaceConfiguration, bool), GpuError> {
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("nuntio"),
        required_limits:
            wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
        ..Default::default()
    }))?;

    let mut config = surface
        .get_default_config(adapter, width.max(1), height.max(1))
        .ok_or(GpuError::UnsupportedSurface)?;
    // Colors in the config/themes are sRGB values; blending in a non-sRGB
    // target keeps them exact, like other terminals do.
    // Only the plain 8-bit variant of the preferred format: the first
    // non-sRGB format offered may be a float or 10-bit HDR format,
    // which changes how colors come out.
    let caps = surface.get_capabilities(adapter);
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

    // Uncaptured, a configure error panics.
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    surface.configure(&device, &config);
    if let Some(err) = pollster::block_on(scope.pop()) {
        return Err(GpuError::Configure(err.to_string()));
    }
    Ok((device, queue, config, transparent))
}
