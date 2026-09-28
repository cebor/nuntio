//! wgpu renderer for nuntio: glyph atlas and instanced quad pipeline.

mod atlas;
mod box_drawing;
mod decoration;
mod font;
mod frame;
mod gpu;
mod renderer;

pub use font::{CellMetrics, preload_fonts};
pub use frame::{Frame, PaneView, Rect, UiRect, UiText};
pub use gpu::{FrameStatus, GpuError, GpuOptions};
#[cfg(feature = "capture")]
pub use renderer::Capture;
pub use renderer::Renderer;
