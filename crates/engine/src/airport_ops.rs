//! Airport-local runway, taxi and resource state. Weather and traffic stay outside.

use openatc_core::airport::Airport;
#[cfg(test)]
use openatc_core::state::Point;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

/// The departure and arrival runway pair for one airport flow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunwayAssignment {
    /// Departure runway designator.
    pub departure: String,
    /// Arrival runway designator.
    pub arrival: String,
    /// Arrival and departure use different physical runways.
    pub split: bool,
}

/// Airport data kept only while the player or a live aircraft still needs it.
pub struct AirportManager {
    contexts: BTreeMap<String, AirportContext>,
}

/// One loaded airport and its live runway resources.
pub struct AirportContext {
    /// Installed airport data.
    pub data: Arc<Airport>,
    occupancy: BTreeMap<String, BTreeSet<u32>>,
    users: BTreeSet<u32>,
    last_used: Instant,
}

#[derive(Clone, Copy)]
struct End<'a> {
    name: &'a str,
    course: f64,
    length: f64,
    physical: usize,
}

impl Default for AirportManager {
    fn default() -> Self {
        Self {
            contexts: BTreeMap::new(),
        }
    }
}

impl AirportManager {
    /// Load or refresh one airport context.
    pub fn ensure(&mut self, data: Arc<Airport>) -> &mut AirportContext {
        let context = self
            .contexts
            .entry(data.icao.clone())
            .or_insert_with(|| AirportContext {
                data: data.clone(),
                occupancy: BTreeMap::new(),
                users: BTreeSet::new(),
                last_used: Instant::now(),
            });
        context.data = data;
        context.last_used = Instant::now();
        context
    }

    /// Airport data, when the context is still loaded.
    #[must_use]
    pub fn airport(&self, icao: &str) -> Option<Arc<Airport>> {
        self.contexts.get(icao).map(|context| context.data.clone())
    }

    /// Choose the departure and arrival runways for this airport flow.
    #[must_use]
    pub fn assignment(
        &self,
        icao: &str,
        planned_departure: &str,
        planned_arrival: &str,
        wind_from: f64,
        wind_knots: f64,
        minimum_length_m: f64,
    ) -> Option<RunwayAssignment> {
        let airport = &self.contexts.get(icao)?.data;
        let ends = runway_ends(airport);
        if ends.is_empty() {
            return None;
        }
        let departure = choose_departure(&ends, planned_departure, wind_from, wind_knots)
            .or_else(|| wind_end(airport, wind_from, wind_knots))
            .or_else(|| ends.first().map(|end| end.name.to_owned()))?;
        let departure_end = ends.iter().find(|end| end.name == departure)?;
        let planned = ends.iter().find(|end| {
            end.name == planned_arrival
                && end.length >= minimum_length_m
                && openatc_core::navigation::turn(departure_end.course, end.course).abs() <= 15.0
        });
        let arrival = planned
            .filter(|end| end.physical != departure_end.physical)
            .map(|end| end.name.to_owned())
            .or_else(|| {
                preferred_parallel(
                    &ends,
                    *departure_end,
                    wind_from,
                    wind_knots,
                    minimum_length_m,
                )
                .map(|end| end.name.to_owned())
            })
            .unwrap_or_else(|| departure.clone());
        let arrival_end = ends.iter().find(|end| end.name == arrival)?;
        Some(RunwayAssignment {
            split: arrival_end.physical != departure_end.physical,
            departure,
            arrival,
        })
    }

    /// Claim a physical runway for one aircraft. Opposite ends count as the same runway.
    pub fn claim(&mut self, icao: &str, runway: &str, id: u32) -> bool {
        let Some(context) = self.contexts.get_mut(icao) else {
            return false;
        };
        let Some(key) = runway_key(&context.data, runway) else {
            return false;
        };
        let Some(group) = openatc_core::airport::runway_group_keys(&context.data, runway) else {
            return false;
        };
        context.last_used = Instant::now();
        if group.iter().any(|member| {
            context
                .occupancy
                .get(member)
                .is_some_and(|owners| owners.iter().any(|owner| *owner != id))
        }) {
            return false;
        }
        context.occupancy.entry(key).or_default().insert(id);
        true
    }

    /// Release only this aircraft's runway claim.
    pub fn release(&mut self, icao: &str, runway: &str, id: u32) {
        let Some(context) = self.contexts.get_mut(icao) else {
            return;
        };
        let Some(key) = runway_key(&context.data, runway) else {
            return;
        };
        if let Some(owners) = context.occupancy.get_mut(&key) {
            owners.remove(&id);
            if owners.is_empty() {
                context.occupancy.remove(&key);
            }
        }
    }

    /// Whether another aircraft owns the same physical runway.
    #[must_use]
    pub fn occupied_by_other(&self, icao: &str, runway: &str, id: u32) -> bool {
        let Some(context) = self.contexts.get(icao) else {
            return false;
        };
        let Some(key) = runway_key(&context.data, runway) else {
            return false;
        };
        context
            .occupancy
            .get(&key)
            .is_some_and(|owners| owners.iter().any(|owner| *owner != id))
    }

    /// Whether another aircraft owns an intersecting runway in the same pavement group.
    #[must_use]
    pub fn crossing_occupied(&self, icao: &str, runway: &str, id: u32) -> bool {
        let Some(context) = self.contexts.get(icao) else {
            return false;
        };
        let Some(own) = runway_key(&context.data, runway) else {
            return false;
        };
        let Some(group) = openatc_core::airport::runway_group_keys(&context.data, runway) else {
            return false;
        };
        group.iter().filter(|member| **member != own).any(|member| {
            context
                .occupancy
                .get(member)
                .is_some_and(|owners| owners.iter().any(|owner| *owner != id))
        })
    }

    /// Refresh the live users of one airport context.
    pub fn sync_users(&mut self, icao: &str, users: impl IntoIterator<Item = u32>) {
        if let Some(context) = self.contexts.get_mut(icao) {
            context.users = users.into_iter().collect();
            context.last_used = Instant::now();
        }
    }

    /// Drop airports the player has left once no aircraft still uses them.
    pub fn prune(&mut self, own: Option<[f64; 2]>, keep_nm: f64) {
        self.contexts.retain(|_, context| {
            if !context.users.is_empty() || !context.occupancy.is_empty() {
                return true;
            }
            let Some(own) = own else {
                return true;
            };
            openatc_core::arrival::distance_nm(
                own,
                [
                    context.data.reference_latitude,
                    context.data.reference_longitude,
                ],
            ) <= keep_nm
        });
    }
}

fn runway_ends(airport: &Airport) -> Vec<End<'_>> {
    let mut ends = Vec::new();
    for (index, runway) in airport.runways.iter().enumerate() {
        let length = (runway.second.east - runway.first.east)
            .hypot(runway.second.north - runway.first.north);
        if !length.is_finite() || length < 100.0 {
            continue;
        }
        let course = (runway.second.east - runway.first.east)
            .atan2(runway.second.north - runway.first.north)
            .to_degrees()
            .rem_euclid(360.0);
        for (name, heading) in [
            (runway.first_name.as_str(), course),
            (
                runway.second_name.as_str(),
                (course + 180.0).rem_euclid(360.0),
            ),
        ] {
            if !name.is_empty() {
                ends.push(End {
                    name,
                    course: heading,
                    length,
                    physical: index,
                });
            }
        }
    }
    ends
}

fn choose_departure<'a>(
    ends: &[End<'a>],
    planned: &str,
    wind_from: f64,
    wind_knots: f64,
) -> Option<String> {
    if let Some(end) = ends.iter().find(|end| end.name == planned) {
        return Some(end.name.to_owned());
    }
    if planned.is_empty() {
        return None;
    }
    ends.iter()
        .filter(|end| wind_score(end, wind_from, wind_knots) > 0.0)
        .max_by(|left, right| {
            wind_score(left, wind_from, wind_knots)
                .total_cmp(&wind_score(right, wind_from, wind_knots))
                .then(left.course.total_cmp(&right.course))
        })
        .map(|end| end.name.to_owned())
}

fn wind_end(airport: &Airport, wind_from: f64, wind_knots: f64) -> Option<String> {
    let name = crate::radio::atis_runway(&airport.runways, "", wind_from, wind_knots);
    (!name.is_empty()).then_some(name)
}

fn preferred_parallel<'a>(
    ends: &[End<'a>],
    departure: End<'a>,
    wind_from: f64,
    wind_knots: f64,
    minimum_length_m: f64,
) -> Option<End<'a>> {
    ends.iter()
        .copied()
        .filter(|end| {
            end.physical != departure.physical
                && end.length >= minimum_length_m
                && openatc_core::navigation::turn(departure.course, end.course).abs() <= 15.0
        })
        .max_by(|left, right| {
            wind_score(left, wind_from, wind_knots)
                .total_cmp(&wind_score(right, wind_from, wind_knots))
                .then(left.length.total_cmp(&right.length))
                .then(left.name.cmp(right.name))
        })
}

fn wind_score(end: &End<'_>, wind_from: f64, wind_knots: f64) -> f64 {
    let headwind = wind_knots.max(0.0) * (wind_from - end.course).to_radians().cos();
    headwind + end.length * 0.001
}

fn runway_key(airport: &Airport, name: &str) -> Option<String> {
    let runway = airport
        .runways
        .iter()
        .find(|runway| runway.first_name == name || runway.second_name == name)?;
    let mut names = [runway.first_name.as_str(), runway.second_name.as_str()];
    names.sort_unstable();
    Some(names.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openatc_core::airport::Runway;

    fn point(east: f64, north: f64) -> Point {
        Point {
            east,
            north,
            height: 0.0,
        }
    }

    fn airport() -> Airport {
        Airport {
            icao: "TEST".to_owned(),
            runways: vec![
                Runway {
                    first_name: "04R".to_owned(),
                    second_name: "22L".to_owned(),
                    first: point(0.0, 0.0),
                    second: point(2_500.0, 0.0),
                    width: 45.0,
                    ..Default::default()
                },
                Runway {
                    first_name: "04L".to_owned(),
                    second_name: "22R".to_owned(),
                    first: point(0.0, 500.0),
                    second: point(2_500.0, 500.0),
                    width: 45.0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    fn manager() -> AirportManager {
        let mut manager = AirportManager::default();
        manager.ensure(Arc::new(airport()));
        manager
    }

    #[test]
    fn the_planned_arrival_uses_a_parallel_when_the_departure_runway_is_the_same() {
        let manager = manager();
        let assignment = manager
            .assignment("TEST", "04R", "04R", 40.0, 10.0, 1_200.0)
            .unwrap();
        assert_eq!(assignment.departure, "04R");
        assert_eq!(assignment.arrival, "04L");
        assert!(assignment.split);
    }

    #[test]
    fn one_runway_stays_mixed() {
        let mut manager = AirportManager::default();
        manager.ensure(Arc::new(Airport {
            icao: "TEST".to_owned(),
            runways: vec![Runway {
                first_name: "09".to_owned(),
                second_name: "27".to_owned(),
                first: point(0.0, 0.0),
                second: point(2_000.0, 0.0),
                width: 45.0,
                ..Default::default()
            }],
            ..Default::default()
        }));
        let assignment = manager
            .assignment("TEST", "09", "", 90.0, 5.0, 1_200.0)
            .unwrap();
        assert_eq!(assignment.arrival, "09");
        assert!(!assignment.split);
    }

    #[test]
    fn unused_airports_are_dropped_when_the_player_leaves() {
        let mut manager = manager();
        manager.prune(Some([10.0, 10.0]), 60.0);
        assert!(manager.airport("TEST").is_none());
        manager.ensure(Arc::new(airport()));
        manager.sync_users("TEST", [7]);
        manager.prune(Some([10.0, 10.0]), 60.0);
        assert!(manager.airport("TEST").is_some());
    }

    #[test]
    fn crossing_runways_share_one_occupancy_group() {
        let mut manager = AirportManager::default();
        manager.ensure(Arc::new(Airport {
            icao: "TEST".to_owned(),
            runways: vec![
                Runway {
                    first_name: "04R".to_owned(),
                    second_name: "22L".to_owned(),
                    first: point(0.0, 0.0),
                    second: point(2_000.0, 0.0),
                    width: 45.0,
                    ..Default::default()
                },
                Runway {
                    first_name: "09".to_owned(),
                    second_name: "27".to_owned(),
                    first: point(1_000.0, -1_000.0),
                    second: point(1_000.0, 1_000.0),
                    width: 45.0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }));
        assert!(manager.claim("TEST", "04R", 1));
        assert!(
            !manager.claim("TEST", "09", 2),
            "a crossing runway cannot be claimed while the other is occupied"
        );
        manager.release("TEST", "04R", 1);
        assert!(manager.claim("TEST", "09", 2));
    }

    #[test]
    fn occupancy_is_per_physical_runway() {
        let mut manager = manager();
        assert!(manager.claim("TEST", "04R", 1));
        assert!(manager.claim("TEST", "04L", 2));
        assert!(!manager.claim("TEST", "04R", 2));
        manager.release("TEST", "04R", 1);
        assert!(manager.claim("TEST", "04R", 2));
    }
}
