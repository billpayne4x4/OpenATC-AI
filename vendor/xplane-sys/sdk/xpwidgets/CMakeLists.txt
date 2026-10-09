# SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
#
# SPDX-License-Identifier: MIT

# Make sure anything wonky from outside doesn't affect finding XPLM
set(CMAKE_FIND_FRAMEWORK FIRST)

if(CMAKE_SYSTEM_NAME STREQUAL "Linux")
    add_subdirectory("src")
elseif(APPLE)
    find_library(
        XPWIDG_FW_PATH
        NAMES XPWidgets
        PATHS "${CMAKE_CURRENT_SOURCE_DIR}/Frameworks" REQUIRED
        NO_DEFAULT_PATH)
    add_library(XPlane::XPWidgets SHARED IMPORTED GLOBAL)
    set_target_properties(XPlane::XPWidgets PROPERTIES IMPORTED_LOCATION
                                                       "${XPWIDG_FW_PATH}")
    target_include_directories(XPlane::XPWidgets INTERFACE include)
    target_link_libraries(XPlane::XPWidgets INTERFACE XPlane::XPLM)
elseif(WIN32)
    find_library(
        XPWIDG_LIB_PATH
        NAMES "XPWidgets_64"
        PATHS "${CMAKE_CURRENT_SOURCE_DIR}/lib" REQUIRED
        NO_DEFAULT_PATH)
    add_library(XPlane::XPWidgets SHARED IMPORTED GLOBAL)
    set_target_properties(XPlane::XPWidgets PROPERTIES IMPORTED_LOCATION
                                                       "${XPWIDG_LIB_PATH}")
    target_include_directories(XPlane::XPWidgets INTERFACE include)
    target_link_libraries(XPlane::XPWidgets INTERFACE XPlane::XPLM)
endif()
