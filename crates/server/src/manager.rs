//! Local browser dashboard supervising its own inference process.
use crate::{resources, setup};
use axum::{
    Json, Router,
    extract::{Path as RoutePath, State},
    http::{HeaderMap, StatusCode},
    response::Html,
    routing::{get, post},
};
use clap::{CommandFactory, Parser, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::{Mutex, RwLock, mpsc, oneshot},
};

type ApiError = (StatusCode, String);
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Runtime {
    stage: String,
    generation: u64,
    pid: Option<u32>,
    health: Option<Value>,
    component: Option<String>,
    error: Option<String>,
    models: BTreeMap<String, Value>,
    logs: VecDeque<String>,
    resources: Value,
    gpu: Value,
}
struct Dashboard {
    path: PathBuf,
    fields: Vec<setup::Field>,
    origin: String,
    instance: String,
    runtime: Arc<RwLock<Runtime>>,
    actions: mpsc::Sender<Request>,
    apply: Mutex<()>,
}
struct Request {
    action: String,
    reply: oneshot::Sender<Result<(), String>>,
}
#[derive(Serialize, Deserialize)]
struct Record {
    origin: String,
    instance: String,
    config: PathBuf,
}
struct RecordGuard {
    path: PathBuf,
    instance: String,
}
impl Drop for RecordGuard {
    fn drop(&mut self) {
        if let Ok(data) = std::fs::read(&self.path) {
            if serde_json::from_slice::<Record>(&data)
                .is_ok_and(|record| record.instance == self.instance)
            {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}
fn origin_allowed(headers: &HeaderMap, origin: &str) -> Result<(), ApiError> {
    if headers.get("origin").and_then(|v| v.to_str().ok()) != Some(origin) {
        Err((
            StatusCode::FORBIDDEN,
            "Use the local dashboard to change settings or server state".into(),
        ))
    } else {
        Ok(())
    }
}
async fn request(dashboard: &Dashboard, action: &str) -> Result<(), ApiError> {
    let (tx, rx) = oneshot::channel();
    dashboard
        .actions
        .send(Request {
            action: action.into(),
            reply: tx,
        })
        .await
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "Server manager is unavailable".into(),
            )
        })?;
    rx.await
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "Server manager stopped".into(),
            )
        })?
        .map_err(|error| (StatusCode::BAD_REQUEST, error))
}
fn arguments(path: &Path) -> Result<crate::Args, String> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| format!("Save the setup wizard before starting the server: {e}"))?;
    let args = crate::config::arguments(&source, &["openatc-ai".into()], &crate::Args::command())?;
    crate::Args::try_parse_from(args).map_err(|e| e.to_string())
}
fn model_usage(runtime: &Runtime, kind: &str, selected: bool) -> Value {
    let active = selected && runtime.pid.is_some();
    let main_process = kind != "stt";
    let shared = main_process
        && runtime
            .health
            .as_ref()
            .is_none_or(|h| h["components"]["llm"] != false && h["components"]["tts"] != false);
    let belongs = |pid: u64| {
        active
            && if main_process {
                Some(pid as u32) == runtime.pid
            } else {
                Some(pid as u32) != runtime.pid
            }
    };
    let ram: Option<u64> = runtime.resources["processes"]
        .as_array()
        .filter(|_| active)
        .and_then(|rows| {
            let matched: Vec<_> = rows
                .iter()
                .filter(|row| row["pid"].as_u64().is_some_and(belongs))
                .collect();
            (!matched.is_empty()).then(|| {
                matched
                    .iter()
                    .filter_map(|row| row["ramBytes"].as_u64())
                    .sum()
            })
        });
    let mut vram = None;
    if let Some(rows) = runtime.gpu["processes"].as_array().filter(|_| active) {
        let matched: Vec<_> = rows
            .iter()
            .filter(|row| row["pid"].as_u64().is_some_and(belongs))
            .collect();
        if !matched.is_empty() && matched.iter().all(|r| r["memoryMiB"].is_u64()) {
            vram = Some(
                matched
                    .iter()
                    .filter_map(|r| r["memoryMiB"].as_u64())
                    .sum::<u64>()
                    * 1048576,
            );
        }
    }
    json!({"ramBytes":ram,"vramBytes":vram,"shared":shared,"description":if shared{"Shared language/voice process: includes both models, context and runtime overhead; do not add these rows together."}else if main_process{"Model process with one enabled language/voice component; includes context and runtime overhead."}else{"Speech recognition process and its sidecar; includes runtime overhead."}})
}
fn model_compute(runtime: &Runtime, kind: &str, cpu: bool, loaded: bool) -> Value {
    if !loaded {
        return json!({"label":if cpu{"CPU configured"}else{"GPU preferred"},"reason":"Saved preference; no device is loaded for this row."});
    }
    let device = &runtime
        .health
        .as_ref()
        .map(|h| h["compute"][kind].clone())
        .unwrap_or(Value::Null);
    let value = device
        .as_str()
        .or_else(|| device["device"].as_str())
        .unwrap_or("unknown");
    let label = if value.contains("cpu") {
        "CPU"
    } else if value.contains("requested") {
        "GPU requested"
    } else if ["cuda", "coreml", "metal", "vulkan", "gpu"]
        .iter()
        .any(|name| value.contains(name))
    {
        "GPU"
    } else {
        "Unknown"
    };
    json!({"label":label,"reason":device["reason"].as_str().unwrap_or(value)})
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveModel {
    name: String,
}
fn remove_model(path: &Path, runtime: &Runtime, name: &str) -> Result<(), String> {
    let catalogue = registry(path, runtime)?;
    let rows = catalogue["models"]
        .as_array()
        .ok_or("Model library is unavailable")?;
    let entry = rows
        .iter()
        .find(|row| row["name"].as_str() == Some(name))
        .ok_or("Unknown model")?;
    if runtime.pid.is_some() && entry["selected"] == true && entry["enabled"] == true {
        return Err("Unload this model before deleting its cached files".into());
    }
    let root = PathBuf::from(catalogue["root"].as_str().ok_or("Invalid model root")?)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let files = entry["files"].as_array().ok_or("Invalid model files")?;
    let paths: Vec<_> = files
        .iter()
        .filter(|file| file["present"] == true)
        .filter_map(|file| file["path"].as_str().map(PathBuf::from))
        .collect();
    if paths.is_empty() {
        return Err("Model files are already absent".into());
    }
    for file in &paths {
        let parent = file
            .parent()
            .ok_or("Invalid model path")?
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if !parent.starts_with(&root) {
            return Err("Model cache path leaves the model folder".into());
        }
    }
    // Remove only the model's known files; retain their checksums for future downloads.
    for file in paths {
        std::fs::remove_file(&file)
            .map_err(|e| format!("Cannot remove {}: {e}", file.display()))?;
    }
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelState {
    name: String,
    loaded: bool,
}
fn model_settings(
    path: &Path,
    runtime: &Runtime,
    change: &ModelState,
) -> Result<toml::Table, String> {
    let catalogue = registry(path, runtime)?;
    let row = catalogue["models"]
        .as_array()
        .ok_or("Model library unavailable")?
        .iter()
        .find(|row| row["name"].as_str() == Some(&change.name))
        .ok_or("Unknown model")?;
    let kind = row["kind"].as_str().ok_or("Invalid model component")?;
    if !change.loaded && row["selected"] != true {
        return Err("That model is already unloaded".into());
    }
    let mut values = setup::values(path)?;
    if change.loaded {
        if let Some(choice) = row["choice"].as_str() {
            if kind == "llm" {
                if row["selected"] != true {
                    for key in [
                        "llm-manifest",
                        "llm-url",
                        "llm-name",
                        "llm-file",
                        "llm-sha256",
                        "llm-bytes",
                        "chat-template",
                        "assistant-prefix",
                    ] {
                        values.remove(key);
                    }
                }
                values.insert("llm-model".into(), toml::Value::String(choice.into()));
            } else if kind == "tts" {
                values.insert("tts-provider".into(), toml::Value::String(choice.into()));
            }
        } else if row["selected"] != true {
            return Err("Select this model in settings before loading it".into());
        }
    }
    values.insert(
        format!("disable-{kind}"),
        toml::Value::Boolean(!change.loaded),
    );
    Ok(values)
}
fn registry(path: &Path, runtime: &Runtime) -> Result<Value, String> {
    let saved = setup::values(path)?;
    let source = toml::to_string(&saved).map_err(|e| e.to_string())?;
    let args = crate::Args::try_parse_from(crate::config::arguments(
        &source,
        &["server".into()],
        &crate::Args::command(),
    )?)
    .map_err(|e| e.to_string())?;
    let root = args
        .models_dir
        .clone()
        .unwrap_or_else(openatc_platform::models_dir);
    let mut manifest = openatc_ai_core::Manifest::parse(include_str!("../../../models.toml"))
        .map_err(|e| e.to_string())?;
    manifest.model.extend(
        openatc_ai_core::Manifest::parse(include_str!("../../../models-kitten.toml"))
            .map_err(|e| e.to_string())?
            .model,
    );
    let custom = crate::llm_config::configured_manifest(&args)?;
    let custom_names: Vec<_> = custom
        .as_ref()
        .map(|m| m.model.iter().map(|v| v.name.clone()).collect())
        .unwrap_or_default();
    if let Some(custom) = custom {
        manifest.model.extend(custom.model);
    }
    let lock: openatc_ai_core::Lockfile = std::fs::read_to_string(root.join("models.lock.toml"))
        .ok()
        .and_then(|v| toml::from_str(&v).ok())
        .unwrap_or_default();
    let mut rows = vec![];
    for entry in manifest.model {
        let file = root.join(&entry.path);
        let bytes = std::fs::metadata(&file)
            .ok()
            .filter(|m| m.is_file())
            .map(|m| m.len());
        let kind = entry.path.split('/').next().unwrap_or("model");
        let selected = if kind == "llm" {
            if custom_names.is_empty() {
                entry.name == args.llm_model.manifest_name()
            } else {
                custom_names.contains(&entry.name)
            }
        } else if kind == "tts" {
            if args.tts_provider == crate::synthesis::Provider::Kitten {
                entry.path.contains("kitten")
            } else {
                !entry.path.contains("kitten")
            }
        } else {
            true
        };
        let event = runtime.models.get(&entry.name);
        let verified = event
            .and_then(|v| v.get("verified"))
            .and_then(Value::as_bool)
            == Some(true);
        let enabled = match kind {
            "llm" => !args.disable_llm,
            "tts" => !args.disable_tts,
            _ => !args.disable_stt,
        };
        let loaded = selected && enabled && runtime.stage == "ready" && verified;
        let choice = if kind == "llm" {
            crate::llm_config::Model::value_variants()
                .iter()
                .find(|model| model.manifest_name() == entry.name)
                .and_then(|model| model.to_possible_value())
                .map(|value| value.get_name().to_owned())
        } else if kind == "tts" {
            Some(
                if entry.path.contains("kitten") {
                    "kitten"
                } else {
                    "kokoro"
                }
                .to_owned(),
            )
        } else {
            None
        };
        let cpu = match kind {
            "llm" => args.cpu.llm(),
            "tts" => args.cpu.tts(),
            _ => args.cpu.stt() || args.stt_backend == "cpu",
        };
        rows.push(json!({"enabled":enabled,"choice":choice,"usage":model_usage(runtime,kind,selected&&enabled),"compute":model_compute(runtime,kind,cpu,loaded),"removable":bytes.is_some()&&(!selected||!enabled||runtime.pid.is_none()),"name":entry.name,"kind":kind,"path":file,"selected":selected,"bytes":bytes,
            "partialBytes":std::fs::metadata(file.with_extension("part")).ok().map(|m|m.len()),
            "expectedBytes":lock.model.get(&entry.name).map(|v|v.size).filter(|n|*n>0).or((entry.size>0).then_some(entry.size)),
            "present":bytes.is_some(),"verified":verified,"loaded":loaded,"progress":event}));
    }
    let mut models = vec![];
    for primary in &rows {
        if primary["kind"] == "tts"
            && !primary["path"]
                .as_str()
                .is_some_and(|path| path.ends_with(".onnx"))
        {
            continue;
        }
        let files:Vec<_>=rows.iter().filter(|file|if primary["kind"]=="tts"{file["kind"]=="tts"&&file["choice"]==primary["choice"]}else{file["name"]==primary["name"]}).map(|file|json!({"name":file["name"],"path":file["path"],"bytes":file["bytes"],"present":file["present"],"progress":file["progress"]})).collect();
        let mut model = primary.clone();
        let cached = files.iter().any(|file| file["present"] == true);
        model["cached"] = json!(cached);
        model["present"] = json!(files.iter().all(|file| file["present"] == true));
        model["bytes"] = if cached {
            json!(
                files
                    .iter()
                    .filter_map(|file| file["bytes"].as_u64())
                    .sum::<u64>()
            )
        } else {
            Value::Null
        };
        model["removable"] = json!(
            cached
                && (!primary["selected"].as_bool().unwrap_or(false)
                    || !primary["enabled"].as_bool().unwrap_or(false)
                    || runtime.pid.is_none())
        );
        if let Some(active) = files.iter().find(|file| {
            file["progress"]["stage"] == "downloading" || file["progress"]["stage"] == "verifying"
        }) {
            model["progress"] = active["progress"].clone();
        }
        model["files"] = json!(files);
        models.push(model);
    }
    Ok(json!({"root":root,"models":models}))
}
fn router(dashboard: Arc<Dashboard>) -> Router {
    Router::new()
        .route("/",get(||async{Html(include_str!("../../../server/settings.html"))}))
        .route("/branding/banner.png",get(||async{([(axum::http::header::CONTENT_TYPE,"image/png")],include_bytes!("../../../assets/installer/banner.png").as_slice())}))
        .route("/identity",get(|State(d):State<Arc<Dashboard>>|async move{Json(json!({"instance":d.instance,"config":d.path}))}))
        .route("/settings",get(|State(d):State<Arc<Dashboard>>|async move{
            setup::values(&d.path).map(|v|Json(json!({"fields":d.fields,"values":v,"firstRun":!d.path.exists(),"configPath":d.path}))).map_err(|e|(StatusCode::BAD_REQUEST,e))
        }).post(|State(d):State<Arc<Dashboard>>,headers:HeaderMap,Json(values):Json<toml::Table>|async move{
            origin_allowed(&headers,&d.origin)?;
            let _apply=d.apply.lock().await;
            setup::save(&d.path,values).map_err(|e|(StatusCode::BAD_REQUEST,e))?;
            request(&d,"restart").await?;
            Ok::<_,ApiError>(Json(json!({"saved":true,"starting":true,"generation":d.runtime.read().await.generation,"instance":d.instance})))
        }))
        .route("/status",get(|State(d):State<Arc<Dashboard>>|async move{
            let state=d.runtime.read().await;
            Json(json!({"instance":d.instance,"managerVersion":crate::VERSION,"machine":{"hostname":sysinfo::System::host_name(),"os":sysinfo::System::long_os_version(),"arch":std::env::consts::ARCH},"runtime":*state,"catalogue":registry(&d.path,&state).unwrap_or_else(|e|json!({"error":e})),"configPath":d.path}))
        }))
        .route("/models/state",post(|State(d):State<Arc<Dashboard>>,headers:HeaderMap,Json(change):Json<ModelState>|async move{
            origin_allowed(&headers,&d.origin)?;
            let _apply=d.apply.lock().await;
            let values=model_settings(&d.path,&*d.runtime.read().await,&change).map_err(|e|(StatusCode::BAD_REQUEST,e))?;
            setup::save(&d.path,values).map_err(|e|(StatusCode::BAD_REQUEST,e))?;
            request(&d,"restart").await?;
            Ok::<_,ApiError>(Json(json!({"accepted":true,"generation":d.runtime.read().await.generation,"instance":d.instance})))
        }))
        .route("/models/remove",post(|State(d):State<Arc<Dashboard>>,headers:HeaderMap,Json(model):Json<RemoveModel>|async move{
            origin_allowed(&headers,&d.origin)?;
            let _apply=d.apply.lock().await;
            remove_model(&d.path,&*d.runtime.read().await,&model.name).map_err(|e|(StatusCode::BAD_REQUEST,e))?;
            Ok::<_,ApiError>(Json(json!({"removed":true})))
        }))
        .route("/action/{action}",post(|State(d):State<Arc<Dashboard>>,RoutePath(action):RoutePath<String>,headers:HeaderMap|async move{
            origin_allowed(&headers,&d.origin)?;
            if !["start","stop","restart","unload-all","shutdown"].contains(&action.as_str()){return Err((StatusCode::BAD_REQUEST,"Unknown server action".into()));}
            let _apply=d.apply.lock().await;
            if action=="shutdown" {
                request(&d,"stop").await?;
                tokio::spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    std::process::exit(0);
                });
                return Ok::<_,ApiError>(Json(json!({"stopping":true})));
            }
            let mut action=action;
            if action=="unload-all" {
                let mut values=setup::values(&d.path).map_err(|e|(StatusCode::BAD_REQUEST,e))?;
                for key in ["disable-llm","disable-stt","disable-tts"] {values.insert(key.into(),toml::Value::Boolean(true));}
                setup::save(&d.path,values).map_err(|e|(StatusCode::BAD_REQUEST,e))?;
                action="restart".into();
            }else if action=="start" {
                let mut values=setup::values(&d.path).map_err(|e|(StatusCode::BAD_REQUEST,e))?;
                let mut changed=false;
                for key in ["disable-llm","disable-stt","disable-tts"] {if values.remove(key).and_then(|v|v.as_bool()).unwrap_or(false){changed=true;}}
                if changed {setup::save(&d.path,values).map_err(|e|(StatusCode::BAD_REQUEST,e))?;action="restart".into();}
            }
            request(&d,&action).await?;
            Ok::<_,ApiError>(Json(json!({"accepted":true,"generation":d.runtime.read().await.generation,"instance":d.instance})))
        })).with_state(dashboard)
}

pub async fn run(
    path: &Path,
    schema: Option<&Path>,
    start: bool,
    browser: bool,
    port: u16,
    host: std::net::IpAddr,
) -> Result<(), String> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    let parent = path
        .parent()
        .ok_or("Settings path needs a parent directory")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let path = parent
        .canonicalize()
        .map_err(|e| e.to_string())?
        .join(path.file_name().ok_or("Settings path needs a filename")?);
    let record_path = path.with_extension("dashboard.json");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| e.to_string())?;
    for attempt in 0..3 {
        if let Ok(data) = std::fs::read(&record_path) {
            if let Ok(record) = serde_json::from_slice::<Record>(&data) {
                if record.config == path && local_origin(&record.origin) {
                    if let Ok(reply) = client
                        .get(format!("{}/identity", record.origin))
                        .send()
                        .await
                    {
                        if let Ok(identity) = reply.json::<Value>().await {
                            if identity["instance"].as_str() == Some(&record.instance) {
                                if browser {
                                    setup::open_browser(&record.origin);
                                }
                                println!("Server dashboard: {}", record.origin);
                                return Ok(());
                            }
                        }
                    }
                }
            }
            if attempt == 0 {
                tokio::time::sleep(Duration::from_millis(300)).await;
                continue;
            }
            std::fs::rename(
                &record_path,
                record_path.with_extension(format!("stale-{}", std::process::id())),
            )
            .map_err(|e| e.to_string())?;
        }
        let record = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&record_path);
        let Ok(mut record) = record else {
            tokio::time::sleep(Duration::from_millis(300)).await;
            continue;
        };
        let listener = tokio::net::TcpListener::bind((host, port))
            .await
            .map_err(|e| e.to_string())?;
        let origin = format!(
            "http://{}",
            listener.local_addr().map_err(|e| e.to_string())?
        );
        let instance = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let _guard = RecordGuard {
            path: record_path.clone(),
            instance: instance.clone(),
        };
        serde_json::to_writer(
            &mut record,
            &Record {
                origin: origin.clone(),
                instance: instance.clone(),
                config: path.clone(),
            },
        )
        .map_err(|e| e.to_string())?;
        drop(record);
        let runtime = Arc::new(RwLock::new(Runtime {
            stage: "stopped".into(),
            ..Runtime::default()
        }));
        let (tx, rx) = mpsc::channel(16);
        let d = Arc::new(Dashboard {
            path: path.clone(),
            fields: setup::fields(schema)?,
            origin: origin.clone(),
            instance,
            runtime: runtime.clone(),
            actions: tx,
            apply: Mutex::new(()),
        });
        let actor = tokio::spawn(supervise(path.clone(), runtime, rx));
        let start_d = d.clone();
        if start && path.exists() {
            tokio::spawn(async move {
                if let Err((_, e)) = request(&start_d, "start").await {
                    start_d.runtime.write().await.error = Some(e);
                }
            });
        }
        println!(
            "Server dashboard: {origin}\nSettings: {}\nThe dashboard stays available while models download and the server reloads.",
            path.display()
        );
        if browser {
            setup::open_browser(&origin);
        }
        let result = axum::serve(listener, router(d.clone()))
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await
            .map_err(|e| e.to_string());
        let _ = request(&d, "stop").await;
        drop(d);
        actor.abort();
        return result;
    }
    Err("Could not open the server dashboard; another dashboard may still be starting".into())
}
fn local_origin(origin: &str) -> bool {
    reqwest::Url::parse(origin).is_ok_and(|url| {
        url.scheme() == "http"
            && url
                .host_str()
                .and_then(|host| host.parse::<std::net::IpAddr>().ok())
                .is_some_and(|ip| match ip {
                    std::net::IpAddr::V4(ip) => ip.is_loopback() || ip.is_private(),
                    std::net::IpAddr::V6(ip) => {
                        ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local()
                    }
                })
            && url.port().is_some()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none()
            && url.username().is_empty()
            && url.password().is_none()
    })
}
async fn stop(child: &mut Option<tokio::process::Child>) {
    if let Some(mut process) = child.take() {
        if let Some(pid) = process.id() {
            resources::kill_descendants(pid);
        }
        let _ = process.kill().await;
        let _ = process.wait().await;
    }
}
async fn supervise(
    path: PathBuf,
    state: Arc<RwLock<Runtime>>,
    mut requests: mpsc::Receiver<Request>,
) {
    let mut child: Option<tokio::process::Child> = None;
    let mut generation = 0u64;
    let (events, mut incoming) = mpsc::channel::<(u64, Option<Value>, String)>(256);
    let mut timer = tokio::time::interval(Duration::from_secs(2));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut monitor = Some(resources::Monitor::new());
    loop {
        tokio::select! {
            request=requests.recv()=>{
                let Some(request)=request else{stop(&mut child).await;return;};
                if request.action=="start"&&child.is_some(){let _=request.reply.send(Ok(()));continue;}
                // Validate before stopping a healthy runtime, so invalid edits never force a restart.
                let config=if request.action!="stop"{match arguments(&path){Ok(args)=>Some(args),Err(e)=>{let _=request.reply.send(Err(e));continue;}}}else{None};
                generation+=1;
                {let mut s=state.write().await;s.stage="restarting".into();s.generation=generation;s.health=None;s.error=None;}
                stop(&mut child).await;
                {let mut s=state.write().await;s.pid=None;s.models.clear();s.component=None;s.stage="stopped".into();}
                if let Some(config)=config {
                    // Report a busy address before fetching models; never stop an unrelated server.
                    match tokio::net::TcpListener::bind((config.host.as_str(),config.port)).await {
                        Ok(probe)=>drop(probe),Err(e)=>{let e=format!("Cannot start on {}:{}: {e}. Another server may already be running.",config.host,config.port);let mut s=state.write().await;s.stage="error".into();s.error=Some(e.clone());let _=request.reply.send(Err(e));continue;}
                    }
                    let executable=match std::env::current_exe(){Ok(v)=>v,Err(e)=>{let _=request.reply.send(Err(e.to_string()));continue;}};
                    let mut command=tokio::process::Command::new(executable);
                    command.arg("--config").arg(&path).arg("--managed-events").stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
                    #[cfg(windows)] command.creation_flags(0x08000000);
                    match command.spawn(){
                        Ok(mut process)=>{
                            {let mut s=state.write().await;s.stage="starting".into();s.pid=process.id();}
                            if let Some(stdout)=process.stdout.take(){read_events(stdout,generation,events.clone(),true);}
                            if let Some(stderr)=process.stderr.take(){read_events(stderr,generation,events.clone(),false);}
                            child=Some(process);let _=request.reply.send(Ok(()));
                        }
                        Err(e)=>{let mut s=state.write().await;s.stage="error".into();s.error=Some(e.to_string());let _=request.reply.send(Err(e.to_string()));}
                    }
                }else{let _=request.reply.send(Ok(()));}
            }
            event=incoming.recv()=>{if let Some((source,event,line))=event {if source!=generation {continue;}let mut s=state.write().await;
                if let Some(event)=event {
                    if let Some(model)=event.get("model").and_then(Value::as_str){let name=model.to_owned();let mut merged=s.models.get(&name).cloned().unwrap_or_else(||json!({}));if let (Some(to),Some(from))=(merged.as_object_mut(),event.as_object()){to.extend(from.clone());}if event["stage"]=="verified"{merged["verified"]=json!(true);}s.models.insert(name,merged);}
                    if let Some(stage)=event["stage"].as_str(){if stage!="verified"{s.stage=stage.into();}}
                    if let Some(component)=event["component"].as_str(){s.component=Some(component.into());}
                    if let Some(error)=event["error"].as_str(){s.error=Some(error.into());}
                    if event["stage"]=="ready"{s.health=event.get("health").cloned();s.component=None;}
                }else{if s.logs.len()>=100{s.logs.pop_front();}s.logs.push_back(line);}
            }}
            _=timer.tick()=>{
                if let Some(process)=child.as_mut(){match process.try_wait(){Ok(Some(exit))=>{child=None;let mut s=state.write().await;s.pid=None;s.health=None;s.stage="error".into();s.error=Some(format!("Server stopped ({exit}). Check the recent messages, then retry."));},Ok(None)=>{},Err(e)=>{state.write().await.error=Some(e.to_string());}}}
                let root=child.as_ref().and_then(tokio::process::Child::id);
                let mut sampler=monitor.take().unwrap_or_else(resources::Monitor::new);
                if let Ok((sampler_returned,sample))=tokio::task::spawn_blocking(move||{let sample=sampler.sample(root);(sampler,sample)}).await{
                    let pids:Vec<u32>=sample["pids"].as_array().into_iter().flatten().filter_map(|v|v.as_u64().and_then(|n|u32::try_from(n).ok())).collect();
                    let gpu=resources::gpu(&pids).await;monitor=Some(sampler_returned);let mut s=state.write().await;s.resources=sample;s.gpu=gpu;
                }
            }
        }
    }
}
fn read_events(
    reader: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    generation: u64,
    events: mpsc::Sender<(u64, Option<Value>, String)>,
    structured: bool,
) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        let mut buffer = Vec::with_capacity(8192);
        loop {
            let available = match reader.fill_buf().await {
                Ok(bytes) if !bytes.is_empty() => bytes,
                _ => break,
            };
            let end = available
                .iter()
                .position(|byte| *byte == b'\n' || *byte == b'\r');
            let used = end.map_or(available.len(), |n| n + 1);
            let keep = (8192 - buffer.len()).min(end.unwrap_or(available.len()));
            buffer.extend_from_slice(&available[..keep]);
            reader.consume(used);
            if end.is_none() {
                continue;
            }
            let text = String::from_utf8_lossy(&buffer).trim().to_owned();
            buffer.clear();
            if text.is_empty() {
                continue;
            }
            let event = if structured {
                text.strip_prefix("OPENATC_STATUS ")
                    .and_then(|v| serde_json::from_str(v).ok())
            } else {
                None
            };
            if events
                .send((generation, event, text.chars().take(2000).collect()))
                .await
                .is_err()
            {
                break;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendezvous_accepts_only_plain_local_network_origins() {
        assert!(local_origin("http://127.0.0.1:12345"));
        assert!(local_origin("http://192.168.1.10:12345"));
        for origin in [
            "http://localhost:12345",
            "http://8.8.8.8:12345",
            "https://127.0.0.1:12345",
            "http://127.0.0.1:12345/path",
            "http://user@127.0.0.1:12345",
            "http://127.0.0.1:12345?next=external",
        ] {
            assert!(!local_origin(origin), "{origin}");
        }
    }

    #[tokio::test]
    async fn local_dashboard_validates_edits_before_applying_and_requires_origin() {
        let directory =
            std::env::temp_dir().join(format!("openatc-dashboard-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("server.toml");
        std::fs::write(&path, "port=8099\n").unwrap();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (actions, mut requests) = mpsc::channel(4);
        let dashboard = Arc::new(Dashboard {
            path: path.clone(),
            fields: setup::fields(None).unwrap(),
            origin: origin.clone(),
            instance: "test".into(),
            runtime: Arc::new(RwLock::new(Runtime::default())),
            actions,
            apply: Mutex::new(()),
        });
        let server = tokio::spawn(async move {
            axum::serve(listener, router(dashboard)).await.unwrap();
        });
        let client = reqwest::Client::new();
        let forbidden = client
            .post(format!("{origin}/settings"))
            .json(&json!({"port":8100}))
            .send()
            .await
            .unwrap();
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
        let invalid = client
            .post(format!("{origin}/settings"))
            .header("origin", &origin)
            .json(&json!({"ctx-size":2}))
            .send()
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "port=8099\n");
        assert!(requests.try_recv().is_err());
        let actor = tokio::spawn(async move {
            let request = requests.recv().await.unwrap();
            assert_eq!(request.action, "restart");
            request.reply.send(Ok(())).unwrap();
        });
        let saved = client
            .post(format!("{origin}/settings"))
            .header("origin", &origin)
            .json(&json!({"port":8100}))
            .send()
            .await
            .unwrap();
        assert!(saved.status().is_success());
        let receipt: Value = saved.json().await.unwrap();
        assert_eq!(receipt["generation"], 0);
        assert_eq!(receipt["instance"], "test");
        actor.await.unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("8100"));
        let status: Value = client
            .get(format!("{origin}/status"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(
            status["catalogue"]["models"]
                .as_array()
                .unwrap()
                .iter()
                .all(|m| m["loaded"] == false)
        );
        server.abort();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn native_progress_lines_are_bounded_and_carriage_returns_are_consumed() {
        use tokio::io::AsyncWriteExt;
        let (mut writer, reader) = tokio::io::duplex(1024);
        let (events, mut incoming) = mpsc::channel(8);
        read_events(reader, 7, events, true);
        let producer = tokio::spawn(async move {
            writer.write_all(&vec![b'x'; 100_000]).await.unwrap();
            writer
                .write_all(b"\rOPENATC_STATUS {\"stage\":\"loading\",\"component\":\"llm\"}\n")
                .await
                .unwrap();
        });
        let (generation, event, line) = incoming.recv().await.unwrap();
        assert_eq!(generation, 7);
        assert!(event.is_none());
        assert_eq!(line.len(), 2000);
        let (_, event, _) = incoming.recv().await.unwrap();
        assert_eq!(event.unwrap()["component"], "llm");
        producer.await.unwrap();
    }
}

#[cfg(test)]
mod model_tests {
    use super::*;
    #[test]
    fn cache_removal_refuses_selected_files_until_unloaded_and_keeps_other_files() {
        let root =
            std::env::temp_dir().join(format!("openatc-model-remove-{}", std::process::id()));
        std::fs::create_dir_all(root.join("tts")).unwrap();
        let model = root.join("tts/kokoro-v1.0.onnx");
        std::fs::write(&model, b"fixture").unwrap();
        std::fs::write(root.join("tts/voices-v1.0.bin"), b"voice fixture").unwrap();
        std::fs::write(root.join("keep.txt"), b"keep").unwrap();
        let path = root.join("server.toml");
        setup::save(
            &path,
            toml::Table::from_iter([(
                "models-dir".into(),
                toml::Value::String(root.to_string_lossy().into_owned()),
            )]),
        )
        .unwrap();
        let runtime = Runtime {
            pid: Some(1234),
            stage: "loading".into(),
            ..Runtime::default()
        };
        assert!(
            remove_model(&path, &runtime, "kokoro-v1.0")
                .unwrap_err()
                .contains("Unload")
        );
        assert!(model.exists());
        assert!(remove_model(&path, &runtime, "../keep.txt").is_err());
        let mut values = setup::values(&path).unwrap();
        values.insert("disable-tts".into(), toml::Value::Boolean(true));
        setup::save(&path, values).unwrap();
        remove_model(&path, &runtime, "kokoro-v1.0").unwrap();
        assert!(!model.exists());
        assert!(!root.join("tts/voices-v1.0.bin").exists());
        assert!(root.join("keep.txt").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn shared_usage_is_labelled_and_stt_uses_only_its_own_process() {
        let runtime = Runtime {
            pid: Some(11),
            resources: json!({"processes":[{"pid":11,"ramBytes":1000},{"pid":12,"ramBytes":200}]}),
            gpu: json!({"processes":[{"pid":11,"memoryMiB":30},{"pid":12,"memoryMiB":4}]}),
            ..Runtime::default()
        };
        let llm = model_usage(&runtime, "llm", true);
        let tts = model_usage(&runtime, "tts", true);
        let stt = model_usage(&runtime, "stt", true);
        assert_eq!(llm, tts);
        assert_eq!(llm["shared"], true);
        assert_eq!(llm["ramBytes"], 1000);
        assert_eq!(stt["shared"], false);
        assert_eq!(stt["ramBytes"], 200);
        assert_eq!(stt["vramBytes"], 4 * 1048576);
        assert!(model_usage(&runtime, "llm", false)["ramBytes"].is_null());
        assert_eq!(
            model_compute(&Runtime::default(), "llm", true, false)["label"],
            "CPU configured"
        );
        assert_eq!(
            model_compute(
                &Runtime {
                    health: Some(
                        json!({"compute":{"llm":{"device":"gpu-requested","reason":"Requested layers"}}})
                    ),
                    ..Runtime::default()
                },
                "llm",
                false,
                true
            )["label"],
            "GPU requested"
        );
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    #[test]
    fn loading_a_bundled_model_changes_only_its_component_and_clears_custom_template() {
        let root = std::env::temp_dir().join(format!("openatc-model-state-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("server.toml");
        setup::save(
            &path,
            toml::Table::from_iter([
                ("disable-stt".into(), toml::Value::Boolean(true)),
                ("disable-llm".into(), toml::Value::Boolean(true)),
                ("ctx-size".into(), toml::Value::Integer(8192)),
                ("chat-template".into(), toml::Value::String("custom".into())),
            ]),
        )
        .unwrap();
        let same = model_settings(
            &path,
            &Runtime::default(),
            &ModelState {
                name: "qwen2.5-7b-instruct-q4_k_m".into(),
                loaded: true,
            },
        )
        .unwrap();
        assert_eq!(same["chat-template"].as_str(), Some("custom"));
        let values = model_settings(
            &path,
            &Runtime::default(),
            &ModelState {
                name: "qwen3-8b-q4_k_m".into(),
                loaded: true,
            },
        )
        .unwrap();
        assert_eq!(values["llm-model"].as_str(), Some("qwen3-8b"));
        assert_eq!(values["disable-llm"].as_bool(), Some(false));
        assert_eq!(values["disable-stt"].as_bool(), Some(true));
        assert_eq!(values["ctx-size"].as_integer(), Some(8192));
        assert!(!values.contains_key("chat-template"));
        assert!(
            model_settings(
                &path,
                &Runtime::default(),
                &ModelState {
                    name: "not-a-model".into(),
                    loaded: true
                }
            )
            .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn disabled_components_do_not_download_or_verify_model_files() {
        let root = std::env::temp_dir().join(format!("openatc-no-models-{}", std::process::id()));
        let (_, manifest, status) = crate::ensure_models(
            root.clone(),
            crate::synthesis::Provider::Kokoro,
            crate::llm_config::Model::Qwen25,
            None,
            vec!["llm", "stt", "tts"],
        )
        .await
        .unwrap();
        assert!(!manifest.model.is_empty());
        assert!(status.values().all(|state| state == "unloaded"));
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }
}
