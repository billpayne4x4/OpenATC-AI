//! Check the distributed configuration using the same loaders as the application.
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn shipped_configuration_loads() {
    let root = root();
    let catalogue = openatc_core::catalog::load_intents(&root.join("intents.toml")).unwrap();
    assert!(!catalogue.is_empty());
    let speech = openatc_core::speech::load_speech_dir(&root.join("speech")).unwrap();
    assert!(!speech.is_empty());
    let regions = openatc_core::regions::load_regions(&root.join("regions.toml")).unwrap();
    assert!(!regions.is_empty());
    let mut profiles = 0;
    for file in std::fs::read_dir(root.join("aircraft")).unwrap() {
        let path = file.unwrap().path();
        if path.extension().and_then(|value| value.to_str()) == Some("toml") {
            openatc_core::profiles::load_aircraft_profile(&path)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            profiles += 1;
        }
    }
    assert!(profiles > 0);
}

#[test]
fn installed_toliss_neo_identity_and_com1_mapping_are_explicit() {
    let profile =
        openatc_core::profiles::load_aircraft_profile(&root().join("aircraft/toliss_a320.toml"))
            .unwrap();
    assert!(openatc_core::profiles::aircraft_matches(
        &profile,
        "Gliding Kiwi",
        "A20N"
    ));
    assert!(!openatc_core::profiles::aircraft_matches(
        &profile,
        "Gliding Kiwi",
        "A320"
    ));
    assert_eq!(
        profile.com1_active_ref,
        "sim/cockpit2/radios/actuators/com1_frequency_hz_833"
    );
}
