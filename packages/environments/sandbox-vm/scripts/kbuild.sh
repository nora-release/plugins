#!/bin/bash

set -e

cd "$(dirname $0)/kbuild"

docker_build_cached() {
  local cache_name="$1"
  shift

  if [ -z "${SANDBOXVM_DOCKER_CACHE_DIR:-}" ] || ! docker buildx version >/dev/null 2>&1; then
    docker build "$@"
    return
  fi

  local cache_dir="${SANDBOXVM_DOCKER_CACHE_DIR}/${cache_name}"
  local next_cache_dir="${SANDBOXVM_DOCKER_CACHE_DIR}/${cache_name}-next"
  local cache_args=()

  mkdir -p "$cache_dir"
  rm -rf "$next_cache_dir"

  if [ -f "$cache_dir/index.json" ]; then
    cache_args+=(--cache-from "type=local,src=${cache_dir}")
  fi
  cache_args+=(--cache-to "type=local,dest=${next_cache_dir},mode=max")

  docker buildx build --load "${cache_args[@]}" "$@"
  rm -rf "$cache_dir"
  mv "$next_cache_dir" "$cache_dir"
}

mkdir -p ../../dist ../../resources/vm

image_tag="$(date -u +%s)-$(openssl rand -hex 4)"
docker_build_cached kernel -t "nora-kbuild:$image_tag" .
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
