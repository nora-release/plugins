#!/bin/bash

set -e

SCRIPT_DIR="$(dirname $0)"
cd "$SCRIPT_DIR/.."

mkdir -p resources/vm

if [ ! -f "resources/vm/kernel.bin" ]; then
  echo "Missing local kernel at resources/vm/kernel.bin"
  echo "Build it with: ./scripts/kbuild.sh"
  exit 1
fi

image_tag="$(date -u +%s)-$(openssl rand -hex 4)"
# Build using multi-stage Dockerfile (compiles vmd inside container)
docker build --platform linux/arm64 -f vmbuild/Dockerfile -t "sandbox-vm-vm:$image_tag" .
container_id="$(docker create "sandbox-vm-vm:$image_tag")"
rm -f resources/vm/rootfs.img.tmp
docker export "$container_id" > resources/vm/rootfs.tar.tmp
erofs_compression_args=(-zzstd,15)
if [ "$(uname -s)" = "Darwin" ]; then
  erofs_compression_args=()
fi
mkfs.erofs -b 4096 -C 65536 "${erofs_compression_args[@]}" --tar=f resources/vm/rootfs.img.tmp resources/vm/rootfs.tar.tmp
rm -f resources/vm/rootfs.tar.tmp
mv resources/vm/rootfs.img.tmp resources/vm/rootfs.img
docker rm "$container_id"
docker image rm "sandbox-vm-vm:$image_tag"

macos_resources_dir="${NORA_MACOS_RESOURCES_DIR:-../macos/Nora/Resources}"
if [ -d "$macos_resources_dir" ]; then
  cp resources/vm/rootfs.img "$macos_resources_dir/"
else
  echo "Skipping macOS rootfs copy; set NORA_MACOS_RESOURCES_DIR when the app target exists."
fi
