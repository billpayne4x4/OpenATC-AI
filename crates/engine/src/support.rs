//! Engine support: config paths, atomic saves, prompt loading, upstream HTTP.

#![allow(clippy::default_trait_access)]

use std::path::{Path, PathBuf};

/// Role prompts with file-loaded overrides and shipped fallbacks.
pub struct Prompts {
    /// Shared framing and context instructions loaded from TOML.
    pub guidance: Guidance,
    /// Wording rules for an already validated ATC decision.
    pub atc: String,
    /// Structured interpretation of multi-part crew requests.
    pub crew_actions: String,
    /// Conversation grounded in an already verified crew result.
    pub crew_response: String,
    /// Cabin crew system prompt.
    pub cabin: String,
    /// Ground crew system prompt.
    pub ground: String,
    /// Copilot readback prompt.
    pub copilot: String,
    /// Intent classifier prompt.
    pub classify: String,
    /// Copilot chat prompt.
    pub copilot_chat: String,
    /// Directory the files came from, or built-in.
    pub source: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptFile {
    atc_reply: String,
    crew_actions: String,
    #[serde(default = "default_crew_response")]
    crew_response: String,
    cabin: String,
    ground: String,
    copilot_readback: String,
    intent_classify: String,
    copilot_chat: String,
}

/// Role prompts a catalogue must define. `crew_response` is not
/// listed: older catalogues predate it and a bundled default covers its absence.
const REQUIRED_ROLE_PROMPTS: [&str; 7] = [
    "atc_reply",
    "crew_actions",
    "cabin",
    "ground",
    "copilot_readback",
    "intent_classify",
    "copilot_chat",
];

fn default_crew_response() -> String {
    include_str!("../../../prompts/crew_response.txt").to_owned()
}

/// Name every role a supplied catalogue leaves undefined. A present but partial
/// file is an operator error, not a reason to silently drop its roles.
fn missing_role_prompts(text: &str) -> Result<(), String> {
    let table: toml::Table =
        openatc_core::configuration::from_str(text).map_err(|error| error.to_string())?;
    let missing: Vec<&str> = REQUIRED_ROLE_PROMPTS
        .iter()
        .copied()
        .filter(|key| !table.contains_key(*key))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("missing role prompts: {}", missing.join(", ")))
    }
}

impl PromptFile {
    fn into_prompts(self, source: String) -> Result<Prompts, String> {
        if [
            &self.atc_reply,
            &self.crew_actions,
            &self.crew_response,
            &self.cabin,
            &self.ground,
            &self.copilot_readback,
            &self.intent_classify,
            &self.copilot_chat,
        ]
        .iter()
        .any(|text| text.trim().is_empty() || text.len() > 65536)
        {
            return Err("Role prompts must be nonempty and at most 64 KiB each".into());
        }
        Ok(Prompts {
            guidance: Guidance::default(),
            atc: self.atc_reply,
            crew_actions: self.crew_actions,
            crew_response: self.crew_response,
            cabin: self.cabin,
            ground: self.ground,
            copilot: self.copilot_readback,
            classify: self.intent_classify,
            copilot_chat: self.copilot_chat,
            source,
        })
    }
}

/// Read a prompt file and trim trailing newlines.
fn read_prompt_file(dir: &Path, name: &str) -> String {
    let mut text = std::fs::read_to_string(dir.join(name)).unwrap_or_default();
    while text.ends_with('\n') || text.ends_with('\r') {
        text.pop();
    }
    text
}

/// Load speech-only identifier pronunciation once, retaining bundled data on invalid overrides.
pub fn load_pronunciation(
    config_dir: &Path,
    speech_source: &str,
) -> openatc_core::radiotelephony::Pronunciation {
    use std::io::Read;
    let mut paths = vec![config_dir.join("speech/runtime/radiotelephony.toml")];
    if !speech_source.is_empty() {
        paths.push(Path::new(speech_source).join("runtime/radiotelephony.toml"));
    }
    for path in paths {
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                eprintln!("Radio pronunciation {}: {error}", path.display());
                continue;
            }
        };
        let mut text = String::new();
        match file
            .take(16385)
            .read_to_string(&mut text)
            .map_err(|error| error.to_string())
            .and_then(|_| openatc_core::radiotelephony::Pronunciation::parse(&text))
        {
            Ok(policy) => return policy,
            Err(error) => eprintln!("Invalid radio pronunciation {}: {error}", path.display()),
        }
    }
    Default::default()
}

/// Replace every `{{callsign}}` placeholder.
#[must_use]
pub fn fill_prompt(text: &str, callsign: &str) -> String {
    text.replace("{{callsign}}", callsign)
}

/// Load prompts: env override, then config dir, then exe-relative, else the
/// shipped texts compiled in. A directory only wins when it holds cabin.txt.
/// A present but incomplete `prompts.toml` is refused with the missing roles
/// rather than quietly discarding the operator's catalogue.
pub fn load_prompts(config_dir: &Path, exe_dir: &Path) -> Result<Prompts, String> {
    let mut prompts = openatc_core::configuration::from_str::<PromptFile>(include_str!(
        "../../../prompts/prompts.toml"
    ))
    .expect("bundled role prompts")
    .into_prompts("built-in".into())
    .expect("valid bundled prompts");
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::var_os("OPENATC_PROMPTS_DIR") {
        candidates.push(PathBuf::from(dir));
    }
    candidates.push(config_dir.join("prompts"));
    candidates.push(exe_dir.join("../prompts"));
    for candidate in candidates {
        let path = candidate.join("prompts.toml");
        if let Ok(text) = std::fs::read_to_string(&path) {
            let mut loaded = missing_role_prompts(&text)
                .and_then(|()| {
                    openatc_core::configuration::from_str::<PromptFile>(&text)
                        .map_err(|error| error.to_string())
                })
                .and_then(|file| file.into_prompts(candidate.to_string_lossy().into_owned()))
                .map_err(|error| format!("{}: {error}", path.display()))?;
            loaded.guidance = Guidance::load(&candidate.join("interaction.toml"));
            return Ok(loaded);
        }
        if read_prompt_file(&candidate, "cabin.txt").is_empty() {
            continue;
        }
        prompts.source = candidate.to_string_lossy().into_owned();
        prompts.guidance = Guidance::load(&candidate.join("interaction.toml"));
        for (name, target) in [
            ("atc_reply.txt", &mut prompts.atc),
            ("crew_actions.txt", &mut prompts.crew_actions),
            ("crew_response.txt", &mut prompts.crew_response),
            ("cabin.txt", &mut prompts.cabin),
            ("ground.txt", &mut prompts.ground),
            ("copilot_readback.txt", &mut prompts.copilot),
            ("copilot_chat.txt", &mut prompts.copilot_chat),
            ("intent_classify.txt", &mut prompts.classify),
        ] {
            let text = read_prompt_file(&candidate, name);
            if !text.is_empty() {
                *target = text;
            }
        }
        break;
    }
    Ok(prompts)
}

/// Latest modification time across every file that can change the active prompt catalogue.
/// Missing files contribute the epoch, so creating or deleting an override is a change.
#[must_use]
pub fn prompt_stamp(config_dir: &Path, exe_dir: &Path) -> std::time::SystemTime {
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::var_os("OPENATC_PROMPTS_DIR") {
        candidates.push(PathBuf::from(dir));
    }
    candidates.push(config_dir.join("prompts"));
    candidates.push(exe_dir.join("../prompts"));
    let mut stamp = std::time::SystemTime::UNIX_EPOCH;
    for candidate in candidates {
        for name in [
            "prompts.toml",
            "interaction.toml",
            "atc_reply.txt",
            "crew_actions.txt",
            "crew_response.txt",
            "cabin.txt",
            "ground.txt",
            "copilot_readback.txt",
            "copilot_chat.txt",
            "intent_classify.txt",
        ] {
            let time = std::fs::metadata(candidate.join(name))
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            stamp = stamp.max(time);
        }
    }
    stamp
}

/// Reload the catalogue when one of its files changed. A rejected catalogue leaves the
/// running prompts untouched; the caller reports the error and keeps operating.
pub fn watch_prompts(
    config_dir: &Path,
    exe_dir: &Path,
    stamp: &mut std::time::SystemTime,
) -> Result<Option<Prompts>, String> {
    let latest = prompt_stamp(config_dir, exe_dir);
    if latest == *stamp {
        return Ok(None);
    }
    *stamp = latest;
    load_prompts(config_dir, exe_dir).map(Some)
}

/// Resolve the speech library dir: env override, then exe-relative.
#[must_use]
pub fn resolve_speech_dir(config_dir: &Path, exe_dir: &Path) -> String {
    if let Ok(override_path) = std::env::var("OPENATC_SPEECH_DIR") {
        return override_path;
    }
    for candidate in [config_dir.join("speech"), exe_dir.join("../speech")] {
        if candidate.is_dir() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    String::new()
}

/// Resolve the regions file: env override, then config dir, then exe-relative.
#[must_use]
pub fn resolve_regions_file(config_dir: &Path, exe_dir: &Path) -> String {
    if let Ok(override_path) = std::env::var("OPENATC_REGIONS_FILE") {
        return override_path;
    }
    for candidate in [
        config_dir.join("regions.toml"),
        exe_dir.join("../regions.toml"),
    ] {
        if candidate.is_file() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    String::new()
}

/// Write JSON atomically through a temp file plus rename.
pub fn save_json(path: &Path, data: &serde_json::Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| "Cannot save configuration".to_owned())?;
    }
    let temporary = path.with_extension("json.tmp");
    std::fs::write(
        &temporary,
        serde_json::to_string_pretty(data).unwrap_or_default(),
    )
    .map_err(|_| "Cannot save configuration".to_owned())?;
    std::fs::rename(&temporary, path).map_err(|_| "Cannot save configuration".to_owned())
}

/// Bearer header from an env key, absent when unset.
/// GET a upstream /health with short timeouts.
pub async fn service_ok(client: &reqwest::Client, url: &str) -> bool {
    if url.is_empty() {
        return false;
    }
    client
        .get(format!("{url}/health"))
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
        .is_ok_and(|reply| reply.status().is_success())
}

#[cfg(test)]
mod prompt_tests {
    use super::*;

    #[test]
    fn toml_prompts_preserve_role_instructions_and_reject_empty_overrides() {
        let text = include_str!("../../../prompts/prompts.toml");
        let file: PromptFile = toml::from_str(text).unwrap();
        let prompts = file.into_prompts("test".into()).unwrap();
        assert_eq!(prompts.atc, include_str!("../../../prompts/atc_reply.txt"));
        assert_eq!(
            prompts.crew_actions,
            include_str!("../../../prompts/crew_actions.txt")
        );
        assert_eq!(
            prompts.crew_response,
            include_str!("../../../prompts/crew_response.txt")
        );
        assert_eq!(prompts.cabin, include_str!("../../../prompts/cabin.txt"));
        assert_eq!(prompts.ground, include_str!("../../../prompts/ground.txt"));
        assert_eq!(
            prompts.copilot,
            include_str!("../../../prompts/copilot_readback.txt")
        );
        assert_eq!(
            prompts.classify,
            include_str!("../../../prompts/intent_classify.txt")
        );
        assert_eq!(
            prompts.copilot_chat,
            include_str!("../../../prompts/copilot_chat.txt")
        );
        let mut file: PromptFile = toml::from_str(text).unwrap();
        file.cabin.clear();
        assert!(file.into_prompts("bad".into()).is_err());
        assert!(toml::from_str::<PromptFile>(&format!("{text}\nunknown = true")).is_err());
    }

    #[test]
    fn edited_prompt_catalogue_reloads_and_a_partial_edit_keeps_the_running_copy() {
        let directory =
            std::env::temp_dir().join(format!("openatc-reload-{}", rand::random::<u64>()));
        let catalogue = directory.join("prompts");
        std::fs::create_dir_all(&catalogue).unwrap();
        let shipped = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../prompts/prompts.toml"),
        )
        .unwrap();
        let path = catalogue.join("prompts.toml");
        std::fs::write(&path, &shipped).unwrap();
        let mut stamp = std::time::SystemTime::UNIX_EPOCH;
        let first = watch_prompts(&directory, &directory, &mut stamp)
            .unwrap()
            .unwrap();
        assert!(first.atc.contains("controller working flight"));
        assert!(
            watch_prompts(&directory, &directory, &mut stamp)
                .unwrap()
                .is_none(),
            "an unchanged catalogue must not be rebuilt"
        );
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(
            &path,
            shipped.replace(
                "You are the controller working flight",
                "You are the controller working flight (edited)",
            ),
        )
        .unwrap();
        let edited = watch_prompts(&directory, &directory, &mut stamp)
            .unwrap()
            .unwrap();
        assert!(edited.atc.contains("(edited)"));
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&path, "atc_reply = 'incomplete'\n").unwrap();
        let error = watch_prompts(&directory, &directory, &mut stamp)
            .err()
            .unwrap();
        assert!(error.contains("missing role prompts"), "{error}");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn incomplete_catalogue_names_every_missing_role_instead_of_falling_back() {
        let directory =
            std::env::temp_dir().join(format!("openatc-prompts-{}", rand::random::<u64>()));
        let catalogue = directory.join("prompts");
        std::fs::create_dir_all(&catalogue).unwrap();
        let path = catalogue.join("prompts.toml");
        std::fs::write(&path, "atc_reply = 'incomplete'\n").unwrap();
        let error = load_prompts(&directory, &directory).err().unwrap();
        assert!(error.contains("prompts.toml"), "{error}");
        for role in [
            "cabin",
            "ground",
            "copilot_readback",
            "intent_classify",
            "copilot_chat",
        ] {
            assert!(error.contains(role), "missing {role} in: {error}");
        }
        let complete = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../prompts/prompts.toml"),
        )
        .unwrap();
        std::fs::write(&path, format!("{complete}\nunknown = 'x'\n")).unwrap();
        assert!(
            load_prompts(&directory, &directory)
                .err()
                .unwrap()
                .contains("unknown")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[cfg(test)]
mod prompt_coverage_tests {
    use super::*;

    /// Guidance keys the engine composes at run time rather than naming literally.
    const COMPOSED_GUIDANCE_KEYS: [&str; 12] = [
        "atc_clearance",
        "atc_ground",
        "atc_tower",
        "atc_approach",
        "atc_departure",
        "atc_center",
        "crew_actions_ground",
        "crew_actions_cabin",
        "crew_actions_copilot",
        "crew_situation_before_departure",
        "crew_situation_airborne",
        "crew_situation_after_landing",
    ];

    /// Collect the literals passed to `guidance.get("...")` across the engine
    /// sources, so a renamed or dropped prompt key fails here rather than at run time.
    fn literal_guidance_keys() -> std::collections::BTreeSet<String> {
        let mut keys = std::collections::BTreeSet::new();
        let mut directories = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
        while let Some(directory) = directories.pop() {
            for entry in std::fs::read_dir(&directory).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    directories.push(path);
                } else if path.extension().and_then(std::ffi::OsStr::to_str) == Some("rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    let mut search = 0;
                    while let Some(found) = text[search..].find("guidance") {
                        let after = search + found + "guidance".len();
                        if let Some(rest) = text[after..].trim_start().strip_prefix(".get(\"")
                            && let Some(end) = rest.find('"')
                        {
                            let key = &rest[..end];
                            if !key.is_empty()
                                && key.chars().all(|c| {
                                    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'
                                })
                            {
                                keys.insert(key.to_owned());
                            }
                        }
                        search = after;
                    }
                }
            }
        }
        keys
    }

    #[test]
    fn bundled_guidance_defines_exactly_the_keys_the_engine_uses() {
        let mut expected = literal_guidance_keys();
        assert!(!expected.is_empty(), "source scan found no guidance keys");
        expected.extend(COMPOSED_GUIDANCE_KEYS.iter().map(|key| (*key).to_owned()));
        let table: toml::Table =
            toml::from_str(include_str!("../../../prompts/interaction.toml")).unwrap();
        let defined: std::collections::BTreeSet<String> = table.keys().cloned().collect();
        let missing: Vec<&String> = expected.difference(&defined).collect();
        assert!(
            missing.is_empty(),
            "code uses guidance keys absent from interaction.toml: {missing:?}"
        );
        let unused: Vec<&String> = defined.difference(&expected).collect();
        assert!(
            unused.is_empty(),
            "interaction.toml defines guidance keys no code path reads: {unused:?}"
        );
        let guidance = Guidance::default();
        for key in &expected {
            assert!(
                !guidance.get(key).trim().is_empty(),
                "empty guidance for {key}"
            );
        }
    }

    #[test]
    fn bundled_role_prompts_define_exactly_the_loaded_fields() {
        let table: toml::Table =
            toml::from_str(include_str!("../../../prompts/prompts.toml")).unwrap();
        let mut keys: Vec<&str> = table.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "atc_reply",
                "cabin",
                "copilot_chat",
                "copilot_readback",
                "crew_actions",
                "crew_response",
                "ground",
                "intent_classify",
            ]
        );
    }
}

/// Shared instruction strings; missing fields retain bundled TOML defaults.
pub struct Guidance(std::collections::BTreeMap<String, String>);
impl Default for Guidance {
    fn default() -> Self {
        Self(
            openatc_core::configuration::from_str(include_str!(
                "../../../prompts/interaction.toml"
            ))
            .expect("bundled interaction instructions"),
        )
    }
}
impl Guidance {
    fn load(path: &Path) -> Self {
        let mut result = Self::default();
        let Ok(text) = std::fs::read_to_string(path) else {
            return result;
        };
        if text.len() > 65536 {
            eprintln!("Shared prompt instructions too large: {}", path.display());
            return result;
        }
        let overrides = match openatc_core::configuration::from_str::<
            std::collections::BTreeMap<String, String>,
        >(&text)
        {
            Ok(values) => values,
            Err(error) => {
                eprintln!(
                    "Invalid shared prompt instructions {}: {error}",
                    path.display()
                );
                return result;
            }
        };
        // A sparse override file may carry a key this build no longer reads, or
        // an empty value. Report each and keep the remaining overrides rather
        // than discarding the whole file.
        for (key, value) in overrides {
            if !result.0.contains_key(&key) {
                eprintln!(
                    "Unknown shared prompt instruction {key} in {}",
                    path.display()
                );
                continue;
            }
            if value.trim().is_empty() || value.len() > 16384 {
                eprintln!(
                    "Ignoring empty or oversized shared prompt instruction {key} in {}",
                    path.display()
                );
                continue;
            }
            result.0.insert(key, value);
        }
        result
    }
    pub fn get(&self, key: &str) -> &str {
        self.0.get(key).expect("known shared prompt instruction")
    }
}

#[cfg(test)]
mod guidance_tests {
    use super::*;
    #[test]
    fn editable_framing_loads_without_requiring_a_complete_new_catalogue() {
        let directory =
            std::env::temp_dir().join(format!("openatc-guidance-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("interaction.toml");
        std::fs::write(&path, "examples = 'Custom example guidance'\n").unwrap();
        let loaded = Guidance::load(&path);
        assert_eq!(loaded.get("examples"), "Custom example guidance");
        assert!(!loaded.get("crew_context").is_empty());
        assert_eq!(
            Guidance::default().get("speech_rules"),
            include_str!("../../../prompts/speech_rules.txt").trim()
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn stale_or_empty_overrides_do_not_discard_the_valid_framing() {
        let directory =
            std::env::temp_dir().join(format!("openatc-guidance-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("interaction.toml");
        std::fs::write(
            &path,
            "examples = 'Custom example guidance'\nretired_key = 'stale'\ncrew_context = '   '\n",
        )
        .unwrap();
        let loaded = Guidance::load(&path);
        assert_eq!(loaded.get("examples"), "Custom example guidance");
        assert_eq!(
            loaded.get("crew_context"),
            Guidance::default().get("crew_context")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
