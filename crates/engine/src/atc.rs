//! Ownship registration and active ATC frequency membership.

use crate::{EngineState, Shared};
use axum::{Json, extract::State as AxumState};
use openatc_core::atc_manager::{
    Aircraft, AircraftManager, AtcManager, PILOT_ID, PilotAircraft, TrafficAircraft,
};
use openatc_core::state::TrafficTarget;
use openatc_core::stations::Station;
use serde_json::{Value, json};
use std::sync::Arc;

/// Keep the player's aircraft and tuned frequencies aligned with the latest telemetry.
pub(crate) fn sync_ownship(state: &mut EngineState) {
    let callsign = state.session.plan.callsign.trim().to_owned();
    sync_aircraft(
        &mut state.aircraft,
        &mut state.atc,
        &callsign,
        state.session.telemetry.radio_power,
        state.session.telemetry.com1_khz,
        state.session.telemetry.com2_khz,
        &openatc_core::stations::nearby(&state.stations, &state.session.telemetry),
    );
}

/// Keep observed traffic aircraft and their known radio channels in the managers.
///
/// An absent traffic snapshot leaves the previous membership untouched; an empty snapshot clears
/// every traffic aircraft.
pub(crate) fn sync_traffic(state: &mut EngineState) {
    let Some(targets) = state.session.telemetry.traffic.as_deref() else {
        return;
    };
    sync_traffic_aircraft(
        &mut state.aircraft,
        &mut state.atc,
        targets,
        &state.stations,
    );
}

fn sync_aircraft(
    aircraft_manager: &mut AircraftManager,
    atc: &mut AtcManager,
    callsign: &str,
    powered: bool,
    com1_khz: i32,
    com2_khz: Option<i32>,
    stations: &[Station],
) {
    if callsign.is_empty() {
        atc.leave_all(PILOT_ID);
        aircraft_manager.unregister(PILOT_ID);
        return;
    }

    let aircraft = match aircraft_manager.get(PILOT_ID) {
        Some(existing) if existing.callsign() == callsign => existing,
        _ => {
            let aircraft: Arc<dyn Aircraft> = Arc::new(PilotAircraft::new(PILOT_ID, callsign));
            aircraft_manager.register(aircraft.clone());
            aircraft
        }
    };

    let mut keep = Vec::new();
    if powered {
        for frequency in [Some(com1_khz), com2_khz].into_iter().flatten() {
            if frequency <= 0 || keep.contains(&frequency) {
                continue;
            }
            // The station index can hold tens of thousands of airports. Skip the scan when
            // the aircraft is already joined with the same identity.
            if atc
                .aircraft_from_id(frequency, PILOT_ID)
                .is_some_and(|joined| joined.callsign() == callsign)
            {
                keep.push(frequency);
                continue;
            }
            let Some(station) = stations
                .iter()
                .find(|station| station.khz == frequency)
                .cloned()
            else {
                continue;
            };
            keep.push(frequency);
            atc.join(frequency, station, aircraft.clone());
        }
    }
    atc.leave_all_except(PILOT_ID, &keep);
}

/// Pick the station at the aircraft's position, not the first station on the frequency.
fn station_for_target(
    stations: &[Station],
    target: &TrafficTarget,
    frequency: i32,
) -> Option<Station> {
    let telemetry = openatc_core::state::Telemetry {
        position_valid: true,
        latitude: target.latitude,
        longitude: target.longitude,
        altitude_feet: target.altitude_feet,
        on_ground: target.on_ground,
        com1_khz: frequency,
        ..Default::default()
    };
    openatc_core::stations::nearby(stations, &telemetry)
        .into_iter()
        .find(|station| station.khz == frequency)
}

fn sync_traffic_aircraft(
    aircraft_manager: &mut AircraftManager,
    atc: &mut AtcManager,
    targets: &[TrafficTarget],
    stations: &[Station],
) {
    let mut seen = Vec::new();
    for target in targets.iter().filter(|target| {
        target.id != 0
            && !target.callsign.is_empty()
            && openatc_core::traffic::advisory::valid(target)
    }) {
        seen.push(target.id);
        let aircraft = match aircraft_manager.get(target.id) {
            Some(existing) if existing.is_traffic() && existing.callsign() == target.callsign => {
                existing
            }
            _ => {
                let aircraft: Arc<dyn Aircraft> =
                    Arc::new(TrafficAircraft::new(target.id, &target.callsign));
                aircraft_manager.register(aircraft.clone());
                aircraft
            }
        };

        let mut keep = Vec::new();
        for frequency in [target.com1_khz, target.com2_khz].into_iter().flatten() {
            if frequency <= 0 || keep.contains(&frequency) {
                continue;
            }
            // The station index can hold tens of thousands of airports. Skip the scan when
            // the aircraft is already joined with the same identity.
            if atc
                .aircraft_from_id(frequency, target.id)
                .is_some_and(|joined| joined.callsign() == target.callsign)
            {
                keep.push(frequency);
                continue;
            }
            let Some(station) = station_for_target(stations, target, frequency) else {
                continue;
            };
            keep.push(frequency);
            atc.join(frequency, station, aircraft.clone());
        }
        atc.leave_all_except(target.id, &keep);
    }

    let stale: Vec<u32> = aircraft_manager
        .traffic()
        .map(|aircraft| aircraft.id())
        .filter(|id| !seen.contains(id))
        .collect();
    for id in stale {
        atc.leave_all(id);
        aircraft_manager.unregister(id);
    }
}

/// Read-only debug view of registered aircraft and active frequencies.
pub async fn debug_atc(AxumState(shared): AxumState<Shared>) -> Json<Value> {
    let state = shared.read().await;
    Json(snapshot(&state))
}

fn snapshot(state: &EngineState) -> Value {
    let mut aircraft: Vec<(u32, &str, &str)> = state
        .aircraft
        .iter()
        .map(|aircraft| (aircraft.id(), aircraft.callsign(), aircraft.kind().as_str()))
        .collect();
    aircraft.sort_by_key(|(id, _, _)| *id);
    let aircraft: Vec<Value> = aircraft
        .into_iter()
        .map(|(id, callsign, kind)| json!({"id": id, "callsign": callsign, "kind": kind}))
        .collect();

    let mut frequencies: Vec<i32> = state.atc.frequencies().collect();
    frequencies.sort_unstable();
    let frequencies: Vec<Value> = frequencies
        .into_iter()
        .filter_map(|frequency| {
            let channel = state.atc.channel(frequency)?;
            let mut members: Vec<(u32, &str, &str)> = channel
                .aircraft
                .values()
                .map(|aircraft| (aircraft.id(), aircraft.callsign(), aircraft.kind().as_str()))
                .collect();
            members.sort_by_key(|(id, _, _)| *id);
            let members: Vec<Value> = members
                .into_iter()
                .map(|(id, callsign, kind)| json!({"id": id, "callsign": callsign, "kind": kind}))
                .collect();
            Some(json!({
                "frequencyKhz": frequency,
                "station": {
                    "airport": channel.station.airport,
                    "name": channel.station.name,
                    "service": channel.station.service,
                },
                "aircraft": members,
                "last": channel.last,
            }))
        })
        .collect();

    json!({
        "aircraft": aircraft,
        "frequencies": frequencies,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn station(khz: i32, name: &str) -> Station {
        Station {
            khz,
            name: name.into(),
            service: "Tower".into(),
            airport: "TEST".into(),
            ..Station::default()
        }
    }

    fn target(id: u32, callsign: &str, com1_khz: i32) -> TrafficTarget {
        TrafficTarget {
            id,
            callsign: callsign.into(),
            com1_khz: Some(com1_khz),
            ..TrafficTarget::default()
        }
    }

    #[test]
    fn ownship_joins_and_moves_between_tuned_stations() {
        let stations = vec![
            station(118_700, "Test Tower"),
            station(121_900, "Test Ground"),
        ];
        let mut aircraft = AircraftManager::new();
        let mut atc = AtcManager::new();

        sync_aircraft(
            &mut aircraft,
            &mut atc,
            "TEST123",
            true,
            118_700,
            None,
            &stations,
        );
        assert!(aircraft.contains(PILOT_ID));
        assert_eq!(atc.frequency_count(), 1);
        assert_eq!(atc.aircraft_count(118_700), 1);

        sync_aircraft(
            &mut aircraft,
            &mut atc,
            "TEST123",
            true,
            121_900,
            None,
            &stations,
        );
        assert_eq!(atc.frequency_count(), 1);
        assert!(atc.channel(118_700).is_none());
        assert_eq!(atc.aircraft_count(121_900), 1);
    }

    #[test]
    fn unpowered_or_unknown_frequencies_leave_no_membership() {
        let stations = vec![station(118_700, "Test Tower")];
        let mut aircraft = AircraftManager::new();
        let mut atc = AtcManager::new();

        sync_aircraft(
            &mut aircraft,
            &mut atc,
            "TEST123",
            true,
            118_700,
            Some(118_700),
            &stations,
        );
        assert_eq!(atc.frequency_count(), 1);

        sync_aircraft(
            &mut aircraft,
            &mut atc,
            "TEST123",
            false,
            118_700,
            Some(118_700),
            &stations,
        );
        assert!(atc.is_empty());
        assert!(aircraft.contains(PILOT_ID));

        sync_aircraft(
            &mut aircraft,
            &mut atc,
            "TEST123",
            true,
            999_000,
            None,
            &stations,
        );
        assert!(atc.is_empty());
    }

    #[test]
    fn clearing_the_callsign_removes_the_ownship() {
        let stations = vec![station(118_700, "Test Tower")];
        let mut aircraft = AircraftManager::new();
        let mut atc = AtcManager::new();

        sync_aircraft(
            &mut aircraft,
            &mut atc,
            "TEST123",
            true,
            118_700,
            None,
            &stations,
        );
        sync_aircraft(&mut aircraft, &mut atc, "", true, 118_700, None, &stations);

        assert!(aircraft.is_empty());
        assert!(atc.is_empty());
    }

    #[test]
    fn observed_traffic_joins_its_known_channel_and_is_retired() {
        let stations = vec![station(118_700, "Test Tower")];
        let mut aircraft = AircraftManager::new();
        let mut atc = AtcManager::new();
        let targets = vec![
            target(7, "TRAFFIC7", 118_700),
            target(8, "TRAFFIC8", 118_700),
        ];

        sync_traffic_aircraft(&mut aircraft, &mut atc, &targets, &stations);

        assert_eq!(aircraft.traffic().count(), 2);
        assert_eq!(atc.aircraft_count(118_700), 2);
        assert_eq!(
            atc.aircraft_from_id(118_700, 7).expect("traffic").kind(),
            openatc_core::atc_manager::AircraftKind::Traffic
        );

        sync_traffic_aircraft(&mut aircraft, &mut atc, &targets[..1], &stations);

        assert_eq!(aircraft.traffic().count(), 1);
        assert_eq!(atc.aircraft_count(118_700), 1);
        assert!(atc.aircraft_from_id(118_700, 8).is_none());

        sync_traffic_aircraft(&mut aircraft, &mut atc, &[], &stations);

        assert!(aircraft.is_empty());
        assert!(atc.is_empty());
    }

    #[test]
    fn traffic_station_lookup_prefers_its_own_airport() {
        let mut nice = station(120_160, "Nice Tower");
        nice.airport = "LFMN".into();
        nice.latitude = 43.66;
        nice.longitude = 7.21;
        let mut other = station(120_160, "Kastrup Tower");
        other.airport = "EKCH".into();
        other.latitude = 55.62;
        other.longitude = 12.65;
        let mut traffic = target(7, "TEST7", 120_160);
        traffic.latitude = 43.66;
        traffic.longitude = 7.21;
        traffic.altitude_feet = 2_000.0;
        let found = station_for_target(&[other, nice], &traffic, 120_160)
            .expect("station at the traffic position");
        assert_eq!(found.airport, "LFMN");
    }

    #[test]
    fn traffic_without_a_matching_station_has_no_frequency_membership() {
        let stations = vec![station(118_700, "Test Tower")];
        let mut aircraft = AircraftManager::new();
        let mut atc = AtcManager::new();
        let targets = vec![target(7, "TRAFFIC7", 999_000)];

        sync_traffic_aircraft(&mut aircraft, &mut atc, &targets, &stations);

        assert_eq!(aircraft.traffic().count(), 1);
        assert!(atc.is_empty());
    }
}
