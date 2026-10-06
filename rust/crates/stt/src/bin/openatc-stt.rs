//! `openatc-stt`: speech-to-text sidecar for `openatc-ai`.
//!
//! Own binary (not a thread) because `whisper.cpp` and `llama.cpp` each vendor
//! `ggml`: linking both into one process collides at link time. The main server
//! spawns this sidecar, waits for `/health`, and proxies transcriptions to it.
//!
//! Contract: `POST /transcribe` takes raw 16 kHz mono s16 WAV bytes and answers
//! `{"text"}`. `GET /health` answers `{"status","backend"}`.

use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use clap::Parser;
use openatc_stt::{SttBackend, SttEngine};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
struct Args {
    /// Local port to listen on.
    #[arg(long, default_value_t = 8099)]
    port: u16,
    /// ggml model file.
    #[arg(long)]
    model: PathBuf,
    /// Compute: auto (CUDA when built for it, else CPU), cpu, or cuda.
    #[arg(long, default_value = "auto")]
    backend: String,
}

struct AppState {
    engine: SttEngine,
}

async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "backend": state.engine.backend(),
    }))
}

async fn transcribe(
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let failed = |message: String| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": message })),
        )
    };
    match state.engine.transcribe_wav(&body) {
        Ok(text) => Ok(Json(serde_json::json!({ "text": text }))),
        Err(message) => Err(failed(message)),
    }
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let args = Args::parse();
    #[cfg(target_os = "linux")]
    tokio::spawn(async {
        // `kill_on_drop` misses signal death: exit when reparented to init.
        // Pure `/proc` read, no syscalls beyond the filesystem.
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let orphaned = std::fs::read_to_string("/proc/self/stat")
                .ok()
                .and_then(|stat| {
                    stat.split_whitespace()
                        .nth(3)
                        .and_then(|ppid| ppid.parse::<u32>().ok())
                        .map(|ppid| ppid == 1)
                })
                .unwrap_or(false);
            if orphaned {
                std::process::exit(0);
            }
        }
    });
    let backend = match args.backend.as_str() {
        "auto" => SttBackend::Auto,
        "cpu" => SttBackend::Cpu,
        "cuda" => SttBackend::Cuda,
        other => {
            return Err(format!("unknown backend {other:?}; want auto, cpu or cuda"));
        }
    };
    let engine = SttEngine::load(&args.model, backend)
        .map_err(|error| format!("STT load failed ({}): {error}", args.model.display()))?;
    eprintln!("openatc-stt: ready ({})", engine.backend());
    let state = Arc::new(AppState { engine });
    let app = Router::new()
        .route("/health", get(health))
        .route("/transcribe", post(transcribe))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", args.port))
        .await
        .map_err(|error| error.to_string())?;
    eprintln!("openatc-stt: listening on 127.0.0.1:{}", args.port);
    axum::serve(listener, app)
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}
