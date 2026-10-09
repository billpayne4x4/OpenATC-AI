# SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
#
# SPDX-License-Identifier: MIT

add_library(XPWidgets_64 SHARED XPWidgets.c XPWidgetUtils.c XPUIGraphics.c)

target_include_directories(XPWidgets_64 PUBLIC ../include)
target_compile_definitions(XPWidgets_64 PRIVATE XPWidgets XPLM_DEPRECATED
                                                XPLM_LEVEL=400)
target_link_libraries(XPWidgets_64 PUBLIC XPlane::XPLM)
set_target_properties(
    XPWidgets_64
    PROPERTIES PREFIX ""
               SUFFIX ".so"
               C_VISIBILITY_PRESET hidden)

add_library(XPlane::XPWidgets ALIAS XPWidgets_64)
