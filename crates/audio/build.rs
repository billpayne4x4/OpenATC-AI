//! Build script: compile the miniaudio C shim.
//!
//! Let miniaudio select the native backend (PulseAudio/PipeWire on Linux,
//! WASAPI on Windows, `CoreAudio` on macOS), with ALSA as a Linux fallback.
//! JACK is unnecessary for desktop device selection and remains disabled.
fn main() {
    cc::Build::new()
        .file("vendor/oatc_audio.c")
        .include("vendor")
        .define("MA_NO_JACK", None)
        .define("MA_NO_AUDIO4", None)
        .warnings(false)
        .compile("oatc_audio");
    println!("cargo:rerun-if-changed=vendor/oatc_audio.c");
    println!("cargo:rerun-if-changed=vendor/miniaudio.h");
}
