#!/usr/bin/env bash
# Usage: ./sandbox.sh <command> [args...]

set -euo pipefail
[[ $# -ge 1 ]] || { echo "Usage: $0 <command> [args...]"; exit 1; }

CWD="$(pwd)"
PROFILE="$(mktemp -t vz-sbx.XXXXXX)"
cleanup(){ rm -f "$PROFILE"; }
trap cleanup EXIT

cat >"$PROFILE" <<EOF
(version 1)
(deny default)

;; Basics
(allow process*)
(allow sysctl-read)
(allow mach-task-name)

;; Filesystem: RO everywhere, RW only in \$PWD
(allow file-read*)
(deny file-read* (subpath "$HOME"))
(allow file-read* (subpath "$CWD"))
(allow file* (subpath "$CWD"))

;; REQUIRED for Virtualization.framework
(allow mach-lookup (global-name "com.apple.Virtualization.VirtualMachine"))
(allow mach-lookup (global-name "com.apple.vmnetd"))         ; if vmnet networking is used

;; Let Security framework talk to trust services (XPC).
(allow mach-lookup (global-name "com.apple.trustd.agent"))

(allow generic-issue-extension
  (extension-class "com.apple.virtualization.extension.fuse"))

;; Networking
(allow network*)
EOF

exec sandbox-exec -f "$PROFILE" -- "$@"
