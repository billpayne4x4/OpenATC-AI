//! Stand availability can change between a taxi clearance and reaching the gate.
use crate::EngineState;
#[cfg(test)]
use openatc_core::aircraft::AircraftBehaviour;
use openatc_core::{apply::add_transmission, dialogue::say, state::PhaseCode, stations};
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Monitor {
    last: Option<Instant>,
}

pub fn tick(state: &mut EngineState) {
    let route = &state.session.taxi_clearance;
    if state.session.phase != PhaseCode::TaxiIn
        || !route.to_parking
        || !route.approved
        || route.pending_readback
        || !route.crossing_runway.is_empty()
        || state.session.telemetry.paused
        || !state.session.telemetry.on_ground
        || !state.session.telemetry.position_valid
        || !state.session.telemetry.radio_power
        || state
            .session
            .transcript
            .iter()
            .rev()
            .find(|t| t.is_controller_instruction())
            .is_some_and(|t| !t.pilot_reply.is_empty())
        || state
            .parking
            .last
            .is_some_and(|last| last.elapsed() < Duration::from_secs(5))
    {
        return;
    }
    let nearby = stations::nearby(&state.stations, &state.session.telemetry);
    let Some(station) = state
        .session
        .control_frequency()
        .and_then(|frequency| stations::tuned(&nearby, frequency))
        .filter(|s| {
            s.airport == route.airport && ["Ground", "Tower"].contains(&s.service.as_str())
        })
    else {
        return;
    };
    let Some(airport) = state.airport.as_ref().filter(|a| a.icao == route.airport) else {
        return;
    };
    let Some(size) = openatc_core::parking::aircraft_size(&state.session) else {
        return;
    };
    state.parking.last = Some(Instant::now());
    let available = airport
        .parking
        .iter()
        .find(|p| p.name == route.destination)
        .and_then(|p| openatc_core::parking::occupied(airport, p, &state.session.telemetry, size))
        .map(|occupied| !occupied);
    if available == Some(true) && !route.parking_hold {
        return;
    }
    if available != Some(true) && !route.parking_hold {
        let stand = route.destination.clone();
        state.session.taxi_clearance.parking_hold = true;
        let voice = crate::flight::tag(state, station);
        let text = say(
            if available == Some(false) {
                "parking_became_occupied"
            } else {
                "parking_availability_unconfirmed"
            },
            &[
                ("callsign", state.session.plan.callsign.clone()),
                ("stand", stand),
            ],
        );
        add_transmission(&mut state.session, "ATC", &text, &voice);
        return;
    }
    // Replanning starts from a stationary position and retains every runway hold.
    if state.session.telemetry.ground_speed_knots > 1.0 {
        return;
    }
    let replacement = openatc_core::parking::candidates(airport, &state.session, size)
        .into_iter()
        .find_map(|stand| {
            openatc_core::airport::calculate_taxi_route(
                airport,
                &state.session.telemetry,
                &stand.name,
                true,
                size,
            )
            .ok()
            .map(|r| (stand.name.clone(), r))
        });
    let Some((stand, mut route)) = replacement else {
        return;
    };
    route.sequence = state.session.next_sequence;
    route.pending_readback = true;
    let slots = [
        ("callsign", state.session.plan.callsign.clone()),
        ("stand", stand.clone()),
        ("instructions", route.instructions.clone()),
    ];
    state.session.plan.arrival_stand = stand;
    state.session.taxi_clearance = route;
    let voice = crate::flight::tag(state, station);
    add_transmission(
        &mut state.session,
        "ATC",
        &say("parking_reassigned", &slots),
        &voice,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn occupied_gate_holds_then_reassigns_without_removing_traffic() {
        let mut s = crate::test_state();
        let mut airport = openatc_core::airport::load_airport(
            include_str!("../../../xtask/fixtures/flight-apt.dat"),
            "DEST",
        )
        .unwrap();
        let mut alternate = airport.parking[0].clone();
        alternate.name = "Gate C2".into();
        alternate.point.north += 200.0;
        airport.parking.push(alternate);
        s.session.demo = false;
        s.session
            .on_event(openatc_core::aircraft::AircraftEvent::TaxiIn);
        s.session.plan.aircraft = "A320".into();
        s.session.plan.destination = "DEST".into();
        s.session.telemetry = openatc_core::state::Telemetry {
            latitude: 0.0042,
            longitude: 4.012,
            position_valid: true,
            com1_khz: 121_600,
            traffic: Some(vec![]),
            ..Default::default()
        };
        let mut route = openatc_core::airport::calculate_taxi_route(
            &airport,
            &s.session.telemetry,
            "Gate C1",
            true,
            'C',
        )
        .unwrap();
        route.approved = true;
        route.pending_readback = false;
        s.session.taxi_clearance = route;
        s.stations = vec![stations::Station {
            airport: "DEST".into(),
            name: "Arrival Ground".into(),
            service: "Ground".into(),
            khz: 121_600,
            latitude: 0.004,
            longitude: 4.012,
            ..Default::default()
        }];
        s.airport = Some(airport);
        s.session.telemetry.traffic = Some(vec![openatc_core::state::TrafficTarget {
            latitude: 0.006,
            longitude: 4.012,
            on_ground: true,
            ..Default::default()
        }]);
        tick(&mut s);
        assert!(s.session.taxi_clearance.parking_hold);
        s.parking.last = None;
        tick(&mut s);
        assert_eq!(s.session.plan.arrival_stand, "Gate C2");
        assert!(s.session.taxi_clearance.pending_readback);
        assert!(!s.session.taxi_clearance.approved);
        assert_eq!(s.session.telemetry.traffic.as_ref().unwrap().len(), 1);
        let mut t = s.session.telemetry.clone();
        t.latitude = 0.006;
        s.session.taxi_clearance.destination = "Gate C1".into();
        s.session.taxi_clearance.approved = true;
        openatc_core::ops::update_flight_phase(&mut s.session, &t).unwrap();
        assert_ne!(s.session.phase, PhaseCode::Finished);
    }
}
