"""GitHub operations for provider_release; all writes follow local validation."""

import base64
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile
import time
from urllib.error import HTTPError, URLError
from urllib.parse import quote, urlsplit
from urllib.request import HTTPRedirectHandler, Request, build_opener

from provider_release import (ReleaseError, check_manifest, json_bytes, load_bundle,
                              merge_catalog, parse_json, require, safe_parts, version)


class HTTPSRedirects(HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, url):
        require(urlsplit(url).scheme == "https", "Public assets must stay on HTTPS when redirected")
        return super().redirect_request(request, response, code, message, headers, url)


class GitHubError(ReleaseError):
    def __init__(self, message, status=None):
        super().__init__(message)
        self.status = status


class GitHub:
    def __init__(self, repo):
        self.repo = repo
        self.release_ids = {}
        self.pending_creations = set()

    def run(self, arguments, payload=None):
        try:
            result = subprocess.run(["gh", *arguments], input=payload, capture_output=True, check=False)
        except OSError as error:
            raise ReleaseError(f"Could not run GitHub CLI: {error}") from error
        if result.returncode:
            message = result.stderr.decode("utf-8", errors="replace").strip()
            match = re.search(r"HTTP (\d{3})", message)
            raise GitHubError(f"GitHub CLI failed: {message}", int(match[1]) if match else None)
        return result.stdout

    def api(self, endpoint, method="GET", payload=None):
        arguments = ["api", f"repos/{self.repo}/{endpoint}", "--method", method]
        encoded = None
        if payload is not None:
            arguments += ["--input", "-"]
            encoded = json_bytes(payload)
        data = self.run(arguments, encoded)
        return parse_json(data, f"GitHub {endpoint}") if data.strip() else None

    def release(self, tag):
        if tag in self.release_ids:
            for attempt in range(3):
                try:
                    return self.remember_release(self.api(f"releases/{self.release_ids[tag]}"), tag)
                except GitHubError as error:
                    if error.status != 404 or attempt == 2:
                        raise
                    # A known release may briefly be absent from a read replica.
                    # Keep its ID: losing it would allow a duplicate create.
                    time.sleep(attempt + 1)
        attempts = 3 if tag in self.pending_creations else 1
        for attempt in range(attempts):
            release = self.find_release(tag)
            if release is not None:
                return release
            if tag not in self.pending_creations:
                return None
            if attempt + 1 < attempts:
                time.sleep(attempt + 1)
        raise ReleaseError("Release creation status is unknown; no second draft was created. Rerun to check it again")

    def find_release(self, tag):
        try:
            return self.remember_release(self.api(f"releases/tags/{quote(tag, safe='')}"), tag)
        except GitHubError as error:
            if error.status != 404:
                raise
        # The tag endpoint only returns published releases. Drafts remain
        # visible to a contents:write token through the paginated release list.
        page = 1
        while True:
            releases = self.api(f"releases?per_page=100&page={page}")
            require(isinstance(releases, list), "GitHub returned no release list")
            matches = [item for item in releases if isinstance(item, dict) and item.get("tag_name") == tag]
            require(len(matches) <= 1, "GitHub returned multiple releases for this tag")
            if matches:
                return self.remember_release(matches[0], tag)
            if len(releases) < 100:
                return None
            page += 1

    def remember_release(self, value, tag):
        require(isinstance(value, dict) and value.get("tag_name") == tag
                and type(value.get("id")) is int and value["id"] > 0,
                "GitHub returned no valid release ID for this tag")
        previous = self.release_ids.get(tag)
        require(previous is None or previous == value["id"], "The release ID changed during publication")
        self.release_ids[tag] = value["id"]
        self.pending_creations.discard(tag)
        return value

    def create_release(self, tag, target, name, body):
        require(tag not in self.pending_creations and tag not in self.release_ids,
                "Release creation is already pending or complete; refusing to create a second draft")
        # A 5xx can hide a successful create. Until its ID is found, only reads
        # are safe, even when the tag and list indexes still report no release.
        self.pending_creations.add(tag)
        created = self.api("releases", "POST", {
            "tag_name": tag, "target_commitish": target, "name": name, "body": body,
            "draft": True, "prerelease": False,
        })
        return self.remember_release(created, tag)

    def content(self, path, branch):
        value = self.api(f"contents/{quote(path, safe='/')}?ref={quote(branch, safe='')}")
        require(isinstance(value, dict) and value.get("encoding") == "base64"
                and isinstance(value.get("sha"), str), f"GitHub returned no file for {path}")
        try:
            content = base64.b64decode("".join(value["content"].split()), validate=True)
        except (KeyError, ValueError) as error:
            raise ReleaseError(f"GitHub returned invalid file bytes for {path}") from error
        return content, value["sha"]

    def verify_tag(self, tag, target):
        value = self.api(f"git/ref/tags/{quote(tag, safe='')}").get("object", {})
        for _ in range(5):
            if value.get("type") == "commit":
                require(value.get("sha") == target, "The GitHub tag no longer points to the build commit")
                return
            require(value.get("type") == "tag" and re.fullmatch(r"[0-9a-f]{40}", value.get("sha", "")),
                    "GitHub returned an invalid tag reference")
            value = self.api(f"git/tags/{value['sha']}").get("object", {})
        raise ReleaseError("GitHub tag nesting is too deep")

    def download(self, asset, public=False):
        require(type(asset.get("id")) is int and asset["id"] > 0, "Release asset has no valid ID")
        require(type(asset.get("size")) is int and 0 <= asset["size"] <= 64 * 1024 * 1024,
                "Release asset exceeds the package size limit")
        if public:
            url = asset.get("browser_download_url", "")
            require(urlsplit(url).scheme == "https", "Published assets require HTTPS")
            request = Request(url, headers={"Accept": "application/octet-stream",
                                            "User-Agent": "nora-provider-release"})
            for attempt in range(3):
                try:
                    with build_opener(HTTPSRedirects()).open(request, timeout=30) as response:
                        data = response.read(asset["size"] + 1)
                    break
                except (URLError, OSError, TimeoutError) as error:
                    permanent = (isinstance(error, HTTPError) and error.code != 404
                                 and not 500 <= error.code < 600)
                    if permanent or attempt == 2:
                        raise ReleaseError(f"Public release download failed: {asset.get('name')}: {error}") from error
                    time.sleep(0.5 * (attempt + 1))
        else:
            data = self.run(["api", f"repos/{self.repo}/releases/assets/{asset['id']}",
                             "--header", "Accept: application/octet-stream"])
        require(len(data) == asset["size"], f"Incomplete release download: {asset.get('name')}")
        return data


def assets_for(release, identity, target):
    require(isinstance(release, dict) and release.get("tag_name") == identity["tag"],
            "GitHub release tag does not match the prepared release")
    require(release.get("target_commitish") == target,
            "Release metadata does not identify the build commit; refusing to reuse its assets")
    require(release.get("prerelease") is False and type(release.get("draft")) is bool,
            "A stable provider release cannot be a prerelease")
    assets = release.get("assets")
    require(isinstance(assets, list) and len(assets) <= 32, "Invalid release assets")
    result = {}
    for asset in assets:
        name = asset.get("name") if isinstance(asset, dict) else None
        require(isinstance(name, str) and re.fullmatch(r"[A-Za-z0-9_.-]+", name)
                and name not in result, "Unsafe or duplicate release asset name")
        if not release["draft"]:
            require(asset.get("browser_download_url") == f"{identity['release_base']}/{name}",
                    "Release asset has an unexpected public download URL")
        result[name] = asset
    return result


def published_entry(github, release, identity, target):
    assets = assets_for(release, identity, target)
    require("provider.json" in assets and "SHA256SUMS" in assets, "Published release metadata is incomplete")
    require(all(asset.get("state") == "uploaded" for asset in assets.values()),
            "Release contains unfinished asset uploads")
    with tempfile.TemporaryDirectory(prefix="nora-published-provider-") as directory:
        for name, asset in assets.items():
            (Path(directory) / name).write_bytes(github.download(asset, public=not release["draft"]))
        _, entry, _ = load_bundle(directory, identity["tag"], identity["repo"])
        return entry


def current_catalog(github, identity, branch, path):
    manifest, _ = github.content(f"packages/{identity['id']}/nora-plugin.json", branch)
    manifest = parse_json(manifest, "main plugin manifest")
    require(isinstance(manifest, dict), "Main plugin manifest must be an object")
    check_manifest(manifest, {**identity, "version": manifest.get("version")})
    require(version(manifest.get("version")) >= version(identity["version"]),
            "Main plugin manifest is older than the release")
    content, sha = github.content(path, branch)
    return parse_json(content, "main provider catalog"), sha


def update_catalog(github, identity, entry, branch, path):
    for attempt in range(5):
        catalog, sha = current_catalog(github, identity, branch, path)
        merged, changed = merge_catalog(catalog, entry)
        if not changed:
            return False
        try:
            github.api(f"contents/{quote(path, safe='/')}", "PUT", {
                "message": f"Release {identity['id']} v{identity['version']}",
                "branch": branch, "sha": sha,
                "content": base64.b64encode(json_bytes(merged)).decode("ascii"),
            })
            return True
        except GitHubError as error:
            if error.status != 409 or attempt == 4:
                raise
    raise ReleaseError("The provider catalog changed too often; rerun this release")


def repair_draft(github, release, identity, target, directory, expected):
    assets = assets_for(release, identity, target)
    for name, asset in assets.items():
        matches = (asset.get("state") == "uploaded" and name in expected
                   and hashlib.sha256(github.download(asset)).hexdigest() == expected[name])
        if matches:
            continue
        # Re-read immediately before any deletion; public assets are immutable.
        latest = github.release(identity["tag"])
        current = assets_for(latest, identity, target)
        require(latest["draft"], "Release became public; its assets will not be overwritten")
        require(name in current and current[name]["id"] == asset["id"], "Draft asset changed; retry the release")
        github.api(f"releases/assets/{asset['id']}", "DELETE")
    latest = github.release(identity["tag"])
    assets = assets_for(latest, identity, target)
    require(latest["draft"], "Release became public; rerun to verify it")
    missing = sorted(set(expected) - set(assets))
    if missing:
        github.run(["release", "upload", identity["tag"], "--repo", identity["repo"],
                    *(str(Path(directory) / name) for name in missing)])


def publish(directory, tag, repo, target, branch="main", catalog_path="providers.json", dry_run=False,
            github=None):
    identity, candidate, expected = load_bundle(directory, tag, repo)
    require(isinstance(target, str) and re.fullmatch(r"[0-9a-f]{40}", target), "Build target must be a full commit SHA")
    require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._/-]*", branch) is not None,
            "Invalid catalog branch")
    safe_parts(branch)
    safe_parts(catalog_path)
    if dry_run:
        return {**identity, "dry_run": True, "target": target,
                "catalog": f"{branch}:{catalog_path}", "assets": sorted(expected)}
    github = github or GitHub(repo)
    for attempt in range(3):
        try:
            return publish_once(github, identity, candidate, expected, directory, target, branch, catalog_path)
        except GitHubError as error:
            if error.status is None or not 500 <= error.status < 600 or attempt == 2:
                raise
            # A failed HTTP response does not mean the write failed. Re-read
            # the tag, release, assets and catalog before attempting any write.
            time.sleep(attempt + 1)


def publish_once(github, identity, candidate, expected, directory, target, branch, catalog_path):
    tag, repo = identity["tag"], identity["repo"]
    github.verify_tag(tag, target)
    release = github.release(tag)
    rerun = release is not None and release.get("draft") is False
    if rerun:
        # Rebuilding one commit can change ZIP timestamps. The already public
        # bundle is authoritative; validate its bytes instead of replacing it.
        entry = published_entry(github, release, identity, target)
    else:
        if release is None:
            catalog, _ = current_catalog(github, identity, branch, catalog_path)
            merge_catalog(catalog, candidate)
            release = github.create_release(
                tag, target, f"{candidate.get('name', identity['id'])} v{identity['version']}",
                f"Provider plugin {identity['id']} {identity['version']}.")
        assets_for(release, identity, target)
        require(release["draft"], "Release became public while preparing its uploads; rerun to verify it")
        try:
            selected = published_entry(github, release, identity, target)
        except GitHubError:
            raise
        except ReleaseError:
            selected = candidate
            catalog, _ = current_catalog(github, identity, branch, catalog_path)
            merge_catalog(catalog, selected)
            repair_draft(github, release, identity, target, directory, expected)
        else:
            catalog, _ = current_catalog(github, identity, branch, catalog_path)
            merge_catalog(catalog, selected)
        # Verify the draft as a complete package before making it immutable.
        draft = github.release(tag)
        published_entry(github, draft, identity, target)
        github.run(["release", "edit", tag, "--repo", repo, "--draft=false"])
        release = github.release(tag)
        require(release is not None and release.get("draft") is False, "Release was not published")
        entry = published_entry(github, release, identity, target)
    changed = update_catalog(github, identity, entry, branch, catalog_path)
    return {**identity, "target": target, "published": True, "rerun": rerun,
            "catalog_changed": changed}
