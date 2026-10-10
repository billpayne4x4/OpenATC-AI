//! `openatc-ai`: one server, three embedded engines (LLM + STT + TTS live).
//!
//! OpenAI-compatible surface the engine already speaks:
//! `POST /v1/chat/completions`, `GET /v1/voices`, `GET /health`,
//! `POST /v1/audio/transcriptions`, `POST /v1/audio/speech`.
//!
//! Threading: the llama context is `!Send`, so all inference lives on one
//! dedicated thread. HTTP handlers post jobs to it and await oneshot replies;
//! only owned data (`String`, numbers) ever crosses threads.

mod voices;

use axum::{
    Json, Router,
    extract::{Multipart, State},
    http::StatusCode,
    routing::{get, post},
};
use clap::Parser;
use llama_cpp_2::{
    context::params::LlamaContextParams, llama_backend::LlamaBackend, llama_batch::LlamaBatch,
    model::LlamaModel, model::params::LlamaModelParams, sampling::LlamaSampler,
};
use openatc_ai_core::{LockedModel, Lockfile, Manifest, ModelEntry, ensure, resolve, sha256_hex};
use openatc_platform::{detect_gpu, models_dir};
use openatc_tts::{TtsEngine, fx::Fx, fx::apply_fx, wav_bytes};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::oneshot;

#[derive(Parser)]
struct Args {
    /// Local port to listen on.
    #[arg(long, default_value_t = 8099)]
    port: u16,
    /// Interface to bind (default loopback; 0.0.0.0 for LAN).
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    /// Model directory override (default: platform data dir or `OPENATC_AI_MODELS`).
    #[arg(long)]
    models_dir: Option<PathBuf>,
    /// KV context size for the LLM.
    #[arg(long, default_value_t = 4096)]
    ctx_size: u32,
    /// STT compute: auto (CUDA when built for it, else CPU), cpu, or cuda.
    #[arg(long, default_value = "auto")]
    stt_backend: String,
    /// `openatc-stt` binary override (default: beside this binary, else PATH).
    #[arg(long)]
    stt_bin: Option<PathBuf>,
}

/// One inference request crossing into the llama thread.
struct InferenceJob {
    messages: Vec<ChatMessage>,
    temperature: f32,
    max_tokens: u32,
    reply: oneshot::Sender<Result<Completion, String>>,
}

/// Completed generation with token counts.
struct Completion {
    text: String,
    prompt_tokens: u32,
    output_tokens: u32,
}

/// Cloneable handle to the inference thread.
#[derive(Clone)]
struct InferenceHandle {
    jobs: mpsc::Sender<InferenceJob>,
}

/// Handle to the STT sidecar (separate process: `whisper.cpp` and
/// `llama.cpp` each vendor `ggml` and cannot link into one binary).
/// The child dies with the server (`kill_on_drop`).
struct SttHandle {
    base_url: String,
    client: reqwest::Client,
    #[allow(dead_code)]
    child: tokio::process::Child,
}

/// Locate the `openatc-stt` sidecar: explicit flag, beside this binary, else PATH.
fn sidecar_path(explicit: Option<&std::path::Path>) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_owned();
    }
    let name = format!("openatc-stt{}", std::env::consts::EXE_SUFFIX);
    if let Ok(us) = std::env::current_exe()
        && let Some(dir) = us.parent()
        && dir.join(&name).exists()
    {
        return dir.join(&name);
    }
    PathBuf::from(name)
}

/// Start the sidecar and wait for `/health`, returning its base URL and backend.
/// Fails fast when the model cannot load, mirroring the inference loop.
async fn start_sidecar(
    bin: &std::path::Path,
    model: &std::path::Path,
    backend: &str,
) -> Result<(tokio::process::Child, String, String), String> {
    let port = {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|error| format!("cannot pick a sidecar port: {error}"))?;
        listener
            .local_addr()
            .map_err(|error| format!("cannot read sidecar port: {error}"))?
            .port()
    };
    let mut child = tokio::process::Command::new(bin)
        .arg("--port")
        .arg(port.to_string())
        .arg("--model")
        .arg(model)
        .arg("--backend")
        .arg(backend)
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("cannot start {}: {error}", bin.display()))?;
    let base_url = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        if let Ok(exit) = child.try_wait()
            && exit.is_some()
        {
            return Err("STT sidecar exited during load".to_owned());
        }
        if let Ok(response) = client
            .get(format!("{base_url}/health"))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
        {
            if response.status().is_success() {
                let body: serde_json::Value = response.json().await.unwrap_or_default();
                let backend = body
                    .get("backend")
                    .and_then(|value| value.as_str())
                    .unwrap_or("cpu")
                    .to_owned();
                return Ok((child, base_url, backend));
            }
        } else if std::time::Instant::now() > deadline {
            return Err("STT sidecar never became healthy".to_owned());
        } else {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        if std::time::Instant::now() > deadline {
            return Err("STT sidecar never became healthy".to_owned());
        }
    }
}

/// Handle to the TTS worker thread (the session lives there, mirroring the
/// gateway's single-inference lock).
struct TtsHandle {
    jobs: mpsc::Sender<TtsJob>,
}

/// One synthesis request crossing into the TTS thread.
struct TtsJob {
    text: String,
    voice: String,
    speed: f32,
    sentence_pause: f32,
    clause_pause: f32,
    fx: Fx,
    reply: oneshot::Sender<Result<Vec<u8>, String>>,
}

/// Shared server state (all fields are Send).
struct AppState {
    inference: InferenceHandle,
    stt: SttHandle,
    tts: TtsHandle,
    model_name: String,
    model_status: BTreeMap<String, String>,
    backend: String,
    stt_backend: String,
}

/// Run synthesis on the TTS thread. The engine loads here so a bad model
/// fails fast at startup, mirroring the inference loop.
fn tts_loop(
    model_path: &std::path::Path,
    voices_path: &std::path::Path,
    jobs: mpsc::Receiver<TtsJob>,
) {
    let mut engine = match TtsEngine::load(model_path, voices_path) {
        Ok(engine) => engine,
        Err(error) => {
            eprintln!("openatc-ai: TTS load failed: {error}");
            return;
        }
    };
    eprintln!(
        "openatc-ai: TTS ready ({} voices)",
        engine.voice_names().len()
    );
    for job in jobs {
        let result = engine
            .synthesize(
                &job.text,
                &job.voice,
                job.speed,
                job.sentence_pause,
                job.clause_pause,
            )
            .map(|samples| {
                let seed = format!(
                    "{}|{}|{}|{}|{}|{}/{}/{}/{}",
                    job.text,
                    job.voice,
                    job.speed,
                    job.sentence_pause,
                    job.clause_pause,
                    job.fx.hiss,
                    job.fx.crackle,
                    job.fx.static_,
                    job.fx.bandpass
                );
                wav_bytes(&apply_fx(&samples, openatc_tts::SAMPLE_RATE, job.fx, &seed))
            });
        if let Err(error) = &result {
            eprintln!(
                "openatc-ai: TTS failed: voice={} text={:?} error={error}",
                job.voice, job.text
            );
        }
        let _ = job.reply.send(result);
    }
}

/// One `OpenAI` chat message.
#[derive(Debug, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

/// `OpenAI` chat completion request (subset we honor).
#[derive(Debug, Deserialize)]
struct ChatRequest {
    #[serde(default)]
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(default = "default_temperature")]
    temperature: f32,
    max_tokens: Option<u32>,
}

fn default_temperature() -> f32 {
    1.0
}

/// Prompt in Qwen2.5 `ChatML` form, built by hand: the model file is pinned,
/// so there is no template to look up.
fn chatml_prompt(messages: &[ChatMessage]) -> String {
    let mut prompt = String::new();
    for message in messages {
        let role = match message.role.as_str() {
            "system" | "user" | "assistant" => message.role.as_str(),
            _ => "user",
        };
        prompt.push_str("<|im_start|>");
        prompt.push_str(role);
        prompt.push('\n');
        prompt.push_str(&message.content);
        prompt.push_str("<|im_end|>\n");
    }
    prompt.push_str("<|im_start|>assistant\n");
    prompt
}

/// Run one completion on the inference thread. Returns text plus token counts.
fn complete(
    model: &LlamaModel,
    context: &mut llama_cpp_2::context::LlamaContext,
    n_ctx: u32,
    messages: &[ChatMessage],
    temperature: f32,
    max_new: u32,
) -> Result<Completion, String> {
    let prompt = chatml_prompt(messages);
    let vocab = model.vocab();
    let tokens = vocab.tokenize(prompt.as_bytes(), true, true);
    let max_new = max_new.clamp(1, 1024) as usize;
    if tokens.len() + max_new + 8 >= n_ctx as usize {
        return Err("prompt plus max_tokens exceeds context size".to_owned());
    }
    let mut batch = LlamaBatch::new(tokens.len() + max_new, 1);
    for (index, token) in tokens.iter().enumerate() {
        batch
            .add(
                *token,
                i32::try_from(index).unwrap_or(i32::MAX),
                &[0],
                index + 1 == tokens.len(),
            )
            .map_err(|error| format!("batch overflow: {error:?}"))?;
    }
    context
        .decode(&mut batch)
        .map_err(|error| format!("decode failed: {error:?}"))?;
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(42, |time| time.subsec_nanos());
    let mut sampler = if temperature <= 0.0 {
        LlamaSampler::greedy()
    } else {
        LlamaSampler::chain_simple([
            LlamaSampler::top_k(40),
            LlamaSampler::top_p(0.9, 1),
            LlamaSampler::temp(temperature),
            LlamaSampler::dist(seed),
        ])
    };
    let mut output = Vec::new();
    let mut generated = 0u32;
    // Token counts stay far below i32 range in practice.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    for position in (tokens.len() as i32..).take(max_new) {
        let token = sampler.sample(context, -1);
        sampler.accept(token);
        if vocab.is_eog(token) {
            break;
        }
        output.extend_from_slice(&vocab.token_to_piece(token, false, None));
        generated += 1;
        batch.clear();
        batch
            .add(token, position, &[0], true)
            .map_err(|error| format!("batch overflow: {error:?}"))?;
        context
            .decode(&mut batch)
            .map_err(|error| format!("decode failed: {error:?}"))?;
    }
    Ok(Completion {
        text: String::from_utf8_lossy(&output).into_owned(),
        prompt_tokens: u32::try_from(tokens.len()).unwrap_or(u32::MAX),
        output_tokens: generated,
    })
}

/// Inference thread entry: loads everything once, then serves jobs forever.
fn inference_loop(
    llm_path: &std::path::Path,
    ctx_size: u32,
    jobs: mpsc::Receiver<InferenceJob>,
) -> Result<(), String> {
    let backend =
        LlamaBackend::init().map_err(|error| format!("backend init failed: {error:?}"))?;
    let model = LlamaModel::load_from_file(&backend, llm_path, &LlamaModelParams::default())
        .map_err(|error| format!("model load failed: {error:?}"))?;
    eprintln!("openatc-ai: model ready ({} params)", model.n_params());
    let mut context = model
        .new_context(
            &backend,
            LlamaContextParams::default().with_n_ctx(std::num::NonZeroU32::new(ctx_size)),
        )
        .map_err(|error| format!("context init failed: {error:?}"))?;
    for job in jobs {
        let result = complete(
            &model,
            &mut context,
            ctx_size,
            &job.messages,
            job.temperature,
            job.max_tokens,
        );
        let _ = job.reply.send(result);
    }
    Ok(())
}

/// One transcription result, gateway-compatible shape.
#[derive(Serialize)]
struct TranscriptionResponse {
    text: String,
}

/// `OpenAI` chat completion response shape.
#[derive(Serialize)]
struct ChatResponse {
    id: String,
    object: String,
    created: u64,
    model: String,
    choices: Vec<ChatChoice>,
    usage: ChatUsage,
}

/// Single completion choice.
#[derive(Serialize)]
struct ChatChoice {
    index: u32,
    message: ChatContent,
    finish_reason: String,
}

/// Assistant message content.
#[derive(Serialize)]
struct ChatContent {
    role: String,
    content: String,
}

/// Token usage counts. Field names follow the `OpenAI` usage object exactly.
#[allow(clippy::struct_field_names)]
#[derive(Serialize)]
struct ChatUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

/// OpenAI-style error body.
#[derive(Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

/// Error detail.
#[derive(Serialize)]
struct ErrorDetail {
    message: String,
    #[serde(rename = "type")]
    kind: String,
}

fn api_error(status: StatusCode, message: String) -> (StatusCode, Json<ErrorBody>) {
    (
        status,
        Json(ErrorBody {
            error: ErrorDetail {
                message,
                kind: "inference_error".to_owned(),
            },
        }),
    )
}

/// Chat completions, `OpenAI` shape in and out.
async fn chat_completions(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, (StatusCode, Json<ErrorBody>)> {
    if request.messages.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "messages must not be empty".to_owned(),
        ));
    }
    let max_tokens = request.max_tokens.unwrap_or(512).clamp(1, 1024);
    let model = if request.model.is_empty() {
        state.model_name.clone()
    } else {
        request.model.clone()
    };
    let (reply_tx, reply_rx) = oneshot::channel();
    let job = InferenceJob {
        messages: request.messages,
        temperature: request.temperature,
        max_tokens,
        reply: reply_tx,
    };
    state.inference.jobs.send(job).map_err(|_| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "inference thread gone".to_owned(),
        )
    })?;
    let output = reply_rx.await.map_err(|_| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "inference reply lost".to_owned(),
        )
    })?;
    let completion =
        output.map_err(|message| api_error(StatusCode::INTERNAL_SERVER_ERROR, message))?;
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs());
    Ok(Json(ChatResponse {
        id: format!("chatcmpl-{created}"),
        object: "chat.completion".to_owned(),
        created,
        model,
        choices: vec![ChatChoice {
            index: 0,
            message: ChatContent {
                role: "assistant".to_owned(),
                content: completion.text,
            },
            finish_reason: "stop".to_owned(),
        }],
        usage: ChatUsage {
            prompt_tokens: completion.prompt_tokens,
            completion_tokens: completion.output_tokens,
            total_tokens: completion.prompt_tokens + completion.output_tokens,
        },
    }))
}

/// Liveness plus per-model file status (recorded at startup).
async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "backend": state.backend,
        "stt": state.stt_backend,
        "models": state.model_status,
    }))
}

/// Kokoro v1 voice roster, matching the voices table on disk.
async fn list_voices() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "voices": voices::KOKORO_V1 }))
}

/// Speech-to-text: multipart `file` (16 kHz mono WAV) plus ignored `model`.
/// Answers `{"text"}` exactly like the gateway it replaces.
async fn transcriptions(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<TranscriptionResponse>, (StatusCode, Json<ErrorBody>)> {
    let mut wav: Option<Vec<u8>> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?
    {
        if field.name() == Some("file") {
            wav = Some(
                field
                    .bytes()
                    .await
                    .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?
                    .to_vec(),
            );
        }
    }
    let wav = wav.ok_or_else(|| {
        api_error(
            StatusCode::BAD_REQUEST,
            "multipart needs a file part".to_owned(),
        )
    })?;
    let response = state
        .stt
        .client
        .post(format!("{}/transcribe", state.stt.base_url))
        .body(wav)
        .send()
        .await
        .map_err(|error| {
            api_error(
                StatusCode::BAD_GATEWAY,
                format!("sidecar unreachable: {error}"),
            )
        })?;
    if !response.status().is_success() {
        return Err(api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "transcription failed".to_owned(),
        ));
    }
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let text = body
        .get("text")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned();
    Ok(Json(TranscriptionResponse { text }))
}

/// Text-to-speech: gateway-compatible JSON in, 16-bit PCM WAV bytes out.
/// Validation mirrors the gateway it replaces.
async fn speech(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<impl axum::response::IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let text = body
        .get("input")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .trim()
        .to_owned();
    if text.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "missing input text".to_owned(),
        ));
    }
    if text.len() > openatc_tts::MAX_TEXT {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "speech text too long".to_owned(),
        ));
    }
    // Speech speeds and pauses live in [0, 2]; exactly representable.
    #[allow(clippy::cast_possible_truncation)]
    let speech_f32 = |value: f64| value as f32;
    let speed = speech_f32(
        body.get("speed")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(1.0)
            .clamp(0.5, 2.0),
    );
    let pause = |key: &str, default: f64| {
        speech_f32(
            body.get(key)
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(default)
                .clamp(0.0, 1.0),
        )
    };
    let sentence_pause = pause("sentence_pause", 0.25);
    let clause_pause = pause("clause_pause", 0.1);
    let fx_body = body.get("effects");
    let level = |key: &str| {
        speech_f32(
            fx_body
                .and_then(|fx| fx.get(key))
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0)
                .clamp(0.0, 1.0),
        )
    };
    let fx = Fx {
        hiss: level("hiss"),
        crackle: level("crackle"),
        static_: level("static"),
        bandpass: fx_body
            .and_then(|fx| fx.get("bandpass"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    };
    let voice = body
        .get("voice")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned();
    let (reply_tx, reply_rx) = oneshot::channel();
    state
        .tts
        .jobs
        .send(TtsJob {
            text,
            voice,
            speed,
            sentence_pause,
            clause_pause,
            fx,
            reply: reply_tx,
        })
        .map_err(|_| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "synthesizer gone".to_owned(),
            )
        })?;
    let wav = reply_rx
        .await
        .map_err(|_| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "synthesis reply lost".to_owned(),
            )
        })?
        .map_err(|message| api_error(StatusCode::INTERNAL_SERVER_ERROR, message))?;
    Ok(([(axum::http::header::CONTENT_TYPE, "audio/wav")], wav))
}

/// Ensure every manifest entry, recording lockfile data for fresh fetches.
/// Blocking network and filesystem IO: call from `spawn_blocking`.
fn ensure_all(
    root: &std::path::Path,
    manifest: &Manifest,
    lockfile: &mut Lockfile,
) -> (BTreeMap<String, String>, bool) {
    let mut status = BTreeMap::new();
    let mut lock_changed = false;
    for entry in &manifest.model {
        let locked = lockfile.model.get(&entry.name).cloned();
        eprintln!("openatc-ai: ensuring {} ...", entry.name);
        match ensure(root, entry, locked.as_ref(), |done, total| {
            if total > 0 {
                eprint!("\ropenatc-ai: {} {done}/{total} bytes", entry.name);
            }
        }) {
            Ok(path) => {
                eprintln!("\nopenatc-ai: {} ready", entry.name);
                status.insert(entry.name.clone(), "ready".to_owned());
                if locked.is_none() {
                    match record_lock(&path, lockfile, entry) {
                        Ok(()) => lock_changed = true,
                        Err(error) => eprintln!("openatc-ai: {error}"),
                    }
                }
            }
            Err(error) => {
                eprintln!("\nopenatc-ai: {error}");
                status.insert(entry.name.clone(), "missing".to_owned());
            }
        }
    }
    (status, lock_changed)
}

/// Ensure the model dir exists and the manifest downloads are complete.
/// Returns the resolved root, manifest and per-model status.
async fn ensure_models(
    root: std::path::PathBuf,
) -> Result<(std::path::PathBuf, Manifest, BTreeMap<String, String>), String> {
    std::fs::create_dir_all(&root)
        .map_err(|error| format!("cannot create {}: {error}", root.display()))?;
    let manifest =
        Manifest::parse(include_str!("../../../models.toml")).map_err(|error| error.to_string())?;
    let lock_path = root.join("models.lock.toml");
    let lockfile: Lockfile = std::fs::read_to_string(&lock_path)
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default();
    let manifest_clone = manifest.clone();
    let root_clone = root.clone();
    let (status, lock_changed, lockfile) = tokio::task::spawn_blocking(move || {
        let mut lockfile = lockfile;
        let (status, lock_changed) = ensure_all(&root_clone, &manifest_clone, &mut lockfile);
        (status, lock_changed, lockfile)
    })
    .await
    .map_err(|error| format!("download task failed: {error}"))?;
    if lock_changed {
        let text = toml::to_string(&lockfile).map_err(|error| error.to_string())?;
        std::fs::write(&lock_path, text).map_err(|error| error.to_string())?;
    }
    Ok((root, manifest, status))
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let args = Args::parse();
    let root = args.models_dir.clone().unwrap_or_else(models_dir);
    let (root, manifest, status) = ensure_models(root).await?;
    let llm_path = resolve(
        &root,
        manifest
            .model
            .iter()
            .find(|entry| entry.path.starts_with("llm/"))
            .ok_or("manifest has no llm entry")?,
    );
    if !llm_path.exists() {
        return Err(format!("LLM file missing: {}", llm_path.display()));
    }
    let stt_backend = match args.stt_backend.as_str() {
        "auto" | "cpu" | "cuda" => args.stt_backend.clone(),
        other => {
            return Err(format!(
                "unknown --stt-backend {other:?}; want auto, cpu or cuda"
            ));
        }
    };
    let stt_path = resolve(
        &root,
        manifest
            .model
            .iter()
            .find(|entry| entry.path.starts_with("stt/"))
            .ok_or("manifest has no stt entry")?,
    );
    if !stt_path.exists() {
        return Err(format!("STT file missing: {}", stt_path.display()));
    }
    let (job_tx, job_rx) = mpsc::channel();
    let loader = std::thread::spawn(move || inference_loop(&llm_path, args.ctx_size, job_rx));
    let sidecar_bin = sidecar_path(args.stt_bin.as_deref());
    let (stt_child, stt_base_url, stt_backend_name) =
        start_sidecar(&sidecar_bin, &stt_path, &stt_backend).await?;
    let tts_path = resolve(
        &root,
        manifest
            .model
            .iter()
            .find(|entry| entry.name == "kokoro-v1.0")
            .ok_or("manifest has no kokoro entry")?,
    );
    let voices_path = resolve(
        &root,
        manifest
            .model
            .iter()
            .find(|entry| entry.name == "kokoro-voices-v1.0")
            .ok_or("manifest has no kokoro voices entry")?,
    );
    if !tts_path.exists() {
        return Err(format!("TTS file missing: {}", tts_path.display()));
    }
    if !voices_path.exists() {
        return Err(format!("TTS voices missing: {}", voices_path.display()));
    }
    let (tts_tx, tts_rx) = mpsc::channel();
    let tts_loader = std::thread::spawn(move || tts_loop(&tts_path, &voices_path, tts_rx));
    // Fail fast when a model itself cannot load (bad file, OOM).
    std::thread::sleep(std::time::Duration::from_secs(2));
    if loader.is_finished() {
        return Err("inference thread exited during load".to_owned());
    }
    if tts_loader.is_finished() {
        return Err("TTS thread exited during load".to_owned());
    }
    let state = Arc::new(AppState {
        inference: InferenceHandle { jobs: job_tx },
        tts: TtsHandle { jobs: tts_tx },
        stt: SttHandle {
            base_url: stt_base_url,
            client: reqwest::Client::new(),
            child: stt_child,
        },
        model_name: "qwen2.5:7b-instruct".to_owned(),
        model_status: status,
        backend: match detect_gpu() {
            openatc_platform::GpuBackend::Cuda => "cpu (cuda toolkit absent)",
            openatc_platform::GpuBackend::Metal => "cpu (metal pending)",
            openatc_platform::GpuBackend::Cpu => "cpu",
        }
        .to_owned(),
        stt_backend: stt_backend_name,
    });
    serve(state, args.host.clone(), args.port).await?;
    let _ = loader.join();
    let _ = tts_loader.join();
    Ok(())
}

/// Bind the router and serve until shutdown.
async fn serve(state: Arc<AppState>, host: String, port: u16) -> Result<(), String> {
    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/voices", get(list_voices))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/audio/transcriptions", post(transcriptions))
        .route("/v1/audio/speech", post(speech))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind((host.as_str(), port))
        .await
        .map_err(|error| error.to_string())?;
    eprintln!("openatc-ai: listening on {host}:{port}");
    axum::serve(listener, app)
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// Record the observed hash and size of a freshly fetched file.
fn record_lock(
    path: &std::path::Path,
    lockfile: &mut Lockfile,
    entry: &ModelEntry,
) -> Result<(), String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("cannot hash {}: {error}", path.display()))?;
    lockfile.model.insert(
        entry.name.clone(),
        LockedModel {
            sha256: sha256_hex(&bytes),
            size: bytes.len() as u64,
        },
    );
    Ok(())
}
