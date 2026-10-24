#!/usr/bin/env bash
# Rust/Cargo `linker` for Linux GNU targets: always drive the link through g++ with LLVM lld.
# rustc does not reliably place `-fuse-ld=lld` where GCC's collect2 will honor it when passed
# only via `cargo:rustc-link-arg`; this wrapper makes the choice unconditional.
# To use bfd ld instead: `export SIV_NO_LLD=1` (requires root build.rs trailing `libstdc++.a` passes).
#
# GTK / libappindicator (tray-icon) and some pkg-config --static lines emit `-lstdc++`.
# rustc then passes that as a dynamic lib after `-Bdynamic`, so the binary records
# DT_NEEDED `libstdc++.so.6` even when rustflags already have `-static-libstdc++`
# and build.rs already appends `libstdc++.a`. Drop those requests here; the static
# archive from the workspace build.rs remains on the link line.
set -euo pipefail

filtered=()
pending_dash_l=0
for arg in "$@"; do
  if [[ "${pending_dash_l}" -eq 1 ]]; then
    pending_dash_l=0
    case "${arg}" in
      stdc++ | stdc++.* | :libstdc++.so | :libstdc++.so.*)
        continue
        ;;
      *)
        filtered+=("-l" "${arg}")
        continue
        ;;
    esac
  fi
  case "${arg}" in
    -l)
      pending_dash_l=1
      ;;
    -lstdc++ | -lstdc++.* | -l:libstdc++.so | -l:libstdc++.so.*)
      ;;
    *)
      filtered+=("${arg}")
      ;;
  esac
done

if [[ "${SIV_NO_LLD:-}" == "1" || "${SIV_NO_LLD:-}" == "true" || "${SIV_NO_LLD:-}" == "yes" ]]; then
  exec g++ -static-libstdc++ -static-libgcc "${filtered[@]}"
fi
exec g++ -fuse-ld=lld -static-libstdc++ -static-libgcc "${filtered[@]}"
