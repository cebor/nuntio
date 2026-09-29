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
    #[cfg(feature = "capture")]
    #[error("failed to capture the frame: {0}")]
    Capture(String),
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

/// How the GPU context is set up.
#[derive(Debug, Clone, Copy)]
pub struct GpuOptions {
    /// Keep the alpha channel if the platform supports it (the window must
    /// be created transparent too).
    pub transparent: bool,
    /// Prefer a CPU adapter; without one the GPU is used anyway.
    pub software: bool,
}

pub struct GpuContext {
    /// Kept to create the surface anew (see `restore_surface`).
    instance: wgpu::Instance,
    /// `None` once released for a successor on the same window.
    surface: Option<wgpu::Surface<'static>>,
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    /// The surface blends with what is behind the window.
    transparent: bool,
    /// The last frame was suboptimal; reconfigure once it has been presented.
    reconfigure: bool,
    /// The adapter renders on the CPU.
    software: bool,
    /// The visual the surface was created from, if it wasn't the HWND.
    #[cfg(windows)]
    layer: Option<crate::dcomp::Layer>,
}

impl GpuContext {
    /// Create a context for `window`. `width`/`height` are in physical pixels.
    /// With `transparent`, the surface keeps the alpha channel if the
    /// platform supports it (the window must be created transparent too).
    /// With `software`, a CPU adapter is preferred; without one, the GPU is
    /// used anyway (see `software()`).
    pub fn new<W>(window: W, width: u32, height: u32, options: GpuOptions) -> Result<Self, GpuError>
    where
        W: HasWindowHandle + HasDisplayHandle + Debug + Clone + Send + Sync + 'static,
    {
        let descriptor =
            || wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(window.clone()));
        if std::env::var_os("WGPU_BACKEND").is_some() {
            return Self::create(window.clone(), width, height, options, descriptor());
        }
        // Probing GL initializes EGL, which makes Mesa print errors on
        // systems without a DRI driver (WSLg) even when Vulkan works. Only
        // machines without any other backend get to see them.
        let mut without_gl = descriptor();
        without_gl.backends.remove(wgpu::Backends::GL);
        Self::create(window.clone(), width, height, options, without_gl).or_else(|err| {
            tracing::debug!("no GPU without GL, trying it: {err}");
            let mut gl = descriptor();
            gl.backends = wgpu::Backends::GL;
            Self::create(window.clone(), width, height, options, gl).map_err(|_| err)
        })
    }

    fn create<W>(
        window: W,
        width: u32,
        height: u32,
        options: GpuOptions,
        descriptor: wgpu::InstanceDescriptor,
    ) -> Result<Self, GpuError>
    where
        W: HasWindowHandle + HasDisplayHandle + Debug + Clone + Send + Sync + 'static,
    {
        let instance = wgpu::Instance::new(descriptor);
        // On Windows, DX12 presents through a DirectComposition visual of
        // our own. A swap chain made from the HWND looks like a game to
        // overlays such as NVIDIA's, which then announce themselves on every
        // start; and the layer lets a successor replace this context without
        // the window going blank in between (see `dcomp`).
        #[cfg(windows)]
        if let Some(layer) = composition_layer(&window, options.transparent) {
            // SAFETY: the visual is valid; the surface keeps its own reference.
            let context = unsafe { instance.create_surface_unsafe(layer.surface_target()) }
                .map_err(GpuError::from)
                .and_then(|surface| {
                    Self::with_surface(instance.clone(), surface, width, height, options)
                });
            match context {
                Ok(mut context) => {
                    context.layer = Some(layer);
                    context.configured();
                    return Ok(context);
                }
                Err(err) => tracing::warn!("no DirectComposition surface, using the HWND: {err}"),
            }
        }
        let surface = instance.create_surface(window)?;
        Self::with_surface(instance, surface, width, height, options)
    }

    /// Create the context on `surface`, with the first adapter that works.
    fn with_surface(
        instance: wgpu::Instance,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
        options: GpuOptions,
    ) -> Result<Self, GpuError> {
        let GpuOptions {
            transparent,
            software,
        } = options;
        let mut failure = None;
        // A system with several GPUs may offer an adapter that can't present
        // to the window after all; fall back to the next one.
        for adapter in adapters(&instance, &surface, transparent, software)? {
            let info = adapter.get_info();
            match setup(&adapter, &surface, width, height, transparent) {
                Ok((device, queue, config, transparent)) => {
                    tracing::info!(adapter = ?info, "selected GPU adapter");
                    let context = Self {
                        instance,
                        surface: Some(surface),
                        device,
                        queue,
                        config,
                        transparent,
                        reconfigure: false,
                        software: info.device_type == wgpu::DeviceType::Cpu,
                        #[cfg(windows)]
                        layer: None,
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

    fn configure(&mut self) {
        let Some(surface) = &self.surface else {
            return;
        };
        surface.configure(&self.device, &self.config);
        self.configured();
    }

    /// The surface was (re)configured.
    fn configured(&mut self) {
        #[cfg(windows)]
        if let Some(layer) = &mut self.layer {
            layer.configured();
        }
        #[cfg(target_os = "macos")]
        self.match_srgb();
    }

    /// A frame was presented.
    pub(crate) fn presented(&mut self) {
        #[cfg(windows)]
        if let Some(layer) = &mut self.layer {
            layer.presented();
        }
    }

    /// Give up the window's surface. A window takes only one swapchain at a
    /// time (DX12, Vulkan), so a successor can only be created after this.
    /// Frames report `Lost` from now on, until `restore_surface`.
    pub fn release_surface(&mut self) {
        #[cfg(target_os = "macos")]
        let layer = self.metal_layer();
        self.surface = None;
        // The last frame's back buffer is kept by the device until its
        // submission is cleaned up, and with it the swapchain (on DX12 the
        // successor's configure fails with "Access is denied").
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        // Every surface gets a new sublayer of the view's layer, which
        // neither wgpu nor raw-window-metal removes: it would stay under the
        // successor's with its last frame, showing through where the window
        // is transparent. winit's view has no `CAMetalLayer` of its own, so
        // this layer is always such a sublayer.
        #[cfg(target_os = "macos")]
        if let Some(layer) = layer {
            layer.removeFromSuperlayer();
        }
    }

    /// A software adapter could present to the window: `adapters` offers
    /// one to a successor. Without the surface, there's no telling.
    pub fn offers_software(&self) -> bool {
        let Some(surface) = &self.surface else {
            return true;
        };
        pollster::block_on(self.instance.enumerate_adapters(wgpu::Backends::all()))
            .iter()
            .any(|adapter| {
                adapter.get_info().device_type == wgpu::DeviceType::Cpu
                    && adapter.is_surface_supported(surface)
            })
    }

    /// Create the surface for `window` again after `release_surface`, with
    /// the same configuration, for when no successor could be created.
    pub fn restore_surface<W>(&mut self, window: W) -> Result<(), GpuError>
    where
        W: HasWindowHandle + HasDisplayHandle + Debug + Send + Sync + 'static,
    {
        #[cfg(windows)]
        let surface = match &self.layer {
            // SAFETY: the visual is valid; the surface keeps its own reference.
            Some(layer) => unsafe { self.instance.create_surface_unsafe(layer.surface_target()) }?,
            None => self.instance.create_surface(window)?,
        };
        #[cfg(not(windows))]
        let surface = self.instance.create_surface(window)?;
        configure_checked(&surface, &self.device, &self.config)?;
        self.surface = Some(surface);
        self.reconfigure = false;
        self.configured();
        Ok(())
    }

    /// Have macOS convert the frames from sRGB, which the theme colors are,
    /// to the display's colors. wgpu leaves the Metal layer without a color
    /// space, and then the values go to the display unconverted: sRGB red
    /// shows as the more saturated Display P3 red on Mac displays.
    #[cfg(target_os = "macos")]
    fn match_srgb(&self) {
        use objc2_core_graphics::{CGColorSpace, kCGColorSpaceSRGB};

        let Some(layer) = self.metal_layer() else {
            return;
        };
        // SAFETY: a constant CoreGraphics provides.
        let srgb = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }));
        layer.setColorspace(srgb.as_deref());
    }

    /// The `CAMetalLayer` the surface draws into, if there is one.
    #[cfg(target_os = "macos")]
    fn metal_layer(
        &self,
    ) -> Option<impl std::ops::Deref<Target = objc2_quartz_core::CAMetalLayer> + use<>> {
        // SAFETY: the layer is only retained, and changed in ways wgpu
        // doesn't track; the surface stays owned by wgpu.
        let surface = unsafe { self.surface.as_ref()?.as_hal::<wgpu::hal::api::Metal>() }?;
        Some(surface.render_layer().lock().clone())
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

    /// The adapter in use renders on the CPU.
    pub fn software(&self) -> bool {
        self.software
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
        let Some(surface) = &self.surface else {
            return Err(FrameStatus::Lost);
        };
        match surface.get_current_texture() {
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
/// `WGPU_ADAPTER_NAME`, then on Windows DX12 ones (see below), then with
/// `software` the CPU ones (without it they come last), then the GPU wgpu
/// picks for the power preference (`WGPU_POWER_PREF`, low power by default),
/// then the rest.
fn adapters(
    instance: &wgpu::Instance,
    surface: &wgpu::Surface<'_>,
    transparent: bool,
    software: bool,
) -> Result<Vec<wgpu::Adapter>, GpuError> {
    let preferred = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference:
            wgpu::PowerPreference::from_env().unwrap_or(wgpu::PowerPreference::LowPower),
        compatible_surface: Some(surface),
        force_fallback_adapter: software,
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
    // wgpu lists Vulkan before DX12 on Windows. Presenting a window through
    // Vulkan there flashes the whole desktop white now and then (seen on AMD
    // GPUs); DX12's flip-model swapchain is composed by DWM like any other
    // window. Its HWND swapchain has no alpha, though, and transparent
    // windows get no DirectComposition layer (see `composition_layer`), so
    // they keep Vulkan. `WGPU_BACKEND` limits the backends and decides by itself.
    let prefer_dx12 = cfg!(windows) && !transparent && std::env::var_os("WGPU_BACKEND").is_none();
    let rank = |adapter: &wgpu::Adapter| {
        let info = adapter.get_info();
        let named = wanted
            .as_deref()
            .is_some_and(|name| info.name.to_lowercase().contains(name));
        let other_backend = prefer_dx12 && info.backend != wgpu::Backend::Dx12;
        // The same GPU under another backend counts as preferred too.
        let preferred = preferred.as_ref().is_some_and(|p| {
            (p.vendor, p.device, &p.name) == (info.vendor, info.device, &info.name)
        });
        let wrong_kind = (info.device_type == wgpu::DeviceType::Cpu) != software;
        (!named, other_backend, wrong_kind, !preferred)
    };
    adapters.sort_by_key(rank);
    if let Some(name) = &wanted
        && adapters.first().is_none_or(|adapter| rank(adapter).0)
    {
        tracing::warn!("no GPU adapter matches WGPU_ADAPTER_NAME={name:?}");
    }
    Ok(adapters)
}

/// A DirectComposition layer for `window`, if DX12 is to present through
/// one: not for transparent windows, which keep Vulkan (see `adapters`), and
/// not when `WGPU_BACKEND` chooses the backends.
#[cfg(windows)]
fn composition_layer(
    window: &impl HasWindowHandle,
    transparent: bool,
) -> Option<crate::dcomp::Layer> {
    use wgpu::rwh::RawWindowHandle;

    if transparent || std::env::var_os("WGPU_BACKEND").is_some() {
        return None;
    }
    let RawWindowHandle::Win32(handle) = window.window_handle().ok()?.as_raw() else {
        return None;
    };
    crate::dcomp::Layer::new(handle.hwnd.get())
        .inspect_err(|err| tracing::warn!("no DirectComposition layer: {err}"))
        .ok()
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

    configure_checked(surface, &device, &config)?;
    Ok((device, queue, config, transparent))
}

/// Configure `surface`, returning the error instead of panicking on it, as an
/// uncaptured configure error would.
fn configure_checked(
    surface: &wgpu::Surface<'_>,
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
) -> Result<(), GpuError> {
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    surface.configure(device, config);
    match pollster::block_on(scope.pop()) {
        Some(err) => Err(GpuError::Configure(err.to_string())),
        None => Ok(()),
    }
}
