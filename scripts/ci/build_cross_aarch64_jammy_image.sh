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

# Skip cross-rs RUN /linux-image.sh aarch64:
# - That script points APT at Debian bullseye while the image still has Ubuntu Jammy ncurses-base
#   installed (6.3-2ubuntu0.1), so "apt-get download ncurses-base" pins a version that Debian
#   never publishes → build fails after a long prefetch.
# - Default cross aarch64-from-x86_64 runs binaries with qemu-*-user (/linux-runner), not qemu-system;
#   kernel/initrd under /qemu are only for qemu-system + dropbear SSH. Cargo build/link and typical
#   cross tests (e.g. user-mode QEMU) do not require them — see docker/linux-runner in cross-rs.

sed -i 's|^RUN /linux-image.sh aarch64$|RUN mkdir -p /qemu \&\& touch /qemu/kernel /qemu/initrd.gz|' "$DF"

docker build -t "$IMAGE_TAG" -f "$DF" "$TMP/docker"

echo "Built $IMAGE_TAG"
