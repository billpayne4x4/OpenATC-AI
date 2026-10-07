//! Speech library: the `speech/*.toml` role files as a tagged example pool.
//!
//! The pool feeds two consumers. The AI path selects few-shot examples by
//! role/situation/phase/variant, fills them with sample slots, and assembles
//! a prompt; the model answers with a transmission plus an optional effect
//! JSON line. The AI-off path renders the first canned `say` line directly.
//! Offer accept/decline pairs live on offer entries; accepting records an
//! expectation (see the compliance loop) that the pilot then flies.

use super::catalog::{Slots, render_template};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// One speech entry: `[group.name]` in a `speech/*.toml` file.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpeechEntry {
    /// Dotted id assigned at load (`offer.shortcut`).
    #[serde(skip)]
    pub id: String,
    /// Scope inferred from folders, never repeated in an entry.
    #[serde(skip)]
    pub region: String,
    /// ifr | vfr | shared, inferred from folders.
    #[serde(skip)]
    pub flight_rules: String,
    /// atc | copilot | ground | attendant.
    pub role: String,
    /// ATC services eligible to use these examples; empty supports legacy files.
    pub service: Vec<String>,
    /// Flight phases where this fits.
    pub phase: Vec<String>,
    /// icao | faa.
    pub variant: String,
    /// Retrieval keys for the prompt builder.
    pub situation: Vec<String>,
    /// Slot names referenced by the lines.
    pub slots: Vec<String>,
    /// Alternative phrasings; AI picks or blends, AI-off reads the first.
    pub say: Vec<String>,
    /// Pilot acceptance line (offers only).
    pub accept: String,
    /// Pilot decline line (offers only).
    pub decline: String,
    /// Plan change on accept: `amend_route` | `amend_altitude` | `amend_arrival` |
    /// `amend_destination` | `amend_speed` | `none` | `""` (no offer).
    pub effect: String,
}

/// Whole pool, keyed by dotted id, alphabetical for determinism.
#[derive(Clone, Debug, Default)]
pub struct SpeechPool {
    entries: BTreeMap<String, SpeechEntry>,
    profiles: BTreeMap<String, SpeechRegion>,
}

/// Editable regional routing. Longest airport ICAO prefix wins.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeechRegion {
    /// Folder name, e.g. us or australia.
    pub name: String,
    /// Airport ICAO prefixes, matched longest first.
    pub prefixes: Vec<String>,
    /// National phraseology baseline, icao or faa.
    pub variant: String,
    /// Human-readable scope and review limits.
    pub notes: String,
}

/// Placeholder vocabulary shared by validation, documentation and examples.
pub const SPEECH_SLOTS: &[&str] = &[
    "alt",
    "approach",
    "atis",
    "block",
    "callsign",
    "dest",
    "direction",
    "fluid",
    "freq",
    "fuel_quantity",
    "heading",
    "holding_point",
    "landing_distance",
    "level",
    "location",
    "mach",
    "mins",
    "nm",
    "qnh",
    "rate",
    "readability",
    "rwy",
    "sequence",
    "sid",
    "speed",
    "sq",
    "stand",
    "star",
    "station",
    "status",
    "surface",
    "time",
    "traffic",
    "turn",
    "via",
    "visibility",
    "weather",
    "wind",
    "wpt",
];

fn collect_files(dir: &Path, files: &mut Vec<std::path::PathBuf>) -> Result<(), String> {
    for file in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let file = file.map_err(|e| e.to_string())?;
        // Do not follow directory symlinks or escape the editable library.
        let kind = file.file_type().map_err(|e| e.to_string())?;
        if kind.is_dir() {
            collect_files(&file.path(), files)?;
        } else if kind.is_file() && file.path().extension().and_then(|s| s.to_str()) == Some("toml")
        {
            files.push(file.path());
        }
    }
    Ok(())
}

/// Validate one entry. Syntax errors and semantic metadata errors identify the entry.
pub fn validate_entry(entry: &SpeechEntry) -> Result<(), String> {
    let fail = |reason: &str| Err(format!("{}: {reason}", entry.id));
    if !["atc", "copilot", "ground", "attendant"].contains(&entry.role.as_str()) {
        return fail("role must be atc, copilot, ground or attendant");
    }
    if entry.service.iter().any(|s| {
        ![
            "Clearance",
            "Ground",
            "Tower",
            "Approach",
            "Departure",
            "ATIS",
            "Unicom",
        ]
        .contains(&s.as_str())
    }) {
        return fail("unknown ATC service");
    }
    if entry.role != "atc" && !entry.service.is_empty() {
        return fail("service applies only to ATC");
    }
    if !["icao", "faa", "common"].contains(&entry.variant.as_str()) {
        return fail("variant must be icao, faa or common");
    }
    if entry.phase.is_empty()
        || entry.phase.iter().any(|p| {
            ![
                "parked",
                "clearance",
                "pushback",
                "taxi",
                "departure",
                "cruise",
                "arrival",
                "approach",
                "landed",
                "taxi_in",
                "finished",
            ]
            .contains(&p.as_str())
        })
    {
        return fail("phase must contain supported flight phases");
    }
    if entry.situation.is_empty() || entry.situation.iter().any(|s| s.trim().is_empty()) {
        return fail("at least one non-empty situation tag is required");
    }
    if entry.say.is_empty() || entry.say.iter().any(|s| s.trim().is_empty()) {
        return fail("say must contain non-empty phrases");
    }
    if entry
        .say
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != entry.say.len()
    {
        return fail("duplicate phrase in say");
    }
    if entry
        .slots
        .iter()
        .any(|s| !SPEECH_SLOTS.contains(&s.as_str()))
    {
        return fail("unknown declared slot; see SPEECH_SLOTS");
    }
    let declared: std::collections::BTreeSet<_> = entry.slots.iter().map(String::as_str).collect();
    let mut used = std::collections::BTreeSet::new();
    let placeholders =
        regex::Regex::new(r"\{([a-z_]+)(?:\|[^{}]+)?\}").map_err(|e| e.to_string())?;
    for line in entry
        .say
        .iter()
        .map(String::as_str)
        .chain([entry.accept.as_str(), entry.decline.as_str()])
    {
        for found in placeholders.captures_iter(line) {
            let name = found
                .get(1)
                .ok_or_else(|| "invalid placeholder capture".to_owned())?
                .as_str();
            if !declared.contains(name) {
                return fail("placeholder is not declared in slots");
            }
            used.insert(name.to_owned());
        }
        if placeholders.replace_all(line, "").contains(['{', '}']) {
            return fail("malformed placeholder or literal brace");
        }
    }
    if used.len() != declared.len() || declared.len() != entry.slots.len() {
        return fail("slots must list each used placeholder exactly once");
    }
    let is_offer =
        !entry.accept.is_empty() || !entry.decline.is_empty() || !entry.effect.is_empty();
    if is_offer
        && (entry.accept.is_empty()
            || entry.decline.is_empty()
            || !(EFFECTS.contains(&entry.effect.as_str()) || entry.effect == "none"))
    {
        return fail("offers require accept, decline and a supported effect");
    }
    Ok(())
}

/// Recursively load regional and IFR/VFR folders; legacy flat files remain supported.
pub fn load_speech_dir(dir: &Path) -> Result<SpeechPool, String> {
    let mut pool = SpeechPool::default();
    let mut files = Vec::new();
    collect_files(dir, &mut files)?;
    files.sort();
    for path in &files {
        if path.file_name().and_then(|s| s.to_str()) != Some("profile.toml") {
            continue;
        }
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let profile: SpeechRegion =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let folder = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if profile.name != folder
            || !["icao", "faa"].contains(&profile.variant.as_str())
            || profile
                .prefixes
                .iter()
                .any(|p| p.is_empty() || !p.bytes().all(|c| c.is_ascii_uppercase()))
        {
            return Err(format!(
                "{}: invalid regional name, prefix or variant",
                path.display()
            ));
        }
        if pool
            .profiles
            .insert(profile.name.clone(), profile)
            .is_some()
        {
            return Err(format!("{}: duplicate region", path.display()));
        }
    }
    let mut prefixes = BTreeMap::new();
    for profile in pool.profiles.values() {
        for prefix in &profile.prefixes {
            if let Some(previous) = prefixes.insert(prefix, &profile.name) {
                return Err(format!(
                    "duplicate ICAO prefix {prefix} in {previous} and {}",
                    profile.name
                ));
            }
        }
    }
    let mut origins = BTreeMap::new();
    for path in files {
        if path.file_name().and_then(|s| s.to_str()) == Some("profile.toml")
            || path
                .strip_prefix(dir)
                .is_ok_and(|p| p.starts_with("runtime"))
        {
            continue;
        }
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let groups: BTreeMap<String, BTreeMap<String, SpeechEntry>> =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let components: Vec<_> = path
            .strip_prefix(dir)
            .map_err(|e| e.to_string())?
            .iter()
            .filter_map(|s| s.to_str())
            .collect();
        let (region, rules) = if components.len() == 1 || components.first() == Some(&"crew") {
            ("common", "shared")
        } else if components.len() == 3
            && pool.profiles.contains_key(components[0])
            && ["ifr", "vfr", "shared"].contains(&components[1])
        {
            (components[0], components[1])
        } else {
            return Err(format!(
                "{}: use region/ifr, region/vfr, region/shared or crew folders",
                path.display()
            ));
        };
        for (group, entries) in groups {
            for (name, mut entry) in entries {
                entry.id = format!("{group}.{name}");
                region.clone_into(&mut entry.region);
                rules.clone_into(&mut entry.flight_rules);
                if entry.role != "atc" && (region != "common" || rules != "shared") {
                    return Err(format!(
                        "{}: crew belongs in the global crew folder",
                        path.display()
                    ));
                }
                validate_entry(&entry).map_err(|e| format!("{}: {e}", path.display()))?;
                if let Some(previous) = origins.insert(entry.id.clone(), path.clone()) {
                    return Err(format!(
                        "duplicate speech ID {} in {} and {}",
                        entry.id,
                        previous.display(),
                        path.display()
                    ));
                }
                pool.entries.insert(entry.id.clone(), entry);
            }
        }
    }
    Ok(pool)
}

impl SpeechPool {
    /// Look up one entry by dotted id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&SpeechEntry> {
        self.entries.get(id)
    }

    /// Iterate `(id, entry)` alphabetically.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &SpeechEntry)> {
        self.entries.iter()
    }

    /// Number of entries in the pool.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the pool holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Select up to `limit` examples: role must match, variant must match, a
    /// listed phase must match (empty filter matches all), and at least one
    /// situation tag must match (empty filter matches all).
    #[must_use]
    pub fn select(
        &self,
        role: &str,
        situations: &[&str],
        phase: &str,
        variant: &str,
        limit: usize,
    ) -> Vec<&SpeechEntry> {
        self.select_scoped(role, situations, phase, variant, "common", "")
            .into_iter()
            .take(limit)
            .collect()
    }

    /// Return the matching regional profile using the longest ICAO prefix.
    #[must_use]
    pub fn region_for_airport(&self, airport: &str) -> Option<&SpeechRegion> {
        let airport = airport.trim().to_ascii_uppercase();
        self.profiles
            .values()
            .filter_map(|profile| {
                profile
                    .prefixes
                    .iter()
                    .filter(|prefix| airport.starts_with(prefix.as_str()))
                    .map(String::len)
                    .max()
                    .map(|length| (length, profile))
            })
            .max_by_key(|(length, _)| *length)
            .map(|(_, profile)| profile)
            .or_else(|| self.profiles.get("common"))
    }

    /// Select local examples first, then shared international examples. Unknown rules
    /// select only shared examples, so IFR material never leaks into VFR by default.
    #[must_use]
    pub fn select_scoped(
        &self,
        role: &str,
        situations: &[&str],
        phase: &str,
        variant: &str,
        region: &str,
        rules: &str,
    ) -> Vec<&SpeechEntry> {
        let phase = if phase == "taxiin" { "taxi_in" } else { phase };
        let mut entries: Vec<_> = self
            .entries
            .values()
            .filter(|entry| {
                entry.role == role
                    && (entry.variant == variant || entry.variant == "common")
                    && (entry.region.is_empty()
                        || entry.region == "common"
                        || entry.region == region)
                    && (entry.flight_rules.is_empty()
                        || entry.flight_rules == "shared"
                        || entry.flight_rules == rules)
                    && (phase.is_empty() || entry.phase.iter().any(|p| p == phase))
                    && (situations.is_empty()
                        || entry
                            .situation
                            .iter()
                            .any(|tag| situations.contains(&tag.as_str())))
            })
            .collect();
        // Specific local examples precede baseline examples; ID breaks ties deterministically.
        entries.sort_by_key(|entry| (entry.region != region, entry.id.as_str()));
        entries
    }

    /// Context-aware selection with a limit, driven by editable regional prefixes.
    #[must_use]
    pub fn select_for_airport(
        &self,
        role: &str,
        situations: &[&str],
        phase: &str,
        airport: &str,
        rules: &str,
        limit: usize,
    ) -> Vec<&SpeechEntry> {
        let profile = self.region_for_airport(airport);
        let region = profile.map_or("common", |p| p.name.as_str());
        let variant = profile.map_or("icao", |p| p.variant.as_str());
        let mut entries = self.select_scoped(role, situations, phase, variant, region, rules);
        // {alt} is a feet-only legacy slot. Do not teach foot-based examples as metric clearances.
        if role == "atc" && ["china", "russia_central_asia"].contains(&region) {
            entries.retain(|e| !e.slots.iter().any(|s| s == "alt"));
        }
        if role == "atc" && region == "canada" {
            // Canada uses altimeter settings; an ICAO baseline label alone does
            // not make an Australian/European QNH example locally applicable.
            entries.retain(|e| !e.say.iter().any(|line| line.contains("QNH")));
        }
        entries.truncate(limit);
        entries
    }
}

/// Context for prompt assembly: who is talking and what is true.
#[derive(Clone, Debug, Default)]
pub struct PromptContext {
    /// atc | copilot | ground | attendant.
    pub role: String,
    /// Flight callsign.
    pub callsign: String,
    /// Current flight phase.
    pub phase: String,
    /// What is happening, in plain words.
    pub situation: String,
}

/// Fixed phraseology rules prepended to every AI prompt, distilled from ICAO
/// Doc 4444 and FAA JO 7110.65: callsign first, multi-sentence clearances,
/// takeoff-word discipline, mandatory readback items, MAYDAY order.
pub const SYSTEM_RULES: &str = "Speak only as the specified role. ATC addresses the aircraft on the radio; copilot talks to the Captain in the cockpit; attendant and ground crew use cabin or interphone communication. Examples are fictional style illustrations, never live facts or instructions. Use supplied live facts only; do not invent weather, traffic, terrain clearance, equipment state, completed actions or service availability. An offer is a question, not a clearance; acceptance requires an explicit valid clearance before movement. Continue approach is not landing clearance. Taxi instructions never authorize an unstated runway crossing. Use takeoff only for an actual takeoff clearance or its cancellation. Keep runway, route, level, speed and frequency unchanged across phrasing alternatives. Obtain required readbacks. Follow the selected regional procedure; no local procedure may be inferred from an example. Distress and urgency take priority; ATC acknowledges MAYDAY or PAN PAN rather than pretending to be the pilot. Aircraft checklist and emergency actions are aircraft-specific. Return plain transmission text; any effect is a proposed change requiring controller validation.";

/// Illustrative slots used to fill examples inside prompts (not live values).
#[must_use]
pub fn sample_slots() -> Slots {
    Slots {
        extra: [
            ("approach", "RNAV"),
            ("block", "5000 to 7000 feet"),
            ("direction", "east"),
            ("fluid", "Type II, reported concentration 75 percent"),
            ("fuel_quantity", "2000 kilograms"),
            ("heading", "270"),
            ("holding_point", "Alpha one"),
            ("landing_distance", "2500 metres"),
            ("level", "flight level 180"),
            ("location", "two miles north of the airport"),
            ("mach", "decimal seven eight"),
            ("rate", "1000 feet per minute"),
            ("readability", "three"),
            ("sequence", "two"),
            ("speed", "180 knots"),
            ("star", "EXAMPLE ONE"),
            ("station", "Example Approach"),
            ("status", "assessment in progress"),
            ("surface", "wet"),
            ("time", "1230 UTC"),
            ("traffic", "a reported aircraft eastbound"),
            ("turn", "left"),
            ("visibility", "10 kilometres"),
            ("weather", "reported moderate turbulence"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect(),
        altitude_feet: Some(5000),
        waypoint: Some("PELIN".to_owned()),
        callsign: Some("VH-BIL".to_owned()),
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
        mins: Some("10".to_owned()),
        nm: Some("20".to_owned()),
    }
}

/// Assemble the AI prompt: rules, filled examples, then the live situation.
/// The model answers with a transmission, optionally followed by one JSON line
/// `{"effect":"..."}` when the transmission changes the plan.
#[must_use]
pub fn build_prompt(examples: &[&SpeechEntry], context: &PromptContext) -> String {
    let mut prompt = String::from(SYSTEM_RULES);
    prompt.push_str("\nRole: ");
    prompt.push_str(&context.role);
    prompt.push_str("\nExamples:\n");
    let samples = sample_slots();
    for example in examples {
        for line in &example.say {
            prompt.push_str("- ");
            prompt.push_str(&render_template(line, &samples));
            prompt.push('\n');
        }
    }
    prompt.push_str("Situation: ");
    prompt.push_str(&context.situation);
    prompt.push_str("\nPhase: ");
    prompt.push_str(&context.phase);
    prompt.push_str("\nCallsign: ");
    prompt.push_str(&context.callsign);
    prompt.push('\n');
    prompt
}

/// Plan effects the AI may return; anything else is ignored.
pub const EFFECTS: [&str; 5] = [
    "amend_route",
    "amend_altitude",
    "amend_arrival",
    "amend_destination",
    "amend_speed",
];

/// Split AI output into transmission text plus an optional plan effect. A
/// trailing line that parses as JSON with a known `effect` value is consumed;
/// everything else stays verbatim in the transmission.
#[must_use]
pub fn parse_ai_output(text: &str) -> (String, Option<String>) {
    let mut lines: Vec<&str> = text.lines().collect();
    if let Some(last) = lines.last()
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(last)
        && let Some(effect) = value.get("effect").and_then(serde_json::Value::as_str)
        && EFFECTS.contains(&effect)
    {
        lines.pop();
        return (lines.join("\n"), Some(effect.to_owned()));
    }
    (text.to_owned(), None)
}

/// AI-off fallback: render the first canned line with live slots.
#[must_use]
pub fn render_canned(entry: &SpeechEntry, slots: &Slots) -> String {
    entry
        .say
        .first()
        .and_then(|line| render_checked(line, slots).ok())
        .unwrap_or_default()
}

/// Render only when every referenced placeholder has a value. No invented defaults
/// are supplied by the caller; an incomplete operational instruction is rejected.
pub fn render_checked(template: &str, slots: &Slots) -> Result<String, String> {
    let placeholders =
        regex::Regex::new(r"\{([a-z_]+)(?:\|[^{}]+)?\}").map_err(|e| e.to_string())?;
    for found in placeholders.find_iter(template) {
        let value = render_template(found.as_str(), slots);
        if value.trim().is_empty() || value.contains(['{', '}']) {
            return Err(format!("missing context for {}", found.as_str()));
        }
    }
    let rendered = render_template(template, slots);
    if rendered.contains(['{', '}']) {
        return Err("malformed placeholder".to_owned());
    }
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, role: &str, situations: &[&str], phase: &str) -> SpeechEntry {
        SpeechEntry {
            id: id.to_owned(),
            role: role.to_owned(),
            phase: vec![phase.to_owned()],
            variant: "icao".to_owned(),
            situation: situations.iter().map(ToString::to_string).collect(),
            say: vec!["{callsign}, climb to {alt}.".to_owned()],
            ..Default::default()
        }
    }

    fn pool() -> SpeechPool {
        let mut pool = SpeechPool::default();
        for (id, entry) in [
            ("a", entry("a", "atc", &["spacing"], "arrival")),
            ("b", entry("b", "atc", &["weather"], "cruise")),
            ("c", entry("c", "copilot", &["spacing"], "arrival")),
        ] {
            pool.entries.insert(id.to_owned(), entry);
        }
        pool
    }

    #[test]
    fn selector_filters_role_situation_phase() {
        let pool = pool();
        let found = pool.select("atc", &["spacing"], "arrival", "icao", 10);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "a");
        assert!(
            pool.select("atc", &["spacing"], "cruise", "icao", 10)
                .is_empty()
        );
        assert_eq!(pool.select("atc", &[], "", "icao", 10).len(), 2);
        assert!(
            pool.select("atc", &["spacing"], "arrival", "faa", 10)
                .is_empty()
        );
        assert_eq!(pool.select("atc", &[], "", "icao", 1).len(), 1);
    }

    #[test]
    fn prompt_holds_rules_examples_situation() {
        let pool = pool();
        let context = PromptContext {
            role: "atc".to_owned(),
            callsign: "VH-BIL".to_owned(),
            phase: "arrival".to_owned(),
            situation: "sequence the arrival".to_owned(),
        };
        let examples = pool.select("atc", &["spacing"], "arrival", "icao", 4);
        let prompt = build_prompt(&examples, &context);
        assert!(prompt.contains("MAYDAY"));
        assert!(prompt.contains("VH-BIL, climb to 5000 feet."));
        assert!(prompt.contains("sequence the arrival"));
        assert!(prompt.contains("Role: atc"));
    }

    #[test]
    fn output_parser_splits_effect() {
        let (text, effect) =
            parse_ai_output("VH-BIL, cleared direct PELIN.\n{\"effect\":\"amend_route\"}");
        assert_eq!(text, "VH-BIL, cleared direct PELIN.");
        assert_eq!(effect, Some("amend_route".to_owned()));
        let (plain, none) = parse_ai_output("VH-BIL, roger.");
        assert_eq!(plain, "VH-BIL, roger.");
        assert_eq!(none, None);
        let (unknown, dropped) =
            parse_ai_output("VH-BIL, roger.\n{\"effect\":\"launch_missiles\"}");
        assert_eq!(unknown, "VH-BIL, roger.\n{\"effect\":\"launch_missiles\"}");
        assert_eq!(dropped, None);
    }

    #[test]
    fn canned_renders_first_line() {
        let slots = Slots {
            callsign: Some("VH-BIL".to_owned()),
            altitude_feet: Some(5000),
            ..Default::default()
        };
        assert_eq!(
            render_canned(&entry("a", "atc", &[], ""), &slots),
            "VH-BIL, climb to 5000 feet."
        );
        assert_eq!(render_canned(&SpeechEntry::default(), &slots), "");
    }
}
