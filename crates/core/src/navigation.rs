//! Route geometry and published crossing limits, separate from planned altitudes.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use crate::state::{FlightPlan, RouteFix, Telemetry};

/// A known route leg near the aircraft.
#[derive(Clone, Debug)]
pub struct Leg {
    /// Destination index in the plan's fix list.
    pub next: usize,
    /// Distance from the leg's centreline in nautical miles.
    pub cross_track_nm: f64,
    /// Projection along the leg; one is the destination fix.
    pub along: f64,
    /// Bearing from the aircraft to that fix, degrees true.
    pub bearing: f64,
    /// Distance to the destination fix.
    pub distance_nm: f64,
}
/// Project onto a straight imported leg. Procedure arcs and vector legs are not inferred.
#[allow(clippy::many_single_char_names)]
#[must_use]
pub fn leg(fixes: &[RouteFix], next: usize, t: &Telemetry) -> Option<Leg> {
    let a = fixes.get(next.checked_sub(1)?)?;
    let b = fixes.get(next)?;
    if !b.path_terminator.is_empty() && !["TF", "DF"].contains(&b.path_terminator.as_str()) {
        return None;
    }
    let cosine = t.latitude.to_radians().cos().abs().max(0.05);
    let lon = |v: f64| (v - t.longitude + 180.0).rem_euclid(360.0) - 180.0;
    let p = [
        lon(a.longitude) * cosine * 60.0,
        (a.latitude - t.latitude) * 60.0,
    ];
    let q = [
        lon(b.longitude) * cosine * 60.0,
        (b.latitude - t.latitude) * 60.0,
    ];
    let d = [q[0] - p[0], q[1] - p[1]];
    let len = d[0] * d[0] + d[1] * d[1];
    if len < 0.01 {
        return None;
    }
    let along = -(p[0] * d[0] + p[1] * d[1]) / len;
    let cross = (p[0] + along * d[0]).hypot(p[1] + along * d[1]);
    Some(Leg {
        next,
        cross_track_nm: cross,
        along,
        bearing: crate::arrival::bearing([t.latitude, t.longitude], [b.latitude, b.longitude]),
        distance_nm: crate::ops::distance_nm(t.latitude, t.longitude, b.latitude, b.longitude),
    })
}

/// The first leg that is still ahead of the aircraft. A vector or direct-to must
/// never target a fix the aircraft has already passed, even when it is off course.
#[must_use]
pub fn route_ahead(fixes: &[RouteFix], next: usize, t: &Telemetry) -> usize {
    let mut next = next.max(1).min(fixes.len().saturating_sub(1).max(1));
    while next + 1 < fixes.len() {
        let Some(fix) = fixes.get(next) else {
            break;
        };
        let captured =
            crate::ops::distance_nm(t.latitude, t.longitude, fix.latitude, fix.longitude) < 0.05;
        let passed = !fix.fly_over
            && (leg(fixes, next, t).is_some_and(|leg| leg.along >= 1.0)
                || fix_passed_on_outbound(fixes, next, t));
        if captured || passed {
            next += 1;
        } else {
            break;
        }
    }
    next.min(fixes.len().saturating_sub(1))
}

/// Recover missed telemetry sequencing only when established on a later straight STAR leg./// This observes route progress; it does not authorise a shortcut or infer missed crossing levels.
#[must_use]
pub fn established_star_leg(fixes: &[RouteFix], next: usize, t: &Telemetry) -> Option<usize> {
    let source = &fixes.get(next)?.constraint_source;
    if !source.starts_with("STAR ") || !t.position_valid {
        return None;
    }
    let track = t.ground_track_degrees?;
    ((next + 1)..fixes.len().min(next + 17)).find(|index| {
        if fixes[next..=*index].iter().any(|fix| {
            fix.constraint_source != *source
                || fix.fly_over
                || !["IF", "TF", "DF", ""].contains(&fix.path_terminator.as_str())
        }) {
            return false;
        }
        let Some(projected) = leg(fixes, *index, t) else {
            return false;
        };
        let course = crate::arrival::bearing(
            [fixes[index - 1].latitude, fixes[index - 1].longitude],
            [fixes[*index].latitude, fixes[*index].longitude],
        );
        projected.cross_track_nm < 0.25
            && (0.05..0.95).contains(&projected.along)
            && turn(course, track).abs() < 10.0
    })
}

/// Observe a fly-by fix from its known outbound track without requiring a
/// sample over the waypoint or extending the incoming course through a turn.
#[must_use]
pub fn fix_passed_on_outbound(fixes: &[RouteFix], next: usize, t: &Telemetry) -> bool {
    let Some(fix) = fixes.get(next).filter(|fix| {
        ["IF", "TF", "DF", "CF", "AF", "RF"].contains(&fix.path_terminator.as_str())
            && !fix.fly_over
    }) else {
        return false;
    };
    // The aircraft has flown by the fix when the fix is behind its track and close enough to
    // matter. This holds for a turning leg, where no straight projection exists, and it stops
    // the monitor from measuring the aircraft against the leg it has already left.
    let track = t.ground_track_degrees.unwrap_or(t.heading_degrees);
    let bearing_to_fix =
        crate::arrival::bearing([t.latitude, t.longitude], [fix.latitude, fix.longitude]);
    if crate::ops::distance_nm(t.latitude, t.longitude, fix.latitude, fix.longitude) < 2.0
        && turn(track, bearing_to_fix).abs() > 90.0
    {
        return true;
    }
    let Some(following) = leg(fixes, next + 1, t) else {
        return false;
    };
    let Some(target) = fixes.get(next + 1) else {
        return false;
    };
    let course = crate::arrival::bearing(
        [fix.latitude, fix.longitude],
        [target.latitude, target.longitude],
    );
    following.along > 0.0
        && following.along < 1.0
        && following.cross_track_nm < 0.5
        && turn(t.ground_track_degrees.unwrap_or(t.heading_degrees), course).abs() < 45.0
}
/// Parse a published altitude in feet or a coded flight level.
fn altitude(value: &str) -> Option<i32> {
    let value = value.trim();
    let (number, multiplier) = value.strip_prefix("FL").map_or((value, 1), |v| (v, 100));
    number
        .parse::<i32>()
        .ok()
        .map(|v| v * multiplier)
        .filter(|v| (0..=60000).contains(v))
}
fn procedure_order(
    kind: &str,
    name: &str,
    runway: &str,
    transition: &str,
    text: &str,
) -> Option<std::collections::BTreeMap<(String, String, u16), usize>> {
    if !["SID", "STAR", "APPCH"].contains(&kind) {
        return None;
    }
    let catalogue = crate::traffic::procedures::Catalogue::parse(text).ok()?;
    let parts = if kind == "SID" {
        catalogue.departure(name, runway, transition)
    } else if kind == "STAR" {
        catalogue.arrival(name, runway, transition)
    } else {
        catalogue
            .approach(name, transition)
            .map(|approach| vec![approach.transition, approach.landing])
    };
    let Ok(parts) = parts else {
        // An incomplete assembly must not become an empty order that silently
        // discards every restriction. The caller falls back to the common,
        // runway and named-transition records.
        return None;
    };
    let mut order = std::collections::BTreeMap::new();
    let mut index = 0;
    for part in parts.into_iter().filter(|part| !part.is_empty()) {
        let (key, _) = catalogue
            .0
            .iter()
            .find(|(_, legs)| legs.as_ptr() == part.as_ptr())?;
        for leg in part {
            order.insert(
                (key.route_type.clone(), key.transition.clone(), leg.sequence),
                index,
            );
            index += 1;
        }
    }
    Some(order)
}
fn record_order(
    order: &std::collections::BTreeMap<(String, String, u16), usize>,
    fields: &[&str],
) -> Option<usize> {
    order
        .get(&(
            fields.get(1)?.to_string(),
            fields.get(3)?.to_string(),
            fields.first()?.parse().ok()?,
        ))
        .copied()
}
/// Merge the selected runway and procedure transitions only.
/// # Panics
///
/// Panics if a procedure record is shorter than the documented field layout.
pub fn load_constraints(plan: &mut FlightPlan, kind: &str, text: &str) {
    let (name, runway, transition) = match kind {
        "SID" => (&plan.sid, &plan.runway, &plan.sid_transition),
        "STAR" => (&plan.star, &plan.arrival_runway, &plan.star_transition),
        "APPCH" => (&plan.approach, &plan.arrival_runway, &String::new()),
        _ => return,
    };
    if name.is_empty() {
        return;
    }
    let order = procedure_order(kind, name, runway, transition, text);
    for line in text.lines() {
        let Some((record, data)) = line.split_once(':') else {
            continue;
        };
        if record != kind {
            continue;
        }
        let f: Vec<_> = data.split(',').map(str::trim).collect();
        if f.len() < 28 || f[2] != name {
            continue;
        }
        if let Some(order) = &order {
            if record_order(order, &f).is_none() {
                continue;
            }
        } else {
            let shared = format!("RW{}B", runway.trim_end_matches(['L', 'R', 'C']));
            if !f[3].is_empty()
                && f[3] != transition
                && f[3] != format!("RW{runway}")
                && f[3] != shared
                && f[3] != "ALL"
            {
                continue;
            }
        }
        if kind == "SID" && ["VM", "VR", "VA", "VD", "VI", "FM"].contains(&f[11]) {
            plan.sid_has_vectors = true;
        }
        let first = altitude(f[23]);
        let second = altitude(f[24]);
        let (min, max) = match f[22] {
            "+" => (first, None),
            "-" => (None, first),
            "B" if first.is_some() && second.is_some() => (
                Some(first.unwrap().min(second.unwrap())),
                Some(first.unwrap().max(second.unwrap())),
            ),
            "H" | "J" if first.is_some() && first == second => (first, first),
            "H" if first.is_some() && second.is_some() && first >= second => (second, first),
            "J" if first.is_some() && second.is_some() => (first, None),
            " " | "" => (first, first),
            _ => (None, None),
        };
        let index = if kind == "SID" {
            plan.fixes.iter().position(|fix| fix.identifier == f[4])
        } else {
            plan.fixes.iter().rposition(|fix| fix.identifier == f[4])
        };
        if let Some(index) = index {
            let fix = &mut plan.fixes[index];
            fix.path_terminator = f[11].into();
            fix.fly_over = f[8].as_bytes().get(1) == Some(&b'Y');

            if let Some(min) = min {
                if fix.minimum_altitude_feet.is_none_or(|old| min >= old) {
                    fix.minimum_altitude_is_flight_level =
                        Some(if first == Some(min) { f[23] } else { f[24] }.starts_with("FL"));
                }
                fix.minimum_altitude_feet =
                    Some(fix.minimum_altitude_feet.map_or(min, |old| old.max(min)));
            }
            if let Some(max) = max {
                if fix.maximum_altitude_feet.is_none_or(|old| max <= old) {
                    fix.maximum_altitude_is_flight_level =
                        Some(if first == Some(max) { f[23] } else { f[24] }.starts_with("FL"));
                }
                fix.maximum_altitude_feet =
                    Some(fix.maximum_altitude_feet.map_or(max, |old| old.min(max)));
            }
            if let Ok(speed) = f[27].parse::<f64>()
                && speed > 0.0
            {
                if f[26] == "+" {
                    fix.minimum_speed_knots =
                        Some(fix.minimum_speed_knots.map_or(speed, |old| old.max(speed)));
                } else if ["", "-"].contains(&f[26]) {
                    fix.maximum_speed_knots =
                        Some(fix.maximum_speed_knots.map_or(speed, |old| old.min(speed)));
                }
            }
            fix.constraint_source = format!("{kind} {name} / installed CIFP");
        }
    }
}
/// Record an unpublished transition and use an unambiguous published name or
/// the common route plus runway transition. The procedure is never dropped.
fn apply_transition_fallback(
    plan: &mut FlightPlan,
    kind: &str,
    catalogue: &crate::traffic::procedures::Catalogue,
) {
    let (name, runway, transition) = if kind == "SID" {
        (&plan.sid, &plan.runway, plan.sid_transition.clone())
    } else if kind == "STAR" {
        (
            &plan.star,
            &plan.arrival_runway,
            plan.star_transition.clone(),
        )
    } else {
        return;
    };
    if name.is_empty()
        || transition.is_empty()
        || transition.starts_with("RW")
        || transition == "ALL"
    {
        return;
    }
    let published = catalogue
        .0
        .keys()
        .any(|key| key.kind == kind && key.name == *name && key.transition == transition);
    if published {
        return;
    }
    let replacement = catalogue
        .0
        .iter()
        .find(|(key, legs)| {
            key.kind == kind
                && key.name == *name
                && if kind == "SID" {
                    legs.last()
                        .is_some_and(|leg| leg.identifier.eq_ignore_ascii_case(&transition))
                } else {
                    legs.first()
                        .is_some_and(|leg| leg.identifier.eq_ignore_ascii_case(&transition))
                }
        })
        .map(|(key, _)| key.transition.clone());
    let label = if kind == "SID" { "SID" } else { "STAR" };
    let note = if let Some(replacement) = replacement {
        if kind == "SID" {
            plan.sid_transition.clone_from(&replacement);
        } else {
            plan.star_transition.clone_from(&replacement);
        }
        format!(
            "{label} {name}: transition {transition} is not published; using the {replacement} transition."
        )
    } else {
        format!(
            "{label} {name}: transition {transition} is not published; using the common route and runway {runway} transition."
        )
    };
    if !plan.procedure_notice.is_empty() {
        plan.procedure_notice.push(' ');
    }
    plan.procedure_notice.push_str(&note);
}

/// Load terminal constraints from the same custom/default navdata priority as X-Plane.
pub fn enrich(plan: &mut FlightPlan, root: &str) {
    plan.fixes.retain(|fix| !fix.procedure_generated);
    plan.sid_has_vectors = false;
    plan.procedure_notice.clear();
    for fix in &mut plan.fixes {
        if fix.constraint_source.ends_with("/ installed CIFP") {
            fix.minimum_altitude_feet = None;
            fix.minimum_altitude_is_flight_level = None;
            fix.maximum_altitude_feet = None;
            fix.maximum_altitude_is_flight_level = None;
            fix.maximum_speed_knots = None;
            fix.minimum_speed_knots = None;
            fix.path_terminator.clear();
            fix.fly_over = false;
            fix.constraint_source.clear();
        }
    }
    for (id, kind) in [
        (plan.departure.clone(), "SID"),
        (plan.destination.clone(), "STAR"),
        (plan.destination.clone(), "APPCH"),
    ] {
        let custom = format!("{root}/Custom Data/CIFP/{id}.dat");
        let path = if std::path::Path::new(&custom).is_file() {
            custom
        } else {
            format!("{root}/Resources/default data/CIFP/{id}.dat")
        };
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(catalogue) = crate::traffic::procedures::Catalogue::parse(&text) {
                apply_transition_fallback(plan, kind, &catalogue);
            }
            if ["SID", "STAR", "APPCH"].contains(&kind) {
                let fix_path = [
                    format!("{root}/Custom Data/earth_fix.dat"),
                    format!("{root}/Resources/default data/earth_fix.dat"),
                ]
                .into_iter()
                .find(|path| std::path::Path::new(path).is_file());
                if let Some(fix_path) = fix_path
                    && let Ok(fixes) = std::fs::read_to_string(fix_path)
                {
                    expand_terminal(plan, kind, &text, &fixes);
                }
            }
            load_constraints(plan, kind, &text);
        }
    }
}

/// Insert named terminal fixes before applying their restrictions. Unsupported
/// leg types remain labelled; this is not permission to fly a straight shortcut.
fn expand_terminal(plan: &mut FlightPlan, kind: &str, procedure: &str, fixes: &str) {
    let (name, runway, transition) = if kind == "SID" {
        (&plan.sid, &plan.runway, &plan.sid_transition)
    } else if kind == "STAR" {
        (&plan.star, &plan.arrival_runway, &plan.star_transition)
    } else {
        (&plan.approach, &plan.arrival_runway, &String::new())
    };
    if name.is_empty() || plan.fixes.is_empty() {
        return;
    }
    let shared = format!("RW{}B", runway.trim_end_matches(['L', 'R', 'C']));
    let order = procedure_order(kind, name, runway, transition, procedure);
    let mut records: Vec<_> = procedure
        .lines()
        .filter_map(|line| {
            let (record, data) = line.split_once(':')?;
            let fields: Vec<_> = data.split(',').map(str::trim).collect();
            (record == kind
                && fields.len() >= 28
                && fields[2] == name
                && !fields[4].is_empty()
                && !fields[4].starts_with("RW")
                && order.as_ref().map_or_else(
                    || {
                        fields[3].is_empty()
                            || fields[3] == transition
                            || fields[3] == format!("RW{runway}")
                            || fields[3] == shared
                            || fields[3] == "ALL"
                    },
                    |order| record_order(order, &fields).is_some(),
                ))
            .then_some(fields)
        })
        .take(256)
        .collect();
    if let Some(order) = &order {
        records.sort_by_key(|fields| record_order(order, fields));
    }
    let wanted: std::collections::BTreeSet<_> = records.iter().map(|f| (f[4], f[5])).collect();
    let airport = if kind == "SID" {
        &plan.departure
    } else {
        &plan.destination
    };
    let mut coordinates = std::collections::BTreeMap::new();
    let mut ambiguous = std::collections::BTreeSet::new();
    for line in fixes.lines() {
        let fields: Vec<_> = line.split_whitespace().take(5).collect();
        if fields.len() < 5 || !wanted.contains(&(fields[2], fields[4])) {
            continue;
        }
        if fields[3] != airport && fields[3] != "ENRT" {
            continue;
        }
        let (Ok(latitude), Ok(longitude)) = (fields[0].parse::<f64>(), fields[1].parse::<f64>())
        else {
            continue;
        };
        if !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
            continue;
        }
        let key = (fields[2], fields[4]);
        let value = (latitude, longitude);
        if coordinates.get(&key).is_some_and(|old| *old != value) {
            ambiguous.insert(key);
        }
        coordinates.insert(key, value);
    }
    let mut cursor = if kind == "SID" {
        plan.fixes
            .iter()
            .position(|fix| fix.identifier == plan.departure)
            .map_or(1, |index| index + 1)
    } else if kind == "APPCH" {
        plan.fixes
            .iter()
            .position(|fix| fix.identifier == plan.destination)
            .unwrap_or(plan.fixes.len())
    } else {
        let Some(anchor) = records.first().and_then(|record| {
            plan.fixes
                .iter()
                .rposition(|fix| fix.identifier == record[4])
        }) else {
            return;
        };
        anchor
    };
    for fields in records {
        if plan.fixes.len() >= 512 {
            break;
        }
        if let Some(index) = plan
            .fixes
            .iter()
            .enumerate()
            .skip(cursor)
            .find_map(|(index, fix)| (fix.identifier == fields[4]).then_some(index))
        {
            cursor = index + 1;
            continue;
        }
        let key = (fields[4], fields[5]);
        if ambiguous.contains(&key) {
            continue;
        }
        let Some(&(latitude, longitude)) = coordinates.get(&key) else {
            continue;
        };
        plan.fixes.insert(
            cursor.min(plan.fixes.len()),
            crate::state::RouteFix {
                procedure_generated: true,
                identifier: fields[4].into(),
                latitude,
                longitude,
                path_terminator: fields[11].into(),
                ..Default::default()
            },
        );
        cursor += 1;
    }
}
/// Smallest signed turn from one heading to another.
#[must_use]
pub fn turn(from: f64, to: f64) -> f64 {
    (to - from + 180.0).rem_euclid(360.0) - 180.0
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    #[test]
    fn fly_by_reversal_sequences_without_crossing_the_incoming_extension() {
        let mut fixes = vec![
            RouteFix {
                latitude: 0.0,
                ..Default::default()
            },
            RouteFix {
                latitude: 0.1,
                path_terminator: "TF".into(),
                ..Default::default()
            },
            RouteFix {
                latitude: 0.0,
                path_terminator: "TF".into(),
                ..Default::default()
            },
        ];
        let mut telemetry = Telemetry {
            latitude: 0.09,
            ground_track_degrees: Some(180.0),
            heading_degrees: 180.0,
            ..Default::default()
        };
        assert!(leg(&fixes, 1, &telemetry).unwrap().along < 1.0);
        assert!(fix_passed_on_outbound(&fixes, 1, &telemetry));
        telemetry.ground_track_degrees = Some(0.0);
        assert!(!fix_passed_on_outbound(&fixes, 1, &telemetry));
        telemetry.ground_track_degrees = Some(180.0);
        fixes[1].fly_over = true;
        assert!(!fix_passed_on_outbound(&fixes, 1, &telemetry));
    }
    #[test]
    fn established_star_progress_does_not_guess_arcs_or_shortcuts() {
        let mut fixes = [(0.0, 0.0), (0.0, 0.2), (0.2, 0.2)]
            .into_iter()
            .map(|(latitude, longitude)| RouteFix {
                latitude,
                longitude,
                constraint_source: "STAR TEST / installed CIFP".into(),
                path_terminator: "TF".into(),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        let t = Telemetry {
            latitude: 0.1,
            longitude: 0.2,
            position_valid: true,
            ground_track_degrees: Some(0.0),
            ..Default::default()
        };
        assert_eq!(established_star_leg(&fixes, 1, &t), Some(2));
        fixes[2].path_terminator = "RF".into();
        assert!(established_star_leg(&fixes, 1, &t).is_none());
        fixes[2].path_terminator = "TF".into();
        fixes[2].constraint_source = "STAR OTHER".into();
        assert!(established_star_leg(&fixes, 1, &t).is_none());
    }
    #[test]
    fn minimum_speed_does_not_become_a_maximum_speed() {
        let mut plan = FlightPlan {
            sid: "TEST1".into(),
            runway: "04".into(),
            fixes: vec![RouteFix {
                identifier: "FIX".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let row = |sequence, condition, speed| {
            let mut fields = vec![""; 38];
            fields[0] = sequence;
            fields[1] = "1";
            fields[2] = "TEST1";
            fields[3] = "RW04";
            fields[4] = "FIX";
            fields[11] = "TF";
            fields[26] = condition;
            fields[27] = speed;
            format!("SID:{};", fields.join(","))
        };
        load_constraints(
            &mut plan,
            "SID",
            &[row("010", "+", "150"), row("020", "-", "230")].join("\n"),
        );
        assert_eq!(plan.fixes[0].minimum_speed_knots, Some(150.0));
        assert_eq!(plan.fixes[0].maximum_speed_knots, Some(230.0));
    }
    #[test]
    fn an_unpublished_star_transition_keeps_the_common_route_restrictions() {
        let mut plan = FlightPlan {
            destination: "TEST".into(),
            star: "TEST1".into(),
            star_transition: "KERAX".into(),
            arrival_runway: "35R".into(),
            fixes: vec![RouteFix {
                identifier: "INTERNAL".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let row = |sequence, transition, identifier| {
            let mut fields = vec![""; 28];
            fields[0] = sequence;
            fields[2] = "TEST1";
            fields[3] = transition;
            fields[4] = identifier;
            fields[5] = "LI";
            fields[11] = "TF";
            fields[22] = "-";
            fields[23] = "FL120";
            format!("STAR:{};", fields.join(","))
        };
        let text = [
            row("010", "ENTRY", "ENTRYFX"),
            row("020", "", "INTERNAL"),
            row("030", "RW35R", "RUNWAY"),
        ]
        .join("\n");
        load_constraints(&mut plan, "STAR", &text);
        assert_eq!(
            plan.fixes[0].maximum_altitude_feet,
            Some(12000),
            "The bogus SimBrief transition dropped every STAR restriction"
        );
    }

    #[test]
    fn internal_star_fixes_receive_constraints_without_duplicate_insertion() {
        let mut plan = FlightPlan {
            destination: "TEST".into(),
            star: "TEST1".into(),
            arrival_runway: "35R".into(),
            fixes: vec![
                RouteFix {
                    identifier: "ENTRY".into(),
                    latitude: 44.9,
                    longitude: 8.6,
                    ..Default::default()
                },
                RouteFix {
                    identifier: "TEST".into(),
                    latitude: 45.6,
                    longitude: 8.7,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut entry = vec![""; 28];
        entry[0] = "010";
        entry[2] = "TEST1";
        entry[3] = "RW35B";
        entry[4] = "ENTRY";
        entry[5] = "LI";
        entry[11] = "IF";
        let mut crossing = entry.clone();
        crossing[0] = "020";
        crossing[4] = "INTERNAL";
        crossing[11] = "TF";
        crossing[22] = "-";
        crossing[23] = "FL120";
        let text = format!("STAR:{}\nSTAR:{}", entry.join(","), crossing.join(","));
        let fixes = "45.0 8.6 INTERNAL TEST LI\n40.0 4.0 INTERNAL OTHER LI";
        expand_terminal(&mut plan, "STAR", &text, fixes);
        load_constraints(&mut plan, "STAR", &text);
        let fix = &plan.fixes[1];
        assert_eq!(fix.identifier, "INTERNAL");
        assert!(fix.procedure_generated);
        assert_eq!(fix.maximum_altitude_feet, Some(12000));
        assert_eq!(fix.latitude, 45.0);
        expand_terminal(&mut plan, "STAR", &text, fixes);
        assert_eq!(plan.fixes.len(), 3);
    }
    #[test]
    #[ignore = "requires installed licensed navigation data via OPENATC_TEST_SIMULATOR_ROOT"]
    fn installed_devo3e_inserts_the_reported_descent_restriction() {
        let root = std::env::var("OPENATC_TEST_SIMULATOR_ROOT").unwrap();
        let mut plan = FlightPlan {
            departure: "LFMN".into(),
            destination: "LIMC".into(),
            star: "DEVO3E".into(),
            arrival_runway: "35R".into(),
            fixes: vec![
                RouteFix {
                    identifier: "DEVOX".into(),
                    latitude: 44.9,
                    longitude: 8.6,
                    ..Default::default()
                },
                RouteFix {
                    identifier: "LIMC".into(),
                    latitude: 45.6,
                    longitude: 8.7,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        enrich(&mut plan, &root);
        assert_eq!(
            plan.fixes
                .iter()
                .find(|fix| fix.identifier == "MC588")
                .unwrap()
                .maximum_altitude_feet,
            Some(12000)
        );
        let count = plan.fixes.len();
        enrich(&mut plan, &root);
        assert_eq!(plan.fixes.len(), count);
    }
    #[test]
    fn an_unpublished_transition_is_reported_in_the_plan_and_not_dropped() {
        let root =
            std::env::temp_dir().join(format!("openatc-transition-notice-{}", std::process::id()));
        let cifp = root.join("Custom Data/CIFP/TEST.dat");
        std::fs::create_dir_all(cifp.parent().unwrap()).unwrap();
        let row = |sequence, transition, identifier| {
            let mut fields = vec![""; 29];
            fields[0] = sequence;
            fields[2] = "TEST1";
            fields[3] = transition;
            fields[4] = identifier;
            fields[5] = "LI";
            fields[11] = "TF";
            fields[22] = "-";
            fields[23] = "FL120";
            format!("SID:{};", fields.join(","))
        };
        std::fs::write(
            &cifp,
            [
                row("010", "RW09", "RUNWAY"),
                row("020", "", "INTERNAL"),
                row("030", "NORTH", "EXITFX"),
            ]
            .join("\n"),
        )
        .unwrap();
        let mut plan = FlightPlan {
            departure: "TEST".into(),
            sid: "TEST1".into(),
            sid_transition: "VIPPA".into(),
            runway: "09".into(),
            fixes: vec![RouteFix {
                identifier: "INTERNAL".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        enrich(&mut plan, root.to_str().unwrap());
        assert!(
            plan.procedure_notice.contains("VIPPA"),
            "{}",
            plan.procedure_notice
        );
        assert!(plan.procedure_notice.contains("common route"));
        assert_eq!(plan.fixes[0].maximum_altitude_feet, Some(12000));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn projection_and_turn_wrap() {
        let fixes = vec![
            RouteFix {
                latitude: 0.0,
                longitude: 0.0,
                ..Default::default()
            },
            RouteFix {
                latitude: 0.0,
                longitude: 1.0,
                ..Default::default()
            },
        ];
        let t = Telemetry {
            latitude: 0.1,
            longitude: 0.5,
            ..Default::default()
        };
        let l = leg(&fixes, 1, &t).unwrap();
        assert!((l.cross_track_nm - 6.0).abs() < 0.1);
        assert!((l.along - 0.5).abs() < 0.01);
        assert_eq!(turn(350.0, 10.0), 20.0);
        let passed = leg(
            &fixes,
            1,
            &Telemetry {
                latitude: 0.0,
                longitude: 1.1,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(passed.along > 1.0);
        assert!(passed.cross_track_nm < 0.001);
    }
    #[test]
    fn initial_fix_sequences_from_observed_outbound_track_without_an_incoming_course() {
        let fixes = vec![
            RouteFix {
                longitude: 0.0,
                ..Default::default()
            },
            RouteFix {
                longitude: 1.0,
                path_terminator: "IF".into(),
                ..Default::default()
            },
            RouteFix {
                longitude: 2.0,
                path_terminator: "TF".into(),
                ..Default::default()
            },
        ];
        let telemetry = Telemetry {
            latitude: 0.005,
            longitude: 1.1,
            heading_degrees: 90.0,
            ..Default::default()
        };
        assert!(leg(&fixes, 1, &telemetry).is_none());
        assert!(fix_passed_on_outbound(&fixes, 1, &telemetry));
        for terminator in ["TF", "DF", "CF", "AF", "RF"] {
            let mut endpoint = fixes.clone();
            endpoint[1].path_terminator = terminator.into();
            assert!(fix_passed_on_outbound(&endpoint, 1, &telemetry));
            endpoint[1].fly_over = true;
            assert!(!fix_passed_on_outbound(&endpoint, 1, &telemetry));
        }
        assert!(!fix_passed_on_outbound(
            &fixes,
            1,
            &Telemetry {
                heading_degrees: 270.0,
                ..telemetry.clone()
            }
        ));
        assert!(!fix_passed_on_outbound(
            &fixes,
            1,
            &Telemetry {
                latitude: 0.1,
                ..telemetry
            }
        ));
        let mut vector = fixes;
        vector[2].path_terminator = "VI".into();
        assert!(!fix_passed_on_outbound(
            &vector,
            1,
            &Telemetry {
                longitude: 1.1,
                heading_degrees: 90.0,
                ..Default::default()
            }
        ));
    }
    #[test]
    fn approach_glideslope_fields_follow_the_published_reading() {
        let mut plan = FlightPlan {
            approach: "I35RY".into(),
            arrival_runway: "35R".into(),
            fixes: vec![
                RouteFix {
                    identifier: "CI3RY".into(),
                    ..Default::default()
                },
                RouteFix {
                    identifier: "FI3RY".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let text = "APPCH:010,I,I35RY, ,CI3RY,LI,P,C,E  I, ,   ,IF, ,MLP,LI,P,I,      ,1660,0145,    ,    ,J,04000,04000,     , ,   ,    ;\nAPPCH:020,I,I35RY, ,FI3RY,LI,P,C,E  F, ,   ,CF, ,MLP,LI,P,I,      ,1660,0125,3460,0020,H,04000,04000,     ,-,170,    ;";
        let text = format!(
            "{text}\nAPPCH:030,I,I35RY, ,RW35R,LI,P,G,G  M, ,   ,CF, ,MLP,LI,P,I,      ,1660,0023,3460,0102, ,00748,     ,     , ,   ,-300,   , , , , , ,0,D,S;\nAPPCH:040,I,I35RY, ,RW35R,LI,P,G,G M , ,   ,FA, ,SRN,LI,D, ,      ,2585,0121,3460,    ,+,01450,     ,     , ,   ,    ,   , , , , , ,0,D,S;"
        );
        load_constraints(&mut plan, "APPCH", &text);
        for fix in &plan.fixes {
            assert_eq!(fix.minimum_altitude_feet, Some(4000));
            assert_eq!(fix.maximum_altitude_feet, Some(4000));
        }
        let mut uncertain = plan.clone();
        for fix in &mut uncertain.fixes {
            fix.minimum_altitude_feet = None;
            fix.maximum_altitude_feet = None;
        }
        load_constraints(
            &mut uncertain,
            "APPCH",
            &text.replace("04000,04000", "04000,03000"),
        );
        let glide = uncertain
            .fixes
            .iter()
            .find(|fix| fix.identifier == "FI3RY")
            .expect("glide-path fix");
        assert_eq!(glide.minimum_altitude_feet, Some(3000));
        assert_eq!(glide.maximum_altitude_feet, Some(4000));
        let intercept = uncertain
            .fixes
            .iter()
            .find(|fix| fix.identifier == "CI3RY")
            .expect("glide-slope intercept fix");
        assert_eq!(intercept.minimum_altitude_feet, Some(4000));
        assert_eq!(intercept.maximum_altitude_feet, None);
    }

    #[test]
    fn planned_altitude_is_not_a_restriction_and_other_runways_are_excluded() {
        let mut p = FlightPlan {
            sid: "TEST1".into(),
            runway: "25L".into(),
            fixes: vec![RouteFix {
                identifier: "FIX".into(),
                altitude_feet: 5000.0,
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut f = vec![""; 38];
        f[2] = "TEST1";
        f[3] = "RW25B";
        f[4] = "FIX";
        f[22] = "B";
        f[23] = "09000";
        f[24] = "FL050";
        load_constraints(&mut p, "SID", &format!("SID:{};", f.join(",")));
        assert_eq!(p.fixes[0].minimum_altitude_feet, Some(5000));
        assert_eq!(p.fixes[0].maximum_altitude_feet, Some(9000));
        f[3] = "RW07B";
        f[22] = "-";
        f[23] = "03000";
        load_constraints(&mut p, "SID", &format!("SID:{};", f.join(",")));
        assert_eq!(p.fixes[0].maximum_altitude_feet, Some(9000));
    }
}

/// Installed one-degree grid minimum off-route altitudes, in feet.
#[derive(Clone, Debug, Default)]
pub struct Mora(pub std::collections::BTreeMap<(i32, i32), i32>);
impl Mora {
    /// Custom navdata takes priority over the bundled grid.
    #[must_use]
    pub fn load(root: &str) -> Self {
        let custom = format!("{root}/Custom Data/earth_mora.dat");
        let path = if std::path::Path::new(&custom).is_file() {
            custom
        } else {
            format!("{root}/Resources/default data/earth_mora.dat")
        };
        std::fs::read_to_string(path).map_or_else(|_| Self::default(), |text| Self::parse(&text))
    }
    /// Rows contain thirty consecutive longitude cells, each in hundreds of feet.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut cells = std::collections::BTreeMap::new();
        for line in text.lines() {
            let f: Vec<_> = line.split_whitespace().collect();
            if f.len() != 32 {
                continue;
            }
            let (Ok(lat), Ok(lon)) = (f[0].parse::<i32>(), f[1].parse::<i32>()) else {
                continue;
            };
            for (index, value) in f[2..].iter().enumerate() {
                if let Ok(height) = value.parse::<i32>()
                    && (1..600).contains(&height)
                {
                    cells.insert((lat, lon + index as i32), height * 100);
                }
            }
        }
        Self(cells)
    }
    /// Conservative floor along a prospective off-route vector; missing cells stay unknown.
    #[must_use]
    pub fn corridor(&self, from: [f64; 2], to: [f64; 2]) -> Option<i32> {
        if [from, to].iter().any(|point| {
            !point.iter().all(|value| value.is_finite())
                || point[0].abs() >= 85.0
                || point[1].abs() > 180.0
        }) {
            return None;
        }
        let vector = |point: [f64; 2]| {
            let (latitude, longitude) = (point[0].to_radians(), point[1].to_radians());
            [
                latitude.cos() * longitude.cos(),
                latitude.cos() * longitude.sin(),
                latitude.sin(),
            ]
        };
        let a = vector(from);
        let b = vector(to);
        let dot = a
            .iter()
            .zip(b)
            .map(|(a, b)| a * b)
            .sum::<f64>()
            .clamp(-1.0, 1.0);
        let tangent = [b[0] - dot * a[0], b[1] - dot * a[1], b[2] - dot * a[2]];
        let sine = tangent
            .iter()
            .map(|value| value * value)
            .sum::<f64>()
            .sqrt();
        let angle = sine.atan2(dot);
        // Antipodal endpoints do not specify one unambiguous shortest route.
        if sine < 1e-12 && dot < 0.0 {
            return None;
        }
        let direction = tangent.map(|value| value / sine.max(1e-15));
        let point = |along: f64| {
            let v =
                std::array::from_fn::<_, 3, _>(|i| a[i] * along.cos() + direction[i] * along.sin());
            [
                v[2].clamp(-1.0, 1.0).asin().to_degrees(),
                v[1].atan2(v[0]).to_degrees(),
            ]
        };
        let extremum = direction[2].atan2(a[2]).rem_euclid(std::f64::consts::PI);
        let count = (angle.to_degrees() * 30.0).ceil().max(1.0) as usize;
        let mut maximum = 0;
        for index in 0..count {
            let start = angle * index as f64 / count as f64;
            let end = angle * (index + 1) as f64 / count as f64;
            let first = if index == 0 { from } else { point(start) };
            let last = if index + 1 == count { to } else { point(end) };
            let mut south = first[0].min(last[0]);
            let mut north = first[0].max(last[0]);
            if extremum > start && extremum < end {
                let latitude = point(extremum)[0];
                south = south.min(latitude);
                north = north.max(latitude);
            }
            if south <= -85.0 || north >= 85.0 {
                return None;
            }
            let longitude = first[1] + turn(first[1], last[1]);
            let west = first[1].min(longitude).floor() as i32;
            let east = first[1].max(longitude).floor() as i32;
            // Cover every intersected grid rectangle, including short corner crossings
            // between samples. Latitude extrema bound the great-circle arc as well.
            for latitude in south.floor() as i32..=north.floor() as i32 {
                for longitude in west..=east {
                    let cell = (latitude, (longitude + 180).rem_euclid(360) - 180);
                    let height = *self.0.get(&cell)?;
                    if !(100..60000).contains(&height) {
                        return None;
                    }
                    maximum = maximum.max(height);
                }
            }
        }
        Some(maximum)
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod mora_tests {
    use super::Mora;
    use std::collections::BTreeMap;

    #[test]
    fn mora_covers_short_corner_crossings_and_unknown_cells() {
        let mut grid = Mora(BTreeMap::from([
            ((0, 0), 1000),
            ((0, 1), 1000),
            ((1, 0), 12000),
            ((1, 1), 1000),
        ]));
        let from = [0.999, 0.99];
        let to = [1.01, 1.001];
        assert_eq!(grid.corridor(from, to), Some(12000));
        assert_eq!(grid.corridor(to, from), Some(12000));
        grid.0.remove(&(1, 0));
        assert_eq!(grid.corridor(from, to), None);
    }

    #[test]
    fn mora_follows_the_dateline_and_great_circle_latitude_peak() {
        let grid = Mora(BTreeMap::from([((10, 179), 1000), ((10, -180), 6000)]));
        assert_eq!(grid.corridor([10.2, 179.9], [10.2, -179.9]), Some(6000));
        assert_eq!(grid.corridor([10.2, -179.9], [10.2, 179.9]), Some(6000));
        let mut cells = BTreeMap::new();
        for latitude in 59..=61 {
            for longitude in -10..=10 {
                cells.insert(
                    (latitude, longitude),
                    if latitude == 60 { 9000 } else { 1000 },
                );
            }
        }
        let grid = Mora(cells);
        assert_eq!(grid.corridor([59.9, -10.0], [59.9, 10.0]), Some(9000));
    }

    #[test]
    fn mora_rejects_invalid_or_ambiguous_paths() {
        let grid = Mora(BTreeMap::from([((10, 20), 1000)]));
        assert_eq!(grid.corridor([10.2, 20.2], [10.2, 20.2]), Some(1000));
        for (from, to) in [
            ([f64::NAN, 20.0], [10.0, 20.0]),
            ([90.0, 20.0], [10.0, 20.0]),
            ([10.0, 181.0], [10.0, 20.0]),
            ([0.0, 0.0], [0.0, 180.0]),
            ([84.9, 0.0], [84.9, 180.0]),
        ] {
            assert_eq!(grid.corridor(from, to), None);
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod route_ahead_tests {
    use super::*;
    use crate::state::{RouteFix, Telemetry};

    fn fix(identifier: &str, longitude: f64) -> RouteFix {
        RouteFix {
            identifier: identifier.into(),
            longitude,
            path_terminator: "TF".into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_vector_target_advances_past_a_fix_the_aircraft_has_left_behind() {
        let fixes = vec![fix("ORIGIN", 0.0), fix("PASSED", 0.2), fix("AHEAD", 0.6)];
        let t = Telemetry {
            position_valid: true,
            latitude: 0.05,
            longitude: 0.4,
            heading_degrees: 90.0,
            ground_track_degrees: Some(90.0),
            ..Default::default()
        };
        assert_eq!(route_ahead(&fixes, 1, &t), 2);
        let t = Telemetry {
            longitude: 0.1,
            ..t
        };
        assert_eq!(route_ahead(&fixes, 1, &t), 1);
    }

    #[test]
    fn a_fix_flown_by_on_a_turning_leg_counts_as_passed() {
        // The aircraft has left the first fix on a turn, so no straight projection exists for
        // the leg that follows it. The fix is behind the aircraft and close, so the route
        // progress must move on rather than measure the aircraft against the leg it left.
        let fixes = vec![
            fix("ORIGIN", 0.0),
            fix("LEFT", 0.1),
            RouteFix {
                identifier: "TURN".into(),
                longitude: 0.1,
                latitude: 0.02,
                path_terminator: "RF".into(),
                ..Default::default()
            },
        ];
        let t = Telemetry {
            position_valid: true,
            latitude: 0.02,
            longitude: 0.11,
            heading_degrees: 0.0,
            ground_track_degrees: Some(0.0),
            ..Default::default()
        };
        assert!(fix_passed_on_outbound(&fixes, 1, &t));
        assert_eq!(route_ahead(&fixes, 1, &t), 2);
    }
}
