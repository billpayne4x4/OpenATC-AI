//! Engine snapshots and a bounded HTTP command queue.
//! HTTPS requests are handled by the companion engine.

use openatc_core::state::{State, Telemetry};
use openatc_settings::Settings;
use std::collections::{HashMap, VecDeque};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

type Json = serde_json::Value;

/// One engine command awaiting send.
struct Command {
    path: String,
    body: Json,
}

/// Reply slot per path, latest wins.
pub struct EngineReply {
    /// HTTP 200 with parseable JSON.
    pub success: bool,
    /// Reply body.
    pub data: Json,
    /// Human error when not successful.
    pub error: String,
}

struct Inner {
    pending: VecDeque<Command>,
    replies: HashMap<String, EngineReply>,
    snapshot: State,
    settings: Settings,
    status: String,
    connected: bool,
    latest_telemetry: Option<Telemetry>,
    running: bool,
}

/// Polls `/telemetry` + `/state` every 200 ms; sends queued commands.
pub struct EngineClient {
    inner: Arc<(Mutex<Inner>, Condvar)>,
    endpoint: String,
    cancelled: Arc<AtomicBool>,
    polling_worker: Option<std::thread::JoinHandle<()>>,
    command_worker: Option<std::thread::JoinHandle<()>>,
}

impl EngineClient {
    /// Connect to the engine at the endpoint, spawning both workers.
    #[must_use]
    pub fn new(endpoint: &str) -> Self {
        let inner = Arc::new((
            Mutex::new(Inner {
                pending: VecDeque::new(),
                replies: HashMap::new(),
                snapshot: State::default(),
                settings: Settings::default(),
                status: "Connecting to engine".to_owned(),
                connected: false,
                latest_telemetry: None,
                running: true,
            }),
            Condvar::new(),
        ));
        let mut client = Self {
            inner: inner.clone(),
            endpoint: endpoint.to_owned(),
            cancelled: Arc::new(AtomicBool::new(false)),
            polling_worker: None,
            command_worker: None,
        };
        let polling = inner.clone();
        let polling_endpoint = endpoint.to_owned();
        let flag = client.cancelled.clone();
        client.polling_worker = Some(std::thread::spawn(move || {
            Self::poll_loop(&polling, &polling_endpoint, &flag);
        }));
        let commands = inner;
        let command_endpoint = endpoint.to_owned();
        let flag = client.cancelled.clone();
        client.command_worker = Some(std::thread::spawn(move || {
            Self::command_loop(&commands, &command_endpoint, &flag);
        }));
        client
    }

    fn poll_loop(inner: &Arc<(Mutex<Inner>, Condvar)>, endpoint: &str, cancelled: &AtomicBool) {
        loop {
            let telemetry = {
                let mut guard = inner
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                guard.latest_telemetry.take()
            };
            if let Some(telemetry) = telemetry {
                let body = serde_json::to_value(&telemetry).unwrap_or(Json::Null);
                let _ = openatc_http::post_json_cancelled(
                    endpoint,
                    "/telemetry",
                    &body,
                    std::time::Duration::from_secs(2),
                    cancelled,
                );
            }
            let response = openatc_http::get_json_cancelled(
                endpoint,
                "/state",
                std::time::Duration::from_secs(2),
                cancelled,
            );
            {
                let mut guard = inner
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if !guard.running {
                    break;
                }
                guard.connected = false;
                match response {
                    Ok(mut data) => {
                        let settings = data.as_object_mut().and_then(|map| map.remove("settings"));
                        match serde_json::from_value::<State>(data) {
                            Ok(snapshot) => {
                                let demo = snapshot.demo;
                                guard.snapshot = snapshot;
                                if let Some(settings) = settings
                                    && let Ok(settings) = serde_json::from_value(settings)
                                {
                                    guard.settings = settings;
                                }
                                guard.connected = true;
                                if demo {
                                    "Demo controller connected".clone_into(&mut guard.status);
                                } else {
                                    "X-Plane connected".clone_into(&mut guard.status);
                                }
                            }
                            Err(_) => {
                                "Incompatible engine response".clone_into(&mut guard.status);
                            }
                        }
                    }
                    Err(_) => "Engine disconnected".clone_into(&mut guard.status),
                }
            }
            for _ in 0..8 {
                if !inner
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .running
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            if !inner
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .running
            {
                break;
            }
        }
    }

    fn command_loop(inner: &Arc<(Mutex<Inner>, Condvar)>, endpoint: &str, cancelled: &AtomicBool) {
        loop {
            let command = {
                let (lock, wake) = &**inner;
                let mut guard = lock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                while guard.running && guard.pending.is_empty() {
                    guard = wake
                        .wait(guard)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                if !guard.running {
                    break;
                }
                guard.pending.pop_front().unwrap_or(Command {
                    path: String::new(),
                    body: Json::Null,
                })
            };
            if command.path.is_empty() {
                continue;
            }
            let mut reply = EngineReply {
                success: false,
                data: Json::Null,
                error: "Engine request failed; it was not replayed.".to_owned(),
            };
            match openatc_http::post_json_cancelled(
                endpoint,
                &command.path,
                &command.body,
                std::time::Duration::from_secs(35),
                cancelled,
            ) {
                Ok((status, data)) => {
                    if status == 200 {
                        reply = EngineReply {
                            success: true,
                            data,
                            error: String::new(),
                        };
                    } else {
                        data.get("error")
                            .and_then(Json::as_str)
                            .unwrap_or("Engine rejected the request")
                            .clone_into(&mut reply.error);
                    }
                }
                Err(_) => "Invalid engine response".clone_into(&mut reply.error),
            }
            let (lock, _) = &**inner;
            let mut guard = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.replies.insert(command.path, reply);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Latest engine snapshot.
    #[must_use]
    pub fn state(&self) -> State {
        self.lock().snapshot.clone()
    }

    /// Latest engine settings.
    #[must_use]
    pub fn settings(&self) -> Settings {
        self.lock().settings.clone()
    }

    /// Whether the last poll succeeded.
    #[must_use]
    pub fn connected(&self) -> bool {
        self.lock().connected
    }

    /// Human connection status.
    #[must_use]
    pub fn status(&self) -> String {
        self.lock().status.clone()
    }

    /// Engine base URL.
    #[must_use]
    pub fn endpoint(&self) -> String {
        self.endpoint.clone()
    }

    /// Queue a command; drops with a queue-full error past depth 32.
    pub fn post(&self, path: &str, body: Json) {
        let mut guard = self.lock();
        if guard.pending.len() < 32 {
            guard.pending.push_back(Command {
                path: path.to_owned(),
                body,
            });
            self.inner.1.notify_one();
        } else {
            guard.replies.insert(
                path.to_owned(),
                EngineReply {
                    success: false,
                    data: Json::Null,
                    error: "Request queue full".to_owned(),
                },
            );
        }
    }

    /// Take the latest reply for a path, if any.
    #[must_use]
    pub fn take_reply(&self, path: &str) -> Option<EngineReply> {
        self.lock().replies.remove(path)
    }

    /// Offer fresh telemetry for the next poll.
    pub fn telemetry(&self, value: Telemetry) {
        self.lock().latest_telemetry = Some(value);
    }
}

impl Drop for EngineClient {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.lock().running = false;
        self.inner.1.notify_all();
        if let Some(worker) = self.polling_worker.take() {
            let _ = worker.join();
        }
        if let Some(worker) = self.command_worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn client_unload_cancels_and_joins_a_stalled_poll() {
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 8192];
            let _ = stream.read(&mut request);
            ready_tx.send(()).unwrap();
            let _ = release_rx.recv();
        });
        let client = EngineClient::new(&origin);
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let begin = std::time::Instant::now();
        drop(client);
        assert!(begin.elapsed() < Duration::from_secs(2));
        release_tx.send(()).unwrap();
        server.join().unwrap();
    }
}
