# SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
#
# SPDX-License-Identifier: MIT

project(
    'x-plane-sdk',
    'c',
    version: '4.0.1',
    license: 'MIT',
    default_options: 'warning_level=0',
    meson_version: '>= 0.63.0',
)

tgt_sys = host_machine.system()
tgt_arch = host_machine.cpu_family()

assert(
    (tgt_sys in ['windows', 'linux'] and tgt_arch == 'x86_64')
    or (tgt_sys == 'darwin'
    and tgt_arch in ['aarch64', 'x86_64']),
    f'''Unsupported target: @tgt_sys@ on @tgt_arch@!

The supported targets are:
- darwin on aarch64
- darwin on x86_64
- linux on x86_64
- windows on x86_64''',
)

xplm_level = get_option('xplm-level')
xplm_lvl_ca = f'-DXPLM_LEVEL=@xplm_level@'
cc = meson.get_compiler('c')

subdir('xplm')
subdir('xpwidgets')

subdir('xp-wrappers')
