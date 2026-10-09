<!--
SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>

SPDX-License-Identifier: MIT
-->

# x-plane-sdk

Have you ever felt like the X-Plane SDK is packaged in a *really* weird format?
Well, it's probably because it originally hails from the early days of XSquawkBox,
where it was distributed *by* XSquawkBox.

For some reason, though, Laminar Research never bothered to reorganize the package.
I took it upon myself to do that. This repository can be imported as a subproject/wrap
in Meson, and as an ExternalProject or FetchDependency or whatever in CMake.

I'm unlikely to add any other build systems.

## Linux: A word of caution

***TL;DR: X-Plane < 12.1.0 on Linux must be patched.***

You may notice that this builds library stubs for Linux. Cool, right? This allows
building of plugins with *all* symbols defined, so whenever *any* external symbols
are used, we ensure we link directly to the libraries that define them, avoiding
issues of expecting X-Plane to provide dependencies. Instead, `ld.so` will happily
load any missing libraries when your plugin is loaded.

There's a problem, though. For a while now, X-Plane has shipped without an `SONAME`
in the `XPLM_64.so` and `XPWidgets_64.so` libraries. This would prevent `ld.so` from
properly locating these libraries when requested by plugins.

There is a simple fix, however:

``` shell
cd X-Plane/Resources/plugins
patchelf --set-soname XPLM_64.so XPLM_64.so
patchelf --set-soname XPWidgets_64.so XPWidgets_64.so
```

This will ensure compatibility with stub-linked plugins, while not changing behavior
for plugins built with dangling symbols.

X-Plane 12.1.0 should ship with `SONAME`s set in these libraries.

