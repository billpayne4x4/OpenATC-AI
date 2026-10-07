//! Exercise regional selection, rendering, and rejection of broken user edits.
use openatc_core::speech::{load_speech_dir, render_checked, sample_slots};
use std::path::PathBuf;
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../speech")
}
#[test]
fn examples_render_and_historical_ids_survive() {
    let pool = load_speech_dir(&root()).unwrap();
    assert!(pool.len() > 450);
    for id in [
        "delivery.ifr_sid",
        "ground.pushback_approved",
        "tower.landing",
        "arrival.hold_short",
        "chatter.check_in",
        "emergency.fire",
        "ramp.pushback_crew",
        "cabin.boarding",
        "copilot.takeoff_roll",
    ] {
        assert!(pool.get(id).is_some(), "missing historical ID {id}");
    }
    for (id, entry) in pool.iter() {
        for line in entry.say.iter().chain([&entry.accept, &entry.decline]) {
            render_checked(line, &sample_slots()).unwrap_or_else(|e| panic!("{id}: {e}"));
        }
    }
    assert!(
        render_checked(
            "Turn {turn} heading {heading}.",
            &openatc_core::catalog::Slots::default()
        )
        .is_err()
    );
    assert_eq!(
        render_checked("Turn {turn} heading {heading}.", &sample_slots()).unwrap(),
        "Turn left heading 270."
    );
    assert!(render_checked("Climb to {alt}.", &openatc_core::catalog::Slots::default()).is_err());
}
#[test]
fn regional_and_flight_rules_boundaries() {
    let pool = load_speech_dir(&root()).unwrap();
    for (airport, region) in [
        ("KJFK", "us"),
        ("PHNL", "us"),
        ("PANC", "us"),
        ("NZAA", "newzealand"),
        ("NFFN", "pacific"),
        ("TTPP", "caribbean"),
        ("CYYZ", "canada"),
        ("EGLL", "united_kingdom"),
        ("LFPG", "europe"),
        ("YMLT", "australia"),
        ("FAOR", "africa"),
        ("SBGR", "south_america"),
        ("WSSS", "asia"),
        ("ZBAA", "china"),
        ("UUEE", "russia_central_asia"),
    ] {
        assert_eq!(pool.region_for_airport(airport).unwrap().name, region);
    }
    for airport in ["KJFK", "CYYZ", "EGLL", "YMLT", "FAOR", "NZAA", "ZBAA"] {
        for rules in ["ifr", "vfr"] {
            let found = pool.select_for_airport("atc", &[], "", airport, rules, usize::MAX);
            assert!(!found.is_empty());
            for entry in found {
                assert!(entry.flight_rules == "shared" || entry.flight_rules == rules);
                let profile = pool.region_for_airport(airport).unwrap();
                assert!(entry.region == "common" || entry.region == profile.name);
                assert!(entry.variant == "common" || entry.variant == profile.variant);
                if airport == "ZBAA" {
                    assert!(!entry.slots.iter().any(|s| s == "alt"));
                }
            }
        }
    }
    assert!(
        pool.select_for_airport(
            "atc",
            &["conditional_lineup_traffic_identified_and_in_sight"],
            "departure",
            "KJFK",
            "ifr",
            20
        )
        .is_empty()
    );
    assert_eq!(
        pool.select_for_airport("atc", &["basic_service"], "cruise", "EGLL", "vfr", 20)[0].id,
        "united_kingdom_services.basic_service"
    );
    assert!(
        pool.select_for_airport("atc", &["basic_service"], "cruise", "YMLT", "vfr", 20)
            .is_empty()
    );
}
#[test]
fn crew_is_global_and_phase_is_normalized() {
    let pool = load_speech_dir(&root()).unwrap();
    for role in ["copilot", "ground", "attendant"] {
        let us = pool.select_for_airport(role, &[], "", "KJFK", "ifr", usize::MAX);
        let au = pool.select_for_airport(role, &[], "", "YMLT", "vfr", usize::MAX);
        assert_eq!(
            us.iter().map(|e| &e.id).collect::<Vec<_>>(),
            au.iter().map(|e| &e.id).collect::<Vec<_>>()
        );
        assert!(
            us.iter()
                .all(|e| e.region == "common" && e.flight_rules == "shared")
        );
    }
    assert!(
        !pool
            .select_for_airport("atc", &["vacate"], "taxiin", "YMLT", "ifr", 10)
            .is_empty()
    );
}
#[test]
fn invalid_edits_and_duplicate_ids_include_paths() {
    let dir = std::env::temp_dir().join(format!("openatc-speech-edit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let original = "[edit.test]\nrole='atc'\nphase=['taxi']\nvariant='common'\nsituation=['hold']\nslots=['callsign']\nsay=['{callsign}, hold position.']\n";
    for broken in [
        original.replace("slots=['callsign']", "slots=['typo']"),
        original.replace("role='atc'", "role='attc'"),
        original.replace("phase=['taxi']", "phase=['taxii']"),
        format!("{original}saay=['Typo']\n"),
        original.replace("{callsign}", "{unknown}"),
        original.replace("{callsign}", "{callsign"),
        format!("{original}effect='amend_route'\n"),
    ] {
        std::fs::write(dir.join("edit.toml"), broken).unwrap();
        assert!(load_speech_dir(&dir).unwrap_err().contains("edit.toml"));
    }
    std::fs::write(dir.join("edit.toml"), original).unwrap();
    assert_eq!(load_speech_dir(&dir).unwrap().len(), 1);
    std::fs::write(dir.join("duplicate.toml"), original).unwrap();
    let error = load_speech_dir(&dir).unwrap_err();
    assert!(error.contains("duplicate speech ID edit.test"));
    assert!(error.contains("edit.toml") && error.contains("duplicate.toml"));
    std::fs::remove_dir_all(dir).unwrap();
}
