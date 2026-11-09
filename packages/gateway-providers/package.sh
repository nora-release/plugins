#!/usr/bin/env bash
# Packages one provider plugin with code as Nora installs it: its manifest
# and its program at the entry the manifest names.
#
#   packages/gateway-providers/package.sh <plugin> <output folder>
set -euo pipefail

plugin="${1:?name the plugin folder}"
output="${2:?the folder to write the package to}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
manifest="$here/$plugin/nora-plugin.json"
entry="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["runtime"]["entry"])' "$manifest")"

cargo build --release --manifest-path "$here/Cargo.toml" -p "nora-gateway-$plugin" --bin "$plugin"
rm -rf "$output"
mkdir -p "$output/$(dirname "$entry")"
cp "$manifest" "$output/nora-plugin.json"
# a cargo with its own target folder (huacnlee/cargo) builds into the
# workspace's ./target as well
cp "$here/target/release/$plugin" "$output/$entry"
strip -x "$output/$entry" 2>/dev/null || true
echo "$output"
