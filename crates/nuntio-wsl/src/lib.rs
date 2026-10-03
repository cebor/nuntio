//! `nuntio-wsl`: the Linux helper of WSL panes.
//!
//! nuntio on Windows starts `wsl.exe` with plain pipes and runs this helper in the distro.
//! The helper owns a real Linux PTY, so the shell's output reaches nuntio byte for byte
//! instead of through ConPTY. [`proto`] is the wire format on the helper's stdin and the
//! ready marker on its stdout.

pub mod proto;

#[cfg(target_os = "linux")]
pub mod relay;
