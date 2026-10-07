<!--
SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>

SPDX-License-Identifier: MPL-2.0
-->

# Introduction

**rust-xplane** is an attempt at making safe bindings for the X-Plane plugin
SDK in Rust. As of the time of writing, it is pre-production software, but
here's what it has going for it:

- Written in [Rust](https://www.rust-lang.org) for speed combined with safety.
- APIs tailored to prevent the accidental use of XPLM APIs outside of callbacks.
- An attentive and committed developer, who will accept constructive feedback.

This guide runs through a rough idea of how to use these bindings, as well as
what's going on inside.

## Contributing

rust-xplane is free and open source. You can find the source code on [GitHub](https://github.com/judemille/rust-xplane).
Issues and feature requests can be posted on the GitHub issue tracker. If you
would like to contribute, by all means, please do! Any and all contributions,
even constructive feedback, are appreciated. Do note that all developers must
sign-off their commits, thereby attesting to the [Developer Certificate of Origin](https://developercertificate.org/).
This can be done by appending a `-s` to the commit command, e.g. `git commit -s`.

## License

The rust-xplane source and documentation are released under the
[Mozilla Public License v2.0](https://www.mozilla.org/MPL/2.0/).
