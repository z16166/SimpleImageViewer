# Overlay for cross arm64-from-x86_64 Docker (ubuntu:22.04): vcpkg's linux.cmake resolves
# CMAKE_CXX_COMPILER=aarch64-linux-gnu-g++. Versioned gcc-12 packages omit that basename;
# symlinks in scripts/ci/vcpkg_inner_jammy.sh supply it.
set(VCPKG_TARGET_ARCHITECTURE arm64)
set(VCPKG_CRT_LINKAGE dynamic)
set(VCPKG_LIBRARY_LINKAGE static)
set(VCPKG_CMAKE_SYSTEM_NAME Linux)
# Baseline -mcpu=cortex-a53: avoid optional ARMv8 features / SVE that SIGILL on older hardware.
# vcpkg-make uses VCPKG_MAKE_BUILD_TRIPLET for ./configure argv (not VCPKG_MAKE_*_TRIPLET alone).
# Some wrappers are not detected as aarch64-linux-gnu-gcc; pass --build/--host explicitly for autotools.
# Pass as a CMake list (use ';' not spaces) so OPTIONS ${BUILD_TRIPLET} expands to two configure argv.
set(VCPKG_MAKE_BUILD_TRIPLET "--build=x86_64-linux-gnu;--host=aarch64-linux-gnu")
if(NOT "$ENV{ZIG_CHAINLOAD_TOOLCHAIN}" STREQUAL "")
  set(VCPKG_CHAINLOAD_TOOLCHAIN_FILE "$ENV{ZIG_CHAINLOAD_TOOLCHAIN}")
endif()

if(NOT DEFINED VCPKG_CHAINLOAD_TOOLCHAIN_FILE)
  if("$ENV{VCPKG_CHAINLOAD_TOOLCHAIN_FILE}" STREQUAL "")
    set(VCPKG_CHAINLOAD_TOOLCHAIN_FILE "${CMAKE_CURRENT_LIST_DIR}/chainload-aarch64-linux.cmake")
  else()
    set(VCPKG_CHAINLOAD_TOOLCHAIN_FILE "$ENV{VCPKG_CHAINLOAD_TOOLCHAIN_FILE}")
  endif()
endif()
