//! Download the publisher's Bluebell archives without blocking the simulator.
use axum::{
    Json,
    extract::State as AxumState,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::io::AsyncWriteExt;

// DDS downloads linked by https://github.com/oktal3700/bluebell.
// Keep the publisher's files on their server; textures are not ours to redistribute.
const ARCHIVES: &[(&str, &str)] = &[
    ("BB_Airbus1.zip", "1XNASfPh9siS8jSxpe5PE-nIjiTZCN47a"),
    ("BB_Airbus2.zip", "1k_17LvxPOxyL8m3DKc-llnz4Ln4eHCYb"),
    ("BB_Boeing1.zip", "1X4MYMA1sUP1R6VN81s8XiLHxyXjobfLm"),
    ("BB_Boeing2.zip", "16tIMzcAevLb14zC01_CTFEWBreytHz7U"),
    ("BB_Boeing3.zip", "1q8k_HAfJEXJdcE7NIFiAhtkNNzCCh97_"),
    ("BB_GA.zip", "1quvHl4qer1LAuP9Du3RYPJyLD4oLJ8gd"),
    ("BB_Heli.zip", "16tlrAhL_kh0HyJt331-gMKcfB9Bgalao"),
    ("BB_Jets.zip", "1DO9ilqH7Fw2Y1FcuSwZchxYplhjLzBlP"),
    ("BB_Mil.zip", "1HgXmHlQavg5R_nqqoEkFiMFM9yqVWpE5"),
    ("BB_Props.zip", "1VC5KpK2xnO2FVBfwcWXI0ktxgAfg7d8l"),
    ("BB_Repaints.zip", "1y_MXXf2VnVk-uG1H75ZFFiPiPSsFVLMW"),
    ("BB_Virtual.zip", "1HOyblLLMzhltbYglnoFmfglQ4lGFL1BP"),
];
const RECEIPT: &str = ".openatc-bluebell.json";
const MAX_DOWNLOAD: u64 = 4 * 1024 * 1024 * 1024;
const MAX_UNPACKED: u64 = 16 * 1024 * 1024 * 1024;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Progress {
    stage: String,
    archive: String,
    completed: usize,
    archive_count: usize,
    downloaded: u64,
    total: Option<u64>,
    extracted: u64,
    files: usize,
    objects: usize,
    packages: usize,
    path: String,
    error: String,
}
struct CachedCatalogue {
    root: PathBuf,
    data: Arc<openatc_core::traffic::csl::Catalogue>,
}

#[derive(Clone, Default)]
pub struct Installer {
    progress: Arc<Mutex<Progress>>,
    running: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    catalogue: Arc<tokio::sync::Mutex<Option<CachedCatalogue>>>,
}
impl Installer {
    pub async fn models(
        &self,
        root: &str,
        workers: &crate::workers::Workers,
    ) -> Result<Arc<openatc_core::traffic::csl::Catalogue>, String> {
        if self.running.load(Ordering::Acquire) {
            return Err("Model installation is running".into());
        }
        let path = model_source(root)?;
        let mut cache = self.catalogue.lock().await;
        if cache.as_ref().is_none_or(|cached| cached.root != path) {
            let source = path.clone();
            let data = workers
                .run(move || openatc_core::traffic::csl::Catalogue::scan(&source))
                .await??;
            if self.running.load(Ordering::Acquire) {
                return Err("Model installation changed the catalogue".into());
            }
            *cache = Some(CachedCatalogue {
                root: path,
                data: Arc::new(data),
            });
        }
        Ok(cache.as_ref().unwrap().data.clone())
    }
    fn update(&self, f: impl FnOnce(&mut Progress)) {
        f(&mut self
            .progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner));
    }
    fn snapshot(&self) -> Progress {
        self.progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    fn check_cancelled(&self) -> Result<(), String> {
        if self.cancelled.load(Ordering::Acquire) {
            Err("Download cancelled".into())
        } else {
            Ok(())
        }
    }
}
/// Writable model store outside the simulator folder.
fn location(root: &str) -> Result<PathBuf, String> {
    let root = Path::new(root);
    if !root.join("Resources/plugins").is_dir() {
        return Err("Set the X-Plane folder before installing traffic models.".into());
    }
    Ok(crate::data_dir().join("csl"))
}

/// Older installs kept models in the simulator folder; keep reading them.
fn legacy_location(root: &str) -> PathBuf {
    Path::new(root).join("Resources/plugins/OpenATC/Resources/CSL")
}

/// The installed catalogue, preferring the writable store.
fn model_source(root: &str) -> Result<PathBuf, String> {
    let current = location(root)?;
    if current.join("Bluebell").is_dir() {
        return Ok(current);
    }
    let legacy = legacy_location(root);
    if legacy.join("Bluebell").is_dir() {
        return Ok(legacy);
    }
    Ok(current)
}
fn failure(message: &str) -> Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        Json(json!({"error":message})),
    )
        .into_response()
}
pub async fn status(AxumState(shared): AxumState<crate::Shared>, Json(_): Json<Value>) -> Response {
    let (installer, root) = {
        let s = shared.read().await;
        (s.model_installer.clone(), s.settings.simulator_root.clone())
    };
    if !installer.running.load(Ordering::Acquire) && installer.snapshot().stage == "installed" {
        let snapshot = installer.snapshot();
        if !Path::new(&snapshot.path).join(RECEIPT).is_file()
            || model_source(&root)
                .is_ok_and(|base| base.join("Bluebell") != Path::new(&snapshot.path))
        {
            installer.update(|p| *p = Progress::default());
        }
    }
    if !installer.running.load(Ordering::Acquire)
        && installer.snapshot().stage.is_empty()
        && let Ok(base) = model_source(&root)
        && let Ok(bytes) = tokio::fs::read(base.join("Bluebell").join(RECEIPT)).await
        && let Ok(mut saved) = serde_json::from_slice::<Progress>(&bytes)
    {
        saved.path = base.join("Bluebell").to_string_lossy().into_owned();
        installer.update(|p| *p = saved);
    }
    Json(installer.snapshot()).into_response()
}

/// Index once per explicit scan or installation, using the shared CPU budget.
pub async fn index(
    AxumState(shared): AxumState<crate::Shared>,
    Json(body): Json<Value>,
) -> Response {
    let (installer, root, workers) = {
        let s = shared.read().await;
        (
            s.model_installer.clone(),
            s.settings.simulator_root.clone(),
            s.workers.clone(),
        )
    };
    let path = match model_source(&root) {
        Ok(v) => v,
        Err(e) => return failure(&e),
    };
    let mut cache = installer.catalogue.lock().await;
    if installer.running.load(Ordering::Acquire) {
        return failure("Model installation is still running. Scan again when it finishes.");
    }
    if body["refresh"] == true || cache.as_ref().is_none_or(|c| c.root != path) {
        let source = path.clone();
        match workers
            .run(move || openatc_core::traffic::csl::Catalogue::scan(&source))
            .await
        {
            Ok(Ok(catalogue)) => {
                if installer.running.load(Ordering::Acquire) {
                    return failure(
                        "Model installation started during the scan. Scan again when it finishes.",
                    );
                }
                *cache = Some(CachedCatalogue {
                    root: path,
                    data: Arc::new(catalogue),
                });
            }
            Ok(Err(e)) | Err(e) => return failure(&e),
        }
    }
    let catalogue = &cache.as_ref().unwrap().data;
    let mut types = std::collections::BTreeSet::new();
    let mut operators = std::collections::BTreeSet::new();
    for model in &catalogue.models {
        for m in &model.matches {
            types.insert(&m.aircraft);
            if !m.operator.is_empty() {
                operators.insert(&m.operator);
            }
        }
    }
    Json(
        json!({"models":catalogue.models.len(),"types":types,"operators":operators,
        "warnings":catalogue.warnings,"cpuWorkers":workers.count()}),
    )
    .into_response()
}
pub async fn install(
    AxumState(shared): AxumState<crate::Shared>,
    Json(_): Json<Value>,
) -> Response {
    let (installer, root) = {
        let s = shared.read().await;
        (s.model_installer.clone(), s.settings.simulator_root.clone())
    };
    let base = match location(&root) {
        Ok(v) => v,
        Err(e) => return failure(&e),
    };
    if installer
        .running
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Json(installer.snapshot()).into_response();
    }
    *installer.catalogue.lock().await = None;
    installer.cancelled.store(false, Ordering::Release);
    installer.update(|p| {
        *p = Progress {
            stage: "preparing".into(),
            archive_count: ARCHIVES.len(),
            path: base.join("Bluebell").to_string_lossy().into_owned(),
            ..Default::default()
        }
    });
    let job = installer.clone();
    tokio::spawn(async move {
        let result = run(&job, &base).await;
        if let Err(error) = result {
            job.update(|p| {
                p.stage = if job.cancelled.load(Ordering::Acquire) {
                    "cancelled"
                } else {
                    "failed"
                }
                .into();
                p.error = error;
            });
        }
        *job.catalogue.lock().await = None;
        job.running.store(false, Ordering::Release);
    });
    Json(installer.snapshot()).into_response()
}
pub async fn cancel(AxumState(shared): AxumState<crate::Shared>, Json(_): Json<Value>) -> Response {
    let installer = shared.read().await.model_installer.clone();
    installer.cancelled.store(true, Ordering::Release);
    if installer.running.load(Ordering::Acquire) {
        installer.update(|p| p.stage = "cancelling".into());
    }
    Json(json!({"accepted":true})).into_response()
}
async fn run(job: &Installer, base: &Path) -> Result<(), String> {
    tokio::fs::create_dir_all(base)
        .await
        .map_err(|e| e.to_string())?;
    let stage = base.join(".bluebell-installing");
    let cache = base.join(".bluebell-downloads");
    for path in [&stage, &cache, &base.join("Bluebell")] {
        if tokio::fs::symlink_metadata(path)
            .await
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err("The model installation folders must not be symbolic links.".into());
        }
    }
    if stage.exists() {
        tokio::fs::remove_dir_all(&stage)
            .await
            .map_err(|e| e.to_string())?;
    }
    tokio::fs::create_dir_all(&stage)
        .await
        .map_err(|e| e.to_string())?;
    tokio::fs::create_dir_all(&cache)
        .await
        .map_err(|e| e.to_string())?;
    let client = reqwest::Client::builder()
        .user_agent("OpenATC-AI/Bluebell-installer")
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let result = async {
        for (index, (name, id)) in ARCHIVES.iter().enumerate() {
            job.check_cancelled()?;
            job.update(|p| { p.stage = "downloading".into(); p.archive = (*name).into(); p.completed = index; p.downloaded = 0; p.total = None; });
            let archive = cache.join(name);
            if archive.is_file() {
                let len = tokio::fs::metadata(&archive).await.map_err(|e| e.to_string())?.len();
                job.update(|p| { p.downloaded = len; p.total = Some(len); });
            } else {
                let url = format!("https://drive.usercontent.google.com/download?id={id}&export=download&confirm=t");
                download(job, &client, &url, &archive).await?;
            }
            job.update(|p| p.stage = "extracting".into());
            let task_job = job.clone();
            let destination = stage.clone();
            let source = archive.clone();
            let result = tokio::task::spawn_blocking(move || extract(&task_job, &source, &destination)).await.map_err(|e| e.to_string())?;
            if result.is_err() && !job.cancelled.load(Ordering::Acquire) {
                // A corrupt cached archive must be downloaded again on retry.
                let _ = tokio::fs::remove_file(&archive).await;
            }
            result?;
            job.update(|p| p.completed = index + 1);
        }
        job.check_cancelled()?;
        if job.snapshot().objects == 0 || job.snapshot().packages == 0 {
            return Err("The download contained no usable CSL model packages.".into());
        }
        job.update(|p| p.stage = "installing".into());
        let mut saved = job.snapshot();
        saved.stage = "installed".into();
        saved.archive.clear();
        tokio::fs::write(stage.join(RECEIPT), serde_json::to_vec_pretty(&saved).map_err(|e| e.to_string())?).await.map_err(|e| e.to_string())?;
        let destination = base.join("Bluebell");
        let old = base.join(".bluebell-previous");
        if old.exists() { return Err("A previous Bluebell backup exists. Restore or move .bluebell-previous before retrying.".into()); }
        if destination.exists() {
            if !destination.join(RECEIPT).is_file() {
                return Err("A manually installed Bluebell folder already exists. Move it before using the installer.".into());
            }
            tokio::fs::rename(&destination, &old).await.map_err(|e| e.to_string())?;
        }
        if let Err(error) = tokio::fs::rename(&stage, &destination).await {
            if old.exists() { let _ = tokio::fs::rename(&old, &destination).await; }
            return Err(error.to_string());
        }
        job.update(|p| *p = saved);
        let _ = tokio::fs::remove_dir_all(&old).await;
        let _ = tokio::fs::remove_dir_all(&cache).await;
        Ok(())
    }.await;
    if result.is_err() {
        let _ = tokio::fs::remove_dir_all(&stage).await;
    }
    result
}
async fn download(
    job: &Installer,
    client: &reqwest::Client,
    url: &str,
    path: &Path,
) -> Result<(), String> {
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    if response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .is_some_and(|v| v.to_str().unwrap_or("").contains("text/"))
    {
        return Err("The publisher returned a web page instead of an archive. Its download quota may be exhausted; try again later.".into());
    }
    let total = response.content_length();
    if total.is_some_and(|n| n > MAX_DOWNLOAD) {
        return Err("Model archive exceeds the download size limit.".into());
    }
    job.update(|p| p.total = total);
    let partial = path.with_extension("part");
    let mut file = tokio::fs::File::create(&partial)
        .await
        .map_err(|e| e.to_string())?;
    let mut count = 0;
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        job.check_cancelled()?;
        count += chunk.len() as u64;
        if count > MAX_DOWNLOAD {
            return Err("Model archive exceeds the download size limit.".into());
        }
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        job.update(|p| p.downloaded = count);
    }
    if count == 0 || total.is_some_and(|n| n != count) {
        return Err("The model download was incomplete.".into());
    }
    file.flush().await.map_err(|e| e.to_string())?;
    drop(file);
    tokio::fs::rename(&partial, path)
        .await
        .map_err(|e| e.to_string())
}
fn extract(job: &Installer, archive: &Path, destination: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("Invalid model archive: {e}"))?;
    if zip.len() > 100_000 {
        return Err("Model archive contains too many files.".into());
    }
    for i in 0..zip.len() {
        job.check_cancelled()?;
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let enclosed = entry
            .enclosed_name()
            .ok_or("Unsafe path in model archive")?;
        if entry.is_symlink()
            || entry.name().contains('\\')
            || enclosed
                .components()
                .any(|p| !matches!(p, std::path::Component::Normal(_)))
        {
            return Err("Unsafe path in model archive".into());
        }
        let target = destination.join(enclosed);
        if entry.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
            continue;
        }
        if job.snapshot().extracted.saturating_add(entry.size()) > MAX_UNPACKED {
            return Err("Model pack exceeds the extraction size limit.".into());
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut output = std::fs::File::create(&target).map_err(|e| e.to_string())?;
        let bytes = std::io::copy(&mut entry, &mut output).map_err(|e| e.to_string())?;
        job.update(|p| {
            p.extracted += bytes;
            p.files += 1;
            if target.extension().is_some_and(|v| v == "obj") {
                p.objects += 1;
            }
            if target.file_name().is_some_and(|v| v == "xsb_aircraft.txt") {
                p.packages += 1;
            }
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn extracts_models_but_rejects_traversal_and_cancellation() {
        let root = std::env::temp_dir().join(format!("openatc-model-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let archive = root.join("models.zip");
        let make = |path: &str| {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
            zip.start_file(path, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"I\n800\nOBJ\n").unwrap();
            zip.finish().unwrap();
        };
        let job = Installer::default();
        make("BB_Test/A320/a320.obj");
        extract(&job, &archive, &root.join("out")).unwrap();
        assert_eq!(job.snapshot().objects, 1);
        assert!(root.join("out/BB_Test/A320/a320.obj").is_file());
        make("../escaped.obj");
        assert!(extract(&job, &archive, &root.join("out")).is_err());
        assert!(!root.join("escaped.obj").exists());
        make("BB_Test/A320/cancelled.obj");
        job.cancelled.store(true, Ordering::Release);
        assert!(extract(&job, &archive, &root.join("out")).is_err());
        assert!(!root.join("out/BB_Test/A320/cancelled.obj").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod download_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn cancelled_download_is_not_promoted_and_can_be_retried() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/models.zip", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = [0; 4096];
                assert!(socket.read(&mut buffer).await.unwrap() > 0);
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nPK123456").await.unwrap();
            }
        });
        let root =
            std::env::temp_dir().join(format!("openatc-download-test-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&root).await.unwrap();
        let destination = root.join("test.zip");
        let job = Installer::default();
        job.cancelled.store(true, Ordering::Release);
        let client = reqwest::Client::new();
        assert!(download(&job, &client, &url, &destination).await.is_err());
        assert!(!destination.exists());
        job.cancelled.store(false, Ordering::Release);
        download(&job, &client, &url, &destination).await.unwrap();
        assert_eq!(tokio::fs::read(&destination).await.unwrap(), b"PK123456");
        assert_eq!(job.snapshot().downloaded, 8);
        server.await.unwrap();
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
