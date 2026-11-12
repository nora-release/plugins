#!/usr/bin/env python3
"""Packages the gateway provider plugins and writes the catalog Nora's
Plugins page installs them from.

    packages/gateway-providers/release.py --tag gateway-providers-1 [--publish]

Each plugin becomes a ZIP of its package (its manifest, and the program a
plugin with code runs): `<id>-<version>-<target>.zip`, or `<id>-<version>.zip`
for one without code, which runs on any Mac. The catalog,
`gateway-providers.json` at the repository's root, lists each plugin with
its providers and the download and SHA-256 of each package. With
--publish the packages are uploaded to the GitHub release named by --tag,
which is made if it doesn't exist.
"""

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
CATALOG = ROOT / "gateway-providers.json"
REPOSITORY = "nora-release/plugins"
# every file in a ZIP gets this time, so the same package is the same bytes
EPOCH = (2020, 1, 1, 0, 0, 0)


def host_target() -> str:
    out = subprocess.run(["rustc", "-vV"], check=True, capture_output=True, text=True).stdout
    for line in out.splitlines():
        if line.startswith("host: "):
            return line.removeprefix("host: ").strip()
    sys.exit("rustc did not say its host target")


def plugins():
    for folder in sorted(HERE.iterdir()):
        manifest = folder / "nora-plugin.json"
        if manifest.is_file():
            yield folder, json.loads(manifest.read_text())


def package(folder: Path, manifest: dict, staging: Path) -> Path:
    """The package's folder: built by package.sh for a plugin with code."""
    out = staging / folder.name
    if manifest["runtime"]["kind"] == "process":
        subprocess.run([str(HERE / "package.sh"), folder.name, str(out)], check=True, stdout=subprocess.DEVNULL)
    else:
        out.mkdir(parents=True)
        shutil.copy2(folder / "nora-plugin.json", out / "nora-plugin.json")
        for icon in folder.glob("icons/*"):
            (out / "icons").mkdir(exist_ok=True)
            shutil.copy2(icon, out / "icons" / icon.name)
        # the README Nora's plugin page shows once the plugin is installed
        shutil.copy2(folder / "README.md", out / "README.md")
    return out


def zip_package(directory: Path, target: Path) -> str:
    with zipfile.ZipFile(target, "w", zipfile.ZIP_DEFLATED) as archive:
        for path in sorted(p for p in directory.rglob("*") if p.is_file()):
            info = zipfile.ZipInfo(str(path.relative_to(directory)), EPOCH)
            info.external_attr = (0o100000 | (0o755 if os.access(path, os.X_OK) else 0o644)) << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, path.read_bytes())
    return hashlib.sha256(target.read_bytes()).hexdigest()


def entry(folder: Path, manifest: dict, packages: dict) -> dict:
    providers = [
        {key: p[key] for key in ("id", "name", "api", "signIn") if key in p}
        for p in manifest.get("contributes", {}).get("ai.providers", [])
    ]
    return {
        "id": manifest["id"],
        "name": manifest["name"],
        "version": manifest["version"],
        "description": manifest.get("description", ""),
        "repository": f"https://github.com/{REPOSITORY}/tree/main/packages/gateway-providers/{folder.name}",
        "providers": providers,
        "packages": packages,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--tag", required=True, help="the GitHub release the packages are uploaded to")
    parser.add_argument("--out", type=Path, help="where the ZIPs are written (default: a temporary folder)")
    parser.add_argument("--publish", action="store_true", help="upload the packages to the release")
    args = parser.parse_args()

    target = host_target()
    out = args.out or Path(tempfile.mkdtemp(prefix="gateway-providers-"))
    out.mkdir(parents=True, exist_ok=True)
    base = f"https://github.com/{REPOSITORY}/releases/download/{args.tag}"
    entries, files = [], []
    with tempfile.TemporaryDirectory() as staging:
        for folder, manifest in plugins():
            built = package(folder, manifest, Path(staging))
            code = manifest["runtime"]["kind"] == "process"
            name = f"{manifest['id']}-{manifest['version']}" + (f"-{target}" if code else "") + ".zip"
            digest = zip_package(built, out / name)
            files.append(out / name)
            entries.append(entry(folder, manifest, {target if code else "any": {"url": f"{base}/{name}", "sha256": digest}}))
            print(f"{name}  {digest}")
    CATALOG.write_text(json.dumps({"schemaVersion": 1, "release": args.tag, "plugins": entries}, indent=2) + "\n")
    print(f"wrote {CATALOG.relative_to(ROOT)} ({len(entries)} plugins); packages in {out}")

    if args.publish:
        exists = subprocess.run(["gh", "release", "view", args.tag, "--repo", REPOSITORY], capture_output=True).returncode == 0
        if not exists:
            subprocess.run(["gh", "release", "create", args.tag, "--repo", REPOSITORY, "--title", args.tag,
                            "--notes", "Gateway provider plugins for Nora; listed in gateway-providers.json."], check=True)
        subprocess.run(["gh", "release", "upload", args.tag, "--repo", REPOSITORY, "--clobber", *map(str, files)], check=True)
        print(f"uploaded {len(files)} packages to {args.tag}")


if __name__ == "__main__":
    main()
