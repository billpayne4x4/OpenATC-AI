//! Select compatible terminal inputs before building physical flight paths.

#![allow(clippy::default_trait_access)]
#[allow(unused_imports, clippy::wildcard_imports)]
use super::*;
use super::{Airport, BTreeMap, BTreeSet};
use openatc_core::traffic::{
    coordinates::Points,
    procedures::{Catalogue, Leg},
};

#[derive(Clone)]
pub(super) struct Terminal {
    pub enroute_from: [f64; 2],
    pub enroute_to: [f64; 2],
    pub enroute_minimum_feet: i32,
    pub departure_name: String,
    pub departure: Vec<Leg>,
    pub arrival_name: String,
    pub arrival_transition: String,
    pub arrival: Vec<Leg>,
    pub approach_name: String,
    pub approach_transition: String,
    pub approach: Vec<Leg>,
    pub missed: Vec<Leg>,
    /// Published manual termination closing the arrival, which needs an explicit amendment.
    pub arrival_manual: Option<openatc_core::traffic::resolved::ManualTermination>,
    /// Enter the approach directly from an enroute fix instead of flying a published STAR.
    /// Used where a category has no arrival procedure inside its own operating envelope.
    pub direct: bool,
}

pub(super) fn supported(legs: &[Leg], missed: bool) -> bool {
    !legs.is_empty()
        && legs.len() <= 128
        && legs.iter().all(|leg| {
            matches!(
                leg.terminator.as_str(),
                "IF" | "TF" | "DF" | "CF" | "CA" | "FA" | "VA" | "VI" | "CI"
            ) || (missed && leg.terminator == "HM")
        })
}

/// A route may also end at a published manual termination, provided every leg before it is
/// computable. The manual transition then needs an explicit, read-back route amendment.
fn manual_tail(legs: &[Leg]) -> Option<openatc_core::traffic::resolved::ManualTermination> {
    let (last, prefix) = legs.split_last()?;
    if !openatc_core::traffic::resolved::MANUAL_TERMINATORS.contains(&last.terminator.as_str()) {
        return None;
    }
    supported(prefix, false).then(|| openatc_core::traffic::resolved::ManualTermination {
        terminator: last.terminator.clone(),
        identifier: last.identifier.clone(),
    })
}

/// A manually terminated arrival is planned only as an explicit amendment, never silently.
fn amendable(legs: &[Leg]) -> bool {
    supported(legs, false) || manual_tail(legs).is_some()
}
fn coordinate(leg: &Leg, airport: &Airport, points: &Points) -> Option<[f64; 2]> {
    Points::runway(leg, airport)
        .or_else(|| points.resolve(leg, &airport.icao))
        .map(|point| point.coordinate)
}
fn assemble(parts: Vec<&[Leg]>) -> Vec<Leg> {
    // Retain repeated fixes and their restrictions; geometry resolves coincident crossings later.
    parts.into_iter().flatten().cloned().collect()
}
fn variants(catalogue: &Catalogue, kind: &str, name: &str) -> Vec<String> {
    let transitions: BTreeSet<_> = catalogue
        .0
        .keys()
        .filter(|key| {
            key.kind == kind
                && key.name == name
                && !key.transition.is_empty()
                && !key.transition.starts_with("RW")
                && key.transition != "ALL"
        })
        .map(|key| key.transition.clone())
        .collect();
    if transitions.is_empty() {
        vec![String::new()]
    } else {
        transitions.into_iter().collect()
    }
}

pub(super) struct Inputs<'a> {
    pub origin: &'a Airport,
    pub destination: &'a Airport,
    pub departure_runway: &'a str,
    pub arrival_runway: &'a str,
    pub departures: &'a Catalogue,
    pub arrivals: &'a Catalogue,
    pub points: &'a Points,
    pub mora: &'a openatc_core::navigation::Mora,
    pub budget: usize,
    /// Build approach-only candidates for a category that cannot fly the published STARs.
    pub direct: bool,
}

/// Rank runway-compatible procedures with an explicit STAR-to-approach join.
/// These candidates are not movement permissions or a terrain-checked enroute route.
pub(super) fn candidates(input: &Inputs<'_>) -> Result<Vec<Terminal>, String> {
    if !(1..=32).contains(&input.budget)
        || input.departure_runway.is_empty()
        || input.arrival_runway.is_empty()
    {
        return Err("Terminal selection needs both runways and a bounded candidate budget".into());
    }
    let departure_names: BTreeSet<_> = input
        .departures
        .0
        .keys()
        .filter(|key| key.kind == "SID")
        .map(|key| key.name.clone())
        .collect();
    let arrival_names: BTreeSet<_> = input
        .arrivals
        .0
        .keys()
        .filter(|key| key.kind == "STAR")
        .map(|key| key.name.clone())
        .collect();
    let approach_names: BTreeSet<_> = input
        .arrivals
        .0
        .keys()
        .filter(|key| key.kind == "APPCH")
        .map(|key| key.name.clone())
        .collect();
    let destination = [
        input.destination.reference_latitude,
        input.destination.reference_longitude,
    ];
    let origin = [
        input.origin.reference_latitude,
        input.origin.reference_longitude,
    ];
    let mut departures = Vec::new();
    for name in departure_names {
        for transition in variants(input.departures, "SID", &name) {
            let Ok(parts) = input
                .departures
                .departure(&name, input.departure_runway, &transition)
            else {
                continue;
            };
            let legs = assemble(parts);
            if !supported(&legs, false) {
                continue;
            }
            let Some(exit) = legs
                .last()
                .and_then(|leg| coordinate(leg, input.origin, input.points))
            else {
                continue;
            };
            departures.push((
                openatc_core::arrival::distance_nm(exit, destination),
                name.clone(),
                transition,
                legs,
                exit,
            ));
            departures.sort_by(|a, b| {
                a.0.total_cmp(&b.0)
                    .then_with(|| a.1.cmp(&b.1))
                    .then_with(|| a.2.cmp(&b.2))
            });
            departures.truncate(input.budget);
        }
    }
    departures.sort_by(|a, b| {
        a.0.total_cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    departures.truncate(input.budget);
    let mut arrivals = Vec::new();
    for name in arrival_names {
        for transition in variants(input.arrivals, "STAR", &name) {
            let Ok(parts) = input
                .arrivals
                .arrival(&name, input.arrival_runway, &transition)
            else {
                continue;
            };
            let legs = assemble(parts);
            if !amendable(&legs) {
                continue;
            }
            // A manual termination publishes no coordinate, so the arrival ends at its last
            // computable crossing and an explicit amendment bridges to the approach entry.
            let manual = manual_tail(&legs);
            let published = manual
                .as_ref()
                .map_or(legs.as_slice(), |_| &legs[..legs.len().saturating_sub(1)]);
            let Some(entry) = published
                .first()
                .and_then(|leg| coordinate(leg, input.destination, input.points))
            else {
                continue;
            };
            let Some(exit) = published
                .last()
                .and_then(|leg| coordinate(leg, input.destination, input.points))
            else {
                continue;
            };
            for approach_name in &approach_names {
                let transitions: BTreeSet<_> = input
                    .arrivals
                    .0
                    .keys()
                    .filter(|key| {
                        key.kind == "APPCH" && &key.name == approach_name && key.route_type == "A"
                    })
                    .map(|key| key.transition.clone())
                    .chain(std::iter::once(String::new()))
                    .collect();
                for approach_transition in transitions {
                    let Ok(approach) = input.arrivals.approach(approach_name, &approach_transition)
                    else {
                        continue;
                    };
                    if approach
                        .landing
                        .last()
                        .is_none_or(|leg| leg.identifier != format!("RW{}", input.arrival_runway))
                    {
                        continue;
                    }
                    let assembled = approach
                        .transition
                        .iter()
                        .chain(approach.landing)
                        .cloned()
                        .collect::<Vec<_>>();
                    if !supported(&assembled, false) || !supported(approach.missed, true) {
                        continue;
                    }
                    let Some(join) = assembled
                        .first()
                        .and_then(|leg| coordinate(leg, input.destination, input.points))
                    else {
                        continue;
                    };
                    if manual.is_none() && openatc_core::arrival::distance_nm(exit, join) > 0.01 {
                        continue;
                    }
                    arrivals.push((
                        openatc_core::arrival::distance_nm(origin, entry),
                        name.clone(),
                        transition.clone(),
                        legs.clone(),
                        entry,
                        approach_name.clone(),
                        approach_transition,
                        assembled,
                        approach.missed.to_vec(),
                        manual.clone(),
                    ));
                    arrivals.sort_by(|a, b| {
                        // Fully published arrivals rank ahead of amended ones, so a manual
                        // termination is only used where no published route reaches the entry.
                        a.9.is_some()
                            .cmp(&b.9.is_some())
                            .then_with(|| a.0.total_cmp(&b.0))
                            .then_with(|| a.1.cmp(&b.1))
                            .then_with(|| a.5.cmp(&b.5))
                            .then_with(|| a.6.cmp(&b.6))
                    });
                    arrivals.truncate(input.budget);
                }
            }
        }
    }
    arrivals.sort_by(|a, b| {
        a.9.is_some()
            .cmp(&b.9.is_some())
            .then_with(|| a.0.total_cmp(&b.0))
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.5.cmp(&b.5))
            .then_with(|| a.6.cmp(&b.6))
    });
    arrivals.truncate(input.budget);
    let mut result = Vec::new();
    for departure in &departures {
        for arrival in &arrivals {
            let Some(floor) = input.mora.corridor(departure.4, arrival.4) else {
                continue;
            };
            result.push((
                openatc_core::arrival::distance_nm(departure.4, arrival.4),
                Terminal {
                    enroute_from: departure.4,
                    enroute_to: arrival.4,
                    enroute_minimum_feet: floor,
                    departure_name: departure.1.clone(),
                    departure: departure.3.clone(),
                    arrival_name: arrival.1.clone(),
                    arrival_transition: arrival.2.clone(),
                    arrival: arrival.3.clone(),
                    approach_name: arrival.5.clone(),
                    approach_transition: arrival.6.clone(),
                    approach: arrival.7.clone(),
                    missed: arrival.8.clone(),
                    arrival_manual: arrival.9.clone(),
                    direct: false,
                },
            ));
            result.sort_by(|a, b| {
                a.0.total_cmp(&b.0)
                    .then_with(|| a.1.departure_name.cmp(&b.1.departure_name))
                    .then_with(|| a.1.arrival_name.cmp(&b.1.arrival_name))
                    .then_with(|| a.1.approach_name.cmp(&b.1.approach_name))
            });
            result.truncate(input.budget);
        }
    }
    result.sort_by(|a, b| {
        a.0.total_cmp(&b.0)
            .then_with(|| a.1.departure_name.cmp(&b.1.departure_name))
            .then_with(|| a.1.arrival_name.cmp(&b.1.arrival_name))
            .then_with(|| a.1.approach_name.cmp(&b.1.approach_name))
    });
    result.truncate(input.budget);
    // Aircraft without a published arrival inside their operating envelope can join the
    // approach directly. The approach transition itself supplies the last published fix and
    // its restrictions, so no STAR limit is removed and no higher altitude is invented.
    let mut direct = Vec::new();
    // If no published STAR reaches the approach, keep the flight by joining the
    // approach directly rather than dropping every arrival.
    if input.direct || arrivals.is_empty() {
        for approach_name in &approach_names {
            let transitions: BTreeSet<_> = input
                .arrivals
                .0
                .keys()
                .filter(|key| key.kind == "APPCH" && &key.name == approach_name)
                .map(|key| key.transition.clone())
                .collect();
            for approach_transition in transitions {
                let Ok(approach) = input.arrivals.approach(approach_name, &approach_transition)
                else {
                    continue;
                };
                let assembled = approach
                    .transition
                    .iter()
                    .chain(approach.landing)
                    .cloned()
                    .collect::<Vec<_>>();
                if !supported(&assembled, false) || !supported(approach.missed, true) {
                    continue;
                }
                if approach
                    .landing
                    .last()
                    .is_none_or(|leg| leg.identifier != format!("RW{}", input.arrival_runway))
                {
                    continue;
                }
                if assembled.len() < 2 || !matches!(assembled[0].terminator.as_str(), "IF" | "TF") {
                    continue;
                }
                let Some(entry) = coordinate(&assembled[0], input.destination, input.points) else {
                    continue;
                };
                // The initial fix publishes no course of its own. Keeping the following leg in
                // the arrival gives the decoder a real course, and the approach resumes at that
                // same published fix without changing any restriction.
                let arrival = assembled[..2].to_vec();
                let approach_legs = assembled[1..].to_vec();
                for departure in &departures {
                    let Some(floor) = input.mora.corridor(departure.4, entry) else {
                        continue;
                    };
                    direct.push((
                        openatc_core::arrival::distance_nm(departure.4, entry),
                        Terminal {
                            enroute_from: departure.4,
                            enroute_to: entry,
                            enroute_minimum_feet: floor,
                            departure_name: departure.1.clone(),
                            departure: departure.3.clone(),
                            arrival_name: "Direct".to_owned(),
                            arrival_transition: approach_transition.clone(),
                            arrival: arrival.clone(),
                            approach_name: approach_name.clone(),
                            approach_transition: approach_transition.clone(),
                            approach: approach_legs.clone(),
                            missed: approach.missed.to_vec(),
                            arrival_manual: None,
                            direct: true,
                        },
                    ));
                }
            }
        }
        // Keep one approach family ahead of the rest so a low published entry is not crowded
        // out by several high-altitude variants of the same procedure.
        direct.sort_by(|a, b| {
            a.1.approach_name
                .cmp(&b.1.approach_name)
                .then_with(|| a.1.approach_transition.cmp(&b.1.approach_transition))
                .then_with(|| a.0.total_cmp(&b.0))
        });
        // Keep a few departure pairings for each published approach entry so a vertical
        // restriction on one transition cannot crowd every other variant out of the budget.
        let mut per_entry = BTreeMap::<(String, String), usize>::new();
        direct.retain(|(_, terminal)| {
            let count = per_entry
                .entry((
                    terminal.approach_name.clone(),
                    terminal.approach_transition.clone(),
                ))
                .or_default();
            *count += 1;
            *count <= 3
        });
        direct.truncate(input.budget);
        result.extend(direct);
    }
    if result.is_empty() {
        return Err(format!(
            "No resolved runway-compatible SID/STAR/approach with a published recovery and connected approach entry ({} supported departure variants, {} joined arrival variants)",
            departures.len(),
            arrivals.len()
        ));
    }
    Ok(result.into_iter().map(|(_, route)| route).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires installed licensed navigation data"]
    fn installed_cargo_terminal_candidates_keep_heading_legs() {
        let root = std::env::var("OPENATC_TEST_SIMULATOR_ROOT").unwrap();
        let origin = airport(&root, "KMEM").unwrap();
        let destination = airport(&root, "KSDF").unwrap();
        let departures = Catalogue::load(std::path::Path::new(&root), "KMEM").unwrap();
        let arrivals = Catalogue::load(std::path::Path::new(&root), "KSDF").unwrap();
        let mut points = Points::requested(&[("KMEM", &departures), ("KSDF", &arrivals)]);
        points.load(std::path::Path::new(&root)).unwrap();
        let result = candidates(&Inputs {
            origin: &origin,
            destination: &destination,
            departure_runway: "36L",
            arrival_runway: "17L",
            departures: &departures,
            arrivals: &arrivals,
            points: &points,
            mora: &openatc_core::navigation::Mora::load(&root),
            budget: 16,
            direct: false,
        })
        .unwrap();
        assert!(
            result
                .iter()
                .any(|route| route.departure.iter().any(|leg| leg.terminator == "VA"))
        );
        for route in result {
            assert_eq!(route.approach.last().unwrap().identifier, "RW17L");
            match &route.arrival_manual {
                // A manual termination is never flown as published geometry: the last leg is
                // the transition itself, and the approach entry is reached only by amendment.
                None => assert_eq!(
                    coordinate(route.arrival.last().unwrap(), &destination, &points),
                    coordinate(route.approach.first().unwrap(), &destination, &points)
                ),
                Some(manual) => {
                    assert_eq!(route.arrival.last().unwrap().terminator, manual.terminator);
                    assert!(!route.approach.first().unwrap().identifier.is_empty());
                }
            }
        }
    }

    #[test]
    #[ignore = "requires installed licensed navigation data"]
    fn installed_ga_candidates_join_the_approach_without_the_high_altitude_star() {
        let root = std::env::var("OPENATC_TEST_SIMULATOR_ROOT").unwrap();
        let origin = airport(&root, "KMEM").unwrap();
        let destination = airport(&root, "KSDF").unwrap();
        let departures = Catalogue::load(std::path::Path::new(&root), "KMEM").unwrap();
        let arrivals = Catalogue::load(std::path::Path::new(&root), "KSDF").unwrap();
        let mut points = Points::requested(&[("KMEM", &departures), ("KSDF", &arrivals)]);
        points.load(std::path::Path::new(&root)).unwrap();
        let result = candidates(&Inputs {
            origin: &origin,
            destination: &destination,
            departure_runway: "36L",
            arrival_runway: "17L",
            departures: &departures,
            arrivals: &arrivals,
            points: &points,
            mora: &openatc_core::navigation::Mora::load(&root),
            budget: 16,
            direct: true,
        })
        .unwrap();
        let direct = result
            .iter()
            .find(|route| route.direct && route.approach_name == "I17L")
            .expect("GA needs a direct I17L candidate");
        assert_eq!(direct.arrival_name, "Direct");
        assert_eq!(direct.arrival.last().unwrap().identifier, "SLGRR");
        assert_eq!(
            coordinate(direct.arrival.last().unwrap(), &destination, &points),
            coordinate(direct.approach.first().unwrap(), &destination, &points),
            "the direct arrival must join the published approach entry"
        );
        assert!(
            !direct
                .arrival
                .iter()
                .chain(&direct.approach)
                .any(|leg| leg.identifier == "JIINN"),
            "a light aircraft is not routed through the high-altitude STAR crossing"
        );
    }

    #[test]
    #[ignore = "requires installed licensed navigation data"]
    fn installed_manual_terminated_arrivals_are_offered_as_explicit_amendments() {
        let root = std::env::var("OPENATC_TEST_SIMULATOR_ROOT").unwrap();
        let origin = airport(&root, "KSDF").unwrap();
        let destination = airport(&root, "KMEM").unwrap();
        let departures = Catalogue::load(std::path::Path::new(&root), "KSDF").unwrap();
        let arrivals = Catalogue::load(std::path::Path::new(&root), "KMEM").unwrap();
        let mut points = Points::requested(&[("KSDF", &departures), ("KMEM", &arrivals)]);
        points.load(std::path::Path::new(&root)).unwrap();
        let result = candidates(&Inputs {
            origin: &origin,
            destination: &destination,
            departure_runway: "17L",
            arrival_runway: "36L",
            departures: &departures,
            arrivals: &arrivals,
            points: &points,
            mora: &openatc_core::navigation::Mora::load(&root),
            budget: 16,
            direct: false,
        })
        .unwrap();
        // Memphis STARs commonly end in a manual transition, so this direction only produces
        // candidates when such an arrival is kept as an amendment rather than discarded.
        assert!(
            result.iter().any(|route| route.arrival_manual.is_some()),
            "expected at least one manual-terminated arrival candidate"
        );
        for route in result {
            if let Some(manual) = &route.arrival_manual {
                assert!(
                    openatc_core::traffic::resolved::MANUAL_TERMINATORS
                        .contains(&manual.terminator.as_str())
                );
                let published = &route.arrival[..route.arrival.len() - 1];
                assert!(
                    published
                        .iter()
                        .all(|leg| supported(std::slice::from_ref(leg), false))
                );
                assert!(
                    !published.is_empty(),
                    "an amendment needs published geometry to precede it"
                );
            }
        }
    }

    #[test]
    #[ignore = "requires installed licensed navigation data"]
    fn installed_nice_to_milan_terminal_candidates_keep_restrictions_and_recovery() {
        let root = std::env::var("OPENATC_TEST_SIMULATOR_ROOT").unwrap();
        let origin = airport(&root, "LFMN").unwrap();
        let destination = airport(&root, "LIMC").unwrap();
        let departures = Catalogue::load(std::path::Path::new(&root), "LFMN").unwrap();
        let arrivals = Catalogue::load(std::path::Path::new(&root), "LIMC").unwrap();
        let mut points = Points::requested(&[("LFMN", &departures), ("LIMC", &arrivals)]);
        points.load(std::path::Path::new(&root)).unwrap();
        let result = candidates(&Inputs {
            origin: &origin,
            destination: &destination,
            departure_runway: "04R",
            arrival_runway: "35R",
            departures: &departures,
            arrivals: &arrivals,
            points: &points,
            mora: &openatc_core::navigation::Mora::load(&root),
            budget: 16,
            direct: false,
        })
        .unwrap();
        assert!(result.len() <= 16);
        assert!(
            candidates(&Inputs {
                origin: &origin,
                destination: &destination,
                departure_runway: "04R",
                arrival_runway: "35R",
                departures: &departures,
                arrivals: &arrivals,
                points: &points,
                mora: &Default::default(),
                budget: 16,
                direct: false
            })
            .is_err()
        );
        for route in result {
            assert!(route.enroute_minimum_feet > 0);
            assert!(openatc_core::arrival::distance_nm(route.enroute_from, route.enroute_to) > 0.0);
            assert_eq!(route.approach.last().unwrap().identifier, "RW35R");
            assert!(
                route
                    .missed
                    .last()
                    .is_some_and(|leg| leg.terminator == "HM")
            );
            let star = route.arrival.last().unwrap();
            let approach = route.approach.first().unwrap();
            assert_eq!(
                coordinate(star, &destination, &points),
                coordinate(approach, &destination, &points)
            );
            if !route.direct {
                let published = arrivals
                    .arrival(&route.arrival_name, "35R", &route.arrival_transition)
                    .unwrap()
                    .concat();
                for leg in &route.arrival {
                    let Some(source) = published.iter().find(|source| {
                        source.sequence == leg.sequence && source.identifier == leg.identifier
                    }) else {
                        continue;
                    };
                    assert_eq!(
                        leg.altitude_one, source.altitude_one,
                        "{} dropped the altitude on {}",
                        route.arrival_name, leg.identifier
                    );
                    assert_eq!(
                        leg.altitude_two, source.altitude_two,
                        "{} dropped the second altitude on {}",
                        route.arrival_name, leg.identifier
                    );
                }
                assert_eq!(
                    route.arrival.iter().any(|leg| leg.altitude_one.is_some()),
                    published.iter().any(|leg| leg.altitude_one.is_some()),
                    "{} {} restrictions differ from the published procedure",
                    route.arrival_name,
                    route.arrival_transition
                );
            }
        }
    }
}

#[cfg(test)]
mod fallback_tests {
    use super::*;

    #[test]
    #[ignore = "requires installed licensed navigation data"]
    fn installed_yscb_to_yssy_terminal_candidates_are_available() {
        let root = std::env::var("OPENATC_TEST_SIMULATOR_ROOT").unwrap();
        let origin = airport(&root, "YSCB").unwrap();
        let destination = airport(&root, "YSSY").unwrap();
        let departures = Catalogue::load(std::path::Path::new(&root), "YSCB").unwrap();
        let arrivals = Catalogue::load(std::path::Path::new(&root), "YSSY").unwrap();
        let mut points = Points::requested(&[("YSCB", &departures), ("YSSY", &arrivals)]);
        points.load(std::path::Path::new(&root)).unwrap();
        let result = candidates(&Inputs {
            origin: &origin,
            destination: &destination,
            departure_runway: "35",
            arrival_runway: "34L",
            departures: &departures,
            arrivals: &arrivals,
            points: &points,
            mora: &openatc_core::navigation::Mora::load(&root),
            budget: 16,
            direct: false,
        })
        .expect("a terminal candidate for the installed pair");
        assert!(!result.is_empty());
        if result.iter().all(|route| !route.direct) {
            eprintln!(
                "published STAR joins kept: {}",
                result
                    .iter()
                    .map(|route| route.arrival_name.as_str())
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
            );
        }
    }
}
