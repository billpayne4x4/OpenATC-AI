//! Departure and arrival route tracking.
//!
//! A filed route is not always a published procedure. An aircraft may depart
//! without a SID, or arrive without a STAR, and the first en-route leg is then
//! not the path it was cleared to fly. This module decides whether lateral
//! tracking is meaningful yet, so the off-plan rule does not report a normal
//! turn onto course as a deviation.

use crate::airport::Airport;
use crate::deviations::Settings;
use crate::state::{FlightPlan, PhaseCode, Telemetry};

/// How the departure is being navigated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepartureMode {
    /// A published SID is filed and known at the departure airport.
    Sid,
    /// No usable SID: own navigation with a join-in grace.
    NoSid,
    /// The departure uses radar vectors rather than a pilot-nav path.
    Vectors,
}

/// How the arrival is being navigated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrivalMode {
    /// A published STAR is filed and known at the arrival airport.
    Star,
    /// No usable STAR: vectors or a direct join to the approach.
    NoStar,
    /// The arrival is on vectors and has no published path to track.
    Vectors,
}

/// Current route tracking state for one sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteStatus {
    /// Departure navigation mode.
    pub departure: DepartureMode,
    /// Arrival navigation mode.
    pub arrival: ArrivalMode,
    /// Lateral tracking is not yet meaningful after departure.
    pub departure_grace: bool,
    /// Lateral tracking is not yet meaningful before the arrival join.
    pub arrival_grace: bool,
}

impl RouteStatus {
    /// Assess the plan, the current position and the installed procedures.
    #[must_use]
    pub fn assess(
        plan: &FlightPlan,
        departure: Option<&Airport>,
        arrival: Option<&Airport>,
        telemetry: &Telemetry,
        next_fix: usize,
        approach_cleared: bool,
        settings: &Settings,
    ) -> Self {
        let departure_mode = departure_mode(plan, departure);
        let arrival_mode = arrival_mode(plan, arrival);
        let departure_grace = departure_grace(
            plan,
            departure_mode,
            departure,
            telemetry,
            next_fix,
            settings,
        );
        let arrival_grace = arrival_grace(
            plan,
            arrival_mode,
            arrival,
            telemetry,
            approach_cleared,
            settings,
        );
        Self {
            departure: departure_mode,
            arrival: arrival_mode,
            departure_grace,
            arrival_grace,
        }
    }

    /// Whether the off-plan rule should stay quiet for this phase.
    #[must_use]
    pub fn grace(&self, phase: PhaseCode) -> bool {
        match phase {
            PhaseCode::Departure => self.departure_grace,
            PhaseCode::Arrival => self.arrival_grace,
            _ => false,
        }
    }
}

fn departure_mode(plan: &FlightPlan, airport: Option<&Airport>) -> DepartureMode {
    if plan.sid_has_vectors {
        return DepartureMode::Vectors;
    }
    if plan.fixes.len() < 2 {
        return DepartureMode::NoSid;
    }
    if plan.sid.trim().is_empty() || !procedure_known(airport, "SID", &plan.sid) {
        DepartureMode::NoSid
    } else {
        DepartureMode::Sid
    }
}

fn arrival_mode(plan: &FlightPlan, airport: Option<&Airport>) -> ArrivalMode {
    if plan.star.trim().is_empty() || !procedure_known(airport, "STAR", &plan.star) {
        ArrivalMode::NoStar
    } else {
        ArrivalMode::Star
    }
}

/// An empty procedure list means the airport data is unavailable, not that
/// every filed procedure is invalid. Only reject a name when the airport has
/// procedure records and none of them match.
fn procedure_known(airport: Option<&Airport>, kind: &str, name: &str) -> bool {
    let Some(airport) = airport else {
        return true;
    };
    if airport.procedures.is_empty() {
        return true;
    }
    airport
        .procedures
        .iter()
        .any(|procedure| procedure.kind == kind && procedure.name == name)
}

fn departure_grace(
    plan: &FlightPlan,
    mode: DepartureMode,
    airport: Option<&Airport>,
    telemetry: &Telemetry,
    next_fix: usize,
    settings: &Settings,
) -> bool {
    if plan.fixes.len() < 2 || !telemetry.position_valid || telemetry.on_ground {
        return true;
    }
    match mode {
        DepartureMode::Sid => false,
        DepartureMode::Vectors => true,
        DepartureMode::NoSid => {
            // The first fix has been passed: the aircraft is on the en-route
            // structure and normal tracking applies.
            if next_fix > 1 {
                return false;
            }
            let Some(airport) = airport else {
                return true;
            };
            let distance = crate::arrival::distance_nm(
                [telemetry.latitude, telemetry.longitude],
                [airport.reference_latitude, airport.reference_longitude],
            );
            let agl = telemetry.altitude_feet - airport.elevation_feet;
            distance < settings.departure_grace_nm || agl < settings.departure_grace_altitude_ft
        }
    }
}

fn arrival_grace(
    plan: &FlightPlan,
    mode: ArrivalMode,
    airport: Option<&Airport>,
    telemetry: &Telemetry,
    approach_cleared: bool,
    settings: &Settings,
) -> bool {
    if plan.fixes.len() < 2 || !telemetry.position_valid {
        return true;
    }
    if approach_cleared {
        return true;
    }
    match mode {
        ArrivalMode::Star => false,
        ArrivalMode::Vectors => true,
        ArrivalMode::NoStar => {
            let Some(airport) = airport else {
                return true;
            };
            crate::arrival::distance_nm(
                [telemetry.latitude, telemetry.longitude],
                [airport.reference_latitude, airport.reference_longitude],
            ) < settings.arrival_grace_nm
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::airport::Procedure;
    use crate::state::RouteFix;

    fn airport(icao: &str, elevation_feet: f64, procedures: Vec<(&str, &str)>) -> Airport {
        Airport {
            icao: icao.into(),
            reference_latitude: 0.0,
            reference_longitude: 0.0,
            elevation_feet,
            procedures: procedures
                .into_iter()
                .map(|(kind, name)| Procedure {
                    kind: kind.into(),
                    name: name.into(),
                    transition: String::new(),
                })
                .collect(),
            ..Default::default()
        }
    }

    fn plan(sid: &str, star: &str) -> FlightPlan {
        FlightPlan {
            sid: sid.into(),
            star: star.into(),
            fixes: vec![
                RouteFix {
                    identifier: "ORIGIN".into(),
                    latitude: 0.0,
                    longitude: 0.0,
                    ..Default::default()
                },
                RouteFix {
                    identifier: "FIRST".into(),
                    latitude: 0.0,
                    longitude: 1.0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    fn telemetry(latitude: f64, altitude_feet: f64) -> Telemetry {
        Telemetry {
            latitude,
            longitude: 0.0,
            altitude_feet,
            position_valid: true,
            on_ground: false,
            ..Default::default()
        }
    }

    #[test]
    fn no_sid_gets_departure_grace_near_the_airport() {
        let status = RouteStatus::assess(
            &plan("", ""),
            Some(&airport("TEST", 0.0, Vec::new())),
            None,
            &telemetry(0.02, 1500.0),
            1,
            false,
            &Settings::default(),
        );
        assert_eq!(status.departure, DepartureMode::NoSid);
        assert!(status.departure_grace);
    }

    #[test]
    fn no_sid_grace_ends_past_the_thresholds_or_the_first_fix() {
        let settings = Settings::default();
        let departure = airport("TEST", 0.0, Vec::new());
        let far = RouteStatus::assess(
            &plan("", ""),
            Some(&departure),
            None,
            &telemetry(0.25, 6000.0),
            1,
            false,
            &settings,
        );
        assert!(!far.departure_grace);
        let passed = RouteStatus::assess(
            &plan("", ""),
            Some(&departure),
            None,
            &telemetry(0.02, 1500.0),
            2,
            false,
            &settings,
        );
        assert!(!passed.departure_grace);
    }

    #[test]
    fn a_known_sid_tracks_from_the_start() {
        let status = RouteStatus::assess(
            &plan("ABC1", ""),
            Some(&airport("TEST", 0.0, vec![("SID", "ABC1")])),
            None,
            &telemetry(0.02, 1500.0),
            1,
            false,
            &Settings::default(),
        );
        assert_eq!(status.departure, DepartureMode::Sid);
        assert!(!status.departure_grace);
    }

    #[test]
    fn an_unknown_sid_falls_back_to_own_navigation() {
        let status = RouteStatus::assess(
            &plan("OLD1", ""),
            Some(&airport("TEST", 0.0, vec![("SID", "NEW1")])),
            None,
            &telemetry(0.02, 1500.0),
            1,
            false,
            &Settings::default(),
        );
        assert_eq!(status.departure, DepartureMode::NoSid);
        assert!(status.departure_grace);
    }

    #[test]
    fn a_vector_sid_never_tracks_a_pilot_nav_path() {
        let mut p = plan("ABC1", "");
        p.sid_has_vectors = true;
        let status = RouteStatus::assess(
            &p,
            Some(&airport("TEST", 0.0, vec![("SID", "ABC1")])),
            None,
            &telemetry(0.25, 6000.0),
            1,
            false,
            &Settings::default(),
        );
        assert_eq!(status.departure, DepartureMode::Vectors);
        assert!(status.departure_grace);
    }

    #[test]
    fn no_star_gets_arrival_grace_near_the_destination() {
        let status = RouteStatus::assess(
            &plan("", ""),
            None,
            Some(&airport("DEST", 0.0, Vec::new())),
            &telemetry(0.1, 5000.0),
            1,
            false,
            &Settings::default(),
        );
        assert_eq!(status.arrival, ArrivalMode::NoStar);
        assert!(status.arrival_grace);
    }

    #[test]
    fn a_cleared_approach_keeps_the_arrival_quiet() {
        let status = RouteStatus::assess(
            &plan("", ""),
            None,
            Some(&airport("DEST", 0.0, Vec::new())),
            &telemetry(0.5, 3000.0),
            1,
            true,
            &Settings::default(),
        );
        assert!(status.arrival_grace);
    }
}
