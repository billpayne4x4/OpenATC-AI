//! Companion engine for controller state, local HTTP requests and AI/STT/TTS services.

mod lifecycle;
mod radio;
mod routes;
mod support;

use openatc_core::airport::Airport;
use openatc_core::regions::{Region, load_regions};
use openatc_core::state::{Controllers, State};
use openatc_settings::Settings;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Product version.
pub const VERSION: &str = "0.2.1";

/// Everything the engine owns: session state plus configuration.
pub struct EngineState {
    /// Live session (plan, telemetry, phase, transcript, ...).
    pub session: State,
    /// Operator settings.
    pub settings: Settings,
    /// Cached published stations.
    pub stations: Vec<openatc_core::stations::Station>,
    /// Root used by the station index.
    pub station_root: String,
    /// Latest simulator airport weather, timestamped at receipt.
    /// Information letter advances only when the rounded airport observation changes.
    pub atis_information: BTreeMap<String, (String, usize)>,
    /// Surface observations received from X-Plane, with freshness timestamps.
    pub sim_weather: BTreeMap<String, (serde_json::Value, std::time::Instant)>,
    /// Loaded airport, if any.
    pub airport: Option<Airport>,
    /// Remembered controller voices.
    pub controllers: Controllers,
    /// Role prompts.
    pub prompts: support::Prompts,
    /// Region table from file, empty when built-in.
    pub regions: BTreeMap<String, Region>,
    /// Regions file path, empty when built-in.
    pub regions_path: String,
    /// Speech example pool for AI few-shot prompts, empty when missing.
    pub speech: openatc_core::speech::SpeechPool,
    /// Speech dir path, empty when missing.
    pub speech_source: String,
    /// Config directory holding settings/controllers/session JSON.
    pub config_dir: PathBuf,
    /// Handoff and clearance expectations awaiting pilot compliance.
    pub expectations: Vec<openatc_core::compliance::Expectation>,
    /// Shared HTTP client for AI/STT/TTS upstreams.
    pub client: reqwest::Client,
}

/// Engine state shared across handlers and the weather watch.
pub type Shared = Arc<RwLock<EngineState>>;

impl EngineState {
    fn prompts_text(&self, attendant: bool) -> String {
        if attendant {
            self.prompts.cabin.clone()
        } else {
            self.prompts.ground.clone()
        }
    }

    fn classify_prompt(&self) -> String {
        self.prompts.classify.clone()
    }

    fn copilot_chat_prompt(&self) -> String {
        self.prompts.copilot_chat.clone()
    }
}

/// Resolve the config dir: env override, then XDG, then home fallback.
fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("OPENATC_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(dir).join("openatc");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".config/openatc");
    }
    PathBuf::from("openatc-config")
}

/// Load settings.json when present, otherwise start from defaults.
fn load_settings(dir: &std::path::Path) -> Settings {
    let path = dir.join("settings.json");
    if let Ok(text) = std::fs::read_to_string(&path)
        && let Ok(settings) = serde_json::from_str(&text)
    {
        return settings;
    }
    Settings::default()
}

fn mtime(path: &std::path::Path) -> std::time::SystemTime {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .unwrap_or(std::time::UNIX_EPOCH)
}

/// Settings hot-reload; live radio weather comes exclusively from the plugin.
async fn weather_loop(shared: Shared) {
    let mut settings_stamp = {
        let state = shared.read().await;
        mtime(&state.config_dir.join("settings.json"))
    };
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        reload_settings_if_changed(&shared, &mut settings_stamp).await;
        // Weather transmissions now use simulator observations only. Internet
        // METAR fetches remain available as explicitly labelled planning data.
    }
}

/// Reload changed personal settings without blocking provider calls.
async fn reload_settings_if_changed(shared: &Shared, stamp: &mut std::time::SystemTime) {
    let state = shared.read().await;
    if mtime(&state.config_dir.join("settings.json")) == *stamp {
        return;
    }
    let path = state.config_dir.join("settings.json");
    if let Ok(text) = std::fs::read_to_string(&path)
        && let Ok(fresh) = serde_json::from_str::<Settings>(&text)
        && fresh.validate().is_ok()
    {
        drop(state);
        shared.write().await.settings = fresh;
    }
    *stamp = mtime(&path);
}

#[tokio::main]
async fn main() {
    use axum::routing::{get, post};
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|arg| arg == "--check-speech") {
        let Some(path) = args.get(2) else {
            eprintln!("Usage: open-atc-engine --check-speech /path/to/speech");
            std::process::exit(2);
        };
        if let Err(error) = openatc_core::dialogue::validate(
            &std::path::Path::new(path).join("runtime/responses.toml"),
        ) {
            eprintln!("Operational speech rejected: {error}");
            std::process::exit(1);
        }
        match openatc_core::speech::load_speech_dir(std::path::Path::new(path)) {
            Ok(pool) => {
                let samples = openatc_core::speech::sample_slots();
                let mut phrases = 0;
                for (id, entry) in pool.iter() {
                    for line in entry.say.iter().chain([&entry.accept, &entry.decline]) {
                        if line.is_empty() {
                            continue;
                        }
                        if let Err(error) = openatc_core::speech::render_checked(line, &samples) {
                            eprintln!("{id}: {error}");
                            std::process::exit(1);
                        }
                    }
                    phrases += entry.say.len();
                }
                println!(
                    "Speech library valid: {} situations, {phrases} phrases",
                    pool.len()
                );
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(8087);
    let plugin_parent = std::env::args().find_map(|arg| {
        arg.strip_prefix("--plugin-parent-pid=")
            .and_then(|pid| pid.parse::<u32>().ok())
    });
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(PathBuf::from))
        .unwrap_or_default();
    let config_dir = config_dir();
    let mut settings = load_settings(&config_dir);
    if settings.validate().is_err() {
        eprintln!("Settings invalid, starting from defaults");
        settings = Settings::default();
    }
    if let Ok(url) = std::env::var("OPENATC_AI_URL") {
        settings.ai_url = url;
        settings.ai_enabled = true;
    }
    if let Ok(model) = std::env::var("OPENATC_AI_MODEL") {
        settings.ai_model = model;
    }
    let mut controllers = Controllers::default();
    let controllers_path = config_dir.join("controllers.json");
    if let Ok(text) = std::fs::read_to_string(&controllers_path)
        && let Ok(saved) = serde_json::from_str(&text)
    {
        controllers = saved;
    }
    let prompts = support::load_prompts(&config_dir, &exe_dir);
    let regions_path = support::resolve_regions_file(&config_dir, &exe_dir);
    let regions = if regions_path.is_empty() {
        BTreeMap::new()
    } else {
        load_regions(std::path::Path::new(&regions_path)).unwrap_or_default()
    };
    let speech_source = support::resolve_speech_dir(&config_dir, &exe_dir);
    if !speech_source.is_empty()
        && let Err(error) = openatc_core::dialogue::load(
            &std::path::Path::new(&speech_source).join("runtime/responses.toml"),
        )
    {
        eprintln!("Operational speech rejected: {error}");
        std::process::exit(1);
    }
    let speech = if speech_source.is_empty() {
        openatc_core::speech::SpeechPool::default()
    } else {
        match openatc_core::speech::load_speech_dir(std::path::Path::new(&speech_source)) {
            Ok(pool) => pool,
            Err(error) => {
                eprintln!("Speech library rejected: {error}");
                openatc_core::speech::SpeechPool::default()
            }
        }
    };
    let shared: Shared = Arc::new(RwLock::new(EngineState {
        session: State::default(),
        settings,
        stations: Vec::new(),
        station_root: String::new(),
        sim_weather: BTreeMap::new(),
        atis_information: BTreeMap::new(),
        airport: None,
        controllers,
        prompts,
        regions,
        regions_path,
        speech,
        speech_source,
        config_dir,
        expectations: Vec::new(),
        client: reqwest::Client::new(),
    }));
    let weather = shared.clone();
    tokio::spawn(async move { weather_loop(weather).await });
    let lifetime = Arc::new(lifecycle::PluginSession::new(plugin_parent));
    let app = axum::Router::new()
        .route("/health", get(routes::health))
        .route("/voice-health", get(routes::voice_health))
        .route("/state", get(routes::full_state))
        .route("/settings", post(routes::post_settings))
        .route("/simulator/root", post(routes::post_simulator_root))
        .route("/request", post(routes::post_request))
        .route("/request/auto-reply", post(routes::post_auto_reply))
        .route("/plan", post(routes::post_plan))
        .route("/plan/parking", post(routes::post_parking))
        .route("/simbrief", post(routes::post_simbrief))
        .route("/airport/load", post(routes::post_airport_load))
        .route("/airport/local", post(routes::post_airport_local))
        .route("/airport/arrival", post(routes::post_airport_arrival))
        .route("/stations/nearby", post(radio::nearby))
        .route("/weather/simulator", post(radio::weather))
        .route("/atis", post(radio::atis))
        .route("/weather", post(routes::post_weather))
        .route("/weather/online", post(routes::post_online_weather))
        .route("/speech/transcribe", post(routes::post_transcribe))
        .route("/speech/speak", post(routes::post_speak))
        .route("/suggest", post(routes::post_suggest))
        .route("/telemetry", post(routes::post_telemetry))
        .route("/plugin/shutdown", post(lifecycle::request_shutdown))
        .route("/plugin/heartbeat", post(lifecycle::heartbeat))
        .route("/demo", post(routes::post_demo))
        .route("/session/reset", post(routes::session_reset))
        .route("/session/save", post(routes::session_save))
        .route("/session/load", post(routes::session_load))
        .layer(axum::Extension(lifetime.clone()))
        .with_state(shared);
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(address).await.unwrap();
    println!("OpenATC engine: http://127.0.0.1:{port}");
    lifecycle::serve(listener, app, lifetime).await;
}
