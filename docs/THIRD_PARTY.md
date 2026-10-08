# Third-party components

The active runtime is the Rust workspace; exact dependency versions are pinned in `Cargo.lock`. It uses imgui/glow for UI, miniaudio behind a native FFI adapter for audio, Axum/reqwest for the companion engine, and xplane/xplane-sys with the SDK 4.0 feature for X-Plane 12 weather APIs. Legacy CMake build files have been removed. Rust dependencies can still contain native C/C++ implementations (including ImGui and miniaudio); first-party Rust does not mean a purely Rust dependency tree. Preserve dependency and SDK redistribution terms when packaging.

Bundled DejaVu Sans and DejaVu Sans Bold fonts retain their license in `assets/fonts/LICENSE.txt`. The OpenATC AI vector logo is project artwork. Font bytes are embedded at build time; no font download or user-system font path is required at runtime.

No X-Plane airport/navdata, Navigraph charts, AIRAC databases, model weights or provider API keys are redistributed. The simulator data readers use the user's installed files. Optional online planning weather is NOAA Aviation Weather Center data. Live ATIS/controller weather instead comes from airport-location X-Plane surface samples. SimBrief and Navigraph access remain subject to their service terms.

The Rust airport map uses earcutr 0.4.3 (ISC) to triangulate installed pavement outlines while preserving holes. Its notice is bundled in `assets/licenses/earcutr-LICENSE`. The illuminated taxi arrow model and textures are original project assets.

The Rust plugin vendors xplane 0.1.0-alpha.1 (MPL 2.0), with a corrected SDK dataref type-mask check. The installer includes its complete source in `source/xplane` and its license in `assets/licenses/xplane-MPL-2.0.txt`. Preserve these files when redistributing the plugin.

The Rust companion engine uses reqwest with default features disabled and rustls/webpki roots for HTTPS. It does not dynamically link OpenSSL; preserve the Rust TLS dependencies’ license notices when distributing.

The Rust UI uses arboard (MIT/Apache-2.0) for native text clipboard access. Linux uses the Rust x11rb transport without linking Xlib into the plugin. Clipboard ownership lives until the UI closes.

## GeoNames settlement data

`assets/geography/places.tsv` is derived from the GeoNames cities1000 download (8 October 2026). Geographical data © GeoNames, https://www.geonames.org/, under CC BY 4.0, https://creativecommons.org/licenses/by/4.0/. Reduced to ASCII names, coordinates, country codes and population; see the asset README for coverage. This dataset is separate from the MIT application license.
