"""Verify provider selection with local fixture compilers and real ZIP output."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import zipfile


SCRIPTS = Path(__file__).resolve().parent
PLATFORM = "aarch64-apple-darwin"

TOOL = r'''
import hashlib
import json
import os
from pathlib import Path
import sys

with open(os.environ["FAKE_TOOL_LOG"], "a") as log:
    log.write(json.dumps(sys.argv[1:]) + "\n")
mode, source, output = sys.argv[1], Path(sys.argv[2]), Path(sys.argv[3])
definitions = [source] if source.is_file() else sorted(source.glob("*/nora-plugin.json"))
if mode == "--prepare":
    for path in definitions:
        manifest = json.loads(path.read_text())
        destination = output / manifest["id"]
        destination.mkdir(parents=True, exist_ok=True)
        (destination / "nora-plugin.json").write_text(json.dumps(manifest, indent=2))
elif mode == "--catalog":
    archives, target, base = Path(sys.argv[4]), sys.argv[5], sys.argv[6]
    providers = []
    for path in definitions:
        manifest = json.loads(path.read_text())
        identity, version = manifest["id"], manifest["version"]
        name = f"{identity}-{version}-{target}.zip"
        digest = hashlib.sha256((archives / name).read_bytes()).hexdigest()
        providers.append({"id":identity, "version":version, "packages":{
            target:{"url":base.rstrip("/") + "/" + name,"sha256":digest}}})
    output.write_text(json.dumps({"schemaVersion":2,"providers":providers}))
else:
    raise SystemExit("Unexpected fixture tool invocation")
'''

CARGO = r'''
import json
import os
from pathlib import Path
import sys
import tomllib

args = sys.argv[1:]
with open(os.environ["FAKE_CARGO_LOG"], "a") as log:
    log.write(json.dumps(args) + "\n")
if os.environ.get("FAKE_CARGO_FAIL"):
    raise SystemExit(23)
command = next(argument for argument in args if argument in {"build", "test", "fmt"})
if command != "build":
    raise SystemExit(0)
manifest = Path(args[args.index("--manifest-path") + 1])
if "--workspace" in args:
    manifests = manifest.parent.glob("packages/*/Cargo.toml")
    packages = [tomllib.loads(path.read_text())["package"]["name"] for path in manifests]
else:
    packages = [args[index + 1] for index, value in enumerate(args) if value == "--package"]
target = Path(os.environ["CARGO_TARGET_DIR"])
if os.environ.get("CARGO_BUILD_TARGET"):
    target /= os.environ["CARGO_BUILD_TARGET"]
target /= "release" if "--release" in args else "debug"
target.mkdir(parents=True, exist_ok=True)
for package in packages:
    binary = target / package
    if package == "nora-provider":
        binary.write_text("#!" + sys.executable + "\n" + Path(os.environ["FAKE_TOOL_SOURCE"]).read_text())
    else:
        binary.write_text("#!/bin/sh\n# " + package + "\nexit 0\n")
    binary.chmod(0o755)
'''

DITTO = r'''
from pathlib import Path
import sys
import zipfile

source, output = Path(sys.argv[-2]), Path(sys.argv[-1])
with zipfile.ZipFile(output, "w", zipfile.ZIP_DEFLATED) as archive:
    for path in sorted(source.rglob("*")):
        if path.is_file():
            archive.write(path, path.relative_to(source.parent))
'''


class ProviderBuildTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="nora-provider-build-test-")
        self.directory = Path(self.temporary.name)
        self.source = self.directory / "source checkout"
        self.source.mkdir()
        self.caller = self.directory / "caller"
        self.caller.mkdir()
        self.output = self.directory / "output"
        self.bin = self.directory / "bin"
        self.bin.mkdir()
        self.log = self.directory / "cargo.jsonl"
        self.tool_log = self.directory / "tool.jsonl"
        (self.source / "Cargo.toml").write_text('[workspace]\nmembers = ["packages/runtime", "packages/openai", "packages/anthropic"]\n')
        for identity in ["openai", "anthropic", "compatible"]:
            self.add_provider(identity, native=identity != "compatible")
        runtime = self.source / "packages/runtime"
        runtime.mkdir()
        (runtime / "Cargo.toml").write_text('[package]\nname = "nora-provider"\nversion = "1.0.0"\n')
        (self.source / "LICENSE").write_text("fixture project license")
        (self.source / "LICENSES").mkdir()
        (self.source / "LICENSES/upstream.txt").write_text("fixture upstream notice")
        (self.directory / "tool.py").write_text(TOOL)
        for name, code in [("cargo", CARGO), ("ditto", DITTO)]:
            self.executable(name, "#!" + sys.executable + "\n" + code)
        for name in ["strip", "codesign"]:
            self.executable(name, "#!/bin/sh\nexit 0\n")
        self.executable("rustc", f"#!/bin/sh\nprintf '%s\\n' 'rustc fixture' 'host: {PLATFORM}'\n")
        self.environment = {
            "PATH": str(self.bin) + os.pathsep + os.environ["PATH"],
            "CARGO": str(self.bin / "cargo"),
            "RUSTC": str(self.bin / "rustc"),
            "FAKE_CARGO_LOG": str(self.log),
            "FAKE_TOOL_LOG": str(self.tool_log),
            "FAKE_TOOL_SOURCE": str(self.directory / "tool.py"),
            "NORA_PROVIDER_SOURCE_ROOT": str(self.source),
            "PROVIDER_DOWNLOAD_BASE_URL": "https://example.test/openai-v1.2.1",
            "PYTHONDONTWRITEBYTECODE": "1",
        }

    def tearDown(self):
        self.temporary.cleanup()

    def executable(self, name, contents):
        path = self.bin / name
        path.write_text(contents)
        path.chmod(0o755)

    def add_provider(self, identity, native=True):
        source = self.source / "packages" / identity
        source.mkdir(parents=True)
        manifest = {"apiVersion":2,"id":identity,"name":identity,"version":"1.2.1",
                    "runtime":{"entry":"provider"},"contributes":{"provider":{}}}
        (source / "nora-plugin.json").write_text(json.dumps(manifest, indent=2))
        if native:
            (source / "Cargo.toml").write_text(f'[package]\nname = "nora-provider-{identity}"\nversion = "1.0.0"\n')

    def run_script(self, script, *arguments, selected="openai", environment=None):
        values = self.environment.copy()
        if selected is not None:
            values["PROVIDER_ID"] = selected
        if environment:
            values.update(environment)
        return subprocess.run(["bash", str(SCRIPTS / script), *map(str, arguments)],
                              text=True, capture_output=True, cwd=self.caller, env=values, check=False)

    def calls(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    @staticmethod
    def packages(call):
        return [call[index + 1] for index, value in enumerate(call) if value == "--package"]

    def packaged_ids(self):
        return sorted(path.name.split("-1.2.1-")[0] for path in self.output.glob("*.zip"))

    def assert_success(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_native_package_builds_only_its_crate_and_shared_preparation_tool(self):
        result = self.run_script("package-providers.sh", self.output)
        self.assert_success(result)
        self.assertEqual(self.packaged_ids(), ["openai"])
        self.assertEqual(len(self.calls()), 1)
        call = self.calls()[0]
        self.assertEqual(self.packages(call), ["nora-provider-openai", "nora-provider"])
        self.assertNotIn("--workspace", call)
        self.assertIn("--locked", call)
        self.assertIn("--release", call)
        self.assertEqual(call[call.index("--manifest-path") + 1], str(self.source / "Cargo.toml"))
        archive = next(self.output.glob("*.zip"))
        with zipfile.ZipFile(archive) as contents:
            self.assertEqual(sorted(contents.namelist()), ["openai/LICENSES/Nora-Plugins-MIT.txt",
                "openai/LICENSES/upstream.txt", "openai/nora-plugin.json", "openai/provider"])
            self.assertEqual(contents.read("openai/LICENSES/Nora-Plugins-MIT.txt"), b"fixture project license")
            self.assertIn(b"nora-provider-openai", contents.read("openai/provider"))
        catalog = json.loads((self.output / "providers.json").read_text())
        self.assertEqual([entry["id"] for entry in catalog["providers"]], ["openai"])
        package = catalog["providers"][0]["packages"][PLATFORM]
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        self.assertEqual(package["sha256"], digest)
        self.assertEqual(package["url"], self.environment["PROVIDER_DOWNLOAD_BASE_URL"] + "/" + archive.name)
        self.assertEqual((self.output / "SHA256SUMS").read_text().strip(), f"{digest}  ./{archive.name}")

    def test_generic_provider_builds_only_runtime_and_uses_its_executable(self):
        result = self.run_script("package-providers.sh", self.output, selected="compatible")
        self.assert_success(result)
        self.assertEqual(self.packaged_ids(), ["compatible"])
        self.assertEqual(self.packages(self.calls()[0]), ["nora-provider"])
        self.assertNotIn("--workspace", self.calls()[0])
        archive = next(self.output.glob("*.zip"))
        with zipfile.ZipFile(archive) as contents:
            expected = self.source / "target" / PLATFORM / "release/nora-provider"
            self.assertEqual(contents.read("compatible/provider"), expected.read_bytes())

    def test_default_package_entry_still_builds_and_archives_every_provider(self):
        result = self.run_script("package-providers.sh", self.output, selected=None)
        self.assert_success(result)
        self.assertEqual(self.packaged_ids(), ["anthropic", "compatible", "openai"])
        self.assertIn("--workspace", self.calls()[0])
        self.assertEqual(self.packages(self.calls()[0]), [])
        catalog = json.loads((self.output / "providers.json").read_text())
        self.assertEqual({entry["id"] for entry in catalog["providers"]}, {"openai", "anthropic", "compatible"})

    def test_selected_tests_and_format_checks_do_not_use_the_workspace(self):
        for mode in ["--test", "--fmt-check", "--fmt"]:
            self.assert_success(self.run_script("build-providers.sh", mode))
        for call in self.calls():
            self.assertEqual(self.packages(call), ["nora-provider-openai"])
            self.assertNotIn("--workspace", call)
            self.assertNotIn("--all", call)

    def test_generic_tests_select_the_shared_runtime(self):
        self.assert_success(self.run_script("build-providers.sh", "--test", selected="compatible"))
        self.assertEqual(self.packages(self.calls()[0]), ["nora-provider"])

    def test_default_test_and_format_entries_remain_workspace_wide(self):
        self.assert_success(self.run_script("build-providers.sh", "--test", selected=None))
        self.assert_success(self.run_script("build-providers.sh", "--fmt-check", selected=None))
        self.assertIn("--workspace", self.calls()[0])
        self.assertIn("--all", self.calls()[1])

    def test_external_source_uses_current_tools_instead_of_old_source_scripts(self):
        old_scripts = self.source / "scripts"
        old_scripts.mkdir()
        (old_scripts / "build-providers.sh").write_text("#!/bin/sh\nexit 94\n")
        result = self.run_script("package-providers.sh", self.output,
                                 environment={"CARGO_TARGET_DIR":"relative target"})
        self.assert_success(result)
        expected = self.caller / "relative target" / PLATFORM / "release/nora-provider-openai"
        self.assertTrue(expected.is_file())
        self.assertEqual(self.packaged_ids(), ["openai"])

    def test_omitting_source_override_uses_the_tools_repository(self):
        self.environment.pop("NORA_PROVIDER_SOURCE_ROOT")
        self.assert_success(self.run_script("build-providers.sh", "--test"))
        call = self.calls()[0]
        self.assertEqual(call[call.index("--manifest-path") + 1], str(SCRIPTS.parent / "Cargo.toml"))

    def test_unknown_or_invalid_provider_ids_do_not_invoke_cargo(self):
        for selected in ["unknown", "../openai", "openai/other", "openai other"]:
            with self.subTest(selected=selected):
                result = self.run_script("build-providers.sh", "build", self.output, selected=selected)
                self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.log.exists())
        self.assertFalse(self.output.exists())

    def test_manifest_identity_mismatch_is_rejected_before_building(self):
        path = self.source / "packages/openai/nora-plugin.json"
        manifest = json.loads(path.read_text())
        manifest["id"] = "other"
        path.write_text(json.dumps(manifest))
        result = self.run_script("package-providers.sh", self.output)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("match its manifest ID", result.stderr)
        self.assertFalse(self.log.exists())

    def test_selected_package_replaces_stale_archives_in_its_dedicated_output(self):
        self.output.mkdir()
        (self.output / "anthropic-old.zip").write_text("stale")
        self.assert_success(self.run_script("package-providers.sh", self.output))
        self.assertEqual(self.packaged_ids(), ["openai"])
        self.assertEqual({path.name for path in self.output.iterdir()},
                         {f"openai-1.2.1-{PLATFORM}.zip", "SHA256SUMS", "providers.json"})

    def test_build_failure_preserves_the_previous_output_and_cleans_staging(self):
        self.output.mkdir()
        marker = self.output / "previous.zip"
        marker.write_text("previous validated artifact")
        result = self.run_script("package-providers.sh", self.output,
                                 environment={"FAKE_CARGO_FAIL":"1"})
        self.assertEqual(result.returncode, 23)
        self.assertEqual(marker.read_text(), "previous validated artifact")
        self.assertEqual(list(self.directory.glob(".providers-*.??????")), [])

    def test_native_crate_name_is_read_from_the_selected_source_manifest(self):
        manifest = self.source / "packages/openai/Cargo.toml"
        manifest.write_text('[package]\nname = "custom-openai-binary"\nversion = "1.0.0"\n')
        self.assert_success(self.run_script("package-providers.sh", self.output))
        self.assertEqual(self.packages(self.calls()[0]), ["custom-openai-binary", "nora-provider"])
        with zipfile.ZipFile(next(self.output.glob("*.zip"))) as archive:
            self.assertIn(b"custom-openai-binary", archive.read("openai/provider"))


if __name__ == "__main__":
    unittest.main()
