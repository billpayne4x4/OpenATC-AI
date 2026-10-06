# Third-party components

Pinned build dependencies: Dear ImGui 1.91.9b, nlohmann JSON 3.11.3, cpp-httplib 0.18.3, miniaudio 0.11.22, GLFW 3.4 (optional desktop), glad 2.0.8 (Linux graphics declarations), OpenSSL (engine only), and the X-Plane SDK 4.3 compatibility API. Their license files are collected by the release packaging script; the SDK has its own redistribution terms and is downloaded separately.

Bundled DejaVu Sans and DejaVu Sans Bold fonts retain their license in `assets/fonts/LICENSE.txt`. The OpenATC AI vector logo is project artwork. Font bytes are embedded at build time; no font download or user-system font path is required at runtime.

No X-Plane airport/navdata, Navigraph charts, AIRAC databases, model weights or provider API keys are redistributed. The simulator data readers use the user's installed files. Online weather observations are NOAA Aviation Weather Center data fetched by the engine for controller advisories. SimBrief and Navigraph access remain subject to their service terms.
