# SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
#
# SPDX-License-Identifier: MIT

add_library(
    XPLM_64 SHARED
    XPLMCamera.c
    XPLMDataAccess.c
    XPLMDisplay.c
    XPLMGraphics.c
    XPLMInstance.c
    XPLMMap.c
    XPLMMenus.c
    XPLMNavigation.c
    XPLMPlanes.c
    XPLMPlugin.c
    XPLMProcessing.c
    XPLMScenery.c
    XPLMSound.c
    XPLMUtilities.c
    XPLMWeather.c)

target_include_directories(XPLM_64 PUBLIC ../include)
target_compile_definitions(XPLM_64 PRIVATE XPLM XPLM_DEPRECATED XPLM_LEVEL=400)
target_compile_definitions(XPLM_64 INTERFACE XPLM_LEVEL=${XPLM_LEVEL})
set_target_properties(
    XPLM_64
    PROPERTIES PREFIX ""
               SUFFIX ".so"
               C_VISIBILITY_PRESET hidden)

add_library(XPlane::XPLM ALIAS XPLM_64)
