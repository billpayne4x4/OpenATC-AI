//! Windows: %LOCALAPPDATA% directories, logon task, NVIDIA detection.
//!
//! Autostart uses a per-user Scheduled Task (no admin prompt). The task XML
//! below is imported once with `schtasks /create /tn OpenATCAI /xml file`.

use super::GpuBackend;
use std::path::PathBuf;

/// `%LOCALAPPDATA%`, falling back to the profile directory.
#[must_use]
pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("LOCALAPPDATA") {
        return PathBuf::from(dir);
    }
    std::env::var("USERPROFILE")
        .map(|profile| PathBuf::from(profile).join("AppData").join("Local"))
        .unwrap_or_default()
}

/// Render a Scheduled Task definition that starts the AI server at logon.
#[must_use]
pub fn service_unit(server_binary: &std::path::Path, port: u16, _use_cuda: bool) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n\
         <Task version=\"1.4\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n\
         \t<Triggers><LogonTrigger><Enabled>true</Enabled></LogonTrigger></Triggers>\n\
         \t<Actions><Exec>\n\
         \t\t<Command>{binary}</Command>\n\t\t<Arguments>--port {port}</Arguments>\n\
         \t</Exec></Actions>\n\
         \t<Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy></Settings>\n\
         </Task>\n",
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
mod tests {
    use super::*;

    #[test]
    fn task_mentions_binary_and_port() {
        let task = service_unit(std::path::Path::new("C:\\openatc-ai.exe"), 8099, false);
        assert!(task.contains("C:\\openatc-ai.exe"));
        assert!(task.contains("8099"));
        assert!(task.contains("LogonTrigger"));
    }
}
