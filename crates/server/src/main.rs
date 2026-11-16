//! `openatc-ai`: one server, three embedded engines (LLM + STT + TTS live).
//!
//! OpenAI-compatible surface the engine already speaks:
//! `POST /v1/chat/completions`, `GET /v1/voices`, `GET /health`,
//! `POST /v1/audio/transcriptions`, `POST /v1/audio/speech`.
//!
//! Threading: the llama context is `!Send`, so all inference lives on one
//! dedicated thread. HTTP handlers post jobs to it and await oneshot replies;
//! only owned data (`String`, numbers) ever crosses threads.

/// Product version reported by `/health`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

mod compute;
mod config;
mod json_mode;
mod manager;
mod progress;
mod resources;
mod setup;
mod synthesis;

mod llm_config;
use axum::{
    Json, Router,
    extract::{Multipart, State},
    http::StatusCode,
    routing::{get, post},
};
use clap::{CommandFactory, Parser};
use llama_cpp_2::{
    context::params::LlamaContextParams, llama_backend::LlamaBackend, llama_batch::LlamaBatch,
    model::LlamaModel, model::params::LlamaModelParams, sampling::LlamaSampler,
};
use openatc_ai_core::{LockedModel, Lockfile, Manifest, ModelEntry, ensure, resolve, sha256_hex};
use openatc_platform::models_dir;
use openatc_tts::fx::Fx;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::oneshot;

#[derive(Parser)]
struct Args {
    /// Saved TOML settings; explicit command-line options take precedence.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Create or edit saved settings in the terminal, then exit.
    #[arg(long)]
    configure: bool,
    /// Open a local browser settings editor without loading inference models.
    #[arg(long)]
    configure_web: bool,
    /// Open the local setup wizard and server dashboard.
    #[arg(long)]
    manage_web: bool,
    /// Keep the dashboard available without opening a browser (headless servers).
    #[arg(long)]
    no_browser: bool,
    /// Fixed loopback dashboard port for a headless service (zero chooses a free port).
    #[arg(long, default_value_t = 0)]
    dashboard_port: u16,
    /// Dashboard interface. Use this server’s LAN address for trusted remote access.
    #[arg(long, default_value = "127.0.0.1")]
    dashboard_host: std::net::IpAddr,
    #[arg(long, hide = true)]
    managed_events: bool,
    /// Leave the language model unloaded until enabled in the dashboard.
    #[arg(long)]
    disable_llm: bool,
    /// Leave speech recognition unloaded.
    #[arg(long)]
    disable_stt: bool,
    /// Leave speech synthesis unloaded.
    #[arg(long)]
    disable_tts: bool,
    /// Editable TOML schema for the setup interfaces.
    #[arg(long)]
    settings_schema: Option<PathBuf>,
    #[command(flatten)]
    cpu: compute::CpuOptions,
    /// Local speech model. Kokoro is the default; Kitten uses Mini 0.8.
    #[arg(long, value_enum, default_value_t = synthesis::Provider::Kokoro)]
    tts_provider: synthesis::Provider,
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
    #[arg(long, default_value_t = 4096, value_parser = clap::value_parser!(u32).range(1024..=1048576))]
    ctx_size: u32,
    /// CPU threads used for token generation (omit to use the backend default).
    #[arg(long, value_parser = clap::value_parser!(i32).range(1..=128))]
    threads: Option<i32>,
    /// CPU threads used for prompt processing.
    #[arg(long, value_parser = clap::value_parser!(i32).range(1..=128))]
    batch_threads: Option<i32>,
    /// Choose the largest bounded GPU context while retaining VRAM headroom.
    #[arg(long, conflicts_with = "ctx_size")]
    ctx_auto: bool,
    /// Maximum context attempted by automatic sizing.
    #[arg(long, default_value_t = 32768, value_parser = clap::value_parser!(u32).range(2048..=1048576))]
    ctx_max: u32,
    /// Free VRAM retained after model/context allocation.
    #[arg(long, default_value_t = 1536)]
    vram_reserve_mib: u64,
    /// Language model; speech providers are unchanged.
    #[arg(long, value_enum, default_value_t = llm_config::Model::Qwen25)]
    llm_model: llm_config::Model,
    /// TOML manifest containing one custom GGUF language model.
    #[arg(long)]
    llm_manifest: Option<PathBuf>,
    /// Optional custom GGUF download, with an explicit size and checksum.
    #[arg(long, conflicts_with = "llm_manifest")]
    llm_url: Option<String>,
    #[arg(long)]
    llm_name: Option<String>,
    #[arg(long)]
    llm_file: Option<String>,
    #[arg(long)]
    llm_sha256: Option<String>,
    #[arg(long)]
    llm_bytes: Option<u64>,
    /// Chat template override for a custom model; otherwise use its GGUF metadata.
    #[arg(long)]
    chat_template: Option<String>,
    /// Optional model-specific assistant prefix, such as a closed thinking block.
    #[arg(long)]
    assistant_prefix: Option<String>,
    /// STT compute: auto prefers the compiled GPU backend and falls back to CPU.
    #[arg(long, default_value = "auto")]
    stt_backend: String,
    /// `openatc-stt` binary override (default: beside this binary, else PATH).
    #[arg(long)]
    stt_bin: Option<PathBuf>,
}

/// One inference request crossing into the llama thread.
struct InferenceJob {
    tokenize_only: bool,
    grammar: Option<String>,
    json: bool,
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

/// Start the sidecar and wait for `/health`.
/// Returns its base URL and backend; a sidecar that exits while loading the
/// model is reported immediately.
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
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("STT sidecar exited during load ({status})"));
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

/// Handle to the TTS worker thread. The ONNX session stays on that thread.
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
    inference: Option<InferenceHandle>,
    stt: Option<SttHandle>,
    tts: Option<TtsHandle>,
    model_name: String,
    model_status: BTreeMap<String, String>,
    backend: String,
    context_tokens: u32,
    stt_backend: String,
    llm_device: compute::DeviceStatus,
    tts_status: synthesis::Status,
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
    response_format: Option<serde_json::Value>,
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
    json: bool,
    direct_reply: bool,
    native_template: bool,
    template_override: Option<&str>,
    assistant_prefix: Option<&str>,
    tokenize_only: bool,
    custom_grammar: Option<&str>,
) -> Result<Completion, String> {
    let mut prompt = if native_template || template_override.is_some() {
        let template = match template_override {
            Some(value) => llama_cpp_2::model::LlamaChatTemplate::new(value)
                .map_err(|error| error.to_string())?,
            None => model.chat_template(None).map_err(|error| {
                format!("custom model chat template: {error:?}; set --chat-template explicitly")
            })?,
        };
        let messages = messages
            .iter()
            .map(|message| {
                llama_cpp_2::model::LlamaChatMessage::new(
                    message.role.clone(),
                    message.content.clone(),
                )
                .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        model
            .apply_chat_template(&template, &messages, true)
            .map_err(|error| format!("chat template failed: {error:?}"))?
    } else {
        chatml_prompt(messages)
    };
    if let Some(prefix) = assistant_prefix {
        prompt.push_str(prefix);
    } else if direct_reply && !native_template {
        prompt.push_str("<think>\n\n</think>\n\n");
    }
    let vocab = model.vocab();
    let tokens = vocab.tokenize(prompt.as_bytes(), true, true);
    if tokenize_only {
        return Ok(Completion {
            text: String::new(),
            prompt_tokens: u32::try_from(tokens.len()).unwrap_or(u32::MAX),
            output_tokens: 0,
        });
    }
    context.clear_kv_cache();
    let max_new = max_new.clamp(1, 1024) as usize;
    if tokens.len() + max_new + 8 >= n_ctx as usize {
        return Err("prompt plus max_tokens exceeds context size".to_owned());
    }
    let mut batch = LlamaBatch::new(512, 1);
    for (chunk_index, chunk) in tokens.chunks(512).enumerate() {
        batch.clear();
        for (offset, token) in chunk.iter().enumerate() {
            let index = chunk_index * 512 + offset;
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
    }
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(42, |time| time.subsec_nanos());
    let sampling = if temperature <= 0.0 {
        LlamaSampler::greedy()
    } else {
        LlamaSampler::chain_simple([
            LlamaSampler::top_k(40),
            LlamaSampler::top_p(0.9, 1),
            LlamaSampler::temp(temperature),
            LlamaSampler::dist(seed),
        ])
    };
    let mut sampler = if json {
        let grammar =
            LlamaSampler::grammar(model, custom_grammar.unwrap_or(json_mode::GRAMMAR), "root")
                .map_err(|e| format!("JSON grammar: {e}"))?;
        LlamaSampler::chain_simple([grammar, sampling])
    } else {
        sampling
    };
    let mut output = Vec::new();
    let mut generated = 0u32;
    // Token counts stay far below i32 range in practice.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    for position in (tokens.len() as i32..).take(max_new) {
        let token = sampler.sample(context, -1);
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
    let text = String::from_utf8_lossy(&output).into_owned();
    if json && !serde_json::from_str::<serde_json::Value>(&text).is_ok_and(|v| v.is_object()) {
        return Err("Model did not finish its JSON object within max_tokens".into());
    }
    Ok(Completion {
        text,
        prompt_tokens: u32::try_from(tokens.len()).unwrap_or(u32::MAX),
        output_tokens: generated,
    })
}

/// Inference thread entry: loads everything once, then serves jobs forever.
fn inference_loop(
    llm_path: &std::path::Path,
    ctx_size: u32,
    jobs: &mpsc::Receiver<InferenceJob>,
    force_cpu: bool,
    ready: &mpsc::Sender<Result<(compute::DeviceStatus, u32), String>>,
    automatic: bool,
    maximum: u32,
    reserve_mib: u64,
    model_choice: llm_config::Model,
    threads: Option<i32>,
    batch_threads: Option<i32>,
    native_template: bool,
    template_override: Option<String>,
    assistant_prefix: Option<String>,
) -> Result<(), String> {
    let backend =
        LlamaBackend::init().map_err(|error| format!("backend init failed: {error:?}"))?;
    let gpu = !force_cpu && backend.supports_gpu_offload();
    let mut device = compute::DeviceStatus::new(
        if gpu { "gpu-requested" } else { "cpu" },
        if force_cpu {
            "Forced by option"
        } else if gpu {
            "All model layers requested on GPU; startup log identifies actual devices"
        } else {
            "No GPU backend available in this build"
        },
    );
    for use_gpu in [true, false].into_iter().filter(|enabled| !*enabled || gpu) {
        let model = match LlamaModel::load_from_file(
            &backend,
            llm_path,
            &LlamaModelParams::default().with_n_gpu_layers(if use_gpu { u32::MAX } else { 0 }),
        ) {
            Ok(model) => model,
            Err(error) if use_gpu => {
                device =
                    compute::DeviceStatus::new("cpu", format!("GPU model load failed: {error:?}"));
                continue;
            }
            Err(error) => {
                let message = format!("model load failed: {error:?}");
                let _ = ready.send(Err(message.clone()));
                return Err(message);
            }
        };
        let automatic = automatic && use_gpu && llm_config::free_vram_mib().is_some();
        let mut selected = None;
        for size in llm_config::candidates(ctx_size, automatic, maximum) {
            let mut params = LlamaContextParams::default()
                .with_n_ctx(std::num::NonZeroU32::new(size))
                .with_n_batch(512)
                .with_offload_kqv(use_gpu)
                .with_op_offload(use_gpu);
            if let Some(threads) = threads {
                params = params.with_n_threads(threads);
            }
            if let Some(threads) = batch_threads {
                params = params.with_n_threads_batch(threads);
            }
            eprintln!(
                "LLM context={size} generation_threads={} batch_threads={}",
                params.n_threads(),
                params.n_threads_batch()
            );
            match model.new_context(&backend, params) {
                Ok(context) => {
                    if automatic
                        && llm_config::free_vram_mib().is_none_or(|free| free < reserve_mib)
                    {
                        drop(context);
                        continue;
                    }
                    selected = Some((context, size));
                    break;
                }
                Err(error) => {
                    eprintln!("Context {size} could not be allocated: {error:?}");
                }
            }
        }
        let Some((mut context, selected_size)) = selected else {
            if use_gpu {
                device = compute::DeviceStatus::new(
                    "cpu",
                    "GPU context allocation could not retain requested headroom",
                );
                continue;
            }
            let message = "No supported context could be allocated".to_owned();
            let _ = ready.send(Err(message.clone()));
            return Err(message);
        };
        context.print_memory_breakdown();
        let _ = ready.send(Ok((device, selected_size)));
        for job in jobs {
            let result = complete(
                &model,
                &mut context,
                selected_size,
                &job.messages,
                job.temperature,
                job.max_tokens,
                job.json,
                model_choice.direct_reply(),
                native_template,
                template_override.as_deref(),
                assistant_prefix.as_deref(),
                job.tokenize_only,
                job.grammar.as_deref(),
            );
            let _ = job.reply.send(result);
        }
        return Ok(());
    }
    unreachable!("CPU initialization is always attempted")
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
    let grammar = if let Some(schema) = request
        .response_format
        .as_ref()
        .and_then(|format| format.pointer("/json_schema/schema"))
    {
        let schema = schema.to_string();
        if schema.len() > 32768 {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "JSON schema exceeds 32 KiB".into(),
            ));
        }
        Some(
            llama_cpp_2::json_schema_to_grammar(&schema)
                .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?,
        )
    } else {
        None
    };
    let job = InferenceJob {
        tokenize_only: false,
        grammar,
        json: json_mode::enabled(request.response_format.as_ref())
            .map_err(|e| api_error(StatusCode::BAD_REQUEST, e))?,
        messages: request.messages,
        temperature: request.temperature,
        max_tokens,
        reply: reply_tx,
    };
    state
        .inference
        .as_ref()
        .ok_or_else(|| unloaded("Language model"))?
        .jobs
        .send(job)
        .map_err(|_| {
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

/// Count the exact rendered prompt with the loaded model's tokenizer.
async fn tokenize(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ChatRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorBody>)> {
    if request.messages.is_empty()
        || request.messages.len() > 64
        || request
            .messages
            .iter()
            .map(|message| message.content.len())
            .sum::<usize>()
            > 262144
    {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Prompt must contain 1–64 messages and at most 256 KiB".into(),
        ));
    }
    let reserve = request.max_tokens.unwrap_or(512).clamp(1, 1024);
    let (reply, result) = oneshot::channel();
    state
        .inference
        .as_ref()
        .ok_or_else(|| unloaded("Language model"))?
        .jobs
        .send(InferenceJob {
            tokenize_only: true,
            grammar: None,
            json: false,
            messages: request.messages,
            temperature: 0.0,
            max_tokens: reserve,
            reply,
        })
        .map_err(|error| api_error(StatusCode::SERVICE_UNAVAILABLE, error.to_string()))?;
    let completion = result
        .await
        .map_err(|error| api_error(StatusCode::SERVICE_UNAVAILABLE, error.to_string()))?
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error))?;
    Ok(Json(
        serde_json::json!({"promptTokens":completion.prompt_tokens,"contextTokens":state.context_tokens,
        "reservedOutputTokens":reserve,"fits":completion.prompt_tokens.saturating_add(reserve).saturating_add(8) < state.context_tokens}),
    ))
}

/// Liveness plus per-model file status (recorded at startup).
async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "version": VERSION,
        "components":{"llm":state.inference.is_some(),"stt":state.stt.is_some(),"tts":state.tts.is_some()},
        "backend": state.backend,
        "stt": state.stt_backend,
        "compute": {"llm": state.llm_device, "stt": state.stt_backend, "tts": state.tts_status.device},
        "ttsProvider": state.tts_status.provider,
        "models": state.model_status,
        "llm": {"model":state.model_name,"contextTokens":state.context_tokens,"maxOutputTokens":1024},
    }))
}

/// Voices in the selected model's loaded table.
async fn list_voices(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorBody>)> {
    if state.tts.is_none() {
        return Err(unloaded("Speech synthesis"));
    }
    Ok(Json(
        serde_json::json!({ "provider": state.tts_status.provider, "voices": state.tts_status.voices }),
    ))
}
fn unloaded(component: &str) -> (StatusCode, Json<ErrorBody>) {
    api_error(
        StatusCode::SERVICE_UNAVAILABLE,
        format!("{component} is unloaded. Load it in the server dashboard."),
    )
}

/// Speech-to-text: multipart `file` (16 kHz mono WAV) plus ignored `model`.
/// Returns `{"text"}`.
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
    let stt = state
        .stt
        .as_ref()
        .ok_or_else(|| unloaded("Speech recognition"))?;
    let response = stt
        .client
        .post(format!("{}/transcribe", stt.base_url))
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
/// Request validation matches the endpoint it replaces.
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
        .as_ref()
        .ok_or_else(|| unloaded("Speech synthesis"))?
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
        progress::emit(
            serde_json::json!({"stage":"verifying","model":entry.name,"path":root.join(&entry.path),"kind":entry.path.split('/').next()}),
        );
        eprintln!("openatc-ai: ensuring {} ...", entry.name);
        match ensure(root, entry, locked.as_ref(), |done, total| {
            progress::emit(
                serde_json::json!({"stage":"downloading","model":entry.name,"done":done,"total":total}),
            );
            if total > 0 {
                eprint!("\ropenatc-ai: {} {done}/{total} bytes", entry.name);
            }
        }) {
            Ok(path) => {
                progress::emit(
                    serde_json::json!({"stage":"verified","model":entry.name,"path":path,"kind":entry.path.split('/').next()}),
                );
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
                progress::emit(
                    serde_json::json!({"stage":"error","model":entry.name,"error":error.to_string()}),
                );
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
    provider: synthesis::Provider,
    llm: llm_config::Model,
    custom: Option<Manifest>,
    disabled: Vec<&'static str>,
) -> Result<(std::path::PathBuf, Manifest, BTreeMap<String, String>), String> {
    std::fs::create_dir_all(&root)
        .map_err(|error| format!("cannot create {}: {error}", root.display()))?;
    let mut manifest =
        Manifest::parse(include_str!("../../../models.toml")).map_err(|error| error.to_string())?;
    if provider == synthesis::Provider::Kitten {
        manifest
            .model
            .retain(|entry| !entry.path.starts_with("tts/"));
        manifest.model.extend(
            Manifest::parse(include_str!("../../../models-kitten.toml"))
                .map_err(|error| error.to_string())?
                .model,
        );
    }
    manifest
        .model
        .retain(|entry| !entry.path.starts_with("llm/") || entry.name == llm.manifest_name());
    if let Some(custom) = custom {
        if custom.model.iter().any(|model| {
            manifest
                .model
                .iter()
                .any(|existing| !existing.path.starts_with("llm/") && existing.name == model.name)
        }) {
            return Err("Custom language model name collides with a speech model".into());
        }
        manifest
            .model
            .retain(|entry| !entry.path.starts_with("llm/"));
        manifest.model.extend(custom.model);
    }
    let lock_path = root.join("models.lock.toml");
    let lockfile: Lockfile = std::fs::read_to_string(&lock_path)
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default();
    let mut manifest_clone = manifest.clone();
    manifest_clone.model.retain(|entry| {
        !disabled
            .iter()
            .any(|kind| entry.path.starts_with(&format!("{kind}/")))
    });
    let root_clone = root.clone();
    let (mut status, lock_changed, lockfile) = tokio::task::spawn_blocking(move || {
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
    for entry in &manifest.model {
        status
            .entry(entry.name.clone())
            .or_insert_with(|| "unloaded".into());
    }
    Ok((root, manifest, status))
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let command_line: Vec<_> = std::env::args_os().collect();
    let initial = Args::parse_from(&command_line);
    let config_path = initial
        .config
        .clone()
        .unwrap_or_else(|| openatc_platform::state_dir().join("server.toml"));
    if initial.configure_web || initial.manage_web {
        return manager::run(
            &config_path,
            initial.settings_schema.as_deref(),
            initial.manage_web,
            !initial.no_browser,
            initial.dashboard_port,
            initial.dashboard_host,
        )
        .await;
    }
    if initial.configure {
        return setup::terminal(&config_path, initial.settings_schema.as_deref());
    }
    progress::enable(initial.managed_events);
    if initial.managed_events {
        // A closed manager pipe means its runtime must not be left running invisibly.
        std::thread::spawn(|| {
            use std::io::Read;
            let mut byte = [0u8; 1];
            while std::io::stdin().read(&mut byte).is_ok_and(|n| n > 0) {}
            resources::kill_descendants(std::process::id());
            std::process::exit(0);
        });
    }
    use std::io::IsTerminal;
    if !config_path.exists() && std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        setup::terminal(&config_path, initial.settings_schema.as_deref())?;
    }
    let args = match std::fs::read_to_string(&config_path) {
        Ok(source) => Args::parse_from(
            config::arguments(&source, &command_line, &Args::command()).unwrap_or_else(|error| {
                eprintln!("{}: {error}", config_path.display());
                std::process::exit(2);
            }),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && initial.config.is_none() => {
            initial
        }
        Err(error) => {
            eprintln!("cannot read {}: {error}", config_path.display());
            std::process::exit(2);
        }
    };
    let root = args.models_dir.clone().unwrap_or_else(models_dir);
    let custom = llm_config::configured_manifest(&args)?;
    let custom_template = custom.is_some();
    let (root, manifest, status) = ensure_models(
        root,
        args.tts_provider,
        args.llm_model,
        custom,
        [
            ("llm", args.disable_llm),
            ("stt", args.disable_stt),
            ("tts", args.disable_tts),
        ]
        .into_iter()
        .filter_map(|(kind, disabled)| disabled.then_some(kind))
        .collect(),
    )
    .await?;
    let model_name = manifest
        .model
        .iter()
        .find(|entry| entry.path.starts_with("llm/"))
        .ok_or("manifest has no language model")?
        .name
        .clone();
    let llm_path = resolve(
        &root,
        manifest
            .model
            .iter()
            .find(|entry| entry.path.starts_with("llm/"))
            .ok_or("manifest has no llm entry")?,
    );
    if !args.disable_llm && !llm_path.exists() {
        return Err(format!("LLM file missing: {}", llm_path.display()));
    }
    let stt_backend = match if args.cpu.stt() {
        "cpu"
    } else {
        args.stt_backend.as_str()
    } {
        value @ ("auto" | "cpu" | "cuda") => value.to_owned(),
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
    if !args.disable_stt && !stt_path.exists() {
        return Err(format!("STT file missing: {}", stt_path.display()));
    }
    let stt = if args.disable_stt {
        None
    } else {
        progress::emit(serde_json::json!({"stage":"loading","component":"stt"}));
        let (child, base_url, backend) = start_sidecar(
            &sidecar_path(args.stt_bin.as_deref()),
            &stt_path,
            &stt_backend,
        )
        .await?;
        Some((
            SttHandle {
                child,
                base_url,
                client: reqwest::Client::new(),
            },
            backend,
        ))
    };
    let (tts_tx, tts_rx) = mpsc::channel();
    let mut tts_loader = None;
    let tts_status = if args.disable_tts {
        synthesis::Status {
            provider: args.tts_provider.name().into(),
            device: compute::DeviceStatus::new("unloaded", "Speech synthesis is unloaded"),
            voices: serde_json::json!([]),
        }
    } else {
        progress::emit(serde_json::json!({"stage":"loading","component":"tts"}));
        let (ready_tx, ready_rx) = mpsc::channel();
        let config = synthesis::Config {
            provider: args.tts_provider,
            force_cpu: args.cpu.tts(),
            root,
        };
        tts_loader = Some(std::thread::spawn(move || {
            synthesis::run(&config, tts_rx, &ready_tx)
        }));
        tokio::task::spawn_blocking(move || {
            ready_rx.recv_timeout(std::time::Duration::from_secs(300))
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("TTS startup: {e}"))??
    };
    let (job_tx, job_rx) = mpsc::channel();
    let mut loader = None;
    let (llm_device, context_tokens) = if args.disable_llm {
        (
            compute::DeviceStatus::new("unloaded", "Language model is unloaded"),
            0,
        )
    } else {
        progress::emit(serde_json::json!({"stage":"loading","component":"llm"}));
        let (ready_tx, ready_rx) = mpsc::channel();
        let force_cpu = args.cpu.llm();
        loader = Some(std::thread::spawn(move || {
            inference_loop(
                &llm_path,
                args.ctx_size,
                &job_rx,
                force_cpu,
                &ready_tx,
                args.ctx_auto,
                args.ctx_max,
                args.vram_reserve_mib,
                args.llm_model,
                args.threads,
                args.batch_threads,
                custom_template,
                args.chat_template,
                args.assistant_prefix,
            )
        }));
        tokio::task::spawn_blocking(move || {
            ready_rx.recv_timeout(std::time::Duration::from_secs(300))
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("LLM startup: {e}"))??
    };
    let (stt, stt_backend) = match stt {
        Some((handle, backend)) => (Some(handle), backend),
        None => (None, "unloaded".into()),
    };
    let state = Arc::new(AppState {
        inference: (!args.disable_llm).then_some(InferenceHandle { jobs: job_tx }),
        tts: (!args.disable_tts).then_some(TtsHandle { jobs: tts_tx }),
        stt,
        model_name,
        context_tokens,
        model_status: status,
        backend: llm_device.device.clone(),
        stt_backend,
        llm_device,
        tts_status,
    });
    serve(state, args.host.clone(), args.port).await?;
    if let Some(loader) = loader {
        let _ = loader.join();
    }
    if let Some(loader) = tts_loader {
        let _ = loader.join();
    }
    Ok(())
}

/// Bind the router and serve until shutdown.
async fn serve(state: Arc<AppState>, host: String, port: u16) -> Result<(), String> {
    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/voices", get(list_voices))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/tokenize", post(tokenize))
        .route("/v1/audio/transcriptions", post(transcriptions))
        .route("/v1/audio/speech", post(speech))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind((host.as_str(), port))
        .await
        .map_err(|error| error.to_string())?;
    eprintln!("openatc-ai: listening on {host}:{port}");
    progress::emit(
        serde_json::json!({"stage":"ready","health":health(State(state.clone())).await.0,"port":port}),
    );
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
