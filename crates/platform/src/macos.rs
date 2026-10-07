//! macOS: Application Support directories, launchd agents, Metal.
//!
//! Apple Silicon and Intel share this module; unified memory means the
//! "VRAM budget" is shared system RAM, so the server sizes contexts accordingly.

use super::{GpuBackend, models_dir};
use std::path::PathBuf;

/// `~/Library/Application Support`.
#[must_use]
pub fn data_dir() -> PathBuf {
    home().join("Library").join("Application Support")
}

fn home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_default()
}

/// Render a launchd agent plist for the AI server.
/// Install to `~/Library/LaunchAgents/ai.openatc.server.plist`, then
/// `launchctl load ~/Library/LaunchAgents/ai.openatc.server.plist`.
#[must_use]
pub fn service_unit(server_binary: &std::path::Path, port: u16, _use_cuda: bool) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n\
         \t<key>Label</key><string>ai.openatc.server</string>\n\
         \t<key>ProgramArguments</key>\n\t<array>\n\
         \t\t<string>{binary}</string>\n\t\t<string>--port</string>\n\t\t<string>{port}</string>\n\
         \t</array>\n\
         \t<key>EnvironmentVariables</key>\n\t<dict>\n\
         \t\t<key>OPENATC_AI_MODELS</key><string>{models}</string>\n\
         \t</dict>\n\
         \t<key>RunAtLoad</key><true/>\n\
         \t<key>KeepAlive</key><true/>\n\
         </dict>\n</plist>\n",
        binary = server_binary.display(),
        port = port,
        models = models_dir().display(),
    )
}

/// macOS always has Metal; whether the model backends use it depends on
/// compile-time features, so report Metal and let the loader decide.
#[must_use]
pub fn detect_gpu() -> GpuBackend {
    GpuBackend::Metal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_mentions_binary_and_port() {
        let plist = service_unit(
            std::path::Path::new("/usr/local/bin/openatc-ai"),
            8099,
            false,
        );
        assert!(plist.contains("/usr/local/bin/openatc-ai"));
        assert!(plist.contains("8099"));
        assert!(plist.contains("ai.openatc.server"));
    }
}
