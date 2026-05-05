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

# Bump base to Jammy to match vcpkg Jammy artifacts.
sed -i 's/^FROM ubuntu:20.04 AS cross-base/FROM ubuntu:22.04 AS cross-base/' "$DF"

# linux-image.sh pins linux-image-5.10.0-34-arm64; that Debian package rotates off mirrors.
# Use a wildcard kernel so bullseye still resolves an available linux-image-*-arm64 (same idea as armv7).
LINUX_IMG_SH="$TMP/docker/linux-image.sh"
if grep -Fq 'kernel="${kversion}-arm64"' "$LINUX_IMG_SH"; then
  sed -i 's/kernel="\${kversion}-arm64"/kernel='"'"'5.*-arm64'"'"'/' "$LINUX_IMG_SH"
else
  echo 'expected aarch64 kernel="${kversion}-arm64" in linux-image.sh; cross-rs may have changed'
  exit 1
fi

docker build -t "$IMAGE_TAG" -f "$DF" "$TMP/docker"

echo "Built $IMAGE_TAG"
