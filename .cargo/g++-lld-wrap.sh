#!/usr/bin/env bash
# Rust/Cargo `linker` for Linux GNU targets: always drive the link through g++ with LLVM lld.
# rustc does not reliably place `-fuse-ld=lld` where GCC's collect2 will honor it when passed
# only via `cargo:rustc-link-arg`; this wrapper makes the choice unconditional.
# To use bfd ld instead: `export SIV_NO_LLD=1` (requires root build.rs trailing `libstdc++.a` passes).
set -euo pipefail
if [[ "${SIV_NO_LLD:-}" == "1" || "${SIV_NO_LLD:-}" == "true" || "${SIV_NO_LLD:-}" == "yes" ]]; then
  exec g++ "$@"
fi
exec g++ -fuse-ld=lld "$@"
