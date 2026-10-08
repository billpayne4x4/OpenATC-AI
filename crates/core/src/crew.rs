//! Aircraft-profile crew controls and checklist exchanges.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
fn one() -> f64 {
    1.0
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
/// Allowed aircraft control and its simulator mapping.
pub struct Control {
    /// Name shown in the controls list.
    pub label: String,
    /// Crew member allowed to operate this control.
    pub role: String,
    /// Writable simulator dataref.
    pub dataref: String,
    /// Dataref used to confirm the resulting setting.
    pub readback_ref: String,
    /// Treat a positive readback value as on.
    pub readback_positive: bool,
    /// Array element, when the dataref is an array.
    pub index: Option<usize>,
    /// Simulator command used to switch on.
    pub on_command: String,
    /// Simulator command used to switch off.
    pub off_command: String,
    /// Lowest permitted target value.
    pub min: f64,
    /// Highest permitted target value.
    pub max: f64,
    /// Target value for an on request.
    pub on: f64,
    /// Target value for an off request.
    pub off: f64,
    /// Require the aircraft to be stationary on the ground.
    pub ground_only: bool,
    /// Require the aircraft to be airborne.
    pub airborne_only: bool,
    /// Allowed difference between the target and observed setting.
    pub tolerance: f64,
    #[serde(default = "one")]
    /// Multiply the requested value by this factor before writing.
    pub write_scale: f64,
    #[serde(default = "one")]
    /// Multiply the observed simulator value by this factor.
    pub readback_scale: f64,
    /// Named settings and their target values.
    pub states: BTreeMap<String, f64>,
    /// Require an integer target.
    pub integer: bool,
    /// One button press; no claim about the resulting system state.
    pub momentary: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
/// Checklist control, wording keys and expected value.
pub struct ChecklistItem {
    /// Identifier of the aircraft control.
    pub control: String,
    /// Speech TOML key for the item challenge.
    pub challenge: String,
    /// Speech TOML key for the expected response.
    pub response: String,
    /// Requested or expected control value.
    pub value: f64,
}
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
/// Aircraft-specific crew mappings and checklist order.
pub struct CrewProfile {
    /// Controls indexed by their mapping identifiers.
    pub controls: BTreeMap<String, Control>,
    /// Ordered items for each named checklist.
    pub checklists: BTreeMap<String, Vec<ChecklistItem>>,
    /// Communication-panel commands that select the cabin.
    pub cabin_commands: Vec<String>,
    /// Communication-panel commands that select ground crew.
    pub ground_commands: Vec<String>,
    /// Communication-panel commands that restore radio transmission.
    pub radio_commands: Vec<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
/// One bounded control request awaiting simulator confirmation.
pub struct Action {
    /// Identifier used to match simulator acknowledgments.
    pub sequence: u32,
    /// Aircraft identity for which the request was made.
    pub aircraft: String,
    /// Identifier of the aircraft control.
    pub control: String,
    /// Requested or expected control value.
    pub value: f64,
    /// Crew member allowed to operate this control.
    pub role: String,
    /// Rendered response to use after simulator confirmation.
    pub response: String,
}

/// Parse explicit control requests; model proposals use the same validation.
pub fn parse(profile: &CrewProfile, role: &str, text: &str) -> Option<(String, f64)> {
    let text = text.to_lowercase().replace("auto throttle", "autothrottle");
    if [
        "don\'t",
        "do not",
        "never",
        "not ",
        "can you confirm",
        "is ",
        "are ",
        "check ",
    ]
    .iter()
    .any(|phrase| text.starts_with(phrase))
    {
        return None;
    }
    let number = regex::Regex::new(r"-?\b\d+(?:\.\d+)?\b").ok()?;
    let mut candidates = profile
        .controls
        .iter()
        .filter(|(_, c)| c.role == role)
        .flat_map(|(id, c)| {
            crate::dialogue::phrases(&format!("crew_request_{id}"))
                .into_iter()
                .filter(|phrase| text.contains(&phrase.to_lowercase()))
                .map(move |phrase| (phrase.len(), id, c))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|c| (c.2.momentary, std::cmp::Reverse(c.0)));
    let (_, id, c) = candidates.first()?;
    let words = text
        .split(|c: char| !c.is_alphanumeric())
        .collect::<Vec<_>>();
    let mut value = if c.momentary {
        c.on
    } else if let Some((_, value)) = c
        .states
        .iter()
        .find(|(name, _)| words.contains(&name.as_str()))
    {
        *value
    } else if words
        .iter()
        .any(|w| ["off", "disarm", "disarmed", "remove", "disconnect"].contains(w))
    {
        c.off
    } else if let Some(n) = number.find(&text) {
        n.as_str().parse().ok()?
    } else if words.iter().any(|w| {
        [
            "on", "arm", "armed", "add", "connect", "engage", "engaged", "lower",
        ]
        .contains(w)
    }) {
        c.on
    } else {
        return None;
    };
    if id.as_str() == "altitude" {
        let level = regex::Regex::new(r"(?:flight level|fl)\s*(\d{2,3})").ok()?;
        if let Some(captures) = level.captures(&text) {
            value = captures[1].parse::<f64>().ok()? * 100.0;
        }
    }
    if id.as_str() == "heading" && value == 360.0 {
        value = 0.0;
    }
    validate(profile, role, id, value).then(|| ((*id).clone(), value))
}
/// Validate a finite target against the aircraft control and crew role.
#[must_use]
pub fn validate(profile: &CrewProfile, role: &str, id: &str, value: f64) -> bool {
    profile.controls.get(id).is_some_and(|c| {
        c.role == role
            && value.is_finite()
            && value >= c.min
            && value <= c.max
            && (!c.integer || value.fract() == 0.0)
            && (c.on_command.is_empty() || value == c.on || value == c.off)
    })
}

/// Straight pushback distance followed by a turn; positive angle means tail right.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
/// Straight distance and tail-relative turn.
pub struct PushbackRequest {
    /// Straight pushback distance in metres.
    pub distance_metres: f64,
    /// Turn in degrees; positive means tail right.
    pub turn_degrees: f64,
}
/// Parse metres and degrees in either order, with a 90-degree left/right default.
pub fn parse_pushback(text: &str) -> Option<PushbackRequest> {
    let text = text.to_lowercase();
    if !text.contains("pushback") && !text.contains("push back") {
        return None;
    }
    let metres = regex::Regex::new(r"(\d+(?:\.\d+)?)\s*(?:metres?|meters?|m)\b").ok()?;
    let degrees = regex::Regex::new(r"(\d+(?:\.\d+)?)\s*(?:degrees?|deg|%|percent)").ok()?;
    let distance = metres.captures(&text)?[1].parse::<f64>().ok()?;
    let left = text.contains("left");
    let right = text.contains("right");
    if left && right {
        return None;
    }
    let angle = degrees
        .captures(&text)
        .and_then(|c| c[1].parse::<f64>().ok())
        .unwrap_or(if left || right { 90.0 } else { 0.0 });
    if !(1.0..=200.0).contains(&distance)
        || !(0.0..=180.0).contains(&angle)
        || angle > 0.0 && !left && !right
    {
        return None;
    }
    Some(PushbackRequest {
        distance_metres: distance,
        turn_degrees: if left { -angle } else { angle },
    })
}

/// Configured control and its live simulator availability.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct Capability {
    /// Aircraft mapping identifier.
    pub id: String,
    /// Display name.
    pub label: String,
    /// Crew member permitted to operate it.
    pub role: String,
    /// The simulator exposes the required writable ref or command.
    pub available: bool,
    /// Last observed setting, when readable.
    pub value: Option<f64>,
}

/// Built-in simulator controls for aircraft without a third-party override.
#[must_use]
pub fn standard_profile() -> CrewProfile {
    use std::sync::OnceLock;
    static PROFILE: OnceLock<CrewProfile> = OnceLock::new();
    PROFILE.get_or_init(build_standard_profile).clone()
}
fn build_standard_profile() -> CrewProfile {
    let mut profile = CrewProfile::default();
    for (id, label, dataref, min, max, on, off) in [
        (
            "heading",
            "Heading",
            "sim/cockpit/autopilot/heading_mag",
            0.0,
            359.0,
            0.0,
            0.0,
        ),
        (
            "altitude",
            "Altitude",
            "sim/cockpit/autopilot/altitude",
            100.0,
            60000.0,
            100.0,
            0.0,
        ),
        (
            "speed",
            "Speed",
            "sim/cockpit/autopilot/airspeed",
            40.0,
            450.0,
            100.0,
            0.0,
        ),
        (
            "vertical_speed",
            "Vertical speed",
            "sim/cockpit2/autopilot/vvi_dial_fpm",
            -6000.0,
            6000.0,
            0.0,
            0.0,
        ),
        (
            "autopilot",
            "Autopilot selector",
            "sim/cockpit/autopilot/autopilot_mode",
            0.0,
            2.0,
            2.0,
            0.0,
        ),
        (
            "autothrottle",
            "Autothrottle selector",
            "sim/cockpit2/autopilot/autothrottle_enabled",
            -1.0,
            3.0,
            1.0,
            -1.0,
        ),
        (
            "gear",
            "Landing gear lever",
            "sim/cockpit2/controls/gear_handle_down",
            0.0,
            1.0,
            1.0,
            0.0,
        ),
        (
            "flaps",
            "Flap lever percent",
            "sim/cockpit2/controls/flap_ratio",
            0.0,
            100.0,
            100.0,
            0.0,
        ),
        (
            "speedbrakes",
            "Speed brakes",
            "sim/cockpit2/controls/speedbrake_ratio",
            -0.5,
            1.0,
            -0.5,
            0.0,
        ),
        (
            "parking_brake",
            "Parking brake",
            "sim/cockpit2/controls/parking_brake_ratio",
            0.0,
            1.0,
            1.0,
            0.0,
        ),
        (
            "beacon",
            "Beacon",
            "sim/cockpit2/switches/beacon_on",
            0.0,
            1.0,
            1.0,
            0.0,
        ),
        (
            "seatbelts",
            "Seat belt signs",
            "sim/cockpit2/switches/fasten_seat_belts",
            0.0,
            1.0,
            1.0,
            0.0,
        ),
        (
            "wing_lights",
            "Wing lights",
            "sim/cockpit2/switches/wing_lights_on",
            0.0,
            1.0,
            1.0,
            0.0,
        ),
        (
            "turnoff_lights",
            "Runway turnoff lights",
            "sim/cockpit2/switches/runway_turnoff_lights_on",
            0.0,
            1.0,
            1.0,
            0.0,
        ),
        (
            "external_power_switch",
            "External power switch",
            "sim/cockpit2/electrical/GPU_generator_on",
            0.0,
            1.0,
            1.0,
            0.0,
        ),
    ] {
        let mut c = Control {
            label: label.into(),
            role: "copilot".into(),
            dataref: dataref.into(),
            min,
            max,
            on,
            off,
            write_scale: 1.0,
            readback_scale: 1.0,
            tolerance: 0.01,
            ..Default::default()
        };
        c.integer = (min == 0.0 && max <= 2.0) || id == "autothrottle";
        if id == "flaps" {
            c.write_scale = 0.01;
            c.readback_scale = 100.0;
            c.states.insert("up".into(), 0.0);
            c.states.insert("full".into(), 100.0);
        }
        if id == "gear" {
            c.states.insert("up".into(), 0.0);
            c.states.insert("down".into(), 1.0);
        }
        if id == "autopilot" {
            c.airborne_only = true;
            c.states.insert("director".into(), 1.0);
        }
        if id == "autothrottle" {
            c.states.insert("armed".into(), 0.0);
        }
        if ["parking_brake", "external_power_switch"].contains(&id) {
            c.ground_only = true;
        }
        profile.controls.insert(id.into(), c);
    }
    for line in include_str!("../../../assets/controls/standard-commands.tsv")
        .lines()
        .filter(|line| !line.starts_with('#'))
    {
        let Some((command, label)) = line.split_once('\t') else {
            continue;
        };
        let id = format!("stock_{}", command.replace('/', "_"));
        profile.controls.insert(
            id,
            Control {
                label: label.into(),
                role: "copilot".into(),
                on_command: command.into(),
                momentary: true,
                airborne_only: command.contains("landing_gear") && !command.ends_with("_down"),
                min: 1.0,
                max: 1.0,
                on: 1.0,
                integer: true,
                write_scale: 1.0,
                readback_scale: 1.0,
                ..Default::default()
            },
        );
    }
    profile
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pushback_orders_preserve_metres_and_tail_direction() {
        assert_eq!(
            parse_pushback("pushback 30 meters to the right")
                .unwrap()
                .turn_degrees,
            90.0
        );
        assert_eq!(
            parse_pushback("pushback 120% to the right 80 meters").unwrap(),
            PushbackRequest {
                distance_metres: 80.0,
                turn_degrees: 120.0
            }
        );
        assert_eq!(
            parse_pushback("pushback 50 metres 60 degrees to the left")
                .unwrap()
                .turn_degrees,
            -60.0
        );
        assert!(parse_pushback("pushback 300 metres to the right").is_none());
        assert!(parse_pushback("pushback 80 metres 60 degrees").is_none());
    }
    #[test]
    fn profile_commands_are_bounded_and_role_specific() {
        let profile = crate::profiles::load_aircraft_profile(std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../aircraft/toliss_a320.toml"
        )))
        .unwrap()
        .crew;
        assert_eq!(
            parse(&profile, "copilot", "set heading 270"),
            Some(("heading".into(), 270.0))
        );
        assert!(parse(&profile, "cabin", "set heading 270").is_none());
        assert!(parse(&profile, "copilot", "set heading 999").is_none());
        assert_eq!(
            parse(&profile, "cabin", "cabin brightness 50 percent"),
            Some(("cabin_brightness".into(), 50.0))
        );
        assert_eq!(
            parse(&profile, "ground", "remove chocks"),
            Some(("chocks".into(), 0.0))
        );
        assert_eq!(
            parse(&profile, "copilot", "gear up"),
            Some(("gear".into(), 0.0))
        );
    }
}
