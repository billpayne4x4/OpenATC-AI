# SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
#
# SPDX-License-Identifier: MIT

xpwidgets_inc = include_directories('include')

if tgt_sys == 'linux'
    subdir('src')
    xpwidgets_dep = declare_dependency(
        include_directories: [xpwidgets_inc],
        compile_args: [xplm_lvl_ca],
        link_with: [xpwidgets_stub],
        dependencies: [xplm_dep],
    )
elif tgt_sys == 'darwin'
    xpwidgets_dep = declare_dependency(
        include_directories: [xpwidgets_inc],
        compile_args: [xplm_lvl_ca],
        link_args: [
            '-F' + meson.current_source_dir() / 'Frameworks',
            '-framework',
            'XPWidgets',
        ],
        dependencies: [xplm_dep],
    )
elif tgt_sys == 'windows'
    xpwidgets_dep = declare_dependency(
        include_directories: [xpwidgets_inc],
        compile_args: [xplm_lvl_ca],
        dependencies: [
            xplm_dep,
            cc.find_library(
                'XPWidgets_64',
                dirs: [meson.current_source_dir() / 'lib'],
            ),
        ],
    )
else
    error('This should be unreachable.')
endif

meson.override_dependency('xpwidgets', xpwidgets_dep)
