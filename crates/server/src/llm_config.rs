//! Model selection and bounded context allocation policy.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Model {
    #[value(name = "qwen2.5-7b")]
    Qwen25,
    #[value(name = "qwen3-8b")]
    Qwen3,
    #[value(name = "qwen3.5-4b")]
    Qwen35,
}
impl Model {
    pub fn manifest_name(self) -> &'static str {
        match self {
            Self::Qwen25 => "qwen2.5-7b-instruct-q4_k_m",
            Self::Qwen3 => "qwen3-8b-q4_k_m",
            Self::Qwen35 => "qwen3.5-4b-q4_k_m",
        }
    }
    pub fn direct_reply(self) -> bool {
        !matches!(self, Self::Qwen25)
    }
}
/// Single NVIDIA device only; other backends retain explicit context sizing.
pub fn free_vram_mib() -> Option<u64> {
    let output = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=memory.free", "--format=csv,noheader,nounits"])
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let text = String::from_utf8(output.stdout).ok()?;
    let lines: Vec<_> = text.lines().collect();
    if lines.len() != 1 {
        return None;
    }
    lines[0].trim().parse().ok()
}
pub fn candidates(fixed: u32, automatic: bool, maximum: u32) -> Vec<u32> {
    if !automatic {
        return vec![fixed];
    }
    let mut result = vec![];
    let mut size = maximum;
    while size >= 2048 {
        result.push(size);
        size /= 2;
    }
    result
}

/// Custom downloads remain inside the model directory and use pinned content.
pub fn custom_manifest(source: &str) -> Result<openatc_ai_core::Manifest, String> {
    if source.len() > 65536 {
        return Err("Custom model manifest exceeds 64 KiB".into());
    }
    let mut manifest =
        openatc_ai_core::Manifest::parse(source).map_err(|error| error.to_string())?;
    if manifest.model.len() != 1 {
        return Err("Custom manifest must contain exactly one language model".into());
    }
    let model = &manifest.model[0];
    let safe_path = model.path.starts_with("llm/")
        && std::path::Path::new(&model.path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("gguf"))
        && !model.path.contains('\\')
        && std::path::Path::new(&model.path)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)));
    if !safe_path
        || model.name.is_empty()
        || model.name.len() > 128
        || !model
            .name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        || !model.url.starts_with("https://")
        || model.size == 0
        || model.sha256.len() != 64
        || !model.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(
            "Custom model needs a safe llm/*.gguf path, name, HTTPS URL, size and SHA-256".into(),
        );
    }
    for source in [
        include_str!("../../../models.toml"),
        include_str!("../../../models-kitten.toml"),
    ] {
        let bundled = openatc_ai_core::Manifest::parse(source).map_err(|e| e.to_string())?;
        if bundled
            .model
            .iter()
            .any(|entry| entry.name == model.name || entry.path == model.path)
        {
            return Err(
                "Choose a custom model name and file path that do not overlap a bundled model"
                    .into(),
            );
        }
    }
    manifest.model[0].sha256.make_ascii_lowercase();
    Ok(manifest)
}
#[cfg(test)]
mod tests {
    #[test]
    fn custom_models_require_pinned_downloads_and_contained_paths() {
        let entry = format!(
            "[[model]]\nname='custom-instruct'\npath='llm/custom.gguf'\nurl='https://example.org/model.gguf'\nsize=1024\nsha256='{}'",
            "a".repeat(64)
        );
        assert!(super::custom_manifest(&entry).is_ok());
        for source in [
            entry.replace("llm/custom.gguf", "llm/../../custom.gguf"),
            entry.replace("https://", "http://"),
            entry.replace("size=1024", "size=0"),
            entry.replace(&"a".repeat(64), "not-a-hash"),
        ] {
            assert!(super::custom_manifest(&source).is_err());
        }
    }
    #[test]
    fn form_custom_downloads_require_all_pinned_fields() {
        use clap::Parser;
        let hash = "A".repeat(64);
        let args = crate::Args::try_parse_from([
            "server",
            "--llm-url",
            "https://example.org/model.gguf",
            "--llm-name",
            "custom",
            "--llm-file",
            "llm/custom.gguf",
            "--llm-sha256",
            &hash,
            "--llm-bytes",
            "1024",
        ])
        .unwrap();
        let manifest = super::configured_manifest(&args).unwrap().unwrap();
        assert_eq!(manifest.model[0].sha256, "a".repeat(64));
        let incomplete =
            crate::Args::try_parse_from(["server", "--llm-url", "https://example.org/model.gguf"])
                .unwrap();
        assert!(super::configured_manifest(&incomplete).is_err());
    }
    #[test]
    fn allocation_is_bounded_and_explicit_size_is_preserved() {
        assert_eq!(super::candidates(4096, false, 32768), vec![4096]);
        assert_eq!(super::candidates(4096, true, 8192), vec![8192, 4096, 2048]);
        assert!(super::Model::Qwen35.direct_reply());
        assert!(!super::Model::Qwen25.direct_reply());
    }
}

/// The form and command line produce the same pinned manifest as a user-authored file.
pub fn configured_manifest(
    args: &crate::Args,
) -> Result<Option<openatc_ai_core::Manifest>, String> {
    if let Some(path) = &args.llm_manifest {
        let source = std::fs::read_to_string(path)
            .map_err(|error| format!("cannot read custom model manifest: {error}"))?;
        return custom_manifest(&source).map(Some);
    }
    let Some(url) = &args.llm_url else {
        if args.llm_name.is_some()
            || args.llm_file.is_some()
            || args.llm_sha256.is_some()
            || args.llm_bytes.is_some()
        {
            return Err("Custom model fields require llm-url".into());
        }
        return Ok(None);
    };
    let mut entry = toml::Table::new();
    for (key, value) in [
        ("name", args.llm_name.as_ref()),
        ("path", args.llm_file.as_ref()),
        ("sha256", args.llm_sha256.as_ref()),
    ] {
        entry.insert(
            key.into(),
            toml::Value::String(
                value
                    .ok_or_else(|| format!("Custom model requires llm-{key}"))?
                    .clone(),
            ),
        );
    }
    entry.insert("url".into(), toml::Value::String(url.clone()));
    let size = i64::try_from(args.llm_bytes.ok_or("Custom model requires llm-bytes")?)
        .map_err(|_| "Custom model size is too large")?;
    entry.insert("size".into(), toml::Value::Integer(size));
    let mut manifest = toml::Table::new();
    manifest.insert(
        "model".into(),
        toml::Value::Array(vec![toml::Value::Table(entry)]),
    );
    custom_manifest(&toml::to_string(&manifest).map_err(|error| error.to_string())?).map(Some)
}

#[cfg(test)]
mod cache_identity_tests {
    #[test]
    fn custom_models_cannot_alias_bundled_cache_entries() {
        let source = format!(
            "[[model]]\nname='custom-test'\npath='llm/custom.gguf'\nurl='https://example.org/custom.gguf'\nsize=42\nsha256='{}'",
            "a".repeat(64)
        );
        assert!(super::custom_manifest(&source).is_ok());
        assert!(
            super::custom_manifest(&source.replace("custom-test", "qwen2.5-7b-instruct-q4_k_m"))
                .is_err()
        );
        let bundled =
            openatc_ai_core::Manifest::parse(include_str!("../../../models.toml")).unwrap();
        let path = &bundled
            .model
            .iter()
            .find(|model| model.path.starts_with("llm/"))
            .unwrap()
            .path;
        assert!(super::custom_manifest(&source.replace("llm/custom.gguf", path)).is_err());
    }
}
