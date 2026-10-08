"""Install Linux AI service binaries with their relocatable eSpeak data."""
from pathlib import Path
import argparse
import shutil

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('destination', type=Path, help='AI service directory containing bin/')
args = parser.parse_args()
repo = Path(__file__).resolve().parent.parent
release = repo / 'target/release'
binaries = [release / 'openatc-ai', release / 'openatc-stt']
candidates = [path for path in (release / 'build').glob('espeak-rs-sys-*/out/share/espeak-ng-data') if (path / 'phontab').is_file()]
if not all(path.is_file() for path in binaries) or not candidates:
    raise SystemExit('Build openatc-ai and openatc-stt in release mode before installation.')
data = max(candidates, key=lambda path: (path / 'phontab').stat().st_mtime)
bin_dir = args.destination / 'bin'
bin_dir.mkdir(parents=True, exist_ok=True)
for source in binaries:
    shutil.copy2(source, bin_dir / source.name)
shutil.copytree(data, bin_dir / 'espeak-ng-data', dirs_exist_ok=True)
print(f'Installed AI binaries and eSpeak data to {bin_dir}. Restart the AI service to reload them.')
