//! Translate saved server settings through the same validation as command-line options.
use std::ffi::OsString;

pub fn arguments(
    source: &str,
    command_line: &[OsString],
    command: &clap::Command,
) -> Result<Vec<OsString>, String> {
    let settings: toml::Table =
        toml::from_str(source).map_err(|error| format!("invalid server configuration: {error}"))?;
    let auto = settings.get("ctx-auto").and_then(toml::Value::as_bool) == Some(true)
        || command_line.iter().any(|arg| arg == "--ctx-auto");
    let explicit_size = command_line
        .iter()
        .any(|arg| arg == "--ctx-size" || arg.to_string_lossy().starts_with("--ctx-size="));
    let mut result = vec![
        command_line
            .first()
            .cloned()
            .unwrap_or_else(|| "openatc-ai".into()),
    ];
    for (key, value) in settings {
        if key == "ctx-size" && auto && !explicit_size {
            continue;
        }
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
        {
            return Err(format!("invalid setting name: {key}"));
        }
        let option = command
            .get_arguments()
            .find(|argument| argument.get_long() == Some(key.as_str()))
            .ok_or_else(|| format!("unknown setting: {key}"))?;
        if [
            "config",
            "help",
            "version",
            "configure",
            "configure-web",
            "manage-web",
            "no-browser",
            "dashboard-port",
            "dashboard-host",
            "managed-events",
            "settings-schema",
        ]
        .contains(&key.as_str())
        {
            return Err(format!("{key} cannot be stored as a server setting"));
        }
        let boolean = matches!(option.get_action(), clap::ArgAction::SetTrue);
        if boolean != value.is_bool() {
            return Err(format!("setting {key} has the wrong value type"));
        }
        let flag = format!("--{key}");
        if key == "ctx-auto"
            && command_line.iter().any(|argument| {
                let argument = argument.to_string_lossy();
                argument == "--ctx-size" || argument.starts_with("--ctx-size=")
            })
        {
            continue;
        }
        if command_line.iter().skip(1).any(|argument| {
            let argument = argument.to_string_lossy();
            argument == flag || argument.starts_with(&format!("{flag}="))
        }) {
            continue;
        }
        match value {
            toml::Value::Boolean(true) => result.push(flag.into()),
            toml::Value::Boolean(false) => {}
            toml::Value::String(value) => result.push(format!("{flag}={value}").into()),
            toml::Value::Integer(value) => result.push(format!("{flag}={value}").into()),
            _ => {
                return Err(format!(
                    "setting {key} must be a string, integer or boolean"
                ));
            }
        }
    }
    result.extend(command_line.iter().skip(1).cloned());
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn automatic_context_omits_saved_fixed_size() {
        use clap::{CommandFactory, Parser};
        let args = arguments(
            "ctx-auto=true\nctx-size=4096",
            &["server".into()],
            &crate::Args::command(),
        )
        .unwrap();
        let parsed = crate::Args::try_parse_from(args).unwrap();
        assert!(parsed.ctx_auto);
        let args = arguments(
            "ctx-auto=true\nctx-size=4096",
            &["server".into(), "--ctx-size=8192".into()],
            &crate::Args::command(),
        )
        .unwrap();
        let parsed = crate::Args::try_parse_from(args).unwrap();
        assert!(!parsed.ctx_auto);
        assert_eq!(parsed.ctx_size, 8192);
    }
    #[test]
    fn explicit_options_override_saved_values_without_shell_interpretation() {
        use clap::CommandFactory;
        let cli = ["server", "--ctx-size", "8192"].map(OsString::from);
        let command = crate::Args::command();
        let args = arguments(
            "ctx-size = 4096\nforce-tts-cpu = true\nmodels-dir = 'C:\\Models with spaces'",
            &cli,
            &command,
        )
        .unwrap();
        assert_eq!(args.iter().filter(|arg| *arg == "--ctx-size").count(), 1);
        assert!(args.contains(&OsString::from("--force-tts-cpu")));
        assert!(args.contains(&OsString::from("--models-dir=C:\\Models with spaces")));
        assert_eq!(args.last().unwrap(), "8192");
        assert!(arguments("[unknown]\nvalue = 1", &cli, &command).is_err());
        assert!(arguments("unknown = false", &cli, &command).is_err());
        assert!(arguments("ctx-size = false", &cli, &command).is_err());
    }
}
