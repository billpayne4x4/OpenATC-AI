//! Complete pilot auto-replies retain controller-assigned facts.
use openatc_core::{
    readback::{auto_reply, prepare},
    state::{Clearance, State, Transmission},
};

fn pending() -> State {
    let mut state = State::default();
    state.plan.callsign = "C-GTLT".into();
    state.plan.destination = "VTBS".into();
    state.clearance = Some(Clearance {
        altitude_feet: 5000,
        route: "CHU1C CMP W21 NOBER NORT2D".into(),
        squawk: "2105".into(),
        runway: "31".into(),
        sequence: 2,
        ..serde_json::from_value(serde_json::json!({})).unwrap()
    });
    state.transcript.push(Transmission {
        speaker: "ATC".into(),
        position: "Airport Tower".into(),
        sequence: 2,
        ..serde_json::from_value(serde_json::json!({})).unwrap()
    });
    state
}
#[test]
fn automatic_ifr_reply_contains_all_operational_values_and_passes_spoken_validation() {
    let state = pending();
    let mut reply = auto_reply(&state).unwrap();
    assert_eq!(reply.clearance_sequence, 2);
    assert_eq!(reply.readback_kind, "ifr");
    for value in [
        "VTBS",
        "CHU1C CMP W21 NOBER NORT2D",
        "5000",
        "2105",
        "31",
        "C-GTLT",
    ] {
        assert!(reply.text.contains(value));
    }
    reply.clearance_sequence = 0;
    prepare(&state, &mut reply);
    assert_eq!(reply.clearance_sequence, 2);
    assert!(
        !reply.waypoint.starts_with("READBACK ERROR:"),
        "{}",
        reply.waypoint
    );
}
#[test]
fn taxi_reply_keeps_hold_short_and_background_traffic_cannot_replace_it() {
    let mut state = pending();
    state.taxi_clearance.pending_readback = true;
    state.taxi_clearance.sequence = 6;
    state.taxi_clearance.instructions = "Taxi via Alpha, hold short of runway 31.".into();
    state.taxi_clearance.via = "Alpha".into();
    state.taxi_clearance.destination = "31".into();
    state.taxi_clearance.hold_short_runway = "31".into();
    state.transcript.push(Transmission {
        speaker: "ATC".into(),
        text: "Jetstar 789, hold short.".into(),
        background: true,
        ..serde_json::from_value(serde_json::json!({})).unwrap()
    });
    let mut reply = auto_reply(&state).unwrap();
    assert_eq!(reply.clearance_sequence, 6);
    assert_eq!(reply.readback_kind, "taxi");
    assert!(reply.text.contains("Taxi via"));
    assert!(!reply.text.contains("backtrack"));
    assert!(!reply.text.contains("5000"));
    assert!(reply.text.contains("Alpha"));
    assert!(!reply.text.contains("Jetstar"));
    reply.clearance_sequence = 0;
    prepare(&state, &mut reply);
    assert!(
        !reply.waypoint.starts_with("READBACK ERROR:"),
        "{}",
        reply.waypoint
    );
    state.taxi_clearance.pending_readback = false;
    state.clearance.as_mut().unwrap().acknowledged = true;
    assert!(auto_reply(&state).is_none());
}

#[test]
fn runway_reply_explicitly_identifies_entry_and_backtrack_runway() {
    let mut state = pending();
    state.taxi_clearance.pending_readback = true;
    state.taxi_clearance.sequence = 6;
    state.taxi_clearance.runway_taxi = true;
    state.taxi_clearance.entry_runway = "31".into();
    state.taxi_clearance.destination = "13".into();
    let reply = auto_reply(&state).unwrap();
    assert_eq!(reply.readback_kind, "taxi");
    assert!(reply.text.contains("via runway 31"));
    assert!(reply.text.contains("backtrack runway 13"));
    assert!(!reply.text.contains("altitude"));
}
