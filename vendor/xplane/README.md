<!--
SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>

SPDX-License-Identifier: MPL-2.0
-->

# X-Plane plugin APIs for Rust

[![Crates.io Version](https://img.shields.io/crates/v/xplane.svg)](https://crates.io/crates/xplane)
[![Documentation](https://docs.rs/xplane/badge.svg)](https://docs.rs/xplane)
[![License](https://img.shields.io/crates/l/xplane.svg)](https://git.sr.ht/~jdemille/xplane.rs#license)

## Purpose

**xplane.rs** provides a convenient interface for X-Plane plugin development in the
Rust programming language for all platforms supported by X-Plane. These interfaces
should *mostly* be safe.

This project is designed to support any 64-bit version of X-Plane, so long as the
right feature gates are used. Testing is performed with XPLM400 and X-Plane 12, but
an effort will be made to keep base API compatibility at XPLM210. Please send an
email to [the list](mailto:~jdemille/xplane.rs-discuss@lists.sr.ht) if there is a
compatibility regression.

Most development happens on the `develop` branch, with the `trunk` branch kept
stable at releases.

## Status

This library is going through a a complete rewrite to enforce threading invariants.
Do not use it in its current state. Here is a checklist of components:

- [ ] Compiles and is callable from X-Plane (status unknown, will be testing.)
- [x] Debug logging to the console / log file
- [x] DataRef reading and writing
- [x] Commands
- [ ] GUI
- [ ] Drawing

## Example

Some more examples can be found in the `examples/` directory.

~~This small snippet, however, is the minimal boilerplate needed to make your plugin compile.~~

```rust
use xplane::plugin::{Plugin, PluginInfo};
use xplane::{debugln, xplane_plugin};

struct MinimalPlugin;

impl Plugin for MinimalPlugin {
    type Error = std::convert::Infallible;

    fn start() -> Result<Self, Self::Error> {
        // The following message should be visible in the developer console and the Log.txt file
        debugln!("Hello, World! From the Minimal Rust Plugin");
        Ok(MinimalPlugin)
    }

    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: String::from("Minimal Rust Plugin"),
            signature: String::from("org.samcrow.xplm.examples.minimal"),
            description: String::from("A plugin written in Rust"),
        }
    }
}

xplane_plugin!(MinimalPlugin);
```

## Disclaimer

The current maintainer of this project is a trans lesbian who unequivocally supports
Ukraine in their struggle against Russia, and opposes any and all human rights
violations.

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

Licensed under the Mozilla Public License, version 2.0.
Any code from commit `ba89d4234c5b4d7088a40b2bb8f537f72e1e2df3` and before is
dual-licensed under the Apache License and the MIT License, at your choice.

### What does this mean for me?

I get it, you don't have time to read the license. Here's some bullet points on what
this license means for you.

- You may combine this library with other work that is under a different license, so
  long as the files of this library remain separate.
  - The code of this library, under the MPL-2.0 license (or compatible), must be made
    readily available to users.
  - Recipients of the larger work must be made aware of the use of this library, its
    license, and how to acquire the code of this library.
- Any modifications of this library's files must be published under the MPL-2.0.
- You may use this library commercially, so long as it is made clear that it is done
  on your own behalf, and not on the behalf of the contributors.

There is some more nuance than that, but those bullet points cover the general points.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in the work by you shall be licensed as above, without any additional terms
or conditions.

All commits must be signed-off (`git commit -s`). This is a declaration that you have
read and agree to the terms in the
[Developer Certificate of Origin](https://developercertificate.org/) (DCO). This can
be compared to a CLA, but is different in that your copyright remains with you. All
you are doing is attesting that you can contribute code under the repository's
license. A copy of the DCO text is kept in this repository at DCO.txt.

You may sign the DCO with your preferred name, so long as it is relatively
consistent, so we can keep track of who's who.

## Minimum Supported Rust Version

The MSRV of this crate is always the latest stable compiler version at the time of a
given commit. Maybe it'll work on an older version. Maybe it won't. No guarantees.

This will probably change once a stable release goes out.
