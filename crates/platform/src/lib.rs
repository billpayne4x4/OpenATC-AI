//! OS-dependent code: data directories, service installation, GPU detection.
//!
//! Everything above this crate stays unaware of the OS. Platform behavior is
//! selected at compile time; each module must provide the same surface.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(target_os = "macos")]
pub use macos::*;
#[cfg(target_os = "windows")]
pub use windows::*;

use std::path::PathBuf;

/// GPU inference backend actually usable on this machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GpuBackend {
    /// NVIDIA CUDA (Linux and Windows).
    Cuda,
    /// Apple Metal (macOS only).
    Metal,
    /// Portable CPU fallback, always available.
    Cpu,
}

/// Directory holding downloaded model files, e.g.
/// `~/.local/share/openatc-ai/models`. Override with `OPENATC_AI_MODELS`.
#[must_use]
pub fn models_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("OPENATC_AI_MODELS") {
        return PathBuf::from(dir);
    }
    data_dir().join("openatc-ai").join("models")
}

/// Sibling directory for server state (controller roster, logs).
#[must_use]
pub fn state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("OPENATC_AI_MODELS") {
        return PathBuf::from(dir);
    }
    data_dir().join("openatc-ai")
}
