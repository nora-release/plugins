#!/bin/bash
set -euo pipefail

plugin_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
plugin_configuration="${NORA_PLUGIN_CONFIGURATION:-debug}"
plugin_signing_identity="${NORA_PLUGIN_SIGNING_IDENTITY:--}"
[[ "$plugin_configuration" == debug || "$plugin_configuration" == release ]] || {
    printf 'NORA_PLUGIN_CONFIGURATION must be debug or release.\n' >&2
    exit 2
}

swift build --package-path "$plugin_root" --configuration "$plugin_configuration" --product PluginHost
swift build --package-path "$plugin_root" --configuration "$plugin_configuration" --product PluginExtension
plugin_binaries="$(swift build --package-path "$plugin_root" --configuration "$plugin_configuration" --show-bin-path)"
plugin_staging="$(mktemp -d "$plugin_root/.build/github-app.XXXXXX")"
trap 'rm -rf "$plugin_staging"' EXIT

python3 - "$plugin_root" "$plugin_binaries" "$plugin_staging" <<'PY'
import json
import os
import pathlib
import plistlib
import re
import shutil
import sys

root, binaries, staging = map(pathlib.Path, sys.argv[1:])
manifest_path = root / 'nora-plugin.json'
manifest = json.loads(manifest_path.read_text())
client_id = os.environ.get('NORA_GITHUB_CLIENT_ID', 'Iv23liPZy7EARoSnYoYy').strip()
if client_id and not re.fullmatch(r'[A-Za-z0-9_-]{1,128}', client_id):
    raise SystemExit('NORA_GITHUB_CLIENT_ID must be a public GitHub App Client ID.')
app = staging / 'NoraGitHub.app'
extension = app / 'Contents/Extensions/NativePlugin.appex'
common = {
    'CFBundleDevelopmentRegion': 'en',
    'CFBundleInfoDictionaryVersion': '6.0',
    'CFBundleShortVersionString': manifest['version'],
    'CFBundleVersion': '1',
    'CFBundleName': manifest['name'],
    'CFBundleDisplayName': manifest['name'],
    'LSMinimumSystemVersion': '14.0',
}
if client_id:
    common['NoraGitHubClientID'] = client_id
for bundle, product, identifier, attributes in (
    (app, 'PluginHost', manifest['id'], {
        'CFBundlePackageType': 'APPL',
        'NSPrincipalClass': 'NSApplication',
        'LSUIElement': True,
    }),
    (extension, 'PluginExtension', manifest['extensionBundleIdentifier'], {
        'CFBundlePackageType': 'XPC!',
        'EXAppExtensionAttributes': {
            'EXExtensionPointIdentifier': 'com.nora.plugin.native',
            'EXExtensionPrincipalClass': 'PluginExtension.GitHubPlugin',
        },
    }),
):
    executable_directory = bundle / 'Contents/MacOS'
    resources = bundle / 'Contents/Resources'
    executable_directory.mkdir(parents=True)
    resources.mkdir()
    (bundle / 'Contents/Info.plist').write_bytes(plistlib.dumps(dict(
        common, **attributes, CFBundleIdentifier=identifier, CFBundleExecutable=product
    )))
    shutil.copy2(binaries / product, executable_directory / product)
    if product == 'PluginExtension':
        for resource in binaries.glob('*.bundle'):
            shutil.copytree(resource, resources / resource.name)
shutil.copy2(manifest_path, app / 'Contents/Resources/nora-plugin.json')
shutil.copytree(root / 'assets', app / 'Contents/Resources/assets')
(staging / 'sandbox.entitlements').write_bytes(plistlib.dumps({
    'com.apple.security.app-sandbox': True,
    'com.apple.security.network.client': True,
}))
PY

plugin_signing_options=(--force --sign "$plugin_signing_identity" --options runtime --entitlements "$plugin_staging/sandbox.entitlements")
if [[ "$plugin_signing_identity" != - ]]; then
    plugin_signing_options+=(--timestamp)
fi
codesign "${plugin_signing_options[@]}" "$plugin_staging/NoraGitHub.app/Contents/Extensions/NativePlugin.appex"
codesign "${plugin_signing_options[@]}" "$plugin_staging/NoraGitHub.app"
codesign --verify --deep --strict "$plugin_staging/NoraGitHub.app"

plugin_destination="$plugin_root/.build/NoraGitHub.app"
rm -rf "$plugin_destination"
mv "$plugin_staging/NoraGitHub.app" "$plugin_destination"
printf 'App: %s\n' "$plugin_destination"
