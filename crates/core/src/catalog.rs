//! Data-driven intent catalogue: `intents.toml` at the repo root.
//!
//! The catalogue describes Rust request buttons and parser/AI examples. The
//! deterministic parser and controller validate operational requests; metadata
//! or an example alone does not implement a clearance. Button order is editable.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{OnceLock, RwLock};

static ACTIVE: OnceLock<RwLock<IntentCatalogue>> = OnceLock::new();
fn active() -> &'static RwLock<IntentCatalogue> {
    ACTIVE.get_or_init(|| {
        let file =
            crate::configuration::from_str::<CatalogueFile>(include_str!("../../../intents.toml"))
                .expect("bundled intents");
        RwLock::new(IntentCatalogue {
            entries: file.intent,
            order: file.buttons.order,
            parser: file.parser,
        })
    })
}

/// Editable request-button order and labels; eligibility remains controller checked.
#[must_use]
pub fn buttons() -> Vec<(String, String)> {
    quick_buttons()
        .into_iter()
        .filter(|button| button.crew.is_empty())
        .map(|button| (button.intent, button.title))
        .collect()
}

/// Quick requests for every catalogue entry, in the editable order.
#[must_use]
pub fn quick_buttons() -> Vec<QuickButton> {
    let Ok(catalogue) = active().read() else {
        return Vec::new();
    };
    catalogue
        .order
        .iter()
        .filter_map(|id| {
            catalogue
                .entries
                .get(id)
                .filter(|entry| entry.catalogue)
                .map(|entry| QuickButton {
                    intent: id.clone(),
                    title: entry.title.clone(),
                    crew: entry.crew.clone(),
                    say: if entry.say.is_empty() {
                        entry.title.clone()
                    } else {
                        entry.say.clone()
                    },
                    phase: entry.phase.clone(),
                    control: entry.control.clone(),
                    aircraft: entry.aircraft.clone(),
                    when: entry.when.clone(),
                    actions: entry.actions.clone(),
                })
        })
        .collect()
}

/// Quick requests for one crew tab, in catalogue order.
#[must_use]
pub fn crew_buttons(role: &str) -> Vec<QuickButton> {
    quick_buttons()
        .into_iter()
        .filter(|button| button.crew == role)
        .collect()
}

/// Load an editable catalogue without changing controller authorization.
pub fn activate(path: &Path) -> Result<(), String> {
    let overrides = load_intents(path)?;
    let defaults =
        crate::configuration::from_str::<CatalogueFile>(include_str!("../../../intents.toml"))
            .map_err(|error| error.to_string())?;
    let mut catalogue = IntentCatalogue {
        entries: defaults.intent,
        order: defaults.buttons.order,
        parser: defaults.parser,
    };
    catalogue.entries.extend(overrides.entries);
    if !overrides.order.is_empty() {
        catalogue.order = overrides.order;
    }
    if !overrides.parser.altitude.is_empty() {
        catalogue.parser = overrides.parser;
    }
    if catalogue.len() > 128
        || catalogue.iter().any(|(id, entry)| {
            id.len() > 64 || entry.exact.len() > 64 || entry.substring.len() > 64
        })
    {
        return Err("Intent catalogue exceeds supported limits".into());
    }
    catalogue.parser.validate()?;
    if catalogue
        .parser
        .amendments
        .iter()
        .any(|entry| !catalogue.entries.contains_key(&entry.intent))
    {
        return Err("Parsing grammar refers to an unknown intent".into());
    }
    if catalogue.order.len() > 128
        || catalogue
            .order
            .iter()
            .any(|id| !catalogue.entries.contains_key(id))
    {
        return Err("Button order contains an unknown intent or exceeds supported limits".into());
    }
    if catalogue.iter().any(|(_, entry)| {
        !matches!(entry.crew.as_str(), "" | "copilot" | "ground" | "attendant")
            || entry.say.len() > 160
            || entry.control.len() > 64
            || entry.aircraft.len() > 8
            || entry.aircraft.iter().any(|tag| {
                tag.is_empty()
                    || tag.len() > 8
                    || !tag.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
            || entry.when.as_ref().is_some_and(|when| {
                when.control.len() > 64
                    || !matches!(when.is.as_str(), "on" | "off" | "available" | "unavailable")
            })
            || entry.actions.len() > 24
            || entry.actions.iter().any(|action| {
                action.control.len() > 64
                    || action.control.is_empty()
                    || action.value.len() > 16
                    || (action.value != "on"
                        && action.value != "off"
                        && action.value.parse::<f64>().is_err())
            })
            || entry
                .phase
                .iter()
                .any(|key| !PHASES.contains(&key.as_str()))
            || entry.phase.len() > PHASES.len()
            || (entry.phase.len() > 1 && entry.phase.iter().any(|key| key == "all"))
    }) {
        return Err("Quick button needs a known crew role, a short phrase and known phases".into());
    }
    *active()
        .write()
        .map_err(|_| "Intent catalogue unavailable")? = catalogue;
    Ok(())
}

/// Longest matching alias wins; configured emergency triggers take precedence.
#[must_use]
pub fn matching_intent(text: &str) -> Option<String> {
    let catalogue = active().read().ok()?;
    catalogue
        .iter()
        .filter(|(_, entry)| !entry.quick_only)
        .flat_map(|(id, entry)| {
            entry
                .exact
                .iter()
                .chain(entry.substring.iter())
                .chain(std::iter::once(&entry.title))
                .filter(move |phrase| !phrase.is_empty() && text.contains(&phrase.to_lowercase()))
                .map(move |phrase| (id == "emergency", phrase.len(), id))
        })
        .max_by_key(|(emergency, length, _)| (*emergency, *length))
        .map(|(_, _, id)| id.clone())
}

/// One worked example: template text plus optional expected slots.
/// `kind` is request (pilot, must stay exactly parseable), say (controller
/// transmission or callsign-bearing pilot call, multi-sentence allowed) or
/// readback (pilot readback of a clearance). `variant` is icao or faa.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct IntentExample {
    /// Template text with `{...}` placeholders.
    pub text: String,
    /// request | say | readback.
    pub kind: String,
    /// icao | faa.
    pub variant: String,
    /// Expected `altitude_feet` for request examples carrying one.
    pub altitude_feet: Option<i32>,
    /// Expected `waypoint` for request examples carrying one.
    pub waypoint: Option<String>,
}

/// Request definition and its eligibility policy.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct IntentEntry {
    /// UI button title.
    pub title: String,
    /// Ground | Communication | Information | Enroute | Arrival | Emergency | Conversation.
    pub group: String,
    /// True for UI button intents; false for parser-only intents.
    pub catalogue: bool,
    /// True when the UI opens the altitude/waypoint modal.
    pub parameter: bool,
    /// Crew roles allowed: atc, copilot, ground, attendant.
    pub roles: Vec<String>,
    /// Extra exact-match phrases beyond the lowercased title.
    pub exact: Vec<String>,
    /// Substring triggers (emergency scan only).
    pub substring: Vec<String>,
    /// exact | substring | altitude | direct | none.
    pub pattern: String,
    /// Slot names this intent fills.
    pub slots: Vec<String>,
    /// Worked examples: request kinds stay exactly parseable; say/readback
    /// kinds are speech material for the AI and the AI-off path.
    pub examples: Vec<IntentExample>,
    /// AI-off canned reply; "" means no canned reply (discipline/AI path owns it).
    pub fallback: String,
    /// Crew tab for a quick request: empty for ATC, else copilot, ground or attendant.
    pub crew: String,
    /// Phrase sent when the quick button is pressed; the title when empty.
    pub say: String,
    /// Phases where the quick button is shown: `all` alone, or phase keys.
    pub phase: Vec<String>,
    /// Aircraft control the request operates, when it is a control request.
    pub control: String,
    /// Aircraft types this button applies to; empty means every aircraft.
    pub aircraft: Vec<String>,
    /// Show the button only when this control condition holds.
    pub when: Option<When>,
    /// Controls set by this button, in order. Empty sends the spoken phrase.
    pub actions: Vec<ButtonAction>,
    /// True for crew-page buttons that the radio parser must ignore.
    pub quick_only: bool,
}

/// Visibility condition for a quick button.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct When {
    /// Profile control to inspect.
    pub control: String,
    /// on | off | available | unavailable.
    pub is: String,
}

/// One aircraft control set by a quick button.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ButtonAction {
    /// Profile control identifier.
    pub control: String,
    /// on | off | numeric target.
    pub value: String,
}

/// Phase keys accepted by quick-button catalogues.
pub const PHASES: [&str; 12] = [
    "all",
    "parked_out",
    "pushback",
    "taxi_out",
    "takeoff",
    "climb",
    "cruise",
    "descend",
    "approach",
    "landing",
    "taxi_in",
    "parked_in",
];

/// Whether a quick button is shown in the current phase. An empty list means all.
#[must_use]
pub fn phase_allowed(phase: &[String], current: &str) -> bool {
    phase.is_empty() || phase.iter().any(|key| key == "all" || key == current)
}

/// One quick request button, for ATC or a crew role.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QuickButton {
    /// Intent key.
    pub intent: String,
    /// Button title.
    pub title: String,
    /// Crew tab: empty for ATC, else copilot, ground or attendant.
    pub crew: String,
    /// Phrase sent when pressed.
    pub say: String,
    /// Phases where the button is shown.
    pub phase: Vec<String>,
    /// Aircraft control required for the button, when any.
    pub control: String,
    /// Aircraft types this button applies to; empty means every aircraft.
    pub aircraft: Vec<String>,
    /// Visibility condition.
    pub when: Option<When>,
    /// Controls set by the button.
    pub actions: Vec<ButtonAction>,
}

/// Whole catalogue, keyed by intent, alphabetical for determinism.
#[derive(Clone, Debug, Default)]
pub struct IntentCatalogue {
    entries: BTreeMap<String, IntentEntry>,
    order: Vec<String>,
    parser: Parser,
}

#[derive(Default, Debug, Deserialize)]
struct Buttons {
    order: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CatalogueFile {
    #[serde(default)]
    intent: BTreeMap<String, IntentEntry>,
    #[serde(default)]
    buttons: Buttons,
    #[serde(default)]
    parser: Parser,
}

#[derive(Clone, Default, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
/// Editable slot-recognition grammar, separate from controller authorization.
pub struct Parser {
    /// Flight-level marker and numeric altitude captures.
    pub altitude: String,
    /// Requested waypoint capture.
    pub direct: String,
    /// Words marking a descent request.
    pub descent_markers: Vec<String>,
    /// Words marking metric altitude units.
    pub metric_markers: Vec<String>,
    /// Additional slot patterns, applied in file order.
    pub amendments: Vec<Amendment>,
}
#[derive(Clone, Default, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
/// One bounded slot pattern associated with a controller intent.
pub struct Amendment {
    /// Supported intent identifier.
    pub intent: String,
    /// Value capture, plus a direction capture for deviations.
    pub expression: String,
}

impl Parser {
    fn validate(&self) -> Result<(), String> {
        if self.amendments.len() > 32
            || self.descent_markers.len() > 32
            || self.metric_markers.len() > 32
        {
            return Err("Parsing grammar exceeds supported limits".into());
        }
        for (pattern, captures) in std::iter::once((&self.altitude, 3))
            .chain(std::iter::once((&self.direct, 2)))
            .chain(self.amendments.iter().map(|entry| {
                (
                    &entry.expression,
                    if entry.intent == "deviation" { 3 } else { 2 },
                )
            }))
        {
            if pattern.len() > 4096 {
                return Err("Parsing expression exceeds 4096 bytes".into());
            }
            let expression = regex::RegexBuilder::new(pattern)
                .size_limit(1024 * 1024)
                .build()
                .map_err(|error| error.to_string())?;
            if expression.captures_len() < captures || expression.is_match("") {
                return Err(
                    "Parsing expression needs its value captures and cannot match empty input"
                        .into(),
                );
            }
        }
        Ok(())
    }
}

/// Current parsing patterns; older files retain bundled patterns.
#[must_use]
pub fn parser() -> Parser {
    active()
        .read()
        .map(|catalogue| catalogue.parser.clone())
        .unwrap_or_default()
}

/// Load `intents.toml` from disk.
pub fn load_intents(path: &Path) -> Result<IntentCatalogue, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("cannot open file: {error}"))?;
    let file: CatalogueFile = crate::configuration::from_str(&text)
        .map_err(|error| format!("cannot parse catalogue: {error}"))?;
    Ok(IntentCatalogue {
        entries: file.intent,
        order: file.buttons.order,
        parser: file.parser,
    })
}

impl IntentCatalogue {
    /// Look up one intent by key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&IntentEntry> {
        self.entries.get(key)
    }

    /// Iterate `(key, entry)` alphabetically.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &IntentEntry)> {
        self.entries.iter()
    }

    /// Number of intents in the catalogue.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the catalogue holds no intents.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Slot values for template rendering, filled voice-implicit from the parse.
#[derive(Clone, Debug, Default)]
pub struct Slots {
    /// Altitude feet, if the request carried one.
    pub altitude_feet: Option<i32>,
    /// Waypoint, if the request carried one.
    pub waypoint: Option<String>,
    /// Callsign context, if known.
    pub callsign: Option<String>,
    /// Destination ICAO.
    pub dest: Option<String>,
    /// Runway designator.
    pub runway: Option<String>,
    /// Transponder code.
    pub squawk: Option<String>,
    /// Pressure setting as spoken ("1018", "2992").
    pub qnh: Option<String>,
    /// Departure/arrival procedure name.
    pub procedure: Option<String>,
    /// Frequency as spoken ("118 decimal 1").
    pub frequency: Option<String>,
    /// Taxi route.
    pub via: Option<String>,
    /// Stand/gate designator.
    pub stand: Option<String>,
    /// Wind as spoken ("270 degrees 8 knots").
    pub wind: Option<String>,
    /// ATIS letter.
    pub atis: Option<String>,
    /// Minutes, renders "10 minutes".
    pub mins: Option<String>,
    /// Additional unit-bearing context supplied by the caller. No automatic units.
    pub extra: BTreeMap<String, String>,
    /// Miles, renders "20 miles".
    pub nm: Option<String>,
}

/// Render `{alt}` / `{dest}` / `{rwy}` / `{sq}` / `{qnh}` / `{sid}` /
/// `{freq}` / `{via}` / `{stand}` / `{wind}` / `{atis}` / `{wpt}` /
/// `{callsign}` from slots.
///
/// `{alt}` renders "5000 feet" or "" when empty; `{alt|3000}` falls back to
/// "3000 feet" (a non-integer default renders ""). String slots render
/// literally or "". Unknown placeholders and unterminated braces pass
/// through untouched so template bugs stay visible.
#[must_use]
pub fn render_template(template: &str, slots: &Slots) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        rest = &rest[start + 1..];
        if let Some(end) = rest.find('}') {
            out.push_str(&render_placeholder(&rest[..end], slots));
            rest = &rest[end + 1..];
        } else {
            out.push('{');
            out.push_str(rest);
            return out;
        }
    }
    out.push_str(rest);
    out
}

/// Render one `{...}` expression.
fn render_placeholder(expression: &str, slots: &Slots) -> String {
    let (name, default) = match expression.split_once('|') {
        Some((name, default)) => (name, Some(default)),
        None => (expression, None),
    };
    match name {
        "alt" => match slots.altitude_feet {
            Some(feet) => format!("{feet} feet"),
            None => match default.and_then(|text| text.parse::<i32>().ok()) {
                Some(fallback) => format!("{fallback} feet"),
                None => String::new(),
            },
        },
        "wpt" => slots.waypoint.clone().unwrap_or_default(),
        "callsign" => slots.callsign.clone().unwrap_or_default(),
        "dest" => slots.dest.clone().unwrap_or_default(),
        "rwy" => slots.runway.clone().unwrap_or_default(),
        "sq" => slots.squawk.clone().unwrap_or_default(),
        "qnh" => slots.qnh.clone().unwrap_or_default(),
        "sid" => slots.procedure.clone().unwrap_or_default(),
        "freq" => slots.frequency.clone().unwrap_or_default(),
        "via" => slots.via.clone().unwrap_or_default(),
        "stand" => slots.stand.clone().unwrap_or_default(),
        "wind" => slots.wind.clone().unwrap_or_default(),
        "atis" => slots.atis.clone().unwrap_or_default(),
        "mins" => slots
            .mins
            .as_ref()
            .map_or_else(String::new, |mins| format!("{mins} minutes")),
        "nm" => slots
            .nm
            .as_ref()
            .map_or_else(String::new, |nm| format!("{nm} miles")),
        _ => slots
            .extra
            .get(name)
            .cloned()
            .unwrap_or_else(|| format!("{{{expression}}}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editable_slot_grammar_is_bounded_and_required_captures_are_validated() {
        let file = toml::from_str::<CatalogueFile>(include_str!("../../../intents.toml")).unwrap();
        file.parser.validate().unwrap();
        let mut broken = file.parser.clone();
        broken.direct = "(.*)".into();
        assert!(broken.validate().is_err());
        broken.direct = "[".into();
        assert!(broken.validate().is_err());
        for (id, entry) in &file.intent {
            assert!(
                entry.phase.iter().all(|key| PHASES.contains(&key.as_str())),
                "{id} has an unknown phase"
            );
            if entry.quick_only {
                assert!(!entry.crew.is_empty(), "{id} needs a crew tab");
            }
        }
        assert_eq!(buttons().len(), 35);
    }

    #[test]
    fn crew_buttons_come_from_the_same_catalogue_and_respect_phases() {
        let ground = crew_buttons("ground");
        let chocks = ground
            .iter()
            .find(|button| button.intent == "crew_chocks_on")
            .expect("add chocks");
        assert_eq!(chocks.actions.len(), 1);
        assert_eq!(chocks.actions[0].value, "on");
        assert_eq!(
            chocks.when.as_ref().map(|when| when.is.as_str()),
            Some("off")
        );
        assert!(
            ground
                .iter()
                .all(|button| button.crew == "ground" && !button.control.is_empty())
        );
        let prepare = crew_buttons("copilot")
            .into_iter()
            .find(|button| button.intent == "crew_prepare_a320")
            .expect("prepare A320");
        assert_eq!(prepare.aircraft, vec!["A20N".to_owned()]);
        assert!(prepare.actions.len() > 8);
        assert!(
            buttons()
                .iter()
                .all(|(intent, _)| !intent.starts_with("crew_"))
        );
        let parked = vec!["parked_out".to_owned(), "pushback".to_owned()];
        assert!(phase_allowed(&parked, "pushback"));
        assert!(!phase_allowed(&parked, "cruise"));
        let passengers = crew_buttons("attendant")
            .into_iter()
            .find(|button| button.intent == "crew_passengers")
            .expect("passenger request");
        assert_eq!(passengers.phase, vec!["all".to_owned()]);
        assert!(phase_allowed(&passengers.phase, "parked_out"));
        assert!(phase_allowed(&passengers.phase, "cruise"));
    }

    #[test]
    fn renderer_fills_placeholders() {
        let slots = Slots {
            altitude_feet: Some(5000),
            waypoint: Some("PELIN".to_owned()),
            callsign: Some("VH-BIL".to_owned()),
            ..Default::default()
        };
        assert_eq!(
            render_template("climb to {alt}", &slots),
            "climb to 5000 feet"
        );
        assert_eq!(
            render_template("direct {wpt} {callsign}", &slots),
            "direct PELIN VH-BIL"
        );
        assert_eq!(render_template("no slots here", &slots), "no slots here");
        assert_eq!(
            render_template("keep {mystery} intact", &slots),
            "keep {mystery} intact"
        );
        assert_eq!(render_template("cut {short", &slots), "cut {short");
        let empty = Slots::default();
        assert_eq!(render_template("climb to {alt}", &empty), "climb to ");
        assert_eq!(
            render_template("descend to {alt|3000}", &empty),
            "descend to 3000 feet"
        );
        assert_eq!(
            render_template("descend to {alt|low}", &empty),
            "descend to "
        );
        assert_eq!(
            render_template("descend to {alt|4500}", &slots),
            "descend to 5000 feet"
        );
        let full = Slots {
            dest: Some("YMML".to_owned()),
            runway: Some("27".to_owned()),
            squawk: Some("2201".to_owned()),
            qnh: Some("1018".to_owned()),
            procedure: Some("WOL 1".to_owned()),
            frequency: Some("118 decimal 1".to_owned()),
            via: Some("Alpha".to_owned()),
            stand: Some("24".to_owned()),
            wind: Some("270 degrees 8 knots".to_owned()),
            atis: Some("Bravo".to_owned()),
            ..Default::default()
        };
        assert_eq!(
            render_template(
                "{callsign} cleared to {dest} via {sid} runway {rwy}, squawk {sq}. QNH {qnh}, information {atis}.",
                &full
            ),
            " cleared to YMML via WOL 1 runway 27, squawk 2201. QNH 1018, information Bravo."
        );
        assert_eq!(
            render_template(
                "contact {freq}, taxi via {via} to stand {stand}, wind {wind}.",
                &full
            ),
            "contact 118 decimal 1, taxi via Alpha to stand 24, wind 270 degrees 8 knots."
        );
        let timed = Slots {
            mins: Some("10".to_owned()),
            nm: Some("20".to_owned()),
            ..Default::default()
        };
        assert_eq!(
            render_template("saves {nm}, expect {mins} delay.", &timed),
            "saves 20 miles, expect 10 minutes delay."
        );
        assert_eq!(render_template("saves {nm}.", &Slots::default()), "saves .");
    }
}
