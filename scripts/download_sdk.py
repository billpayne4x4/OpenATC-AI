from pathlib import Path
from urllib.request import urlretrieve
from zipfile import ZipFile

sdk_url = "https://developer.x-plane.com/wp-content/plugins/code-sample-generation/sdk_zip_files/XPSDK430.zip"
vendor_directory = Path("vendor")
vendor_directory.mkdir(exist_ok=True)
if (vendor_directory / "SDK/CHeaders/XPLM/XPLMPlugin.h").is_file():
    print("Using existing X-Plane SDK")
    raise SystemExit(0)
archive_path = vendor_directory / "XPSDK430.zip"
urlretrieve(sdk_url, archive_path)
with ZipFile(archive_path) as archive:
    archive.extractall(vendor_directory)
if not (vendor_directory / "SDK/CHeaders/XPLM/XPLMPlugin.h").is_file():
    raise SystemExit("SDK archive layout changed. Set XPLANE_SDK to its SDK directory.")
