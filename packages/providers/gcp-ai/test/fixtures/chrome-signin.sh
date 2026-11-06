#!/bin/sh
set -eu
profile=''
for arg in "$@"; do
    case "$arg" in --user-data-dir=*) profile=${arg#--user-data-dir=} ;; esac
done
printf '%s\n' "$*" > "$profile/signin-args"
while :; do /bin/sleep 0.1; done
