//! Data-driven intent catalogue: `intents.toml` at the repo root.
//!
//! The catalogue describes Rust request buttons and parser/AI examples. The
//! deterministic parser and controller validate operational requests; metadata
//! or an example alone does not implement a clearance. Button order lives in UI.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

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
}

/// Whole catalogue, keyed by intent, alphabetical for determinism.
#[derive(Clone, Debug, Default)]
pub struct IntentCatalogue {
    entries: BTreeMap<String, IntentEntry>,
}

#[derive(Debug, Deserialize)]
struct CatalogueFile {
    #[serde(default)]
    intent: BTreeMap<String, IntentEntry>,
}

/// Load `intents.toml` from disk.
pub fn load_intents(path: &Path) -> Result<IntentCatalogue, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("cannot open file: {error}"))?;
    let file: CatalogueFile =
        toml::from_str(&text).map_err(|error| format!("cannot parse catalogue: {error}"))?;
    Ok(IntentCatalogue {
        entries: file.intent,
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
