set(VCPKG_TARGET_ARCHITECTURE x64)
set(VCPKG_CRT_LINKAGE dynamic)
set(VCPKG_LIBRARY_LINKAGE static)
set(VCPKG_CMAKE_SYSTEM_NAME Linux)

# Overlay for static vcpkg dependencies on native Linux x64.
# Optional: ZIG_CHAINLOAD_X64_LINUX may point at a custom toolchain file to chainload.
# AlmaLinux 8 / host GCC builds: leave it unset — falls through to vcpkg linux.cmake.
if(NOT "$ENV{ZIG_CHAINLOAD_X64_LINUX}" STREQUAL "")
  set(VCPKG_CHAINLOAD_TOOLCHAIN_FILE "$ENV{ZIG_CHAINLOAD_X64_LINUX}")
endif()

if(NOT DEFINED VCPKG_CHAINLOAD_TOOLCHAIN_FILE)
  if("$ENV{VCPKG_CHAINLOAD_TOOLCHAIN_FILE}" STREQUAL "")
    # Use ENV: CMake variable VCPKG_ROOT is not always set when the triplet is parsed.
    set(VCPKG_CHAINLOAD_TOOLCHAIN_FILE "$ENV{VCPKG_ROOT}/scripts/toolchains/linux.cmake")
  else()
    set(VCPKG_CHAINLOAD_TOOLCHAIN_FILE "$ENV{VCPKG_CHAINLOAD_TOOLCHAIN_FILE}")
  endif()
endif()
