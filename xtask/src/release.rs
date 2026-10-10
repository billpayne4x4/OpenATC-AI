//! Native release packages and the small launcher shipped with their payloads.
use crate::{Result, distribution, repo, run};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn write(path: &Path, text: &str) -> Result {
    fs::create_dir_all(path.parent().ok_or("Missing parent")?)?;
    fs::write(path, text)?;
    Ok(())
}
fn executable(path: &Path) -> Result {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Fetch fixed upstream releases and verify their bytes before execution.
pub fn fetch_tools(destination: &Path) -> Result {
    use sha2::{Digest, Sha256};
    fs::create_dir_all(destination)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;
    for (name, url, expected) in [
        (
            "appimagetool",
            "https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage",
            "ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0",
        ),
        (
            "runtime-x86_64",
            "https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64",
            "2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d",
        ),
    ] {
        let bytes = client.get(url).send()?.error_for_status()?.bytes()?;
        if format!("{:x}", Sha256::digest(&bytes)) != expected {
            return Err(format!("Checksum mismatch for {name}").into());
        }
        let file = destination.join(name);
        fs::write(&file, &bytes)?;
        executable(&file)?;
    }
    Ok(())
}
fn version(value: &str) -> Result<String> {
    let value = value.strip_prefix('v').unwrap_or(value);
    let (base, suffix) = value.split_once('-').unwrap_or((value, ""));
    let pieces: Vec<_> = base.split('.').collect();
    if pieces.len() != 3
        || pieces
            .iter()
            .any(|p| p.is_empty() || p.parse::<u32>().is_err())
        || suffix
            .chars()
            .any(|c| !c.is_ascii_alphanumeric() && c != '.' && c != '-')
        || value.ends_with('-')
    {
        return Err("Version must be MAJOR.MINOR.PATCH, optionally followed by -alpha.1 or another prerelease".into());
    }
    Ok(value.to_owned())
}

/// Entry point when this tooling binary is distributed as the installer/launcher.
pub fn launch() -> Result {
    let exe = std::env::current_exe()?;
    let root = exe.parent().ok_or("Cannot locate bundled files")?;
    let product = fs::read_to_string(root.join("product.txt"))?;
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|a| a == "--help") {
        println!(
            "OpenATC {}\nPlugin: supply the X-Plane directory, or choose it in the folder dialog.\nAI server: arguments are forwarded to openatc-ai (for example --host 0.0.0.0 --port 8099).",
            product.trim()
        );
        return Ok(());
    }
    if product.trim() == "plugin" {
        let sim = if let Some(path) = args.first() {
            PathBuf::from(path)
        } else {
            #[cfg(target_os = "macos")]
            let result = Command::new("osascript")
                .args([
                    "-e",
                    "POSIX path of (choose folder with prompt \"Select your X-Plane 12 folder\")",
                ])
                .output()?;
            #[cfg(windows)]
            let result = Command::new("powershell.exe").args(["-NoProfile", "-STA", "-Command", "Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.FolderBrowserDialog; $d.Description = 'Select your X-Plane 12 folder'; if ($d.ShowDialog() -eq 'OK') { Write-Output $d.SelectedPath } else { exit 1 }"]).output()?;
            #[cfg(all(not(target_os = "macos"), not(windows)))]
            let result = {
                let mut picker = if std::env::var_os("FLATPAK_ID").is_some() {
                    let mut host = Command::new("flatpak-spawn");
                    host.args(["--host", "zenity"]);
                    host
                } else {
                    Command::new("zenity")
                };
                picker.args(["--file-selection", "--directory", "--title=Select your X-Plane 12 folder"]).output().map_err(|_| "Supply the X-Plane directory as an argument, or install zenity for the folder picker")?
            };
            if !result.status.success() {
                return Err("Installation cancelled".into());
            }
            PathBuf::from(String::from_utf8(result.stdout)?.trim())
        };
        if !sim.join("Resources/plugins").is_dir() {
            return Err("Select the X-Plane root folder, containing Resources/plugins".into());
        }
        let target = sim.join("Resources/plugins/OpenATC");
        if target.exists() {
            let backup = openatc_platform::data_dir().join("openatc-ai/backups");
            fs::create_dir_all(&backup)?;
            let saved = tempfile::Builder::new()
                .prefix("OpenATC-")
                .tempdir_in(backup)?
                .keep();
            distribution::tree(&target, &saved)?;
            println!("Previous plugin backed up to {}", saved.display());
        }
        let source = root.join("payload/OpenATC");
        // Keep existing station overrides; personal settings live outside the plugin.
        let overrides = fs::read(target.join("radio-stations.toml")).ok();
        distribution::tree(&source, &target)?;
        if let Some(bytes) = overrides {
            fs::write(target.join("radio-stations.toml"), bytes)?;
        }
        println!("Installed {}. Restart X-Plane.", target.display());
        Ok(())
    } else if product.trim() == "ai-server" {
        let bin = root.join("bin");
        let mut command =
            Command::new(bin.join(format!("openatc-ai{}", std::env::consts::EXE_SUFFIX)));
        command.args(args).current_dir(&bin);
        #[cfg(target_os = "linux")]
        {
            let mut paths = vec![bin.clone()];
            paths.extend(std::env::split_paths(
                &std::env::var_os("LD_LIBRARY_PATH").unwrap_or_default(),
            ));
            command.env("LD_LIBRARY_PATH", std::env::join_paths(paths)?);
        }
        run(&mut command)
    } else {
        Err("Unknown bundled product".into())
    }
}

pub fn package(product: &str, platform: &str, requested_version: &str) -> Result {
    if !["plugin", "ai-server"].contains(&product) {
        return Err("Unknown product".into());
    }
    if !["linux-x64", "windows-x64", "macos-intel", "macos-arm64"].contains(&platform) {
        return Err("Unknown platform".into());
    }
    let version = version(requested_version)?;
    let output = repo().join("packages");
    fs::create_dir_all(&output)?;
    let work = tempfile::tempdir()?;
    let bundle = work.path().join("bundle");
    fs::create_dir_all(&bundle)?;
    let release = repo().join("target/release");
    if product == "plugin" {
        let sim = work.path().join("sim");
        distribution::install_plugin(&sim, false)?;
        distribution::tree(
            &sim.join("Resources/plugins/OpenATC"),
            &bundle.join("payload/OpenATC"),
        )?;
    } else {
        distribution::install_ai(&bundle)?;
        #[cfg(target_os = "linux")]
        bundle_linux_libraries(&bundle.join("bin"))?;
        fs::copy(repo().join("models.toml"), bundle.join("models.toml"))?;
    }
    let launcher = if product == "plugin" {
        "openatc-install"
    } else {
        "openatc-launcher"
    };
    let executable_name = format!("{launcher}{}", std::env::consts::EXE_SUFFIX);
    fs::copy(
        release.join(format!("xtask{}", std::env::consts::EXE_SUFFIX)),
        bundle.join(&executable_name),
    )?;
    executable(&bundle.join(&executable_name))?;
    write(&bundle.join("product.txt"), product)?;
    if product == "ai-server" {
        for name in ["openatc-ai", "openatc-stt"] {
            run(Command::new(
                bundle
                    .join("bin")
                    .join(format!("{name}{}", std::env::consts::EXE_SUFFIX)),
            )
            .arg("--help")
            .current_dir(bundle.join("bin")))?;
        }
    }
    for name in ["README.md", "LICENSE", "Cargo.lock"] {
        fs::copy(repo().join(name), bundle.join(name))?;
    }
    distribution::tree(&repo().join("assets/licenses"), &bundle.join("licenses"))?;
    write(
        &bundle.join("INSTALL.txt"),
        &format!(
            "OpenATC {product} {version}\n\nClose X-Plane before installing the plugin. Run {launcher} and select your X-Plane root directory, or pass that directory as an argument.\nThe complete plugin folder is also in payload/OpenATC and can be copied to Resources/plugins/OpenATC.\n\nFor the AI server, run openatc-launcher --host 0.0.0.0 --port 8099 to serve your LAN, or omit --host to listen locally. Models download on first startup; internet access and sufficient disk space are required. Set the plugin's AI/STT/TTS base URLs to http://SERVER-IP:8099. Models and personal settings are stored separately and are preserved on upgrades.\n"
        ),
    )?;
    let stem = format!("openatc-{product}-{version}-{platform}");
    zip(&bundle, &output.join(format!("{stem}.zip")))?;
    match platform {
        "linux-x64" => linux(&bundle, &output, &stem, product, &version, launcher)?,
        "windows-x64" => windows(&bundle, &output, &stem, product, &version)?,
        _ => macos(&bundle, &output, &stem, product)?,
    }
    for entry in fs::read_dir(&output)? {
        let path = entry?.path();
        if path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with(&stem))
            && path.extension().is_some_and(|e| e != "sha256")
        {
            use sha2::{Digest, Sha256};
            let hash = Sha256::digest(fs::read(&path)?);
            write(
                &path.with_extension(format!(
                    "{}.sha256",
                    path.extension().unwrap().to_string_lossy()
                )),
                &format!(
                    "{hash:x}  {}\n",
                    path.file_name().unwrap().to_string_lossy()
                ),
            )?;
        }
    }
    Ok(())
}

fn zip(root: &Path, output: &Path) -> Result {
    let mut archive = zip::ZipWriter::new(fs::File::create(output)?);
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        #[cfg(unix)]
        let options = {
            use std::os::unix::fs::PermissionsExt;
            options.unix_permissions(entry.metadata()?.permissions().mode())
        };
        archive.start_file(
            entry
                .path()
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/"),
            options,
        )?;
        std::io::copy(&mut fs::File::open(entry.path())?, &mut archive)?;
    }
    archive.finish()?;
    Ok(())
}

fn windows(bundle: &Path, output: &Path, stem: &str, product: &str, version: &str) -> Result {
    let script = bundle.parent().unwrap().join("installer.iss");
    let destination = if product == "plugin" {
        "{autopf32}\\Steam\\steamapps\\common\\X-Plane 12\\Resources\\plugins\\OpenATC"
    } else {
        "{autopf}\\OpenATC AI"
    };
    let source = if product == "plugin" {
        bundle.join("payload/OpenATC")
    } else {
        bundle.to_owned()
    };
    write(
        &script,
        &format!(
            "[Setup]\nAppId=OpenATC-{product}\nAppName=OpenATC {product}\nAppVersion={version}\nDefaultDirName={destination}\nDisableProgramGroupPage=yes\nPrivilegesRequired=admin\nArchitecturesAllowed=x64compatible\nArchitecturesInstallIn64BitMode=x64compatible\nOutputDir={}\nOutputBaseFilename={stem}-setup\nCompression=lzma2\nSolidCompression=yes\nUninstallFilesDir={{localappdata}}\\OpenATC\\uninstall-{product}\n[Files]\nSource: \"{}\\*\"; DestDir: \"{{app}}\"; Flags: recursesubdirs createallsubdirs ignoreversion\n",
            output.display(),
            source.display()
        ),
    )?;
    let compiler = std::env::var_os("ISCC")
        .unwrap_or_else(|| "C:\\Program Files (x86)\\Inno Setup 6\\ISCC.exe".into());
    run(Command::new(compiler).arg(script))
}

#[cfg(target_os = "linux")]
fn bundle_linux_libraries(bin: &Path) -> Result {
    for name in ["openatc-ai", "openatc-stt"] {
        let result = Command::new("ldd").arg(bin.join(name)).output()?;
        if !result.status.success() {
            return Err("Cannot inspect AI runtime dependencies".into());
        }
        for line in String::from_utf8_lossy(&result.stdout).lines() {
            if line.contains("not found") {
                return Err(format!("Unresolved AI dependency: {line}").into());
            }
            let Some((library, tail)) = line.trim().split_once(" => ") else {
                continue;
            };
            if [
                "libc.so",
                "libm.so",
                "libpthread.so",
                "libdl.so",
                "librt.so",
                "libresolv.so",
                "libutil.so",
            ]
            .iter()
            .any(|base| library.starts_with(base))
            {
                continue;
            }
            if let Some(source) = tail
                .split_whitespace()
                .next()
                .filter(|p| p.starts_with('/'))
            {
                let destination = bin.join(library);
                if !destination.exists()
                    || fs::canonicalize(source)? != fs::canonicalize(&destination)?
                {
                    fs::copy(source, destination)?;
                }
            }
        }
    }
    let licenses = bin.parent().unwrap().join("licenses/system-runtime");
    for folder in [
        "/usr/share/licenses/gcc",
        "/usr/share/doc/libstdc++6",
        "/usr/share/doc/libgcc-s1",
        "/usr/share/doc/libgomp1",
    ] {
        let source = Path::new(folder);
        if source.is_dir() {
            distribution::tree(source, &licenses.join(source.file_name().unwrap()))?;
        }
    }
    Ok(())
}

fn macos(bundle: &Path, output: &Path, stem: &str, product: &str) -> Result {
    let image = bundle.parent().unwrap().join("disk-image");
    fs::create_dir_all(&image)?;
    if product == "plugin" {
        distribution::tree(&bundle.join("payload/OpenATC"), &image.join("OpenATC"))?;
        fs::copy(bundle.join("INSTALL.txt"), image.join("INSTALL.txt"))?;
    } else {
        let contents = image.join("OpenATC AI.app/Contents");
        distribution::tree(bundle, &contents.join("MacOS"))?;
        write(
            &contents.join("Info.plist"),
            "<?xml version=\"1.0\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>org.openatc.ai</string><key>CFBundleName</key><string>OpenATC AI</string><key>CFBundleExecutable</key><string>openatc-launcher</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>",
        )?;
    }
    run(Command::new("hdiutil")
        .args([
            "create", "-fs", "HFS+", "-format", "UDZO", "-volname", "OpenATC",
        ])
        .arg("-srcfolder")
        .arg(image)
        .arg(output.join(format!("{stem}.dmg"))))
}

fn linux(
    bundle: &Path,
    output: &Path,
    stem: &str,
    product: &str,
    version: &str,
    launcher: &str,
) -> Result {
    let work = bundle.parent().unwrap();
    let package_root = work.join("linux-root");
    let name = format!("openatc-{product}");
    let destination = package_root.join(format!("usr/lib/{name}"));
    distribution::tree(bundle, &destination)?;
    let id = if product == "plugin" {
        "org.openatc.Plugin"
    } else {
        "org.openatc.AI"
    };
    let desktop = format!(
        "[Desktop Entry]\nType=Application\nName=OpenATC {product}\nExec=/usr/lib/{name}/{launcher}\nIcon={id}\nTerminal=true\nCategories=Game;Simulation;\n"
    );
    write(
        &package_root.join(format!("usr/share/applications/{id}.desktop")),
        &desktop,
    )?;
    if product == "ai-server" {
        write(
            &package_root.join("usr/lib/systemd/user/openatc-ai.service"),
            "[Unit]\nDescription=OpenATC AI server\nAfter=network-online.target\n[Service]\nExecStart=/usr/lib/openatc-ai-server/openatc-launcher\nRestart=on-failure\nRestartSec=5\n[Install]\nWantedBy=default.target\n",
        )?;
    }
    let icon = package_root.join(format!("usr/share/icons/hicolor/scalable/apps/{id}.svg"));
    fs::create_dir_all(icon.parent().unwrap())?;
    fs::copy(repo().join("assets/logo.svg"), &icon)?;
    deb(&package_root, work, output, stem, &name, version)?;
    rpm(&package_root, work, output, stem, &name, version)?;
    appimage(&package_root, work, output, stem, &name, launcher, id)?;
    flatpak(bundle, work, output, stem, product, launcher, id)?;
    Ok(())
}

fn deb(root: &Path, work: &Path, output: &Path, stem: &str, name: &str, version: &str) -> Result {
    let package = work.join("deb");
    distribution::tree(root, &package)?;
    write(
        &package.join("DEBIAN/control"),
        &format!(
            "Package: {name}\nVersion: {}\nArchitecture: amd64\nMaintainer: OpenATC contributors <noreply@openatc.invalid>\nSection: games\nPriority: optional\nDepends: libc6 (>= 2.35), libstdc++6, libgcc-s1\nRecommends: zenity\nDescription: OpenATC flight simulation {name}\n Free and open-source X-Plane ATC software.\n",
            version.replace('-', "~")
        ),
    )?;
    run(Command::new("dpkg-deb")
        .args(["--build", "--root-owner-group"])
        .arg(package)
        .arg(output.join(format!("{stem}.deb"))))
}

fn rpm(root: &Path, work: &Path, output: &Path, stem: &str, name: &str, version: &str) -> Result {
    let top = work.join("rpm");
    let base = version.split('-').next().unwrap();
    let spec = top.join("SPECS/openatc.spec");
    write(
        &spec,
        &format!(
            "Name: {name}\nVersion: {base}\nRelease: {}\nSummary: OpenATC flight simulation software\nLicense: MIT\nBuildArch: x86_64\nAutoReqProv: yes\n%global debug_package %{{nil}}\n%global __os_install_post %{{nil}}\n%description\nFree and open-source X-Plane ATC software.\n%prep\n%build\n%install\nmkdir -p %{{buildroot}}\ncp -a \"{}\"/. %{{buildroot}}/\n%files\n/usr/lib/{name}\n/usr/share/applications/*\n/usr/share/icons/hicolor/scalable/apps/*\n",
            if version.contains('-') {
                format!("0.{}", version.split_once('-').unwrap().1.replace('-', "."))
            } else {
                "1".into()
            },
            root.display()
        ),
    )?;
    if name == "openatc-ai-server" {
        let text = fs::read_to_string(&spec)? + "/usr/lib/systemd/user/openatc-ai.service\n";
        write(&spec, &text)?;
    }
    run(Command::new("rpmbuild")
        .args(["-bb", "--define"])
        .arg(format!("_topdir {}", top.display()))
        .arg(spec))?;
    let rpm = walkdir::WalkDir::new(top.join("RPMS"))
        .into_iter()
        .filter_map(std::result::Result::ok)
        .find(|e| e.path().extension().is_some_and(|s| s == "rpm"))
        .ok_or("RPM not produced")?;
    fs::copy(rpm.path(), output.join(format!("{stem}.rpm")))?;
    Ok(())
}

fn appimage(
    root: &Path,
    work: &Path,
    output: &Path,
    stem: &str,
    name: &str,
    launcher: &str,
    id: &str,
) -> Result {
    let app = work.join("OpenATC.AppDir");
    distribution::tree(root, &app)?;
    fs::copy(
        app.join(format!("usr/share/applications/{id}.desktop")),
        app.join(format!("{id}.desktop")),
    )?;
    let desktop_path = app.join(format!("{id}.desktop"));
    let desktop = fs::read_to_string(&desktop_path)?
        .replace(&format!("/usr/lib/{name}/{launcher}"), launcher);
    write(&desktop_path, &desktop)?;
    fs::copy(
        app.join(format!("usr/share/icons/hicolor/scalable/apps/{id}.svg")),
        app.join(format!("{id}.svg")),
    )?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(format!("usr/lib/{name}/{launcher}"), app.join("AppRun"))?;
    run(
        Command::new(std::env::var_os("APPIMAGETOOL").unwrap_or_else(|| "appimagetool".into()))
            .env("ARCH", "x86_64")
            .env("APPIMAGE_EXTRACT_AND_RUN", "1")
            .arg("--runtime-file")
            .arg(
                std::env::var_os("APPIMAGE_RUNTIME")
                    .ok_or("Set APPIMAGE_RUNTIME to the pinned type2 runtime")?,
            )
            .arg(app)
            .arg(output.join(format!("{stem}.AppImage"))),
    )
}

fn flatpak(
    bundle: &Path,
    work: &Path,
    output: &Path,
    stem: &str,
    product: &str,
    launcher: &str,
    id: &str,
) -> Result {
    let build = work.join("flatpak-build");
    run(Command::new("flatpak")
        .args(["build-init"])
        .arg(&build)
        .args([
            id,
            "org.freedesktop.Sdk",
            "org.freedesktop.Platform",
            "25.08",
        ]))?;
    distribution::tree(bundle, &build.join("files/openatc"))?;
    let desktop = format!(
        "[Desktop Entry]\nType=Application\nName=OpenATC {product}\nExec=/app/openatc/{launcher}\nIcon={id}\nTerminal=true\nCategories=Game;Simulation;\n"
    );
    write(
        &build.join(format!("files/share/applications/{id}.desktop")),
        &desktop,
    )?;
    let icon = build.join(format!("files/share/icons/hicolor/scalable/apps/{id}.svg"));
    fs::create_dir_all(icon.parent().unwrap())?;
    fs::copy(repo().join("assets/logo.svg"), icon)?;
    let mut finish = Command::new("flatpak");
    finish
        .args(["build-finish", "--share=network"])
        .arg(format!("--command=/app/openatc/{launcher}"));
    if product == "plugin" {
        finish.args([
            "--filesystem=host",
            "--talk-name=org.freedesktop.Flatpak",
            "--socket=x11",
            "--socket=wayland",
            "--share=ipc",
        ]);
    }
    finish.arg(&build);
    run(&mut finish)?;
    let repository = work.join("flatpak-repo");
    run(Command::new("flatpak")
        .args(["build-export"])
        .arg(&repository)
        .arg(build))?;
    run(Command::new("flatpak")
        .args(["build-bundle"])
        .arg(repository)
        .arg(output.join(format!("{stem}.flatpak")))
        .arg(id)
        .arg("--runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn release_versions_reject_paths_and_shell_text() {
        assert_eq!(version("v0.0.1-alpha.1").unwrap(), "0.0.1-alpha.1");
        for invalid in ["../bad", "1.2", "1.2.3;rm", "1.2.3-", "1.2.3-evil/thing"] {
            assert!(version(invalid).is_err());
        }
    }
}
