//! What nuntio's terminal apps (`nuntio-config-tui`, `nuntio-apps`) share:
//! terminal setup and restore, key normalization and the clipboard, a
//! single-line text input, popups and the colors, tones and drawing pieces
//! they draw with.

pub mod list;
pub mod popup;
pub mod style;
pub mod terminal;
pub mod text_input;
