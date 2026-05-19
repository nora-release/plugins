#!/bin/bash

set -euxo pipefail

cd "$(dirname $0)/.."

# go 1.25.5-nora
GO_REVISION="5eb78c98996e90574f1abd8454e76dd97407a520"

if [ ! -f ./toolchain/bin/go ] || [ "$(cat ./toolchain/.git/FETCH_HEAD | awk '{print $1}')" != "$GO_REVISION" ]; then
  mkdir -p toolchain
  cd toolchain
  git init
  git remote remove origin || true
  go_toolchain_repo="${NORA_GO_TOOLCHAIN_REPO:-https://github.com/nora-release/go-toolchain}"
  git remote add origin "$go_toolchain_repo" || true
  git fetch --depth 1 origin "$GO_REVISION"
  git checkout FETCH_HEAD
  git reset --hard
  rm -rf bin
  cd src
  ./make.bash
  cd ../..
fi

export PATH="$PWD/toolchain/bin:$PATH"
export GOPATH="$PWD/toolchain"

if [ ! -f ./toolchain/bin/gomobile ] || [ ! -f ./toolchain/bin/gobind ]; then
  go install golang.org/x/mobile/cmd/gomobile@v0.0.0-20251021151156-188f512ec823
  go install golang.org/x/mobile/cmd/gobind@v0.0.0-20251021151156-188f512ec823
fi

mkdir -p dist
export MACOSX_DEPLOYMENT_TARGET=14.0
export CGO_CFLAGS="-mmacosx-version-min=14.0"
export CGO_LDFLAGS="-mmacosx-version-min=14.0"
gomobile bind -target=macos -trimpath -ldflags="-s -w" -o dist/SandboxVM.xcframework ./sdk/apple

macos_framework_dir="${NORA_MACOS_FRAMEWORK_DIR:-../macos}"
if [ -d "$macos_framework_dir" ]; then
  rm -rf "$macos_framework_dir/SandboxVM.xcframework"
  cp -a ./dist/SandboxVM.xcframework "$macos_framework_dir/"
else
  echo "Skipping macOS framework copy; set NORA_MACOS_FRAMEWORK_DIR when the app target exists."
fi
