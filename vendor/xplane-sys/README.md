<!--
SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com

SPDX-License-Identifier: Apache-2.0 OR MIT
-->

[![docs.rs](https://img.shields.io/docsrs/xplane-sys)](https://docs.rs/xplane-sys) [![GitHub Workflow Status (with event)](https://img.shields.io/github/actions/workflow/status/judemille/xplane-sys/rust.yml)](https://github.com/judemille/xplane-sys/actions)

# xplane-sys: Rust bindings for the X-Plane plugin SDK

This library provides Rust bindings to the X-Plane plugin SDK.

## SDK versions

This crate is based on SDK version 4.0.1 (XPLM400), supporting X-Plane 12.04 and
newer. Plugins made with this crate can be used with earlier versions of X-Plane, so
long as the right feature gates are set. The gates are as follows:

- `XPLM400` -- X-Plane 12.04 and newer.
- `XPLM303` -- X-Plane 11.50 and newer.
- `XPLM301` -- X-Plane 11.20 and newer.
- `XPLM300` -- X-Plane 11.10 and newer.
- `XPLM210` -- X-Plane 10.20 and newer. (actually 10.00 and newer, but this crate forbids 32-bit plugins.)

Unlike the X-Plane C SDK shipped by Laminar, this crate automatically infers older
versions from requesting a newer version, so no need to define all the versions up
to your desired version. Features from SDK versions newer than the requested version
*can* be used, if you perform a check on the SDK version level at runtime. At that
point, if the version is high enough, you can request a pointer to the desired
function. This is outlined in the X-Plane documentation. This allows the support of
a range of versions, while also allowing access to newer features if present.

Some features from earlier SDK versions have been deprecated. This crate does not
provide a way to access them. Realistically, if you are using gates below `XPLM303`,
you are nuts. The feature gates are included, but X-Plane 11.50 was released in 2020,
so most users should have it, even if they are not on X-Plane 12.

This crate will be updated as the X-Plane SDK updates, and will have an equal
version number. (except in extraneous circumstances)

## Documentation

The types and functions are documented on [the X-Plane plugin API website](http://developer.x-plane.com/sdk/).

## Compiling and linking

The following targets are supported, constrained by X-Plane support:
- `x86_64-pc-windows-msvc`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`
- `x86_64-unknown-linux-gnu`

On macOS and Windows, plugins must be dynamically linked with libraries that
provide stub implementations of the SDK functions. This crate includes those
libraries and tells Cargo to link them.

### Stubs on Linux

This crate includes the experimental ability to generate and link library stubs for
Linux. This is desirable, so that plugins can be built without having to leave
undefined symbols, a point at which other dependencies may be left dangling.

The feature can be enabled with the feature flag `stub-linux`.

This should work seamlessly on the upcoming X-Plane 12.1.0, however on previous
versions, some changes will be necessary to make this work:

``` shell
cd X-Plane/Resources/plugins
patchelf --set-soname XPLM_64.so XPLM_64.so
patchelf --set-soname XPWidgets_64.so XPWidgets_64.so
```

Prior to X-Plane 12.1.0, XPLM and XPWidgets were built without an `SONAME`. Those
commands fix that.

## Disclaimer

The current maintainer of this project is a trans lesbian who unequivocally supports
Ukraine against Russia, and opposes any and all human rights violations.

### *You should not use this project if you:*

- Do not unequivocally support the LGBTQ+ population, including transgender
  individuals.
- Think that LGBTQ+ people "shouldn't put it out on display"
- Support "drop the T", TERF, or similar movements.
- Think that pedophilia is included in LGBTQ+, either because you want it to be
  included, or you think that the community accepts it. It does not accept it.
- Refuse to address and refer to people with their preferred name, pronouns, and
  gender labels.
- Do not support Ukraine's struggle against their Russian oppressors.
- Support any far-right parties or politicians (including Vladimir Putin, the GOP,
  AfD, FdI, and similar)

I cannot stop you, but anyone observed to meet the above listed criteria who
interacts with the project will be blocked from further interaction.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](licenses/Apache-2.0.txt) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](licenses/MIT.txt) or <http://opensource.org/licenses/MIT>)

at your option.

Files in the SDK folder are provided under a separate license, provided in
[XPlaneSDK/license.txt](XPlaneSDK/license.txt).

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you shall be dual licensed as above, without any
additional terms or conditions.

## Unit Testing

The `mockall` feature of this crate is intended to be enabled when unit testing a
crate that uses this crate. All functions will be mocked, using
[mockall.](https://github.com/asomers/mockall) This crate currently exposes
`mockall = "~0.12"`.
