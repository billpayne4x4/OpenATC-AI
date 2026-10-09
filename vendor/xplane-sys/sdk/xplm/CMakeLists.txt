# SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
#
# SPDX-License-Identifier: MIT

# Make sure anything wonky from outside doesn't affect finding XPLM
set(CMAKE_FIND_FRAMEWORK FIRST)

if(CMAKE_SYSTEM_NAME STREQUAL "Linux")
    add_subdirectory("src")
elseif(APPLE)
    find_library(
        XPLM_FW_PATH
        NAMES XPLM
        PATHS "${CMAKE_CURRENT_SOURCE_DIR}/Frameworks" REQUIRED
        NO_DEFAULT_PATH)
    add_library(XPlane::XPLM SHARED IMPORTED GLOBAL)
    set_target_properties(XPlane::XPLM PROPERTIES IMPORTED_LOCATION
                                                  "${XPLM_FW_PATH}")
    target_include_directories(XPlane::XPLM INTERFACE include)
    target_compile_definitions(XPlane::XPLM INTERFACE XPLM_LEVEL=${XPLM_LEVEL})
elseif(WIN32)
    find_library(
        XPLM_LIB_PATH
        NAMES "XPLM_64"
        PATHS "${CMAKE_CURRENT_SOURCE_DIR}/lib" REQUIRED
        NO_DEFAULT_PATH)
    add_library(XPlane::XPLM SHARED IMPORTED GLOBAL)
    set_target_properties(XPlane::XPLM PROPERTIES IMPORTED_LOCATION
                                                  "${XPLM_LIB_PATH}")
    target_include_directories(XPlane::XPLM INTERFACE include)
    target_compile_definitions(XPlane::XPLM INTERFACE XPLM_LEVEL=${XPLM_LEVEL})
endif()
