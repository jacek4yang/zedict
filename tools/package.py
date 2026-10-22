"""Package only executable artifacts and usage documentation, never local dictionary data."""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import tarfile
import tomllib
import zipfile

parser = argparse.ArgumentParser()
parser.add_argument("--target")
args = parser.parse_args()
root = Path(__file__).resolve().parents[1]
version = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
target = args.target or f"{platform.system().lower()}-{platform.machine().lower()}"
windows = "windows" in target
directory = root / "target" / args.target / "release" if args.target else root / "target" / "release"
files = [directory / (name + (".exe" if windows else "")) for name in ("zedict", "dict-english")]
files += [
    root / "README.md",
    root / "CHANGELOG.md",
    root / "LICENSE-MIT",
    root / "LICENSE-APACHE",
]
for file in files:
    if not file.is_file():
        raise SystemExit(f"Missing build artifact: {file}")
out = root / "target" / "packages"
out.mkdir(parents=True, exist_ok=True)
name = f"zedict-{version}-{target}"
archive = out / (name + (".zip" if windows else ".tar.gz"))
if windows:
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as package:
        for file in files:
            package.write(file, f"{name}/{file.name}")
else:
    with tarfile.open(archive, "w:gz") as package:
        for file in files:
            package.add(file, arcname=f"{name}/{file.name}")
checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
archive.with_name(archive.name + ".sha256").write_text(f"{checksum}  {archive.name}\n", encoding="utf-8")
print(json.dumps({"archive": str(archive), "sha256": checksum}))
