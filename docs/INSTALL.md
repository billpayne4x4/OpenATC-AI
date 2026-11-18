# Install OpenATC AI

OpenATC has two downloads: the **X-Plane 12 plugin** and the **AI server**. Install the plugin on the simulator computer. Run the server there or on another computer on your network. Download the package matching your operating system and processor. Intel and Apple Silicon Macs have separate packages. Linux and Windows packages target x86-64.

Read the release notes for unfinished features. Generated traffic is disabled by default while its full flight lifecycle is being completed. A package building successfully does not establish simulator, audio or GPU compatibility on every computer.

Native Linux AI-server ZIP, AppImage, DEB and RPM packages require glibc 2.39 or newer, such as Ubuntu 24.04. The Linux plugin retains its older build baseline; the AI server can run separately on a compatible machine. Flatpak uses its own runtime.

## Before installing

1. For standalone installations, start the chosen X-Plane 12 installation once, then close it. The installer uses its native executable and `Log.txt` to validate platform and major version. Fresh Steam installations can instead be identified by their matching X-Plane 12 app manifest, without a first launch.
2. Close every X-Plane process before installing, updating or removing the plugin. Close other OpenATC installers too.
3. Keep a backup of customised configuration. Allow enough free space for the new plugin, preserved custom files and the previous version. Legacy CSL/model folders inside the plugin folder also need staging space; models downloaded by the current build live in the user data folder.
4. Extract portable ZIP downloads completely before running anything. Keep their folder contents together. Compare the download's SHA-256 checksum with its accompanying `.sha256` file if checking download integrity.

The installer discovers recorded standalone installations and Steam libraries, including configured libraries on other disks. On Linux it also checks native Steam, Flatpak Steam and Snap Steam locations. Discovery is a convenience: you can always select or enter the actual X-Plane folder. Choose the folder containing `Resources` and the simulator executable, not `Resources/plugins`. Custom locations, mounted disks and sandbox permissions can require explicit selection. Windows/Proton installations are not accepted by the Linux installer. No whole-disk search is performed.

## Windows plugin

Run the **plugin Setup.exe**. Its branded wizard installs a small per-user installer application, then asks which X-Plane installation to update. Choose a detected installation or browse. Administrator access is normally unnecessary; a protected simulator folder may require moving X-Plane to a user-writable location or arranging permissions yourself.

Start X-Plane after setup finishes. OpenATC appears in the plugin menu. To update, close X-Plane and run the newer Setup.exe with the same destination.

Remove **OpenATC Plugin** through Windows Settings → Apps → Installed apps. This removes the managed plugin files from the recorded simulator installation and removes the installer application. Modified and unowned files are preserved. One installed wizard manages one simulator location: remove that managed copy before changing destinations. Use the portable ZIP for additional simulator copies.

The ZIP is also available: run `openatc-install.exe "D:\Flight Sims\X-Plane 12" --yes` from its extracted folder, or copy the complete `payload/OpenATC` folder into `Resources/plugins`. A manual copy has no installer ownership record; back it up and remove it manually, or use the managed installer for future updates. Do not copy only the `.xpl` file.

Unsigned test builds can display a Windows publisher warning. Public signed builds require publisher credentials configured by the maintainer; the presence of Setup.exe alone does not mean it is signed.

## macOS plugin

Open the plugin DMG matching your Mac. Open **OpenATC Plugin Installer.app**, choose **Install / Update**, then select your X-Plane 12 folder. Wait for the success dialog before opening X-Plane. The DMG includes the offline welcome page, this guide and licences.

To remove the plugin, reopen the installer app, choose **Remove**, then select the same simulator. Dragging the installer app to Trash alone does not remove the plugin from X-Plane. Modified and unowned files are preserved. Eject the DMG after use.

Portable ZIPs include `openatc-install` and the complete payload. Terminal installation is also supported: `./openatc-install "/path/to/X-Plane 12" --yes`; removal: `./openatc-install --uninstall "/path/to/X-Plane 12" --yes`.

Developer test packages may be unsigned or unnotarised. Follow Apple's per-application Open Anyway process only for downloads you trust; do not disable Gatekeeper globally. Signing and notarisation status must be stated in the release notes. The installer refuses a simulator executable that lacks your platform architecture.

## Linux plugin

No graphical installer is required. Extract the plugin ZIP, open a terminal in its extracted folder, and run:

```sh
sh ./install.sh
```

Choose a detected simulator by number or type its full path. Confirm the operation. To specify a path directly:

```sh
sh ./install.sh "$HOME/.steam/steam/steamapps/common/X-Plane 12"
```

Paths containing spaces must be quoted. **Do not use sudo** for an installation owned by your account. A native X-Plane executable must be executable and the plugins directory writable. If extraction lost permissions, use `chmod +x openatc-install` before retrying. The script and bundled Rust installer must stay together with `payload`.

Removal uses the same extracted download:

```sh
sh ./uninstall.sh "/path/to/X-Plane 12"
```

For a noninteractive managed install or removal, add `--yes` after the simulator path. This skips confirmation, not version/process/ownership checks. `./openatc-install --check "/path/to/X-Plane 12"` checks the selected installation and that the simulator is closed.

Flatpak Steam and Snap Steam installations may restrict access to external disks. Select the real mounted simulator path and grant permissions through the relevant Steam package's settings if required. OpenATC's installer itself runs outside Steam's sandbox. Never point it at an inaccessible or guessed sandbox path.

## What updates and removal preserve

Installation stages a replacement beside the plugin, checks the simulator is still closed, moves the existing tree into **`X-Plane 12/Resources/OpenATC Backups/`**, then moves the staged tree into place. If the replacement rename fails, the installer restores the previous tree. Backups deliberately live outside `Resources/plugins` so X-Plane cannot load duplicate plugins. Keep the latest backup until the new version works; remove older backups manually when no longer needed.

A managed install creates `openatc-install-manifest.json` with owned file hashes. Updates preserve unowned custom files and modified TOML/JSON/TXT/MD files. Unchanged obsolete managed files are removed. Removal deletes unchanged managed files and retains modified/unowned files, so an OpenATC folder can remain afterwards. Personal settings and downloaded AI models outside the plugin are never removed. Retained files and backups can contain private configuration; manage them like your other backups.

For older manual installations, customised editable files that differ from the new payload are preserved. **Configuration is preserved, not automatically merged.** Compare preserved prompts/policies with the defaults in the new ZIP when a release adds settings or changes their schema. A preserved old default can also need review. Legacy/manual copies without an ownership manifest are not automatically uninstalled.

The installer refuses symlinked plugin trees or files rather than following them into unrelated folders. Use a deliberate manual installation for such setups. It also refuses concurrent operations using `Resources/plugins/.openatc-install.lock`. If a crash leaves that lock, first confirm no installer or simulator is running, then remove only that lock and retry. Never delete your plugin folder merely because an installation failed.

To restore a backup manually: close X-Plane, move the current OpenATC folder aside, and copy the chosen backup to `Resources/plugins/OpenATC`. Keep only one loadable copy in the plugins folder.

## Install and configure the AI server

Use **one** package format for a server installation:

- **Windows:** run the AI-server Setup.exe. Launch OpenATC AI Server from the Start menu. Use Configure OpenATC AI Server to open the local settings editor; its terminal prints the address. Save, stop the editor with Ctrl-C and restart the server. Remove the application through Settings → Apps after stopping it.
- **macOS:** open the server DMG and drag OpenATC AI Server.app into Applications. Open it and choose Configure or Start. It opens Terminal so setup, model downloads and errors remain visible. First-run Start also offers terminal configuration when no config exists. Stop the server with Ctrl-C in that terminal before replacing or moving the application to Trash. Closing the application icon is not a substitute for stopping its server process.
- **Linux AppImage:** make the downloaded file executable and run it from a terminal. If FUSE is unavailable, AppImage's `--appimage-extract-and-run` mode can be used. Stop it with Ctrl-C; remove the AppImage to uninstall the application.
- **Debian/Ubuntu:** install the `.deb` with your distribution's graphical package manager or `sudo apt install ./openatc-ai-server-VERSION-linux-x64.deb`. Remove with `sudo apt remove openatc-ai-server` after stopping it.
- **Fedora/RPM:** use your package manager or `sudo dnf install ./openatc-ai-server-VERSION-linux-x64.rpm`; remove with `sudo dnf remove openatc-ai-server` after stopping it.
- **Flatpak:** install the `.flatpak` bundle using your software centre or `flatpak install --user ./PACKAGE.flatpak`; launch with `flatpak run org.openatc.AI`. The Freedesktop 25.08 runtime must be available. Remove with `flatpak uninstall --user org.openatc.AI`. Do not request data deletion if keeping configuration/models. GPU drivers/extensions and sandbox behaviour need verification on your machine; a successful bundle build does not guarantee CUDA availability.
- **Portable ZIP:** extract it, keep all contents together and run `openatc-launcher` (`.exe` on Windows) from a terminal. Stop it before replacing or deleting the extracted folder.

For terminal configuration run the launcher with **`--configure`**. **`--configure-web`** starts a loopback-only editor and prints its browser URL; open that URL on the server computer, save, stop the editor with Ctrl-C, then restart the normal server. It does not load inference models while editing. On macOS the app opens a terminal automatically. RPM/DEB packages also include an optional user systemd service; it is not enabled automatically. Configure first, then use `systemctl --user enable --now openatc-ai` if wanted, and `systemctl --user disable --now openatc-ai` before uninstalling.

Settings are stored in `server.toml` under the OpenATC data directory: normally `~/.local/share/openatc-ai` on Linux, `~/Library/Application Support/openatc-ai` on macOS and `%LOCALAPPDATA%\openatc-ai` on Windows. `XDG_DATA_HOME`, `--config`, `--models-dir` or `OPENATC_AI_MODELS` can change these locations. Flatpak uses its own application data directory. Models normally live in the `models` subfolder. Application uninstall preserves that data by default; remove it manually only if you intend to delete settings and downloaded models.

The server downloads selected models on first inference startup. Keep internet access and sufficient disk space available. CPU/GPU assignments, model, context size and other controls are in the configuration form/TOML. Automatic VRAM sizing is **off by default** because the simulator also needs GPU memory. CPU mode can be preferable on a shared machine; begin conservatively and adjust for your hardware. Do not assume a package with GPU support can run without the appropriate host drivers. On Linux proprietary GPU drivers belong to the host, not the portable payload.

By default the server listens locally. For a separate server use `openatc-launcher --host 0.0.0.0 --port 8099` and allow the port only on your trusted network. Set the plugin's AI, STT and TTS base URLs to `http://SERVER-IP:8099`. Do not expose this unauthenticated local service directly to the internet. Keep the terminal/log available when diagnosing connection failures; use the README's server diagnostics and API checks.

## If something fails

Read the complete error before retrying. Check the selected simulator root, that `Log.txt` identifies X-Plane 12, matching platform/architecture, simulator closure, write permissions and free disk space. A running-process check that cannot inspect processes fails safely. Installer errors do not require a simulator launch to investigate. Report the package name/version, operating system, error and selected installation type; omit credentials and private configuration. Release notes distinguish tested installations from pending native acceptance.
