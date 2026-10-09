# SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
#
# SPDX-License-Identifier: MIT

enable_language(CXX)

add_library(
    wrappers STATIC
    src/XPCBroadcaster.cpp src/XPCDisplay.cpp src/XPCListener.cpp
    src/XPCProcessing.cpp src/XPCWidget.cpp src/XPCWidgetAttachments.cpp)
set_target_properties(wrappers PROPERTIES POSITION_INDEPENDENT_CODE TRUE)
target_include_directories(wrappers PUBLIC include)
target_link_libraries(wrappers PUBLIC XPlane::XPLM XPlane::XPWidgets)
