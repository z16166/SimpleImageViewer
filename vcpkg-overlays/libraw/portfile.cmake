vcpkg_from_github(
    OUT_SOURCE_PATH SOURCE_PATH
    REPO LibRaw/LibRaw
    REF "${VERSION}"
    SHA512 123050ea30366ada37b40e0aee84453f71f10a5e5e39261a1d16b96dc395f85a9ecdfd043d51b4c347a67546affdfa7ca84c10fa84d73b9b4070c074f1d301e8
    HEAD_REF master
)

vcpkg_from_github(
    OUT_SOURCE_PATH LIBRAW_CMAKE_SOURCE_PATH
    REPO LibRaw/LibRaw-cmake
    REF eb98e4325aef2ce85d2eb031c2ff18640ca616d3
    SHA512 63e68a4d30286ec3aa97168d46b7a1199268099ae27b61abcc92e93ec30e48d364086227983a1d724415e5f4da44d905422f30192453b95f31040e5f8469c3f9
    HEAD_REF master
    PATCHES
        dependencies.patch
        # Move the non-thread-safe library to manual-link. This is unfortunately needed
        # because otherwise libraries that build on top of libraw have to choose.
        fix-install.patch
)

file(COPY "${LIBRAW_CMAKE_SOURCE_PATH}/CMakeLists.txt" DESTINATION "${SOURCE_PATH}")
file(COPY "${LIBRAW_CMAKE_SOURCE_PATH}/cmake" DESTINATION "${SOURCE_PATH}")


vcpkg_check_features(OUT_FEATURE_OPTIONS FEATURE_OPTIONS
    FEATURES
        openmp      ENABLE_OPENMP
        openmp      CMAKE_REQUIRE_FIND_PACKAGE_OpenMP
        dng-lossy   CMAKE_REQUIRE_FIND_PACKAGE_JPEG
        x3f         ENABLE_X3FTOOLS
)

set(LIBRAW_CMAKE_OPTIONS
    ${FEATURE_OPTIONS}
    -DENABLE_EXAMPLES=OFF
    -DCMAKE_REQUIRE_FIND_PACKAGE_Jasper=1
    -DCMAKE_REQUIRE_FIND_PACKAGE_ZLIB=1
)

# Apple Clang ships without OpenMP; Homebrew libomp is required on macOS builders.
if(VCPKG_TARGET_IS_OSX)
    list(FIND FEATURES "openmp" _libraw_openmp_idx)
    if(_libraw_openmp_idx GREATER_EQUAL 0)
        if(DEFINED ENV{LIBOMP_PREFIX} AND EXISTS "$ENV{LIBOMP_PREFIX}")
            set(_libomp_prefix "$ENV{LIBOMP_PREFIX}")
        else()
            execute_process(
                COMMAND brew --prefix libomp
                OUTPUT_VARIABLE _brew_libomp_prefix
                OUTPUT_STRIP_TRAILING_WHITESPACE
                ERROR_QUIET
            )
            if(_brew_libomp_prefix AND EXISTS "${_brew_libomp_prefix}")
                set(_libomp_prefix "${_brew_libomp_prefix}")
            elseif(EXISTS "/opt/homebrew/opt/libomp")
                set(_libomp_prefix "/opt/homebrew/opt/libomp")
            elseif(EXISTS "/usr/local/opt/libomp")
                set(_libomp_prefix "/usr/local/opt/libomp")
            endif()
        endif()
        if(_libomp_prefix)
            if(EXISTS "${_libomp_prefix}/lib/libomp.a")
                set(_libomp_library "${_libomp_prefix}/lib/libomp.a")
            else()
                set(_libomp_library "${_libomp_prefix}/lib/libomp.dylib")
            endif()
            list(APPEND LIBRAW_CMAKE_OPTIONS
                "-DOpenMP_CXX_FLAGS=-Xpreprocessor -fopenmp -I${_libomp_prefix}/include"
                "-DOpenMP_C_FLAGS=-Xpreprocessor -fopenmp -I${_libomp_prefix}/include"
                "-DOpenMP_CXX_LIB_NAMES=omp"
                "-DOpenMP_C_LIB_NAMES=omp"
                "-DOpenMP_omp_LIBRARY=${_libomp_library}"
            )
        else()
            message(FATAL_ERROR "libraw openmp feature requires Homebrew libomp on macOS (brew install libomp).")
        endif()
    endif()
endif()

vcpkg_cmake_configure(
    SOURCE_PATH "${SOURCE_PATH}"
    OPTIONS
        ${LIBRAW_CMAKE_OPTIONS}
    MAYBE_UNUSED_VARIABLES
        CMAKE_REQUIRE_FIND_PACKAGE_OpenMP
)

vcpkg_cmake_install()
vcpkg_copy_pdbs()
vcpkg_cmake_config_fixup(CONFIG_PATH "lib/cmake")
vcpkg_fixup_pkgconfig()

if(VCPKG_LIBRARY_LINKAGE STREQUAL "static")
    vcpkg_replace_string("${CURRENT_PACKAGES_DIR}/include/libraw/libraw_types.h"
        "#ifdef LIBRAW_NODLL" "#if 1"
    )
else()
    vcpkg_replace_string("${CURRENT_PACKAGES_DIR}/include/libraw/libraw_types.h"
        "#ifdef LIBRAW_NODLL" "#if 0"
    )
endif()

file(COPY "${CURRENT_PACKAGES_DIR}/share/cmake/libraw/FindLibRaw.cmake" DESTINATION "${CURRENT_PACKAGES_DIR}/share/${PORT}")
file(REMOVE_RECURSE
    "${CURRENT_PACKAGES_DIR}/debug/include"
    "${CURRENT_PACKAGES_DIR}/debug/share"
    "${CURRENT_PACKAGES_DIR}/share/cmake"
    "${CURRENT_PACKAGES_DIR}/share/doc"
)

configure_file("${CMAKE_CURRENT_LIST_DIR}/vcpkg-cmake-wrapper.cmake" "${CURRENT_PACKAGES_DIR}/share/${PORT}/vcpkg-cmake-wrapper.cmake" @ONLY)
file(INSTALL "${CMAKE_CURRENT_LIST_DIR}/usage" DESTINATION "${CURRENT_PACKAGES_DIR}/share/${PORT}")
vcpkg_install_copyright(FILE_LIST
    "${SOURCE_PATH}/COPYRIGHT"
    "${SOURCE_PATH}/LICENSE.LGPL"
    "${SOURCE_PATH}/LICENSE.CDDL"
)
