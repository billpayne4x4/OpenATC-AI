# SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
#
# SPDX-License-Identifier: MIT

cmake_minimum_required(VERSION 3.12...3.27)
project(
    XPlaneSDK
    VERSION 4.0.1
    LANGUAGES C)

if(PROJECT_SOURCE_DIR STREQUAL PROJECT_BINARY_DIR)
    message(
        FATAL_ERROR "In-source builds are forbidden! Please make a build directory.")
endif()

if((CMAKE_SYSTEM_NAME STREQUAL "Linux") AND (CMAKE_SYSTEM_PROCESSOR STREQUAL "x86_64"
                                            ))
    message(TRACE "good target: linux-amd64")
elseif(APPLE AND (CMAKE_SYSTEM_PROCESSOR STREQUAL "x86_64"))
    message(TRACE "good target: darwin-amd64")
elseif(APPLE AND (CMAKE_SYSTEM_PROCESSOR STREQUAL "arm64"))
    message(TRACE "good target: darwin-aarch64")
elseif(
    WIN32
    AND (CMAKE_SYSTEM_PROCESSOR STREQUAL "AMD64")
    AND (CMAKE_SIZEOF_VOID_P EQUAL 8))
    message(TRACE "good target: windows-amd64")
else()
    message(
        FATAL_ERROR
            "Unsupported target: ${CMAKE_SYSTEM_NAME} on ${CMAKE_SYSTEM_PROCESSOR}!

The supported targets are:
- Linux on x86_64 (amd64)
- Darwin on x86_64 (amd64)
- Darwin on arm64 (aarch64)
- Windows on amd64 (x86_64)")
endif()

file(
    GENERATE
    OUTPUT .gitignore
    CONTENT "# Automatically placed in build directory by CMake script.\n*")

set(XPLM_LEVEL
    102
    CACHE STRING
          "XPLM compatibility level to use. Positive integer. See the documentation."
)

set(BUILD_WRAPPERS
    OFF
    CACHE BOOL "Build C++ wrappers for XPLM APIs")

if((XPLM_LEVEL GREATER 400) OR (XPLM_LEVEL LESS 102))
    message(
        FATAL_ERROR
            "Invalid XPLM compat level! Valid values are in the range [102, 400]")
endif()

add_subdirectory("xplm")
add_subdirectory("xpwidgets")

if(BUILD_WRAPPERS)
    add_subdirectory("xp-wrappers")
endif()
