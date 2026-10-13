//! What nuntio's terminal apps (`nuntio-config-tui`, `nuntio-apps`) share:
//! terminal setup and restore, key normalization and the clipboard, a
//! single-line text input and the colors and tones they draw with.

pub mod style;
pub mod terminal;
pub mod text_input;
