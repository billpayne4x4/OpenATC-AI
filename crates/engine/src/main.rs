//! Companion engine for controller state, local HTTP requests and AI/STT/TTS services.

#![allow(clippy::default_trait_access)]

mod airport_ops;
mod atc;
mod cabin;
mod chatter;
mod config_cache;
mod context;
mod crew;
mod debug_jump;
mod discipline;
mod flight;
mod generated;
mod initiative;
mod instructions;
mod intent;
mod lifecycle;
mod llm;
mod memory;
mod missions;
mod parking;
mod radio;
mod routes;
mod setup;
mod squawk;
mod support;
mod surface;
mod traffic;
mod traffic_models;
mod traffic_profiles;
mod traffic_runtime;
mod workers;

use openatc_core::airport::Airport;
use openatc_core::regions::{Region, load_regions};
use openatc_core::state::{Controllers, State};
use openatc_settings::Settings;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Product version with the compile timestamp.
pub const VERSION: &str = concat!("0.2.1 (", env!("OPENATC_BUILD"), ")");

/// A fresh, non-zero seed for one generated-traffic session.
///
/// The engine deliberately does not reproduce yesterday's airport on every launch. The seed is
/// logged with the first traffic status so an unusual scene can still be reproduced when needed.
pub(crate) fn traffic_seed() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos() as u64);
    let seed = nanos
        ^ u64::from(std::process::id()).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ COUNTER
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_mul(0xD1B5_4A32_D192_ED03);
    if seed == 0 { 1 } else { seed }
}

/// Everything the engine owns: session state plus configuration.
pub struct EngineState {
    /// Generated scene preparation and future movement state.
    pub generated: generated::Runtime,
    /// Shared CPU budget for work that must not occupy the HTTP executor.
    pub workers: Arc<workers::Workers>,
    /// Model downloads run separately from flight and radio work.
    pub model_installer: traffic_models::Installer,
    /// Validated airline, hub and aircraft preferences, loaded on CPU workers.
    pub traffic_profiles: traffic_profiles::Store,
    /// Share one scenery scan between map, station and radio requests.
    pub station_index_lock: Arc<tokio::sync::Mutex<()>>,
    /// Optional controller offers and their expiry.
    pub initiative: initiative::Monitor,
    /// Arrival stand changes are retried at a bounded rate.
    pub parking: parking::Monitor,
    /// Persistent ambient airport traffic.
    pub ambient: traffic::manager::TrafficManager,
    /// Audio-only simulated airport chatter.
    pub chatter: chatter::ChatterManager,
    /// CSL animation channel names shared by generated and ambient traffic.
    pub traffic_channels: BTreeMap<String, openatc_core::traffic::animation::Channel>,
    /// Animation rates shared by generated and ambient traffic.
    pub traffic_handling: BTreeMap<
        openatc_core::traffic::animation::Kind,
        openatc_core::traffic::animation::Handling,
    >,
    /// Aircraft known to the engine, including the player's ownship.
    pub aircraft: openatc_core::atc_manager::AircraftManager,
    /// Active ATC frequencies and their aircraft membership.
    pub atc: openatc_core::atc_manager::AtcManager,
    /// Bounded traffic history and active spacing instructions.
    pub traffic: traffic::Traffic,
    /// Frequency station threads and their transmission queues.
    pub(crate) station_bus: traffic_runtime::StationBus,
    /// Selected mission clock and branching outcomes.
    pub mission: missions::Runtime,
    /// Codes recently assigned by this engine, including previous flights.
    pub squawks: squawk::Allocator,
    /// Passenger activity for the simulated cabin conversation.
    pub cabin: cabin::Cabin,
    /// Recent requests and verified outcomes for this flight.
    pub request_memory: memory::RequestMemory,
    /// Invalidates work started before a new-flight reset.
    pub session_generation: u64,
    /// Seed for the generated traffic cast. A new value each engine start keeps the airport
    /// from opening with exactly the same flights, stands and procedures.
    pub traffic_seed: u64,
    /// Prepared developer checkpoint awaiting native positioning.
    pub debug_jump: Option<debug_jump::Pending>,
    /// Prepared crew radio exchange and its owning flight generation.
    pub pending_copilot_reply: Option<routes::PreparedReply>,
    /// Previous phrase choices and transmitted wording.
    pub phrase_history: radio::PhraseHistory,
    /// Aircraft crew controls, live readbacks and checklist progress.
    pub crew: crew::CrewSession,
    /// Automatic station transitions and arrival permissions.
    pub flow: flight::FlightFlow,
    /// Live session (plan, telemetry, phase, transcript, ...).
    pub session: State,
    /// Operator settings.
    pub settings: Settings,
    /// Cached published stations.
    pub stations: Vec<openatc_core::stations::Station>,
    /// Voice roster fetched from the configured TTS service.
    pub voices: Vec<openatc_core::voices::Voice>,
    /// TTS endpoint associated with the cached roster.
    pub voices_source: String,
    /// Last voice refresh attempt, including failed requests.
    pub voices_refreshed: Option<std::time::Instant>,
    /// Airport positions including fields without radio frequencies.
    pub map_airports: Vec<openatc_core::stations::AirportMarker>,
    /// Simulator folder used to build the airport marker index.
    pub map_root: String,
    /// Root used by the station index.
    pub station_root: String,
    /// True while the station index is being built on a worker.
    pub station_indexing: bool,
    /// Latest simulator airport weather, timestamped at receipt.
    /// Information letter advances only when the rounded airport observation changes.
    pub atis_information: BTreeMap<String, (String, usize)>,
    /// Bounded geometry cache for weather-based ATIS runway selection.
    pub atis_runways: BTreeMap<String, Vec<openatc_core::airport::Runway>>,
    /// A runway change advances the information letter independently of weather changes.
    pub atis_last_runway: BTreeMap<String, String>,
    /// Last information bulletin logged per airport and receiving radio.
    pub atis_logged: BTreeMap<String, String>,
    /// Surface observations received from X-Plane, with freshness timestamps.
    pub sim_weather: BTreeMap<String, (serde_json::Value, std::time::Instant)>,
    /// Loaded airport, if any.
    pub airport: Option<Airport>,
    /// Airport-local data and runway resources.
    pub airports: airport_ops::AirportManager,
    /// Destination geometry retained while departure surface data is in use.
    pub arrival_airport: Option<Airport>,
    /// Published approach entry levels used to plan descent before final approach.
    pub arrival_profile: Option<openatc_core::arrival::ArrivalAirport>,
    /// Reviewed nominal descent margins loaded when a flight plan is submitted.
    pub vertical_policy: instructions::VerticalPolicy,
    /// Installed minimum off-route grid for vectoring checks.
    pub mora: openatc_core::navigation::Mora,
    /// Remembered controller voices.
    pub controllers: Controllers,
    /// Role prompts.
    pub prompts: support::Prompts,
    /// Cached speech identifier pronunciation, separate from displayed text.
    pub pronunciation: Arc<openatc_core::radiotelephony::Pronunciation>,
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
    /// Directory holding the engine executable, used to resolve shipped prompts.
    pub exe_dir: PathBuf,
    /// Handoff and clearance expectations awaiting pilot compliance.
    pub expectations: Vec<openatc_core::compliance::Expectation>,
    /// Shared HTTP client for AI/STT/TTS upstreams.
    pub client: reqwest::Client,
}

/// Engine state shared across handlers and the weather watch.
pub type Shared = Arc<RwLock<EngineState>>;

impl EngineState {
    /// Native simulator traffic plus the frames this engine draws itself.
    pub(crate) fn traffic_targets(&self) -> Vec<openatc_core::state::TrafficTarget> {
        let mut targets = self.session.telemetry.traffic.clone().unwrap_or_default();
        // Some simulator builds report an injected model back as native traffic with a
        // different id. The drawn frame is authoritative; drop the native echo so the
        // aircraft does not appear twice or block itself.
        let drawn: std::collections::BTreeSet<&str> = self
            .session
            .generated_traffic
            .iter()
            .map(|frame| frame.target.callsign.as_str())
            .filter(|callsign| !callsign.is_empty())
            .collect();
        targets.retain(|target| target.id == 0 || !drawn.contains(target.callsign.as_str()));
        let mut ids: std::collections::BTreeSet<u32> =
            targets.iter().map(|target| target.id).collect();
        for frame in &self.session.generated_traffic {
            if frame.target.id != 0 && ids.insert(frame.target.id) {
                targets.push(frame.target.clone());
            }
        }
        targets
    }

    /// Copy of the current sample with the combined traffic picture attached.
    pub(crate) fn telemetry_with_traffic(&self) -> openatc_core::state::Telemetry {
        let mut telemetry = self.session.telemetry.clone();
        if telemetry.traffic.is_some() {
            telemetry.traffic = Some(self.traffic_targets());
        }
        telemetry
    }

    /// Use the configured callsign only while the plan has none of its own.
    pub(crate) fn apply_settings_callsign(&mut self) {
        if self.session.clearance.is_some() || !self.session.plan.callsign.trim().is_empty() {
            return;
        }
        let callsign = self.settings.callsign.trim();
        if callsign.is_empty() {
            return;
        }
        self.session.plan.callsign = callsign.to_ascii_uppercase();
        if self.session.plan.registration.trim().is_empty() {
            let callsign = self.session.plan.callsign.clone();
            self.session.plan.registration.clone_from(&callsign);
        }
    }

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

/// The product directory name, used for every directory `OpenATC AI` owns.
const PRODUCT_DIR: &str = "openatc-ai";

/// Resolve the config dir: env override, then the platform convention, then a fallback.
fn config_dir() -> PathBuf {
    if let Some(dir) =
        std::env::var_os("OPENATC_AI_CONFIG_DIR").or_else(|| std::env::var_os("OPENATC_CONFIG_DIR"))
    {
        return PathBuf::from(dir);
    }
    #[cfg(windows)]
    if let Some(dir) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(dir).join(PRODUCT_DIR);
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join("Library/Application Support")
            .join(PRODUCT_DIR);
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(dir).join(PRODUCT_DIR);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".config").join(PRODUCT_DIR);
    }
    PathBuf::from(format!("{PRODUCT_DIR}-config"))
}

/// Resolve the data dir: env override, then the platform convention, then a fallback.
///
/// The data dir holds the models, the backups and the rebuildable scenery index.
#[allow(dead_code)] // Used by the scenery index.
fn data_dir() -> PathBuf {
    if let Some(dir) =
        std::env::var_os("OPENATC_AI_DATA_DIR").or_else(|| std::env::var_os("OPENATC_DATA_DIR"))
    {
        return PathBuf::from(dir);
    }
    #[cfg(windows)]
    if let Some(dir) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(dir).join(PRODUCT_DIR);
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join("Library/Application Support")
            .join(PRODUCT_DIR);
    }
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(dir).join(PRODUCT_DIR);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".local/share").join(PRODUCT_DIR);
    }
    PathBuf::from(format!("{PRODUCT_DIR}-data"))
}

/// The directory used before the product namespace was unified.
fn legacy_config_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    if let Some(dir) = std::env::var_os("LOCALAPPDATA") {
        return Some(PathBuf::from(dir).join("openatc"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        return Some(PathBuf::from(home).join(".config/openatc"));
    }
    None
}

/// Copy one file, keeping the destination when it already exists.
fn copy_file(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    if to.exists() {
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(from, to)?;
    Ok(())
}

/// Copy a directory tree, keeping existing destination files.
fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            copy_file(&entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Move an installation from the old product directory to the current one.
///
/// The old directory is left in place as a backup, and only the files the new directory does
/// not have are copied, so a reinstall never overwrites newer settings.
fn migrate_legacy_config(config: &std::path::Path) -> Option<std::path::PathBuf> {
    if config.exists() {
        return None;
    }
    let legacy = legacy_config_dir()?;
    if !legacy.is_dir() {
        return None;
    }
    match copy_tree(&legacy, config) {
        Ok(()) => Some(legacy),
        Err(error) => {
            eprintln!(
                "openatc-ai: could not migrate {}: {error}",
                legacy.display()
            );
            None
        }
    }
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
    let (mut settings_stamp, mut prompts_stamp) = {
        let state = shared.read().await;
        (
            mtime(&state.config_dir.join("settings.json")),
            crate::support::prompt_stamp(&state.config_dir, &state.exe_dir),
        )
    };
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        reload_settings_if_changed(&shared, &mut settings_stamp).await;
        reload_prompts_if_changed(&shared, &mut prompts_stamp).await;
        // Weather transmissions use simulator observations only. Internet
        // METAR fetches remain available as explicitly labelled planning data.
    }
}

/// Reload an edited prompt catalogue without interrupting an in-flight exchange.
async fn reload_prompts_if_changed(shared: &Shared, stamp: &mut std::time::SystemTime) {
    let (config_dir, exe_dir) = {
        let state = shared.read().await;
        (state.config_dir.clone(), state.exe_dir.clone())
    };
    match crate::support::watch_prompts(&config_dir, &exe_dir, stamp) {
        Ok(Some(fresh)) => {
            shared.write().await.prompts = fresh;
        }
        Ok(None) => {}
        Err(error) => eprintln!("Role prompt reload rejected: {error}"),
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
    if let Some(legacy) = migrate_legacy_config(&config_dir) {
        eprintln!(
            "openatc-ai: migrated settings from {} to {}",
            legacy.display(),
            config_dir.display()
        );
    }
    let (settings, prompts, regions_path, regions, speech_source, pronunciation, speech, cabin) =
        tokio::task::spawn_blocking({
            let config_dir = config_dir.clone();
            let exe_dir = exe_dir.clone();
            move || {
                config_cache::on_worker(|| {
                    config_cache::initialize();
                    for path in [
                        exe_dir.join("../intents.toml"),
                        config_dir.join("intents.toml"),
                    ] {
                        if path.is_file()
                            && let Err(error) = openatc_core::catalog::activate(&path)
                        {
                            eprintln!("Intent catalogue rejected: {error}");
                        }
                    }
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
                    let prompts = match support::load_prompts(&config_dir, &exe_dir) {
                        Ok(prompts) => prompts,
                        Err(error) => {
                            eprintln!("Role prompt catalogue rejected: {error}");
                            std::process::exit(1);
                        }
                    };
                    let regions_path = support::resolve_regions_file(&config_dir, &exe_dir);
                    let regions = if regions_path.is_empty() {
                        BTreeMap::new()
                    } else {
                        load_regions(std::path::Path::new(&regions_path)).unwrap_or_default()
                    };
                    let speech_source = support::resolve_speech_dir(&config_dir, &exe_dir);
                    let pronunciation =
                        Arc::new(support::load_pronunciation(&config_dir, &speech_source));
                    eprintln!(
                        "Speech library: {speech_source}; phrase variety: {}",
                        settings.llm_phrase_variety
                    );
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
                        match openatc_core::speech::load_speech_dir(std::path::Path::new(
                            &speech_source,
                        )) {
                            Ok(pool) => pool,
                            Err(error) => {
                                eprintln!("Speech library rejected: {error}");
                                openatc_core::speech::SpeechPool::default()
                            }
                        }
                    };
                    let cabin = match cabin::Cabin::load(&config_dir, &exe_dir) {
                        Ok(cabin) => cabin,
                        Err(error) => {
                            eprintln!("Cabin policy rejected: {error}");
                            std::process::exit(1);
                        }
                    };
                    (
                        settings,
                        prompts,
                        regions_path,
                        regions,
                        speech_source,
                        pronunciation,
                        speech,
                        cabin,
                    )
                })
            }
        })
        .await
        .expect("setup worker");
    let mut controllers = Controllers::default();
    let controllers_path = config_dir.join("controllers.json");
    if let Ok(text) = std::fs::read_to_string(&controllers_path)
        && let Ok(saved) = serde_json::from_str(&text)
    {
        controllers = saved;
    }
    let workers = Arc::new(workers::Workers::new(settings.cpu_workers));
    let mut wake_loaded = false;
    for path in [
        config_dir.join("traffic/wake"),
        exe_dir.join("traffic/wake"),
        exe_dir.join("../traffic/wake"),
    ] {
        if !path.is_dir() {
            continue;
        }
        match openatc_core::traffic::wake_policy::load_dir(&path) {
            Ok(()) => {
                wake_loaded = true;
                break;
            }
            Err(error) => eprintln!("Wake policy folder {} rejected: {error}", path.display()),
        }
    }
    if !wake_loaded {
        let wake_text = [
            config_dir.join("traffic/wake.toml"),
            exe_dir.join("traffic/wake.toml"),
            exe_dir.join("../traffic/wake.toml"),
        ]
        .into_iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_else(|| include_str!("../../../traffic/wake.toml").to_owned());
        if let Err(error) = openatc_core::traffic::wake_policy::load(&wake_text) {
            eprintln!("Wake policy rejected: {error}");
        }
    }
    let traffic_profiles = traffic_profiles::Store::default();
    if let Err(error) = traffic_profiles.get(&config_dir, &workers, false).await {
        eprintln!("Traffic profiles rejected: {error}");
    }
    let mut engine = EngineState {
        workers,
        model_installer: Default::default(),
        station_index_lock: Default::default(),
        cabin,
        request_memory: Default::default(),
        squawks: Default::default(),
        mission: Default::default(),
        parking: Default::default(),
        initiative: Default::default(),
        ambient: Default::default(),
        chatter: Default::default(),
        traffic_channels: generated::animation_channels(&config_dir),
        traffic_handling: generated::animation_handling(&config_dir),
        aircraft: Default::default(),
        atc: Default::default(),
        traffic: Default::default(),
        station_bus: Default::default(),
        traffic_profiles,
        generated: Default::default(),
        session_generation: 0,
        traffic_seed: traffic_seed(),
        debug_jump: None,
        pending_copilot_reply: None,
        phrase_history: radio::PhraseHistory::load(&config_dir),
        crew: crew::CrewSession::default(),
        flow: flight::FlightFlow::default(),
        session: State::default(),
        settings,
        stations: Vec::new(),
        voices: Vec::new(),
        voices_source: String::new(),
        voices_refreshed: None,
        map_airports: Vec::new(),
        map_root: String::new(),
        station_root: String::new(),
        station_indexing: false,
        sim_weather: BTreeMap::new(),
        atis_information: BTreeMap::new(),
        atis_runways: BTreeMap::new(),
        atis_last_runway: BTreeMap::new(),
        atis_logged: BTreeMap::new(),
        airport: None,
        airports: Default::default(),
        arrival_airport: None,
        arrival_profile: None,
        vertical_policy: Default::default(),
        mora: Default::default(),
        controllers,
        prompts,
        pronunciation,
        regions,
        regions_path,
        speech,
        speech_source,
        config_dir,
        exe_dir,
        expectations: Vec::new(),
        client: reqwest::Client::new(),
    };
    engine.apply_settings_callsign();
    let shared: Shared = Arc::new(RwLock::new(engine));
    radio::warm_station_index(shared.clone());
    setup::start_initial(&shared).await;
    let weather = shared.clone();
    tokio::spawn(async move { weather_loop(weather).await });
    let lifetime = Arc::new(lifecycle::PluginSession::new(plugin_parent));
    let app = axum::Router::new()
        .route("/health", get(routes::health))
        .route("/setup/status", post(setup::status))
        .route("/setup/rebuild", post(setup::rerun))
        .route("/voice-health", get(routes::voice_health))
        .route("/speech/voices", post(routes::post_voices))
        .route("/debug/map", post(radio::debug_map))
        .route("/debug/atc", get(atc::debug_atc))
        .route("/debug/ambient", get(debug_ambient))
        .route("/debug/generated", get(generated::debug_generated))
        .route("/state", get(routes::full_state))
        .route("/settings", post(routes::post_settings))
        .route("/missions/list", post(missions::list))
        .route("/missions/save", post(missions::save))
        .route("/missions/start", post(missions::start))
        .route("/missions/stop", post(missions::stop))
        .route("/missions/choose", post(missions::choose))
        .route("/traffic/talk", post(traffic::talk))
        .route("/traffic/profiles", post(traffic_profiles::inspect))
        .route(
            "/traffic/scenery/status",
            post(generated::scenery::cache::status),
        )
        .route(
            "/traffic/scenery/clear",
            post(generated::scenery::cache::clear),
        )
        .route("/traffic/models/status", post(traffic_models::status))
        .route("/traffic/models/index", post(traffic_models::index))
        .route("/traffic/models/install", post(traffic_models::install))
        .route("/traffic/models/cancel", post(traffic_models::cancel))
        .route("/atc/offer", post(initiative::answer))
        .route("/simulator/root", post(routes::post_simulator_root))
        .route("/debug/cancel-actions", post(routes::debug_cancel_actions))
        .route("/debug/jump/prepare", post(debug_jump::prepare))
        .route("/debug/jump/finish", post(debug_jump::finish))
        .route("/crew/observe", post(crew::observe))
        .route("/crew/ack", post(crew::acknowledge))
        .route("/crew/callout", post(crew::callout_log))
        .route("/request", post(routes::post_request))
        .route("/request/auto-reply", post(routes::post_auto_reply))
        .route("/request/copilot-reply", post(routes::post_copilot_reply))
        .route(
            "/request/copilot-checkin",
            post(routes::post_copilot_checkin),
        )
        .route(
            "/request/copilot-prepare",
            post(routes::post_copilot_prepare),
        )
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

async fn debug_ambient(
    axum::extract::State(shared): axum::extract::State<Shared>,
) -> axum::Json<serde_json::Value> {
    let state = shared.read().await;
    let Some(airport) = state.airport.as_ref() else {
        return axum::Json(serde_json::json!({"error": "airport not loaded"}));
    };
    axum::Json(state.ambient.debug(airport))
}

#[cfg(test)]
fn test_state() -> EngineState {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let config_dir = std::env::temp_dir().join(format!(
        "openatc-unit-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let settings = Settings {
        ..Default::default()
    };
    let mut state = EngineState {
        workers: Arc::new(workers::Workers::new(1)),
        model_installer: Default::default(),
        station_index_lock: Default::default(),
        cabin: Default::default(),
        request_memory: Default::default(),
        squawks: Default::default(),
        mission: Default::default(),
        parking: Default::default(),
        initiative: Default::default(),
        ambient: Default::default(),
        chatter: Default::default(),
        traffic_channels: generated::animation_channels(&config_dir),
        traffic_handling: generated::animation_handling(&config_dir),
        aircraft: Default::default(),
        atc: Default::default(),
        traffic: Default::default(),
        station_bus: Default::default(),
        traffic_profiles: Default::default(),
        generated: Default::default(),
        session_generation: 0,
        traffic_seed: traffic_seed(),
        debug_jump: None,
        pending_copilot_reply: None,
        phrase_history: radio::PhraseHistory::load(&config_dir),
        crew: crew::CrewSession::default(),
        flow: flight::FlightFlow::default(),
        session: State::default(),
        settings,
        stations: Vec::new(),
        voices: Vec::new(),
        voices_source: String::new(),
        voices_refreshed: None,
        map_airports: Vec::new(),
        map_root: String::new(),
        station_root: String::new(),
        station_indexing: false,
        sim_weather: BTreeMap::new(),
        atis_information: BTreeMap::new(),
        atis_runways: BTreeMap::new(),
        atis_last_runway: BTreeMap::new(),
        atis_logged: BTreeMap::new(),
        airport: None,
        airports: Default::default(),
        arrival_airport: None,
        arrival_profile: None,
        vertical_policy: Default::default(),
        mora: Default::default(),
        controllers: Controllers::default(),
        prompts: support::load_prompts(&config_dir, &config_dir).expect("bundled role prompts"),
        pronunciation: Arc::new(Default::default()),
        regions: BTreeMap::new(),
        regions_path: String::new(),
        speech: Default::default(),
        speech_source: String::new(),
        exe_dir: config_dir.clone(),
        config_dir,
        expectations: Vec::new(),
        client: reqwest::Client::new(),
    };
    state.apply_settings_callsign();
    state
}

#[cfg(test)]
mod traffic_target_tests {
    use super::*;

    fn frame(id: u32, callsign: &str) -> openatc_core::state::GeneratedTraffic {
        openatc_core::state::GeneratedTraffic {
            vertical_speed_fpm: 0.0,
            body_heading_degrees: None,
            target: openatc_core::state::TrafficTarget {
                id,
                callsign: callsign.into(),
                ..Default::default()
            },
            model: std::sync::Arc::new(openatc_core::traffic::csl::Model {
                id: format!("test-{id}"),
                objects: Vec::new(),
                vertical_offset: None,
                matches: Vec::new(),
            }),
            animation: Default::default(),
        }
    }

    #[test]
    fn a_native_echo_of_a_drawn_aircraft_is_removed_by_callsign() {
        let mut state = test_state();
        state.session.telemetry.traffic = Some(vec![openatc_core::state::TrafficTarget {
            id: 7,
            callsign: "AMBIENT2".into(),
            ..Default::default()
        }]);
        state.session.generated_traffic = vec![frame(14_680_064, "AMBIENT2")];
        let targets = state.traffic_targets();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].id, 14_680_064);
    }

    #[test]
    fn native_and_simulated_targets_share_one_view_without_duplicates() {
        let mut state = test_state();
        state.session.telemetry.traffic = Some(vec![openatc_core::state::TrafficTarget {
            id: 1,
            callsign: "NATIVE1".into(),
            ..Default::default()
        }]);
        state.session.generated_traffic = vec![frame(1, "DUPLICATE"), frame(2, "AMBIENT2")];
        let targets = state.traffic_targets();
        assert_eq!(targets.len(), 2);
        assert!(targets.iter().any(|target| target.callsign == "NATIVE1"));
        assert!(targets.iter().any(|target| target.callsign == "AMBIENT2"));
    }
}

#[cfg(test)]
mod callsign_tests {
    use super::*;

    #[test]
    fn settings_callsign_fills_an_empty_plan_but_not_a_filed_one() {
        let mut state = test_state();
        state.settings.callsign = "VH-ABC".into();
        state.session.plan.callsign.clear();
        state.session.plan.registration.clear();
        state.apply_settings_callsign();
        assert_eq!(state.session.plan.callsign, "VH-ABC");
        assert_eq!(state.session.plan.registration, "VH-ABC");
        state.session.plan.callsign = "QFA123".into();
        state.apply_settings_callsign();
        assert_eq!(state.session.plan.callsign, "QFA123");
    }
}
