//! `nuntio-wsl`: the Linux helper of WSL panes.
//!
//! nuntio on Windows starts `wsl.exe` with plain pipes and runs this helper in the distro.
//! The helper owns a real Linux PTY, so the shell's output reaches nuntio byte for byte
//! instead of through ConPTY. [`proto`] is the wire format: frames in on the helper's stdin,
//! PTY bytes out on its stdout, mixed with `nuntio-wsl;` OSC reports (ready, foreground).
//! `nuntio-wsl socket <path>` runs the other mode instead: it bridges a Unix socket to
//! its standard streams (`relay::socket_bridge`).

pub mod proto;

#[cfg(target_os = "linux")]
pub mod relay;
