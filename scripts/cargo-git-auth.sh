#!/usr/bin/env bash
set +x
set +v
set -euo pipefail

askpass() {
  [[ $# -eq 1 && -n "${NORA_SOURCE_TOKEN:-}" ]] || return 1
  case "$1" in
    "Username for 'https://github.com/nora-release/nora': "|\
    "Username for 'https://github.com/nora-release/nora.git': ")
      printf '%s\n' x-access-token
      ;;
    "Password for 'https://github.com/nora-release/nora': "|\
    "Password for 'https://github.com/nora-release/nora.git': "|\
    "Password for 'https://x-access-token@github.com/nora-release/nora': "|\
    "Password for 'https://x-access-token@github.com/nora-release/nora.git': ")
      printf '%s\n' "$NORA_SOURCE_TOKEN"
      ;;
    *) return 1 ;;
  esac
}

if [[ "${1:-}" != -- ]]; then
  askpass "$@"
  exit
fi
shift
if [[ $# -eq 0 ]]; then
  printf '%s\n' 'Usage: cargo-git-auth.sh -- command [arguments...]' >&2
  exit 2
fi
if [[ -z "${NORA_SOURCE_TOKEN:-}" ]]; then
  printf '%s\n' 'NORA_SOURCE_TOKEN is required to read the private nora-ai dependency. Configure a token with read-only Contents access to nora-release/nora.' >&2
  exit 1
fi
case "$NORA_SOURCE_TOKEN" in
  *$'\r'*|*$'\n'*)
    printf '%s\n' 'NORA_SOURCE_TOKEN must contain one credential without line breaks.' >&2
    exit 1
    ;;
esac

# Keep these settings inside this process tree; no credentials or Git
# configuration are written to the checkout, user configuration or runner disk.
export CARGO_NET_GIT_FETCH_WITH_CLI=true GIT_TERMINAL_PROMPT=0 LC_ALL=C
export GIT_ASKPASS
GIT_ASKPASS="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
while IFS= read -r nora_trace_variable; do
  case "$nora_trace_variable" in
    GIT_TRACE*|GIT_CURL_VERBOSE) unset "$nora_trace_variable" ;;
  esac
done < <(compgen -e)

nora_git_count="${GIT_CONFIG_COUNT:-0}"
if [[ ${#nora_git_count} -gt 3 || "$nora_git_count" == *[!0-9]* || -z "$nora_git_count" ]]; then
  printf '%s\n' 'The existing process Git configuration is invalid.' >&2
  exit 1
fi
nora_git_count=$((10#$nora_git_count))
append_config() {
  local nora_git_key="GIT_CONFIG_KEY_${nora_git_count}"
  local nora_git_value="GIT_CONFIG_VALUE_${nora_git_count}"
  printf -v "$nora_git_key" '%s' "$1"
  printf -v "$nora_git_value" '%s' "$2"
  export "${nora_git_key?}" "${nora_git_value?}"
  nora_git_count=$((nora_git_count + 1))
}
append_config credential.helper ''
append_config credential.useHttpPath true
append_config credential.https://github.com/nora-release/nora.username x-access-token
append_config credential.https://github.com/nora-release/nora.git.username x-access-token
append_config http.followRedirects false
export GIT_CONFIG_COUNT="$nora_git_count"
exec "$@"
