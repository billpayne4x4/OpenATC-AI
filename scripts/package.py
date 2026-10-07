"""Package the built Rust Linux plugin through the same installer used locally."""
from pathlib import Path
import argparse
import hashlib
import os
import re
import shutil
import subprocess
import sys
import tempfile
import zipfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("label", choices=["linux-x64"])
args = parser.parse_args()
repo = Path(__file__).resolve().parent.parent
packages = repo / "packages"
packages.mkdir(exist_ok=True)
with tempfile.TemporaryDirectory(prefix="openatc-package-") as temporary:
    folder = Path(temporary)
    simulator = folder / "simulator"
    subprocess.run([sys.executable, str(repo / "scripts/install-rust-plugin.py"), str(simulator)], check=True)
    stage = folder / "stage"
    stage.mkdir()
    shutil.copytree(simulator / "Resources/plugins/OpenATC", stage / "OpenATC")
    shutil.copy2(repo / "LICENSE", stage / "LICENSE")
    shutil.copy2(repo / "README.md", stage / "README.md")
    shutil.copy2(repo / "Cargo.lock", stage / "OpenATC/Cargo.lock")
    # Preserve the modified miniaudio header, native adapter and embedded license.
    shutil.copytree(repo / "crates/audio", stage / "OpenATC/source/audio")
    shutil.copytree(repo / "assets/fonts", stage / "OpenATC/assets/fonts", dirs_exist_ok=True)
    tree = subprocess.run(["cargo", "tree", "--manifest-path", str(repo / "Cargo.toml"), "--locked", "--offline", "-p", "openatc-plugin", "-p", "openatc-engine", "--prefix", "none", "--format", "{p}"], check=True, capture_output=True, text=True).stdout
    registry = Path(os.environ.get("CARGO_HOME", str(Path.home() / ".cargo"))) / "registry/src"
    notices = stage / "OpenATC/assets/licenses/cargo"
    notices.mkdir(parents=True)
    for name, version in sorted(set(re.findall(r"^([a-zA-Z0-9_-]+) v([^\s]+)", tree, re.MULTILINE))):
        for source in registry.glob(f"*/{name}-{version}"):
            # Include embedded native-library notices as well as crate-level licenses.
            for notice in source.rglob("*"):
                if notice.is_file() and notice.name.lower().startswith(("license", "copying", "notice", "copyright")):
                    destination = notices / f"{name}-{version}" / notice.relative_to(source)
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(notice, destination)
    archive = packages / f"open-atc-{args.label}.zip"
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED, strict_timestamps=False) as bundle:
        for file in sorted(stage.rglob("*")):
            if file.is_file():
                bundle.write(file, file.relative_to(stage))
archive.with_suffix(".zip.sha256").write_text(hashlib.sha256(archive.read_bytes()).hexdigest() + "  " + archive.name + "\n")
print(f"Packaged: {archive}")
