//! Observed liftoffs and departure wake delays, kept separate from radio chatter.
use crate::EngineState;
use openatc_core::{
    airport::Airport,
    state::{Telemetry, TrafficTarget},
    traffic::{
        runway_position,
        wake::{self, Departure},
    },
};
use std::{
    collections::{BTreeMap, VecDeque},
    time::Instant,
};

struct Liftoff {
    id: u32,
    airport: String,
    runway: String,
    heading: f64,
    category: Option<char>,
    wake_category: Option<u8>,
    aircraft: String,
    callsign: String,
    distance_pending: bool,
    at: f64,
}

struct ArrivalWake {
    airport: String,
    runway: String,
    required_nm: f64,
}

#[derive(Default)]
pub(super) struct Monitor {
    clock: Option<f64>,
    wall: Option<Instant>,
    elapsed: f64,
    previous: BTreeMap<u32, TrafficTarget>,
    departures: VecDeque<Liftoff>,
    arrival: Option<ArrivalWake>,
    unresolved_heavy: bool,
}

impl Monitor {
    pub(super) fn departure_delay(
        &self,
        id: u32,
        airport: &Airport,
        runway: &str,
        aircraft: &str,
        latitude: f64,
        longitude: f64,
    ) -> f64 {
        if !wake::supported(&airport.icao) {
            return 0.0;
        }
        let heavy_distance = airport.icao.starts_with('Y')
            && openatc_core::traffic::profiles::aircraft(aircraft).is_some_and(|a| a.wake == 'H');
        if heavy_distance && self.unresolved_heavy {
            return 1.0;
        }
        let Some(position) = runway_position(airport, runway, latitude, longitude) else {
            return 0.0;
        };
        self.departures
            .iter()
            .filter(|event| {
                event.id != id
                    && event.airport == airport.icao
                    && wake::related_runways(airport, &event.runway, runway, event.category)
            })
            .map(|event| {
                if heavy_distance && event.distance_pending {
                    let separated = self
                        .previous
                        .get(&event.id)
                        .filter(|t| {
                            !t.on_ground
                                && t.aircraft == event.aircraft
                                && t.callsign == event.callsign
                                && openatc_core::traffic::advisory::valid(t)
                        })
                        .and_then(|t| runway_position(airport, runway, t.latitude, t.longitude))
                        .is_some_and(|lead| lead.0 - position.0 >= 4.0 * 1852.0);
                    if !separated {
                        return 1.0;
                    }
                }
                let kind = if openatc_core::navigation::turn(event.heading, position.3).abs() > 90.0
                {
                    Departure::Opposite
                } else if position.0
                    > if airport.icao.starts_with('Y') {
                        150.0
                    } else {
                        152.4
                    }
                {
                    Departure::Intersection
                } else {
                    Departure::FullLength
                };
                // Unknown pairs do not invent a wake hold. The published table is leader-based:
                // a heavier follower behind a lighter leader needs no extra interval.
                let required = wake::departure_for_wake(
                    &airport.icao,
                    &event.aircraft,
                    event.wake_category,
                    aircraft,
                    None,
                    kind,
                )
                .unwrap_or(0);
                (f64::from(required) - (self.elapsed - event.at)).max(0.0)
            })
            .fold(0.0, f64::max)
    }

    fn arrival_conflict(
        &mut self,
        t: &Telemetry,
        targets: &[TrafficTarget],
        airport: Option<&Airport>,
        runway: &str,
        delta: f64,
    ) -> bool {
        let Some(airport) = airport.filter(|a| wake::supported(&a.icao)) else {
            self.arrival = None;
            return false;
        };
        let Some(own) =
            openatc_core::traffic::approach_position(airport, runway, t.latitude, t.longitude)
        else {
            self.arrival = None;
            return false;
        };
        if t.on_ground
            || own.0 > 0.0
            || own.1.abs() > 500.0
            || openatc_core::navigation::turn(
                t.ground_track_degrees.unwrap_or(t.heading_degrees),
                own.3,
            )
            .abs()
                > 30.0
        {
            self.arrival = None;
            return false;
        }
        if self.arrival.as_ref().is_some_and(|a| {
            a.airport != airport.icao || a.runway != runway || -own.0 / 1852.0 >= a.required_nm
        }) {
            self.arrival = None;
        }
        for target in targets.iter().take(126) {
            let Some(previous) = self
                .previous
                .get(&target.id)
                .filter(|p| !p.on_ground && p.callsign == target.callsign)
            else {
                continue;
            };
            let Some(leader_runway) = wake::approach_runway(airport, runway, target) else {
                continue;
            };
            let (Some(before), Some(after)) = (
                openatc_core::traffic::approach_position(
                    airport,
                    leader_runway,
                    previous.latitude,
                    previous.longitude,
                ),
                openatc_core::traffic::approach_position(
                    airport,
                    leader_runway,
                    target.latitude,
                    target.longitude,
                ),
            ) else {
                continue;
            };
            if before.0 >= 0.0
                || after.0 < 0.0
                || before.1.abs() > 500.0
                || after.1.abs() > 500.0
                || (target.altitude_feet - airport.elevation_feet) > 1000.0
                || openatc_core::navigation::turn(target.track_degrees, own.3).abs() > 30.0
            {
                continue;
            }
            let required_nm = wake::arrival_for_wake(
                &airport.icao,
                &target.aircraft,
                target.wake_category,
                &t.aircraft_icao,
                t.wake_category,
            )
            .unwrap_or(10.0);
            if required_nm == 0.0 {
                continue;
            }
            let elapsed_since_threshold = delta.max(0.0) * after.0 / (after.0 - before.0);
            let Some(relative) = openatc_core::traffic::approach_position(
                airport,
                runway,
                target.latitude,
                target.longitude,
            ) else {
                continue;
            };
            let along = relative.0 - after.0 - own.0
                + t.ground_speed_knots * elapsed_since_threshold * 1852.0 / 3600.0;
            let spacing = if along < 0.0 {
                0.0
            } else {
                along.hypot(relative.1 - own.1) / 1852.0
            };
            if spacing < required_nm {
                self.arrival = Some(ArrivalWake {
                    airport: airport.icao.clone(),
                    runway: runway.into(),
                    required_nm,
                });
            }
        }
        self.arrival.is_some()
    }
}

pub(super) fn observe(state: &mut EngineState) {
    let observed = state.traffic_targets();
    let t = &state.session.telemetry;
    let monitor = &mut state.traffic.wake;
    let now = Instant::now();
    let wall_delta = monitor
        .wall
        .replace(now)
        .map_or(0.0, |last| now.duration_since(last).as_secs_f64());
    let clock = t.simulation_seconds.filter(|v| v.is_finite() && *v >= 0.0);
    let delta = match (monitor.clock.replace(clock.unwrap_or(0.0)), clock) {
        (Some(before), Some(after)) => after - before,
        (_, None) => wall_delta,
        _ => 0.0,
    };
    if clock.is_none() {
        monitor.clock = None;
    }
    if !t.paused && (0.0..=30.0).contains(&delta) {
        monitor.elapsed += delta;
    }
    monitor
        .departures
        .retain(|d| d.distance_pending || monitor.elapsed - d.at < 240.0);
    if t.paused || !t.position_valid {
        return;
    }
    if !(0.0..=30.0).contains(&delta) {
        // A teleport or clock reset is not an observed liftoff or threshold crossing.
        monitor.previous.clear();
    }
    state.session.departure_wake_seconds = 0.0;
    state.session.arrival_wake_conflict = monitor.arrival_conflict(
        t,
        &observed,
        state.arrival_airport.as_ref(),
        &state.session.plan.arrival_runway,
        delta,
    );

    // Keep the ownship in the previous-position map even when the native feed is empty.
    {
        let own = TrafficTarget {
            id: 0,
            callsign: state.session.plan.callsign.clone(),
            aircraft: if t.aircraft_icao.is_empty() {
                state.session.plan.aircraft.clone()
            } else {
                t.aircraft_icao.clone()
            },
            latitude: t.latitude,
            longitude: t.longitude,
            altitude_feet: t.altitude_feet,
            track_degrees: t.ground_track_degrees.unwrap_or(t.heading_degrees),
            ground_speed_knots: Some(t.ground_speed_knots),
            on_ground: t.on_ground,
            ..Default::default()
        };
        let targets = observed
            .iter()
            .filter(|p| p.id != 0)
            .take(126)
            .chain(std::iter::once(&own));
        for target in targets.clone() {
            let lifted = !target.on_ground
                && monitor
                    .previous
                    .get(&target.id)
                    .is_some_and(|p| p.on_ground && p.callsign == target.callsign);
            if !lifted || !openatc_core::traffic::advisory::valid(target) {
                continue;
            }
            for airport in state
                .airport
                .iter()
                .chain(state.arrival_airport.iter())
                .chain(state.generated.airport.iter().map(AsRef::as_ref))
                .chain(state.generated.destination.iter().map(AsRef::as_ref))
                .filter(|a| wake::supported(&a.icao))
            {
                if let Some((runway, heading)) = airport.runways.iter().find_map(|r| {
                    let p =
                        runway_position(airport, &r.first_name, target.latitude, target.longitude)?;
                    if !(-100.0..=p.2 + 1000.0).contains(&p.0)
                        || p.1.abs() > r.width / 2.0 + 50.0
                        || (target.altitude_feet - airport.elevation_feet) > 600.0
                    {
                        return None;
                    }
                    let first =
                        openatc_core::navigation::turn(p.3, target.track_degrees).abs() < 90.0;
                    Some((
                        if first {
                            r.first_name.clone()
                        } else {
                            r.second_name.clone()
                        },
                        if first { p.3 } else { (p.3 + 180.0) % 360.0 },
                    ))
                }) {
                    monitor.departures.push_back(Liftoff {
                        id: target.id,
                        airport: airport.icao.clone(),
                        runway,
                        heading,
                        category: wake::category(&target.aircraft),
                        wake_category: target.wake_category,
                        aircraft: target.aircraft.clone(),
                        callsign: target.callsign.clone(),
                        distance_pending: airport.icao.starts_with('Y')
                            && openatc_core::traffic::profiles::aircraft(&target.aircraft)
                                .is_some_and(|a| a.wake == 'H'),
                        at: monitor.elapsed,
                    });
                    break;
                }
            }
        }
        monitor.previous = targets.map(|p| (p.id, p.clone())).collect();
        for event in monitor.departures.iter_mut().filter(|e| e.distance_pending) {
            let airport = state
                .airport
                .iter()
                .chain(state.arrival_airport.iter())
                .chain(state.generated.airport.iter().map(AsRef::as_ref))
                .chain(state.generated.destination.iter().map(AsRef::as_ref))
                .find(|a| a.icao == event.airport);
            if let (Some(airport), Some(target)) = (airport, monitor.previous.get(&event.id))
                && target.aircraft == event.aircraft
                && target.callsign == event.callsign
                && !target.on_ground
                && openatc_core::traffic::advisory::valid(target)
                && runway_position(airport, &event.runway, target.latitude, target.longitude)
                    .is_some_and(|p| {
                        p.0 > p.2 + 5.0 * 1852.0 || p.0 < -5.0 * 1852.0 || p.1.abs() > 5.0 * 1852.0
                    })
            {
                event.distance_pending = false;
            }
        }
    }
    while monitor.departures.len() > 64 {
        // Bounded history must not turn lost distance-only wake evidence into permission.
        monitor.unresolved_heavy |= monitor
            .departures
            .pop_front()
            .is_some_and(|e| e.distance_pending);
    }
    if !t.on_ground {
        return;
    }
    let Some(airport) = state
        .airport
        .as_ref()
        .filter(|a| a.icao == state.session.plan.departure)
    else {
        return;
    };
    state.session.departure_wake_seconds = monitor.departure_delay(
        0,
        airport,
        &state.session.plan.runway,
        if t.aircraft_icao.is_empty() {
            &state.session.plan.aircraft
        } else {
            &t.aircraft_icao
        },
        t.latitude,
        t.longitude,
    );
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use openatc_core::{
        airport::{Airport, Runway},
        state::{Point, Telemetry},
    };
    #[test]
    fn touching_down_or_losing_the_leader_does_not_erase_a_threshold_wake_conflict() {
        for (spacing, conflict) in [(2.0, true), (5.0, false)] {
            let mut s = crate::test_state();
            s.session.plan.arrival_runway = "09".into();
            s.arrival_airport = Some(Airport {
                icao: "KTEST".into(),
                runways: vec![Runway {
                    first_name: "09".into(),
                    second_name: "27".into(),
                    width: 45.0,
                    second: Point {
                        east: 3000.0,
                        ..Default::default()
                    },
                    ..Default::default()
                }],
                ..Default::default()
            });
            s.session.telemetry = Telemetry {
                position_valid: true,
                aircraft_icao: "P28A".into(),
                on_ground: false,
                longitude: -spacing / 60.0,
                ground_speed_knots: 100.0,
                heading_degrees: 90.0,
                simulation_seconds: Some(10.0),
                traffic: Some(vec![TrafficTarget {
                    id: 2,
                    callsign: "TEST2".into(),
                    aircraft: "A320".into(),
                    longitude: -0.0001,
                    altitude_feet: 30.0,
                    track_degrees: 90.0,
                    on_ground: false,
                    ..Default::default()
                }]),
                ..Default::default()
            };
            observe(&mut s);
            let leader = &mut s.session.telemetry.traffic.as_mut().unwrap()[0];
            leader.longitude = 0.0001;
            leader.on_ground = true;
            s.session.telemetry.simulation_seconds = Some(11.0);
            observe(&mut s);
            assert_eq!(s.session.arrival_wake_conflict, conflict);
            s.session.telemetry.traffic = Some(vec![]);
            s.session.telemetry.simulation_seconds = Some(12.0);
            observe(&mut s);
            assert_eq!(s.session.arrival_wake_conflict, conflict);
            s.session.telemetry.heading_degrees = 0.0;
            observe(&mut s);
            assert!(!s.session.arrival_wake_conflict);
        }
    }

    #[test]
    fn an_ambient_frame_participates_in_arrival_wake_spacing() {
        let mut s = crate::test_state();
        s.session.plan.arrival_runway = "09".into();
        s.arrival_airport = Some(Airport {
            icao: "KTEST".into(),
            runways: vec![Runway {
                first_name: "09".into(),
                second_name: "27".into(),
                width: 45.0,
                second: Point {
                    east: 3000.0,
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        });
        s.session.telemetry = Telemetry {
            position_valid: true,
            aircraft_icao: "C172".into(),
            on_ground: false,
            longitude: -4.0 / 60.0,
            heading_degrees: 90.0,
            ground_speed_knots: 90.0,
            simulation_seconds: Some(10.0),
            traffic: Some(Vec::new()),
            ..Default::default()
        };
        let ambient = |longitude: f64| openatc_core::state::GeneratedTraffic {
            vertical_speed_fpm: 0.0,
            body_heading_degrees: None,
            target: TrafficTarget {
                id: 2,
                callsign: "HEAVY2".into(),
                aircraft: "B77W".into(),
                longitude,
                altitude_feet: 30.0,
                track_degrees: 90.0,
                wingspan_meters: Some(60.0),
                wake_category: Some(2),
                on_ground: false,
                ..Default::default()
            },
            model: std::sync::Arc::new(openatc_core::traffic::csl::Model {
                id: "heavy".into(),
                objects: Vec::new(),
                vertical_offset: None,
                matches: Vec::new(),
            }),
            animation: Default::default(),
        };
        s.session.generated_traffic = vec![ambient(-0.0001)];
        observe(&mut s);
        s.session.generated_traffic = vec![ambient(0.0001)];
        s.session.telemetry.simulation_seconds = Some(11.0);
        observe(&mut s);
        assert!(s.session.arrival_wake_conflict);
    }

    #[test]
    fn threshold_observation_uses_the_leaders_displaced_parallel_threshold() {
        let mut s = crate::test_state();
        s.session.plan.arrival_runway = "09L".into();
        s.arrival_airport = Some(Airport {
            icao: "KTEST".into(),
            runways: vec![
                Runway {
                    first_name: "09L".into(),
                    second_name: "27R".into(),
                    width: 45.0,
                    second: Point {
                        east: 3000.0,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                Runway {
                    first_name: "09R".into(),
                    second_name: "27L".into(),
                    width: 45.0,
                    first: Point {
                        north: 500.0,
                        ..Default::default()
                    },
                    second: Point {
                        east: 3000.0,
                        north: 500.0,
                        ..Default::default()
                    },
                    first_displaced_meters: 500.0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        s.session.telemetry = Telemetry {
            position_valid: true,
            on_ground: false,
            aircraft_icao: "P28A".into(),
            longitude: -2.0 / 60.0,
            heading_degrees: 90.0,
            simulation_seconds: Some(1.0),
            traffic: Some(vec![TrafficTarget {
                id: 2,
                callsign: "TEST2".into(),
                aircraft: "A320".into(),
                latitude: 500.0 / 111_320.0,
                longitude: -0.0001,
                track_degrees: 90.0,
                on_ground: false,
                ..Default::default()
            }]),
            ..Default::default()
        };
        observe(&mut s);
        s.session.telemetry.traffic.as_mut().unwrap()[0].longitude = 0.0001;
        s.session.telemetry.simulation_seconds = Some(2.0);
        observe(&mut s);
        assert!(
            !s.session.arrival_wake_conflict,
            "Physical pavement start is not the landing threshold"
        );
        s.session.telemetry.traffic.as_mut().unwrap()[0].longitude = 510.0 / 111_320.0;
        s.session.telemetry.simulation_seconds = Some(7.0);
        observe(&mut s);
        assert!(s.session.arrival_wake_conflict);
    }
    #[test]
    fn observed_heavy_departure_delays_takeoff_and_pause_does_not_age_wake() {
        for airport in ["KTEST", "YSSY"] {
            let mut s = crate::test_state();
            s.session.plan.departure = airport.into();
            s.session.plan.runway = "09".into();
            s.airport = Some(Airport {
                icao: airport.into(),
                runways: vec![Runway {
                    first_name: "09".into(),
                    second_name: "27".into(),
                    width: 45.0,
                    second: Point {
                        east: 3000.0,
                        ..Default::default()
                    },
                    ..Default::default()
                }],
                ..Default::default()
            });
            let target = TrafficTarget {
                id: 42,
                callsign: "TEST42".into(),
                aircraft: "B77W".into(),
                longitude: 0.01,
                track_degrees: 90.0,
                on_ground: true,
                ..Default::default()
            };
            s.session.telemetry = Telemetry {
                aircraft_icao: "A320".into(),
                position_valid: true,
                simulation_seconds: Some(10.0),
                traffic: Some(vec![target]),
                ..Default::default()
            };
            observe(&mut s);
            s.session.telemetry.simulation_seconds = Some(11.0);
            s.session.telemetry.traffic.as_mut().unwrap()[0].on_ground = false;
            observe(&mut s);
            assert_eq!(s.session.departure_wake_seconds, 120.0);
            s.session.telemetry.paused = true;
            s.session.telemetry.simulation_seconds = Some(200.0);
            observe(&mut s);
            s.session.telemetry.paused = false;
            observe(&mut s);
            assert_eq!(s.session.departure_wake_seconds, 120.0);
            for i in 1..=12 {
                s.session.telemetry.simulation_seconds = Some(200.0 + f64::from(i) * 10.0);
                observe(&mut s);
            }
            assert_eq!(s.session.departure_wake_seconds, 0.0);
            if airport == "YSSY" {
                s.session.telemetry.aircraft_icao = "B744".into();
                s.session.telemetry.simulation_seconds = Some(330.0);
                observe(&mut s);
                assert!(
                    s.session.departure_wake_seconds > 0.0,
                    "heavy-heavy is distance-only"
                );
                s.session.telemetry.traffic = Some(vec![]);
                for i in 1..=30 {
                    s.session.telemetry.simulation_seconds = Some(330.0 + f64::from(i) * 10.0);
                    observe(&mut s);
                }
                assert!(
                    s.session.departure_wake_seconds > 0.0,
                    "missing leader cannot satisfy distance"
                );
                s.session.telemetry.traffic = Some(vec![TrafficTarget {
                    id: 42,
                    callsign: "TEST42".into(),
                    aircraft: "B77W".into(),
                    longitude: 4.5 / 60.0,
                    track_degrees: 90.0,
                    on_ground: false,
                    ..Default::default()
                }]);
                observe(&mut s);
                assert_eq!(s.session.departure_wake_seconds, 0.0);
            }
        }
    }
}
