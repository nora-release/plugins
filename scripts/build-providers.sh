#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Current tooling may build a previous source checkout; selection is optional
# so existing local workspace builds keep preparing every provider.
REPO_ROOT="$(cd "${NORA_PROVIDER_SOURCE_ROOT:-${SCRIPT_DIR}/..}" && pwd)"
PROVIDERS_ROOT="${REPO_ROOT}/packages"
MANIFEST_PATH="${REPO_ROOT}/Cargo.toml"
CARGO_BIN="${CARGO:-cargo}"
MODE="${1:-build}"
OUTPUT="${2:-${REPO_ROOT}/.build/providers}"
CARGO_COMMAND=("${CARGO_BIN}")
WORKSPACE_ARGS=(--manifest-path "${MANIFEST_PATH}")
PROVIDER_ID="${PROVIDER_ID:-}"
PROVIDER_PACKAGE="nora-provider"
PROVIDER_BINARY="nora-provider"
if [[ -n "${PROVIDER_ID}" ]]; then
  if [[ ! "${PROVIDER_ID}" =~ ^[a-z0-9][a-z0-9._-]{0,63}$ || ! -f "${PROVIDERS_ROOT}/${PROVIDER_ID}/nora-plugin.json" ]]; then
    echo "Choose an existing provider ID: ${PROVIDER_ID}" >&2
    exit 2
  fi
  PROVIDER_PACKAGE="$(python3 - "${PROVIDERS_ROOT}/${PROVIDER_ID}" "${PROVIDER_ID}" <<'PY'
import json
from pathlib import Path
import sys
import tomllib

source, selected = Path(sys.argv[1]), sys.argv[2]
if json.loads((source / "nora-plugin.json").read_text()).get("id") != selected:
    raise SystemExit("The selected provider directory must match its manifest ID.")
manifest = source / "Cargo.toml"
package = tomllib.loads(manifest.read_text())["package"]["name"] if manifest.is_file() else "nora-provider"
if not isinstance(package, str) or not package or any(character not in "abcdefghijklmnopqrstuvwxyz0123456789_-" for character in package):
    raise SystemExit("The selected provider has an invalid Cargo package name.")
print(package)
PY
)"
  PROVIDER_BINARY="${PROVIDER_PACKAGE}"
  WORKSPACE_ARGS+=(--package "${PROVIDER_PACKAGE}")
else
  WORKSPACE_ARGS+=(--workspace)
fi
# Builds against local checkouts instead of the pinned git revisions:
# NORA_PROVIDER_NORA_SOURCE for nora-ai, NORA_PLUGIN_KIT_SOURCE for the SDK.
patch_local() {
  local repository="$1" source="$2" package_name="$3" package_path="$4"
  if [[ ! -f "${source}/${package_path}/Cargo.toml" ]]; then
    echo "The local source ${source} must contain ${package_path}." >&2
    exit 1
  fi
  CARGO_COMMAND+=(--config "$(python3 -c 'import json,sys; print("patch." + json.dumps(sys.argv[1]) + "." + sys.argv[2] + ".path=" + json.dumps(sys.argv[3]))' "${repository}" "${package_name}" "${source}/${package_path}")")
}
if [[ -n "${NORA_PROVIDER_NORA_SOURCE:-}" ]]; then
  LOCAL_NORA_SOURCE="$(cd "${NORA_PROVIDER_NORA_SOURCE}" && pwd)"
  patch_local https://github.com/nora-release/nora.git "${LOCAL_NORA_SOURCE}" nora-ai packages/agents/ai
fi
if [[ -n "${NORA_PLUGIN_KIT_SOURCE:-}" ]]; then
  LOCAL_PLUGIN_KIT_SOURCE="$(cd "${NORA_PLUGIN_KIT_SOURCE}" && pwd)"
  for package_name in nora-provider-plugin-sdk nora-network nora-plugin-manifest; do
    patch_local https://github.com/nora-release/plugin-kit "${LOCAL_PLUGIN_KIT_SOURCE}" "${package_name}" "crates/${package_name}"
  done
fi
if [[ -z "${NORA_PROVIDER_NORA_SOURCE:-}${NORA_PLUGIN_KIT_SOURCE:-}" ]]; then
  WORKSPACE_ARGS+=(--locked)
fi

if [[ $# -gt 2 ]]; then
  echo "Usage: build-providers.sh [build|--test|--fmt|--fmt-check] [output-directory]" >&2
  exit 2
fi
case "${MODE}" in
  --test)
    "${CARGO_COMMAND[@]}" test "${WORKSPACE_ARGS[@]}"
    exit
    ;;
  --fmt-check)
    if [[ -n "${PROVIDER_ID}" ]]; then
      "${CARGO_COMMAND[@]}" fmt --manifest-path "${MANIFEST_PATH}" --package "${PROVIDER_PACKAGE}" --check
    else
      "${CARGO_COMMAND[@]}" fmt --manifest-path "${MANIFEST_PATH}" --all --check
    fi
    exit
    ;;
  --fmt)
    if [[ -n "${PROVIDER_ID}" ]]; then
      "${CARGO_COMMAND[@]}" fmt --manifest-path "${MANIFEST_PATH}" --package "${PROVIDER_PACKAGE}"
    else
      "${CARGO_COMMAND[@]}" fmt --manifest-path "${MANIFEST_PATH}" --all
    fi
    exit
    ;;
  build) ;;
  *)
    echo "Usage: build-providers.sh [build|--test|--fmt|--fmt-check] [output-directory]" >&2
    exit 2
    ;;
esac

case "$(basename "${OUTPUT}")" in
  /|.|..) echo "The provider output must name a dedicated directory." >&2; exit 2 ;;
esac
mkdir -p "$(dirname "${OUTPUT}")"
OUTPUT="$(cd "$(dirname "${OUTPUT}")" && pwd)/$(basename "${OUTPUT}")"
if [[ "${REPO_ROOT}/" == "${OUTPUT}/"* || "${OUTPUT}/" == "${PROVIDERS_ROOT}/"* || -e "${OUTPUT}/Cargo.toml" || -e "${OUTPUT}/.git" || -L "${OUTPUT}" ]]; then
  echo "The provider output must be a dedicated directory outside the source packages: ${OUTPUT}" >&2
  exit 2
fi

CONFIGURATION_NAME="${CONFIGURATION:-${BUILD_CONFIGURATION:-Debug}}"
PROFILE_DIR="debug"
BUILD_ARGS=(build "${WORKSPACE_ARGS[@]}" --bins)
if [[ -n "${PROVIDER_ID}" && "${PROVIDER_PACKAGE}" != nora-provider ]]; then
  # The selected provider supplies its executable; the shared CLI prepares its
  # manifest and catalog without compiling any other provider executable.
  BUILD_ARGS+=(--package nora-provider)
fi
case "${CONFIGURATION_NAME}" in
  Release|Staging)
    PROFILE_DIR="release"
    BUILD_ARGS+=(--release)
    ;;
esac

if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
  if [[ "${CARGO_TARGET_DIR}" = /* ]]; then
    CARGO_OUTPUT_ROOT="${CARGO_TARGET_DIR}"
  else
    CARGO_OUTPUT_ROOT="$(pwd)/${CARGO_TARGET_DIR}"
  fi
else
  CARGO_OUTPUT_ROOT="${REPO_ROOT}/target"
fi
export CARGO_TARGET_DIR="${CARGO_OUTPUT_ROOT}"
BINARY_ROOT="${CARGO_OUTPUT_ROOT}/${CARGO_BUILD_TARGET:+${CARGO_BUILD_TARGET}/}${PROFILE_DIR}"

"${CARGO_COMMAND[@]}" "${BUILD_ARGS[@]}"
STAGING_ROOT="$(mktemp -d "$(dirname "${OUTPUT}")/.providers-build.XXXXXX")"
BACKUP=""
cleanup() {
  if [[ -n "${BACKUP}" && -d "${BACKUP}" ]]; then
    if [[ ! -e "${OUTPUT}" ]]; then
      mv "${BACKUP}" "${OUTPUT}"
    else
      find "${BACKUP}" -depth -delete
    fi
  fi
  if [[ -d "${STAGING_ROOT}" ]]; then
    find "${STAGING_ROOT}" -depth -delete
  fi
}
trap cleanup EXIT
mkdir -p "${STAGING_ROOT}/runtime"

copy_binary() {
  if ! cp -c "$1" "$2" 2>/dev/null; then
    cp "$1" "$2"
  fi
  chmod 755 "$2"
}

stage_provider() {
  local binary_name="$1"
  local source_binary="${BINARY_ROOT}/${binary_name}"
  local staged_binary="${STAGING_ROOT}/runtime/${binary_name}"
  if [[ ! -f "${source_binary}" ]]; then
    echo "Missing provider executable: ${source_binary}" >&2
    return 1
  fi
  copy_binary "${source_binary}" "${staged_binary}"
  strip -S -x "${staged_binary}"
  if [[ "$(uname -s)" == "Darwin" ]]; then
    codesign --force --sign - --timestamp=none "${staged_binary}"
    codesign --verify --strict "${staged_binary}"
  fi
}

stage_provider nora-provider
DEFINITIONS="${PROVIDERS_ROOT}"
if [[ -n "${PROVIDER_ID}" ]]; then
  DEFINITIONS="${PROVIDERS_ROOT}/${PROVIDER_ID}/nora-plugin.json"
fi
"${STAGING_ROOT}/runtime/nora-provider" --prepare "${DEFINITIONS}" "${STAGING_ROOT}/providers"

PACKAGE_COUNT=0
for manifest in "${STAGING_ROOT}/providers"/*/nora-plugin.json; do
  [[ -f "${manifest}" ]] || continue
  package_dir="$(dirname "${manifest}")"
  provider_id="$(basename "${package_dir}")"
  binary_name="nora-provider"
  if [[ -n "${PROVIDER_ID}" ]]; then
    if [[ "${provider_id}" != "${PROVIDER_ID}" ]]; then
      echo "The prepared package does not match the selected provider: ${provider_id}" >&2
      exit 1
    fi
    binary_name="${PROVIDER_BINARY}"
    if [[ "${binary_name}" != nora-provider ]]; then
      stage_provider "${binary_name}"
    fi
  elif [[ -f "${PROVIDERS_ROOT}/${provider_id}/Cargo.toml" ]]; then
    binary_name="nora-provider-${provider_id}"
    stage_provider "${binary_name}"
  fi
  copy_binary "${STAGING_ROOT}/runtime/${binary_name}" "${package_dir}/provider"
  mkdir -p "${package_dir}/LICENSES"
  if [[ -f "${REPO_ROOT}/LICENSE" ]]; then
    cp "${REPO_ROOT}/LICENSE" "${package_dir}/LICENSES/Nora-Plugins-MIT.txt"
  fi
  for notice in "${REPO_ROOT}/LICENSES/"*.txt; do
    [[ -f "${notice}" ]] || continue
    cp "${notice}" "${package_dir}/LICENSES/$(basename "${notice}")"
  done
  PACKAGE_BYTES="$(wc -c < "${package_dir}/provider")"
  MANIFEST_BYTES="$(wc -c < "${manifest}")"
  PACKAGE_BYTES=$((PACKAGE_BYTES + MANIFEST_BYTES))
  if [[ ${PACKAGE_BYTES} -gt 67108864 ]]; then
    echo "The provider package exceeds the 64 MiB import limit: ${provider_id}" >&2
    exit 1
  fi
  PACKAGE_COUNT=$((PACKAGE_COUNT + 1))
done

if [[ ${PACKAGE_COUNT} -eq 0 ]]; then
  echo "No provider packages were prepared." >&2
  exit 1
fi
if [[ -e "${OUTPUT}" ]]; then
  BACKUP="${STAGING_ROOT}/previous"
  mv "${OUTPUT}" "${BACKUP}"
fi
mv "${STAGING_ROOT}/providers" "${OUTPUT}"
echo "Prepared ${PACKAGE_COUNT} provider packages in ${OUTPUT}"
