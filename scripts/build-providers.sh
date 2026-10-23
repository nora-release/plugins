#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
PROVIDERS_ROOT="${REPO_ROOT}/packages"
MANIFEST_PATH="${REPO_ROOT}/Cargo.toml"
CARGO_BIN="${CARGO:-cargo}"
MODE="${1:-build}"
OUTPUT="${2:-${REPO_ROOT}/.build/providers}"

if [[ $# -gt 2 ]]; then
  echo "Usage: build-providers.sh [build|--test|--fmt|--fmt-check] [output-directory]" >&2
  exit 2
fi
case "${MODE}" in
  --test)
    "${CARGO_BIN}" test --manifest-path "${MANIFEST_PATH}" --workspace --locked
    exit
    ;;
  --fmt-check)
    "${CARGO_BIN}" fmt --manifest-path "${MANIFEST_PATH}" --all --check
    exit
    ;;
  --fmt)
    "${CARGO_BIN}" fmt --manifest-path "${MANIFEST_PATH}" --all
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
BUILD_ARGS=(build --manifest-path "${MANIFEST_PATH}" --workspace --bins --locked)
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

"${CARGO_BIN}" "${BUILD_ARGS[@]}"
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
"${STAGING_ROOT}/runtime/nora-provider" --prepare "${PROVIDERS_ROOT}" "${STAGING_ROOT}/providers"

PACKAGE_COUNT=0
for manifest in "${STAGING_ROOT}/providers"/*/nora-provider.json; do
  [[ -f "${manifest}" ]] || continue
  package_dir="$(dirname "${manifest}")"
  provider_id="$(basename "${package_dir}")"
  binary_name="nora-provider"
  if [[ -f "${PROVIDERS_ROOT}/${provider_id}/Cargo.toml" ]]; then
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
