#!/bin/bash

set -e

cd "$(dirname $0)/kbuild"

mkdir -p ../../dist ../../resources/vm

image_tag="$(date -u +%s)-$(openssl rand -hex 4)"
docker build -t "nora-kbuild:$image_tag" .
container_id="$(docker create "nora-kbuild:$image_tag")"
docker cp "$container_id:/opt/kernel.arm64" ../../dist/
docker rm "$container_id"
docker image rm "nora-kbuild:$image_tag"

cp ../../dist/kernel.arm64 ../../resources/vm/kernel.bin

macos_resources_dir="${NORA_MACOS_RESOURCES_DIR:-../../../macos/Nora/Resources}"
if [ -d "$macos_resources_dir" ]; then
  cp ../../dist/kernel.arm64 "$macos_resources_dir/kernel.bin"
else
  echo "Skipping macOS kernel copy; set NORA_MACOS_RESOURCES_DIR when the app target exists."
fi
