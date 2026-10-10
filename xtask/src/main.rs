//! Build, install and validation tools for OpenATC.
mod checks;
mod distribution;
mod release;
use std::{
    error::Error,
    path::{Path, PathBuf},
    process::Command,
};
type Result<T = ()> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .into()
}
fn run(command: &mut Command) -> Result {
    let status = command.status()?;
    if !status.success() {
        return Err(format!("Command failed ({status}): {command:?}").into());
    }
    Ok(())
}
fn main() {
    let launcher = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .is_some_and(|s| s == "openatc-install" || s == "openatc-launcher");
    let result = if launcher {
        release::launch()
    } else {
        dispatch()
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
fn dispatch() -> Result {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let arg = |n: usize| -> Result<&str> {
        args.get(n)
            .map(String::as_str)
            .ok_or_else(|| "Missing argument; run cargo xtask help".into())
    };
    match args.first().map(String::as_str).unwrap_or("help") {
        "install-plugin" => distribution::install_plugin(Path::new(arg(1)?), true),
        "install-ai" => distribution::install_ai(Path::new(arg(1)?)),
        "release-package" => release::package(arg(1)?, arg(2)?, arg(3)?),
        "fetch-package-tools" => release::fetch_tools(Path::new(arg(1)?)),
        "package" => distribution::package(arg(1)?),
        "collect-artifacts" => distribution::collect(
            arg(1)?,
            Path::new(args.get(2).map(String::as_str).unwrap_or("build-artifacts")),
        ),
        "audit-linux" => {
            for file in &args[1..] {
                distribution::audit(Path::new(file))?;
            }
            if args.len() < 2 {
                return Err("Supply binaries to audit".into());
            }
            Ok(())
        }
        "test-radio" | "test-crew" | "test-speech" => checks::exercise(
            &args[0],
            Path::new(arg(1)?),
            Path::new(args.get(2).map(String::as_str).unwrap_or("speech")),
        ),
        "build" => {
            let root = repo();
            run(Command::new("cargo").current_dir(&root).args([
                "build",
                "--locked",
                "--release",
                "-p",
                "openatc-plugin",
                "-p",
                "openatc-engine",
            ]))?;
            run(Command::new("cargo").current_dir(&root).args([
                "test",
                "--locked",
                "-p",
                "openatc-core",
                "-p",
                "openatc-engine",
                "-p",
                "openatc-ui",
                "-p",
                "openatc-plugin",
            ]))?;
            checks::exercise(
                "test-radio",
                &root.join("target/release/open-atc-engine"),
                &root.join("speech"),
            )?;
            if cfg!(target_os = "linux") {
                distribution::audit(&root.join("target/release/libopenatc_plugin.so"))?;
                distribution::audit(&root.join("target/release/open-atc-engine"))?;
            }
            if args.get(1).is_some_and(|a| a == "preview") {
                run(Command::new("cargo").current_dir(root).args([
                    "build",
                    "--locked",
                    "--release",
                    "-p",
                    "openatc-desktop",
                ]))?;
            }
            Ok(())
        }
        "help" => {
            println!(
                "cargo xtask <command>\n  build [preview]\n  install-plugin <X-Plane directory>\n  install-ai <service directory>\n  package linux-x64\n  collect-artifacts <plugin|ai-server> [output]\n  audit-linux <binary>...\n  test-radio|test-crew|test-speech <engine> [speech directory]"
            );
            Ok(())
        }
        other => Err(format!("Unknown command: {other}").into()),
    }
}
