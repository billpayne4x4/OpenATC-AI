import re
import subprocess
import sys

for binary in sys.argv[1:]:
    result = subprocess.run(["ldd", binary], capture_output=True, text=True, check=True)
    dependencies = result.stdout
    forbidden = re.findall(r"\blib(?:X[^\s/]*|xcb[^\s/]*|GLX[^\s/]*|GL\.so[^\s/]*)", dependencies)
    if binary.endswith(".xpl"):
        forbidden += re.findall(r"\blib(?:ssl|crypto|curl)[^\s/]*", dependencies)
    if forbidden or "not found" in dependencies:
        raise SystemExit(f"Dependency audit failed for {binary}:\n{dependencies}")
    if binary.endswith(".xpl"):
        exports = subprocess.run(["nm", "-D", "--defined-only", binary], capture_output=True, text=True, check=True).stdout
        symbols = {line.split()[-1] for line in exports.splitlines() if line.split()}
        required = {"XPluginStart", "XPluginStop", "XPluginEnable", "XPluginDisable", "XPluginReceiveMessage"}
        if not required.issubset(symbols):
            raise SystemExit(f"Missing plugin exports: {sorted(required - symbols)}")
    print(f"Dependency audit passed: {binary}")
