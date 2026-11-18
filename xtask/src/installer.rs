//! Discover installations and replace only the plugin tree, preserving user files.
use crate::Result;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    process::Command,
};

const MANIFEST: &str = "openatc-install-manifest.json";

fn text(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut value = String::new();
    file.take(262145).read_to_string(&mut value).ok()?;
    (value.len() <= 262144).then_some(value)
}

// Steam's configuration is data. Never evaluate or source its contents.
fn tokens(input: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut word = String::new();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => {
                    if let Some(next) = chars.next() {
                        if next != '\\' && next != '"' {
                            word.push('\\');
                        }
                        word.push(next);
                    }
                }
                _ => word.push(c),
            }
        }
        result.push(word);
    }
    result
}

fn executable_name() -> &'static str {
    if cfg!(windows) {
        "X-Plane.exe"
    } else if cfg!(target_os = "macos") {
        "X-Plane.app/Contents/MacOS/X-Plane"
    } else {
        "X-Plane-x86_64"
    }
}

fn steam_identifies_xplane12(root: &Path) -> bool {
    let Some(common) = root.parent() else {
        return false;
    };
    let Some(apps) = common.parent() else {
        return false;
    };
    let Some(source) = text(&apps.join("appmanifest_2014780.acf")) else {
        return false;
    };
    let words = tokens(&source);
    if !words
        .windows(2)
        .any(|pair| pair[0] == "appid" && pair[1] == "2014780")
    {
        return false;
    }
    let Some(pair) = words.windows(2).find(|pair| pair[0] == "installdir") else {
        return false;
    };
    let relative = Path::new(&pair[1]);
    if relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return false;
    }
    // Match the manifest's actual installation, not a similarly named nearby folder.
    fs::canonicalize(apps.join("common").join(relative)).is_ok_and(|path| path == root)
}

pub fn validate(root: &Path) -> Result<PathBuf> {
    let root = fs::canonicalize(root)?;
    if !root.join("Resources/plugins").is_dir() || !root.join(executable_name()).is_file() {
        return Err("Choose the native X-Plane 12 root folder, not its plugins folder or a Windows/Proton installation.".into());
    }
    let mut header = [0u8; 64];
    let mut executable = fs::File::open(root.join(executable_name()))?;
    executable.read_exact(&mut header)?;
    let native = if cfg!(windows) {
        if &header[..2] != b"MZ" {
            false
        } else {
            let offset = u32::from_le_bytes(header[60..64].try_into()?);
            if offset > 16 * 1024 * 1024 {
                false
            } else {
                executable.seek(SeekFrom::Start(u64::from(offset)))?;
                let mut pe = [0u8; 6];
                executable.read_exact(&mut pe)?;
                &pe[..4] == b"PE\0\0" && u16::from_le_bytes([pe[4], pe[5]]) == 0x8664
            }
        }
    } else if cfg!(target_os = "macos") {
        let cpu = if cfg!(target_arch = "aarch64") {
            0x0100000cu32
        } else {
            0x01000007u32
        };
        match &header[..4] {
            [0xcf, 0xfa, 0xed, 0xfe] => u32::from_le_bytes(header[4..8].try_into()?) == cpu,
            [0xfe, 0xed, 0xfa, 0xcf] => u32::from_be_bytes(header[4..8].try_into()?) == cpu,
            [0xca, 0xfe, 0xba, 0xbe] | [0xca, 0xfe, 0xba, 0xbf] => {
                let count = u32::from_be_bytes(header[4..8].try_into()?);
                let stride = if header[3] == 0xbf { 32 } else { 20 };
                let mut found = false;
                if count <= 64 {
                    for index in 0..count {
                        executable.seek(SeekFrom::Start(8 + u64::from(index * stride)))?;
                        let mut entry = [0u8; 4];
                        executable.read_exact(&mut entry)?;
                        found |= u32::from_be_bytes(entry) == cpu;
                    }
                }
                found
            }
            _ => false,
        }
    } else {
        &header[..4] == b"\x7fELF"
            && header[4] == 2
            && header[5] == 1
            && u16::from_le_bytes([header[18], header[19]]) == 62
    };
    if !native {
        return Err("The simulator executable does not match this platform's installer.".into());
    }
    let mut log = Vec::new();
    match fs::File::open(root.join("Log.txt")) {
        Ok(file) => { file.take(4096).read_to_end(&mut log)?; }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && steam_identifies_xplane12(&root) => return Ok(root),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err("This folder has no X-Plane 12 log or matching Steam manifest. For standalone installations, start X-Plane once, then close it before installing.".into()),
        Err(error) => return Err(error.into()),
    }
    if !String::from_utf8_lossy(&log)
        .lines()
        .next()
        .is_some_and(|line| line.starts_with("Log.txt for X-Plane 12."))
    {
        return Err("The simulator log does not identify an X-Plane 12 installation.".into());
    }
    Ok(root)
}

pub fn discover(home: &Path, extra_steam: &[PathBuf]) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for record in [
        home.join(".x-plane/x-plane_install_12.txt"),
        home.join(".config/x-plane_install_12.txt"),
        home.join("Library/Preferences/x-plane_install_12.txt"),
        home.join("AppData/Local/x-plane_install_12.txt"),
    ] {
        if let Some(value) = text(&record) {
            candidates.extend(
                value
                    .lines()
                    .filter(|v| !v.trim().is_empty())
                    .map(|v| PathBuf::from(v.trim())),
            );
        }
    }
    candidates.extend([home.join("X-Plane 12"), home.join("Desktop/X-Plane 12")]);
    let mut roots = vec![
        home.join(".steam/steam"),
        home.join(".steam/root"),
        home.join(".local/share/Steam"),
        home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
        home.join(".var/app/com.valvesoftware.Steam/data/Steam"),
        home.join("snap/steam/common/.local/share/Steam"),
        home.join("snap/steam/current/.local/share/Steam"),
        home.join("Library/Application Support/Steam"),
    ];
    roots.extend(extra_steam.iter().cloned());
    let mut libraries = roots.clone();
    for root in roots {
        if let Some(value) = text(&root.join("steamapps/libraryfolders.vdf")) {
            let words = tokens(&value);
            for pair in words.windows(2) {
                if pair[0] == "path" || pair[0].parse::<u32>().is_ok() {
                    let path = PathBuf::from(&pair[1]);
                    if path.is_absolute() {
                        libraries.push(path);
                    }
                }
            }
        }
    }
    libraries.sort();
    libraries.dedup();
    for library in libraries.into_iter().take(128) {
        let apps = library.join("steamapps");
        candidates.push(apps.join("common/X-Plane 12"));
        if let Ok(entries) = fs::read_dir(&apps) {
            for entry in entries.filter_map(std::result::Result::ok).take(4096) {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with("appmanifest_") || !name.ends_with(".acf") {
                    continue;
                }
                let Some(value) = text(&entry.path()) else {
                    continue;
                };
                let words = tokens(&value);
                if !words
                    .windows(2)
                    .any(|p| p[0] == "name" && p[1] == "X-Plane 12")
                {
                    continue;
                }
                if let Some(pair) = words.windows(2).find(|p| p[0] == "installdir") {
                    let relative = Path::new(&pair[1]);
                    if relative
                        .components()
                        .all(|c| matches!(c, Component::Normal(_)))
                    {
                        candidates.push(apps.join("common").join(relative));
                    }
                }
            }
        }
    }
    let mut found = candidates
        .into_iter()
        .filter_map(|p| validate(&p).ok())
        .collect::<Vec<_>>();
    found.sort();
    found.dedup();
    found
}

// Folder pickers expect ordinary drive/UNC paths, not Rust's extended-length spelling.
fn windows_folder_path(path: &str) -> String {
    if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{unc}");
    }
    if let Some(drive) = path.strip_prefix(r"\\?\") {
        let bytes = drive.as_bytes();
        if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1..3] == *b":\\" {
            return drive.to_owned();
        }
    }
    path.to_owned()
}

fn home() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_default()
}
fn steam_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for name in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(path) = std::env::var_os(name) {
            roots.push(PathBuf::from(path).join("Steam"));
        }
    }
    #[cfg(windows)]
    if let Ok(output) = Command::new("reg.exe")
        .args(["query", "HKCU\\Software\\Valve\\Steam", "/v", "SteamPath"])
        .output()
        && output.status.success()
    {
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if let Some((_, path)) = line.split_once("REG_SZ") {
                roots.push(PathBuf::from(path.trim()));
            }
        }
    }
    roots
}

pub fn closed() -> Result {
    let output = if cfg!(windows) {
        Command::new("tasklist.exe")
            .args(["/FO", "CSV", "/NH"])
            .output()?
    } else {
        Command::new("ps").args(["-A", "-o", "comm="]).output()?
    };
    if !output.status.success() {
        return Err("Cannot check running processes. Close X-Plane and retry.".into());
    }
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let name = line
            .trim()
            .split(',')
            .next()
            .unwrap_or("")
            .trim_matches('"');
        let name = Path::new(name)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        if ["x-plane", "x-plane.exe", "x-plane-x86_64", "x-plane-arm64"].contains(&name.as_str()) {
            return Err("Close X-Plane before installing, updating or removing OpenATC.".into());
        }
    }
    Ok(())
}

fn hash(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
fn files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_symlink() {
            return Err(format!(
                "Refusing to follow a symbolic link in the plugin tree: {}",
                entry.path().display()
            )
            .into());
        }
        if entry.file_type().is_file() {
            files.push(entry.path().strip_prefix(root)?.to_owned());
        }
    }
    Ok(files)
}
fn manifest(root: &Path) -> Result<BTreeMap<String, String>> {
    let path = root.join(MANIFEST);
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let mut text = String::new();
    fs::File::open(path)?
        .take(8388609)
        .read_to_string(&mut text)?;
    if text.len() > 8388608 {
        return Err("Plugin ownership manifest is too large.".into());
    }
    let value: serde_json::Value = serde_json::from_str(&text)?;
    if value["version"] != 1 {
        return Err("Unsupported plugin ownership manifest.".into());
    }
    let entries: BTreeMap<String, String> = serde_json::from_value(value["files"].clone())?;
    for (name, digest) in &entries {
        if name.is_empty()
            || name.contains([':', '\\'])
            || !Path::new(name)
                .components()
                .all(|c| matches!(c, Component::Normal(_)))
            || digest.len() != 64
            || !digest.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("Invalid path or checksum in plugin ownership manifest.".into());
        }
    }
    Ok(entries)
}
fn key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
fn copy(source: &Path, destination: &Path) -> Result {
    fs::create_dir_all(destination.parent().ok_or("Missing parent")?)?;
    fs::copy(source, destination)?;
    Ok(())
}

fn replace(stage: &Path, target: &Path, backups: &Path, check: fn() -> Result) -> Result {
    check()?;
    let backup = if target.exists() {
        fs::create_dir_all(backups)?;
        let reserved = tempfile::Builder::new()
            .prefix("OpenATC-")
            .tempdir_in(backups)?;
        let path = reserved.path().to_owned();
        reserved.close()?;
        fs::rename(target, &path)?;
        Some(path)
    } else {
        None
    };
    if let Err(error) = fs::rename(stage, target) {
        if let Some(backup) = &backup {
            fs::rename(backup, target).map_err(|restore| {
                format!(
                    "Install failed ({error}); rollback failed ({restore}). Recover {}",
                    backup.display()
                )
            })?;
        }
        return Err(error.into());
    }
    if let Some(backup) = backup {
        println!("Previous plugin retained at {}", backup.display());
    }
    Ok(())
}

fn payload_binaries() -> [PathBuf; 2] {
    [
        PathBuf::from(if cfg!(windows) {
            "64/win.xpl"
        } else if cfg!(target_os = "macos") {
            "64/mac.xpl"
        } else {
            "64/lin.xpl"
        }),
        PathBuf::from(format!(
            "bin/open-atc-engine{}",
            std::env::consts::EXE_SUFFIX
        )),
    ]
}

pub fn change(sim: &Path, payload: Option<&Path>) -> Result {
    change_with(sim, payload, closed)
}
fn change_with(sim: &Path, payload: Option<&Path>, check: fn() -> Result) -> Result {
    let sim = validate(sim)?;
    check()?;
    let parent = fs::canonicalize(sim.join("Resources/plugins"))?;
    let target = parent.join("OpenATC");
    if target.is_symlink() {
        return Err("The OpenATC folder is a symlink; use manual installation instead.".into());
    }
    let lock = parent.join(".openatc-install.lock");
    let mut guard = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
        .map_err(|e| {
            format!(
                "Cannot acquire installer lock {}: {e}. Another installer may be running.",
                lock.display()
            )
        })?;
    struct Unlock(PathBuf);
    impl Drop for Unlock {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let _unlock = Unlock(lock);
    writeln!(guard, "{}", std::process::id())?;
    drop(guard);
    let stage = tempfile::Builder::new()
        .prefix(".openatc-stage-")
        .tempdir_in(&parent)?;
    let old = manifest(&target)?;
    if payload.is_none() && old.is_empty() {
        return Err("No managed installation manifest. Back up and remove a legacy/manual installation manually.".into());
    }
    let mut owned = BTreeMap::new();
    if let Some(payload) = payload {
        for required in payload_binaries() {
            if fs::metadata(payload.join(&required)).map_or(true, |m| !m.is_file() || m.len() == 0)
            {
                return Err(format!("Incomplete plugin payload: missing {}. Extract the complete download before installing.", required.display()).into());
            }
        }
        for relative in files(payload)? {
            if relative == Path::new(MANIFEST) {
                continue;
            }
            copy(&payload.join(&relative), &stage.path().join(&relative))?;
            owned.insert(key(&relative), hash(&payload.join(&relative))?);
        }
        if owned.is_empty() {
            return Err("Installer payload is empty.".into());
        }
    }
    if target.exists() {
        for relative in files(&target)? {
            if relative == Path::new(MANIFEST) {
                continue;
            }
            let name = key(&relative);
            let path = target.join(&relative);
            let changed = if let Some(digest) = old.get(&name) {
                hash(&path)? != *digest
            } else {
                false
            };
            let editable = relative
                .extension()
                .is_some_and(|e| ["toml", "json", "txt", "md"].iter().any(|v| e == *v));
            let legacy_override = if old.is_empty() && editable {
                match owned.get(&name) {
                    Some(digest) => hash(&path)? != *digest,
                    None => false,
                }
            } else {
                false
            };
            let unowned = !old.contains_key(&name) && !owned.contains_key(&name);
            if unowned || changed && (editable || payload.is_none()) || legacy_override {
                copy(&path, &stage.path().join(&relative))?;
                if changed || legacy_override {
                    println!("Preserved customised file: {name}");
                }
                if let Some(baseline) = old.get(&name)
                    && payload.is_some()
                {
                    owned.insert(name, baseline.clone());
                }
            }
        }
    }
    if payload.is_some() {
        fs::write(
            stage.path().join(MANIFEST),
            serde_json::to_vec_pretty(&serde_json::json!({"version":1,"files":owned}))?,
        )?;
    }
    let backups = parent
        .parent()
        .ok_or("Missing plugin parent")?
        .join("OpenATC Backups");
    replace(stage.path(), &target, &backups, check)?;
    if payload.is_none() {
        if fs::read_dir(&target)?.next().is_none() {
            fs::remove_dir(&target)?;
        }
        println!(
            "OpenATC removed. Personal settings, models outside the plugin, backups and modified/unowned files were preserved."
        );
    } else {
        println!("Installed OpenATC in {}", target.display());
    }
    Ok(())
}

fn select(paths: &[PathBuf]) -> Result<PathBuf> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        return Err(
            "Supply the X-Plane root as an argument when running without an interactive terminal."
                .into(),
        );
    }
    for (i, path) in paths.iter().enumerate() {
        println!("{}. {}", i + 1, path.display());
    }
    print!("Select a number, or enter the X-Plane 12 root path (blank cancels): ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let line = line.trim();
    if line.is_empty() {
        return Err("Installation cancelled.".into());
    }
    if let Ok(index) = line.parse::<usize>() {
        return paths
            .get(index.wrapping_sub(1))
            .cloned()
            .ok_or_else(|| "Invalid installation selection.".into());
    }
    Ok(PathBuf::from(line.trim_matches('"')))
}

pub fn launch(bundle: &Path, args: &[std::ffi::OsString]) -> Result {
    let error_file = args
        .windows(2)
        .find(|p| p[0] == "--error-file")
        .map(|p| PathBuf::from(&p[1]));
    let result = launch_inner(bundle, args);
    if let (Err(error), Some(path)) = (&result, error_file) {
        fs::write(
            path,
            format!("{}{}", if cfg!(windows) { "\u{feff}" } else { "" }, error),
        )?;
    }
    result
}
fn launch_inner(bundle: &Path, args: &[std::ffi::OsString]) -> Result {
    if args.first().is_some_and(|s| s == "--help") {
        println!(
            "OpenATC installer: [--install|--uninstall|--check] XPLANE_ROOT [--yes]\n--discover-list FILE lists validated installations. Close X-Plane first. Personal settings are preserved."
        );
        return Ok(());
    }
    if args.first().is_some_and(|s| s == "--check-closed") {
        return closed();
    }
    if args.first().is_some_and(|s| s == "--discover-list") {
        let path = args.get(1).ok_or("Missing discovery output file")?;
        let found = discover(&home(), &steam_roots());
        fs::write(
            path,
            format!(
                "{}{}",
                if cfg!(windows) { "\u{feff}" } else { "" },
                found
                    .iter()
                    .map(|p| {
                        let path = p.to_string_lossy();
                        if cfg!(windows) {
                            windows_folder_path(&path)
                        } else {
                            path.into_owned()
                        }
                    })
                    .filter(|p| !p.contains(['\r', '\n']))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        )?;
        return Ok(());
    }
    let recorded = args.first().is_some_and(|s| s == "--uninstall-record");
    #[allow(unused_mut)]
    let mut remove = recorded || args.first().is_some_and(|s| s == "--uninstall");
    let flag = args.first().is_some_and(|s| {
        s == "--install" || s == "--uninstall" || s == "--check" || s == "--uninstall-record"
    });
    let supplied = args
        .get(usize::from(flag))
        .filter(|p| !p.to_string_lossy().starts_with("--"));
    #[cfg(target_os = "macos")]
    if args.is_empty() {
        let choice = Command::new("osascript").args(["-e", "button returned of (display dialog \"OpenATC Plugin Installer\" buttons {\"Cancel\", \"Remove\", \"Install / Update\"} default button \"Install / Update\" cancel button \"Cancel\")"]).output()?;
        if !choice.status.success() {
            return Err("Installation cancelled.".into());
        }
        remove = String::from_utf8(choice.stdout)?.trim() == "Remove";
    }
    let path = if let Some(path) = supplied {
        if recorded {
            PathBuf::from(text(Path::new(path)).ok_or("Missing installation location record; reinstall the installer before removing the plugin.")?.trim())
        } else {
            PathBuf::from(path)
        }
    } else {
        #[cfg(target_os = "macos")]
        {
            let output=Command::new("osascript").args(["-e","POSIX path of (choose folder with prompt \"Select your X-Plane 12 root folder\")"]).output()?;
            if !output.status.success() {
                return Err("Installation cancelled.".into());
            }
            PathBuf::from(String::from_utf8(output.stdout)?.trim())
        }
        #[cfg(not(target_os = "macos"))]
        {
            select(&discover(&home(), &steam_roots()))?
        }
    };
    let path = validate(&path)?;
    if args.first().is_some_and(|s| s == "--check") {
        closed()?;
        return Ok(());
    }
    if !args.iter().any(|s| s == "--yes") {
        #[cfg(not(target_os = "macos"))]
        {
            print!(
                "{} OpenATC in {}? [y/N] ",
                if remove { "Remove" } else { "Install/update" },
                path.display()
            );
            std::io::stdout().flush()?;
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            if !line.trim().eq_ignore_ascii_case("y") {
                return Err("Installation cancelled.".into());
            }
        }
    }
    let record = (!remove)
        .then(|| args.windows(2).find(|p| p[0] == "--record"))
        .flatten()
        .map(|p| PathBuf::from(&p[1]));
    let previous = if let Some(record) = &record {
        let previous = match fs::read(record) {
            Ok(value) => Some(value),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        if let Some(value) = &previous {
            let old = PathBuf::from(String::from_utf8(value.clone())?.trim());
            if old != path {
                return Err("This installer already manages another X-Plane installation. Remove that managed installation first, or use the portable ZIP for additional copies.".into());
            }
        }
        fs::write(record, path.to_string_lossy().as_bytes())?;
        previous
    } else {
        None
    };
    if let Err(error) = change(
        &path,
        (!remove).then(|| bundle.join("payload/OpenATC")).as_deref(),
    ) {
        if let Some(record) = record {
            match previous {
                Some(value) => fs::write(record, value)?,
                None => fs::remove_file(record)?,
            }
        }
        return Err(error);
    }
    #[cfg(target_os = "macos")]
    crate::release::installer_message(if remove {
        "OpenATC was removed. Personal settings were preserved."
    } else {
        "OpenATC is installed. You can now start X-Plane."
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn idle() -> Result {
        Ok(())
    }
    fn simulator(root: &Path) {
        fs::create_dir_all(root.join("Resources/plugins")).unwrap();
        let executable = root.join(executable_name());
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        let mut header = vec![0u8; 128];
        if cfg!(windows) {
            header[..2].copy_from_slice(b"MZ");
            header[60] = 64;
            header[64..68].copy_from_slice(b"PE\0\0");
            header[68..70].copy_from_slice(&0x8664u16.to_le_bytes());
        } else if cfg!(target_os = "macos") {
            header[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
            let cpu = if cfg!(target_arch = "aarch64") {
                0x0100000cu32
            } else {
                0x01000007u32
            };
            header[4..8].copy_from_slice(&cpu.to_le_bytes());
        } else {
            header[..4].copy_from_slice(b"\x7fELF");
            header[4] = 2;
            header[5] = 1;
            header[18] = 62;
        }
        fs::write(executable, header).unwrap();
        fs::write(root.join("Log.txt"), "Log.txt for X-Plane 12.4.3\n").unwrap();
    }
    fn put(root: &Path, name: &str, value: &str) {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, value).unwrap();
    }
    #[test]
    fn steam_paths_preserve_literal_backslashes_and_incomplete_payload_is_refused() {
        assert_eq!(
            tokens(r#""path" "C:\Flight Sims\Steam""#),
            vec!["path", r"C:\Flight Sims\Steam"]
        );
        assert_eq!(
            tokens(r#""path" "C:\\Flight Sims\\Steam""#),
            vec!["path", r"C:\Flight Sims\Steam"]
        );
        let temp = tempfile::tempdir().unwrap();
        let sim = temp.path().join("sim");
        simulator(&sim);
        let source = temp.path().join("partial");
        put(&source, "README.md", "incomplete extraction");
        let target = sim.join("Resources/plugins/OpenATC");
        put(&target, "old", "keep");
        assert!(change_with(&sim, Some(&source), idle).is_err());
        assert_eq!(fs::read_to_string(target.join("old")).unwrap(), "keep");
        assert!(!sim.join("Resources/plugins/.openatc-install.lock").exists());
    }
    #[test]
    fn discovers_sandboxed_and_external_libraries_without_default_path_assumptions() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let external = temp.path().join("External flight library café");
        let steam = home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam");
        let sim = external.join("steamapps/common/Custom simulator name");
        simulator(&sim);
        put(
            &steam,
            "steamapps/libraryfolders.vdf",
            &format!(
                "\"libraryfolders\" {{ \"1\" {{ \"path\" \"{}\" }} }}",
                external.to_string_lossy().replace('\\', "\\\\")
            ),
        );
        put(
            &external,
            "steamapps/appmanifest_123.acf",
            "\"AppState\" { \"name\" \"X-Plane 12\" \"installdir\" \"Custom simulator name\" }",
        );
        let direct = home.join("Standalone simulator");
        simulator(&direct);
        put(
            &home,
            ".x-plane/x-plane_install_12.txt",
            &format!("{}\n{}\n", direct.display(), direct.display()),
        );
        let found = discover(&home, &[]);
        assert_eq!(found.len(), 2);
        assert!(found.contains(&fs::canonicalize(sim).unwrap()));
        assert!(found.contains(&fs::canonicalize(direct).unwrap()));
    }
    #[test]
    fn windows_folder_picker_paths_keep_drive_and_network_roots_usable() {
        assert_eq!(
            windows_folder_path(r"\\?\C:\Flight Sims\X-Plane 12"),
            r"C:\Flight Sims\X-Plane 12"
        );
        assert_eq!(
            windows_folder_path(r"\\?\UNC\server\Flight Sims\X-Plane 12"),
            r"\\server\Flight Sims\X-Plane 12"
        );
        assert_eq!(
            windows_folder_path(r"C:\Flight Sims\X-Plane 12"),
            r"C:\Flight Sims\X-Plane 12"
        );
        assert_eq!(
            windows_folder_path(r"\\server\Flight Sims"),
            r"\\server\Flight Sims"
        );
        assert_eq!(
            windows_folder_path(r"\\?\Volume{abc}\sim"),
            r"\\?\Volume{abc}\sim"
        );
    }
    #[test]
    fn fresh_steam_install_uses_matching_app_identity_without_launching_simulator() {
        let temp = tempfile::tempdir().unwrap();
        let steam = temp.path().join("Steam");
        let sim = steam.join("steamapps/common/X-Plane 12");
        simulator(&sim);
        fs::remove_file(sim.join("Log.txt")).unwrap();
        assert!(validate(&sim).is_err());
        put(
            &steam,
            "steamapps/appmanifest_2014780.acf",
            "\"AppState\" { \"appid\" \"2014780\" \"installdir\" \"X-Plane 12\" }",
        );
        assert_eq!(
            discover(temp.path(), std::slice::from_ref(&steam)),
            vec![fs::canonicalize(&sim).unwrap()]
        );
        let other = steam.join("steamapps/common/Other simulator");
        simulator(&other);
        fs::remove_file(other.join("Log.txt")).unwrap();
        assert!(validate(&other).is_err());
        put(&sim, "Log.txt", "Log.txt for X-Plane 11.55\n");
        assert!(validate(&sim).is_err());
        fs::remove_file(sim.join("Log.txt")).unwrap();
        put(
            &steam,
            "steamapps/appmanifest_2014780.acf",
            "\"AppState\" { \"appid\" \"269950\" \"installdir\" \"X-Plane 12\" }",
        );
        assert!(validate(&sim).is_err());
    }
    #[test]
    fn upgrades_preserve_custom_files_remove_obsolete_owned_files_and_uninstall_safely() {
        let temp = tempfile::tempdir().unwrap();
        let sim = temp.path().join("sim");
        simulator(&sim);
        let source = temp.path().join("payload");
        for required in payload_binaries() {
            put(&source, required.to_str().unwrap(), "native fixture");
        }
        put(&source, "bin/engine", "v1");
        put(&source, "prompts/role.toml", "default1");
        put(&source, "obsolete.txt", "old");
        change_with(&sim, Some(&source), idle).unwrap();
        let target = sim.join("Resources/plugins/OpenATC");
        put(&target, "prompts/role.toml", "personal wording");
        put(&target, "CSL/user-model.txt", "my model");
        put(&source, "bin/engine", "v2");
        put(&source, "prompts/role.toml", "default2");
        fs::remove_file(source.join("obsolete.txt")).unwrap();
        change_with(&sim, Some(&source), idle).unwrap();
        assert_eq!(fs::read_to_string(target.join("bin/engine")).unwrap(), "v2");
        assert_eq!(
            fs::read_to_string(target.join("prompts/role.toml")).unwrap(),
            "personal wording"
        );
        assert!(!target.join("obsolete.txt").exists());
        assert_eq!(
            fs::read_to_string(target.join("CSL/user-model.txt")).unwrap(),
            "my model"
        );
        let backups = sim.join("Resources/OpenATC Backups");
        assert!(fs::read_dir(&backups).unwrap().next().is_some());
        change_with(&sim, None, idle).unwrap();
        assert!(!target.join("bin/engine").exists());
        assert!(!target.join(MANIFEST).exists());
        assert!(target.join("prompts/role.toml").exists());
        assert!(target.join("CSL/user-model.txt").exists());
        assert!(
            fs::read_dir(sim.join("Resources/plugins"))
                .unwrap()
                .all(|e| !e.unwrap().file_name().to_string_lossy().contains("stage"))
        );
    }
    #[test]
    fn failed_replacement_restores_original_and_manifest_paths_cannot_escape() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("OpenATC");
        put(&target, "original.txt", "keep");
        assert!(
            replace(
                &temp.path().join("missing-stage"),
                &target,
                &temp.path().join("backups"),
                idle
            )
            .is_err()
        );
        assert_eq!(
            fs::read_to_string(target.join("original.txt")).unwrap(),
            "keep"
        );
        put(
            &target,
            MANIFEST,
            &serde_json::json!({"version":1,"files":{"../escape":"a".repeat(64)}}).to_string(),
        );
        assert!(manifest(&target).is_err());
    }
    #[test]
    fn wrong_simulator_and_live_process_checks_stop_before_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let sim = temp.path().join("sim");
        simulator(&sim);
        put(&sim, "Log.txt", "Log.txt for X-Plane 11.55\n");
        assert!(validate(&sim).is_err());
        put(&sim, "Log.txt", "Log.txt for X-Plane 12.4.3\n");
        let source = temp.path().join("payload");
        put(&source, "bin/engine", "new");
        fn busy() -> Result {
            Err("Close X-Plane".into())
        }
        assert!(change_with(&sim, Some(&source), busy).is_err());
        assert!(!sim.join("Resources/plugins/OpenATC").exists());
    }
    #[cfg(unix)]
    #[test]
    fn symlinked_payload_is_rejected_without_touching_existing_installation() {
        let temp = tempfile::tempdir().unwrap();
        let sim = temp.path().join("sim");
        simulator(&sim);
        let source = temp.path().join("payload");
        for required in payload_binaries() {
            put(&source, required.to_str().unwrap(), "native fixture");
        }
        let external = temp.path().join("external");
        fs::write(&external, "private").unwrap();
        std::os::unix::fs::symlink(&external, source.join("link")).unwrap();
        let target = sim.join("Resources/plugins/OpenATC");
        put(&target, "old", "keep");
        assert!(change_with(&sim, Some(&source), idle).is_err());
        assert_eq!(fs::read_to_string(target.join("old")).unwrap(), "keep");
        assert_eq!(fs::read_to_string(external).unwrap(), "private");
    }
}
