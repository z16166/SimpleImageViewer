#!/usr/bin/env bash
# Build a cross-rs aarch64-linux-gnu Docker image on Ubuntu 22.04 so the linker's glibc
# matches vcpkg static libs produced in ubuntu:22.04 (see vcpkg_inner_jammy.sh).
# Requires: docker, git. CROSS_GIT_REF must match the cross CLI ref in CI (release.yml).
set -euo pipefail

CROSS_GIT_REF="${CROSS_GIT_REF:-main}"
IMAGE_TAG="${CROSS_AARCH64_JAMMY_TAG:-cross-aarch64-jammy:local}"
TMP="${TMPDIR:-/tmp}/cross-src-$$"

cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

git clone --depth 1 --single-branch --branch "$CROSS_GIT_REF" https://github.com/cross-rs/cross "$TMP"

DF="$TMP/docker/Dockerfile.aarch64-unknown-linux-gnu"
if [[ ! -f "$DF" ]]; then
  echo "missing $DF"
  exit 1
fi

# cross-rs uses ubuntu:20.04 in the multi-stage Dockerfile; bump base to Jammy to match vcpkj Jammy artifacts.
sed -i 's/^FROM ubuntu:20.04 AS cross-base/FROM ubuntu:22.04 AS cross-base/' "$DF"

docker build -t "$IMAGE_TAG" -f "$DF" "$TMP/docker"

echo "Built $IMAGE_TAG"
