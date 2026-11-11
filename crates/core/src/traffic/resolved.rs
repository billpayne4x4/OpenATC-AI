//! Decode terminal crossings without discarding unsupported path terminators.
use super::{
    air::{self, Atmosphere},
    coordinates::Points,
    curves::Turn,
    motion::Waypoint,
    procedures::Leg,
    trajectory::Fix,
};
use crate::{
    airport::Airport,
    arrival::{bearing, destination, distance_nm},
};

/// Environmental and performance inputs already sampled outside the planning worker.
pub struct Context<'a> {
    /// Installed airport geometry.
    pub airport: &'a Airport,
    /// Region-qualified terminal coordinates.
    pub points: &'a Points,
    /// Airport pressure and temperature reference.
    pub atmosphere: Atmosphere,
    /// Local magnetic variation, east positive.
    pub variation: f64,
    /// Wind direction true, from which it blows.
    pub wind_from: f64,
    /// Wind speed in knots.
    pub wind_knots: f64,
    /// Planned indicated speed before published limits.
    pub indicated_knots: f64,
    /// Maximum distance permitted for a published heading intercept.
    pub maximum_intercept_nm: f64,
    /// Planned climb rate used for altitude-terminated legs.
    pub climb_fpm: f64,
    /// Geometric terrain/corridor floor supplied by the route planner.
    pub minimum_feet: f64,
    /// Geometric aircraft ceiling.
    pub maximum_feet: f64,
    /// Preferred geometric level.
    pub preferred_feet: f64,
}

/// Convert a published crossing band to geometric heights; this grants no path or clearance.
/// # Panics
///
/// Panics if a crossing leg has no resolved altitude values.
pub fn altitude_bounds(leg: &Leg, c: &Context<'_>) -> Result<(f64, f64), String> {
    let convert = |altitude: Option<i32>, standard: Option<bool>| -> Result<Option<f64>, String> {
        altitude
            .map(|value| {
                c.atmosphere.geometric(
                    f64::from(value),
                    standard.ok_or("Missing published pressure reference")?,
                )
            })
            .transpose()
    };
    let one = convert(leg.altitude_one, leg.altitude_one_is_flight_level)?;
    let two = convert(leg.altitude_two, leg.altitude_two_is_flight_level)?;
    let (low, high) = match leg.altitude_description.as_str() {
        "" => (one, one),
        "+" if one.is_some() => (one, None),
        "-" if one.is_some() => (None, one),
        "B" if one.is_some() && two.is_some() => (
            Some(one.unwrap().min(two.unwrap())),
            Some(one.unwrap().max(two.unwrap())),
        ),
        // H at a glide-path fix: the second altitude is the glide-path altitude at that
        // fix (for example 1610 ft at 5 NM for a 3 degree path), the first is the ceiling.
        "H" if one.is_some() && two.is_some() && one >= two => (two, one),
        // At the FACF, J's second altitude is a glide-slope intercept reference,
        // not an upper crossing limit. The first altitude remains the published minimum.
        "J" if one.is_some() && two.is_some() => (one, None),
        _ => {
            return Err(format!(
                "Unsupported vertical descriptor at {}",
                leg.identifier
            ));
        }
    };
    let low = low.unwrap_or(c.minimum_feet).max(c.minimum_feet);
    let high = high.unwrap_or(c.maximum_feet).min(c.maximum_feet);
    if low > high {
        return Err(format!(
            "Crossing conflicts with aircraft or terrain envelope at {}",
            leg.identifier
        ));
    }
    Ok((low, high))
}

fn intercept(
    from: [f64; 2],
    heading: f64,
    fix: [f64; 2],
    course: f64,
    maximum_nm: f64,
) -> Result<[f64; 2], String> {
    let scale = from[0].to_radians().cos();
    let delta = [
        crate::navigation::turn(from[1], fix[1]) * 60.0 * scale,
        (fix[0] - from[0]) * 60.0,
    ];
    let u = [heading.to_radians().sin(), heading.to_radians().cos()];
    let v = [course.to_radians().sin(), course.to_radians().cos()];
    let cross = |a: [f64; 2], b: [f64; 2]| a[0] * b[1] - a[1] * b[0];
    let divisor = cross(u, v);
    if divisor.abs() < 0.001 {
        return Err("Published intercept courses are parallel".into());
    }
    let along = cross(delta, v) / divisor;
    let remaining = cross(u, delta) / divisor;
    // The limit bounds this intercept leg, not the following course-to-fix leg.
    if !(0.001..=maximum_nm).contains(&along) || !remaining.is_finite() || remaining < 0.0 {
        return Err(format!(
            "Published course intercept is behind the aircraft or outside its limit (forward {along:.2} NM, remaining {remaining:.2} NM, limit {maximum_nm:.2} NM)"
        ));
    }
    Ok(destination(from, heading, along))
}

/// ARINC path terminators that publish a manual transition point instead of flyable geometry.
pub const MANUAL_TERMINATORS: [&str; 2] = ["FM", "VM"];

/// A published manual termination: the procedure ends its computable path here.
/// The remaining lateral geometry is not published, so any onward path is an amendment.
#[derive(Clone, Debug, PartialEq)]
pub struct ManualTermination {
    /// Path terminator that ends the computable published geometry.
    pub terminator: String,
    /// Published identifier of the manual transition point, when the record carries one.
    pub identifier: String,
}

/// Resolve fixes, published courses and heading intercepts without granting a clearance.
/// Corridor protection and reachable vertical scheduling remain separate checks.
pub fn crossings(start: Waypoint, legs: &[Leg], c: &Context<'_>) -> Result<Vec<Fix>, String> {
    decode(start, legs, c, false)
}

/// Keep an initial boundary restriction for the full-flight solver to apply upstream.
/// Unlike an aircraft check-in, planning must not assume its crossing altitude is already fixed.
pub fn boundary_crossings(
    start: Waypoint,
    legs: &[Leg],
    c: &Context<'_>,
) -> Result<Vec<Fix>, String> {
    decode(start, legs, c, true)
}

/// Decode a procedure that ends at a published manual termination.
///
/// Returns the computable published crossings together with the manual transition that
/// follows them. Callers may only continue past that point by issuing and read-backing an
/// explicit route amendment; this decode never authorises lateral movement on its own.
pub fn amended_crossings(
    start: Waypoint,
    legs: &[Leg],
    c: &Context<'_>,
) -> Result<(Vec<Fix>, Option<ManualTermination>), String> {
    decode_amended(start, legs, c, true)
}

fn decode(
    start: Waypoint,
    legs: &[Leg],
    c: &Context<'_>,
    retain_boundary: bool,
) -> Result<Vec<Fix>, String> {
    decode_amended(start, legs, c, retain_boundary).and_then(|(fixes, manual)| match manual {
        None => Ok(fixes),
        Some(manual) => Err(format!(
            "{} uses unsupported {} geometry",
            manual.identifier, manual.terminator
        )),
    })
}

fn decode_amended(
    start: Waypoint,
    legs: &[Leg],
    c: &Context<'_>,
    retain_boundary: bool,
) -> Result<(Vec<Fix>, Option<ManualTermination>), String> {
    if legs.is_empty()
        || legs.len() > 128
        || !c.variation.is_finite()
        || !(-180.0..=180.0).contains(&c.variation)
        || !(100.0..=10000.0).contains(&c.climb_fpm)
        || !(1.0..=100.0).contains(&c.maximum_intercept_nm)
        || c.minimum_feet > c.maximum_feet
    {
        return Err("Invalid terminal decoder inputs".into());
    }
    let mut from = [start.latitude, start.longitude];
    let mut altitude = start.altitude_feet;
    let mut result: Vec<Fix> = Vec::with_capacity(legs.len());
    let mut indicated_speeds = Vec::with_capacity(legs.len());
    let mut incoming_track = None;
    let mut manual = None;
    for (index, leg) in legs.iter().enumerate() {
        if !matches!(
            leg.terminator.as_str(),
            "IF" | "TF" | "DF" | "CF" | "CA" | "FA" | "VA" | "VI" | "CI"
        ) {
            if !MANUAL_TERMINATORS.contains(&leg.terminator.as_str()) {
                return Err(format!(
                    "{} uses unsupported {} geometry",
                    leg.identifier, leg.terminator
                ));
            }
            // A manual transition publishes no flyable geometry. Keeping the computable
            // crossings and reporting the transition lets a caller continue only through
            // an explicit amendment; computable legs after it would be dropped silently.
            if index + 1 != legs.len() {
                return Err(format!(
                    "{} terminates {} before {} published legs complete",
                    leg.identifier,
                    leg.terminator,
                    legs.len() - index - 1
                ));
            }
            manual = Some(ManualTermination {
                terminator: leg.terminator.clone(),
                identifier: leg.identifier.clone(),
            });
            break;
        }
        let (minimum_feet, mut maximum_feet) = altitude_bounds(leg, c)?;
        if matches!(leg.terminator.as_str(), "CA" | "FA" | "VA") {
            maximum_feet = minimum_feet;
        }
        let preferred_feet = c.preferred_feet.clamp(minimum_feet, maximum_feet);
        let indicated = match leg.speed_knots {
            Some(speed) => match leg.speed_description.as_str() {
                "" | "-" => c.indicated_knots.min(speed),
                "+" => c.indicated_knots.max(speed),
                _ => {
                    return Err(format!(
                        "Unsupported speed descriptor at {}",
                        leg.identifier
                    ));
                }
            },
            None => c.indicated_knots,
        };
        let course = leg.course.map(|value| {
            (value
                + c.points
                    .course_variation(leg, &c.airport.icao)
                    .unwrap_or(c.variation))
            .rem_euclid(360.0)
        });
        let mut interception = None;
        let coordinate = if matches!(leg.terminator.as_str(), "VI" | "CI") {
            let prescribed = course.ok_or("Heading-intercept leg has no course")?;
            let heading = if leg.terminator == "VI" {
                air::heading_wind(
                    c.atmosphere.true_knots(indicated, preferred_feet)?,
                    prescribed,
                    c.wind_from,
                    c.wind_knots,
                )?
                .1
            } else {
                prescribed
            };
            let next = legs
                .get(index + 1)
                .filter(|next| next.terminator == "CF")
                .ok_or("Heading intercept needs a following course-to-fix leg")?;
            let fix = Points::runway(next, c.airport)
                .or_else(|| c.points.resolve(next, &c.airport.icao))
                .ok_or("Intercept course has an unresolved fix")?
                .coordinate;
            let track = (next.course.ok_or("Intercept target has no course")?
                + c.points
                    .course_variation(next, &c.airport.icao)
                    .unwrap_or(c.variation))
            .rem_euclid(360.0);
            interception = Some(super::trajectory::CourseIntercept {
                target: fix,
                target_course: track,
                course: heading,
                heading: (leg.terminator == "VI").then_some(prescribed),
                maximum_nm: c.maximum_intercept_nm,
            });
            intercept(from, heading, fix, track, c.maximum_intercept_nm)
                .map_err(|error| format!("{} to {}: {error}", leg.terminator, next.identifier))?
        } else if matches!(leg.terminator.as_str(), "CA" | "FA" | "VA") {
            let heading = course.ok_or("Altitude-terminated leg has no course")?;
            if leg.terminator == "FA" {
                let anchor = Points::runway(leg, c.airport)
                    .or_else(|| c.points.resolve(leg, &c.airport.icao))
                    .ok_or("Fix-to-altitude leg has an unresolved starting fix")?;
                if distance_nm(from, anchor.coordinate) >= 0.001 {
                    return Err("Fix-to-altitude leg does not start at its published fix".into());
                }
            }
            if leg.terminator == "VA"
                && result.last().is_some_and(|crossing| {
                    (crossing.minimum_feet - crossing.maximum_feet).abs() > f64::EPSILON
                })
            {
                return Err("Heading-to-altitude leg needs a fixed incoming altitude".into());
            }
            if minimum_feet <= altitude {
                continue;
            }
            // Nominal movement uses one ground speed per leg. Use the same
            // endpoint TAS as the trajectory solver when locating this CA end.
            let tas = c.atmosphere.true_knots(indicated, minimum_feet)?;
            let (speed, track) = if leg.terminator == "VA" {
                air::heading_wind(tas, heading, c.wind_from, c.wind_knots)?
            } else {
                (
                    air::wind(tas, heading, c.wind_from, c.wind_knots)?.0,
                    heading,
                )
            };
            let minutes = (minimum_feet - altitude) / c.climb_fpm;
            destination(from, track, speed * minutes / 60.0)
        } else {
            Points::runway(leg, c.airport)
                .or_else(|| c.points.resolve(leg, &c.airport.icao))
                .ok_or_else(|| format!("Unresolved terminal crossing {}", leg.identifier))?
                .coordinate
        };
        let distance = distance_nm(from, coordinate);
        let track = if distance < 0.001 {
            // A repeated fix has no bearing; retain the incoming path for wind conversion.
            incoming_track
                .or(course)
                .or_else(|| {
                    legs.get(index + 1).and_then(|next| {
                        Points::runway(next, c.airport)
                            .or_else(|| c.points.resolve(next, &c.airport.icao))
                            .filter(|point| distance_nm(coordinate, point.coordinate) >= 0.001)
                            .map(|point| bearing(coordinate, point.coordinate))
                    })
                })
                .ok_or("Initial terminal boundary has no usable course")?
        } else {
            bearing(from, coordinate)
        };
        let arrival_heading = if matches!(leg.terminator.as_str(), "VI" | "VA") {
            Some(track)
        } else if matches!(leg.terminator.as_str(), "CF" | "CA" | "FA" | "CI") {
            Some(course.ok_or("Course leg has no published course")?)
        } else if leg.fly_over() {
            Some(track)
        } else {
            None
        };
        let tas = c.atmosphere.true_knots(indicated, preferred_feet)?;
        let (ground_knots, _) = air::wind(tas, track, c.wind_from, c.wind_knots)?;
        let required_turn = match leg.turn.as_str() {
            "" => None,
            "L" => Some(Turn::Left),
            "R" => Some(Turn::Right),
            _ => return Err("Unknown published turn direction".into()),
        };
        if distance < 0.001 {
            if let Some(previous) = result.last_mut() {
                // A STAR/approach join can repeat a fix with a tighter crossing restriction.
                let low = previous.minimum_feet.max(minimum_feet);
                let high = previous.maximum_feet.min(maximum_feet);
                if low > high {
                    return Err(
                        "Coincident terminal crossings have conflicting altitude limits".into(),
                    );
                }
                if previous
                    .arrival_heading
                    .zip(arrival_heading)
                    .is_some_and(|(a, b)| crate::navigation::turn(a, b).abs() > 1.0)
                    || previous
                        .required_turn
                        .zip(required_turn)
                        .is_some_and(|(a, b)| a != b)
                {
                    return Err(
                        "Coincident terminal crossings have conflicting courses or turns".into(),
                    );
                }
                if (indicated - indicated_speeds.last().copied().unwrap_or(indicated)).abs() > 1e-6
                {
                    return Err("Coincident terminal crossing needs a joint speed solution".into());
                }
                previous.minimum_feet = low;
                previous.maximum_feet = high;
                previous.preferred_feet = preferred_feet.clamp(low, high);
                previous.ground_knots = air::wind(
                    c.atmosphere
                        .true_knots(indicated, previous.preferred_feet)?,
                    track,
                    c.wind_from,
                    c.wind_knots,
                )?
                .0;
                previous.arrival_heading = previous.arrival_heading.or(arrival_heading);
                previous.required_turn = previous.required_turn.or(required_turn);
                altitude = previous.preferred_feet;
            } else if retain_boundary {
                indicated_speeds.push(indicated);
                result.push(Fix {
                    intercept: interception,
                    airspeed: Some(super::trajectory::Airspeed {
                        indicated_knots: indicated,
                        atmosphere: c.atmosphere,
                        wind_from: c.wind_from,
                        wind_knots: c.wind_knots,
                    }),
                    coordinate,
                    arrival_heading,
                    minimum_feet,
                    maximum_feet,
                    preferred_feet,
                    ground_knots,
                    required_turn,
                });
                altitude = preferred_feet;
            } else if altitude < minimum_feet || altitude > maximum_feet {
                return Err("Initial fix restriction is not satisfied".into());
            }
            continue;
        }
        indicated_speeds.push(indicated);
        result.push(Fix {
            intercept: interception,
            airspeed: Some(super::trajectory::Airspeed {
                indicated_knots: indicated,
                atmosphere: c.atmosphere,
                wind_from: c.wind_from,
                wind_knots: c.wind_knots,
            }),
            coordinate,
            arrival_heading,
            minimum_feet,
            maximum_feet,
            preferred_feet,
            ground_knots,
            required_turn,
        });
        from = coordinate;
        incoming_track = Some(track);
        altitude = preferred_feet;
    }
    if result.is_empty() {
        return Err("Terminal route has no moving leg".into());
    }
    Ok((result, manual))
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    fn leg(terminator: &str, description: &str, altitude: &str) -> Leg {
        let mut fields = vec![""; 38];
        fields[0] = "010";
        fields[1] = "1";
        fields[2] = "TEST1";
        fields[3] = "RW04";
        fields[11] = terminator;
        fields[20] = "0420";
        fields[22] = description;
        fields[23] = altitude;
        super::super::procedures::Catalogue::parse(&format!("SID:{};", fields.join(",")))
            .unwrap()
            .0
            .into_values()
            .next()
            .unwrap()
            .remove(0)
    }
    #[test]
    fn manual_termination_keeps_published_geometry_and_names_the_amendment_point() {
        let airport = Airport {
            reference_latitude: 43.0,
            reference_longitude: 7.0,
            runways: vec![crate::airport::Runway {
                first_name: "04".into(),
                second_name: "22".into(),
                first: crate::state::Point {
                    east: 9260.0,
                    north: 9260.0,
                    height: 0.0,
                },
                second: crate::state::Point {
                    east: 9360.0,
                    north: 9260.0,
                    height: 0.0,
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        let points = Points::default();
        let atmosphere = Atmosphere::surface(1013.25, 15.0, 0.0).unwrap();
        let context = Context {
            airport: &airport,
            points: &points,
            atmosphere,
            variation: 0.0,
            wind_from: 90.0,
            wind_knots: 30.0,
            indicated_knots: 150.0,
            maximum_intercept_nm: 20.0,
            climb_fpm: 1000.0,
            minimum_feet: 0.0,
            maximum_feet: 20000.0,
            preferred_feet: 10000.0,
        };
        let start = Waypoint {
            latitude: 43.2,
            longitude: 7.2,
            altitude_feet: 0.0,
        };
        let mut published = leg("DF", "", "01000");
        published.identifier = "RW04".into();
        published.section = "P".into();
        published.subsection = "G".into();
        for terminator in MANUAL_TERMINATORS {
            let mut manual = leg(terminator, "", "");
            manual.identifier = "MOSBE".into();
            let legs = [published.clone(), manual.clone()];
            // The strict decoder still refuses: nothing authorises the manual transition.
            let refused = boundary_crossings(start, &legs, &context)
                .map(|_| ())
                .unwrap_err();
            assert!(
                refused.contains(&format!("MOSBE uses unsupported {terminator} geometry")),
                "{refused}"
            );
            let (fixes, termination) = amended_crossings(start, &legs, &context).unwrap();
            assert_eq!(fixes.len(), 1);
            assert!(
                (fixes[0].minimum_feet - atmosphere.geometric(1000.0, false).unwrap()).abs() < 1e-6
            );
            assert_eq!(
                termination,
                Some(ManualTermination {
                    terminator: terminator.into(),
                    identifier: "MOSBE".into(),
                })
            );
            // Computable legs after a manual transition would be dropped silently.
            let mut trailing = leg("DF", "", "01000");
            trailing.identifier = "RW04".into();
            trailing.section = "P".into();
            trailing.subsection = "G".into();
            assert!(
                amended_crossings(
                    start,
                    &[published.clone(), manual.clone(), trailing],
                    &context
                )
                .map(|_| ())
                .unwrap_err()
                .contains("before 1 published legs complete")
            );
            // A manual first leg leaves no published geometry to precede the amendment.
            assert!(
                amended_crossings(start, &[manual.clone()], &context)
                    .map(|_| ())
                    .unwrap_err()
                    .contains("no moving leg")
            );
            // Other unsupported terminators are still refused as unsupported geometry.
            let mut unknown = manual.clone();
            unknown.terminator = "ZZ".into();
            assert!(
                amended_crossings(start, &[published.clone(), unknown], &context)
                    .map(|_| ())
                    .unwrap_err()
                    .contains("unsupported ZZ geometry")
            );
        }
    }
    #[test]
    fn heading_to_altitude_keeps_wind_drift_and_the_published_altitude() {
        let airport = Airport::default();
        let points = Points::default();
        let atmosphere = Atmosphere::surface(1013.25, 15.0, 0.0).unwrap();
        let context = Context {
            airport: &airport,
            points: &points,
            atmosphere,
            variation: 0.0,
            wind_from: 90.0,
            wind_knots: 30.0,
            indicated_knots: 150.0,
            maximum_intercept_nm: 20.0,
            climb_fpm: 1000.0,
            minimum_feet: 0.0,
            maximum_feet: 20000.0,
            preferred_feet: 10000.0,
        };
        let mut heading = leg("VA", "+", "01000");
        heading.course = Some(0.0);
        let start = Waypoint {
            latitude: 43.0,
            longitude: 7.0,
            altitude_feet: 0.0,
        };
        let fixes = crossings(start, &[heading.clone()], &context).unwrap();
        assert_eq!(fixes.len(), 1);
        let altitude = atmosphere.geometric(1000.0, false).unwrap();
        assert_eq!(fixes[0].minimum_feet, altitude);
        assert_eq!(fixes[0].maximum_feet, altitude);
        let expected = air::heading_wind(
            atmosphere.true_knots(150.0, altitude).unwrap(),
            0.0,
            90.0,
            30.0,
        )
        .unwrap();
        assert!(
            crate::navigation::turn(
                bearing([start.latitude, start.longitude], fixes[0].coordinate),
                expected.1
            )
            .abs()
                < 1e-6
        );
        assert!((fixes[0].ground_knots - expected.0).abs() < 1e-6);
        assert!(
            (distance_nm([start.latitude, start.longitude], fixes[0].coordinate)
                - expected.0 * altitude / 1000.0 / 60.0)
                .abs()
                < 1e-6
        );
        let mut course = heading.clone();
        course.terminator = "CA".into();
        let held = crossings(start, &[course], &context).unwrap();
        assert!((held[0].coordinate[1] - start.longitude).abs() < 1e-8);
        assert!((fixes[0].coordinate[1] - start.longitude).abs() > 0.001);
        heading.course = None;
        assert!(crossings(start, &[heading], &context).is_err());
    }

    #[test]
    fn course_intercept_retains_the_published_ground_course_in_crosswind() {
        let airport = Airport {
            reference_latitude: 43.0,
            reference_longitude: 7.0,
            runways: vec![crate::airport::Runway {
                first_name: "04".into(),
                second_name: "22".into(),
                first: crate::state::Point {
                    east: 9260.0,
                    north: 9260.0,
                    height: 0.0,
                },
                second: crate::state::Point {
                    east: 9360.0,
                    north: 9260.0,
                    height: 0.0,
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        let points = Points::default();
        let context = Context {
            airport: &airport,
            points: &points,
            atmosphere: Atmosphere::surface(1013.25, 15.0, 0.0).unwrap(),
            variation: 0.0,
            wind_from: 90.0,
            wind_knots: 30.0,
            indicated_knots: 150.0,
            maximum_intercept_nm: 20.0,
            climb_fpm: 1000.0,
            minimum_feet: 0.0,
            maximum_feet: 2000.0,
            preferred_feet: 1000.0,
        };
        let mut incoming = leg("CI", "", "");
        incoming.course = Some(0.0);
        let mut next = leg("CF", "", "");
        next.course = Some(90.0);
        next.identifier = "RW04".into();
        next.section = "P".into();
        next.subsection = "G".into();
        let start = Waypoint {
            latitude: 43.0,
            longitude: 7.0,
            altitude_feet: 1000.0,
        };
        let decoded = crossings(start, &[incoming.clone(), next.clone()], &context).unwrap();
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].arrival_heading, Some(0.0));
        assert!((decoded[0].coordinate[1] - start.longitude).abs() < 1e-6);
        assert!(decoded[0].coordinate[0] > start.latitude);
        let tas = context
            .atmosphere
            .true_knots(150.0, decoded[0].preferred_feet)
            .unwrap();
        let expected = air::wind(tas, 0.0, 90.0, 30.0).unwrap();
        assert!(expected.1.abs() > 5.0);
        assert!((decoded[0].ground_knots - expected.0).abs() < 0.001);
        let mut heading_leg = incoming.clone();
        heading_leg.terminator = "VI".into();
        let heading_decoded = crossings(start, &[heading_leg, next.clone()], &context).unwrap();
        assert!(heading_decoded[0].coordinate[1] < start.longitude);
        assert_eq!(heading_decoded[0].intercept.unwrap().heading, Some(0.0));
        let envelope = super::super::trajectory::Envelope {
            bank_degrees: 20.0,
            turn_rate_degrees_sec: 3.0,
            climb_fpm: 1000.0,
            descent_fpm: 1000.0,
            spacing_meters: 150.0,
            maximum_detour_meters: 5000.0,
        };
        let solved =
            super::super::trajectory::draft(start, 0.0, &heading_decoded, 90.0, &envelope).unwrap();
        assert!(solved.clone().protect(|_, _| false).is_err());
        let path = solved.protect(|_, _| true).unwrap();
        let end = path[0].points.last().unwrap();
        let before = path[0].points.iter().rev().nth(1).unwrap();
        let track = bearing(
            [before.latitude, before.longitude],
            [end.latitude, end.longitude],
        );
        let tas = context
            .atmosphere
            .true_knots(150.0, end.altitude_feet)
            .unwrap();
        let (_, crab) = air::wind(tas, track, 90.0, 30.0).unwrap();
        assert!(crate::navigation::turn(0.0, track + crab).abs() < 0.1);
        assert!(end.longitude < start.longitude);
        assert!(crate::navigation::turn(0.0, track).abs() > 5.0);
        assert!(
            (path[0].ground_knots - air::heading_wind(tas, 0.0, 90.0, 30.0).unwrap().0).abs()
                < 0.02
        );
        assert!(crossings(start, &[incoming.clone()], &context).is_err());
        let mut parallel = next.clone();
        parallel.course = Some(0.0);
        assert!(crossings(start, &[incoming.clone(), parallel], &context).is_err());
        let mut behind = incoming.clone();
        behind.course = Some(180.0);
        assert!(crossings(start, &[behind, next.clone()], &context).is_err());
        next.terminator = "TF".into();
        assert!(crossings(start, &[incoming, next], &context).is_err());
    }

    #[test]
    fn repeated_join_fix_intersects_limits_instead_of_dropping_them() {
        let airport = Airport {
            reference_latitude: 43.0,
            reference_longitude: 7.0,
            runways: vec![crate::airport::Runway {
                first_name: "04".into(),
                second_name: "22".into(),
                first: crate::state::Point {
                    north: 10000.0,
                    ..Default::default()
                },
                second: crate::state::Point {
                    north: 12000.0,
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        let points = Points::default();
        let atmosphere = Atmosphere::surface(1013.25, 15.0, 0.0).unwrap();
        let context = Context {
            airport: &airport,
            points: &points,
            atmosphere,
            variation: 0.0,
            wind_from: 90.0,
            wind_knots: 20.0,
            indicated_knots: 150.0,
            maximum_intercept_nm: 40.0,
            climb_fpm: 1000.0,
            minimum_feet: 0.0,
            maximum_feet: 20000.0,
            preferred_feet: 10000.0,
        };
        let mut first = leg("TF", "+", "01000");
        first.identifier = "RW04".into();
        first.section = "P".into();
        first.subsection = "G".into();
        let mut second = first.clone();
        second.altitude_description = "-".into();
        second.altitude_one = Some(3000);
        let start = Waypoint {
            latitude: 43.0,
            longitude: 7.0,
            altitude_feet: 0.0,
        };
        let fixes = crossings(start, &[first.clone(), second.clone()], &context).unwrap();
        assert_eq!(fixes.len(), 1);
        assert_eq!(
            fixes[0].minimum_feet,
            atmosphere.geometric(1000.0, false).unwrap()
        );
        assert_eq!(
            fixes[0].maximum_feet,
            atmosphere.geometric(3000.0, false).unwrap()
        );
        assert!(fixes[0].preferred_feet <= fixes[0].maximum_feet);
        let expected = air::wind(
            atmosphere
                .true_knots(150.0, fixes[0].preferred_feet)
                .unwrap(),
            bearing([start.latitude, start.longitude], fixes[0].coordinate),
            context.wind_from,
            context.wind_knots,
        )
        .unwrap()
        .0;
        assert!((fixes[0].ground_knots - expected).abs() < 1e-6);
        second.altitude_one = Some(500);
        assert!(crossings(start, &[first.clone(), second], &context).is_err());

        // A planned join is constrained upstream; a live check-in must already meet it.
        let boundary = Waypoint {
            latitude: fixes[0].coordinate[0],
            longitude: fixes[0].coordinate[1],
            altitude_feet: 0.0,
        };
        assert!(crossings(boundary, &[first.clone()], &context).is_err());
        let retained = boundary_crossings(boundary, &[first.clone()], &context).unwrap();
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].minimum_feet, fixes[0].minimum_feet);

        let mut slower = first.clone();
        slower.speed_knots = Some(100.0);
        slower.speed_description = "-".into();
        assert!(
            boundary_crossings(boundary, &[first, slower], &context)
                .err()
                .unwrap()
                .contains("joint speed solution")
        );
    }
    #[test]
    fn fix_to_altitude_climbs_from_the_named_fix_without_relocating_the_aircraft() {
        let airport = Airport {
            reference_latitude: 43.0,
            reference_longitude: 7.0,
            runways: vec![crate::airport::Runway {
                first_name: "04".into(),
                second_name: "22".into(),
                second: crate::state::Point {
                    north: 3000.0,
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        let points = Points::default();
        let atmosphere = Atmosphere::surface(1013.25, 15.0, 0.0).unwrap();
        let context = Context {
            airport: &airport,
            points: &points,
            atmosphere,
            variation: 2.0,
            wind_from: 44.0,
            wind_knots: 10.0,
            indicated_knots: 140.0,
            maximum_intercept_nm: 20.0,
            climb_fpm: 1000.0,
            minimum_feet: 0.0,
            maximum_feet: 20000.0,
            preferred_feet: 10000.0,
        };
        let mut published = leg("FA", "+", "03000");
        published.identifier = "RW04".into();
        published.section = "P".into();
        published.subsection = "G".into();
        let anchor = Points::runway(&published, &airport).unwrap().coordinate;
        let start = Waypoint {
            latitude: anchor[0],
            longitude: anchor[1],
            altitude_feet: 1000.0,
        };
        let decoded = crossings(start, &[published.clone()], &context).unwrap();
        assert_eq!(decoded.len(), 1);
        let end = &decoded[0];
        assert_eq!(end.arrival_heading, Some(44.0));
        assert_eq!(
            end.minimum_feet,
            atmosphere.geometric(3000.0, false).unwrap()
        );
        assert_eq!(end.maximum_feet, end.minimum_feet);
        let seconds = distance_nm(anchor, end.coordinate) / end.ground_knots * 3600.0;
        assert!(
            ((end.preferred_feet - start.altitude_feet) * 60.0 / seconds - context.climb_fpm).abs()
                < 1e-6
        );
        let displaced = Waypoint {
            latitude: start.latitude + 0.01,
            ..start
        };
        assert!(crossings(displaced, &[published.clone()], &context).is_err());
        published.identifier = "UNKNOWN".into();
        assert!(crossings(start, &[published], &context).is_err());
    }

    #[test]
    fn mixed_pressure_bands_keep_their_physical_limits() {
        let airport = Airport::default();
        let points = Points::default();
        let atmosphere = Atmosphere::surface(1023.0, 25.0, 0.0).unwrap();
        let context = Context {
            airport: &airport,
            points: &points,
            atmosphere,
            variation: 2.0,
            wind_from: 44.0,
            wind_knots: 10.0,
            indicated_knots: 150.0,
            maximum_intercept_nm: 40.0,
            climb_fpm: 1000.0,
            minimum_feet: 0.0,
            maximum_feet: 30000.0,
            preferred_feet: 10000.0,
        };
        let mut facf = leg("IF", "J", "06000");
        facf.altitude_two = Some(4000);
        facf.altitude_two_is_flight_level = Some(true);
        let (low, high) = altitude_bounds(&facf, &context).unwrap();
        assert_eq!(low, atmosphere.geometric(6000.0, false).unwrap());
        assert_eq!(high, context.maximum_feet);
        facf.altitude_two_is_flight_level = None;
        assert!(altitude_bounds(&facf, &context).is_err());
        facf.altitude_two = None;
        assert!(altitude_bounds(&facf, &context).is_err());
        let mut crossing = leg("TF", "B", "FL120");
        crossing.altitude_two = Some(8000);
        crossing.altitude_two_is_flight_level = Some(false);
        let (low, high) = altitude_bounds(&crossing, &context).unwrap();
        assert_eq!(low, atmosphere.geometric(8000.0, false).unwrap());
        assert_eq!(high, atmosphere.geometric(12000.0, true).unwrap());
        let start = Waypoint {
            latitude: 43.0,
            longitude: 7.0,
            altitude_feet: 1000.0,
        };
        let climb = crossings(start, &[leg("CA", "+", "03000")], &context).unwrap();
        assert_eq!(climb.len(), 1);
        assert_eq!(climb[0].arrival_heading, Some(44.0));
        assert_eq!(climb[0].minimum_feet, climb[0].maximum_feet);
        let seconds = distance_nm([start.latitude, start.longitude], climb[0].coordinate)
            / climb[0].ground_knots
            * 3600.0;
        let rate = (climb[0].preferred_feet - start.altitude_feet) * 60.0 / seconds;
        assert!((rate - context.climb_fpm).abs() < 1e-6);

        assert!(
            climb[0].ground_knots
                < atmosphere
                    .true_knots(150.0, climb[0].preferred_feet)
                    .unwrap()
        );
        assert!(
            crossings(start, &[leg("RF", "", "")], &context)
                .err()
                .unwrap()
                .contains("unsupported RF")
        );
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod intercept_tests {
    #[test]
    fn intercept_joins_the_course_before_the_fix() {
        let origin = [44.0, 8.0];
        let fix =
            crate::arrival::destination(crate::arrival::destination(origin, 0.0, 5.0), 90.0, 5.0);
        let intersection = super::intercept(origin, 0.0, fix, 90.0, 20.0).unwrap();
        assert!((crate::arrival::distance_nm(origin, intersection) - 5.0).abs() < 0.02);
        assert!((crate::arrival::bearing(intersection, fix) - 90.0).abs() < 0.1);
        assert!(super::intercept(origin, 180.0, fix, 90.0, 20.0).is_err());
        assert!(super::intercept(origin, 90.0, fix, 90.0, 20.0).is_err());
        let far_fix =
            crate::arrival::destination(crate::arrival::destination(origin, 0.0, 5.0), 90.0, 30.0);
        let near = super::intercept(origin, 0.0, far_fix, 90.0, 20.0).unwrap();
        assert!(crate::arrival::distance_nm(origin, near) < 6.0);
        assert!(crate::arrival::distance_nm(near, far_fix) > 20.0);
        let long_intercept =
            crate::arrival::destination(crate::arrival::destination(origin, 0.0, 30.0), 90.0, 5.0);
        assert!(super::intercept(origin, 0.0, long_intercept, 90.0, 20.0).is_err());
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod installed_tests {
    use super::*;
    #[test]
    #[ignore = "requires installed licensed navigation data"]
    fn nice_departure_decodes_the_published_intercept() {
        let root = std::env::var("OPENATC_TEST_SIMULATOR_ROOT").unwrap();
        let airport = crate::airport::load_airport_from_simulator(
            &root,
            "LFMN",
            &|path| std::path::Path::new(path).is_file(),
            &|path| std::fs::read_to_string(path).map_err(|error| error.to_string()),
        )
        .unwrap();
        let catalogue =
            super::super::procedures::Catalogue::load(std::path::Path::new(&root), "LFMN").unwrap();
        let mut points = Points::requested(&[("LFMN", &catalogue)]);
        points.load(std::path::Path::new(&root)).unwrap();
        let legs = catalogue
            .departure("BASI8A", "04R", "")
            .unwrap()
            .into_iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        let runway = airport
            .runways
            .iter()
            .find(|r| r.first_name == "04R" || r.second_name == "04R")
            .unwrap();
        let point = if runway.first_name == "04R" {
            runway.first
        } else {
            runway.second
        };
        let start = Waypoint {
            latitude: airport.reference_latitude + point.north / 111_320.0,
            longitude: airport.reference_longitude
                + point.east / (111_320.0 * airport.reference_latitude.to_radians().cos()),
            altitude_feet: airport.elevation_feet + 35.0,
        };
        let context = Context {
            airport: &airport,
            points: &points,
            atmosphere: Atmosphere::surface(1023.0, 15.0, airport.elevation_feet).unwrap(),
            variation: 2.0,
            wind_from: 44.0,
            wind_knots: 5.0,
            indicated_knots: 180.0,
            maximum_intercept_nm: 40.0,
            climb_fpm: 2000.0,
            minimum_feet: 0.0,
            maximum_feet: 18000.0,
            preferred_feet: 5000.0,
        };
        let crossings = crossings(start, &legs, &context).unwrap();
        assert!(crossings.len() >= 5);
        assert!(
            crossings
                .iter()
                .any(|fix| fix.arrival_heading == Some(131.0))
        );
    }
}
