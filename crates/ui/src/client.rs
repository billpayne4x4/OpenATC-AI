//! Engine snapshots and a bounded HTTP command queue.
//! HTTPS requests are handled by the companion engine.

// Layout tests compare exact fixture coordinates.
#![allow(clippy::float_cmp)]

use openatc_core::state::{State, Telemetry};
use openatc_settings::Settings;
use std::collections::{HashMap, VecDeque};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

type Json = serde_json::Value;

/// One engine command awaiting send.
struct Command {
    path: String,
    body: Json,
    generation: u64,
    telemetry_revision: u64,
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
    snapshot: Arc<State>,
    settings: Settings,
    status: String,
    connected: bool,
    telemetry_error: Option<String>,
    latest_telemetry: Option<Telemetry>,
    latest_crew_observation: Option<Json>,
    latest_crew_ack: Option<Json>,
    observed_telemetry: Option<Telemetry>,
    telemetry_revision: u64,
    delivered_revision: u64,
    running: bool,
    generation: u64,
}

/// Polls `/telemetry` + `/state` every 200 ms; sends queued commands.
pub struct EngineClient {
    inner: Arc<(Mutex<Inner>, Condvar)>,
    endpoint: String,
    cancelled: Arc<AtomicBool>,
    polling_worker: Option<std::thread::JoinHandle<()>>,
    command_worker: Option<std::thread::JoinHandle<()>>,
    /// Concurrent crew conversations in flight.
    crew_in_flight: Arc<AtomicUsize>,
    /// Failures from the concurrent crew lane.
    crew_errors: Arc<Mutex<Vec<String>>>,
}

impl EngineClient {
    /// Connect to the engine at the endpoint, spawning both workers.
    #[must_use]
    pub fn new(endpoint: &str) -> Self {
        let inner = Arc::new((
            Mutex::new(Inner {
                pending: VecDeque::new(),
                replies: HashMap::new(),
                snapshot: Arc::new(State::default()),
                settings: Settings::default(),
                status: "Connecting to engine".to_owned(),
                connected: false,
                telemetry_error: None,
                latest_telemetry: None,
                latest_crew_observation: None,
                latest_crew_ack: None,
                observed_telemetry: None,
                telemetry_revision: 0,
                delivered_revision: 0,
                running: true,
                generation: 0,
            }),
            Condvar::new(),
        ));
        let mut client = Self {
            inner: inner.clone(),
            endpoint: endpoint.to_owned(),
            cancelled: Arc::new(AtomicBool::new(false)),
            polling_worker: None,
            command_worker: None,
            crew_in_flight: Arc::new(AtomicUsize::new(0)),
            crew_errors: Arc::new(Mutex::new(Vec::new())),
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
            let (telemetry, observation, ack, generation, revision) = {
                let mut guard = inner
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                (
                    guard.latest_telemetry.take(),
                    guard.latest_crew_observation.take(),
                    guard.latest_crew_ack.take(),
                    guard.generation,
                    guard.telemetry_revision,
                )
            };
            if let Some(observation) = observation {
                let _ = openatc_http::post_json_cancelled(
                    endpoint,
                    "/crew/observe",
                    &observation,
                    Duration::from_secs(2),
                    cancelled,
                );
            }
            if let Some(ack) = ack {
                let _ = openatc_http::post_json_cancelled(
                    endpoint,
                    "/crew/ack",
                    &ack,
                    Duration::from_secs(2),
                    cancelled,
                );
            }
            let sent_telemetry = telemetry.is_some();
            let telemetry_result = if let Some(telemetry) = telemetry {
                let body = serde_json::to_value(&telemetry).unwrap_or(Json::Null);
                openatc_http::post_json_cancelled(
                    endpoint,
                    "/telemetry",
                    &body,
                    std::time::Duration::from_secs(2),
                    cancelled,
                )
                .and_then(|(status, data)| {
                    if status == 200 {
                        Ok(())
                    } else {
                        Err(data["error"]
                            .as_str()
                            .unwrap_or("Engine rejected simulator telemetry")
                            .to_owned())
                    }
                })
            } else {
                Ok(())
            };
            if sent_telemetry {
                let mut guard = inner
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if guard.generation == generation {
                    guard.telemetry_error = telemetry_result.as_ref().err().cloned();
                    if telemetry_result.is_ok() {
                        guard.delivered_revision = revision;
                    }
                    inner.1.notify_all();
                }
            }
            let response = openatc_http::get_json_cancelled(
                endpoint,
                "/state",
                std::time::Duration::from_secs(2),
                cancelled,
            );
            // Parse off the UI lock; large transcripts must not stall the draw callback.
            let response = response.map(|mut data| {
                let settings = data
                    .as_object_mut()
                    .and_then(|map| map.remove("settings"))
                    .and_then(|value| serde_json::from_value::<Settings>(value).ok());
                (
                    serde_json::from_value::<State>(data).map(Arc::new),
                    settings,
                )
            });
            {
                let mut guard = inner
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if !guard.running {
                    break;
                }
                if guard.generation != generation {
                    continue;
                }
                guard.connected = false;
                match response {
                    Ok((snapshot, settings)) => match snapshot {
                        Ok(mut snapshot) => {
                            let demo = snapshot.demo;
                            if let Some(observed) = &guard.observed_telemetry {
                                Arc::make_mut(&mut snapshot).telemetry = observed.clone();
                            }
                            guard.snapshot = snapshot;
                            if let Some(settings) = settings {
                                guard.settings = settings;
                            }
                            guard.connected = true;
                            if demo {
                                "Demo controller connected".clone_into(&mut guard.status);
                            } else {
                                "X-Plane connected".clone_into(&mut guard.status);
                            }
                            if let Some(error) = &guard.telemetry_error {
                                guard.status = format!("Simulator telemetry rejected: {error}");
                            }
                        }
                        Err(_) => {
                            "Incompatible engine response".clone_into(&mut guard.status);
                        }
                    },
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
                    generation: guard.generation,
                    telemetry_revision: 0,
                })
            };
            if command.path.is_empty() {
                continue;
            }
            // A click immediately after tuning must reach the newly tuned controller.
            if command.telemetry_revision > 0 {
                let deadline = std::time::Instant::now() + Duration::from_secs(3);
                let mut guard = inner
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                while guard.running
                    && guard.generation == command.generation
                    && guard.delivered_revision < command.telemetry_revision
                    && std::time::Instant::now() < deadline
                {
                    guard = inner
                        .1
                        .wait_timeout(guard, Duration::from_millis(100))
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .0;
                }
                if !guard.running || guard.generation != command.generation {
                    continue;
                }
                if guard.delivered_revision < command.telemetry_revision {
                    guard.replies.insert(command.path, EngineReply {
                        success: false, data: Json::Null,
                        error: "The engine has not received the current aircraft radio state. Try again once connected.".into(),
                    });
                    continue;
                }
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
            if guard.generation == command.generation {
                guard.replies.insert(command.path, reply);
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Share the latest immutable snapshot without copying routes or conversation history.
    #[must_use]
    pub fn state(&self) -> Arc<State> {
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

    /// Retained until a fresh simulator sample is accepted.
    #[must_use]
    pub fn telemetry_error(&self) -> Option<String> {
        self.lock().telemetry_error.clone()
    }

    /// Engine base URL.
    #[must_use]
    pub fn endpoint(&self) -> String {
        self.endpoint.clone()
    }

    /// Queue a command; drops with a queue-full error past depth 32.
    pub fn post(&self, path: &str, body: Json) {
        let mut guard = self.lock();
        if path == "/session/reset" {
            guard.pending.clear();
            guard.latest_crew_observation = None;
            guard.latest_crew_ack = None;
            guard.replies.clear();
            guard.generation = guard.generation.wrapping_add(1);
            self.crew_errors
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clear();
        }
        if path == "/crew/ack" {
            // The current action must finish even when a chat request is still awaiting a reply.
            guard.latest_crew_ack = Some(body);
            return;
        }
        if path == "/crew/observe" {
            // Observations must stay fresh while a conversational command awaits inference.
            if body
                .get("profile")
                .is_some_and(|profile| !profile.is_null())
            {
                guard.latest_crew_observation = Some(body);
            } else if let Some(previous) = guard.latest_crew_observation.as_mut() {
                let profile = previous.get("profile").cloned();
                *previous = body;
                if let Some(profile) = profile {
                    previous["profile"] = profile;
                }
            } else {
                guard.latest_crew_observation = Some(body);
            }
            return;
        }
        if guard.pending.len() < 32 {
            let generation = guard.generation;
            let telemetry_revision = if path == "/request" || path.starts_with("/request/") {
                guard.telemetry_revision
            } else {
                0
            };
            guard.pending.push_back(Command {
                path: path.to_owned(),
                body,
                generation,
                telemetry_revision,
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

    /// Send a crew conversation outside the ordered radio queue. Up to six run at once.
    pub fn post_crew(&self, body: Json) {
        let in_flight = self.crew_in_flight.fetch_add(1, Ordering::SeqCst);
        if in_flight >= 6 {
            self.crew_in_flight.fetch_sub(1, Ordering::SeqCst);
            self.push_crew_error("Too many crew conversations are already waiting.");
            return;
        }
        let endpoint = self.endpoint.clone();
        let cancelled = self.cancelled.clone();
        let counter = self.crew_in_flight.clone();
        let errors = self.crew_errors.clone();
        std::thread::spawn(move || {
            let result = openatc_http::post_json_cancelled(
                &endpoint,
                "/request",
                &body,
                Duration::from_secs(120),
                &cancelled,
            );
            match result {
                Ok((200, _)) => {}
                Ok((status, data)) => {
                    let detail = data
                        .get("error")
                        .and_then(Json::as_str)
                        .unwrap_or("No detail returned.");
                    errors
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(format!("Crew request failed ({status}): {detail}"));
                }
                Err(error) => errors
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(format!("Crew request failed: {error}")),
            }
            counter.fetch_sub(1, Ordering::SeqCst);
        });
    }

    fn push_crew_error(&self, message: &str) {
        self.crew_errors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(message.to_owned());
    }

    /// Take the next failure from the concurrent crew lane.
    #[must_use]
    pub fn take_crew_error(&self) -> Option<String> {
        let mut errors = self
            .crew_errors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (!errors.is_empty()).then(|| errors.remove(0))
    }

    /// Replace cached flight data immediately after an accepted reset.
    pub fn accept_reset(&self, data: &Json) {
        if let Ok(snapshot) = serde_json::from_value::<State>(data.clone()) {
            let mut guard = self.lock();
            guard.snapshot = Arc::new(snapshot);
            guard.latest_telemetry = None;
            guard.latest_crew_observation = None;
            guard.latest_crew_ack = None;
            guard.pending.clear();
            guard.replies.clear();
            guard.generation = guard.generation.wrapping_add(1);
        }
    }

    /// Take the latest reply for a path, if any.
    #[must_use]
    pub fn take_reply(&self, path: &str) -> Option<EngineReply> {
        self.lock().replies.remove(path)
    }

    /// Offer fresh telemetry for the next poll.
    pub fn telemetry(&self, value: Telemetry) {
        let mut guard = self.lock();
        Arc::make_mut(&mut guard.snapshot).telemetry = value.clone();
        guard.observed_telemetry = Some(value.clone());
        guard.telemetry_revision = guard.telemetry_revision.wrapping_add(1);
        guard.latest_telemetry = Some(value);
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
    fn crew_requests_bypass_the_ordered_command_queue() {
        let client = EngineClient::new("http://127.0.0.1:1");
        client.post_crew(serde_json::json!({"role":"copilot","text":"good morning"}));
        client.post_crew(serde_json::json!({"role":"ground","text":"remove chocks"}));
        assert!(
            client.lock().pending.is_empty(),
            "crew conversations must not occupy the ordered radio queue"
        );
    }

    #[test]
    fn rejected_telemetry_keeps_connection_and_error_until_a_valid_sample_arrives() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server_polls = polls.clone();
        let accept_sample = Arc::new(AtomicBool::new(false));
        let accepted = accept_sample.clone();
        let server = std::thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut buffer = [0; 4096];
                    let size = stream.read(&mut buffer).unwrap();
                    assert!(size > 0);
                    bytes.extend_from_slice(&buffer[..size]);
                    if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        let headers = std::str::from_utf8(&bytes[..end]).unwrap();
                        let length = headers
                            .lines()
                            .filter_map(|line| line.split_once(':'))
                            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                            .map_or(0, |(_, value)| value.trim().parse::<usize>().unwrap());
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                let request = String::from_utf8_lossy(&bytes);
                let (status, body) = if request.starts_with("POST /telemetry ")
                    && !accepted.load(Ordering::SeqCst)
                {
                    (
                        400,
                        serde_json::json!({"error":"Invalid telemetry: latitude = 95"}).to_string(),
                    )
                } else if request.starts_with("GET /state ") {
                    server_polls.fetch_add(1, Ordering::SeqCst);
                    let mut state = serde_json::to_value(State::default()).unwrap();
                    let settings = Settings {
                        tts_url: "http://speech-server:8099".into(),
                        ..Default::default()
                    };
                    state["settings"] = serde_json::to_value(settings).unwrap();
                    (200, state.to_string())
                } else {
                    (200, "{}".into())
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} Reply\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        let client = EngineClient::new(&endpoint);
        client.telemetry(Telemetry {
            latitude: 95.0,
            ..Default::default()
        });
        let wait_for = |predicate: &dyn Fn() -> bool| {
            let start = std::time::Instant::now();
            while !predicate() {
                assert!(
                    start.elapsed() < Duration::from_secs(5),
                    "client did not reach expected state"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        wait_for(&|| client.connected() && client.telemetry_error().is_some());
        let before = polls.load(Ordering::SeqCst);
        wait_for(&|| polls.load(Ordering::SeqCst) >= before + 3);
        assert!(client.connected());
        assert!(client.status().contains("latitude = 95"));
        assert_eq!(client.settings().tts_url, "http://speech-server:8099");
        assert_eq!(client.lock().delivered_revision, 0);
        accept_sample.store(true, Ordering::SeqCst);
        client.telemetry(Telemetry {
            latitude: 33.938_318,
            ..Default::default()
        });
        wait_for(&|| client.telemetry_error().is_none() && client.lock().delivered_revision == 2);
        assert!(client.connected());
        drop(client);
        stop.store(true, Ordering::SeqCst);
        server.join().unwrap();
    }

    #[test]
    fn telemetry_stays_fresh_while_a_command_waits_and_snapshots_are_shared() {
        use std::io::{Read, Write};
        use std::sync::mpsc;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (held_tx, held_rx) = mpsc::channel();
        let (seen_tx, seen_rx) = mpsc::channel();
        let (crew_tx, crew_rx) = mpsc::channel();
        let (ack_tx, ack_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let server_stop = stop.clone();
        let server = std::thread::spawn(move || {
            let mut held = None;
            while !server_stop.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = Vec::new();
                let (header_end, length) = loop {
                    let mut buffer = [0; 4096];
                    let n = stream.read(&mut buffer).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        let header_text = std::str::from_utf8(&bytes[..end]).unwrap();
                        let length = header_text
                            .lines()
                            .filter_map(|l| l.split_once(':'))
                            .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                            .map_or(0, |(_, v)| v.trim().parse::<usize>().unwrap());
                        if bytes.len() >= end + 4 + length {
                            break (end + 4, length);
                        }
                    }
                };
                let path = std::str::from_utf8(&bytes[..header_end])
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap();
                if path == "/request" {
                    held = Some(stream);
                    held_tx.send(()).unwrap();
                    continue;
                }
                if path == "/telemetry" {
                    let t: Telemetry =
                        serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
                    seen_tx.send(t.altitude_feet).unwrap();
                }
                if path == "/crew/ack" {
                    let ack: Json =
                        serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
                    ack_tx.send(ack).unwrap();
                }
                if path == "/crew/observe" {
                    let observation: Json =
                        serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
                    crew_tx.send(observation).unwrap();
                }
                let body = if path == "/state" {
                    serde_json::to_string(&State::default()).unwrap()
                } else {
                    "{}".into()
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
            drop(held);
        });
        let client = EngineClient::new(&origin);
        client.post("/request", Json::Null);
        held_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        client.post(
            "/crew/observe",
            serde_json::json!({"aircraft":"test","profile":{"controls":{}},"values":{"chocks":1}}),
        );
        let observed = crew_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(observed["values"]["chocks"], 1);
        assert!(observed["profile"].is_object());
        client.post(
            "/crew/observe",
            serde_json::json!({"aircraft":"test","profile":null,"values":{"chocks":0}}),
        );
        assert_eq!(
            crew_rx.recv_timeout(Duration::from_secs(3)).unwrap()["values"]["chocks"],
            0
        );
        client.post("/crew/ack", serde_json::json!({"sequence":42,"ok":true}));
        assert_eq!(
            ack_rx.recv_timeout(Duration::from_secs(3)).unwrap()["sequence"],
            42
        );
        let before = client.state();
        let mut reset = State::default();
        reset.plan.departure = "NEW".into();
        client.accept_reset(&serde_json::to_value(&reset).unwrap());
        let first = client.state();
        let second = client.state();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.plan.departure, "NEW");
        assert!(before.plan.departure.is_empty());
        for altitude in 0..10_000 {
            client.telemetry(Telemetry {
                altitude_feet: f64::from(altitude),
                ..Default::default()
            });
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            let value = seen_rx
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap();
            if value == 9999.0 {
                break;
            }
        }
        assert!(client.lock().pending.is_empty());
        stop.store(true, Ordering::SeqCst);
        server.join().unwrap();
        drop(client);
    }

    #[test]
    fn response_started_before_reset_cannot_restore_old_flight() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0; 8192];
            assert!(stream.read(&mut buffer).unwrap() > 0);
            ready_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            let mut old = State::default();
            old.plan.departure = "OLD".into();
            let body = serde_json::to_string(&old).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let client = EngineClient::new(&origin);
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        client.accept_reset(&serde_json::to_value(State::default()).unwrap());
        release_tx.send(()).unwrap();
        server.join().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(client.state().plan.departure.is_empty());
    }

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
