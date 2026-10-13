use thiserror::Error;

/// Why the renderer could not be created, restored or captured.
#[derive(Debug, Error)]
pub enum RenderError {
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
    #[error(transparent)]
    Fonts(#[from] crate::font::NoFontsError),
    #[cfg(feature = "capture")]
    #[error("failed to capture the frame: {0}")]
    Capture(String),
}
