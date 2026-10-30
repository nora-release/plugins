import base64
import copy
import hashlib
import io
import json
from pathlib import Path
import stat
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from unittest.mock import patch
from urllib.error import URLError
import zipfile

sys.dont_write_bytecode = True

import provider_release as release
from provider_release_github import GitHub, GitHubError, publish


TAG = "openai-v1.2.1"
REPO = "nora-release/plugins"
TARGET = "a" * 40
PLATFORM = "aarch64-apple-darwin"


def row(plugin="openai", version="1.2.0", checksum="0" * 64):
    return {"apiVersion": 2, "id": plugin, "name": plugin, "version": version,
            "description": f"The {plugin} provider", "contributes": ["provider"],
            "repository": f"https://github.com/nora-release/plugins/tree/main/packages/{plugin}",
            "packages": {PLATFORM: {"url": "https://example.test/old.zip", "sha256": checksum}}}


class Fixture:
    def __init__(self, directory):
        self.root = Path(directory)
        self.identity = release.identity(TAG, REPO)
        self.manifest = {"apiVersion": 2, "id": "openai", "name": "OpenAI", "version": "1.2.1",
                         "runtime": {"entry": "provider"}, "contributes": {"provider": {}}}
        source = self.root / "packages/openai"
        source.mkdir(parents=True)
        (source / "nora-plugin.json").write_bytes(release.json_bytes(self.manifest))
        self.current = {"schemaVersion": 2, "providers": [row(), row("other", "4.0.0")]}
        self.current["providers"][1]["icon"] = "other-icon.svg"
        self.catalog = self.root / "providers.json"
        self.catalog.write_bytes(release.json_bytes(self.current))
        self.packages = self.root / ".build/packages"
        self.packages.mkdir(parents=True)
        self.output = self.root / ".build/release"
        self.archive = self.packages / f"openai-1.2.1-{PLATFORM}.zip"
        self.write_archive()

    def write_archive(self, extra=(), manifest=None, date=(2024, 1, 1, 0, 0, 0)):
        entries = [
            ("openai/nora-plugin.json", release.json_bytes(manifest or self.manifest), stat.S_IFREG | 0o644),
            ("openai/provider", b"fixture executable", stat.S_IFREG | 0o755),
            *extra,
        ]
        with zipfile.ZipFile(self.archive, "w", zipfile.ZIP_DEFLATED) as archive:
            for name, data, mode in entries:
                info = zipfile.ZipInfo(name, date_time=date)
                info.external_attr = mode << 16
                archive.writestr(info, data)
        entry = row(version="1.2.1", checksum=release.digest(self.archive))
        entry["packages"][PLATFORM]["url"] = f"{self.identity['release_base']}/{self.archive.name}"
        self.entry = entry
        (self.packages / "providers.json").write_bytes(release.json_bytes({
            "schemaVersion": 2, "providers": [entry, row("unrelated", "8.0.0")]}))
        (self.packages / "unrelated-8.0.0.zip").write_bytes(b"not selected")

    def prepare(self):
        return release.prepare(TAG, REPO, self.root, self.catalog, self.packages, self.output)


class FakeGitHub:
    def __init__(self, fixture):
        self.fixture = fixture
        self.catalog = copy.deepcopy(fixture.current)
        self.manifest = copy.deepcopy(fixture.manifest)
        self.current_release = None
        self.blobs = {}
        self.calls = []
        self.writes = []
        self.revision = 0
        self.conflicts = 0
        self.fail_upload = False
        self.fail_update = False
        self.corrupt_public = False
        self.move_tag = False
        self.advance_manifest = False
        self.fail_edit = False
        self.download_modes = []

    def verify_tag(self, tag, target):
        self.calls.append(("verify-tag", tag, target))
        release.require(not self.move_tag and tag == TAG and target == TARGET, "Tag moved")

    def release(self, tag):
        return copy.deepcopy(self.current_release)

    def content(self, path, branch):
        if path.startswith("packages/"):
            return release.json_bytes(self.manifest), "manifest-sha"
        return release.json_bytes(self.catalog), f"catalog-{self.revision}"

    def run(self, arguments, payload=None):
        self.calls.append(tuple(arguments))
        command = arguments[1]
        if command == "create":
            self.current_release = {"tag_name": TAG, "target_commitish": TARGET,
                                    "draft": True, "prerelease": False, "assets": []}
        elif command == "upload":
            if self.fail_upload:
                raise GitHubError("upload failed", 500)
            for path in arguments[5:]:
                path = Path(path)
                self.add_asset(path.name, path.read_bytes())
        elif command == "edit":
            if self.fail_edit:
                raise GitHubError("publish unavailable", 500)
            self.current_release["draft"] = False
            for asset in self.current_release["assets"]:
                asset["browser_download_url"] = f"{self.fixture.identity['release_base']}/{asset['name']}"
        else:
            raise AssertionError(arguments)
        return b""

    def add_asset(self, name, data):
        asset_id = len(self.blobs) + 1
        self.blobs[asset_id] = data
        self.current_release["assets"].append({
            "id": asset_id, "name": name, "size": len(data), "state": "uploaded",
            "browser_download_url": f"https://github.com/{REPO}/releases/download/untagged-draft/{name}"})

    def download(self, asset, public=False):
        self.download_modes.append(public)
        data = self.blobs[asset["id"]]
        if self.corrupt_public and not self.current_release["draft"] and asset["name"].endswith(".zip"):
            return data + b"corruption"
        return data

    def api(self, endpoint, method="GET", payload=None):
        if method == "DELETE":
            self.calls.append(("delete-draft-asset", endpoint))
            assert self.current_release["draft"]
            asset_id = int(endpoint.rsplit("/", 1)[1])
            self.current_release["assets"] = [asset for asset in self.current_release["assets"] if asset["id"] != asset_id]
            return None
        if method != "PUT":
            raise AssertionError((endpoint, method))
        self.writes.append(copy.deepcopy(payload))
        if self.fail_update:
            raise GitHubError("catalog unavailable", 500)
        if self.conflicts:
            self.conflicts -= 1
            self.revision += 1
            self.catalog["providers"][1] = row("other", f"4.0.{self.revision}", "c" * 64)
            self.catalog["providers"][1]["description"] = f"Concurrent update {self.revision}"
            self.catalog["providers"][1]["icon"] = "concurrent.svg"
            if self.advance_manifest:
                self.manifest["version"] = "1.2.2"
            raise GitHubError("catalog changed", 409)
        if payload["sha"] != f"catalog-{self.revision}":
            raise GitHubError("stale content SHA", 409)
        self.catalog = json.loads(base64.b64decode(payload["content"]))
        self.revision += 1
        return {"commit": {"sha": "committed"}}


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="nora-release-test-")
        self.addCleanup(self.temporary.cleanup)
        self.fixture = Fixture(self.temporary.name)

    def github(self):
        self.fixture.prepare()
        return FakeGitHub(self.fixture)

    def publish(self, github, **options):
        return publish(self.fixture.output, TAG, REPO, TARGET, github=github, **options)

    def test_validate_outputs_identity_for_workflow_and_marks_verifiable_reruns(self):
        output = self.fixture.root / "github-output"
        with redirect_stdout(io.StringIO()) as stdout:
            self.assertEqual(release.main(["validate", "--tag", TAG, "--repo", REPO,
                                          "--root", str(self.fixture.root), "--catalog", str(self.fixture.catalog),
                                          "--github-output", str(output)]), 0)
        self.assertEqual(json.loads(stdout.getvalue())["id"], "openai")
        self.assertIn("release_base=https://github.com/nora-release/plugins/releases/download/openai-v1.2.1\n",
                      output.read_text())
        self.fixture.current["providers"][0]["version"] = "1.2.1"
        self.fixture.catalog.write_bytes(release.json_bytes(self.fixture.current))
        self.assertTrue(release.validate(TAG, REPO, self.fixture.root, self.fixture.catalog)["rerun"])

    def test_invalid_tags_and_downgrades_fail(self):
        for tag in ("providers-v2.0.0", "openai-v1.02.1", "../openai-v1.2.1", "openai-v1.2.1-rc.1", "openai-v1.2"):
            with self.subTest(tag=tag), self.assertRaises(release.ReleaseError):
                release.validate(tag, REPO, self.fixture.root, self.fixture.catalog)
        self.fixture.current["providers"][0]["version"] = "1.3.0"
        self.fixture.catalog.write_bytes(release.json_bytes(self.fixture.current))
        with self.assertRaisesRegex(release.ReleaseError, "downgrade"):
            self.fixture.prepare()

    def test_tag_manifest_and_generated_catalog_must_agree(self):
        for field, value in (("id", "other"), ("version", "1.2.0"), ("apiVersion", 1), ("apiVersion", True)):
            with self.subTest(field=field, value=value):
                manifest = {**self.fixture.manifest, field: value}
                with self.assertRaises(release.ReleaseError):
                    release.check_manifest(manifest, self.fixture.identity)
        with self.assertRaisesRegex(release.ReleaseError, "Duplicate JSON"):
            release.parse_json('{"id":"openai","id":"other"}', "fixture")

    def test_prepare_copies_only_selected_archives_and_emits_verifiable_metadata(self):
        result = self.fixture.prepare()
        self.assertEqual(set(result["assets"]), {self.fixture.archive.name, "provider.json", "SHA256SUMS"})
        identity, entry, expected = release.load_bundle(self.fixture.output, TAG, REPO)
        self.assertEqual(identity["version"], "1.2.1")
        self.assertEqual(entry, self.fixture.entry)
        self.assertEqual(expected[self.fixture.archive.name], release.digest(self.fixture.archive))
        before = {path.name: path.stat().st_mtime_ns for path in self.fixture.output.iterdir()}
        self.fixture.prepare()
        self.assertEqual(before, {path.name: path.stat().st_mtime_ns for path in self.fixture.output.iterdir()})

    def test_prepare_rejects_bad_hash_or_another_release_url(self):
        self.fixture.entry["packages"][PLATFORM]["sha256"] = "f" * 64
        catalog = {"schemaVersion": 2, "providers": [self.fixture.entry]}
        (self.fixture.packages / "providers.json").write_bytes(release.json_bytes(catalog))
        with self.assertRaisesRegex(release.ReleaseError, "SHA256"):
            self.fixture.prepare()
        self.fixture.entry["packages"][PLATFORM]["url"] = "https://github.com/other/repo/releases/download/tag/archive.zip"
        (self.fixture.packages / "providers.json").write_bytes(release.json_bytes(catalog))
        with self.assertRaisesRegex(release.ReleaseError, "URL"):
            self.fixture.prepare()
        self.assertFalse(self.fixture.output.exists())

    def test_archives_reject_traversal_other_plugins_symlinks_and_case_collisions(self):
        cases = [
            [("openai/../outside", b"bad", stat.S_IFREG | 0o644)],
            [("/openai/absolute", b"bad", stat.S_IFREG | 0o644)],
            [("other/provider", b"bad", stat.S_IFREG | 0o644)],
            [("openai\\escape", b"bad", stat.S_IFREG | 0o644)],
            [("openai/link", b"../../outside", stat.S_IFLNK | 0o777)],
            [("openai/PROVIDER", b"duplicate", stat.S_IFREG | 0o755)],
        ]
        for extra in cases:
            with self.subTest(extra=extra):
                self.fixture.write_archive(extra)
                with self.assertRaises(release.ReleaseError):
                    self.fixture.prepare()
        self.fixture.write_archive(manifest={**self.fixture.manifest, "version": "9.0.0"})
        with self.assertRaisesRegex(release.ReleaseError, "Manifest version"):
            self.fixture.prepare()

    def test_release_bundle_detects_post_prepare_changes_and_extra_files(self):
        self.fixture.prepare()
        (self.fixture.output / "extra.zip").write_bytes(b"unexpected")
        with self.assertRaisesRegex(release.ReleaseError, "unexpected"):
            release.load_bundle(self.fixture.output)
        (self.fixture.output / "extra.zip").unlink()
        (self.fixture.output / self.fixture.archive.name).write_bytes(b"changed")
        with self.assertRaisesRegex(release.ReleaseError, "SHA256"):
            release.load_bundle(self.fixture.output)

    def test_merge_preserves_other_providers_and_their_optional_metadata(self):
        self.fixture.prepare()
        latest = copy.deepcopy(self.fixture.current)
        latest["providers"][1] = row("other", "4.5.0", "b" * 64)
        latest["providers"][1]["icon"] = "updated-icon.svg"
        merged, changed = release.merge_catalog(latest, self.fixture.entry)
        self.assertTrue(changed)
        self.assertEqual(merged["providers"][1], latest["providers"][1])
        self.assertEqual(latest["providers"][0]["version"], "1.2.0")
        identical, changed = release.merge_catalog(merged, self.fixture.entry)
        self.assertFalse(changed)
        self.assertIs(identical, merged)
        altered = copy.deepcopy(self.fixture.entry)
        altered["packages"][PLATFORM]["sha256"] = "d" * 64
        with self.assertRaisesRegex(release.ReleaseError, "immutable"):
            release.merge_catalog(merged, altered)

    def test_dry_run_never_initializes_github_or_touches_accounts(self):
        self.fixture.prepare()
        with patch("provider_release_github.GitHub", side_effect=AssertionError("No external operations")):
            result = publish(self.fixture.output, TAG, REPO, TARGET, dry_run=True)
        self.assertTrue(result["dry_run"])
        self.assertEqual(result["id"], "openai")

    def test_publish_verifies_downloads_then_merges_with_conflict_retry(self):
        github = self.github()
        github.conflicts = 1
        result = self.publish(github)
        self.assertTrue(result["published"])
        self.assertTrue(result["catalog_changed"])
        self.assertFalse(github.current_release["draft"])
        self.assertEqual(github.catalog["providers"][0], self.fixture.entry)
        self.assertEqual(github.catalog["providers"][1]["version"], "4.0.1")
        self.assertEqual(github.catalog["providers"][1]["description"], "Concurrent update 1")
        self.assertEqual(github.catalog["providers"][1]["icon"], "concurrent.svg")
        self.assertIn(True, github.download_modes)
        self.assertIn(False, github.download_modes)
        self.assertEqual([write["sha"] for write in github.writes], ["catalog-0", "catalog-1"])
        self.assertFalse(any("--clobber" in call for call in github.calls))

    def test_public_rerun_is_read_only_when_catalog_matches(self):
        github = self.github()
        self.publish(github)
        commands = len(github.calls)
        writes = len(github.writes)
        result = self.publish(github)
        self.assertTrue(result["rerun"])
        self.assertFalse(result["catalog_changed"])
        self.assertEqual(len(github.writes), writes)
        self.assertEqual(github.calls[commands:], [("verify-tag", TAG, TARGET)])

    def test_public_release_recovers_catalog_failure_with_original_zip_bytes(self):
        github = self.github()
        github.fail_update = True
        original_checksum = self.fixture.entry["packages"][PLATFORM]["sha256"]
        with self.assertRaisesRegex(GitHubError, "catalog unavailable"):
            self.publish(github)
        self.assertFalse(github.current_release["draft"])
        self.assertEqual(github.catalog, self.fixture.current)
        commands = len(github.calls)
        shutil_output = self.fixture.output
        for path in shutil_output.iterdir():
            path.unlink()
        shutil_output.rmdir()
        self.fixture.write_archive(date=(2025, 2, 2, 0, 0, 0))
        self.assertNotEqual(original_checksum, self.fixture.entry["packages"][PLATFORM]["sha256"])
        self.fixture.prepare()
        github.fail_update = False
        self.publish(github)
        self.assertEqual(github.catalog["providers"][0]["packages"][PLATFORM]["sha256"], original_checksum)
        self.assertEqual(github.calls[commands:], [("verify-tag", TAG, TARGET)])

    def test_upload_failure_leaves_draft_without_updating_main(self):
        github = self.github()
        github.fail_upload = True
        with self.assertRaisesRegex(GitHubError, "upload failed"):
            self.publish(github)
        self.assertTrue(github.current_release["draft"])
        self.assertEqual(github.writes, [])
        self.assertEqual(github.catalog, self.fixture.current)
        github.fail_upload = False
        self.assertTrue(self.publish(github)["published"])

    def test_conflicting_partial_draft_asset_is_deleted_only_while_still_draft(self):
        github = self.github()
        github.run(["release", "create", TAG])
        github.add_asset(self.fixture.archive.name, b"different existing asset")
        self.assertTrue(self.publish(github)["published"])
        self.assertTrue(any(call[0] == "delete-draft-asset" for call in github.calls))
        self.assertFalse(any("--clobber" in call for call in github.calls))

    def test_corrupt_public_download_never_updates_the_catalog(self):
        github = self.github()
        github.corrupt_public = True
        with self.assertRaisesRegex(release.ReleaseError, "SHA256"):
            self.publish(github)
        self.assertFalse(github.current_release["draft"])
        self.assertEqual(github.writes, [])

    def test_five_conflicts_stop_and_newer_main_manifest_allows_recovery(self):
        github = self.github()
        github.conflicts = 5
        with self.assertRaises(GitHubError):
            self.publish(github)
        self.assertEqual(len(github.writes), 5)
        self.assertEqual(github.catalog["providers"][0]["version"], "1.2.0")
        github.conflicts = 1
        github.advance_manifest = True
        self.publish(github)
        self.assertEqual(github.manifest["version"], "1.2.2")
        self.assertEqual(github.catalog["providers"][0]["version"], "1.2.1")

    def test_moved_tag_and_mismatched_public_release_commit_fail(self):
        github = self.github()
        github.move_tag = True
        with self.assertRaisesRegex(release.ReleaseError, "Tag moved"):
            self.publish(github)
        self.assertIsNone(github.current_release)
        github.move_tag = False
        self.publish(github)
        github.current_release["target_commitish"] = "b" * 40
        with self.assertRaisesRegex(release.ReleaseError, "metadata"):
            self.publish(github)

    def test_annotated_github_tag_is_dereferenced_to_exact_build_commit(self):
        github = GitHub(REPO)
        replies = [{"object": {"type": "tag", "sha": "b" * 40}},
                   {"object": {"type": "commit", "sha": TARGET}}]
        with patch.object(github, "api", side_effect=replies) as api:
            github.verify_tag(TAG, TARGET)
        self.assertEqual(api.call_count, 2)
        with patch.object(github, "api", return_value={"object": {"type": "commit", "sha": "c" * 40}}):
            with self.assertRaisesRegex(release.ReleaseError, "no longer points"):
                github.verify_tag(TAG, TARGET)

    def test_draft_lookup_paginates_after_published_tag_endpoint_returns_404(self):
        github = GitHub(REPO)
        draft = {"tag_name": TAG, "draft": True, "id": 17}
        replies = [GitHubError("HTTP 404", 404),
                   [{"tag_name": f"other-v1.0.{index}"} for index in range(100)], [draft]]
        with patch.object(github, "api", side_effect=replies) as api:
            self.assertEqual(github.release(TAG), draft)
        self.assertEqual(api.call_args_list[-1].args, ("releases?per_page=100&page=2",))

    def test_public_download_is_anonymous_has_a_timeout_and_retries_transient_errors(self):
        github = GitHub(REPO)
        asset = {"id": 1, "size": 3, "name": "fixture.zip",
                 "browser_download_url": "https://github.com/nora-release/plugins/releases/download/openai-v1.2.1/fixture.zip"}
        with patch("provider_release_github.build_opener") as opener, \
                patch("provider_release_github.time.sleep"), \
                patch.object(github, "run", side_effect=AssertionError("Public downloads cannot use authenticated gh")):
            opener.return_value.open.side_effect = [URLError("CDN not ready"), io.BytesIO(b"zip")]
            self.assertEqual(github.download(asset, public=True), b"zip")
        request = opener.return_value.open.call_args.args[0]
        self.assertFalse(request.has_header("Authorization"))
        self.assertEqual(opener.return_value.open.call_args.kwargs["timeout"], 30)
        self.assertEqual(opener.return_value.open.call_count, 2)

    def test_complete_draft_reuses_its_valid_bundle_after_rebuilding_same_tag(self):
        github = self.github()
        github.fail_edit = True
        original = self.fixture.entry["packages"][PLATFORM]["sha256"]
        with self.assertRaisesRegex(GitHubError, "publish unavailable"):
            self.publish(github)
        self.fixture.write_archive(date=(2025, 2, 2, 0, 0, 0))
        self.fixture.output = self.fixture.root / ".build/rebuilt-release"
        self.fixture.prepare()
        previous = len(github.calls)
        github.fail_edit = False
        self.publish(github)
        self.assertEqual(github.catalog["providers"][0]["packages"][PLATFORM]["sha256"], original)
        self.assertFalse(any(call[:2] == ("release", "upload") or call[0] == "delete-draft-asset"
                             for call in github.calls[previous:]))

    def test_unfinished_starter_upload_is_replaced_without_trying_to_download_it(self):
        github = self.github()
        github.run(["release", "create", TAG])
        github.add_asset(self.fixture.archive.name, b"")
        github.current_release["assets"][0]["state"] = "starter"
        download = github.download
        def uploaded_only(asset, public=False):
            self.assertEqual(asset["state"], "uploaded")
            return download(asset, public)
        with patch.object(github, "download", side_effect=uploaded_only):
            self.assertTrue(self.publish(github)["published"])
        self.assertTrue(any(call[0] == "delete-draft-asset" for call in github.calls))

    def test_public_rerun_with_new_build_bytes_and_already_current_catalog_is_a_noop(self):
        github = self.github()
        self.publish(github)
        self.fixture.catalog.write_bytes(release.json_bytes(github.catalog))
        self.fixture.write_archive(date=(2025, 2, 2, 0, 0, 0))
        self.fixture.output = self.fixture.root / ".build/rebuilt-release"
        self.fixture.prepare()
        previous = len(github.writes)
        self.assertFalse(self.publish(github)["catalog_changed"])
        self.assertEqual(len(github.writes), previous)

    def test_whole_catalog_matches_nora_schema_before_any_write(self):
        mutations = [
            lambda catalog: catalog.update(metadata={"unknown": True}),
            lambda catalog: catalog["providers"][1].pop("description"),
            lambda catalog: catalog["providers"][1].update(repository="file:///private"),
            lambda catalog: catalog["providers"][1].update(contributes=["unknown"]),
            lambda catalog: catalog["providers"][1].update(contributes=["provider", "provider"]),
            lambda catalog: catalog["providers"][1].update(secret="unknown"),
            lambda catalog: catalog["providers"][1]["packages"][PLATFORM].update(unknown=True),
        ]
        for mutate in mutations:
            catalog = copy.deepcopy(self.fixture.current)
            mutate(catalog)
            with self.subTest(catalog=catalog), self.assertRaises(release.ReleaseError):
                release.merge_catalog(catalog, self.fixture.entry)
        current, _ = release.merge_catalog(self.fixture.current, self.fixture.entry)
        modified = copy.deepcopy(self.fixture.entry)
        modified["packages"][PLATFORM]["url"] += "?different-source=1"
        with self.assertRaisesRegex(release.ReleaseError, "immutable"):
            release.merge_catalog(current, modified)


if __name__ == "__main__":
    unittest.main()
