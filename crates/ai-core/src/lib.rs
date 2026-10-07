//! Model management: pinned manifest, verified paths, resumable downloads.
//!
//! The manifest (`models.toml`) pins where each file comes from. Hashes and
//! sizes are recorded on first successful fetch into a lockfile next to the
//! models (`models.lock.toml` in the state directory), so later starts only
//! verify instead of downloading. A file that fails verification is fetched
//! again from scratch.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// One downloadable model file, as listed in `models.toml`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ModelEntry {
    /// Short name used in logs and the lockfile (`qwen2.5-7b-q4_k_m`).
    pub name: String,
    /// Location under the models directory (`llm/qwen2.5-7b-q4_k_m.gguf`).
    pub path: String,
    /// Download URL (supports `Range` resume).
    pub url: String,
    /// Expected size in bytes. Zero means unknown (size check skipped).
    #[serde(default)]
    pub size: u64,
    /// Expected lowercase hex sha256. Empty means trust-on-first-use:
    /// the observed hash is recorded in the lockfile and enforced after.
    #[serde(default)]
    pub sha256: String,
}

/// Whole `models.toml` file.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Manifest {
    /// Model files to fetch, in download order.
    #[serde(default)]
    pub model: Vec<ModelEntry>,
}

impl Manifest {
    /// Parse manifest text (used for the in-repo `models.toml`).
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        Ok(toml::from_str(text)?)
    }
}

/// Hashes and sizes observed on first fetch, enforced on later starts.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Lockfile {
    /// Records by model name.
    #[serde(default)]
    pub model: BTreeMap<String, LockedModel>,
}

/// Lockfile record for one model file.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LockedModel {
    /// Observed lowercase hex sha256.
    pub sha256: String,
    /// Observed size in bytes.
    pub size: u64,
}

/// Manifest, downloader, and lockfile problems.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// Manifest text did not parse.
    #[error("cannot parse manifest: {0}")]
    Parse(#[from] toml::de::Error),
    /// Network or filesystem failure during download.
    #[error("download failed for {name}: {problem}")]
    Download {
        /// Manifest entry being fetched.
        name: String,
        /// What went wrong.
        problem: String,
    },
    /// File present but failed size or hash verification.
    #[error("verification failed for {name}: {problem}")]
    Verify {
        /// Manifest entry being checked.
        name: String,
        /// What went wrong.
        problem: String,
    },
    /// Lockfile could not be read or written.
    #[error("lockfile error: {0}")]
    Lock(String),
}

/// Absolute path of a manifest entry under the models directory.
#[must_use]
pub fn resolve(models_dir: &Path, entry: &ModelEntry) -> PathBuf {
    models_dir.join(&entry.path)
}

fn hash_file(path: &Path) -> Result<String, std::io::Error> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Hash bytes for lockfile records.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(bytes);
    format!("{:x}", hash.finalize())
}

/// Check an on-disk file against the manifest entry and lockfile record.
/// Returns true when the file is usable as-is.
pub fn verify(
    path: &Path,
    entry: &ModelEntry,
    locked: Option<&LockedModel>,
) -> Result<bool, ManifestError> {
    let Ok(metadata) = std::fs::metadata(path) else {
        return Ok(false);
    };
    let expected_size = if entry.size > 0 {
        entry.size
    } else {
        locked.map_or(0, |record| record.size)
    };
    if expected_size > 0 && metadata.len() != expected_size {
        return Ok(false);
    }
    let expected_hash = if entry.sha256.is_empty() {
        locked.map_or(String::new(), |record| record.sha256.clone())
    } else {
        entry.sha256.clone()
    };
    if expected_hash.is_empty() {
        return Ok(metadata.len() > 0);
    }
    match hash_file(path) {
        Ok(actual) => Ok(actual == expected_hash),
        Err(_) => Ok(false),
    }
}

/// Download one entry with resume, then verify it. Reports progress as
/// `(downloaded_bytes, total_bytes_or_zero)`; totals are zero when the
/// server omits `Content-Length`.
pub fn ensure(
    models_dir: &Path,
    entry: &ModelEntry,
    locked: Option<&LockedModel>,
    mut progress: impl FnMut(u64, u64),
) -> Result<PathBuf, ManifestError> {
    let path = resolve(models_dir, entry);
    if verify(&path, entry, locked).map_err(|error| ManifestError::Verify {
        name: entry.name.clone(),
        problem: error.to_string(),
    })? {
        return Ok(path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| ManifestError::Download {
            name: entry.name.clone(),
            problem: error.to_string(),
        })?;
    }
    let part_path = path.with_extension("part");
    let resume_from = std::fs::metadata(&part_path).map_or(0, |meta| meta.len());
    let client = reqwest::blocking::Client::new();
    let mut request = client.get(&entry.url);
    if resume_from > 0 {
        request = request.header("Range", format!("bytes={resume_from}-"));
    }
    let mut response = request.send().map_err(|error| ManifestError::Download {
        name: entry.name.clone(),
        problem: error.to_string(),
    })?;
    if resume_from > 0 && response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        std::fs::remove_file(&part_path).ok();
        return ensure(models_dir, entry, locked, progress);
    }
    if !response.status().is_success() {
        return Err(ManifestError::Download {
            name: entry.name.clone(),
            problem: format!("HTTP {}", response.status()),
        });
    }
    let total = response.content_length().unwrap_or(0);
    let mut file = File::options()
        .create(true)
        .append(true)
        .open(&part_path)
        .map_err(|error| ManifestError::Download {
            name: entry.name.clone(),
            problem: error.to_string(),
        })?;
    // If the server ignored our range, restart the file instead of appending.
    if resume_from > 0 {
        file.seek(SeekFrom::Start(resume_from))
            .map_err(|error| ManifestError::Download {
                name: entry.name.clone(),
                problem: error.to_string(),
            })?;
    }
    let mut downloaded = resume_from;
    let mut last_report = 0u64;
    progress(downloaded, total.saturating_add(resume_from));
    let mut buffer = [0u8; 8192];
    loop {
        let count = response
            .read(&mut buffer)
            .map_err(|error| ManifestError::Download {
                name: entry.name.clone(),
                problem: error.to_string(),
            })?;
        if count == 0 {
            break;
        }
        file.write_all(&buffer[..count])
            .map_err(|error| ManifestError::Download {
                name: entry.name.clone(),
                problem: error.to_string(),
            })?;
        downloaded += count as u64;
        // Report at most every 32 MB so logs stay readable.
        if downloaded.saturating_sub(last_report) >= 32 * 1024 * 1024 {
            last_report = downloaded;
            progress(downloaded, total.saturating_add(resume_from));
        }
    }
    progress(downloaded, total.saturating_add(resume_from));
    drop(file);
    std::fs::rename(&part_path, &path).map_err(|error| ManifestError::Download {
        name: entry.name.clone(),
        problem: error.to_string(),
    })?;
    if verify(&path, entry, locked).map_err(|error| ManifestError::Verify {
        name: entry.name.clone(),
        problem: error.to_string(),
    })? {
        return Ok(path);
    }
    Err(ManifestError::Verify {
        name: entry.name.clone(),
        problem: "downloaded file failed verification".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"
[[model]]
name = "tiny-test"
path = "stt/ggml-tiny.bin"
url = "https://example.com/ggml-tiny.bin"
size = 5
sha256 = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
"#;

    #[test]
    fn manifest_parses() {
        let manifest = Manifest::parse(MANIFEST).unwrap();
        assert_eq!(manifest.model.len(), 1);
        assert_eq!(manifest.model[0].path, "stt/ggml-tiny.bin");
    }

    #[test]
    fn resolve_joins_models_dir() {
        let manifest = Manifest::parse(MANIFEST).unwrap();
        let path = resolve(Path::new("/data/models"), &manifest.model[0]);
        assert_eq!(path, PathBuf::from("/data/models/stt/ggml-tiny.bin"));
    }

    #[test]
    fn verify_accepts_matching_file() {
        let dir = std::env::temp_dir().join("openatc-ai-core-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hello.bin");
        std::fs::write(&path, b"hello").unwrap();
        let manifest = Manifest::parse(MANIFEST).unwrap();
        // "hello" is 5 bytes with the sha256 pinned above.
        assert!(verify(&path, &manifest.model[0], None).unwrap());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn verify_rejects_missing_file() {
        let manifest = Manifest::parse(MANIFEST).unwrap();
        assert!(
            !verify(
                Path::new("/nonexistent/ggml-tiny.bin"),
                &manifest.model[0],
                None
            )
            .unwrap()
        );
    }
}
