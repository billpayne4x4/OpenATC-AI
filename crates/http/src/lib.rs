//! Blocking HTTP transport for local plugin-to-engine requests.
//! Supports JSON and WAV payloads, timeouts and cancellation.
//! The companion engine handles HTTPS connections.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Split an `http://host:port` origin into host and port. Anything else is
/// rejected: no TLS, no paths, no userinfo.
fn origin_parts(origin: &str) -> Result<(String, u16), String> {
    let rest = origin
        .strip_prefix("http://")
        .ok_or_else(|| "Only plain http:// origins allowed".to_owned())?;
    if rest.contains('/') {
        return Err("Origin must not carry a path".to_owned());
    }
    let (host, port) = match rest.rsplit_once(':') {
        Some((host, port)) => (
            host.to_owned(),
            port.parse::<u16>()
                .map_err(|_| "Bad origin port".to_owned())?,
        ),
        None => (rest.to_owned(), 80),
    };
    if host.is_empty() {
        return Err("Bad origin host".to_owned());
    }
    Ok((host, port))
}

/// Send one request and return the status code plus body.
fn round_trip(
    origin: &str,
    request: &str,
    body: &[u8],
    timeout: Duration,
) -> Result<(u16, Vec<u8>), String> {
    round_trip_cancelled(origin, request, body, timeout, None)
}

fn round_trip_cancelled(
    origin: &str,
    request: &str,
    body: &[u8],
    timeout: Duration,
    cancelled: Option<&AtomicBool>,
) -> Result<(u16, Vec<u8>), String> {
    let stopped = || cancelled.is_some_and(|flag| flag.load(Ordering::SeqCst));
    if stopped() {
        return Err("Cancelled".to_owned());
    }
    let deadline = Instant::now() + timeout;
    let (host, port) = origin_parts(origin)?;
    let address = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|_| "Cannot resolve engine host")?
        .next()
        .ok_or("Cannot resolve engine host")?;
    let interval = if cancelled.is_some() {
        timeout.min(Duration::from_millis(100))
    } else {
        timeout
    };
    let connect_timeout = if cancelled.is_some() {
        timeout.min(Duration::from_millis(250))
    } else {
        timeout
    };
    let mut stream =
        TcpStream::connect_timeout(&address, connect_timeout).map_err(|_| "Cannot reach engine")?;
    stream
        .set_read_timeout(Some(interval))
        .map_err(|_| "Cannot reach engine")?;
    stream
        .set_write_timeout(Some(interval))
        .map_err(|_| "Cannot reach engine")?;
    stream
        .write_all(request.as_bytes())
        .map_err(|_| "Engine request failed")?;
    stream
        .write_all(body)
        .map_err(|_| "Engine request failed")?;
    let mut raw = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        if stopped() {
            return Err("Cancelled".to_owned());
        }
        if Instant::now() >= deadline {
            return Err("Engine response timed out".to_owned());
        }
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                raw.extend_from_slice(&buffer[..count]);
                // Content-Length completes the response without waiting for EOF.
                if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n")
                    && let Ok(head) = std::str::from_utf8(&raw[..end])
                    && let Some(length) = head
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                    && raw.len().saturating_sub(end + 4) >= length
                {
                    break;
                }
            }
            Err(error)
                if cancelled.is_some()
                    && [std::io::ErrorKind::WouldBlock, std::io::ErrorKind::TimedOut]
                        .contains(&error.kind()) => {}
            Err(_) => return Err("Engine response unreadable".to_owned()),
        }
    }
    split_response(&raw)
}

/// Split a raw HTTP/1.x response into status and body.
fn split_response(raw: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let head_end = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "Engine response unreadable".to_owned())?;
    let head = std::str::from_utf8(&raw[..head_end])
        .map_err(|_| "Engine response unreadable".to_owned())?;
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| "Engine response unreadable".to_owned())?;
    let mut length = None;
    for line in head.lines().skip(1) {
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        } else if let Some(value) = line.strip_prefix("content-length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = raw[head_end + 4..].to_vec();
    if let Some(length) = length {
        body.truncate(length);
    }
    Ok((status, body))
}

fn request_line(method: &str, path: &str, host: &str, body_len: usize) -> String {
    format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {body_len}\r\nConnection: close\r\n\r\n"
    )
}

/// GET a JSON document.
pub fn get_json(origin: &str, path: &str, timeout: Duration) -> Result<serde_json::Value, String> {
    let (host, _) = origin_parts(origin)?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    let (status, body) = round_trip(origin, &request, &[], timeout)?;
    if status != 200 {
        return Err("Engine request failed".to_owned());
    }
    serde_json::from_slice(&body).map_err(|_| "Invalid engine response".to_owned())
}

/// POST a JSON document, returning status plus parsed body.
pub fn post_json(
    origin: &str,
    path: &str,
    body: &serde_json::Value,
    timeout: Duration,
) -> Result<(u16, serde_json::Value), String> {
    let (host, _) = origin_parts(origin)?;
    let bytes = serde_json::to_vec(body).unwrap_or_default();
    let request = request_line("POST", path, &host, bytes.len());
    let (status, reply) = round_trip(origin, &request, &bytes, timeout)?;
    let parsed = serde_json::from_slice(&reply).unwrap_or(serde_json::Value::Null);
    Ok((status, parsed))
}

/// POST raw bytes (WAV captures), returning status plus parsed body.
pub fn post_bytes(
    origin: &str,
    path: &str,
    content_type: &str,
    body: &[u8],
    timeout: Duration,
) -> Result<(u16, serde_json::Value), String> {
    let (host, _) = origin_parts(origin)?;
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let (status, reply) = round_trip(origin, &request, body, timeout)?;
    let parsed = serde_json::from_slice(&reply).unwrap_or(serde_json::Value::Null);
    Ok((status, parsed))
}

/// POST a JSON document and return raw bytes (WAV answers).
pub fn post_wav(
    origin: &str,
    path: &str,
    body: &serde_json::Value,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let (host, _) = origin_parts(origin)?;
    let bytes = serde_json::to_vec(body).unwrap_or_default();
    let request = request_line("POST", path, &host, bytes.len());
    let (status, reply) = round_trip(origin, &request, &bytes, timeout)?;
    if status != 200 {
        return Err("Engine request failed".to_owned());
    }
    Ok(reply)
}

/// GET JSON with cancellation on client shutdown.
pub fn get_json_cancelled(
    origin: &str,
    path: &str,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> Result<serde_json::Value, String> {
    let (host, _) = origin_parts(origin)?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    let (status, body) = round_trip_cancelled(origin, &request, &[], timeout, Some(cancelled))?;
    if status != 200 {
        return Err("Engine request failed".to_owned());
    }
    serde_json::from_slice(&body).map_err(|_| "Invalid engine response".to_owned())
}
/// POST JSON with cancellation on client shutdown.
pub fn post_json_cancelled(
    origin: &str,
    path: &str,
    body: &serde_json::Value,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> Result<(u16, serde_json::Value), String> {
    let (host, _) = origin_parts(origin)?;
    let bytes = serde_json::to_vec(body).unwrap_or_default();
    let request = request_line("POST", path, &host, bytes.len());
    let (status, reply) = round_trip_cancelled(origin, &request, &bytes, timeout, Some(cancelled))?;
    Ok((
        status,
        serde_json::from_slice(&reply).unwrap_or(serde_json::Value::Null),
    ))
}

/// POST synthesis with cancellation while waiting for a slow speech provider.
pub fn post_wav_cancelled(
    origin: &str,
    path: &str,
    body: &serde_json::Value,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, String> {
    let (host, _) = origin_parts(origin)?;
    let bytes = serde_json::to_vec(body).unwrap_or_default();
    let request = request_line("POST", path, &host, bytes.len());
    let (status, reply) = round_trip_cancelled(origin, &request, &bytes, timeout, Some(cancelled))?;
    if status != 200 {
        return Err("Engine request failed".to_owned());
    }
    Ok(reply)
}
/// POST transcription with cancellation on plugin unload.
pub fn post_bytes_cancelled(
    origin: &str,
    path: &str,
    content_type: &str,
    body: &[u8],
    timeout: Duration,
    cancelled: &AtomicBool,
) -> Result<(u16, serde_json::Value), String> {
    let (host, _) = origin_parts(origin)?;
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let (status, reply) = round_trip_cancelled(origin, &request, body, timeout, Some(cancelled))?;
    Ok((
        status,
        serde_json::from_slice(&reply).unwrap_or(serde_json::Value::Null),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_server(body: &'static str) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let worker = std::thread::spawn(move || {
            for _ in 0..2 {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut head = vec![0u8; 4096];
                let _ = std::io::Read::read(&mut stream, &mut head);
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = std::io::Write::write_all(&mut stream, reply.as_bytes());
            }
        });
        (format!("http://127.0.0.1:{port}"), worker)
    }

    #[test]
    fn rejects_non_http_origins() {
        assert!(origin_parts("https://example.com").is_err());
        assert!(origin_parts("http://example.com/v1").is_err());
        assert!(origin_parts("http://example.com:abc").is_err());
        assert_eq!(
            origin_parts("http://127.0.0.1:8087").unwrap(),
            ("127.0.0.1".to_owned(), 8087)
        );
    }

    #[test]
    fn round_trips_json() {
        let (origin, worker) = test_server(r#"{"ok":true}"#);
        let reply = get_json(&origin, "/state", Duration::from_secs(5)).unwrap();
        assert_eq!(reply, serde_json::json!({"ok": true}));
        let (status, posted) = post_json(
            &origin,
            "/request",
            &serde_json::json!({}),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(status, 200);
        assert_eq!(posted, serde_json::json!({"ok": true}));
        worker.join().unwrap();
    }
    #[test]
    fn cancel_stalled_provider_without_waiting_for_request_timeout() {
        use std::sync::{Arc, mpsc};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 8192];
            let _ = stream.read(&mut request);
            ready_tx.send(()).unwrap();
            let _ = release_rx.recv();
        });
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let client = std::thread::spawn(move || {
            post_wav_cancelled(
                &origin,
                "/speech/speak",
                &serde_json::json!({"text":"test"}),
                Duration::from_secs(30),
                &flag,
            )
        });
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let begin = Instant::now();
        cancelled.store(true, Ordering::SeqCst);
        assert_eq!(client.join().unwrap().unwrap_err(), "Cancelled");
        assert!(begin.elapsed() < Duration::from_secs(2));
        release_tx.send(()).unwrap();
        server.join().unwrap();
    }
}
