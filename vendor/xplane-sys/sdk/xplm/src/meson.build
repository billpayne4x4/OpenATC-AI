# SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
#
# SPDX-License-Identifier: MIT

xplm_stub = shared_library(
    'XPLM_64',
    files(
        'XPLMCamera.c',
        'XPLMDataAccess.c',
        'XPLMDisplay.c',
        'XPLMGraphics.c',
        'XPLMInstance.c',
        'XPLMMap.c',
        'XPLMMenus.c',
        'XPLMNavigation.c',
        'XPLMPlanes.c',
        'XPLMPlugin.c',
        'XPLMProcessing.c',
        'XPLMScenery.c',
        'XPLMSound.c',
        'XPLMUtilities.c',
        'XPLMWeather.c',
    ),
    name_prefix: '',
    name_suffix: 'so',
    include_directories: xplm_inc,
    c_args: ['-DXPLM', '-DXPLM_DEPRECATED', '-DXPLM_LEVEL=400'],
    gnu_symbol_visibility: 'hidden',
)
