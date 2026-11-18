//! Native release packages and the small launcher shipped with their payloads.
use crate::{Result, distribution, repo, run};
use std::{fs, path::Path, process::Command};

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

fn welcome(product: &str, version: &str) -> String {
    let (name, intro, notice, start) = if product == "plugin" {
        (
            "X-Plane 12 Plugin",
            "Install OpenATC AI into your chosen X-Plane 12 installation.",
            "Close X-Plane before installing or updating the plugin.",
            "Run the included installer and select the X-Plane root folder. For manual installation, use the complete OpenATC folder supplied in the ZIP download.",
        )
    } else {
        (
            "AI Server",
            "Run speech and language services on this computer or a separate machine on your network.",
            "First startup downloads the selected models. Allow sufficient disk space and keep your internet connection available.",
            "Launch the included OpenATC server launcher. Keep personal configuration and downloaded models when upgrading. Follow the installation guide to connect the plugin to this server.",
        )
    };
    include_str!("../../packaging/installer/welcome.html")
        .replace("@PRODUCT@", name)
        .replace("@VERSION@", version)
        .replace("@INTRO@", intro)
        .replace("@NOTICE@", notice)
        .replace("@START@", start)
}

fn branding(bundle: &Path, product: &str, version: &str) -> Result {
    check_branding()?;
    distribution::tree(&repo().join("assets/installer"), &bundle.join("branding"))?;
    write(&bundle.join("Welcome.html"), &welcome(product, version))
}

fn check_branding() -> Result {
    let root = repo().join("assets/installer");
    for (name, width, height) in [
        ("wizard.bmp", 164u32, 314u32),
        ("wizard-2x.bmp", 328, 628),
        ("wizard-small.bmp", 55, 55),
        ("wizard-small-2x.bmp", 110, 110),
    ] {
        let bytes = fs::read(root.join(name))?;
        if bytes.len() < 54 || &bytes[..2] != b"BM" {
            return Err(format!("Invalid installer bitmap: {name}").into());
        }
        let number = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        if number(18) != width
            || number(22) != height
            || u16::from_le_bytes(bytes[28..30].try_into().unwrap()) != 24
            || number(30) != 0
            || number(2) as usize != bytes.len()
        {
            return Err(
                format!("Installer bitmap has unexpected dimensions or encoding: {name}").into(),
            );
        }
    }
    for name in ["banner.png", "sidebar.png", "icon.png", "flatpak-icon.png"] {
        let bytes = fs::read(root.join(name))?;
        if bytes.len() < 33 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
            return Err(format!("Invalid installer PNG: {name}").into());
        }
        let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
        if width == 0
            || height == 0
            || (name == "icon.png" && (width != 1024 || height != 1024))
            || (name == "flatpak-icon.png" && (width != 128 || height != 128))
        {
            return Err(format!("Installer PNG has unexpected dimensions: {name}").into());
        }
    }
    let icon = fs::read(root.join("setup.ico"))?;
    if icon.len() < 102 || &icon[..6] != b"\0\0\x01\0\x06\0" {
        return Err("Installer icon must contain six Windows icon sizes".into());
    }
    for index in 0..6 {
        let entry = 6 + index * 16;
        let length = u32::from_le_bytes(icon[entry + 8..entry + 12].try_into().unwrap()) as usize;
        let offset = u32::from_le_bytes(icon[entry + 12..entry + 16].try_into().unwrap()) as usize;
        if length == 0
            || offset < 102
            || offset
                .checked_add(length)
                .is_none_or(|end| end > icon.len())
        {
            return Err("Installer icon contains an invalid image range".into());
        }
    }
    Ok(())
}

/// Exercise the native packaging tools with tiny payloads before building model runtimes.
fn native_platform() -> &'static str {
    if cfg!(windows) {
        "windows-x64"
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "macos-arm64"
        } else {
            "macos-intel"
        }
    } else {
        "linux-x64"
    }
}

pub fn preflight(platform: &str) -> Result {
    if platform != native_platform() {
        return Err(
            "Packaging preflight must run on the selected native platform and architecture".into(),
        );
    }
    check_branding()?;
    let work = tempfile::tempdir()?;
    for product in ["plugin", "ai-server"] {
        let root = work.path().join(product);
        let bundle = root.join("bundle");
        let output = root.join("packages");
        fs::create_dir_all(&output)?;
        branding(&bundle, product, "0.0.0-preflight")?;
        for name in ["INSTALL.txt", "INSTALL.md", "README.md", "LICENSE"] {
            write(
                &bundle.join(name),
                "Packaging preflight fixture; not a runnable release.",
            )?;
        }
        fs::create_dir_all(bundle.join("licenses"))?;
        if cfg!(target_os = "windows") {
            crate::windows_runtime::install(&bundle)?;
        }
        let launcher = if product == "plugin" {
            "openatc-install"
        } else {
            "openatc-launcher"
        };
        write(
            &bundle.join(format!("{launcher}{}", std::env::consts::EXE_SUFFIX)),
            "Packaging fixture",
        )?;
        executable(&bundle.join(format!("{launcher}{}", std::env::consts::EXE_SUFFIX)))?;
        write(
            &bundle.join("payload/OpenATC/preflight.txt"),
            "Not a plugin binary",
        )?;
        let stem = format!("openatc-{product}-preflight");
        zip(&bundle, &output.join(format!("{stem}.zip")))?;
        match platform {
            "windows-x64" => windows(&bundle, &output, &stem, product, "0.0.0-preflight")?,
            "macos-intel" | "macos-arm64" => {
                macos(&bundle, &output, &stem, product, "0.0.0-preflight")?;
                let app = if product == "plugin" {
                    "OpenATC Plugin Installer"
                } else {
                    "OpenATC AI Server"
                };
                run(Command::new("plutil")
                    .arg("-lint")
                    .arg(root.join(format!("disk-image/{app}.app/Contents/Info.plist"))))?;
                run(Command::new("hdiutil")
                    .arg("verify")
                    .arg(output.join(format!("{stem}.dmg"))))?;
            }
            _ => {}
        }
    }
    println!(
        "Installer branding and {platform} packaging preflight passed; runtime acceptance is separate."
    );
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn installer_message(message: &str) -> Result {
    let exe = std::env::current_exe()?;
    let icon = exe
        .parent()
        .and_then(Path::parent)
        .ok_or("Cannot locate installer resources")?
        .join("Resources/OpenATC.icns");
    let dialog = if icon.is_file() {
        "display dialog (item 1 of argv) with title \"OpenATC AI\" buttons {\"OK\"} default button \"OK\" with icon (POSIX file (item 2 of argv))"
    } else {
        "display dialog (item 1 of argv) with title \"OpenATC AI\" buttons {\"OK\"} default button \"OK\""
    };
    run(Command::new("osascript")
        .args(["-e", "on run argv", "-e", dialog, "-e", "end run", "--"])
        .arg(message)
        .arg(icon))
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
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if exe
        .file_stem()
        .is_some_and(|name| name == "openatc-install")
    {
        return crate::installer::launch(root, &args);
    }
    let product = fs::read_to_string(root.join("product.txt"))?;
    if args.first().is_some_and(|a| a == "--help") {
        println!(
            "OpenATC {}\nPlugin: supply the X-Plane directory, or choose it in the folder dialog.\nAI server: arguments are forwarded to openatc-ai (for example --host 0.0.0.0 --port 8099).",
            product.trim()
        );
        return Ok(());
    }
    if product.trim() == "plugin" {
        crate::installer::launch(root, &args)
    } else if product.trim() == "ai-server" {
        #[cfg(target_os = "macos")]
        {
            use std::io::IsTerminal;
            if !std::io::stdin().is_terminal() && !args.iter().any(|a| a == "--terminal-child") {
                let mode = if args.is_empty() {
                    let choice = Command::new("osascript").args(["-e", "button returned of (display dialog \"OpenATC AI Server\" buttons {\"Cancel\", \"Configure\", \"Start\"} default button \"Start\" cancel button \"Cancel\")"]).output()?;
                    if !choice.status.success() {
                        return Ok(());
                    }
                    (String::from_utf8(choice.stdout)?.trim() == "Configure")
                        .then_some("--configure-web")
                } else {
                    None
                };
                let quote = |value: &str| format!("'{}'", value.replace('\'', "'\"'\"'"));
                let mut shell = format!("{} --terminal-child", quote(&exe.to_string_lossy()));
                for arg in &args {
                    shell.push(' ');
                    shell.push_str(&quote(&arg.to_string_lossy()));
                }
                if let Some(mode) = mode {
                    shell.push(' ');
                    shell.push_str(mode);
                }
                // LaunchServices opens Terminal without an Apple-events automation grant.
                use std::os::unix::fs::PermissionsExt;
                let script = tempfile::Builder::new()
                    .prefix("openatc-server-")
                    .suffix(".command")
                    .tempfile()?;
                fs::write(
                    script.path(),
                    format!("#!/bin/sh\nrm -f -- \"$0\"\nexec {shell}\n"),
                )?;
                fs::set_permissions(script.path(), fs::Permissions::from_mode(0o700))?;
                let (file, path) = script.keep()?;
                drop(file);
                if let Err(error) = run(Command::new("open").args(["-a", "Terminal"]).arg(&path)) {
                    let _ = fs::remove_file(path);
                    return Err(error);
                }
                return Ok(());
            }
        }
        let bin = root.join("bin");
        let mut command =
            Command::new(bin.join(format!("openatc-ai{}", std::env::consts::EXE_SUFFIX)));
        let args: Vec<_> = args
            .into_iter()
            .filter(|a| a != "--terminal-child")
            .collect();
        if args.is_empty() {
            command.arg("--manage-web");
        }
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
    if platform != native_platform() {
        return Err(
            "Release packaging must run on the selected native platform and architecture".into(),
        );
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
        fs::copy(
            repo().join("models-kitten.toml"),
            bundle.join("models-kitten.toml"),
        )?;
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
    if cfg!(target_os = "windows") {
        crate::windows_runtime::install(&bundle)?;
    }
    write(&bundle.join("product.txt"), product)?;
    if product == "ai-server" {
        for name in ["openatc-ai", "openatc-stt", "openatc-tray"] {
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
    distribution::tree(&repo().join("docs"), &bundle.join("docs"))?;
    write(
        &bundle.join("INSTALL.txt"),
        &format!(
            "OpenATC {product} {version}\n\nRead INSTALL.md for platform-specific installation, configuration and removal. On Linux extract the plugin ZIP and run sh ./install.sh (no sudo).\n\nClose X-Plane before installing the plugin. Run {launcher} and select your X-Plane root directory, or pass that directory as an argument.\nThe complete plugin folder is also in payload/OpenATC and can be copied to Resources/plugins/OpenATC.\n\nFor the AI server, run openatc-launcher --host 0.0.0.0 --port 8099 to serve your LAN, or omit --host to listen locally. Models download on first startup; internet access and sufficient disk space are required. Set the plugin's AI/STT/TTS base URLs to http://SERVER-IP:8099. Models and personal settings are stored separately and are preserved on upgrades.\n"
        ),
    )?;
    if product == "plugin" {
        for (name, operation) in [("install.sh", "--install"), ("uninstall.sh", "--uninstall")] {
            write(
                &bundle.join(name),
                &format!(
                    "#!/bin/sh\nset -eu\nbase=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)\nexec \"$base/openatc-install\" {operation} \"$@\"\n"
                ),
            )?;
            executable(&bundle.join(name))?;
        }
    }
    fs::copy(repo().join("docs/INSTALL.md"), bundle.join("INSTALL.md"))?;
    branding(&bundle, product, &version)?;
    sign_payload(&bundle)?;
    let stem = format!("openatc-{product}-{version}-{platform}");
    zip(&bundle, &output.join(format!("{stem}.zip")))?;
    match platform {
        "linux-x64" if product == "plugin" => {}
        "linux-x64" => linux(&bundle, &output, &stem, product, &version, launcher)?,
        "windows-x64" => windows(&bundle, &output, &stem, product, &version)?,
        _ => macos(&bundle, &output, &stem, product, &version)?,
    }
    let extensions: &[&str] = match platform {
        "linux-x64" if product == "plugin" => &["zip"],
        "linux-x64" => &["zip", "deb", "rpm", "AppImage", "flatpak"],
        "windows-x64" => &["zip", "exe"],
        _ => &["zip", "dmg"],
    };
    for extension in extensions {
        let filename = if *extension == "exe" {
            format!("{stem}-setup.exe")
        } else {
            format!("{stem}.{extension}")
        };
        let path = output.join(&filename);
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(fs::read(&path)?);
        write(
            &output.join(format!("{filename}.sha256")),
            &format!("{hash:x}  {filename}\n"),
        )?;
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
    let appearance = include_str!("../../packaging/installer/windows-branding.iss")
        .replace("@BRANDING_DIR@", &bundle.join("branding").to_string_lossy());
    let name = if product == "plugin" {
        "OpenATC Plugin"
    } else {
        "OpenATC AI Server"
    };
    let directory = if product == "plugin" {
        "OpenATC Plugin Installer"
    } else {
        "OpenATC AI Server"
    };
    let mut content = format!(
        "[Setup]\n{appearance}\nAppId=OpenATC-{product}\nAppName={name}\nAppVersion={version}\nDefaultDirName={{localappdata}}\\Programs\\{directory}\nDisableProgramGroupPage=yes\nDisableWelcomePage=no\nDisableFinishedPage=no\nLicenseFile={}\nPrivilegesRequired=lowest\nArchitecturesAllowed=x64compatible\nArchitecturesInstallIn64BitMode=x64compatible\nOutputDir={}\nOutputBaseFilename={stem}-setup\nCompression=lzma2\nSolidCompression=yes\n[Files]\n",
        bundle.join("LICENSE").display(),
        output.display()
    );
    if windows_certificate()?.is_some() {
        let directory = bundle.parent().unwrap().join("signed-uninstallers");
        fs::create_dir_all(&directory)?;
        let signing = format!(
            "SignTool=openatc\nSignedUninstaller=yes\nSignedUninstallerDir={}\n",
            directory.display()
        );
        content = content.replace("[Files]\n", &format!("{signing}[Files]\n"));
    }
    let mut extract_runtime = String::new();
    if product == "plugin" {
        for entry in fs::read_dir(bundle)? {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "dll") {
                content.push_str(&format!(
                    "Source: \"{}\"; Flags: dontcopy\n",
                    path.display()
                ));
                extract_runtime.push_str(&format!(
                    "  ExtractTemporaryFile('{}');\n",
                    path.file_name().unwrap().to_string_lossy()
                ));
            }
        }
        content.push_str(&format!(
            "Source: \"{}\"; Flags: dontcopy\n",
            bundle.join("openatc-install.exe").display()
        ));
    }
    content.push_str(&format!("Source: \"{}\\*\"; DestDir: \"{{app}}\"; Flags: recursesubdirs createallsubdirs ignoreversion\n",bundle.display()));
    if product == "plugin" {
        content.push_str("[UninstallDelete]\nType: files; Name: \"{app}\\xplane-path.txt\"\n");
        content.push_str(
            &include_str!("../../packaging/installer/windows-plugin.iss").replace(
                "  { Extract runtime before starting the discovery helper. }",
                &extract_runtime,
            ),
        );
    } else {
        content.push_str("[Icons]\nName: \"{autoprograms}\\OpenATC AI Server\"; Filename: \"{app}\\openatc-launcher.exe\"\nName: \"{autoprograms}\\Configure OpenATC AI Server\"; Filename: \"{app}\\openatc-launcher.exe\"; Parameters: \"--configure-web\"\n");
    }
    write(&script, &content)?;
    let compiler = std::env::var_os("ISCC")
        .unwrap_or_else(|| "C:\\Program Files (x86)\\Inno Setup 6\\ISCC.exe".into());
    let mut command = Command::new(compiler);
    if let Some(thumbprint) = windows_certificate()? {
        command.arg(format!("/Sopenatc=signtool.exe sign /sha1 {thumbprint} /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 $f"));
    }
    run(command.arg(script))
}

fn windows_certificate() -> Result<Option<String>> {
    let value = std::env::var("OPENATC_WINDOWS_SIGN_CERT_SHA1").ok();
    if value
        .as_ref()
        .is_some_and(|v| v.len() != 40 || !v.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(
            "OPENATC_WINDOWS_SIGN_CERT_SHA1 must contain a 40-character certificate thumbprint"
                .into(),
        );
    }
    Ok(value)
}

// Sign nested native files before archives are assembled. Never sign model data.
fn sign_payload(bundle: &Path) -> Result {
    let windows = if cfg!(windows) {
        windows_certificate()?
    } else {
        None
    };
    let apple = if cfg!(target_os = "macos") {
        std::env::var("OPENATC_MAC_SIGN_IDENTITY").ok()
    } else {
        None
    };
    if windows.is_none() && apple.is_none() {
        return Ok(());
    }
    for entry in walkdir::WalkDir::new(bundle).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if let Some(thumbprint) = &windows
            && path
                .extension()
                .is_some_and(|ext| ["exe", "dll", "xpl"].iter().any(|e| ext == *e))
        {
            run(Command::new("signtool.exe")
                .args([
                    "sign",
                    "/sha1",
                    thumbprint,
                    "/fd",
                    "SHA256",
                    "/tr",
                    "http://timestamp.digicert.com",
                    "/td",
                    "SHA256",
                ])
                .arg(path))?;
            run(Command::new("signtool.exe")
                .args(["verify", "/pa"])
                .arg(path))?;
        }
        if let Some(identity) = &apple {
            use std::io::Read;
            let mut magic = [0u8; 4];
            let n = fs::File::open(path)?.read(&mut magic)?;
            if n == 4
                && matches!(
                    magic,
                    [0xcf, 0xfa, 0xed, 0xfe]
                        | [0xfe, 0xed, 0xfa, 0xcf]
                        | [0xca, 0xfe, 0xba, 0xbe]
                        | [0xca, 0xfe, 0xba, 0xbf]
                )
            {
                run(Command::new("codesign")
                    .args([
                        "--force",
                        "--timestamp",
                        "--options",
                        "runtime",
                        "--sign",
                        identity,
                    ])
                    .arg(path))?;
                run(Command::new("codesign")
                    .args(["--verify", "--strict"])
                    .arg(path))?;
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn bundle_linux_libraries(bin: &Path) -> Result {
    for name in ["openatc-ai", "openatc-stt", "openatc-tray"] {
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
                "libcuda.so",
                "libnvidia",
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

fn macos(bundle: &Path, output: &Path, stem: &str, product: &str, version: &str) -> Result {
    let image = bundle.parent().unwrap().join("disk-image");
    fs::create_dir_all(&image)?;
    let (name, identifier, launcher) = if product == "plugin" {
        (
            "OpenATC Plugin Installer",
            "org.openatc.plugininstaller",
            "openatc-install",
        )
    } else {
        ("OpenATC AI Server", "org.openatc.ai", "openatc-launcher")
    };
    let contents = image.join(format!("{name}.app/Contents"));
    distribution::tree(bundle, &contents.join("MacOS"))?;
    let numeric_version = version.split('-').next().unwrap_or(version);
    write(
        &contents.join("Info.plist"),
        &format!(
            "<?xml version=\"1.0\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>{identifier}</string><key>CFBundleName</key><string>{name}</string><key>CFBundleExecutable</key><string>{launcher}</string><key>CFBundleIconFile</key><string>OpenATC.icns</string><key>CFBundleShortVersionString</key><string>{numeric_version}</string><key>CFBundleVersion</key><string>{numeric_version}</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>"
        ),
    )?;
    let resources = contents.join("Resources");
    fs::create_dir_all(&resources)?;
    let iconset = bundle.parent().unwrap().join("OpenATC.iconset");
    fs::create_dir_all(&iconset)?;
    for size in [16, 32, 128, 256, 512] {
        for density in [1, 2] {
            let suffix = if density == 2 { "@2x" } else { "" };
            let pixels = (size * density).to_string();
            run(Command::new("sips")
                .args(["-z", &pixels, &pixels])
                .arg(bundle.join("branding/icon.png"))
                .arg("--out")
                .arg(iconset.join(format!("icon_{size}x{size}{suffix}.png"))))?;
        }
    }
    run(Command::new("iconutil")
        .args(["-c", "icns"])
        .arg(&iconset)
        .arg("-o")
        .arg(resources.join("OpenATC.icns")))?;
    distribution::tree(&bundle.join("branding"), &image.join("branding"))?;
    for name in [
        "Welcome.html",
        "INSTALL.txt",
        "INSTALL.md",
        "README.md",
        "LICENSE",
    ] {
        fs::copy(bundle.join(name), image.join(name))?;
    }
    distribution::tree(&bundle.join("licenses"), &image.join("licenses"))?;
    #[cfg(unix)]
    if product == "ai-server" {
        std::os::unix::fs::symlink("/Applications", image.join("Applications"))?;
    }
    let app = contents.parent().unwrap();
    if let Ok(identity) = std::env::var("OPENATC_MAC_SIGN_IDENTITY") {
        run(Command::new("codesign")
            .args([
                "--force",
                "--timestamp",
                "--options",
                "runtime",
                "--sign",
                &identity,
            ])
            .arg(app))?;
        run(Command::new("codesign")
            .args(["--verify", "--strict", "--deep"])
            .arg(app))?;
    }
    let dmg = output.join(format!("{stem}.dmg"));
    run(Command::new("hdiutil")
        .args([
            "create", "-fs", "HFS+", "-format", "UDZO", "-volname", "OpenATC",
        ])
        .arg("-srcfolder")
        .arg(image)
        .arg(&dmg))?;
    if let Ok(identity) = std::env::var("OPENATC_MAC_SIGN_IDENTITY") {
        run(Command::new("codesign")
            .args(["--timestamp", "--sign", &identity])
            .arg(&dmg))?;
    }
    if let Ok(profile) = std::env::var("OPENATC_NOTARY_PROFILE") {
        if std::env::var_os("OPENATC_MAC_SIGN_IDENTITY").is_none() {
            return Err("Notarisation requires OPENATC_MAC_SIGN_IDENTITY".into());
        }
        let status = Command::new("xcrun")
            .args(["notarytool", "submit"])
            .arg(&dmg)
            .args([
                "--keychain-profile",
                &profile,
                "--wait",
                "--timeout",
                "30m",
                "--output-format",
                "json",
            ])
            .output()?;
        let response: serde_json::Value = serde_json::from_slice(&status.stdout).map_err(
            |_| "Notarisation did not return valid JSON; inspect the Apple submission log",
        )?;
        if !status.status.success()
            || response.get("status").and_then(|v| v.as_str()) != Some("Accepted")
        {
            return Err(format!("Apple did not accept notarisation: {}. Inspect this submission with notarytool log.", response).into());
        }
        run(Command::new("xcrun").args(["stapler", "staple"]).arg(&dmg))?;
        run(Command::new("xcrun")
            .args(["stapler", "validate"])
            .arg(&dmg))?;
    }
    Ok(())
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
    let libc_version = if name == "openatc-ai-server" {
        "2.39"
    } else {
        "2.35"
    };
    let package = work.join("deb");
    distribution::tree(root, &package)?;
    write(
        &package.join("DEBIAN/control"),
        &format!(
            "Package: {name}\nVersion: {}\nArchitecture: amd64\nMaintainer: OpenATC contributors <noreply@openatc.invalid>\nSection: games\nPriority: optional\nDepends: libc6 (>= {libc_version}), libstdc++6, libgcc-s1\nDescription: OpenATC flight simulation {name}\n Free and open-source X-Plane ATC software.\n",
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
    let libc_requirement = if name == "openatc-ai-server" {
        "Requires: glibc >= 2.39\n"
    } else {
        ""
    };
    let spec = top.join("SPECS/openatc.spec");
    write(
        &spec,
        &format!(
            "Name: {name}\nVersion: {base}\nRelease: {}\nSummary: OpenATC flight simulation software\nLicense: MIT\nBuildArch: x86_64\n{libc_requirement}AutoReqProv: yes\n%global debug_package %{{nil}}\n%global __os_install_post %{{nil}}\n%description\nFree and open-source X-Plane ATC software.\n%prep\n%build\n%install\nmkdir -p %{{buildroot}}\ncp -a \"{}\"/. %{{buildroot}}/\n%files\n/usr/lib/{name}\n/usr/share/applications/*\n/usr/share/icons/hicolor/scalable/apps/*\n",
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
    let icon = build.join(format!("files/share/icons/hicolor/128x128/apps/{id}.png"));
    fs::create_dir_all(icon.parent().unwrap())?;
    fs::copy(repo().join("assets/installer/flatpak-icon.png"), icon)?;
    let mut finish = Command::new("flatpak");
    finish
        .args(["build-finish", "--share=network", "--device=dri"])
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
    fn welcome_pages_have_local_assets_and_product_specific_instructions() {
        check_branding().unwrap();
        for product in ["plugin", "ai-server"] {
            let page = welcome(product, "0.0.1-alpha.1");
            assert!(page.contains("branding/banner.png"));
            assert!(page.contains("0.0.1-alpha.1"));
            assert!(!page.contains("@PRODUCT@") && !page.contains("@START@"));
            assert!(!page.contains("Founder") && !page.contains("BIL4X4"));
            assert_eq!(page.contains("Close X-Plane"), product == "plugin");
            assert_eq!(
                page.contains("downloads the selected models"),
                product == "ai-server"
            );
        }
    }
    #[test]
    #[ignore = "requires Flatpak and the installed Freedesktop 25.08 SDK/runtime"]
    fn flatpak_package_exports_with_png_icon() {
        let work = tempfile::tempdir().unwrap();
        let bundle = work.path().join("bundle");
        let output = work.path().join("output");
        fs::create_dir_all(&bundle).unwrap();
        fs::create_dir_all(&output).unwrap();
        write(&bundle.join("openatc-launcher"), "#!/bin/sh\nexit 0\n").unwrap();
        flatpak(
            &bundle,
            work.path(),
            &output,
            "icon-check",
            "ai-server",
            "openatc-launcher",
            "org.openatc.AI",
        )
        .unwrap();
        assert!(
            fs::metadata(output.join("icon-check.flatpak"))
                .unwrap()
                .len()
                > 0
        );
    }
    #[test]
    fn release_versions_reject_paths_and_shell_text() {
        assert_eq!(version("v0.0.1-alpha.1").unwrap(), "0.0.1-alpha.1");
        for invalid in ["../bad", "1.2", "1.2.3;rm", "1.2.3-", "1.2.3-evil/thing"] {
            assert!(version(invalid).is_err());
        }
    }
}
