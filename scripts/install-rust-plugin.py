"""Install built Linux Rust binaries and their assets, retaining a recoverable backup."""
from pathlib import Path
from datetime import datetime
import argparse
import os
import shutil
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("xplane", type=Path, help="X-Plane installation directory")
args = parser.parse_args()
repo = Path(__file__).resolve().parent.parent
plugin = args.xplane / "Resources/plugins/OpenATC"
files = {
    repo / "target/release/libopenatc_plugin.so": plugin / "64/lin.xpl",
    repo / "target/release/open-atc-engine": plugin / "bin/open-atc-engine",
    repo / "radio-stations.toml": plugin / "radio-stations.toml",
    repo / "regions.toml": plugin / "regions.toml",
    repo / "intents.toml": plugin / "intents.toml",
    repo / "README.md": plugin / "README.md",
}
for folder in ("aircraft", "prompts", "speech", "assets/taxi-arrow", "assets/licenses", "assets/branding", "assets/fonts", "assets/geography", "assets/controls", "docs"):
    for source in (repo / folder).rglob("*"):
        if source.is_file():
            files[source] = plugin / source.relative_to(repo)
for source in (repo / "vendor/xplane").rglob("*"):
    if source.is_file():
        files[source] = plugin / "source/xplane" / source.relative_to(repo / "vendor/xplane")
for source in files:
    if not source.is_file():
        raise SystemExit(f"Missing build artifact: {source}")
# Reject a broken library before touching the existing installation.
subprocess.run([str(repo / "target/release/open-atc-engine"),
                "--check-speech", str(repo / "speech")], check=True)
if plugin.exists():
    backup = Path.home() / ".local/share/openatc-ai/backups" / datetime.now().strftime("OpenATC-%Y%m%d-%H%M%S")
    shutil.copytree(plugin, backup)
    print(f"Backup: {backup}")
# Preserve installed flat files as disabled legacy copies, after the full backup.
# Otherwise old IDs would collide with the new recursive library.
if (plugin / "speech").exists():
    for legacy in (plugin / "speech").glob("*.toml"):
        disabled = legacy.with_suffix(".toml.legacy")
        if disabled.exists():
            disabled = legacy.with_suffix(".toml." + datetime.now().strftime("%Y%m%d-%H%M%S") + ".legacy")
        legacy.rename(disabled)
for source, destination in files.items():
    if source.name == "radio-stations.toml" and destination.exists():
        continue
    destination.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".install-", dir=destination.parent)
    os.close(fd)
    try:
        shutil.copy2(source, temporary)
        os.replace(temporary, destination)
    finally:
        Path(temporary).unlink(missing_ok=True)
print(f"Installed: {plugin}")
