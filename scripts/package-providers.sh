#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
OUTPUT="${1:-${REPO_ROOT}/.build/packages}"
if [[ $# -gt 1 ]]; then
  echo "Usage: package-providers.sh [output-directory]" >&2
  exit 2
fi
case "$(basename "${OUTPUT}")" in
  /|.|..) echo "The archive output must name a dedicated directory." >&2; exit 2 ;;
esac
mkdir -p "$(dirname "${OUTPUT}")"
OUTPUT="$(cd "$(dirname "${OUTPUT}")" && pwd)/$(basename "${OUTPUT}")"
if [[ "${REPO_ROOT}/" == "${OUTPUT}/"* || "${OUTPUT}/" == "${REPO_ROOT}/packages/"* || -e "${OUTPUT}/Cargo.toml" || -e "${OUTPUT}/.git" || -L "${OUTPUT}" ]]; then
  echo "The archive output must be a dedicated directory outside the source packages: ${OUTPUT}" >&2
  exit 2
fi

RUSTC_BIN="${RUSTC:-rustc}"
HOST_TARGET="$("${RUSTC_BIN}" -vV | sed -n 's/^host: //p')"
if [[ ! "${HOST_TARGET}" =~ ^[A-Za-z0-9._-]+$ ]]; then
  echo "rustc did not report a valid host target." >&2
  exit 1
fi
STAGING_ROOT="$(mktemp -d "$(dirname "${OUTPUT}")/.providers-package.XXXXXX")"
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

CONFIGURATION=Release BUILD_CONFIGURATION=Release CARGO_BUILD_TARGET="${HOST_TARGET}" \
  bash "${SCRIPT_DIR}/build-providers.sh" build "${STAGING_ROOT}/providers"
mkdir -p "${STAGING_ROOT}/archives"
PACKAGE_COUNT=0
for manifest in "${STAGING_ROOT}/providers"/*/nora-provider.json; do
  [[ -f "${manifest}" ]] || continue
  package_dir="$(dirname "${manifest}")"
  provider_id="$(basename "${package_dir}")"
  version="$(awk -F '"' '/^  "version": "/ { print $4; exit }' "${manifest}")"
  if [[ ! "${version}" =~ ^[A-Za-z0-9._-]+$ ]]; then
    echo "The provider package has an invalid version: ${provider_id}" >&2
    exit 1
  fi
  archive="${STAGING_ROOT}/archives/${provider_id}-${version}-${HOST_TARGET}.zip"
  if command -v ditto >/dev/null 2>&1; then
    ditto -c -k --keepParent --norsrc --noextattr "${package_dir}" "${archive}"
  else
    (
      cd "$(dirname "${package_dir}")"
      zip -q -r "${archive}" "${provider_id}"
    )
  fi
  PACKAGE_COUNT=$((PACKAGE_COUNT + 1))
done
if [[ ${PACKAGE_COUNT} -eq 0 ]]; then
  echo "No provider archives were prepared." >&2
  exit 1
fi
(
  cd "${STAGING_ROOT}/archives"
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 ./*.zip > SHA256SUMS
  else
    sha256sum ./*.zip > SHA256SUMS
  fi
)
if [[ -e "${OUTPUT}" ]]; then
  BACKUP="${STAGING_ROOT}/previous"
  mv "${OUTPUT}" "${BACKUP}"
fi
mv "${STAGING_ROOT}/archives" "${OUTPUT}"
echo "Packaged ${PACKAGE_COUNT} provider archives for ${HOST_TARGET} in ${OUTPUT}"
