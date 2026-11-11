//! Model-compatible airport pairs selected from installed controllers and airport scenery.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
#[allow(unused_imports, clippy::wildcard_imports)]
use super::*;
use super::{
    Airport, Arc, BTreeMap, BTreeSet, Category, MAXIMUM_AIRCRAFT, Operations, Preparation, Scene,
    airport, animation, environment, landing, policy, prepare_direction_with_terrain, routing,
    runway,
};

#[derive(Clone)]
pub(super) struct Pair {
    pub origin: Arc<Airport>,
    pub destination: Arc<Airport>,
    pub departure_runway: String,
    pub arrival_runway: String,
    compatible: [[bool; 4]; 2],
}

#[derive(Clone)]
pub(super) struct Plan {
    pub key: String,
    pub airports: BTreeMap<String, Arc<Airport>>,
    pairs: Vec<Pair>,
    warnings: Vec<String>,
}

fn published_pair(
    input: &Preparation,
    first: &Airport,
    second: &Airport,
    departure_runway: &str,
    arrival_runway: &str,
    mora: &openatc_core::navigation::Mora,
    budget: usize,
) -> Result<[Result<(), String>; 2], String> {
    use openatc_core::traffic::{coordinates::Points, procedures::Catalogue};
    let root = std::path::Path::new(&input.simulator_root);
    let departures = Catalogue::load(root, &first.icao)?;
    let arrivals = Catalogue::load(root, &second.icao)?;
    let mut points = Points::requested(&[(&first.icao, &departures), (&second.icao, &arrivals)]);
    points.load(root)?;
    Ok([
        (
            first,
            second,
            departure_runway,
            arrival_runway,
            &departures,
            &arrivals,
        ),
        (
            second,
            first,
            arrival_runway,
            departure_runway,
            &arrivals,
            &departures,
        ),
    ]
    .map(
        |(origin, destination, departure_runway, arrival_runway, departures, arrivals)| {
            let candidates = |direct| {
                routing::candidates(&routing::Inputs {
                    origin,
                    destination,
                    departure_runway,
                    arrival_runway,
                    departures,
                    arrivals,
                    points: &points,
                    mora,
                    budget,
                    direct,
                })
            };
            // Prefer a published STAR, but allow a direct approach entry when the destination
            // has no STAR that joins the available approach.
            candidates(false).or_else(|_| candidates(true)).map(|_| ())
        },
    ))
}

fn distance(first: &Airport, second: &Airport) -> f64 {
    openatc_core::arrival::distance_nm(
        [first.reference_latitude, first.reference_longitude],
        [second.reference_latitude, second.reference_longitude],
    )
}

fn compatible(input: &Preparation, first: &Airport, second: &Airport) -> [bool; 4] {
    let (Some(origin), Some(destination)) =
        (port_published(input, first), port_published(input, second))
    else {
        return [false; 4];
    };
    [
        Category::Airline,
        Category::Regional,
        Category::Cargo,
        Category::GeneralAviation,
    ]
    .map(|category| {
        input
            .profiles
            .select_where(
                &origin,
                &destination,
                distance(first, second),
                category,
                &input.models,
                input.epoch,
                |_, aircraft| {
                    [first, second].iter().all(|airport| {
                        airport
                            .parking
                            .iter()
                            .any(|stand| openatc_core::parking::moving_supports(stand, aircraft))
                    })
                },
            )
            .is_some()
    })
}

fn longest_runway(airport: &Airport) -> Option<String> {
    airport
        .runways
        .iter()
        .max_by(|a, b| {
            let length = |runway: &openatc_core::airport::Runway| {
                (runway.second.east - runway.first.east)
                    .hypot(runway.second.north - runway.first.north)
            };
            length(a).total_cmp(&length(b))
        })
        .map(|runway| runway.first_name.clone())
}

fn runway(airport: &Airport, preferred: &str) -> Option<String> {
    airport
        .runways
        .iter()
        .any(|runway| runway.first_name == preferred || runway.second_name == preferred)
        .then(|| preferred.to_owned())
        .or_else(|| longest_runway(airport))
}

/// Pick the bounded set of same-flow runway ends in the order the airport prefers them.
fn runway_ends(airport: &Airport, preferred: &str) -> Vec<String> {
    let heading = |name: &str| {
        airport
            .runways
            .iter()
            .find(|runway| runway.first_name == name || runway.second_name == name)
            .map(|runway| {
                let (first, second) = if runway.first_name == name {
                    (runway.first, runway.second)
                } else {
                    (runway.second, runway.first)
                };
                (second.east - first.east)
                    .atan2(second.north - first.north)
                    .to_degrees()
                    .rem_euclid(360.0)
            })
    };
    let Some(course) = heading(preferred) else {
        return vec![preferred.to_owned()];
    };
    let length = |runway: &openatc_core::airport::Runway| {
        (runway.second.east - runway.first.east).hypot(runway.second.north - runway.first.north)
    };
    let mut runways = airport.runways.iter().collect::<Vec<_>>();
    runways.sort_by(|left, right| length(right).total_cmp(&length(left)));
    let mut ends = vec![preferred.to_owned()];
    for runway in runways {
        for name in [&runway.first_name, &runway.second_name] {
            if name != preferred
                && !ends.contains(name)
                && heading(name).is_some_and(|other| {
                    openatc_core::navigation::turn(course, other).abs() <= 15.0
                })
            {
                ends.push(name.clone());
            }
        }
    }
    ends
}

/// Prefer a runway end that a representative of each eligible category can leave
/// after braking, falling back to the airport's preferred end when none can.
fn vacatable_runway(
    input: &Preparation,
    origin: &Airport,
    destination: &Airport,
    preferred: &str,
    eligible: [bool; 4],
) -> String {
    if input.simulator_root.is_empty() {
        return preferred.to_owned();
    }
    let distance = distance(origin, destination);
    let (Some(origin_port), Some(destination_port)) = (
        port_published(input, origin),
        port_published(input, destination),
    ) else {
        return preferred.to_owned();
    };
    let representatives = [
        Category::Airline,
        Category::Regional,
        Category::Cargo,
        Category::GeneralAviation,
    ]
    .into_iter()
    .enumerate()
    .filter(|(index, _)| eligible[*index] && input.counts[*index] > 0)
    .filter_map(|(_, category)| {
        input
            .profiles
            .select_where(
                &origin_port,
                &destination_port,
                distance,
                category,
                &input.models,
                input.epoch,
                |_, aircraft| {
                    [origin, destination].iter().all(|airport| {
                        airport
                            .parking
                            .iter()
                            .any(|stand| openatc_core::parking::moving_supports(stand, aircraft))
                    })
                },
            )
            .and_then(|choice| input.profiles.aircraft_for(&choice.aircraft))
    })
    .collect::<Vec<_>>();
    if representatives.is_empty() {
        return preferred.to_owned();
    }
    let Ok(data) =
        openatc_core::arrival::ArrivalAirport::load(&input.simulator_root, destination.clone())
    else {
        return preferred.to_owned();
    };
    let Ok((_, policy, _)) = policy::load(&input.config) else {
        return preferred.to_owned();
    };
    let Some(handling) = animation::builtin().ok() else {
        return preferred.to_owned();
    };
    let ends = runway_ends(destination, preferred);
    let candidates = ends
        .into_iter()
        .filter_map(|name| {
            runway::Surface::new(
                destination,
                &name,
                &data.thresholds,
                policy.runway_slope_limit_percent,
            )
            .ok()
            .map(|surface| (name, surface))
        })
        .collect::<Vec<_>>();
    let mut best = (0usize, preferred.to_owned());
    for (name, _) in &candidates {
        let accepted = representatives
            .iter()
            .filter(|aircraft| {
                aircraft.handling.is_some_and(|kind| {
                    handling.get(&kind).is_some_and(|handling| {
                        landing::vacatable_runway(
                            destination,
                            &candidates,
                            aircraft,
                            handling,
                            &policy,
                            aircraft.approach_knots,
                        )
                        .is_ok_and(|chosen| chosen == *name)
                    })
                })
            })
            .count();
        if accepted > best.0 {
            best = (accepted, name.clone());
        }
    }
    best.1
}

/// Traffic discovery can start before the player has filed a route.
pub(super) fn endpoints(
    root: &str,
    plan: &openatc_core::state::FlightPlan,
    stations: &[openatc_core::stations::Station],
    own: [f64; 2],
    policy: &Operations,
    airborne: bool,
) -> Result<(Airport, Airport, String, String), String> {
    if !own.iter().all(|coordinate| coordinate.is_finite())
        || own[0].abs() >= 85.0
        || own[1].abs() >= 180.0
    {
        return Err("Traffic airport discovery needs a valid position".into());
    }
    if plan.departure != plan.destination
        && let (Ok(origin), Ok(destination)) = (
            airport(root, &plan.departure),
            airport(root, &plan.destination),
        )
        && let (Some(departure), Some(arrival)) = (
            runway(&origin, &plan.runway),
            runway(&destination, &plan.arrival_runway),
        )
        // A filed plan names the traffic airports while the player is near one of them. While
        // airborne, the filed destination also counts out to the preparation range, so its STAR
        // exists before arrival. After a landing elsewhere the plan is stale, and following it
        // would put every generated flight at an airport the player cannot see.
        && ([&origin, &destination].iter().any(|airport| {
            openatc_core::arrival::distance_nm(
                own,
                [airport.reference_latitude, airport.reference_longitude],
            ) <= policy.surface_monitor_radius_nm
        }) || airborne
            && openatc_core::arrival::distance_nm(
                own,
                [
                    destination.reference_latitude,
                    destination.reference_longitude,
                ],
            ) <= super::ARRIVAL_PREPARE_NM)
    {
        return Ok((origin, destination, departure, arrival));
    }
    let mut candidates = BTreeMap::<String, f64>::new();
    for station in stations.iter().filter(|station| {
        station.service == "Tower" || station.services.iter().any(|service| service == "Tower")
    }) {
        if !(3..=4).contains(&station.airport.len())
            || !station
                .airport
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric())
        {
            continue;
        }
        let distance =
            openatc_core::arrival::distance_nm(own, [station.latitude, station.longitude]);
        if distance.is_finite() && distance <= policy.network_radius_nm {
            candidates
                .entry(station.airport.clone())
                .and_modify(|current| *current = current.min(distance))
                .or_insert(distance);
        }
    }
    let mut candidates = candidates.into_iter().collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.1
            .total_cmp(&right.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    let tower_count = stations
        .iter()
        .filter(|station| {
            station.service == "Tower" || station.services.iter().any(|service| service == "Tower")
        })
        .count();
    let mut first_error = None;
    let endpoints: Vec<_> = candidates
        .into_iter()
        .take(policy.network_candidate_limit)
        .filter_map(|(id, _)| {
            let airport = match airport(root, &id) {
                Ok(airport) => airport,
                Err(error) => {
                    first_error.get_or_insert(format!("{id}: {error}"));
                    return None;
                }
            };
            let Some(runway) = longest_runway(&airport) else {
                first_error.get_or_insert(format!("{id}: no usable runway"));
                return None;
            };
            if airport.parking.is_empty() {
                first_error.get_or_insert(format!("{id}: no parking positions"));
                return None;
            }
            Some((airport, runway))
        })
        .collect();
    let mut endpoints = endpoints.into_iter();
    let (origin, departure) = endpoints.next().ok_or_else(|| {
        let detail = first_error.map_or_else(String::new, |error| format!("; first: {error}"));
        format!(
            "No installed controlled traffic airport near the aircraft ({} stations, {tower_count} tower{detail})",
            stations.len()
        )
    })?;
    let (destination, arrival) = endpoints
        .next()
        .ok_or("No second installed controlled traffic airport near the aircraft")?;
    Ok((origin, destination, departure, arrival))
}

/// Reserve half the survey budget for configured network links, cycling requested
/// categories, and keep the remaining slots for nearby fields. Eligibility is checked later.
fn balanced_candidates(
    nearby: Vec<String>,
    links: &BTreeMap<String, BTreeSet<Category>>,
    categories: &[Category],
    limit: usize,
) -> Vec<String> {
    let mut chosen = Vec::new();
    let mut seen = BTreeSet::new();
    let linked_limit = limit / 2;
    loop {
        let before = chosen.len();
        for category in categories {
            if chosen.len() >= linked_limit {
                break;
            }
            if let Some(id) = nearby.iter().find(|id| {
                !seen.contains(*id)
                    && links
                        .get(*id)
                        .is_some_and(|supported| supported.contains(category))
            }) {
                seen.insert(id.clone());
                chosen.push(id.clone());
            }
        }
        if chosen.len() == before || chosen.len() >= linked_limit {
            break;
        }
    }
    for id in nearby {
        if chosen.len() >= limit {
            break;
        }
        if seen.insert(id.clone()) {
            chosen.push(id);
        }
    }
    chosen
}

/// Discovery requests real airport weather before any additional flight is scheduled.
pub(super) fn survey(input: &Preparation, key: String) -> Result<Plan, String> {
    let trace = std::env::var_os("OPENATC_TRAFFIC_PROFILE").is_some();
    let started = Instant::now();
    let (_, policy, _) = policy::load(&input.config)?;
    let origin = Arc::new(input.origin.clone());
    let destination = Arc::new(input.destination.clone());
    let mut plan = Plan {
        key,
        airports: [origin.clone(), destination.clone()]
            .into_iter()
            .map(|airport| (airport.icao.clone(), airport))
            .collect(),
        warnings: Vec::new(),
        pairs: vec![Pair {
            origin: origin.clone(),
            destination: destination.clone(),
            departure_runway: input.runway.clone(),
            arrival_runway: input.arrival_runway.clone(),
            compatible: [
                compatible(input, &origin, &destination),
                compatible(input, &destination, &origin),
            ],
        }],
    };
    let own_distance = |airport: &Airport| {
        openatc_core::arrival::distance_nm(
            input.own,
            [airport.reference_latitude, airport.reference_longitude],
        )
    };
    let anchor = if own_distance(&origin) <= own_distance(&destination) {
        origin
    } else {
        destination
    };
    let mut candidates = BTreeMap::<String, (f64, f64)>::new();
    for station in input.stations.iter().filter(|station| {
        station.service == "Tower" || station.services.iter().any(|service| service == "Tower")
    }) {
        if plan.airports.contains_key(&station.airport)
            || !(3..=4).contains(&station.airport.len())
            || !station
                .airport
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric())
        {
            continue;
        }
        let distance =
            openatc_core::arrival::distance_nm(input.own, [station.latitude, station.longitude]);
        if !distance.is_finite() || !(5.0..=policy.network_radius_nm).contains(&distance) {
            continue;
        }
        let preference = input
            .profiles
            .hubs
            .get(&super::published_code(
                &input.airport_codes,
                &station.airport,
            ))
            .map_or(0u64, |hub| {
                hub.airlines.values().map(|weight| u64::from(*weight)).sum()
            });
        let score = distance / (1.0 + (preference as f64).ln_1p() * 0.1);
        candidates
            .entry(station.airport.clone())
            .or_insert((score, distance));
    }
    let mut candidates = candidates.into_iter().collect::<Vec<_>>();
    candidates.sort_by(|a, b| a.1.0.total_cmp(&b.1.0).then_with(|| a.0.cmp(&b.0)));
    let anchor_code = super::published_code(&input.airport_codes, &anchor.icao);
    let mut categories = [
        Category::Airline,
        Category::Regional,
        Category::Cargo,
        Category::GeneralAviation,
    ]
    .into_iter()
    .zip(input.counts)
    .enumerate()
    .filter_map(|(index, (category, count))| (count > 0).then_some((category, index)))
    .collect::<Vec<_>>();
    // Fill missing primary-pair categories first; this only orders discovery, not admission.
    categories.sort_by_key(|(_, index)| {
        plan.pairs[0]
            .compatible
            .iter()
            .any(|direction| direction[*index])
    });
    let categories = categories
        .into_iter()
        .map(|(category, _)| category)
        .collect::<Vec<_>>();
    let mut published_links = BTreeMap::<String, BTreeSet<Category>>::new();
    for route in input.profiles.routes.values().flatten() {
        let endpoint = if route.origin == anchor_code {
            &route.destination
        } else if route.destination == anchor_code {
            &route.origin
        } else {
            continue;
        };
        if let Some(operator) = input.profiles.airlines.get(&route.airline)
            && categories.contains(&operator.category)
        {
            published_links
                .entry(endpoint.clone())
                .or_default()
                .insert(operator.category);
        }
    }
    let links = candidates
        .iter()
        .filter_map(|(id, _)| {
            let code = super::published_code(&input.airport_codes, id);
            published_links
                .get(&code)
                .map(|categories| (id.clone(), categories.clone()))
        })
        .collect();
    let candidates = balanced_candidates(
        candidates.into_iter().map(|(id, _)| id).collect(),
        &links,
        &categories,
        policy.network_candidate_limit,
    );
    let mora = openatc_core::navigation::Mora::load(&input.simulator_root);
    let mut one_way = Vec::new();
    for id in candidates {
        if plan.airports.len() >= policy.network_airport_limit {
            break;
        }
        if trace {
            eprintln!(
                "Traffic preparation +{:.2}s: survey {id}",
                started.elapsed().as_secs_f64()
            );
        }
        let Ok(airport) = airport(&input.simulator_root, &id) else {
            continue;
        };
        let mut available = [
            compatible(input, &anchor, &airport),
            compatible(input, &airport, &anchor),
        ];
        if !available.iter().any(|direction| {
            direction
                .iter()
                .zip(input.counts)
                .any(|(eligible, count)| *eligible && count > 0)
        }) {
            continue;
        }
        let Some(preferred) = longest_runway(&airport) else {
            continue;
        };
        let airport_runway = vacatable_runway(input, &anchor, &airport, &preferred, available[0]);
        let airport = Arc::new(airport);
        let anchor_runway = if anchor.icao == input.origin.icao {
            input.runway.clone()
        } else {
            input.arrival_runway.clone()
        };
        let routes = published_pair(
            input,
            &anchor,
            &airport,
            &anchor_runway,
            &airport_runway,
            &mora,
            policy.terminal_candidate_limit,
        )
        .unwrap_or_else(|error| [Err(error.clone()), Err(error)]);
        for (direction, result) in routes.into_iter().enumerate() {
            if let Err(error) = result {
                available[direction] = [false; 4];
                if plan.warnings.len() < 8 {
                    let (from, to) = if direction == 0 {
                        (&anchor.icao, &id)
                    } else {
                        (&id, &anchor.icao)
                    };
                    plan.warnings
                        .push(format!("{from}–{to}: published route unavailable: {error}"));
                }
            }
        }
        if !available.iter().any(|direction| {
            direction
                .iter()
                .zip(input.counts)
                .any(|(eligible, count)| *eligible && count > 0)
        }) {
            continue;
        }
        let pair = Pair {
            origin: anchor.clone(),
            destination: airport.clone(),
            departure_runway: anchor_runway,
            arrival_runway: airport_runway,
            compatible: available,
        };
        // Keep two-way coverage ahead of one-way alternatives within the same airport budget.
        if available.iter().all(|direction| {
            direction
                .iter()
                .zip(input.counts)
                .any(|(eligible, count)| *eligible && count > 0)
        }) {
            plan.pairs.push(pair);
            plan.airports.insert(id, airport);
        } else {
            one_way.push(pair);
        }
    }
    for pair in one_way {
        if plan.airports.len() >= policy.network_airport_limit {
            break;
        }
        plan.airports
            .insert(pair.destination.icao.clone(), pair.destination.clone());
        plan.pairs.push(pair);
    }
    if trace {
        eprintln!(
            "Traffic preparation: survey finished in {:.2}s, {} airport pairs",
            started.elapsed().as_secs_f64(),
            plan.pairs.len()
        );
    }
    Ok(plan)
}

fn allocate(pairs: &[Pair], counts: [u8; 4]) -> (Vec<[u8; 4]>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut allocations = vec![[0u8; 4]; pairs.len()];
    let mut remaining = MAXIMUM_AIRCRAFT as u8;
    for category in 0..4 {
        let two_way = pairs
            .iter()
            .enumerate()
            .filter_map(|(index, pair)| {
                pair.compatible
                    .iter()
                    .all(|direction| direction[category])
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        let eligible = if two_way.is_empty() {
            pairs
                .iter()
                .enumerate()
                .filter_map(|(index, pair)| {
                    pair.compatible
                        .iter()
                        .any(|direction| direction[category])
                        .then_some(index)
                })
                .collect::<Vec<_>>()
        } else {
            two_way
        };
        let count = counts[category].min(remaining);
        remaining -= count;
        if count == 0 {
            continue;
        }
        if eligible.is_empty() {
            let category = [
                Category::Airline,
                Category::Regional,
                Category::Cargo,
                Category::GeneralAviation,
            ][category];
            warnings.push(format!(
                "{category:?}: no model-compatible installed airport pair"
            ));
            continue;
        }
        for slot in 0..usize::from(count) {
            allocations[eligible[slot % eligible.len()]][category] += 1;
        }
    }
    (allocations, warnings)
}

/// Split only between independently eligible directions; allocation already enforces the global limit.
fn directional_counts(pair: &Pair, counts: [u8; 4]) -> [[u8; 4]; 2] {
    let mut result = [[0; 4]; 2];
    for (category, count) in counts.into_iter().enumerate() {
        match (pair.compatible[0][category], pair.compatible[1][category]) {
            (true, true) => {
                result[0][category] = count.div_ceil(2);
                result[1][category] = count / 2;
            }
            (true, false) => result[0][category] = count,
            (false, true) => result[1][category] = count,
            (false, false) => {}
        }
    }
    result
}

fn ready_pairs(plan: &Plan, samples: &environment::Samples) -> (Vec<Pair>, BTreeSet<String>) {
    let missing = plan
        .airports
        .values()
        .filter(|airport| {
            samples
                .get(&airport.icao)
                .and_then(|sample| environment::Environment::from_sample(airport, sample).ok())
                .is_none()
        })
        .map(|airport| airport.icao.clone())
        .collect::<BTreeSet<_>>();
    let pairs = plan
        .pairs
        .iter()
        .cloned()
        .map(|mut pair| {
            if missing.contains(&pair.origin.icao) || missing.contains(&pair.destination.icao) {
                pair.compatible = [[false; 4]; 2];
            }
            pair
        })
        .collect();
    (pairs, missing)
}

pub(super) fn prepare(
    mut input: Preparation,
    plan: &Plan,
    local_only: bool,
) -> Result<Scene, String> {
    let trace = std::env::var_os("OPENATC_TRAFFIC_PROFILE").is_some();
    let started = Instant::now();
    input.refresh_weather()?;
    let (mut pairs, missing) = ready_pairs(plan, &input.weather);
    // The first publication plans only the player's own airport pair, so traffic exists in
    // seconds. The rest of the network is admitted by a later merge.
    if local_only {
        let origin = input.origin.icao.clone();
        let destination = input.destination.icao.clone();
        let local = pairs
            .iter()
            .filter(|pair| {
                (pair.origin.icao == origin && pair.destination.icao == destination)
                    || (pair.origin.icao == destination && pair.destination.icao == origin)
            })
            .cloned()
            .collect::<Vec<_>>();
        if !local.is_empty() {
            pairs = local;
        }
    }
    if trace {
        eprintln!(
            "Traffic preparation +{:.2}s: {} network pairs ready",
            started.elapsed().as_secs_f64(),
            pairs.len()
        );
    }
    if !missing.is_empty()
        && pairs
            .iter()
            .all(|pair| pair.compatible.iter().flatten().all(|eligible| !eligible))
    {
        return Err(format!(
            "Waiting for simulator airport weather: {}",
            missing.iter().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    let (allocations, mut warnings) = allocate(&pairs, input.counts);
    warnings.extend(missing.into_iter().map(|airport| {
        format!("{airport}: additional traffic route awaits fresh simulator weather")
    }));
    // All directions in this worker share one scenery planner and its bounded tile cache.
    let terrain = super::scenery::Terrain::new(std::path::Path::new(&input.simulator_root))
        .map(std::cell::RefCell::new);
    let mut scene: Option<Scene> = None;
    for (pair, counts) in pairs.iter().zip(allocations) {
        if counts.iter().all(|count| *count == 0) {
            continue;
        }
        for (direction, counts) in directional_counts(pair, counts).into_iter().enumerate() {
            if counts.iter().all(|count| *count == 0) {
                continue;
            }
            let mut request = input.clone();
            request.origin = pair.origin.as_ref().clone();
            request.destination = pair.destination.as_ref().clone();
            request.runway.clone_from(&pair.departure_runway);
            request.arrival_runway.clone_from(&pair.arrival_runway);
            if direction == 1 {
                std::mem::swap(&mut request.origin, &mut request.destination);
                std::mem::swap(&mut request.runway, &mut request.arrival_runway);
            }
            request.counts = counts;
            if let Some(previous) = &scene {
                previous.observe_prepared(&mut request);
            }
            let from = request.origin.icao.clone();
            let to = request.destination.icao.clone();
            if trace {
                eprintln!(
                    "Traffic preparation +{:.2}s: routes {from}-{to}, counts {counts:?}",
                    started.elapsed().as_secs_f64()
                );
            }
            match prepare_direction_with_terrain(request, &terrain, true) {
                Ok(next) => {
                    scene = Some(match scene {
                        Some(previous) => previous.merge(next)?,
                        None => next,
                    });
                }
                Err(error) => warnings.push(format!("{from}–{to}: {error}")),
            }
        }
    }
    let mut scene = scene
        .ok_or_else(|| format!("No complete traffic airport pair: {}", warnings.join("; ")))?;
    scene.warnings.extend(plan.warnings.iter().cloned());
    scene.warnings.extend(warnings);
    scene.seed_population(
        input.own,
        input.seed,
        input.startup_cast,
        input.taxi_target,
        input.seed_single_arrival,
        &input.observed,
        input.player_airborne,
        input.minimum_airborne_spawn_nm,
    );
    super::ctaf::prepare(&mut scene, &input);
    if trace {
        eprintln!(
            "Traffic preparation: routes finished in {:.2}s",
            started.elapsed().as_secs_f64()
        );
    }
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_balances_published_categories_with_nearby_fields() {
        let nearby = ["NEAR", "NEXT", "THRD", "ALN1", "ALN2", "CGO1", "CGO2"]
            .map(str::to_owned)
            .to_vec();
        let links = BTreeMap::from([
            ("ALN1".into(), BTreeSet::from([Category::Airline])),
            ("ALN2".into(), BTreeSet::from([Category::Airline])),
            ("CGO1".into(), BTreeSet::from([Category::Cargo])),
            ("CGO2".into(), BTreeSet::from([Category::Cargo])),
        ]);
        assert_eq!(
            balanced_candidates(
                nearby.clone(),
                &links,
                &[Category::Cargo, Category::Airline],
                4
            ),
            ["CGO1", "ALN1", "NEAR", "NEXT"]
        );
        assert_eq!(
            balanced_candidates(nearby.clone(), &links, &[Category::GeneralAviation], 4),
            ["NEAR", "NEXT", "THRD", "ALN1"]
        );
        assert_eq!(
            balanced_candidates(nearby.clone(), &links, &[Category::Cargo], 1),
            ["NEAR"]
        );
        assert!(balanced_candidates(nearby.clone(), &links, &[Category::Cargo], 0).is_empty());
        let all = balanced_candidates(nearby, &links, &[Category::Cargo, Category::Airline], 16);
        assert_eq!(all.len(), 7);
        assert_eq!(all.iter().collect::<BTreeSet<_>>().len(), all.len());
    }

    #[test]
    fn unavailable_distant_weather_does_not_block_ready_local_pairs() {
        let airport = |id: &str, latitude| {
            Arc::new(Airport {
                icao: id.into(),
                reference_latitude: latitude,
                ..Default::default()
            })
        };
        let home = airport("HOME", 0.0);
        let near = airport("NEAR", 0.5);
        let far = airport("FAR1", 50.0);
        let pair = |destination| Pair {
            origin: home.clone(),
            destination,
            departure_runway: "09".into(),
            arrival_runway: "27".into(),
            compatible: [[true, false, false, false]; 2],
        };
        let plan = Plan {
            key: "test".into(),
            airports: [home.clone(), near.clone(), far.clone()]
                .into_iter()
                .map(|airport| (airport.icao.clone(), airport))
                .collect(),
            pairs: vec![pair(far), pair(near.clone())],
            warnings: Vec::new(),
        };
        let mut samples = [home, near].into_iter().map(|airport| (airport.icao.clone(), (serde_json::json!({
            "airport":airport.icao, "latitude":airport.reference_latitude, "longitude":0.0, "sampleAltitudeFeet":0.0,
            "pressureHpa":1013.25, "temperatureC":15.0, "magneticVariationDegrees":0.0, "windDegrees":0.0, "windKnots":0.0
        }), Instant::now()))).collect::<environment::Samples>();
        let (ready, missing) = ready_pairs(&plan, &samples);
        assert_eq!(missing, BTreeSet::from(["FAR1".into()]));
        let (allocation, _) = allocate(&ready, [8, 0, 0, 0]);
        assert_eq!(allocation[0], [0; 4]);
        assert_eq!(allocation[1], [8, 0, 0, 0]);
        samples.get_mut("HOME").unwrap().1 =
            Instant::now().checked_sub(Duration::from_secs(46)).unwrap();
        let (ready, _) = ready_pairs(&plan, &samples);
        assert!(ready.iter().all(|pair| pair.compatible == [[false; 4]; 2]));
    }

    #[test]
    fn one_way_routes_keep_the_full_category_allocation_without_inventing_a_return() {
        let pair = Pair {
            origin: Arc::new(Airport::default()),
            destination: Arc::new(Airport::default()),
            departure_runway: "09".into(),
            arrival_runway: "27".into(),
            compatible: [[true, false, true, false], [false, true, true, false]],
        };
        let (allocations, warnings) = allocate(std::slice::from_ref(&pair), [5, 3, 7, 2]);
        assert_eq!(allocations, vec![[5, 3, 7, 0]]);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.starts_with("GeneralAviation:"))
        );
        assert_eq!(
            directional_counts(&pair, allocations[0]),
            [[5, 0, 4, 0], [0, 3, 3, 0]]
        );
        let (allocations, _) = allocate(std::slice::from_ref(&pair), [63; 4]);
        assert_eq!(
            directional_counts(&pair, allocations[0])
                .iter()
                .flatten()
                .map(|n| usize::from(*n))
                .sum::<usize>(),
            63
        );
    }

    #[test]
    fn one_way_fallback_does_not_displace_two_way_category_coverage() {
        let mut pair = Pair {
            origin: Arc::new(Airport::default()),
            destination: Arc::new(Airport::default()),
            departure_runway: "09".into(),
            arrival_runway: "27".into(),
            compatible: [[true, false, false, false]; 2],
        };
        let two_way = pair.clone();
        pair.compatible = [[true, true, false, false], [false; 4]];
        let (allocations, _) = allocate(&[two_way.clone(), pair.clone()], [8, 3, 0, 0]);
        assert_eq!(allocations, vec![[8, 0, 0, 0], [0, 3, 0, 0]]);
        let mut expired = two_way;
        expired.compatible = [[false; 4]; 2];
        let (allocations, _) = allocate(&[expired, pair], [8, 3, 0, 0]);
        assert_eq!(allocations, vec![[0; 4], [8, 3, 0, 0]]);
    }

    #[test]
    fn allocation_keeps_one_global_limit_and_respects_fleet_eligibility() {
        let airport = Arc::new(Airport::default());
        let pair = |compatible| Pair {
            origin: airport.clone(),
            destination: airport.clone(),
            departure_runway: "09".into(),
            arrival_runway: "27".into(),
            compatible: [compatible; 2],
        };
        let pairs = [
            pair([true, false, false, true]),
            pair([true, true, false, false]),
            pair([false, true, false, true]),
        ];
        for counts in [[8, 4, 3, 2], [63, 63, 63, 63], [0, 0, 0, 0]] {
            let (allocated, warnings) = allocate(&pairs, counts);
            assert!(
                allocated
                    .iter()
                    .flatten()
                    .map(|count| usize::from(*count))
                    .sum::<usize>()
                    <= 63
            );
            for (pair, allocation) in pairs.iter().zip(&allocated) {
                for (category, count) in allocation.iter().enumerate() {
                    assert!(
                        pair.compatible.iter().any(|direction| direction[category]) || *count == 0
                    );
                }
            }
            if counts == [8, 4, 3, 2] {
                for (category, expected) in [8, 4, 0, 2].into_iter().enumerate() {
                    assert_eq!(
                        allocated
                            .iter()
                            .map(|allocation| usize::from(allocation[category]))
                            .sum::<usize>(),
                        expected
                    );
                }
                assert!(warnings.iter().any(|warning| warning.starts_with("Cargo:")));
            }
        }
    }
}
