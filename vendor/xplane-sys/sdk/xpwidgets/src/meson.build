# SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
#
# SPDX-License-Identifier: MIT

xpwidgets_stub = shared_library(
    'XPWidgets_64',
    files(
        'XPUIGraphics.c',
        'XPWidgetUtils.c',
        'XPWidgets.c',
    ),
    name_prefix: '',
    include_directories: [xplm_inc, xpwidgets_inc],
    link_with: [xplm_stub],
    c_args: ['-DXPWIDGETS', '-DXPLM_DEPRECATED', '-DXPLM_LEVEL=400'],
    gnu_symbol_visibility: 'hidden',
)
