//! wgpu renderer for nuntio: glyph atlas and instanced quad pipeline.

mod atlas;
mod box_drawing;
mod font;
mod frame;
mod gpu;
mod renderer;

pub use font::CellMetrics;
pub use frame::{Frame, PaneView, UiRect, UiText};
pub use gpu::{FrameStatus, GpuError};
pub use renderer::Renderer;
