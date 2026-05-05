#!/usr/bin/env bash
# Build a cross-rs aarch64-linux-gnu Docker image on Ubuntu 22.04 so the linker's glibc
# matches vcpkg static libs produced in ubuntu:22.04 (see vcpkg_inner_jammy.sh).
# Requires: docker, git. Clones cross-rs/default branch (same as plain `cargo install cross --git ...`).
set -euo pipefail

IMAGE_TAG="${CROSS_AARCH64_JAMMY_TAG:-cross-aarch64-jammy:local}"
TMP="${TMPDIR:-/tmp}/cross-src-$$"

cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

git clone --depth 1 https://github.com/cross-rs/cross "$TMP"

DF="$TMP/docker/Dockerfile.aarch64-unknown-linux-gnu"
if [[ ! -f "$DF" ]]; then
  echo "missing $DF"
  exit 1
fi

# bumps base to Jammy to match vcpkg Jammy artifacts.
sed -i 's/^FROM ubuntu:20.04 AS cross-base/FROM ubuntu:22.04 AS cross-base/' "$DF"

docker build -t "$IMAGE_TAG" -f "$DF" "$TMP/docker"

echo "Built $IMAGE_TAG"
