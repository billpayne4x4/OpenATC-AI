# SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
#
# SPDX-License-Identifier: MIT

xplm_inc = include_directories('include')

if tgt_sys == 'linux'
    subdir('src')
    xplm_dep = declare_dependency(
        include_directories: [xplm_inc],
        link_with: [xplm_stub],
        compile_args: [xplm_lvl_ca],
    )
elif tgt_sys == 'darwin'
    xplm_dep = declare_dependency(
        include_directories: [xplm_inc],
        link_args: [
            '-F' + meson.current_source_dir() / 'Frameworks',
            '-framework',
            'XPLM',
        ],
        compile_args: [xplm_lvl_ca],
    )
elif tgt_sys == 'windows'
    xplm_dep = declare_dependency(
        include_directories: [xplm_inc],
        dependencies: [
            cc.find_library(
                'XPLM_64',
                dirs: [meson.current_source_dir() / 'lib'],
            ),
        ],
        compile_args: [xplm_lvl_ca],
    )
else
    error('This should be unreachable.')
endif

meson.override_dependency('xplm', xplm_dep)
