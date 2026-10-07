//! Lifetime of a local engine started by the simulator plugin.
use std::time::{Duration, Instant};

const STARTUP_GRACE: Duration = Duration::from_secs(60);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(10);

fn expired(started: Instant, last_seen: Instant, now: Instant) -> bool {
    (last_seen > started || now.duration_since(started) >= STARTUP_GRACE)
        && now.duration_since(last_seen) >= HEARTBEAT_TIMEOUT
}

fn host_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        // The command name may contain spaces or parentheses.
        stat.rsplit_once(')').is_some_and(|(_, rest)| {
            !matches!(rest.split_whitespace().next(), Some("Z" | "X") | None)
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        true // Heartbeat timeout is the portable fallback.
    }
}

/// Keep liveness separate from dialogue locks, which can wait on AI work.
pub struct PluginSession {
    parent: Option<u32>,
    last_seen: std::sync::Mutex<Instant>,
    shutdown: tokio::sync::Notify,
}

impl PluginSession {
    pub fn new(parent: Option<u32>) -> Self {
        Self {
            parent,
            last_seen: std::sync::Mutex::new(Instant::now()),
            shutdown: tokio::sync::Notify::new(),
        }
    }

    fn owns(&self, body: &serde_json::Value) -> bool {
        self.parent.is_some()
            && body.get("parentPid").and_then(serde_json::Value::as_u64)
                == self.parent.map(u64::from)
    }
}

pub async fn heartbeat(
    axum::Extension(session): axum::Extension<std::sync::Arc<PluginSession>>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if !session.owns(&body) {
        return axum::http::StatusCode::FORBIDDEN.into_response();
    }
    *session
        .last_seen
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
    axum::Json(serde_json::json!({"accepted": true})).into_response()
}

pub async fn request_shutdown(
    axum::Extension(session): axum::Extension<std::sync::Arc<PluginSession>>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if !session.owns(&body) {
        return (
            axum::http::StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({"error": "Engine is not owned by this plugin"})),
        )
            .into_response();
    }
    // Give the response a chance to reach the plugin before closing sockets.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        session.shutdown.notify_one();
    });
    axum::Json(serde_json::json!({"accepted": true})).into_response()
}

pub async fn wait_for_plugin_exit(session: &PluginSession, parent: u32) {
    let started = Instant::now();
    loop {
        tokio::select! {
            () = session.shutdown.notified() => return,
            () = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
        if !host_alive(parent)
            || expired(
                started,
                *session
                    .last_seen
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                Instant::now(),
            )
        {
            return;
        }
    }
}

pub async fn serve(
    listener: tokio::net::TcpListener,
    app: axum::Router,
    session: std::sync::Arc<PluginSession>,
) {
    if let Some(parent) = session.parent {
        use std::future::IntoFuture;
        tokio::select! {
            result = axum::serve(listener, app).into_future() => result.unwrap(),
            () = wait_for_plugin_exit(&session, parent) => {
                println!("OpenATC engine: plugin gone; shutting down");
            }
        }
    } else {
        axum::serve(listener, app).await.unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_owning_plugin_can_control_an_engine() {
        let session = PluginSession::new(Some(42));
        assert!(session.owns(&serde_json::json!({"parentPid":42})));
        assert!(!session.owns(&serde_json::json!({"parentPid":43})));
        assert!(!session.owns(&serde_json::json!({})));
        assert!(!PluginSession::new(None).owns(&serde_json::json!({"parentPid":42})));
    }

    #[test]
    fn watchdog_allows_startup_but_expires_missing_heartbeat() {
        let start = Instant::now();
        assert!(!expired(start, start, start + Duration::from_secs(59)));
        assert!(expired(start, start, start + Duration::from_secs(60)));
        let first_seen = start + Duration::from_secs(1);
        assert!(!expired(start, first_seen, start + Duration::from_secs(10)));
        assert!(expired(start, first_seen, start + Duration::from_secs(11)));
        let now = start + Duration::from_secs(100);
        assert!(!expired(
            start,
            now.checked_sub(Duration::from_secs(9)).unwrap(),
            now
        ));
        assert!(expired(
            start,
            now.checked_sub(Duration::from_secs(10)).unwrap(),
            now
        ));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn watchdog_detects_disappearing_host() {
        assert!(host_alive(std::process::id()));
        assert!(!host_alive(u32::MAX));
    }
}
