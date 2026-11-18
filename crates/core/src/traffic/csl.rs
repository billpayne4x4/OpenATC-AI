//! Installed CSL metadata and deterministic aircraft/operator matching.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
};

/// One advertised aircraft/operator/livery combination.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Match {
    /// ICAO aircraft type from the package.
    pub aircraft: String,
    /// ICAO operator, or empty for an unmarked model.
    pub operator: String,
    /// Optional registration or special paint identifier.
    pub livery: String,
}

/// All object parts belong to one model and must be loaded together.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Model {
    /// Package-qualified model identity.
    pub id: String,
    /// Canonical paths to every required object part.
    pub objects: Vec<PathBuf>,
    /// Published wheel-to-origin correction, metres.
    pub vertical_offset: Option<f64>,
    /// Type/operator/paint combinations advertised by this model.
    pub matches: Vec<Match>,
}

/// Cached model catalogue; scanning belongs on a CPU worker, never in a draw callback.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Catalogue {
    /// Complete, locally available OBJ8 models.
    pub models: Vec<Model>,
    /// Bounded diagnostics for entries that were skipped.
    pub warnings: Vec<String>,
}

#[derive(Default)]
struct Definition {
    name: String,
    paths: Vec<String>,
    offset: Option<f64>,
    matches: Vec<Match>,
    unsupported: bool,
}

impl Catalogue {
    /// Read installed packages only. Symlinks and paths outside the CSL root are excluded.
    /// # Panics
    ///
    /// Panics if a catalogue entry is shorter than the documented field layout.
    pub fn scan(root: &Path) -> Result<Self, String> {
        let root = root
            .canonicalize()
            .map_err(|e| format!("CSL folder unavailable: {e}"))?;
        let mut pending = vec![(root.clone(), 0)];
        let mut files = Vec::new();
        let mut visited = 0;
        while let Some((dir, depth)) = pending.pop() {
            for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                visited += 1;
                if visited > 100_000 {
                    return Err("CSL catalogue scan exceeds the file limit".into());
                }
                let kind = entry.file_type().map_err(|e| e.to_string())?;
                if kind.is_dir() && depth < 8 {
                    pending.push((entry.path(), depth + 1));
                } else if kind.is_file()
                    && entry
                        .file_name()
                        .to_string_lossy()
                        .eq_ignore_ascii_case("xsb_aircraft.txt")
                {
                    if entry.metadata().map_err(|e| e.to_string())?.len() > 4 * 1024 * 1024 {
                        return Err("CSL metadata exceeds the size limit".into());
                    }
                    files.push(entry.path());
                }
            }
        }
        files.sort();
        let mut exports = BTreeMap::new();
        let mut sources = Vec::new();
        let mut metadata_bytes = 0;
        for path in files {
            let mut text = String::new();
            std::fs::File::open(&path)
                .map_err(|e| e.to_string())?
                .take(4 * 1024 * 1024 + 1)
                .read_to_string(&mut text)
                .map_err(|e| e.to_string())?;
            metadata_bytes += text.len();
            if text.len() > 4 * 1024 * 1024 || metadata_bytes > 32 * 1024 * 1024 {
                return Err("CSL metadata exceeds the catalogue memory budget".into());
            }
            let names: Vec<_> = text
                .lines()
                .filter_map(|line| {
                    let (key, value) = line.trim().split_once(char::is_whitespace)?;
                    (key == "EXPORT_NAME").then(|| value.trim().to_owned())
                })
                .filter(|s| !s.is_empty())
                .collect();
            for name in &names {
                exports
                    .entry(name.clone())
                    .or_insert_with(|| path.parent().unwrap().to_path_buf());
            }
            if let Some(name) = names.first() {
                sources.push((name.clone(), text));
            }
        }
        let mut result = Self::default();
        let mut ids = BTreeSet::new();
        for (package, text) in sources {
            for raw in definitions(&text) {
                let id = format!("{package}/{}", raw.name);
                if !ids.insert(id.clone()) {
                    continue;
                }
                if raw.unsupported || raw.matches.is_empty() || raw.paths.is_empty() {
                    result.warn(format!("{id}: incomplete metadata or texture override"));
                    continue;
                }
                let objects: Option<Vec<_>> = raw
                    .paths
                    .iter()
                    .map(|p| object_path(&root, &exports, p))
                    .collect();
                let Some(objects) = objects else {
                    result.warn(format!("{id}: object missing or outside CSL folder"));
                    continue;
                };
                result.models.push(Model {
                    id,
                    objects,
                    vertical_offset: raw.offset,
                    matches: raw.matches,
                });
                if result.models.len() > 20000 {
                    return Err("CSL catalogue exceeds the model limit".into());
                }
            }
        }
        Ok(result)
    }

    fn warn(&mut self, message: String) {
        if self.warnings.len() < 64 {
            self.warnings.push(message);
        }
    }

    /// Prefer an exact operator/livery, then an unmarked model of the same type.
    /// A known competing airline is not a fallback for a requested operator.
    #[must_use]
    pub fn choose(
        &self,
        aircraft: &str,
        operator: &str,
        livery: &str,
        seed: u64,
    ) -> Option<&Model> {
        let mut best = 0;
        let mut choices = Vec::new();
        for model in &self.models {
            let score = model
                .matches
                .iter()
                .filter(|m| m.aircraft.eq_ignore_ascii_case(aircraft))
                .filter(|m| m.operator.is_empty() || m.operator.eq_ignore_ascii_case(operator))
                .map(|m| {
                    1 + u8::from(!operator.is_empty() && m.operator.eq_ignore_ascii_case(operator))
                        * 2
                        + u8::from(!livery.is_empty() && m.livery.eq_ignore_ascii_case(livery))
                })
                .max()
                .unwrap_or(0);
            if score > best {
                best = score;
                choices.clear();
            }
            if score > 0 && score == best {
                choices.push(model);
            }
        }
        if choices.is_empty() {
            None
        } else {
            Some(choices[(seed % choices.len() as u64) as usize])
        }
    }
}

fn object_path(root: &Path, exports: &BTreeMap<String, PathBuf>, text: &str) -> Option<PathBuf> {
    let path = text.replace('\\', "/");
    let (package, relative) = path.split_once('/')?;
    if relative.split('/').any(|v| v == ".." || v.contains(':')) {
        return None;
    }
    let candidate = exports.get(package)?.join(relative).canonicalize().ok()?;
    (candidate.starts_with(root)
        && candidate.is_file()
        && candidate
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("obj")))
    .then(|| simplified(candidate))
}

/// Remove the Windows verbatim prefix that the simulator object loader rejects.
#[cfg(any(windows, test))]
fn strip_verbatim(text: &str) -> Option<String> {
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return Some(format!(r"\\{rest}"));
    }
    text.strip_prefix(r"\\?\").map(str::to_owned)
}

#[cfg(windows)]
fn simplified(path: PathBuf) -> PathBuf {
    strip_verbatim(&path.to_string_lossy()).map_or(path, PathBuf::from)
}

#[cfg(not(windows))]
fn simplified(path: PathBuf) -> PathBuf {
    path
}

fn definitions(text: &str) -> Vec<Definition> {
    let mut models = Vec::new();
    let mut current = Definition::default();
    for line in text.lines().map(str::trim).filter(|s| !s.starts_with('#')) {
        let fields: Vec<_> = line.split_whitespace().collect();
        match fields.as_slice() {
            ["OBJ8_AIRCRAFT", name @ ..] => {
                if !current.name.is_empty() {
                    models.push(std::mem::take(&mut current));
                }
                current.name = name.join(" ");
            }
            ["OBJ8", _, _, path] => current.paths.push((*path).into()),
            ["OBJ8", ..] => current.unsupported = true,
            ["VERT_OFFSET", value] | ["OFFSET", _, _, value] => {
                current.offset = value
                    .parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite() && (-10.0..=50.0).contains(v));
            }
            [
                "ICAO" | "AIRLINE" | "LIVERY" | "MATCHES",
                aircraft,
                rest @ ..,
            ] => {
                if current
                    .matches
                    .first()
                    .is_some_and(|m| m.aircraft != aircraft.to_ascii_uppercase())
                {
                    continue;
                }
                let operator = rest.first().copied().unwrap_or("");
                current.matches.push(Match {
                    aircraft: aircraft.to_ascii_uppercase(),
                    operator: if operator == "-" {
                        String::new()
                    } else {
                        operator.to_ascii_uppercase()
                    },
                    livery: rest.get(1..).unwrap_or_default().join(" "),
                });
            }
            _ => {}
        }
    }
    if !current.name.is_empty() {
        models.push(current);
    }
    models
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn verbatim_windows_paths_are_reduced_to_normal_paths() {
        assert_eq!(
            strip_verbatim(r"\\?\C:\Models\A320.obj").as_deref(),
            Some(r"C:\Models\A320.obj")
        );
        assert_eq!(
            strip_verbatim(r"\\?\UNC\server\share\A320.obj").as_deref(),
            Some(r"\\server\share\A320.obj")
        );
        assert_eq!(strip_verbatim(r"C:\Models\A320.obj"), None);
    }

    #[cfg(windows)]
    #[test]
    fn scanned_model_paths_are_not_verbatim() {
        let root = std::env::temp_dir().join(format!("openatc-csl-win-{}", std::process::id()));
        std::fs::create_dir_all(root.join("models")).unwrap();
        std::fs::write(root.join("models/aircraft.obj"), "A\n800\nOBJ\n").unwrap();
        std::fs::write(
            root.join("models/xsb_aircraft.txt"),
            "EXPORT_NAME planes\nOBJ8_AIRCRAFT A320_QFA\nOBJ8 SOLID YES planes/aircraft.obj\nAIRLINE A320 QFA\n",
        )
        .unwrap();
        let catalogue = Catalogue::scan(&root).unwrap();
        let text = catalogue.models[0].objects[0].to_string_lossy();
        assert!(!text.starts_with(r"\\?\"), "{text}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn package_parts_operator_matching_and_path_boundaries() {
        let root = std::env::temp_dir().join(format!("openatc-csl-{}", std::process::id()));
        std::fs::create_dir_all(root.join("models")).unwrap();
        std::fs::write(root.join("models/aircraft.obj"), "A\n800\nOBJ\n").unwrap();
        std::fs::write(root.join("models/xsb_aircraft.txt"), "EXPORT_NAME planes\nOBJ8_AIRCRAFT A320_QFA\nOBJ8 SOLID YES planes/aircraft.obj\nVERT_OFFSET 2.1\nAIRLINE A320 QFA\nOBJ8_AIRCRAFT A320_plain\nOBJ8 SOLID YES planes/aircraft.obj\nICAO A320\nOBJ8_AIRCRAFT bad\nOBJ8 SOLID YES planes/../outside.obj\nICAO PA28\n").unwrap();
        let c = Catalogue::scan(&root).unwrap();
        assert_eq!(c.models.len(), 2);
        assert!(
            c.choose("A320", "QFA", "", 123)
                .unwrap()
                .id
                .ends_with("A320_QFA")
        );
        assert!(
            c.choose("A320", "SIA", "", 123)
                .unwrap()
                .id
                .ends_with("A320_plain")
        );
        assert!(c.choose("B738", "QFA", "", 1).is_none());
        assert_eq!(c.warnings.len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "requires installed Bluebell CSL models"]
    fn installed_catalogue_scan_time_and_model_count_are_bounded() {
        let root = std::env::var("OPENATC_TEST_SIMULATOR_ROOT").unwrap();
        let path = std::path::Path::new(&root).join("Resources/plugins/OpenATC/Resources/CSL");
        let started = std::time::Instant::now();
        let catalogue = Catalogue::scan(&path).unwrap();
        let elapsed = started.elapsed();
        eprintln!(
            "CSL catalogue: {} models, {} warnings, scanned in {:.3}s",
            catalogue.models.len(),
            catalogue.warnings.len(),
            elapsed.as_secs_f64()
        );
        assert!(!catalogue.models.is_empty());
        assert!(
            elapsed < std::time::Duration::from_secs(60),
            "CSL scan exceeded its budget: {elapsed:?}"
        );
    }
}
