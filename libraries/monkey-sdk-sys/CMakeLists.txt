# Simple Image Viewer - A high-performance, cross-platform image viewer
# Copyright (C) 2024-2026 Simple Image Viewer Contributors
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.

cmake_minimum_required(VERSION 3.15...4.2)
cmake_policy(SET CMP0091 NEW)

# Use static CRT for MSVC (Must be set before project())
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded")

project("Monkey's Audio SDK")

set(CMAKE_CXX_STANDARD 11)
set(CMAKE_CXX_EXTENSIONS OFF)

# The SDK root is passed from build.rs or assumed relative
if(NOT MONKEY_SDK_ROOT)
    set(MONKEY_SDK_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../../3rdparty/monkey-sdk")
endif()

message(STATUS "Using Monkey's Audio SDK from: ${MONKEY_SDK_ROOT}")

#
# Architecture detection
#
if(NOT ARCHITECTURE)
    set(archdetect_c_code "
    #if defined __i386__ || defined _M_IX86
        #error cmake_ARCH x86
    #elif defined __x86_64__ || defined _M_AMD64
        #error cmake_ARCH x86_64
    #elif defined __arm__ && (!defined __ARM_ARCH || __ARM_ARCH < 7 || !defined __ARM_FP)
        #error cmake_ARCH arm
    #elif (defined __arm__ && defined __ARM_ARCH && __ARM_ARCH >= 7 && defined __ARM_FP) || defined _M_ARM
        #error cmake_ARCH armhf
    #elif defined __aarch64__ || defined _M_ARM64
        #error cmake_ARCH aarch64
    #elif defined __riscv && __riscv_xlen == 32
        #error cmake_ARCH riscv
    #elif defined __riscv && __riscv_xlen == 64
        #error cmake_ARCH riscv64
    #elif defined __ppc64__ || defined __powerpc64__
        #error cmake_ARCH ppc64
    #elif defined __ppc__ || defined __powerpc__
        #error cmake_ARCH ppc
    #else
        #error cmake_ARCH unknown
    #endif
    ")
    file(WRITE "${CMAKE_BINARY_DIR}/arch_detect.c" "${archdetect_c_code}")
    enable_language(C)
    try_run(run_res compile_res "${CMAKE_BINARY_DIR}" "${CMAKE_BINARY_DIR}/arch_detect.c" COMPILE_OUTPUT_VARIABLE ARCH_OUT)
    string(REGEX MATCH "cmake_ARCH ([a-zA-Z0-9_]+)" ARCH_MATCH "${ARCH_OUT}")
    string(REPLACE "cmake_ARCH " "" ARCHITECTURE "${ARCH_MATCH}")
endif()

message(STATUS "Target Architecture: ${ARCHITECTURE}")

#
# Compile Options
#
if(MSVC)
  add_compile_options(/W3 /EHsc)
else()
  add_compile_options(-Wall -Wextra -fPIC -fvisibility=hidden)
endif()

#
# Source Files (Relative to MONKEY_SDK_ROOT)
#
set(MAC_SHARED_SOURCE_FILES
        ${MONKEY_SDK_ROOT}/Source/Shared/BufferIO.cpp
        ${MONKEY_SDK_ROOT}/Source/Shared/CharacterHelper.cpp
        ${MONKEY_SDK_ROOT}/Source/Shared/CircleBuffer.cpp
        ${MONKEY_SDK_ROOT}/Source/Shared/CPUFeatures.cpp
        ${MONKEY_SDK_ROOT}/Source/Shared/CRC.cpp
        ${MONKEY_SDK_ROOT}/Source/Shared/GlobalFunctions.cpp
        ${MONKEY_SDK_ROOT}/Source/Shared/MemoryIO.cpp
        ${MONKEY_SDK_ROOT}/Source/Shared/Semaphore.cpp
        ${MONKEY_SDK_ROOT}/Source/Shared/Thread.cpp
        ${MONKEY_SDK_ROOT}/Source/Shared/WholeFileIO.cpp)

if(WIN32)
    list(APPEND MAC_SHARED_SOURCE_FILES ${MONKEY_SDK_ROOT}/Source/Shared/WinFileIO.cpp)
else()
    list(APPEND MAC_SHARED_SOURCE_FILES ${MONKEY_SDK_ROOT}/Source/Shared/StdLibFileIO.cpp)
endif()

set(MAC_LIBRARY_SOURCE_FILES
        ${MONKEY_SDK_ROOT}/Source/MACLib/APECompress.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/APECompressCore.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/APECompressCreate.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/APEDecompress.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/APEDecompressCore.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/APEHeader.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/APEInfo.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/APELink.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/APETag.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/BitArray.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/FloatTransform.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/MACLib.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/MACProgressHelper.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/MD5.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/NewPredictor.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/NNFilter.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/NNFilterGeneric.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/Prepare.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/UnBitArray.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/UnBitArrayBase.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/WAVInputSource.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/Old/AntiPredictorOld.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/Old/AntiPredictorExtraHighOld.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/Old/AntiPredictorFastOld.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/Old/AntiPredictorHighOld.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/Old/AntiPredictorNormalOld.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/Old/APEDecompressCoreOld.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/Old/APEDecompressOld.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/Old/UnBitArrayOld.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/Old/UnMACOld.cpp)

# SIMD detection and file inclusion
if(${ARCHITECTURE} STREQUAL x86 OR ${ARCHITECTURE} STREQUAL x86_64)
    list(APPEND MAC_LIBRARY_SOURCE_FILES
        ${MONKEY_SDK_ROOT}/Source/MACLib/NNFilterAVX2.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/NNFilterAVX512.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/NNFilterSSE2.cpp
        ${MONKEY_SDK_ROOT}/Source/MACLib/NNFilterSSE4.1.cpp)
        
    if(MSVC)
        set_source_files_properties(${MONKEY_SDK_ROOT}/Source/MACLib/NNFilterAVX512.cpp PROPERTIES COMPILE_FLAGS "/arch:AVX512")
        set_source_files_properties(${MONKEY_SDK_ROOT}/Source/MACLib/NNFilterAVX2.cpp PROPERTIES COMPILE_FLAGS "/arch:AVX2")
    else()
        set_source_files_properties(${MONKEY_SDK_ROOT}/Source/MACLib/NNFilterAVX512.cpp PROPERTIES COMPILE_FLAGS "-mavx512dq -mavx512bw")
        set_source_files_properties(${MONKEY_SDK_ROOT}/Source/MACLib/NNFilterAVX2.cpp PROPERTIES COMPILE_FLAGS "-mavx2")
        set_source_files_properties(${MONKEY_SDK_ROOT}/Source/MACLib/NNFilterSSE4.1.cpp PROPERTIES COMPILE_FLAGS "-msse4.1")
        set_source_files_properties(${MONKEY_SDK_ROOT}/Source/MACLib/NNFilterSSE2.cpp PROPERTIES COMPILE_FLAGS "-msse2")
    endif()
endif()

if(${ARCHITECTURE} STREQUAL arm OR ${ARCHITECTURE} STREQUAL armhf OR ${ARCHITECTURE} STREQUAL aarch64)
    list(APPEND MAC_LIBRARY_SOURCE_FILES ${MONKEY_SDK_ROOT}/Source/MACLib/NNFilterNeon.cpp)
    if(${ARCHITECTURE} STREQUAL armhf AND NOT MSVC)
        set_source_files_properties(${MONKEY_SDK_ROOT}/Source/MACLib/NNFilterNeon.cpp PROPERTIES COMPILE_FLAGS "-march=armv7-a -mfpu=neon -mfloat-abi=hard")
    endif()
endif()

if(${ARCHITECTURE} STREQUAL riscv OR ${ARCHITECTURE} STREQUAL riscv64)
    list(APPEND MAC_LIBRARY_SOURCE_FILES ${MONKEY_SDK_ROOT}/Source/MACLib/NNFilterRVV.cpp)
endif()

if(${ARCHITECTURE} STREQUAL ppc OR ${ARCHITECTURE} STREQUAL ppc64)
    list(APPEND MAC_LIBRARY_SOURCE_FILES ${MONKEY_SDK_ROOT}/Source/MACLib/NNFilterAltiVec.cpp)
endif()

#
# Library Target
#
add_library(MAC STATIC ${MAC_SHARED_SOURCE_FILES} ${MAC_LIBRARY_SOURCE_FILES})

target_include_directories(MAC PUBLIC 
    ${MONKEY_SDK_ROOT}/Source/MACLib 
    ${MONKEY_SDK_ROOT}/Source/Shared
    ${MONKEY_SDK_ROOT}/Shared)

if(WIN32)
    target_compile_definitions(MAC PUBLIC PLATFORM_WINDOWS)
elseif(APPLE)
    target_compile_definitions(MAC PUBLIC PLATFORM_APPLE)
else()
    target_compile_definitions(MAC PUBLIC PLATFORM_LINUX)
    if(ANDROID)
        target_compile_definitions(MAC PUBLIC PLATFORM_ANDROID)
    endif()
endif()

install(TARGETS MAC ARCHIVE DESTINATION lib)
