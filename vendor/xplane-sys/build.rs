// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com
//
// SPDX-License-Identifier: Apache-2.0 OR MIT

use bindgen::callbacks::{IntKind, ParseCallbacks};
use bindgen::EnumVariation;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn get_clang_args(crate_path: &Path) -> Vec<String> {
    let mut r = Vec::new();
    assert!(
        cfg!(feature = "XPLM200"),
        "Please set a desired SDK version!"
    );

    let xplm_level = env::vars()
        .filter_map(|(k, _)| match k.strip_prefix("CARGO_FEATURE_")? {
            "XPLM400" => Some(400),
            "XPLM303" => Some(303),
            "XPLM302" => Some(302),
            "XPLM301" => Some(301),
            "XPLM300" => Some(300),
            "XPLM210" => Some(210),
            "XPLM200" => Some(200),
            _ => None,
        })
        .max()
        .unwrap();

    r.push(format!("-DXPLM_LEVEL={xplm_level}"));
    r.push("-DXPLM_FORCE_ENUM_TYPE".to_owned());

    if cfg!(feature = "fmod") {
        r.push("-D_FMOD_STUB_".to_string());
    }

    let xplmheaders = crate_path.join("sdk/xplm/include");
    let widgetheaders = crate_path.join("sdk/xpwidgets/include");
    r.push(format!("-I{}", xplmheaders.to_str().unwrap()));
    r.push(format!("-I{}", widgetheaders.to_str().unwrap()));

    r
}

const ALLOWED_TARGETS_TEXT: &str = r"Supported targets for xplane-sys are:
- x86_64-pc-windows-msvc
- x86_64-apple-darwin
- aarch64-apple-darwin
- x86_64-unknown-linux-gnu";

fn handle_platform(crate_path: &Path) {
    let target = env::var("TARGET").unwrap();
    match target.as_str() {
        "x86_64-pc-windows-msvc" => {
            let xplm_path = crate_path.join("sdk/xplm/lib");
            let xpw_path = crate_path.join("sdk/xpwidgets/lib");
            println!("cargo:rustc-link-search={}", xplm_path.to_str().unwrap());
            println!("cargo:rustc-link-search={}", xpw_path.to_str().unwrap());
            println!("cargo:rustc-link-lib=XPLM_64");
            println!("cargo:rustc-link-lib=XPWidgets_64");
        }
        "x86_64-apple-darwin" | "aarch64-apple-darwin" => {
            let xplm_path = crate_path.join("sdk/xplm/Frameworks");
            println!(
                "cargo:rustc-link-search=framework={}",
                xplm_path.to_str().unwrap()
            );
            let xpw_path = crate_path.join("sdk/xpwidgets/Frameworks");
            println!(
                "cargo:rustc-link-search=framework={}",
                xpw_path.to_str().unwrap()
            );
            println!("cargo:rustc-link-lib=framework=XPLM");
            println!("cargo:rustc-link-lib=framework=XPWidgets");
        }
        "x86_64-unknown-linux-gnu" => {
            // so that we don't try to resolve cmake on other OSes
            #[cfg(target_os = "linux")]
            if cfg!(feature = "stub-linux") {
                println!("cargo:rustc-link-arg=--no-allow-shlib-undefined");
                let dst = cmake::Config::new("sdk")
                    .build_target("all")
                    .very_verbose(true)
                    .build();
                println!(
                    "cargo:rustc-link-search={}",
                    dst.join("build/xplm/src").to_str().unwrap()
                );
                println!(
                    "cargo:rustc-link-search={}",
                    dst.join("build/xpwidgets/src").to_str().unwrap()
                );
                println!("cargo:rustc-link-lib=dylib:+verbatim=XPLM_64.so");
                println!("cargo:rustc-link-lib=dylib:+verbatim=XPWidgets_64.so");
            }
        } // No need to link libraries on Linux.
        _ => {
            eprintln!("{}", ALLOWED_TARGETS_TEXT);
            panic!("unsupported target: {target}")
        }
    };
}

#[derive(Debug)]
struct NamingHandler;

impl ParseCallbacks for NamingHandler {
    fn int_macro(&self, name: &str, _value: i64) -> Option<IntKind> {
        if name.starts_with("XPLM_VK") || name.starts_with("XPLM_KEY") {
            Some(IntKind::U32)
        } else {
            None
        }
    }
    fn enum_variant_name(
        &self,
        enum_name: Option<&str>,
        original_variant_name: &str,
        _variant_value: bindgen::callbacks::EnumVariantValue,
    ) -> Option<String> {
        let mut out = original_variant_name
            .trim_start_matches("xplm")
            .trim_start_matches('_');
        let Some(enum_name) = enum_name else {
            return Some(out.to_string());
        };
        out = match enum_name {
            "XPLMCameraControlDuration" => out.trim_start_matches("ControlCamera"),
            "XPLMDataTypeID" => out.trim_start_matches("Type"),
            "XPLMKeyFlags" => out.trim_end_matches("Flag"),
            "XPLMDrawingPhase" => out.trim_start_matches("Phase"),
            "XPLMDeviceID" => {
                out = if out.ends_with("_1") || out.ends_with("_2") || out.ends_with("_3") {
                    let side = out.chars().last().unwrap(); // We know this string ends with something.
                    let side = match side {
                        '1' => "Pilot",
                        '2' => "Copilot",
                        '3' => "Center",
                        _ => unreachable!(),
                    };
                    out = out.trim_start_matches("device_");
                    return Some(out[0..out.len() - 1].to_string() + side);
                } else {
                    out
                };
                out.trim_start_matches("device")
            }
            "XPLMMouseStatus" => out.trim_start_matches("Mouse"),
            "XPLMCursorStatus" => out.trim_start_matches("Cursor"),
            "XPLMWindowLayer" => out.trim_start_matches("WindowLayer"),
            "XPLMWindowDecoration" => out.trim_start_matches("WindowDecoration"),
            "XPLMWindowPositioningMode" => out.trim_start_matches("Window"),
            "XPLMFontID" => out.trim_start_matches("Font"),
            "XPLMMapStyle" => out.trim_start_matches("MapStyle"),
            "XPLMMapLayerType" => out.trim_start_matches("MapLayer"),
            "XPLMMapOrientation" => out.trim_start_matches("MapOrientation"),
            "XPLMMenuCheck" => out.trim_start_matches("Menu"),
            "XPLMNavType" => out.trim_start_matches("Nav"),
            "XPLMFlightLoopPhaseType" => out.trim_start_matches("FlightLoop_Phase"),
            "XPLMProbeType" | "XPLMProbeResult" => out.trim_start_matches("Probe"),
            "XPLMAudioBus" => out.trim_start_matches("Audio"),
            "XPLMBankID" => out.trim_end_matches("Bank"),
            "XPLMCommandPhase" => out.trim_start_matches("Command"),
            "XPLMDataFileType" => out.trim_start_matches("DataFile"),
            "XPLMHostApplicationID" => out.trim_start_matches("Host"),
            "XPLMLanguageCode" => out.trim_start_matches("Language"),
            "XPWindowStyle" => out.trim_start_matches("xpWindow"),
            "XPElementStyle" => out.trim_start_matches("xpElement"),
            "XPTrackStyle" => out.trim_start_matches("xpTrack"),
            "XPWidgetPropertyID" => out.trim_start_matches("xpProperty"),
            "XPDispatchMode" => out.trim_start_matches("xpMode"),
            "XPWidgetMessage" => out.trim_start_matches("xpMsg"),
            enum_name => {
                if !enum_name.starts_with("XPLM") && enum_name.starts_with("XP") {
                    out = out.trim_start_matches("xp").trim_start_matches('_');
                    if enum_name.ends_with("Property") {
                        out = out.trim_start_matches("Property");
                    } else if enum_name.ends_with("Message") {
                        out = out.trim_start_matches("Message").trim_start_matches("Msg");
                    } else if enum_name == "XPScrollBarType" {
                        out = out.trim_start_matches("ScrollBarType");
                    } else if enum_name == "XPTextFieldType" {
                        out = out.trim_start_matches("Text");
                    } else if enum_name == "XPButtonBehavior" {
                        out = out.trim_start_matches("ButtonBehavior");
                    }
                    out.trim_start_matches('_')
                } else {
                    out
                }
            }
        };
        out = out.trim_start_matches('_');
        Some(out.to_string())
    }
}

fn main() {
    // Get the absolute path to this crate, so that linking will work when done in another folder
    let crate_path = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let bindings_builder = bindgen::Builder::default()
        .header("src/combined.h")
        .use_core()
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .clang_args(get_clang_args(&crate_path));

    let bindings_except_fns = bindings_builder
        .clone()
        .prepend_enum_name(false)
        .default_enum_style(EnumVariation::NewType {
            is_bitfield: false,
            is_global: false,
        })
        .bitfield_enum("(XPLMDataTypeID|XPLMKeyFlags|XPLMNavType)")
        .no_debug("(XPLMDataTypeID|XPLMKeyFlags|XPLMNavType)")
        .parse_callbacks(Box::new(NamingHandler))
        .default_macro_constant_type(bindgen::MacroTypeVariation::Signed)
        .ignore_functions()
        .generate()
        .expect("Unable to generate bindings!")
        .to_string()
        .replace(r#"extern "C""#, r#"extern "C-unwind""#);

    let bindings_fns_only = bindings_builder
        .with_codegen_config(bindgen::CodegenConfig::FUNCTIONS)
        // Windows headers also declare CRT functions. Only SDK calls may use C-unwind.
        .allowlist_function("(XPLM|XP).*")
        .override_abi(bindgen::Abi::CUnwind, ".*")
        .generate()
        .expect("Unable to generate bindings!")
        .to_string()
        .replace(r#"extern "C""#, r#"extern "C-unwind""#);

    let bindings = &[
        r#"#[cfg(feature = "mockall")]
use mockall::automock;
#[cfg_attr(feature = "mockall", automock)]
#[cfg_attr(feature = "mockall", allow(dead_code))] // Don't warn on not using mocked functions.
mod functions {
    use super::*;
"#,
        &bindings_fns_only,
        r#"}
#[cfg(not(feature = "mockall"))]
#[doc(inline)]
pub use functions::*;
#[cfg(feature = "mockall")]
#[doc(inline)]
pub use mock_functions::*;
"#,
        &bindings_except_fns,
    ]
    .join("");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("bindings.rs");
    fs::write(out_path, bindings.as_bytes()).expect("Could not write bindings!");

    if !cfg!(feature = "mockall") {
        handle_platform(&crate_path);
    }
}
