//! Ordered terminal legs. Fix names alone are not unique within an approach.
use std::collections::BTreeMap;

/// One CIFP procedure/transition, with the published route-type discriminator.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key {
    /// SID, STAR or APPCH.
    pub kind: String,
    /// Published procedure identifier.
    pub name: String,
    /// Runway, named transition or the common segment.
    pub transition: String,
    /// CIFP route type; approach transitions and the main approach stay distinct.
    pub route_type: String,
}

/// Facility reference used to interpret published courses.
#[derive(Clone, Debug, PartialEq)]
pub struct Reference {
    /// Recommended navaid identifier.
    pub identifier: String,
    /// ICAO region disambiguating the facility.
    pub region: String,
    /// Navaid record section.
    pub section: String,
    /// Navaid record subsection.
    pub subsection: String,
}

/// Published leg fields retained before resolving coordinates and flight geometry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Leg {
    /// Recommended facility used for a course or bearing, distinct from the terminating fix.
    pub recommended: Option<Reference>,
    /// Ordering within this route type and transition.
    pub sequence: u16,
    /// May be empty on altitude-terminated legs.
    pub identifier: String,
    /// ICAO region used to disambiguate fixes.
    pub region: String,
    /// Navaid/fix section code.
    pub section: String,
    /// Terminal/en-route subsection code.
    pub subsection: String,
    /// Preserve all four character positions, including the approach/missed markers.
    pub description: String,
    /// Published left/right turn indication.
    pub turn: String,
    /// ARINC path terminator, retained even when geometry is not supported yet.
    pub terminator: String,
    /// Degrees in the published reference; magnetic conversion is a separate step.
    pub course: Option<f64>,
    /// Raw distance/time code: a timed holding leg must not become a distance.
    pub distance_time: String,
    /// Published altitude condition; glide-slope codes are not ordinary ceilings.
    pub altitude_description: String,
    /// First altitude in feet, with flight levels decoded to their standard-pressure level.
    pub altitude_one: Option<i32>,
    /// Explicit pressure reference retained from an FL-prefixed CIFP altitude.
    pub altitude_one_is_flight_level: Option<bool>,
    /// Second altitude where the condition supplies an altitude band.
    pub altitude_two: Option<i32>,
    /// Reference of the second altitude, independently of the first.
    pub altitude_two_is_flight_level: Option<bool>,
    /// Published speed condition.
    pub speed_description: String,
    /// Indicated knots; conversion to movement speed is a separate planning step.
    pub speed_knots: Option<f64>,
    /// Signed published vertical path angle.
    pub vertical_angle_degrees: Option<f64>,
}

impl Leg {
    /// The second description column explicitly marks a fly-over waypoint.
    #[must_use]
    pub fn fly_over(&self) -> bool {
        self.description.as_bytes().get(1) == Some(&b'Y')
    }
    /// The third column marks the first leg after the missed-approach point.
    #[must_use]
    pub fn starts_missed(&self) -> bool {
        self.description.as_bytes().get(2) == Some(&b'M')
    }
    /// The fourth column marks the published missed-approach point itself.
    #[must_use]
    pub fn missed_point(&self) -> bool {
        self.description.as_bytes().get(3) == Some(&b'M')
    }
    /// Resolve ordinary crossing limits; unsupported descriptors cannot silently lose a restriction.
    /// # Panics
    ///
    /// Panics if a published altitude descriptor has an unsupported shape.
    pub fn altitude_limits(&self) -> Result<(Option<i32>, Option<i32>), String> {
        let first = self.altitude_one;
        let second = self.altitude_two;
        match self.altitude_description.as_str() {
            "" => Ok((first, first)),
            "+" if first.is_some() => Ok((first, None)),
            "-" if first.is_some() => Ok((None, first)),
            "B" if first.is_some() && second.is_some() => Ok((
                Some(first.unwrap().min(second.unwrap())),
                Some(first.unwrap().max(second.unwrap())),
            )),
            "H" | "J" if first.is_some() && first == second => Ok((first, first)),
            "H" if first.is_some() && second.is_some() && first >= second => Ok((second, first)),
            "J" if first.is_some() && second.is_some() => Ok((first, None)),
            other => Err(format!(
                "{}: altitude descriptor {other} needs a dedicated vertical-path decoder",
                self.identifier
            )),
        }
    }
}

/// Bounded airport catalogue, suitable for sharing between planning workers.
#[derive(Clone, Debug, Default)]
pub struct Catalogue(pub BTreeMap<Key, Vec<Leg>>);

/// Keep the landing path and its recovery separate instead of flattening repeated fixes.
pub struct Approach<'a> {
    /// Explicit selected entry transition, if any.
    pub transition: &'a [Leg],
    /// Main approach through the missed-approach point.
    pub landing: &'a [Leg],
    /// Published recovery starting after the missed-approach point.
    pub missed: &'a [Leg],
}

fn altitude(text: &str) -> Result<Option<i32>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let value = if let Some(level) = text.strip_prefix("FL") {
        level.parse::<i32>().map(|level| level * 100)
    } else {
        text.parse::<i32>()
    }
    .map_err(|_| format!("Invalid procedure altitude {text}"))?;
    if !(-2000..=60000).contains(&value) {
        return Err("Procedure altitude is outside supported limits".into());
    }
    Ok(Some(value))
}

fn number(text: &str, scale: f64, minimum: f64, maximum: f64) -> Result<Option<f64>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let value = text
        .parse::<f64>()
        .map_err(|_| format!("Invalid procedure value {text}"))?
        / scale;
    if !value.is_finite() || !(minimum..=maximum).contains(&value) {
        return Err(format!(
            "Procedure value {text} is outside supported limits"
        ));
    }
    Ok(Some(value))
}

impl Catalogue {
    /// Join runway, common and explicitly selected enroute parts without sorting their fixes together.
    pub fn departure(
        &self,
        name: &str,
        runway: &str,
        transition: &str,
    ) -> Result<Vec<&[Leg]>, String> {
        if name.is_empty() || runway.is_empty() {
            return Err("Departure needs a procedure and runway".into());
        }
        let exact = format!("RW{runway}");
        let shared = format!("RW{}B", runway.trim_end_matches(['L', 'R', 'C']));
        let routes: Vec<_> = self
            .0
            .iter()
            .filter(|(key, _)| key.kind == "SID" && key.name == name)
            .collect();
        let exact_routes: Vec<_> = routes
            .iter()
            .filter(|(key, _)| key.transition == exact)
            .collect();
        let runway_routes: Vec<_> = if exact_routes.is_empty() {
            routes
                .iter()
                .filter(|(key, _)| key.transition == shared)
                .collect()
        } else {
            exact_routes
        };
        if runway_routes.len() != 1 {
            return Err("Departure runway transition is missing or ambiguous".into());
        }
        let common: Vec<_> = routes
            .iter()
            .filter(|(key, _)| key.transition.is_empty())
            .collect();
        if common.len() > 1 {
            return Err("Departure common segment is ambiguous".into());
        }
        let mut result = vec![runway_routes[0].1.as_slice()];
        if let Some((_, legs)) = common.first() {
            result.push(legs.as_slice());
        }
        if !transition.is_empty() && !transition.starts_with("RW") {
            let exact: Vec<_> = routes
                .iter()
                .filter(|(key, _)| key.transition == transition)
                .collect();
            let by_terminal_fix: Vec<_> = routes
                .iter()
                .filter(|(_, legs)| {
                    legs.last()
                        .is_some_and(|leg| leg.identifier.eq_ignore_ascii_case(transition))
                })
                .collect();
            let selected = if exact.len() == 1 {
                Some(exact[0])
            } else if exact.is_empty() && by_terminal_fix.len() == 1 {
                // SimBrief and many charts name a SID by its last fix rather
                // than the CIFP transition name. Accept that when it is unique.
                Some(by_terminal_fix[0])
            } else {
                None
            };
            // A name that is not a published transition still leaves the
            // common route and runway transition; the procedure is not dropped.
            if let Some((_, legs)) = selected {
                result.push(legs.as_slice());
            }
        }
        if result.iter().any(|legs| legs.is_empty())
            || result.iter().map(|legs| legs.len()).sum::<usize>() > 512
        {
            return Err("Assembled departure exceeds its leg budget".into());
        }
        Ok(result)
    }
    /// Join an arrival's enroute, common and runway parts in flight order.
    pub fn arrival(
        &self,
        name: &str,
        runway: &str,
        transition: &str,
    ) -> Result<Vec<&[Leg]>, String> {
        if name.is_empty() || runway.is_empty() {
            return Err("Arrival needs a procedure, runway and optional enroute transition".into());
        }
        let routes: Vec<_> = self
            .0
            .iter()
            .filter(|(key, _)| key.kind == "STAR" && key.name == name)
            .collect();
        if routes.is_empty() {
            return Err("Arrival procedure is unavailable".into());
        }
        let exact = format!("RW{runway}");
        let shared = format!("RW{}B", runway.trim_end_matches(['L', 'R', 'C']));
        let mut runway_routes = Vec::new();
        for selected in [&exact, &shared, "ALL"] {
            runway_routes = routes
                .iter()
                .filter(|(key, _)| key.transition == *selected)
                .collect::<Vec<_>>();
            if !runway_routes.is_empty() {
                break;
            }
        }
        if runway_routes.len() > 1
            || (runway_routes.is_empty()
                && routes
                    .iter()
                    .any(|(key, _)| key.transition.starts_with("RW") || key.transition == "ALL"))
        {
            return Err("Arrival runway transition is missing or ambiguous".into());
        }
        let common: Vec<_> = routes
            .iter()
            .filter(|(key, _)| key.transition.is_empty())
            .collect();
        if common.len() > 1 {
            return Err("Arrival common segment is ambiguous".into());
        }
        let mut result = Vec::new();
        if !transition.is_empty() && !transition.starts_with("RW") && transition != "ALL" {
            let exact: Vec<_> = routes
                .iter()
                .filter(|(key, _)| key.transition == transition)
                .collect();
            let by_entry_fix: Vec<_> = routes
                .iter()
                .filter(|(_, legs)| {
                    legs.first()
                        .is_some_and(|leg| leg.identifier.eq_ignore_ascii_case(transition))
                })
                .collect();
            let selected = if exact.len() == 1 {
                Some(exact[0])
            } else if exact.is_empty() && by_entry_fix.len() == 1 {
                // A STAR is commonly named by its entry fix, which may only
                // appear inside a differently named CIFP transition.
                Some(by_entry_fix[0])
            } else {
                None
            };
            // An unrecognised transition falls back to the common route and
            // the runway transition; dropping the procedure is not acceptable.
            if let Some((_, legs)) = selected {
                result.push(legs.as_slice());
            }
        }
        if let Some((_, legs)) = common.first() {
            result.push(legs.as_slice());
        }
        if let Some((_, legs)) = runway_routes.first() {
            result.push(legs.as_slice());
        }
        if result.is_empty()
            || result.iter().any(|legs| legs.is_empty())
            || result.iter().map(|legs| legs.len()).sum::<usize>() > 512
        {
            return Err("Assembled arrival is empty or exceeds its leg budget".into());
        }
        Ok(result)
    }
    /// Assemble one named approach with an explicit transition and unambiguous recovery.
    pub fn approach(&self, name: &str, transition: &str) -> Result<Approach<'_>, String> {
        let routes: Vec<_> = self
            .0
            .iter()
            .filter(|(key, _)| key.kind == "APPCH" && key.name == name)
            .collect();
        let entry = if transition.is_empty() {
            &[][..]
        } else {
            let entries: Vec<_> = routes
                .iter()
                .filter(|(key, _)| key.route_type == "A" && key.transition == transition)
                .collect();
            if entries.len() != 1 {
                return Err("Approach transition is missing or ambiguous".into());
            }
            entries[0].1.as_slice()
        };
        let main: Vec<_> = routes
            .iter()
            .filter(|(key, _)| {
                key.transition.is_empty() && key.route_type != "A" && key.route_type != "Z"
            })
            .collect();
        if main.len() != 1 {
            return Err("Main approach is missing or ambiguous".into());
        }
        let main = main[0].1.as_slice();
        let marked: Vec<_> = main
            .iter()
            .enumerate()
            .filter(|(_, leg)| leg.starts_missed())
            .map(|(index, _)| index)
            .collect();
        let separate: Vec<_> = routes
            .iter()
            .filter(|(key, _)| key.route_type == "Z" && key.transition.is_empty())
            .collect();
        let (landing, missed) = if marked.len() == 1 && separate.is_empty() {
            main.split_at(marked[0])
        } else if marked.is_empty() && separate.len() == 1 {
            (main, separate[0].1.as_slice())
        } else {
            return Err("Missed approach is missing or ambiguous".into());
        };
        if landing.is_empty() || missed.is_empty() || !landing.iter().any(Leg::missed_point) {
            return Err("Approach has no explicit missed-approach point and recovery".into());
        }
        Ok(Approach {
            transition: entry,
            landing,
            missed,
        })
    }
    /// Read converted terminal data on a worker using the custom/default priority.
    pub fn load(root: &std::path::Path, airport: &str) -> Result<Self, String> {
        use std::io::Read;
        if !(3..=4).contains(&airport.len())
            || !airport
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        {
            return Err("Invalid terminal airport identifier".into());
        }
        if root.join("Custom Data/earth_424.dat").is_file()
            || root.join("Custom Data/FAACIFP18").is_file()
        {
            return Err("ARINC terminal overrides need their own decoder; converted CIFP cannot replace them".into());
        }
        let custom = root.join("Custom Data/CIFP").join(format!("{airport}.dat"));
        let path = if custom.is_file() {
            custom
        } else {
            root.join("Resources/default data/CIFP")
                .join(format!("{airport}.dat"))
        };
        let mut text = String::new();
        std::fs::File::open(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?
            .take(1024 * 1024 + 1)
            .read_to_string(&mut text)
            .map_err(|error| error.to_string())?;
        Self::parse(&text)
    }
    /// Read the simulator's converted CIFP format without modifying licensed data.
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > 1024 * 1024 {
            return Err("Terminal procedures exceed 1 MiB".into());
        }
        let mut result = Self::default();
        let mut count = 0;
        for line in text.lines() {
            let Some((kind, payload)) = line.split_once(':') else {
                continue;
            };
            if !matches!(kind, "SID" | "STAR" | "APPCH") {
                continue;
            }
            let fields: Vec<_> = payload
                .split(';')
                .next()
                .unwrap_or_default()
                .split(',')
                .collect();
            if fields.len() < 29 {
                return Err("Truncated terminal procedure leg".into());
            }
            let key = Key {
                kind: kind.into(),
                name: fields[2].trim().into(),
                transition: fields[3].trim().into(),
                route_type: fields[1].trim().into(),
            };
            if key.name.is_empty() || key.name.len() > 16 || key.transition.len() > 16 {
                return Err("Invalid terminal procedure identity".into());
            }
            count += 1;
            if count > 8192 || result.0.len() >= 1024 && !result.0.contains_key(&key) {
                return Err("Terminal procedure catalogue exceeds its leg budget".into());
            }
            let leg = Leg {
                recommended: (!fields[13].trim().is_empty()).then(|| Reference {
                    identifier: fields[13].trim().into(),
                    region: fields[14].trim().into(),
                    section: fields[15].trim().into(),
                    subsection: fields[16].trim().into(),
                }),
                sequence: fields[0]
                    .trim()
                    .parse()
                    .map_err(|_| "Invalid procedure sequence")?,
                identifier: fields[4].trim().into(),
                region: fields[5].trim().into(),
                section: fields[6].trim().into(),
                subsection: fields[7].trim().into(),
                description: fields[8].into(),
                turn: fields[9].trim().into(),
                terminator: fields[11].trim().into(),
                course: number(fields[20], 10.0, 0.0, 360.0)?,
                distance_time: fields[21].trim().into(),
                altitude_description: fields[22].trim().into(),
                altitude_one: altitude(fields[23])?,
                altitude_one_is_flight_level: (!fields[23].trim().is_empty())
                    .then(|| fields[23].trim().starts_with("FL")),
                altitude_two: altitude(fields[24])?,
                altitude_two_is_flight_level: (!fields[24].trim().is_empty())
                    .then(|| fields[24].trim().starts_with("FL")),
                speed_description: fields[26].trim().into(),
                speed_knots: number(fields[27], 1.0, 0.0, 600.0)?,
                vertical_angle_degrees: number(fields[28], 100.0, -15.0, 15.0)?,
            };
            let legs = result.0.entry(key).or_default();
            if legs.len() >= 512
                || legs
                    .iter()
                    .any(|previous| previous.sequence == leg.sequence)
            {
                return Err("Duplicate procedure sequence or transition exceeds 512 legs".into());
            }
            legs.push(leg);
        }
        for legs in result.0.values_mut() {
            legs.sort_by_key(|leg| leg.sequence);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    fn approach_leg(description: &str, one: i32, two: Option<i32>) -> Leg {
        Leg {
            altitude_description: description.into(),
            altitude_one: Some(one),
            altitude_two: two,
            identifier: "PEGBA".into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_glide_path_fix_is_a_band_from_the_glide_altitude_up_to_the_ceiling() {
        // YSSY PEGBA: 1610 ft is the 3 degree glide-path altitude at 5 NM, 3000 ft the ceiling.
        let leg = approach_leg("H", 3000, Some(1610));
        assert_eq!(leg.altitude_limits(), Ok((Some(1610), Some(3000))));
        let equal = approach_leg("H", 3000, Some(3000));
        assert_eq!(equal.altitude_limits(), Ok((Some(3000), Some(3000))));
        let inverted = approach_leg("H", 1610, Some(3000));
        assert!(inverted.altitude_limits().is_err());
    }

    #[test]
    fn a_glide_slope_intercept_fix_keeps_the_first_altitude_as_its_minimum() {
        let leg = approach_leg("J", 3000, Some(3000));
        assert_eq!(leg.altitude_limits(), Ok((Some(3000), Some(3000))));
        let band = approach_leg("J", 2000, Some(1610));
        assert_eq!(band.altitude_limits(), Ok((Some(2000), None)));
    }

    use super::*;
    #[test]
    fn arrival_uses_enroute_common_then_selected_runway() {
        let row = |kind, route, transition, identifier| {
            let mut fields = vec![""; 38];
            fields[0] = "010";
            fields[1] = route;
            fields[2] = "TEST1";
            fields[3] = transition;
            fields[4] = identifier;
            fields[5] = "LI";
            fields[6] = "E";
            fields[11] = "TF";
            format!("{kind}:{};", fields.join(","))
        };
        let text = [
            row("STAR", "1", "ENTRY", "FIRST"),
            row("STAR", "2", "", "COMMON"),
            row("STAR", "3", "RW35B", "SHARED"),
            row("STAR", "3", "RW35R", "EXACT"),
            row("SID", "3", "RW35R", "WRONG"),
        ]
        .join("\n");
        let catalogue = Catalogue::parse(&text).unwrap();
        let identifiers = |runway| {
            catalogue
                .arrival("TEST1", runway, "ENTRY")
                .unwrap()
                .iter()
                .map(|legs| legs[0].identifier.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(identifiers("35R"), ["FIRST", "COMMON", "EXACT"]);
        assert_eq!(identifiers("35L"), ["FIRST", "COMMON", "SHARED"]);
        assert!(catalogue.arrival("TEST1", "17L", "ENTRY").is_err());
        let unknown = catalogue.arrival("TEST1", "35R", "UNKNOWN").unwrap();
        assert_eq!(
            unknown
                .iter()
                .map(|legs| legs[0].identifier.clone())
                .collect::<Vec<_>>(),
            ["COMMON", "EXACT"],
            "An unpublished transition must keep the common and runway route"
        );
        let by_entry = catalogue.arrival("TEST1", "35R", "FIRST").unwrap();
        assert_eq!(
            by_entry
                .iter()
                .map(|legs| legs[0].identifier.clone())
                .collect::<Vec<_>>(),
            ["FIRST", "COMMON", "EXACT"],
            "A STAR named by its entry fix keeps its published transition"
        );
        let all = Catalogue::parse(&row("STAR", "5", "ALL", "ANY")).unwrap();
        assert_eq!(
            all.arrival("TEST1", "04L", "").unwrap()[0][0].identifier,
            "ANY"
        );
    }
    #[test]
    fn an_unpublished_sid_transition_still_keeps_the_runway_and_common_route() {
        let row = |route, transition, identifier| {
            let mut fields = vec![""; 38];
            fields[0] = "010";
            fields[1] = route;
            fields[2] = "TEST1";
            fields[3] = transition;
            fields[4] = identifier;
            fields[5] = "LI";
            fields[6] = "E";
            fields[11] = "TF";
            format!("SID:{};", fields.join(","))
        };
        let text = [
            row("2", "RW09", "RUNWAY"),
            row("3", "", "COMMON"),
            row("4", "NORTH", "EXITFX"),
        ]
        .join("\n");
        let catalogue = Catalogue::parse(&text).unwrap();
        let fallback = catalogue.departure("TEST1", "09", "VIPPA").unwrap();
        assert_eq!(
            fallback
                .iter()
                .map(|legs| legs[0].identifier.clone())
                .collect::<Vec<_>>(),
            ["RUNWAY", "COMMON"]
        );
        let by_terminal = catalogue.departure("TEST1", "09", "EXITFX").unwrap();
        assert_eq!(
            by_terminal
                .iter()
                .map(|legs| legs[0].identifier.clone())
                .collect::<Vec<_>>(),
            ["RUNWAY", "COMMON", "EXITFX"]
        );
    }

    #[test]
    fn repeated_missed_fixes_and_timed_holds_keep_their_sequence() {
        let row = |sequence, terminator, description, distance| {
            let mut fields = vec![""; 38];
            fields[0] = sequence;
            fields[1] = "I";
            fields[2] = "I35RZ";
            fields[4] = "SRN";
            fields[5] = "LI";
            fields[6] = "D";
            fields[8] = description;
            fields[9] = "R";
            fields[11] = terminator;
            fields[20] = "1330";
            fields[21] = distance;
            fields[22] = "+";
            fields[23] = "04000";
            fields[27] = "190";
            format!("APPCH:{};", fields.join(","))
        };
        let text = format!(
            "{}\n{}",
            row("080", "HM", "VE H", "T010"),
            row("070", "TF", "V   ", "")
        );
        let catalogue = Catalogue::parse(&text).unwrap();
        let legs = catalogue.0.values().next().unwrap();
        assert_eq!(legs.len(), 2);
        assert_eq!(legs[0].sequence, 70);
        assert_eq!(legs[1].sequence, 80);
        assert_eq!(legs[1].distance_time, "T010");
        assert_eq!(legs[0].description, "V   ");
        assert_eq!(legs[1].course, Some(133.0));
        assert_eq!(legs[1].altitude_limits().unwrap(), (Some(4000), None));
        assert!(
            Catalogue::parse(&format!("{text}\n{}", row("080", "HM", "VE H", "T010"))).is_err()
        );
    }
    #[test]
    #[ignore = "requires installed licensed navigation data"]
    fn installed_nice_and_milan_keep_full_procedure_legs() {
        let root = std::env::var("OPENATC_TEST_SIMULATOR_ROOT").unwrap();
        for airport in ["LFMN", "LIMC"] {
            let text =
                std::fs::read_to_string(format!("{root}/Custom Data/CIFP/{airport}.dat")).unwrap();
            let catalogue = Catalogue::parse(&text).unwrap();
            assert!(!catalogue.0.is_empty());
            if airport == "LIMC" {
                let approach = catalogue.approach("I35RZ", "INLER").unwrap();
                assert_eq!(approach.transition[0].identifier, "INLER");
                assert_eq!(approach.landing.last().unwrap().identifier, "RW35R");
                assert_eq!(approach.missed[0].identifier, "MC801");
                assert!(approach.missed[0].fly_over());
                assert_eq!(approach.missed.last().unwrap().terminator, "HM");
                let (_, legs) = catalogue
                    .0
                    .iter()
                    .find(|(key, _)| key.name == "I35RZ" && key.transition.is_empty())
                    .unwrap();
                assert!(
                    legs.iter()
                        .any(|leg| leg.terminator == "HM" && leg.identifier == "SRN")
                );
                assert!(legs.iter().any(|leg| leg.terminator == "CA"));
                let arrival = catalogue.arrival("DEVO3E", "35R", "").unwrap();
                assert_eq!(arrival[0][0].identifier, "DEVOX");
                assert!(catalogue.arrival("DEVO3E", "17L", "").is_err());
                let restriction = catalogue
                    .0
                    .iter()
                    .filter(|(key, _)| key.name == "DEVO3E")
                    .flat_map(|(_, legs)| legs)
                    .find(|leg| leg.identifier == "MC588")
                    .unwrap();
                assert_eq!(restriction.altitude_limits().unwrap(), (None, Some(12000)));
                assert_eq!(restriction.altitude_one_is_flight_level, Some(true));
            }
        }
    }
}
