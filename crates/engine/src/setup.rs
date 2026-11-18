//! Rebuild only derived setup data. No simulator calls or live flight-state reset.
use axum::{
    Json,
    extract::State,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::Read,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    run_id: u64,
    first_run: bool,
    reset: bool,
    running: bool,
    phase: String,
    files: usize,
    completed: usize,
    total: usize,
    current_file: String,
    errors: Vec<String>,
}
static PROGRESS: OnceLock<Mutex<Progress>> = OnceLock::new();
fn progress() -> &'static Mutex<Progress> {
    PROGRESS.get_or_init(Mutex::default)
}
fn phase(text: &str) {
    progress()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .phase = text.into();
}
fn begin(first_run: bool, reset: bool) -> bool {
    let mut status = progress()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if status.running {
        return false;
    }
    let run_id = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
    .max(status.run_id.saturating_add(1));
    *status = Progress {
        run_id,
        first_run,
        reset,
        running: true,
        phase: "Waiting for setup worker".into(),
        ..Default::default()
    };
    true
}
fn file_progress(completed: usize, total: usize, name: &str) {
    let mut status = progress()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    status.completed = completed;
    status.total = total;
    status.current_file = name.into();
}

// These are the distributed plugin configuration locations, not the user's whole config tree.
const FOLDERS: &[&str] = &[
    "aircraft",
    "prompts",
    "speech",
    "missions",
    "cabin",
    "traffic",
    "custom-restrictions",
];
const ROOT_FILES: &[&str] = &[
    "intents.toml",
    "regions.toml",
    "radio-stations.toml",
    "deviations.toml",
];
const BUNDLED: &[&str] = &[
    include_str!("../../../intents.toml"),
    include_str!("../../../regions.toml"),
    include_str!("../../../radio-stations.toml"),
    include_str!("../../../deviations.toml"),
    include_str!("../../../speech/runtime/responses.toml"),
    include_str!("../../../prompts/prompts.toml"),
    include_str!("../../../prompts/interaction.toml"),
    include_str!("../../../cabin/behaviour.toml"),
    include_str!("../../../cabin/response-policy.toml"),
    include_str!("../../../traffic/aircraft.toml"),
    include_str!("../../../traffic/airlines.toml"),
    include_str!("../../../traffic/hubs.toml"),
    include_str!("../../../traffic/routes.toml"),
    include_str!("../../../traffic/wake.toml"),
    include_str!("../../../traffic/operations.toml"),
    include_str!("../../../traffic/handling.toml"),
    include_str!("../../../traffic/animation-channels.toml"),
    include_str!("../../../traffic/behaviour.toml"),
    include_str!("../../../traffic/vertical.toml"),
    include_str!("../../../aircraft/callouts.toml"),
    include_str!("../../../aircraft/transponder.toml"),
    include_str!("../../../custom-restrictions/example.toml"),
];
fn roots(config: &Path, executable: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for base in [
        executable.parent().unwrap_or(executable),
        executable,
        config,
    ] {
        roots.extend(FOLDERS.iter().chain(ROOT_FILES).map(|name| base.join(name)));
    }
    for name in [
        "OPENATC_TRAFFIC_DIR",
        "OPENATC_PROMPTS_DIR",
        "OPENATC_SPEECH_DIR",
        "OPENATC_REGIONS_FILE",
    ] {
        if let Some(path) = std::env::var_os(name) {
            roots.push(PathBuf::from(path));
        }
    }
    roots
}
fn collect(
    path: &Path,
    depth: usize,
    visited: &mut BTreeSet<PathBuf>,
    files: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let canonical = match path.canonicalize() {
        Ok(path) => path,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    if !visited.insert(canonical.clone()) {
        return Ok(());
    }
    if depth > 8 || visited.len() > 8192 {
        return Err("Setup configuration tree exceeds its scan budget".into());
    }
    if canonical.is_dir() {
        for entry in
            std::fs::read_dir(&canonical).map_err(|e| format!("{}: {e}", path.display()))?
        {
            collect(
                &entry.map_err(|e| e.to_string())?.path(),
                depth + 1,
                visited,
                files,
            )?;
        }
    } else if canonical.extension().is_some_and(|ext| ext == "toml") {
        files.push(canonical);
    }
    Ok(())
}
fn populate(
    paths: &[PathBuf],
    parse: impl Fn(&str) -> Result<toml::Value, toml::de::Error>,
) -> Result<usize, String> {
    let mut visited = BTreeSet::new();
    let mut files = Vec::new();
    for path in paths {
        collect(path, 0, &mut visited, &mut files)?;
    }
    files.sort();
    phase("Caching configuration files");
    let count = files.len() + BUNDLED.len();
    file_progress(0, count, "Bundled defaults");
    for (index, text) in BUNDLED.iter().enumerate() {
        parse(text).map_err(|e| format!("Bundled TOML: {e}"))?;
        file_progress(index + 1, count, "Bundled defaults");
    }
    let mut total = 0;
    for (index, path) in files.iter().enumerate() {
        let mut text = String::new();
        std::fs::File::open(path)
            .and_then(|f| f.take(32 * 1024 * 1024 + 1).read_to_string(&mut text))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        total += text.len();
        if text.len() > 32 * 1024 * 1024 || total > 128 * 1024 * 1024 {
            return Err(format!(
                "{}: TOML setup exceeds its size budget",
                path.display()
            ));
        }
        file_progress(BUNDLED.len() + index, count, &path.display().to_string());
        parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        file_progress(
            BUNDLED.len() + index + 1,
            count,
            &path.display().to_string(),
        );
    }
    Ok(files.len())
}
fn finish(result: Result<usize, String>, reset: bool) {
    let result = result.and_then(|files| {
        let error = crate::config_cache::status().error;
        if !error.is_empty() {
            return Err(error);
        }
        crate::config_cache::finish_setup()?;
        Ok(files)
    });
    let mut status = progress()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    status.running = false;
    match result {
        Ok(files) => {
            status.files = files;
            status.first_run = false;
            status.reset = false;
            status.phase=if reset {"Setup caches rebuilt. Scenery rebuilds on demand. Restart the plugin to reload all active policies."} else {"TOML setup cache ready"}.into();
            status.errors.clear();
            status.current_file.clear();
        }
        Err(error) => {
            status.phase =
                "Setup cache rebuild incomplete; fix the reported error and retry".into();
            status.errors = vec![error];
        }
    }
}
fn initial(config: &Path, executable: &Path) {
    if !crate::config_cache::available() {
        finish(Err("TOML cache unavailable; parsing configuration directly. Fix the cache directory and retry Re-setup.".into()), false);
        return;
    }
    phase("Preparing TOML setup cache");
    let result = populate(&roots(config, executable), crate::config_cache::parse);
    if let Err(error) = &result {
        eprintln!("Setup cache: {error}");
    }
    finish(result, false);
}
pub async fn start_initial(shared: &crate::Shared) {
    let (config, executable, workers) = {
        let state = shared.read().await;
        (
            state.config_dir.clone(),
            state.exe_dir.clone(),
            state.workers.clone(),
        )
    };
    // The status endpoint remains responsive while the bounded worker scans files.
    begin(!crate::config_cache::setup_complete(), false);
    tokio::spawn(async move {
        if let Err(error) = workers.run(move || initial(&config, &executable)).await {
            finish(Err(error), false);
        }
    });
}
pub async fn status(State(_): State<crate::Shared>, Json(_): Json<Value>) -> Json<Value> {
    let status = progress()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    Json(json!({"setup":status,"toml":crate::config_cache::status()}))
}
pub async fn rerun(State(shared): State<crate::Shared>, Json(_): Json<Value>) -> Response {
    let (config, executable, root, workers) = {
        let state = shared.read().await;
        if !state.settings.dev_mode {
            return crate::routes::bad_request("Enable debugging tools before re-running setup");
        }
        (
            state.config_dir.clone(),
            state.exe_dir.clone(),
            state.settings.simulator_root.clone(),
            state.workers.clone(),
        )
    };
    if !begin(false, true) {
        return Json(json!({"accepted":false,"running":true})).into_response();
    }
    tokio::spawn(async move {
        let result = workers
            .run(move || {
                phase("Dropping and recreating TOML cache tables");
                crate::config_cache::rebuild()?;
                phase("Dropping and recreating scenery cache tables");
                crate::generated::scenery::cache::rebuild(Path::new(&root))?;
                populate(&roots(&config, &executable), crate::config_cache::parse)
            })
            .await
            .and_then(|value| value);
        finish(result, true);
    });
    Json(json!({"accepted":true})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn setup_reimports_edited_sources_and_preserves_user_files() {
        let dir = std::env::temp_dir().join(format!(
            "openatc-setup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("traffic")).unwrap();
        let source = dir.join("traffic/behaviour.toml");
        std::fs::write(&source, "chance=12").unwrap();
        for file in ["settings.json", "plan.json", "log.txt"] {
            std::fs::write(dir.join(file), "keep me").unwrap();
        }
        let cache = crate::config_cache::Cache::open(&dir.join("cache.sqlite3")).unwrap();
        assert_eq!(
            populate(&[dir.join("traffic")], |text| cache.parse(text)).unwrap(),
            1
        );
        std::fs::write(&source, "chance=35").unwrap();
        cache.rebuild().unwrap();
        assert_eq!(
            populate(&[dir.join("traffic")], |text| cache.parse(text)).unwrap(),
            1
        );
        assert_eq!(
            cache
                .parse(&std::fs::read_to_string(&source).unwrap())
                .unwrap()["chance"]
                .as_integer(),
            Some(35)
        );
        for file in ["settings.json", "plan.json", "log.txt"] {
            assert_eq!(std::fs::read_to_string(dir.join(file)).unwrap(), "keep me");
        }
        std::fs::write(&source, "chance=[").unwrap();
        cache.rebuild().unwrap();
        assert!(
            populate(&[dir.join("traffic")], |text| cache.parse(text))
                .unwrap_err()
                .contains("behaviour.toml")
        );
        drop(cache);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
