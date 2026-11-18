//! Conservative library variants for obstacle bounds, including seasonal and regional alternatives.
use super::object;
#[allow(unused_imports)]
use std::fmt::Write as _;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Component, Path, PathBuf},
};

pub(super) struct Library {
    exports: BTreeMap<String, BTreeSet<PathBuf>>,
    incomplete: BTreeSet<String>,
    // Scenery definitions are shared by many placements and tiles, so resolved paths and
    // heights are cached for the life of one planning job.
    resources_cache: RefCell<ResourceCache>,
    solid_cache: RefCell<BTreeMap<String, Option<bool>>>,
    string_cache: RefCell<StringCache>,
    ground_reach_cache: RefCell<BTreeMap<(PathBuf, String), Option<f64>>>,
    forest_cache: RefCell<BTreeMap<(PathBuf, String), Option<f64>>>,
}

type ResourceCache = BTreeMap<(PathBuf, String), Option<Vec<PathBuf>>>;
type StringCache = BTreeMap<(PathBuf, String), Option<(f64, f64)>>;

impl Default for Library {
    fn default() -> Self {
        Self {
            exports: BTreeMap::new(),
            incomplete: BTreeSet::new(),
            resources_cache: RefCell::new(BTreeMap::new()),
            solid_cache: RefCell::new(BTreeMap::new()),
            string_cache: RefCell::new(BTreeMap::new()),
            ground_reach_cache: RefCell::new(BTreeMap::new()),
            forest_cache: RefCell::new(BTreeMap::new()),
        }
    }
}

pub(super) fn relative(text: &str) -> Result<&Path, String> {
    let path = Path::new(text);
    if text.is_empty()
        || !path
            .components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err("Scenery library resource leaves its package".into());
    }
    Ok(path)
}

/// Resolve a reference written beside a scenery definition.
///
/// Definitions may use Windows separators and climb out of their own directory with `..`, but
/// the result must stay inside the package that owns the definition.
pub(super) fn nested(base: &Path, text: &str) -> Result<PathBuf, String> {
    if text.is_empty() {
        return Err("Scenery definition reference is empty".into());
    }
    let normalized = text.replace('\\', "/");
    let path = Path::new(&normalized);
    let mut result = base.to_path_buf();
    for part in path.components() {
        match part {
            Component::Normal(name) => result.push(name),
            Component::CurDir => {}
            Component::ParentDir => {
                if !result.pop() {
                    return Err("Scenery definition reference leaves its package".into());
                }
            }
            _ => return Err("Scenery definition reference leaves its package".into()),
        }
    }
    let mut root = base.to_path_buf();
    let mut bounded = false;
    for _ in 0..8 {
        if root.join("library.txt").is_file()
            || case_insensitive(root.join("library.txt")).is_file()
        {
            bounded = true;
            break;
        }
        if !root.pop() {
            break;
        }
    }
    if !result.starts_with(if bounded { root } else { base.to_path_buf() }) {
        return Err("Scenery definition reference leaves its package".into());
    }
    Ok(case_insensitive(result))
}

// Path spelling is stable for the life of a planning job, and the same resource is looked up
// from many tiles and airport candidates.
thread_local! {
    static CASE_CACHE: RefCell<BTreeMap<PathBuf, PathBuf>> = const { RefCell::new(BTreeMap::new()) };
}

/// Resolve a path whose spelling differs from the file on disk only by letter case.
///
/// Scenery packages are authored on case-insensitive systems and some exports do not match the
/// shipped file name exactly. A unique case-insensitive match resolves to the same resource; an
/// ambiguous directory keeps the original path so the obstacle stays unresolved rather than
/// choosing the wrong file.
pub(super) fn case_insensitive(path: PathBuf) -> PathBuf {
    super::cache::observe(&path);
    let cacheable = !path.starts_with(std::env::temp_dir());
    if cacheable && let Some(resolved) = CASE_CACHE.with(|cache| cache.borrow().get(&path).cloned())
    {
        return resolved;
    }
    let resolved = case_insensitive_uncached(path.clone());
    if cacheable {
        CASE_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.len() < 200_000 {
                cache.insert(path, resolved.clone());
            }
        });
    }
    resolved
}

fn case_insensitive_uncached(path: PathBuf) -> PathBuf {
    if path.exists() {
        return path;
    }
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(name) => {
                let candidate = resolved.join(name);
                if candidate.exists() {
                    resolved = candidate;
                    continue;
                }
                let Some(name) = name.to_str() else {
                    return path;
                };
                super::cache::observe(&resolved);
                let Ok(entries) = std::fs::read_dir(&resolved) else {
                    return path;
                };
                let mut found = None;
                for (index, entry) in entries.flatten().enumerate() {
                    if index >= 65_536 {
                        return path;
                    }
                    let Some(candidate) = entry.file_name().to_str().map(str::to_owned) else {
                        continue;
                    };
                    if !candidate.eq_ignore_ascii_case(name) {
                        continue;
                    }
                    if found.is_some() {
                        return path;
                    }
                    found = Some(candidate);
                }
                let Some(found) = found else {
                    return path;
                };
                resolved.push(found);
            }
            _ => resolved.push(component.as_os_str()),
        }
    }
    resolved
}

impl Library {
    /// Cached definitions are valid for a planning job, but a test may rewrite a file in place
    /// under the temporary directory to exercise another branch.
    fn caching(pack: &Path) -> bool {
        !pack.starts_with(std::env::temp_dir())
    }

    pub fn load(root: &Path, packs: &[PathBuf]) -> Result<Self, String> {
        CASE_CACHE.with(|cache| cache.borrow_mut().clear());
        let mut folders = packs.iter().cloned().collect::<BTreeSet<_>>();
        let defaults = root.join("Resources/default scenery");
        super::cache::observe(&defaults);
        if defaults.is_dir() {
            for (index, entry) in std::fs::read_dir(defaults)
                .map_err(|e| e.to_string())?
                .enumerate()
            {
                if index >= 1024 {
                    return Err("Default scenery package budget exceeded".into());
                }
                let entry = entry.map_err(|e| e.to_string())?;
                if entry.path().is_dir() {
                    folders.insert(entry.path());
                }
            }
        }
        if folders.len() > 2048 {
            return Err("Scenery library package budget exceeded".into());
        }
        let mut result = Self::default();
        let mut bytes = 0;
        for folder in folders {
            let path = case_insensitive(folder.join("library.txt"));
            if !path.is_file() {
                continue;
            }
            let mut text = String::new();
            super::cache::open_file(path)
                .map_err(|e| e.to_string())?
                .take(8 * 1024 * 1024 + 1)
                .read_to_string(&mut text)
                .map_err(|e| e.to_string())?;
            bytes += text.len();
            if text.len() > 8 * 1024 * 1024 || bytes > 32 * 1024 * 1024 {
                return Err("Scenery library input budget exceeded".into());
            }
            result
                .add(&folder, &text)
                .map_err(|error| format!("{}: {error}", folder.display()))?;
        }
        Ok(result)
    }

    fn add(&mut self, folder: &Path, text: &str) -> Result<(), String> {
        let mut lines = text
            .lines()
            .map(|line| line.split('#').next().unwrap_or("").trim())
            .filter(|line| !line.is_empty());
        if !matches!(lines.next(), Some("A" | "I"))
            || !matches!(lines.next(), Some("800" | "1200"))
            || lines.next() != Some("LIBRARY")
        {
            return Err("Unsupported scenery library header".into());
        }
        for line in lines {
            let mut rest = line;
            let mut next = || {
                rest = rest.trim_start();
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                let token = &rest[..end];
                rest = &rest[end..];
                token
            };
            let command = next();
            if !command.starts_with("EXPORT") {
                continue;
            }
            if !matches!(
                command,
                "EXPORT"
                    | "EXPORT_RATIO"
                    | "EXPORT_EXTEND"
                    | "EXPORT_EXCLUDE"
                    | "EXPORT_BACKUP"
                    | "EXPORT_SEASON"
                    | "EXPORT_RATIO_SEASON"
                    | "EXPORT_EXTEND_SEASON"
                    | "EXPORT_EXCLUDE_SEASON"
            ) {
                return Err(format!("Unresolved scenery library directive {command}"));
            }
            if command.ends_with("_SEASON") {
                let seasons = next();
                if seasons.is_empty()
                    || !seasons
                        .split(',')
                        .all(|season| matches!(season, "spr" | "sum" | "fal" | "win"))
                {
                    return Err("Invalid library season list".into());
                }
            }
            if command.starts_with("EXPORT_RATIO") {
                let ratio = next()
                    .parse::<f64>()
                    .map_err(|_| "Invalid library export ratio")?;
                if !ratio.is_finite() || ratio < 0.0 {
                    return Err("Invalid library export ratio".into());
                }
            }
            // A leading slash belongs to the virtual namespace, never the host filesystem. Some
            // shipped libraries use Windows separators on either side of an export.
            let name = next().trim_start_matches('/').replace('\\', "/");
            relative(&name)?;
            if name.len() > 8192 || rest.len() > 8192 || self.exports.len() >= 200_000 {
                return Err("Scenery library export budget exceeded".into());
            }
            let destination = rest.trim().replace('\\', "/");
            let Ok(destination) = relative(&destination) else {
                self.incomplete.insert(name);
                continue;
            };
            let variants = self.exports.entry(name.clone()).or_default();
            let path = folder.join(destination);
            if variants.len() >= 256 && !variants.contains(&path) {
                self.incomplete.insert(name);
                continue;
            }
            // Keeping lower-priority and inactive variants can reject a path, but cannot shrink a building.
            variants.insert(path);
        }
        Ok(())
    }

    pub fn resources(&self, pack: &Path, name: &str) -> Result<Vec<PathBuf>, String> {
        let key = (pack.to_path_buf(), name.to_owned());
        if !Self::caching(pack) {
            return self.resources_uncached(pack, name);
        }
        if let Some(cached) = self.resources_cache.borrow().get(&key) {
            return cached
                .clone()
                .ok_or_else(|| "Scenery resource is unresolved".into());
        }
        let result = self.resources_uncached(pack, name);
        self.resources_cache
            .borrow_mut()
            .insert(key, result.as_ref().ok().cloned());
        result
    }

    fn resources_uncached(&self, pack: &Path, name: &str) -> Result<Vec<PathBuf>, String> {
        let name = name.trim_start_matches('/');
        if self.incomplete.contains(name) {
            return Err(format!(
                "Scenery resource exceeds its variant budget: {name}"
            ));
        }
        let local = pack.join(relative(name)?);
        let mut paths = self.exports.get(name).cloned().unwrap_or_default();
        if paths.is_empty() || local.is_file() {
            paths.insert(local);
        }
        Ok(paths.into_iter().map(case_insensitive).collect())
    }

    pub fn paved_surface(&self, pack: &Path, name: &str) -> Result<bool, String> {
        for path in self.resources(pack, name)? {
            let mut text = String::new();
            super::cache::open_file(&path)
                .map_err(|e| e.to_string())?
                .take(64 * 1024 + 1)
                .read_to_string(&mut text)
                .map_err(|e| e.to_string())?;
            if text.len() > 64 * 1024 {
                return Err("Draped surface definition exceeds its budget".into());
            }
            if !paved_definition(&text) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Tallest tree in a forest definition, in metres.
    ///
    /// The DSF polygon parameter is a density for forests, so the obstruction height has to
    /// come from the definition itself. `TREE` publishes a minimum and maximum height, `TREE2`
    /// publishes a maximum and a nominal height, and a mesh-based forest has neither, so its
    /// height is the tallest vertex of its own geometry. Whichever form the file uses, the
    /// largest published value bounds the obstacle without inventing one.
    pub fn forest_height(&self, pack: &Path, name: &str) -> Result<f64, String> {
        let key = (pack.to_path_buf(), name.to_owned());
        if !Self::caching(pack) {
            return self.forest_height_uncached(pack, name);
        }
        if let Some(cached) = self.forest_cache.borrow().get(&key) {
            return (*cached).ok_or_else(|| "Forest definition is unresolved".into());
        }
        let result = self.forest_height_uncached(pack, name);
        self.forest_cache
            .borrow_mut()
            .insert(key, result.as_ref().ok().copied());
        result
    }

    fn forest_height_uncached(&self, pack: &Path, name: &str) -> Result<f64, String> {
        let mut tallest: f64 = 0.0;
        let mut found = false;
        let mut mesh_low: f64 = f64::INFINITY;
        let mut mesh_high: f64 = f64::NEG_INFINITY;
        for path in self.resources(pack, name)? {
            let mut text = String::new();
            // A mesh-built forest lists every vertex of every tree it can draw, so a shipped
            // definition is far larger than the simple quad format. The cap still bounds it.
            super::cache::open_file(&path)
                .map_err(|e| e.to_string())?
                .take(64 * 1024 * 1024 + 1)
                .read_to_string(&mut text)
                .map_err(|e| e.to_string())?;
            if text.len() > 64 * 1024 * 1024 {
                return Err("Forest definition exceeds its budget".into());
            }
            for line in text.lines() {
                let line = line.split('#').next().unwrap_or("").trim();
                // A forest definition may separate fields with tabs, so whitespace splitting
                // is required rather than a single space.
                let mut fields = line.split_whitespace();
                let command = fields.next().unwrap_or("");
                let heights: Vec<f64> = match command {
                    // s t w h offset frequency min-h max-h quads layer notes
                    "TREE" => fields
                        .skip(6)
                        .take(2)
                        .filter_map(|value| value.parse::<f64>().ok())
                        .collect(),
                    // s t w h sw percent min-h max-h nominal-h lod quads type notes
                    "TREE2" => fields
                        .skip(6)
                        .take(3)
                        .filter_map(|value| value.parse::<f64>().ok())
                        .collect(),
                    // An X-Plane 12 forest can be built from meshes alone, in which case no
                    // TREE line publishes a height and the tree's size is its own geometry.
                    // The second component of a vertex is its height in metres.
                    "VERTEX" => fields
                        .nth(1)
                        .and_then(|value| value.parse::<f64>().ok())
                        .into_iter()
                        .collect(),
                    // A mesh tree is positioned by its own geometry, so a vertex height alone
                    // understates it; the nominal tree height is the published size.
                    "MESH_3D" | "MESH" => Vec::new(),
                    _ => continue,
                };
                for height in heights {
                    if height.is_finite() && height > 0.0 {
                        tallest = tallest.max(height);
                        found = true;
                    }
                }
                if command == "VERTEX"
                    && let Some(value) = line
                        .split_whitespace()
                        .nth(2)
                        .and_then(|value| value.parse::<f64>().ok())
                        .filter(|value| value.is_finite())
                {
                    mesh_low = mesh_low.min(value);
                    mesh_high = mesh_high.max(value);
                }
            }
        }
        // A mesh-built tree publishes no height command at all; its size is the vertical span
        // of its own geometry.
        if mesh_high.is_finite() && mesh_low.is_finite() {
            let span = mesh_high - mesh_low;
            if span.is_finite() && span > 0.0 {
                tallest = tallest.max(span);
                found = true;
            }
        }
        if !found {
            return Err("Forest definition publishes no tree height".into());
        }
        Ok(tallest)
    }

    /// Tallest obstacle placed by an object string (`.str`) or autogen string (`.ags`).
    ///
    /// Both formats name a catalogue of objects, and autogen strings also name vegetation. The
    /// string itself only describes where the catalogue is repeated, so the height of the
    /// resulting obstacle is the tallest thing it can place. A reference that cannot be resolved
    /// makes the whole string unresolved rather than being treated as flat ground.
    /// Bound the distance between an annotation and its tile's ground point.
    /// `SHARE_Y` can refer to another tile, so those definitions retain the area-wide bound.
    pub fn autogen_ground_reach(&self, pack: &Path, name: &str) -> Option<f64> {
        let key = (pack.to_path_buf(), name.to_owned());
        if let Some(value) = self.ground_reach_cache.borrow().get(&key) {
            return *value;
        }
        let result = (|| {
            let mut maximum = 0.0_f64;
            for path in self.resources(pack, name).ok()? {
                let mut text = String::new();
                super::cache::open_file(&path)
                    .ok()?
                    .take(16 * 1024 * 1024 + 1)
                    .read_to_string(&mut text)
                    .ok()?;
                if text.len() > 16 * 1024 * 1024 {
                    return None;
                }
                let mut scale = None;
                let mut width = None;
                let mut height = None;
                let mut tiles = 0;
                let mut tile = None;
                for line in text.lines() {
                    let fields: Vec<_> = line.split('#').next()?.split_whitespace().collect();
                    let number = |index: usize| {
                        fields
                            .get(index)?
                            .parse::<f64>()
                            .ok()
                            .filter(|n| n.is_finite())
                    };
                    match fields.first().copied() {
                        Some("SHARE_Y") => return None,
                        Some("GROUND_PT") => {
                            let [left, bottom, right, top]: [f64; 4] = tile?;
                            if !(left..=right).contains(&number(1)?)
                                || !(bottom..=top).contains(&number(2)?)
                            {
                                return None;
                            }
                        }
                        Some("TEXTURE_SCALE") => scale = Some([number(1)?, number(2)?]),
                        Some("TEXTURE_WIDTH") => width = Some(number(1)?),
                        Some("TEXTURE_HEIGHT") => height = Some(number(1)?),
                        Some("TILE") => {
                            let scale: [f64; 2] = scale?;
                            let width: f64 = width?;
                            if scale.iter().any(|n| *n <= 0.0) || width <= 0.0 {
                                return None;
                            }
                            let sx = width / scale[0];
                            let sy = height.map_or(sx, |height| height / scale[1]);
                            if sy <= 0.0 {
                                return None;
                            }
                            let bounds = [number(1)?, number(2)?, number(3)?, number(4)?];
                            if bounds[0] > bounds[2] || bounds[1] > bounds[3] {
                                return None;
                            }
                            tile = Some(bounds);
                            let diagonal = ((number(3)? - number(1)?) * sx)
                                .hypot((number(4)? - number(2)?) * sy);
                            if !(0.0..=5000.0).contains(&diagonal) {
                                return None;
                            }
                            maximum = maximum.max(diagonal);
                            tiles += 1;
                        }
                        _ => {}
                    }
                }
                if tiles == 0 {
                    return None;
                }
            }
            (maximum > 0.0).then_some(maximum)
        })();
        self.ground_reach_cache.borrow_mut().insert(key, result);
        result
    }

    pub fn string_height(
        &self,
        pack: &Path,
        name: &str,
        autogen: bool,
        known: &mut BTreeMap<String, Option<(f64, f64)>>,
    ) -> Result<(f64, f64), String> {
        let key = (pack.to_path_buf(), name.to_owned());
        if !Self::caching(pack) {
            return self.string_height_uncached(pack, name, autogen, known);
        }
        if let Some(cached) = self.string_cache.borrow().get(&key) {
            return (*cached).ok_or_else(|| "Object string definition is unresolved".into());
        }
        let result = self.string_height_uncached(pack, name, autogen, known);
        self.string_cache
            .borrow_mut()
            .insert(key, result.as_ref().ok().copied());
        result
    }

    fn string_height_uncached(
        &self,
        pack: &Path,
        name: &str,
        autogen: bool,
        known: &mut BTreeMap<String, Option<(f64, f64)>>,
    ) -> Result<(f64, f64), String> {
        let mut tallest = 0.0_f64;
        let mut reach = 0.0_f64;
        let mut found = false;
        for path in self.resources(pack, name)? {
            let mut text = String::new();
            super::cache::open_file(&path)
                .map_err(|e| e.to_string())?
                .take(16 * 1024 * 1024 + 1)
                .read_to_string(&mut text)
                .map_err(|e| e.to_string())?;
            if text.len() > 16 * 1024 * 1024 {
                return Err("Object string definition exceeds its budget".into());
            }
            let parent = path.parent().ok_or("Object string has no directory")?;
            let mut references = 0usize;
            for line in text.lines() {
                let line = line.split('#').next().unwrap_or("").trim();
                let mut fields = line.split_whitespace();
                let command = fields.next();
                if !matches!(command, Some("OBJECT" | "VEGETATION")) {
                    continue;
                }
                if command == Some("VEGETATION") && !autogen {
                    continue;
                }
                let reference = fields
                    .last()
                    .ok_or("Object string reference is missing")?
                    .to_owned();
                references += 1;
                if references > 4096 {
                    return Err("Object string catalogue exceeds its budget".into());
                }
                let height = if let Some(height) = known.get(&reference) {
                    *height
                } else {
                    let height = if command == Some("VEGETATION") {
                        // A tree is a point instance; its crown and spacing are bounded well
                        // inside thirty metres.
                        self.forest_height(pack, &reference)
                            .ok()
                            .map(|height| (height, 30.0))
                    } else {
                        let resources = if reference.to_ascii_lowercase().starts_with("lib/") {
                            self.resources(pack, &reference).ok()
                        } else {
                            nested(parent, &reference).ok().map(|path| vec![path])
                        };
                        resources.and_then(|resources| {
                            let mut height: Option<f64> = None;
                            let mut reach = 0.0_f64;
                            for resource in resources {
                                let geometry = object::load(&resource).ok()?;
                                for bound in geometry {
                                    height = Some(
                                        height.unwrap_or(0.0_f64).max(f64::from(bound.high[1])),
                                    );
                                    reach = reach.max(super::horizontal_reach(&bound));
                                }
                            }
                            height.map(|height| (height, reach))
                        })
                    };
                    known.insert(reference.clone(), height);
                    height
                };
                let (height, object_reach) = height.ok_or_else(|| {
                    format!("Object string reference {reference} is unresolvable")
                })?;
                if !height.is_finite() || !(0.0..=1000.0).contains(&height) {
                    return Err("Object string reference height is outside its range".into());
                }
                tallest = tallest.max(height);
                reach = reach.max(object_reach);
                found = true;
            }
        }
        if !found {
            return Err("Object string publishes no resolvable obstacle".into());
        }
        Ok((tallest, reach))
    }

    /// Whether a facade definition encloses space. A definition with a roof is a building; one
    /// with only wall profiles is a fence or an open structure whose drawn geometry is its
    /// perimeter, not the area inside it.
    pub fn facade_solid(&self, pack: &Path, name: &str) -> Result<bool, String> {
        if !Self::caching(pack) {
            return self.facade_solid_uncached(pack, name);
        }
        if let Some(cached) = self.solid_cache.borrow().get(name) {
            return (*cached).ok_or_else(|| "Facade definition is unresolved".into());
        }
        let result = self.facade_solid_uncached(pack, name);
        self.solid_cache
            .borrow_mut()
            .insert(name.to_owned(), result.as_ref().ok().copied());
        result
    }

    fn facade_solid_uncached(&self, pack: &Path, name: &str) -> Result<bool, String> {
        let mut solid = false;
        for path in self.resources(pack, name)? {
            let mut bytes = Vec::new();
            super::cache::open_file(&path)
                .map_err(|e| e.to_string())?
                .take(4 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > 4 * 1024 * 1024 {
                // The header is at the start of the file; a definition too large to inspect
                // fully is treated as solid rather than as an open wall.
                return Ok(true);
            }
            for line in String::from_utf8_lossy(&bytes).lines() {
                let command = line.split_whitespace().next().unwrap_or("");
                if matches!(command, "HARD_ROOF" | "ROOF" | "ROOF_MESH" | "SHADER_ROOF") {
                    solid = true;
                }
            }
        }
        Ok(solid)
    }

    pub fn ground_only(&self, pack: &Path, name: &str) -> Result<bool, String> {
        for path in self.resources(pack, name)? {
            let mut text = String::new();
            super::cache::open_file(path)
                .map_err(|e| e.to_string())?
                .take(16384)
                .read_to_string(&mut text)
                .map_err(|e| e.to_string())?;
            let mut lines = text
                .lines()
                .map(|line| line.split('#').next().unwrap_or("").trim())
                .filter(|line| !line.is_empty());
            if !matches!(lines.next(), Some("A" | "I"))
                || !matches!(lines.next(), Some("800" | "850"))
                || !matches!(
                    lines.next(),
                    Some("DRAPED_POLYGON" | "LINE_PAINT" | "BEACH")
                )
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

fn paved_definition(text: &str) -> bool {
    let mut lines = text
        .lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter(|line| !line.is_empty());
    if !matches!(lines.next(), Some("A" | "I"))
        || lines.next() != Some("850")
        || lines.next() != Some("DRAPED_POLYGON")
    {
        return false;
    }
    let mut surface = false;
    for line in lines {
        let mut words = line.split_whitespace();
        if words.next() == Some("SURFACE") {
            if surface
                || !matches!(words.next(), Some("asphalt" | "concrete"))
                || words.next().is_some()
            {
                return false;
            }
            surface = true;
        }
    }
    surface
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    #[test]
    fn every_library_variant_must_declare_physical_pavement() {
        let root = std::env::temp_dir().join(format!(
            "openatc-surface-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut library = Library::default();
        library.add(&root, "A\n1200\nLIBRARY\nEXPORT surface.pol paved.pol\nEXPORT_SEASON win surface.pol winter.pol\n").unwrap();
        std::fs::write(
            root.join("paved.pol"),
            "A\n850\nDRAPED_POLYGON\nSURFACE concrete\n",
        )
        .unwrap();
        std::fs::write(
            root.join("winter.pol"),
            "A\n850\nDRAPED_POLYGON\nTEXTURE snow.png\n",
        )
        .unwrap();
        assert!(!library.paved_surface(&root, "surface.pol").unwrap());
        std::fs::write(
            root.join("winter.pol"),
            "A\n850\nDRAPED_POLYGON\nSURFACE asphalt\n",
        )
        .unwrap();
        assert!(library.paved_surface(&root, "surface.pol").unwrap());
        std::fs::write(root.join("winter.pol"), "x".repeat(64 * 1024 + 1)).unwrap();
        assert!(library.paved_surface(&root, "surface.pol").is_err());
        std::fs::remove_file(root.join("winter.pol")).unwrap();
        assert!(library.paved_surface(&root, "surface.pol").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_mesh_built_forest_derives_its_height_from_its_own_geometry() {
        // X-Plane 12 forests commonly publish no TREE height at all; the tree is 3-D geometry
        // described by MESH and tab-separated VERTEX lines, so the size must come from there.
        let root = std::env::temp_dir().join(format!(
            "openatc-mesh-forest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut library = Library::default();
        library
            .add(&root, "A\n1200\nLIBRARY\nEXPORT trees.for trees.for\n")
            .unwrap();
        // Tab separated, as the shipped definitions are, spanning 0 to 12 m vertically.
        std::fs::write(
            root.join("trees.for"),
            "A\n1200\nFOREST\n\nMESH\t3D_ash_01\t0\t500\t3\t3\t2.0\t0.4\t10.0\t\nVERTEX\t0.03\t0.00\t0.01\t0\t0\t1\t0.5\t0.8\t0\t0\t0.1\nVERTEX\t0.05\t12.00\t-0.01\t1\t0\t0\t0.5\t0.8\t0\t0\t0.1\nVERTEX\t0.01\t-0.50\t0.00\t0\t0\t1\t0.5\t0.8\t0\t0\t0.1\n",
        )
        .unwrap();
        let height = library.forest_height(&root, "trees.for").unwrap();
        assert!(
            (height - 12.5).abs() < 1e-6,
            "expected the vertical span of the mesh, got {height}"
        );
        // A definition with neither a tree command nor usable geometry still refuses.
        std::fs::write(
            root.join("trees.for"),
            "A\n1200\nFOREST\nTEXTURE pines.png\n",
        )
        .unwrap();
        assert!(library.forest_height(&root, "trees.for").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_forest_reports_the_tallest_published_tree() {
        let root = std::env::temp_dir().join(format!(
            "openatc-forest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut library = Library::default();
        library
            .add(&root, "A\n1200\nLIBRARY\nEXPORT trees.for trees.for\n")
            .unwrap();
        // s t w h offset frequency min-h max-h quads layer notes
        std::fs::write(
            root.join("trees.for"),
            "A\n800\nFOREST\nTEXTURE pines.png\nTREE 44 896 52 127 25 50 10 28 1 1 pine\nTREE 194 896 44 127 22 50 8 31 2 1 spruce\nSKIP_SURFACE water\n",
        )
        .unwrap();
        assert_eq!(library.forest_height(&root, "trees.for").unwrap(), 31.0);
        // A TREE2 publishes its maximum height directly and can exceed a legacy tree.
        std::fs::write(
            root.join("trees.for"),
            "A\n800\nFOREST\nTREE2 44 896 52 127 25 50 10 28 24 2000 1 oak\n",
        )
        .unwrap();
        assert_eq!(library.forest_height(&root, "trees.for").unwrap(), 28.0);
        // A forest with no usable height must refuse rather than be treated as flat ground.
        std::fs::write(
            root.join("trees.for"),
            "A\n800\nFOREST\nTEXTURE pines.png\n",
        )
        .unwrap();
        assert!(library.forest_height(&root, "trees.for").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pavement_requires_an_explicit_unambiguous_hard_surface() {
        for surface in ["asphalt", "concrete"] {
            assert!(paved_definition(&format!(
                "A\n850\nDRAPED_POLYGON\nSURFACE {surface} # apron\n"
            )));
        }
        for tail in [
            "TEXTURE concrete.png",
            "SURFACE grass",
            "SURFACE concrete\nSURFACE asphalt",
            "SURFACE concrete extra",
        ] {
            assert!(!paved_definition(&format!(
                "I\n850\nDRAPED_POLYGON\n{tail}\n"
            )));
        }
        assert!(!paved_definition("A\n850\nLINE_PAINT\nSURFACE concrete\n"));
    }

    #[test]
    fn autogen_ground_references_are_bounded_by_the_tile_not_the_whole_area() {
        let root =
            std::env::temp_dir().join(format!("openatc-ground-reach-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let text = "A\n1000\nAG_STRING\nTEXTURE_SCALE 2048 1024\nTEXTURE_WIDTH 256\nTEXTURE_HEIGHT 128\nTILE 0 0 240 320\nGROUND_PT 30 40\n";
        std::fs::write(root.join("local.ags"), text).unwrap();
        std::fs::write(root.join("shared.ags"), format!("{text}SHARE_Y\n")).unwrap();
        let library = Library::default();
        assert_eq!(library.autogen_ground_reach(&root, "local.ags"), Some(50.0));
        assert_eq!(library.autogen_ground_reach(&root, "shared.ags"), None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_object_string_height_comes_from_its_catalogue() {
        let root = std::env::temp_dir().join(format!(
            "openatc-string-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("tower.obj"),
            "I\n800\nOBJ\nPOINT_COUNTS 3 0 0 3\nVT -2 0 -20 0 1 0 0 0\nVT 2 12 -20 0 1 0 0 0\nVT 0 0 20 0 1 0 0 0\nIDX 0\nIDX 1\nIDX 2\nTRIS 0 3\n",
        )
        .unwrap();
        std::fs::write(
            root.join("lights.str"),
            "A\n850\nOBJECT_STRING\nOBJECT 0 0 tower.obj\nOFFSET 2\n",
        )
        .unwrap();
        let mut library = Library::default();
        library
            .add(&root, "A\n1200\nLIBRARY\nEXPORT lights.str lights.str\n")
            .unwrap();
        let mut known = BTreeMap::new();
        assert_eq!(
            library
                .string_height(&root, "lights.str", false, &mut known)
                .unwrap(),
            (12.0, 20.0)
        );
        // A catalogue entry that cannot be loaded keeps the string unresolved.
        std::fs::write(
            root.join("lights.str"),
            "A\n850\nOBJECT_STRING\nOBJECT 0 0 missing.obj\nOFFSET 2\n",
        )
        .unwrap();
        let mut known = BTreeMap::new();
        assert!(
            library
                .string_height(&root, "lights.str", false, &mut known)
                .is_err()
        );
        // A definition with no catalogue publishes no height.
        std::fs::write(root.join("lights.str"), "A\n850\nOBJECT_STRING\nOFFSET 2\n").unwrap();
        let mut known = BTreeMap::new();
        assert!(
            library
                .string_height(&root, "lights.str", false, &mut known)
                .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn all_library_variants_are_retained_without_following_paths_outside_the_package() {
        let mut library = Library::default();
        library.add(Path::new("scenery/A"), "I\n1200\nLIBRARY\nEXPORT lib/tower.obj small.obj\nREGION elsewhere\nEXPORT_RATIO 0.2 lib/tower.obj large tower.obj\nEXPORT_SEASON win lib/tower.obj snow.obj\nEXPORT_EXTEND_SEASON spr,sum,fal lib/tower.obj spring.obj\n").unwrap();
        library
            .add(
                Path::new("scenery/B"),
                "A\n800\nLIBRARY\nEXPORT_BACKUP lib/tower.obj backup.obj\n",
            )
            .unwrap();
        let variants = library
            .resources(Path::new("scenery/C"), "lib/tower.obj")
            .unwrap();
        assert_eq!(variants.len(), 5);
        assert!(variants.contains(&PathBuf::from("scenery/A/large tower.obj")));
        for mapping in [
            "EXPORT_RATIO NaN lib/a.obj a.obj",
            "EXPORT_SEASON invalid lib/a.obj a.obj",
            "EXPORT_NEW lib/a.obj a.obj",
        ] {
            assert!(
                library
                    .add(
                        Path::new("scenery/A"),
                        &format!("I\n1200\nLIBRARY\n{mapping}\n")
                    )
                    .is_err()
            );
        }
        assert!(
            library
                .resources(Path::new("scenery/C"), "../escape.obj")
                .is_err()
        );
        for path in ["../a.obj", "/absolute.obj", ""] {
            library
                .add(
                    Path::new("scenery/A"),
                    &format!("I\n800\nLIBRARY\nEXPORT lib/missing.obj {path}\n"),
                )
                .unwrap();
            assert!(
                library
                    .resources(Path::new("scenery/A"), "lib/missing.obj")
                    .is_err()
            );
        }
        let mut crowded = "I\n800\nLIBRARY\n".to_owned();
        for index in 0..257 {
            writeln!(crowded, "EXPORT /lib/crowded.obj model{index}.obj").unwrap();
        }
        library.add(Path::new("scenery/A"), &crowded).unwrap();
        assert!(
            library
                .resources(Path::new("scenery/A"), "lib/crowded.obj")
                .is_err()
        );
        assert_eq!(
            library
                .resources(Path::new("scenery/C"), "/lib/tower.obj")
                .unwrap()
                .len(),
            5
        );
    }

    #[test]
    fn a_resource_name_matching_only_by_case_resolves_to_the_shipped_file() {
        let root = std::env::temp_dir().join(format!(
            "openatc-case-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Dir_Ramp_Lit.obj"), "A\n800\nOBJ\n").unwrap();
        let mut library = Library::default();
        library
            .add(
                &root,
                "A\n1200\nLIBRARY\nEXPORT ramp.obj dir_ramp_lit.obj\n",
            )
            .unwrap();
        assert_eq!(
            library.resources(&root, "ramp.obj").unwrap(),
            vec![root.join("Dir_Ramp_Lit.obj")]
        );
        // Two files that differ only by case are ambiguous, so neither is chosen.
        std::fs::write(root.join("DIR_RAMP_LIT.obj"), "A\n800\nOBJ\n").unwrap();
        assert!(
            library
                .resources(&root, "ramp.obj")
                .unwrap()
                .iter()
                .all(|path| !path.is_file())
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
