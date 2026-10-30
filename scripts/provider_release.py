#!/usr/bin/env python3
"""Prepare and publish one immutable provider release using Python's stdlib."""

import argparse
import copy
import hashlib
import json
from pathlib import Path
import re
import shutil
import stat
import sys
import tempfile
import unicodedata
import zipfile
from urllib.parse import urlsplit

sys.dont_write_bytecode = True


class ReleaseError(Exception):
    pass


if __name__ == "__main__":
    sys.modules["provider_release"] = sys.modules[__name__]


ID = re.compile(r"[a-z0-9][a-z0-9._-]{0,63}\Z")
VERSION = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\Z")
PLATFORM = re.compile(r"[a-z0-9]+(?:[-_][a-z0-9]+)*\Z")
CHECKSUM = re.compile(r"[0-9a-f]{64}\Z")
MAX_PACKAGE_BYTES = 64 * 1024 * 1024
MAX_MANIFEST_BYTES = 256 * 1024
CONTRIBUTIONS = {"provider", "hooks", "middleware", "masker", "panels", "page", "actions"}
ENTRY_FIELDS = {"apiVersion", "minimumHostVersion", "id", "name", "icon", "description", "version",
                "repository", "contributes", "packages", "localSignIns"}


def text(value, limit):
    return (isinstance(value, str) and value.strip() and len(value.encode("utf-8")) <= limit
            and not any(unicodedata.category(char) == "Cc" for char in value))


def check_sign_ins(sign_ins):
    require(isinstance(sign_ins, list) and len(sign_ins) <= 8, "Invalid localSignIns list")
    for sign_in in sign_ins:
        require(isinstance(sign_in, dict) and set(sign_in) <= {"method", "label", "paths", "keychain", "inputs"},
                "Unknown local sign-in fields")
        require(text(sign_in.get("method"), 64) and text(sign_in.get("label"), 128), "Invalid local sign-in identity")
        paths, keychain = sign_in.get("paths", []), sign_in.get("keychain")
        require(isinstance(paths, list) and len(paths) <= 8 and (paths or keychain is not None),
                "Local sign-in needs file paths or a Keychain item")
        require(all(text(path, 1024) and (path.startswith("~/") or path.startswith("/"))
                    and ".." not in path.split("/") for path in paths), "Invalid local sign-in path")
        require(keychain is None or text(keychain, 128), "Invalid local sign-in Keychain item")
        inputs = sign_in.get("inputs", {})
        require(isinstance(inputs, dict) and len(inputs) <= 8
                and all(text(key, 64) and isinstance(value, str) and len(value.encode()) <= 1024
                        for key, value in inputs.items()), "Invalid local sign-in inputs")


def require(condition, message):
    if not condition:
        raise ReleaseError(message)


def version(value):
    match = VERSION.fullmatch(value) if isinstance(value, str) else None
    require(match is not None, f"Invalid stable version: {value!r}")
    require(all(len(part) <= 20 for part in match.groups()), "Version component is too large")
    result = tuple(int(part) for part in match.groups())
    require(all(part < 2**64 for part in result), "Version component is too large")
    return result


def identity(tag, repo):
    match = re.fullmatch(r"(.+)-v([0-9]+\.[0-9]+\.[0-9]+)", tag)
    require(match is not None and ID.fullmatch(match[1]), "Use a stable <plugin-id>-vX.Y.Z tag")
    version(match[2])
    require(
        re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9-]{0,38}/[A-Za-z0-9_.-]{1,100}", repo)
        and repo.split("/")[1] not in (".", ".."),
        "Repository must be owner/repository",
    )
    return {"id": match[1], "version": match[2], "tag": tag,
            "repo": repo, "release_base": f"https://github.com/{repo}/releases/download/{tag}"}


def parse_json(data, label):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, f"Duplicate JSON key in {label}: {key}")
            result[key] = value
        return result
    def invalid_constant(value):
        raise ReleaseError(f"Invalid JSON number in {label}: {value}")
    try:
        return json.loads(data, object_pairs_hook=unique, parse_constant=invalid_constant)
    except (ValueError, UnicodeError) as error:
        raise ReleaseError(f"Invalid JSON in {label}: {error}") from error


def read_json(path):
    try:
        return parse_json(Path(path).read_bytes(), str(path))
    except OSError as error:
        raise ReleaseError(f"Cannot read {path}: {error}") from error


def json_bytes(value):
    return (json.dumps(value, ensure_ascii=False, indent=2, allow_nan=False) + "\n").encode()


def check_manifest(manifest, release):
    require(isinstance(manifest, dict), "Plugin manifest must be an object")
    for key in ("id", "version"):
        require(manifest.get(key) == release[key], f"Manifest {key} does not match tag")
    require(type(manifest.get("apiVersion")) is int and manifest["apiVersion"] == 2,
            "Only apiVersion 2 packages can be released")


def catalog_entries(catalog):
    require(isinstance(catalog, dict) and type(catalog.get("schemaVersion")) is int
            and catalog["schemaVersion"] == 2,
            "Provider catalog must use schemaVersion 2")
    require(set(catalog) == {"schemaVersion", "providers"}, "Unknown provider catalog fields")
    rows = catalog.get("providers")
    require(isinstance(rows, list) and len(rows) <= 10_000 and len(json_bytes(catalog)) <= 1024 * 1024,
            "Provider catalog has no valid providers list or exceeds its size limit")
    by_id = {}
    for entry in rows:
        require(isinstance(entry, dict) and isinstance(entry.get("id"), str)
                and ID.fullmatch(entry["id"]), "Catalog contains an invalid provider ID")
        require(entry["id"] not in by_id, f"Duplicate catalog provider: {entry['id']}")
        require(set(entry) <= ENTRY_FIELDS, "Unknown catalog provider fields")
        version(entry.get("version"))
        require(type(entry.get("apiVersion")) is int and entry["apiVersion"] == 2,
                f"Catalog provider has an invalid apiVersion: {entry['id']}")
        require(text(entry.get("name"), 512),
                f"Catalog provider has no valid name: {entry['id']}")
        require(isinstance(entry.get("description"), str) and len(entry["description"].encode()) <= 4096,
                "Catalog provider has no valid description")
        require(entry.get("icon") is None or isinstance(entry["icon"], str), "Invalid provider icon")
        minimum = entry.get("minimumHostVersion")
        if minimum is not None:
            require(isinstance(minimum, str) and re.fullmatch(
                r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?", minimum),
                "Invalid minimumHostVersion")
        check_sign_ins(entry.get("localSignIns", []))
        repository = entry.get("repository")
        require(isinstance(repository, str), "Catalog provider has no repository URL")
        try:
            parsed_repository = urlsplit(repository)
            require(parsed_repository.scheme == "https" and parsed_repository.hostname
                    and not parsed_repository.username and not parsed_repository.password
                    and not any(char.isspace() for char in repository), "Invalid catalog repository URL")
        except ValueError as error:
            raise ReleaseError(f"Invalid catalog repository URL: {repository}") from error
        contributes = entry.get("contributes")
        require(isinstance(contributes, list) and contributes
                and all(isinstance(kind, str) and kind in CONTRIBUTIONS for kind in contributes)
                and len(set(contributes)) == len(contributes),
                "Catalog provider has no valid contributes list")
        packages = entry.get("packages")
        require(isinstance(packages, dict) and packages, f"Catalog provider has no packages: {entry['id']}")
        for platform, package in packages.items():
            require(isinstance(platform, str) and PLATFORM.fullmatch(platform)
                    and isinstance(package, dict) and set(package) == {"url", "sha256"},
                    "Invalid catalog package platform or fields")
            checksum, url = package.get("sha256"), package.get("url")
            require(isinstance(checksum, str) and CHECKSUM.fullmatch(checksum), "Invalid catalog package SHA256")
            require(isinstance(url, str) and not any(char.isspace() for char in url), "Invalid catalog package URL")
            try:
                parsed = urlsplit(url)
                require(parsed.scheme == "https" and parsed.hostname and parsed.path
                        and not parsed.username and not parsed.password and not parsed.fragment,
                        "Catalog package URLs must be absolute HTTPS URLs without credentials")
            except ValueError as error:
                raise ReleaseError(f"Invalid catalog package URL: {url}") from error
        by_id[entry["id"]] = entry
    return by_id


def check_upgrade(catalog, release):
    current = catalog_entries(catalog).get(release["id"])
    require(current is None or version(release["version"]) >= version(current["version"]),
            "A provider release cannot downgrade the published catalog")
    return current


def validate(tag, repo, root, catalog):
    release = identity(tag, repo)
    check_manifest(read_json(Path(root) / "packages" / release["id"] / "nora-plugin.json"), release)
    current = check_upgrade(read_json(catalog), release)
    return {**release, "rerun": current is not None and current["version"] == release["version"]}


def digest(path):
    hasher = hashlib.sha256()
    with Path(path).open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def safe_parts(name):
    require(isinstance(name, str) and name and "\\" not in name and "\0" not in name,
            f"Unsafe archive path: {name!r}")
    parts = name.removesuffix("/").split("/")
    require(len(parts) <= 64 and all(part not in ("", ".", "..") for part in parts),
            f"Unsafe archive path: {name!r}")
    return parts


def check_archive(path, release):
    path = Path(path)
    require(path.is_file() and not path.is_symlink(), f"Missing regular package: {path}")
    require(path.stat().st_size <= MAX_PACKAGE_BYTES, "Package archive exceeds 64 MiB")
    try:
        with zipfile.ZipFile(path) as archive:
            infos = archive.infolist()
            require(0 < len(infos) <= 10_000, "Invalid package file count")
            require(sum(info.file_size for info in infos) <= MAX_PACKAGE_BYTES,
                    "Uncompressed package exceeds 64 MiB")
            names = {}
            normalized = set()
            for info in infos:
                parts = safe_parts(info.orig_filename)
                require(parts[0] == release["id"], "Archive has the wrong package root")
                name = info.filename
                folded = unicodedata.normalize("NFC", name).casefold()
                require(folded not in normalized, f"Duplicate archive path: {name}")
                normalized.add(folded)
                mode = info.external_attr >> 16
                require(stat.S_IFMT(mode) in (0, stat.S_IFREG, stat.S_IFDIR),
                        f"Archive contains a symlink or special file: {name}")
                require(not info.flag_bits & 1, "Encrypted package files are not supported")
                names[name] = info
            manifest_path = f"{release['id']}/nora-plugin.json"
            info = names.get(manifest_path)
            require(info is not None and not info.is_dir() and info.file_size <= MAX_MANIFEST_BYTES,
                    "Archive has no valid nora-plugin.json")
            manifest = parse_json(archive.read(info), manifest_path)
            check_manifest(manifest, release)
            runtime = manifest.get("runtime")
            require(isinstance(runtime, dict), "Plugin manifest has no runtime")
            entry = runtime.get("entry")
            require(isinstance(entry, str), "Plugin manifest has no runtime entry")
            safe_parts(entry)
            executable = names.get(f"{release['id']}/{entry}")
            require(executable is not None and not executable.is_dir(), "Archive runtime entry is missing")
            require(archive.testzip() is None, "Package ZIP checksum is invalid")
    except (zipfile.BadZipFile, RuntimeError, OSError) as error:
        raise ReleaseError(f"Invalid archive {path.name}: {error}") from error


def package_files(entry, release):
    check_manifest(entry, release)
    packages = entry.get("packages")
    require(isinstance(packages, dict) and packages, "Provider has no release packages")
    result = {}
    for platform, package in packages.items():
        require(isinstance(platform, str) and PLATFORM.fullmatch(platform), "Invalid package platform")
        filename = f"{release['id']}-{release['version']}-{platform}.zip"
        require(isinstance(package, dict) and package.get("url") == f"{release['release_base']}/{filename}",
                f"Package URL does not match this release: {platform}")
        checksum = package.get("sha256")
        require(isinstance(checksum, str) and CHECKSUM.fullmatch(checksum), "Invalid package SHA256")
        result[filename] = checksum
    return result


def merge_catalog(catalog, entry):
    current = check_upgrade(catalog, entry)
    if current is not None and current["version"] == entry["version"]:
        require(current["packages"] == entry.get("packages"),
                "Published versions are immutable; bump the plugin version before changing its bytes")
        return catalog, False
    merged = copy.deepcopy(catalog)
    if current is None:
        merged["providers"].append(copy.deepcopy(entry))
    else:
        index = next(index for index, row in enumerate(merged["providers"]) if row["id"] == entry["id"])
        merged["providers"][index] = copy.deepcopy(entry)
    catalog_entries(merged)
    return merged, True


def prepare(tag, repo, root, current_catalog, packages_dir, output):
    release = validate(tag, repo, root, current_catalog)
    packages_dir, output = Path(packages_dir), Path(output)
    generated = catalog_entries(read_json(packages_dir / "providers.json"))
    entry = generated.get(release["id"])
    require(entry is not None, "Packaged catalog does not contain the selected plugin")
    files = package_files(entry, release)
    # Equal versions remain provisional until publish verifies the immutable
    # GitHub bundle: rebuilding the same tag may change ZIP timestamps.
    check_upgrade(read_json(current_catalog), entry)
    for filename, checksum in files.items():
        path = packages_dir / filename
        check_archive(path, release)
        require(digest(path) == checksum, f"Package SHA256 does not match catalog: {filename}")
    require(not output.is_symlink(), "Release output cannot be a symbolic link")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".provider-release-", dir=output.parent) as directory:
        staging = Path(directory) / "release"
        staging.mkdir()
        for filename, checksum in files.items():
            shutil.copyfile(packages_dir / filename, staging / filename)
            require(digest(staging / filename) == checksum, f"Package changed while copying: {filename}")
        (staging / "provider.json").write_bytes(json_bytes({"schemaVersion": 2, "providers": [entry]}))
        checksums = {**files, "provider.json": digest(staging / "provider.json")}
        (staging / "SHA256SUMS").write_text(
            "".join(f"{checksum}  {name}\n" for name, checksum in sorted(checksums.items())), encoding="utf-8")
        if output.exists():
            require(output.is_dir() and {path.name for path in output.iterdir()} == {path.name for path in staging.iterdir()},
                    "Release output already contains different files")
            require(all(path.is_file() and not path.is_symlink() and digest(path) == digest(staging / path.name)
                        for path in output.iterdir()), "Release output already contains a different release")
        else:
            staging.rename(output)
    return {**release, "directory": str(output), "assets": sorted([*files, "provider.json", "SHA256SUMS"])}


def load_bundle(directory, tag=None, repo=None):
    directory = Path(directory)
    catalog = read_json(directory / "provider.json")
    entries = catalog_entries(catalog)
    require(len(entries) == 1, "Release catalog must contain exactly one plugin")
    entry = next(iter(entries.values()))
    if tag is None:
        tag = f"{entry['id']}-v{entry['version']}"
    if repo is None:
        first = next(iter(entry.get("packages", {}).values()), {})
        match = re.fullmatch(r"https://github.com/([^/]+/[^/]+)/releases/download/[^/]+/[^/]+", first.get("url", ""))
        require(match is not None, "Release package has no GitHub download URL")
        repo = match[1]
    release = identity(tag, repo)
    files = package_files(entry, release)
    expected = {**files, "provider.json": digest(directory / "provider.json")}
    try:
        lines = (directory / "SHA256SUMS").read_text(encoding="utf-8").splitlines()
    except OSError as error:
        raise ReleaseError(f"Missing release checksums: {error}") from error
    declared = {}
    for line in lines:
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9_.-]+)", line)
        require(match is not None and match[2] not in declared, "Invalid or duplicate SHA256SUMS entry")
        declared[match[2]] = match[1]
    require(declared == expected, "Release checksums do not match its catalog")
    require({path.name for path in directory.iterdir()} == {*expected, "SHA256SUMS"},
            "Release directory contains unexpected files")
    for filename, checksum in expected.items():
        path = directory / filename
        require(path.is_file() and not path.is_symlink() and digest(path) == checksum,
                f"Release asset SHA256 does not match: {filename}")
        if filename.endswith(".zip"):
            check_archive(path, release)
    expected["SHA256SUMS"] = digest(directory / "SHA256SUMS")
    return release, entry, expected


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("validate", "prepare", "publish"):
        command = commands.add_parser(name)
        command.add_argument("--tag", required=True)
        command.add_argument("--repo", required=True)
        if name != "publish":
            command.add_argument("--root", default=".")
        if name == "validate":
            command.add_argument("--catalog", default="providers.json")
            command.add_argument("--github-output")
        elif name == "prepare":
            command.add_argument("--current-catalog", default="providers.json")
            command.add_argument("--packages-dir", required=True)
            command.add_argument("--output", required=True)
        else:
            command.add_argument("--release-dir", required=True)
            command.add_argument("--target", required=True)
            command.add_argument("--branch", default="main")
            command.add_argument("--catalog-path", default="providers.json")
            command.add_argument("--dry-run", action="store_true")
    command = commands.add_parser("merge")
    command.add_argument("--catalog", required=True)
    command.add_argument("--release-dir", required=True)
    command.add_argument("--output", required=True)
    args = parser.parse_args(argv)
    try:
        if args.command == "validate":
            result = validate(args.tag, args.repo, args.root, args.catalog)
            if args.github_output:
                with Path(args.github_output).open("a", encoding="utf-8") as output:
                    for key in ("id", "version", "tag", "release_base"):
                        output.write(f"{key}={result[key]}\n")
        elif args.command == "prepare":
            result = prepare(args.tag, args.repo, args.root, args.current_catalog, args.packages_dir, args.output)
        elif args.command == "merge":
            release, entry, _ = load_bundle(args.release_dir)
            original = Path(args.catalog).read_bytes()
            merged, changed = merge_catalog(parse_json(original, args.catalog), entry)
            if changed or Path(args.catalog).resolve() != Path(args.output).resolve():
                Path(args.output).write_bytes(json_bytes(merged) if changed else original)
            result = {**release, "changed": changed}
        else:
            from provider_release_github import publish
            result = publish(args.release_dir, args.tag, args.repo, args.target, args.branch, args.catalog_path, args.dry_run)
        print(json.dumps(result, sort_keys=True))
        return 0
    except (ReleaseError, OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
