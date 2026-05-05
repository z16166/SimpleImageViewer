# Include vcpkg's Linux toolchain (cross flags). Do not FORCE gcc-* here — that overrides
# per-port compilers (e.g. libyuv uses clang-15). Docker symlinks aarch64-linux-gnu-* → *-10.
if(NOT "$ENV{VCPKG_ROOT}x" STREQUAL "x" AND EXISTS "$ENV{VCPKG_ROOT}/scripts/toolchains/linux.cmake")
  include("$ENV{VCPKG_ROOT}/scripts/toolchains/linux.cmake")
endif()
