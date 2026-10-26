#!/bin/sh
set -eu
printf '%s | config=%s | browser=%s\n' "$*" "${CLOUDSDK_CONFIG-}" "${BROWSER-}" >> "$FAKE_GCLOUD_LOG"
if [ "${FAKE_GCLOUD_FAIL-}" = yes ]; then
    printf 'a-secret-must-never-appear-in-an-error\n' >&2
    exit 1
fi
case "$*" in
    'config get-value project'*) printf '%s\n' "${FAKE_GCLOUD_PROJECT-}" ;;
    'config get-value account'*) printf '%s\n' "${FAKE_GCLOUD_ACCOUNT-}" ;;
    'auth application-default login'*)
        printf 'Approve https://accounts.google.com/o/oauth2/auth?client_id=fixture&state=opaque'
        /bin/sleep 0.02
        printf '&redirect_uri=http%%3A%%2F%%2Flocalhost%%3A8765%%2Fcallback&scope=scope-a%%20scope-b\n' >&2
        if [ "${FAKE_LOGIN_WAIT-}" != '' ]; then
            while [ -f "$FAKE_LOGIN_WAIT" ]; do /bin/sleep 0.02; done
        fi
        if [ "${FAKE_LOGIN_READY-}" != '' ]; then
            while [ ! -f "$FAKE_LOGIN_READY" ]; do /bin/sleep 0.02; done
        fi
        if [ "${FAKE_LOGIN_NO_ADC-}" != yes ]; then
            /bin/mkdir -p "$CLOUDSDK_CONFIG"
            printf '{"type":"authorized_user","fixture":"new-adc"}' > "$CLOUDSDK_CONFIG/application_default_credentials.json"
        fi
        exit "${FAKE_LOGIN_EXIT-0}"
        ;;
    'auth application-default print-access-token'*)
        /bin/sleep 0.05
        owner=''
        if [ -f "${CLOUDSDK_CONFIG-}/application_default_credentials.json" ]; then
            owner=$(/usr/bin/sed -n 's/.*"owner":"\([^"]*\)".*/\1/p' "$CLOUDSDK_CONFIG/application_default_credentials.json")
        fi
        if [ "$owner" != '' ]; then
            nonce=$(/usr/bin/sed -n 's/.*"nonce":"\([^"]*\)".*/\1/p' "$CLOUDSDK_CONFIG/application_default_credentials.json")
            printf 'owner.%s~%s\n' "$owner" "$nonce"
        else printf '%s\n' "${FAKE_GCLOUD_TOKEN-fixture-adc-token}"; fi
        ;;
    'auth print-access-token'*) /bin/sleep 0.05;printf '%s\n' "${FAKE_GCLOUD_TOKEN-fixture-gcloud-token}" ;;
    *) exit 1 ;;
esac
