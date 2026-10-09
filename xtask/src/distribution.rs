use crate::{Result, repo, run};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
};
use walkdir::WalkDir;
fn files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_file() || entry.file_type().is_symlink() && entry.path().is_file() {
            out.push(entry.into_path());
        }
    }
    out.sort();
    Ok(out)
}
fn copy(source: &Path, target: &Path) -> Result {
    fs::create_dir_all(target.parent().ok_or("Destination has no parent")?)?;
    fs::copy(source, target)?;
    Ok(())
}
pub(crate) fn tree(source: &Path, target: &Path) -> Result {
    if !source.is_dir() {
        return Err(format!("Missing directory: {}", source.display()).into());
    }
    for file in files(source)? {
        copy(&file, &target.join(file.strip_prefix(source)?))?;
    }
    Ok(())
}
fn native_plugin() -> &'static str {
    if cfg!(target_os = "windows") {
        "openatc_plugin.dll"
    } else if cfg!(target_os = "macos") {
        "libopenatc_plugin.dylib"
    } else {
        "libopenatc_plugin.so"
    }
}
fn executable(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}
fn atomic_copy(source: &Path, target: &Path) -> Result {
    fs::create_dir_all(target.parent().unwrap())?;
    let temp = tempfile::NamedTempFile::new_in(target.parent().unwrap())?;
    fs::copy(source, temp.path())?;
    temp.persist(target)?;
    Ok(())
}
pub(crate) fn install_plugin(sim: &Path, backup: bool) -> Result {
    let root = repo();
    let release = root.join("target/release");
    let plugin = sim.join("Resources/plugins/OpenATC");
    let mut entries = vec![
        (
            release.join(native_plugin()),
            plugin.join(if cfg!(target_os = "windows") {
                "64/win.xpl"
            } else if cfg!(target_os = "macos") {
                "64/mac.xpl"
            } else {
                "64/lin.xpl"
            }),
        ),
        (
            release.join(executable("open-atc-engine")),
            plugin.join("bin").join(executable("open-atc-engine")),
        ),
    ];
    for name in [
        "radio-stations.toml",
        "regions.toml",
        "intents.toml",
        "README.md",
    ] {
        entries.push((root.join(name), plugin.join(name)));
    }
    for folder in [
        "aircraft",
        "prompts",
        "speech",
        "assets/taxi-arrow",
        "assets/licenses",
        "assets/branding",
        "assets/fonts",
        "assets/geography",
        "assets/controls",
        "docs",
    ] {
        for file in files(&root.join(folder))? {
            entries.push((file.clone(), plugin.join(file.strip_prefix(&root)?)));
        }
    }
    for name in ["xplane", "xplane-sys"] {
        let vendor = root.join("vendor").join(name);
        for file in files(&vendor)? {
            entries.push((
                file.clone(),
                plugin
                    .join("source")
                    .join(name)
                    .join(file.strip_prefix(&vendor)?),
            ));
        }
    }
    for (source, _) in &entries {
        if !source.is_file() {
            return Err(format!("Missing build artifact: {}", source.display()).into());
        }
    }
    run(Command::new(release.join(executable("open-atc-engine")))
        .arg("--check-speech")
        .arg(root.join("speech")))?;
    if backup && plugin.exists() {
        let base = openatc_platform::data_dir().join("openatc-ai/backups");
        fs::create_dir_all(&base)?;
        let saved = tempfile::Builder::new()
            .prefix("OpenATC-")
            .tempdir_in(&base)?
            .keep();
        tree(&plugin, &saved)?;
        println!("Backup: {}", saved.display());
    }
    if plugin.join("speech").exists() {
        for item in fs::read_dir(plugin.join("speech"))? {
            let p = item?.path();
            if p.extension().is_some_and(|e| e == "toml") {
                let mut destination = p.with_extension("toml.legacy");
                let mut n = 1;
                while destination.exists() {
                    destination = p.with_extension(format!("toml.{n}.legacy"));
                    n += 1;
                }
                fs::rename(p, destination)?;
            }
        }
    }
    for (source, target) in entries {
        if source
            .file_name()
            .is_some_and(|n| n == "radio-stations.toml")
            && target.exists()
        {
            continue;
        }
        atomic_copy(&source, &target)?;
    }
    println!("Installed: {}", plugin.display());
    Ok(())
}
fn espeak_data(release: &Path) -> Result<PathBuf> {
    let mut choices = Vec::new();
    for item in fs::read_dir(release.join("build"))? {
        let p = item?.path().join("out/share/espeak-ng-data");
        if p.join("phontab").is_file() {
            choices.push(p);
        }
    }
    choices.sort_by_key(|p| {
        fs::metadata(p.join("phontab"))
            .and_then(|m| m.modified())
            .ok()
    });
    choices
        .pop()
        .ok_or_else(|| "Build openatc-ai before installing: eSpeak data is missing".into())
}
pub(crate) fn install_ai(destination: &Path) -> Result {
    let release = repo().join("target/release");
    let data = espeak_data(&release)?;
    for name in ["openatc-ai", "openatc-stt"] {
        let binary = release.join(executable(name));
        if !binary.is_file() {
            return Err(format!("Missing build artifact: {}", binary.display()).into());
        }
    }
    let bin = destination.join("bin");
    for name in ["openatc-ai", "openatc-stt"] {
        copy(&release.join(executable(name)), &bin.join(executable(name)))?;
    }
    tree(&data, &bin.join("espeak-ng-data"))?;
    shared_libraries(&release, &bin)?;
    println!("Installed AI runtime: {}", bin.display());
    Ok(())
}
fn shared_libraries(source: &Path, target: &Path) -> Result {
    for entry in fs::read_dir(source)? {
        let p = entry?.path();
        let name = p.file_name().unwrap().to_string_lossy();
        if p.is_file()
            && (name.ends_with(".dll") || name.ends_with(".dylib") || name.contains(".so"))
        {
            copy(&p, &target.join(p.file_name().unwrap()))?;
        }
    }
    Ok(())
}
pub(crate) fn collect(product: &str, output: &Path) -> Result {
    let release = repo().join("target/release");
    match product {
        "plugin" => {
            copy(
                &release.join(native_plugin()),
                &output.join(native_plugin()),
            )?;
            copy(
                &release.join(executable("open-atc-engine")),
                &output.join(executable("open-atc-engine")),
            )?;
        }
        "ai-server" => {
            for name in ["openatc-ai", "openatc-stt"] {
                copy(
                    &release.join(executable(name)),
                    &output.join(executable(name)),
                )?;
            }
            tree(&espeak_data(&release)?, &output.join("espeak-ng-data"))?;
        }
        _ => return Err("Expected plugin or ai-server".into()),
    }
    shared_libraries(&release, output)
}
pub(crate) fn audit(binary: &Path) -> Result {
    let result = Command::new("ldd").arg(binary).output()?;
    let text = String::from_utf8_lossy(&result.stdout);
    let forbidden = text.split_whitespace().any(|s| {
        [
            "libX",
            "libxcb",
            "libGLX",
            "libGL.so",
            "libssl",
            "libcrypto",
            "libcurl",
        ]
        .iter()
        .any(|prefix| s.starts_with(prefix))
    });
    if !result.status.success() || forbidden || text.contains("not found") {
        return Err(format!("Dependency audit failed for {}:\n{text}", binary.display()).into());
    }
    if binary.extension().is_some_and(|e| e == "xpl" || e == "so") {
        let symbols = Command::new("nm")
            .args(["-D", "--defined-only"])
            .arg(binary)
            .output()?;
        let exports = String::from_utf8_lossy(&symbols.stdout);
        for name in [
            "XPluginStart",
            "XPluginStop",
            "XPluginEnable",
            "XPluginDisable",
            "XPluginReceiveMessage",
        ] {
            if !exports
                .lines()
                .any(|l| l.split_whitespace().last() == Some(name))
            {
                return Err(format!("Missing plugin export: {name}").into());
            }
        }
    }
    println!("Dependency audit passed: {}", binary.display());
    Ok(())
}
pub(crate) fn package(label: &str) -> Result {
    if label != "linux-x64" || !cfg!(target_os = "linux") {
        return Err("Plugin ZIP packaging currently supports linux-x64".into());
    }
    let root = repo();
    let temp = tempfile::tempdir()?;
    let sim = temp.path().join("simulator");
    install_plugin(&sim, false)?;
    let stage = temp.path().join("stage");
    tree(
        &sim.join("Resources/plugins/OpenATC"),
        &stage.join("OpenATC"),
    )?;
    for name in ["README.md", "LICENSE"] {
        copy(&root.join(name), &stage.join(name))?;
    }
    copy(&root.join("Cargo.lock"), &stage.join("OpenATC/Cargo.lock"))?;
    tree(
        &root.join("crates/audio"),
        &stage.join("OpenATC/source/audio"),
    )?;
    let output = Command::new("cargo")
        .current_dir(&root)
        .args([
            "tree",
            "--locked",
            "--offline",
            "-p",
            "openatc-plugin",
            "-p",
            "openatc-engine",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ])
        .output()?;
    if !output.status.success() {
        return Err("Cannot collect dependency notices".into());
    }
    let cargo = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cargo")
        });
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut parts = line.split_whitespace();
        if let (Some(name), Some(version)) = (parts.next(), parts.next()) {
            for registry in fs::read_dir(cargo.join("registry/src"))? {
                let folder = registry?
                    .path()
                    .join(format!("{name}-{}", version.trim_start_matches('v')));
                if folder.is_dir() {
                    for notice in files(&folder)? {
                        let filename = notice.file_name().unwrap().to_string_lossy().to_lowercase();
                        if ["license", "copying", "notice", "copyright"]
                            .iter()
                            .any(|prefix| filename.starts_with(prefix))
                        {
                            copy(
                                &notice,
                                &stage
                                    .join("OpenATC/assets/licenses/cargo")
                                    .join(folder.file_name().unwrap())
                                    .join(notice.strip_prefix(&folder)?),
                            )?;
                        }
                    }
                }
            }
        }
    }
    let packages = root.join("packages");
    fs::create_dir_all(&packages)?;
    let archive = packages.join(format!("open-atc-{label}.zip"));
    let mut zip = zip::ZipWriter::new(fs::File::create(&archive)?);
    for file in files(&stage)? {
        let name = file
            .strip_prefix(&stage)?
            .to_string_lossy()
            .replace('\\', "/");
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        #[cfg(unix)]
        let options = {
            use std::os::unix::fs::PermissionsExt;
            options.unix_permissions(fs::metadata(&file)?.permissions().mode())
        };
        zip.start_file(name, options)?;
        std::io::copy(&mut fs::File::open(file)?, &mut zip)?;
    }
    zip.finish()?;
    let mut hash = Sha256::new();
    let mut input = fs::File::open(&archive)?;
    let mut buffer = [0u8; 65536];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    fs::File::create(archive.with_extension("zip.sha256"))?.write_all(
        format!(
            "{:x}  {}\n",
            hash.finalize(),
            archive.file_name().unwrap().to_string_lossy()
        )
        .as_bytes(),
    )?;
    println!("Packaged: {}", archive.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_install_replaces_contents_and_preserves_permissions() -> Result {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("engine");
        let installed = temp.path().join("bin/engine");
        fs::write(&source, "new engine")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&source, fs::Permissions::from_mode(0o755))?;
        }
        copy(&source, &installed)?;
        fs::write(&installed, "old engine")?;
        atomic_copy(&source, &installed)?;
        assert_eq!(fs::read_to_string(&installed)?, "new engine");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&installed)?.permissions().mode() & 0o777,
                0o755
            );
        }
        assert_eq!(fs::read_dir(installed.parent().unwrap())?.count(), 1);
        Ok(())
    }

    #[test]
    fn tree_preserves_nested_resources() -> Result {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("source");
        fs::create_dir_all(source.join("speech/ifr"))?;
        fs::write(source.join("speech/ifr/delivery.toml"), "clearance")?;
        let destination = temp.path().join("installed");
        tree(&source, &destination)?;
        assert_eq!(
            fs::read_to_string(destination.join("speech/ifr/delivery.toml"))?,
            "clearance"
        );
        assert!(tree(&temp.path().join("missing"), &destination).is_err());
        Ok(())
    }
}
