#!/usr/bin/env python3
"""Packs a signed native plugin application as a package in Nora's format.

The package puts nora-plugin.json, the README and one JSON schema per command
beside the application. Every declaration is read from the manifest signed inside the
application, so the two cannot disagree.
"""

import argparse
import json
import pathlib
import shutil
import subprocess
import sys
import tempfile

INNER_MANIFEST = "Contents/Resources/nora-plugin.json"


def package_manifest(inner: dict, application: str) -> tuple[dict, dict]:
    contributes = {}
    if inner.get("panels"):
        contributes["ui.panels"] = inner["panels"]
    if inner.get("skills"):
        contributes["agent.skills"] = inner["skills"]
    schemas = {}
    tools = []
    for command in inner.get("commands") or []:
        path = f"tools/{command['id']}.json"
        schemas[path] = json.loads(command["inputSchemaJSON"])
        tools.append({"name": command["id"], "schema": path})
    if tools:
        contributes["agent.tools"] = tools
    manifest = {
        "apiVersion": 3,
        "id": inner["id"],
        "name": inner["name"],
        "version": inner["version"],
        "runtime": {
            "kind": "swift",
            "application": application,
            "extensionBundleIdentifier": inner["extensionBundleIdentifier"],
        },
        "scope": "host",
        "permissions": inner.get("permissions") or [],
        "contributes": contributes,
    }
    if inner.get("summary"):
        manifest["description"] = inner["summary"]
    return manifest, schemas


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", required=True, type=pathlib.Path, help="the signed .app")
    parser.add_argument("--archive", required=True, type=pathlib.Path, help="the ZIP to write")
    options = parser.parse_args()

    app = options.app.resolve()
    inner = json.loads((app / INNER_MANIFEST).read_text())
    manifest, schemas = package_manifest(inner, app.name)
    with tempfile.TemporaryDirectory() as staging:
        root = pathlib.Path(staging)
        subprocess.run(["ditto", str(app), str(root / app.name)], check=True)
        (root / "nora-plugin.json").write_text(json.dumps(manifest, indent=2) + "\n")
        # the README Nora's plugin page shows, beside the manifest as in every package
        readme = app / "Contents/Resources/README.md"
        if readme.is_file():
            shutil.copy2(readme, root / "README.md")
        for path, schema in schemas.items():
            (root / path).parent.mkdir(parents=True, exist_ok=True)
            (root / path).write_text(json.dumps(schema, indent=2) + "\n")
        options.archive.parent.mkdir(parents=True, exist_ok=True)
        options.archive.unlink(missing_ok=True)
        # Extended attributes would add AppleDouble files beside the application.
        subprocess.run(["ditto", "-c", "-k", "--norsrc", "--noextattr", "--noacl", str(root), str(options.archive)], check=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
