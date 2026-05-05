vcpkg_from_git(
    OUT_SOURCE_PATH SOURCE_PATH
    URL https://chromium.googlesource.com/libyuv/libyuv
    REF d98915a654d3564e4802a0004add46221c4e4348
    # Check https://chromium.googlesource.com/libyuv/libyuv/+/refs/heads/main/include/libyuv/version.h for a version!
    PATCHES
        cmake.diff
)

# Ubuntu focal cross gcc-10: cc1 rejects '+i8mm' in -march; GNU as still needs +i8mm for Neon usdot
# (see linux-arm64-libyuv-as.cmake). SVE2 row_sve.cc needs GCC 11+ / sve2 march — use LIBYUV_DISABLE_SVE.
if(VCPKG_TARGET_IS_LINUX AND VCPKG_TARGET_ARCHITECTURE STREQUAL "arm64")
    file(READ "${SOURCE_PATH}/CMakeLists.txt" _ly_cml)
    string(REPLACE "+dotprod+i8mm" "+dotprod" _ly_cml "${_ly_cml}")
    # Use [[ ]] so ${ly_lib_name} is not expanded by the portfile's CMake (variable is in libyuv's CMakeLists only).
    string(REPLACE [[target_compile_options(${ly_lib_name}_sve PRIVATE -march=armv8.5-a+i8mm+sve2)]]
                   [[target_compile_options(${ly_lib_name}_sve PRIVATE -march=armv8-a)]] _ly_cml "${_ly_cml}")
    string(REPLACE "-march=armv9-a+i8mm+sme" "-march=armv8-a" _ly_cml "${_ly_cml}")
    file(WRITE "${SOURCE_PATH}/CMakeLists.txt" "${_ly_cml}")
endif()

set(libyuv_extra_cmake_opts "")
if(VCPKG_TARGET_IS_LINUX AND VCPKG_TARGET_ARCHITECTURE STREQUAL "arm64")
    list(APPEND libyuv_extra_cmake_opts
        "-DCMAKE_PROJECT_YUV_INCLUDE=${CMAKE_CURRENT_LIST_DIR}/linux-arm64-libyuv-as.cmake")
endif()

vcpkg_check_features(OUT_FEATURE_OPTIONS FEATURE_OPTIONS
    FEATURES
        tools BUILD_TOOLS
)

vcpkg_cmake_configure(
    SOURCE_PATH "${SOURCE_PATH}"
    OPTIONS
        ${FEATURE_OPTIONS}
        ${libyuv_extra_cmake_opts}
    OPTIONS_DEBUG
        -DBUILD_TOOLS=OFF
)

vcpkg_cmake_install()
vcpkg_cmake_config_fixup()
if("tools" IN_LIST FEATURES)
    vcpkg_copy_tools(TOOL_NAMES yuvconvert yuvconstants AUTO_CLEAN)
endif()

if(VCPKG_LIBRARY_LINKAGE STREQUAL "dynamic")
    vcpkg_replace_string("${CURRENT_PACKAGES_DIR}/include/libyuv/basic_types.h" "defined(LIBYUV_USING_SHARED_LIBRARY)" "1")
endif()

file(REMOVE_RECURSE "${CURRENT_PACKAGES_DIR}/debug/include")
file(REMOVE_RECURSE "${CURRENT_PACKAGES_DIR}/debug/share")

file(COPY "${CMAKE_CURRENT_LIST_DIR}/libyuv-config.cmake" DESTINATION "${CURRENT_PACKAGES_DIR}/share/${PORT}")
file(COPY "${CMAKE_CURRENT_LIST_DIR}/usage" DESTINATION "${CURRENT_PACKAGES_DIR}/share/${PORT}")

vcpkg_cmake_get_vars(cmake_vars_file)
include("${cmake_vars_file}")
if(VCPKG_DETECTED_CMAKE_CXX_COMPILER_ID STREQUAL "MSVC")
    file(APPEND "${CURRENT_PACKAGES_DIR}/share/${PORT}/usage" [[

Attention:
You are using MSVC to compile libyuv. This build won't compile any
of the acceleration codes, which results in a very slow library.
See workarounds: https://github.com/microsoft/vcpkg/issues/28446
]])
endif()

vcpkg_install_copyright(FILE_LIST "${SOURCE_PATH}/LICENSE")
