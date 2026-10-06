//! Linux: XDG directories, systemd user units, NVIDIA detection.

use super::{GpuBackend, models_dir};
use std::path::PathBuf;

/// `~/.local/share`, honoring `XDG_DATA_HOME`.
#[must_use]
pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_DATA_HOME") {
        return PathBuf::from(dir);
    }
    home().join(".local").join("share")
}

fn home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_default()
}

/// Render a systemd user unit that runs the AI server after login.
/// Install to `~/.config/systemd/user/openatc-ai.service`, then
/// `systemctl --user enable --now openatc-ai`.
#[must_use]
pub fn service_unit(server_binary: &std::path::Path, port: u16, use_cuda: bool) -> String {
    let nvidia_libs = if use_cuda {
        "\nEnvironment=LD_LIBRARY_PATH=%h/.local/share/openatc-ai/lib"
    } else {
        ""
    };
    format!(
        "[Unit]\nDescription=OpenATC AI server (LLM + STT + TTS)\nAfter=network.target\n\n\
         [Service]\nType=simple\nEnvironment=OPENATC_AI_MODELS={models}\n\
         Environment=ONNX_PROVIDER=CUDAExecutionProvider{nvidia_libs}\n\
         ExecStart={binary} --port {port}\nRestart=always\nRestartSec=5\n\n\
         [Install]\nWantedBy=default.target\n",
        models = models_dir().display(),
        binary = server_binary.display(),
        port = port,
    )
}

/// CUDA is usable when the NVIDIA management interface answers.
#[must_use]
pub fn detect_gpu() -> GpuBackend {
    let probing = std::process::Command::new("nvidia-smi")
        .arg("--query-gpu=name")
        .arg("--format=csv,noheader")
        .output();
    match probing {
        Ok(output) if output.status.success() && !output.stdout.is_empty() => GpuBackend::Cuda,
        _ => GpuBackend::Cpu,
    }
}

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;

    #[test]
    fn unit_mentions_binary_and_port() {
        let unit = service_unit(
            std::path::Path::new("/usr/local/bin/openatc-ai"),
            8099,
            false,
        );
        assert!(unit.contains("/usr/local/bin/openatc-ai"));
        assert!(unit.contains("8099"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn models_dir_honors_override() {
        // `std::env::set_var` is unsafe in edition 2024 (process-wide races).
        unsafe {
            std::env::set_var("OPENATC_AI_MODELS", "/tmp/ai-models-test");
        }
        assert_eq!(models_dir(), PathBuf::from("/tmp/ai-models-test"));
        unsafe {
            std::env::remove_var("OPENATC_AI_MODELS");
        }
    }
}
