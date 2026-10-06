from pathlib import Path
import hashlib
import shutil
import sys
from urllib.request import urlretrieve

label = sys.argv[1]
stage = Path("dist")
notices = stage / "licenses"
notices.mkdir(exist_ok=True)
dependencies = ["imgui", "glfw", "json", "httplib", "miniaudio"]
if sys.platform.startswith("linux"):
    dependencies.append("glad")
for dependency in dependencies:
    source = Path("build/_deps") / f"{dependency}-src"
    candidates = [path for path in source.iterdir() if path.is_file() and path.name.lower().startswith(("license", "copying"))]
    if not candidates:
        raise SystemExit(f"License missing for {dependency}")
    for candidate in candidates:
        shutil.copy2(candidate, notices / f"{dependency}-{candidate.name}")
shutil.copy2("docs/THIRD_PARTY.md", notices / "THIRD_PARTY.md")
urlretrieve("https://raw.githubusercontent.com/openssl/openssl/openssl-3.0/LICENSE.txt", notices / "OpenSSL-LICENSE.txt")
packages = Path("packages")
packages.mkdir(exist_ok=True)
archive = Path(shutil.make_archive(str(packages / f"open-atc-{label}"), "zip", stage))
archive.with_suffix(".zip.sha256").write_text(hashlib.sha256(archive.read_bytes()).hexdigest() + "  " + archive.name + "\n")
