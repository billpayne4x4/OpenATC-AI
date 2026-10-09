# SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
#
# SPDX-License-Identifier: MIT

# There is no reason to make this a dynamic library.
# Static is the only sensible choice for this particular library.
xpwrappers_lib = static_library(
    'xp-wrappers',
    cpp_args: [xplm_lvl_ca],
    include_directories: [xpwrappers_inc],
    pic: true,
    dependencies: [xplm_dep, xpwidgets_dep],
    sources: files(
        'XPCBroadcaster.cpp',
        'XPCDisplay.cpp',
        'XPCListener.cpp',
        'XPCProcessing.cpp',
        'XPCWidget.cpp',
        'XPCWidgetAttachments.cpp',
    ),
)
