//! `openatc-ai`: one server, three embedded engines (LLM today, STT/TTS next).
//!
//! OpenAI-compatible surface the engine already speaks:
//! `POST /v1/chat/completions`, `GET /v1/voices`, `GET /health`.
//! Audio endpoints answer 501 until their slices land.
//!
//! Threading: the llama context is `!Send`, so all inference lives on one
//! dedicated thread. HTTP handlers post jobs to it and await oneshot replies;
//! only owned data (`String`, numbers) ever crosses threads.

mod voices;

use axum::{
    Json, Router,
    extract::State,
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
    /// Model directory override (default: platform data dir or `OPENATC_AI_MODELS`).
    #[arg(long)]
    models_dir: Option<PathBuf>,
    /// KV context size for the LLM.
    #[arg(long, default_value_t = 4096)]
    ctx_size: u32,
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
    completion_tokens: u32,
}

/// Cloneable handle to the inference thread.
#[derive(Clone)]
struct InferenceHandle {
    jobs: mpsc::Sender<InferenceJob>,
}

/// Shared server state (all fields are Send).
struct AppState {
    inference: InferenceHandle,
    model_name: String,
    model_status: BTreeMap<String, String>,
    backend: String,
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
            .add(*token, index as i32, &[0], index + 1 == tokens.len())
            .map_err(|error| format!("batch overflow: {error:?}"))?;
    }
    context
        .decode(&mut batch)
        .map_err(|error| format!("decode failed: {error:?}"))?;
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.subsec_nanos())
        .unwrap_or(42);
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
        prompt_tokens: tokens.len() as u32,
        completion_tokens: generated,
    })
}

/// Inference thread entry: loads everything once, then serves jobs forever.
fn inference_loop(
    llm_path: PathBuf,
    ctx_size: u32,
    jobs: mpsc::Receiver<InferenceJob>,
) -> Result<(), String> {
    let backend =
        LlamaBackend::init().map_err(|error| format!("backend init failed: {error:?}"))?;
    let model = LlamaModel::load_from_file(&backend, &llm_path, &LlamaModelParams::default())
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

/// Token usage counts.
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
        .map(|time| time.as_secs())
        .unwrap_or(0);
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
            completion_tokens: completion.completion_tokens,
            total_tokens: completion.prompt_tokens + completion.completion_tokens,
        },
    }))
}

/// Liveness plus per-model file status (recorded at startup).
async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "backend": state.backend,
        "models": state.model_status,
    }))
}

/// Kokoro v1 voice roster (replaced by runtime query in the TTS slice).
async fn list_voices() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "voices": voices::KOKORO_V1 }))
}

/// Placeholder until the STT slice lands.
async fn transcriptions() -> (StatusCode, Json<ErrorBody>) {
    api_error(
        StatusCode::NOT_IMPLEMENTED,
        "transcriptions arrive with the STT slice; use the gateway meanwhile".to_owned(),
    )
}

/// Placeholder until the TTS slice lands.
async fn speech() -> (StatusCode, Json<ErrorBody>) {
    api_error(
        StatusCode::NOT_IMPLEMENTED,
        "speech arrives with the TTS slice; use the gateway meanwhile".to_owned(),
    )
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

#[tokio::main]
async fn main() -> Result<(), String> {
    let args = Args::parse();
    let root = args.models_dir.clone().unwrap_or_else(models_dir);
    std::fs::create_dir_all(&root).map_err(|error| format!("cannot create {root:?}: {error}"))?;
    let manifest =
        Manifest::parse(include_str!("../../../models.toml")).map_err(|error| error.to_string())?;
    let lock_path = root.join("models.lock.toml");
    let mut lockfile: Lockfile = std::fs::read_to_string(&lock_path)
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
    let (job_tx, job_rx) = mpsc::channel();
    let loader = std::thread::spawn(move || inference_loop(llm_path, args.ctx_size, job_rx));
    // Fail fast when the model itself cannot load (bad file, OOM).
    std::thread::sleep(std::time::Duration::from_secs(2));
    if loader.is_finished() {
        return Err("inference thread exited during load".to_owned());
    }
    let state = Arc::new(AppState {
        inference: InferenceHandle { jobs: job_tx },
        model_name: "qwen2.5:7b-instruct".to_owned(),
        model_status: status,
        backend: match detect_gpu() {
            openatc_platform::GpuBackend::Cuda => "cpu (cuda toolkit absent)",
            openatc_platform::GpuBackend::Metal => "cpu (metal pending)",
            openatc_platform::GpuBackend::Cpu => "cpu",
        }
        .to_owned(),
    });
    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/voices", get(list_voices))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/audio/transcriptions", post(transcriptions))
        .route("/v1/audio/speech", post(speech))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", args.port))
        .await
        .map_err(|error| error.to_string())?;
    eprintln!("openatc-ai: listening on 127.0.0.1:{}", args.port);
    axum::serve(listener, app)
        .await
        .map_err(|error| error.to_string())?;
    let _ = loader.join();
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
