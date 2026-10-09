# SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
#
# SPDX-License-Identifier: MIT

xpwrappers_inc = include_directories('include')

if add_languages('cpp', native: false, required: get_option('wrappers'))
    subdir('src')
    meson.override_dependency(
        'xp-wrappers',
        declare_dependency(
            include_directories: xpwrappers_inc,
            link_with: [xpwrappers_lib],
            dependencies: [xplm_dep, xpwidgets_dep],
        ),
    )
else
    meson.override_dependency(
        'xp-wrappers',
        dependency('', required: false),
        native: false,
        static: true,
    )
endif
