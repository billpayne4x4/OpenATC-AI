//! Shared schema-driven terminal and local browser setup.
use clap::{CommandFactory, Parser};
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Write},
    path::Path,
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Field {
    key: String,
    label: String,
    kind: String,
    default: toml::Value,
    #[serde(default)]
    help: String,
    #[serde(default)]
    choices: Vec<String>,
    #[serde(default)]
    section: String,
    #[serde(default)]
    advanced: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Schema {
    field: Vec<Field>,
}
pub(super) fn fields(path: Option<&Path>) -> Result<Vec<Field>, String> {
    let installed = std::env::current_exe()
        .ok()
        .and_then(|exe| {
            exe.parent().map(|parent| {
                let beside = parent.join("server/settings-schema.toml");
                if beside.is_file() {
                    beside
                } else {
                    parent.join("../server/settings-schema.toml")
                }
            })
        })
        .filter(|path| path.is_file());
    let path = path.or(installed.as_deref());
    let source = match path {
        Some(path) => std::fs::read_to_string(path).map_err(|error| error.to_string())?,
        None => include_str!("../../../server/settings-schema.toml").into(),
    };
    if source.len() > 65536 {
        return Err("Settings schema exceeds 64 KiB".into());
    }
    let schema: Schema = toml::from_str(&source).map_err(|error| error.to_string())?;
    let command = crate::Args::command();
    let mut keys = std::collections::BTreeSet::new();
    if schema.field.len() > 128 {
        return Err("Settings schema exceeds 128 fields".into());
    }
    for field in &schema.field {
        let typed_default = match field.kind.as_str() {
            "string" => field.default.is_str(),
            "integer" => field.default.is_integer(),
            "boolean" => field.default.is_bool(),
            _ => false,
        };
        if !keys.insert(&field.key) || !typed_default {
            return Err(format!("Duplicate or invalid settings field {}", field.key));
        }
        if !command
            .get_arguments()
            .any(|argument| argument.get_long() == Some(field.key.as_str()))
            || !["string", "integer", "boolean"].contains(&field.kind.as_str())
        {
            return Err(format!("Unsupported settings field {}", field.key));
        }
    }
    Ok(schema.field)
}
pub(super) fn values(path: &Path) -> Result<toml::Table, String> {
    match std::fs::read_to_string(path) {
        Ok(source) => toml::from_str(&source).map_err(|error| error.to_string()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(toml::Table::new()),
        Err(error) => Err(error.to_string()),
    }
}
fn normalize(mut values: toml::Table) -> toml::Table {
    values.retain(|key, value| {
        !(matches!(value, toml::Value::String(value) if value.is_empty())
            || (matches!(key, "threads" | "batch-threads" | "llm-bytes")
                && value.as_integer() == Some(0)))
    });
    if values.get("ctx-auto").and_then(toml::Value::as_bool) == Some(true) {
        values.remove("ctx-size");
    }
    values
}
pub(super) fn save(path: &Path, values: toml::Table) -> Result<(), String> {
    let source = toml::to_string_pretty(&normalize(values)).map_err(|error| error.to_string())?;
    let arguments =
        crate::config::arguments(&source, &["openatc-ai".into()], &crate::Args::command())?;
    let args = crate::Args::try_parse_from(arguments).map_err(|error| error.to_string())?;
    crate::llm_config::configured_manifest(&args)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    if path.exists() {
        std::fs::copy(path, path.with_extension("toml.bak")).map_err(|error| error.to_string())?;
    }
    let temporary = path.with_extension("toml.tmp");
    std::fs::write(&temporary, source).map_err(|error| error.to_string())?;
    match std::fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(_) if path.exists() => {
            std::fs::remove_file(path).map_err(|error| error.to_string())?;
            std::fs::rename(&temporary, path).map_err(|error| {
                let _ = std::fs::copy(path.with_extension("toml.bak"), path);
                error.to_string()
            })
        }
        Err(error) => Err(error.to_string()),
    }
}
pub fn terminal(path: &Path, schema: Option<&Path>) -> Result<(), String> {
    let mut current = values(path)?;
    for field in fields(schema)? {
        loop {
            let default = current.get(&field.key).unwrap_or(&field.default);
            println!("{} {}", field.label, field.help);
            if !field.choices.is_empty() {
                println!("Choices: {}", field.choices.join(", "));
            }
            print!("[{default}]: ");
            io::stdout().flush().map_err(|error| error.to_string())?;
            let mut line = String::new();
            if io::stdin()
                .read_line(&mut line)
                .map_err(|error| error.to_string())?
                == 0
            {
                return Err("Setup input closed; configuration was not saved".into());
            }
            let line = line.trim();
            let value = if line.is_empty() {
                Some(default.clone())
            } else {
                match field.kind.as_str() {
                    "boolean" => line.parse::<bool>().ok().map(toml::Value::Boolean),
                    "integer" => line.parse::<i64>().ok().map(toml::Value::Integer),
                    _ => Some(toml::Value::String(line.into())),
                }
            };
            if let Some(value) = value
                && (field.choices.is_empty()
                    || value
                        .as_str()
                        .is_some_and(|value| field.choices.iter().any(|choice| choice == value)))
            {
                current.insert(field.key, value);
                break;
            }
            println!("Please enter a valid {} value.", field.kind);
        }
    }
    save(path, current)?;
    println!("Saved {}", path.display());
    Ok(())
}
pub(super) fn open_browser(origin: &str) {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = tokio::process::Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = tokio::process::Command::new("open");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut command = tokio::process::Command::new("xdg-open");
    let child = command
        .arg(origin)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let origin = origin.to_owned();
    match child {
        Ok(mut child) => {
            tokio::spawn(async move {
                if !child.wait().await.is_ok_and(|status| status.success()) {
                    eprintln!(
                        "Could not open the browser automatically. Open {origin} in your browser."
                    );
                }
            });
        }
        Err(error) => eprintln!(
            "Could not open the browser automatically: {error}. Open {origin} in your browser."
        ),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn shipped_form_defaults_validate_and_do_not_enable_auto() {
        use clap::CommandFactory;
        let fields = super::fields(None).unwrap();
        let values = fields
            .into_iter()
            .map(|field| (field.key, field.default))
            .collect::<toml::Table>();
        let values = super::normalize(values);
        assert_eq!(
            values.get("ctx-auto").and_then(toml::Value::as_bool),
            Some(false)
        );
        assert!(!values.contains_key("llm-manifest"));
        let source = toml::to_string(&values).unwrap();
        let args =
            crate::config::arguments(&source, &["server".into()], &crate::Args::command()).unwrap();
        assert!(crate::Args::command().try_get_matches_from(args).is_ok());
    }
}
