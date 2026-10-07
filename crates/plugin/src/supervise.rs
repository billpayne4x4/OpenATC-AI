//! Find and launch the companion engine, capture its logs and retry failed starts.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Real-time liveness independent of simulator time, pause and loading screens.
/// Drop wakes and joins the worker before the plugin library can unload.
pub struct Heartbeat {
    stop: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Heartbeat {
    pub fn start(origin: &str) -> Self {
        let origin = origin.to_owned();
        let stop = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let signal = stop.clone();
        let worker = std::thread::spawn(move || {
            let (lock, wake) = &*signal;
            let body = serde_json::json!({"parentPid": std::process::id()});
            loop {
                if *lock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                {
                    break;
                }
                let _ = openatc_http::post_json(
                    &origin,
                    "/plugin/heartbeat",
                    &body,
                    Duration::from_millis(250),
                );
                let stopped = lock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let (stopped, _) = wake
                    .wait_timeout_while(stopped, Duration::from_secs(2), |stop| !*stop)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if *stopped {
                    break;
                }
            }
        });
        Self {
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        let (lock, wake) = &*self.stop;
        *lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        wake.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// How long to wait for a dead engine before the first respawn.
const GRACE: Duration = Duration::from_secs(5);

/// Supervision state for one engine binary.
pub struct Supervisor {
    /// Resolved engine binary path.
    pub binary: Option<PathBuf>,
    /// Engine disconnected since.
    pub disconnected_since: Option<Instant>,
    /// Last spawn attempt.
    pub last_attempt: Option<Instant>,
    /// Consecutive launch failures.
    pub failures: u32,
    /// Successful spawns that died anyway.
    pub fruitless: u32,
    /// Missing-binary notice already logged.
    pub missing_logged: bool,
}

impl Supervisor {
    /// Fresh supervisor with no resolved binary.
    #[must_use]
    pub fn new() -> Self {
        Self {
            binary: None,
            disconnected_since: None,
            last_attempt: None,
            failures: 0,
            fruitless: 0,
            missing_logged: false,
        }
    }

    /// Reset all backoff after a clean disconnect ends.
    pub fn mark_connected(&mut self) {
        self.disconnected_since = None;
        self.failures = 0;
        self.fruitless = 0;
    }

    /// Maybe spawn the engine. Returns a fault line for the UI when the
    /// engine repeatedly exits.
    pub fn poll(&mut self, connected: bool, log: &mut Vec<String>) -> String {
        if connected {
            self.mark_connected();
            return String::new();
        }
        let now = Instant::now();
        if self.disconnected_since.is_none() {
            self.disconnected_since = Some(now);
            return String::new();
        }
        if now - self.disconnected_since.unwrap_or(now) < GRACE {
            return String::new();
        }
        let backoff = Duration::from_secs(5 * (1 << self.failures.min(3)));
        if self.last_attempt.is_some_and(|at| now - at < backoff) {
            return String::new();
        }
        self.last_attempt = Some(now);
        let binary = self.binary.clone().unwrap_or_else(|| {
            let path = plugin_file_path()
                .and_then(|file| {
                    std::path::Path::new(&file)
                        .parent()
                        .and_then(|dir| dir.parent())
                        .map(|dir| dir.join("bin").join("open-atc-engine"))
                })
                .unwrap_or_default();
            self.binary = Some(path.clone());
            path
        });
        if !binary.is_file() {
            if !self.missing_logged {
                log.push(
                    "OpenATC AI: engine binary missing, start open-atc-engine manually".to_owned(),
                );
                self.missing_logged = true;
            }
            return String::new();
        }
        self.missing_logged = false;
        if spawn_detached(&binary) {
            self.failures = 0;
            self.fruitless += 1;
            if self.fruitless >= 3 {
                return "Engine won't start - see engine.log".to_owned();
            }
        } else {
            self.failures += 1;
            log.push("OpenATC AI: engine launch failed".to_owned());
        }
        String::new()
    }
}

impl Default for Supervisor {
    fn default() -> Self {
        Self::new()
    }
}

/// Ask the host for this plugin binary path.
/// Empty when the host will not say (then supervision stays manual).
pub(super) fn plugin_file_path() -> Option<String> {
    unsafe {
        let mut name = [0 as std::ffi::c_char; 512];
        let mut path = [0 as std::ffi::c_char; 4096];
        let mut signature = [0 as std::ffi::c_char; 512];
        let mut description = [0 as std::ffi::c_char; 512];
        xplane_sys::XPLMGetPluginInfo(
            xplane_sys::XPLMGetMyID(),
            name.as_mut_ptr(),
            path.as_mut_ptr(),
            signature.as_mut_ptr(),
            description.as_mut_ptr(),
        );
        std::ffi::CStr::from_ptr(path.as_ptr())
            .to_str()
            .ok()
            .map(ToOwned::to_owned)
    }
}

/// Double-fork the engine detached with stdout/stderr appended to the log.
fn spawn_detached(binary: &Path) -> bool {
    let log_path = engine_log_path();
    // Record the simulator PID before forking; detached engines still belong
    // to this plugin session and must stop when its host disappears.
    let owner = std::ffi::CString::new(format!("--plugin-parent-pid={}", std::process::id()))
        .expect("numeric PID contains no nul");
    unsafe {
        let first = libc::fork();
        if first < 0 {
            return false;
        }
        if first == 0 {
            if libc::fork() != 0 {
                libc::_exit(0);
            }
            libc::setsid();
            let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY);
            if null >= 0 {
                libc::dup2(null, libc::STDIN_FILENO);
                if null > libc::STDERR_FILENO {
                    libc::close(null);
                }
            }
            if !log_path.is_empty() {
                let path = std::ffi::CString::new(log_path).unwrap_or_default();
                let file = libc::open(
                    path.as_ptr(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
                    0o644,
                );
                if file >= 0 {
                    libc::dup2(file, libc::STDOUT_FILENO);
                    libc::dup2(file, libc::STDERR_FILENO);
                    if file > libc::STDERR_FILENO {
                        libc::close(file);
                    }
                }
            }
            #[cfg(target_os = "linux")]
            libc::close_range(3, u32::MAX, 0);
            let program =
                std::ffi::CString::new(binary.to_string_lossy().into_owned()).unwrap_or_default();
            libc::execl(
                program.as_ptr(),
                program.as_ptr(),
                owner.as_ptr(),
                std::ptr::null::<libc::c_char>(),
            );
            libc::_exit(127);
        }
        let mut status = 0;
        while libc::waitpid(first, &raw mut status, 0) < 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
        {}
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0
    }
}

/// Companion-engine log path in the user configuration directory.
fn engine_log_path() -> String {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".config/openatc/engine.log")
            .to_string_lossy()
            .into_owned();
    }
    String::new()
}

#[cfg(test)]
mod heartbeat_tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn heartbeat_runs_without_flight_loop_and_joins_on_drop() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let heartbeat = Heartbeat::start(&origin);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut requests = 0;
        while requests < 2 && Instant::now() < deadline {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    socket
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut bytes = [0; 1024];
                    let count = socket.read(&mut bytes).unwrap();
                    assert!(
                        String::from_utf8_lossy(&bytes[..count])
                            .starts_with("POST /plugin/heartbeat ")
                    );
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                        )
                        .unwrap();
                    requests += 1;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("{error}"),
            }
        }
        assert_eq!(requests, 2, "no simulator calls are needed for heartbeats");
        let stopping = Instant::now();
        drop(heartbeat);
        assert!(stopping.elapsed() < Duration::from_secs(1));
    }
}
