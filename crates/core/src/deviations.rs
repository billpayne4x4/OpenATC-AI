//! Deviation policy, rule metadata and the built-in detection rules.
//!
//! Rules return findings; the engine manager owns warning counts, radio calls,
//! diversions and plan changes.

use crate::airspace::AirspaceIndex;
use crate::navigation;
use crate::state::{FlightPlan, PhaseCode, TaxiClearance, Telemetry};
use serde::Deserialize;
use std::collections::BTreeMap;

/// Shipped policy used when the player has not provided `deviations.toml`.
pub const DEFAULT_POLICY: &str = include_str!("../../../deviations.toml");

/// Rule identifiers accepted in `deviations.toml`.
pub const KNOWN_RULES: [&str; 3] = ["hold_short_passed", "airspace", "off_plan"];

/// How strictly deviations are treated.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Mode {
    /// No deviation monitoring.
    Off,
    /// Wide tolerances and a long warning sequence.
    Loose,
    /// Standard tolerances and three warnings.
    #[default]
    Realistic,
    /// Tight tolerances and immediate diversion for serious findings.
    Strict,
}

impl Mode {
    /// Parse the TOML value. Unknown names use the realistic mode.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Self::Off,
            "loose" => Self::Loose,
            "strict" => Self::Strict,
            _ => Self::Realistic,
        }
    }
}

/// Whether a deviation applies on the ground or in flight.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scope {
    /// Taxi and runway holding.
    Ground,
    /// Airborne route and airspace.
    Flight,
}

/// How a finding is treated by the warning sequence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Severity {
    /// Informational; no warning sequence.
    Advisory,
    /// Counts as one strike.
    Correction,
    /// Counts as extra strikes or diverts immediately in strict mode.
    Serious,
}

/// One rule result.
#[derive(Clone, Debug)]
pub struct Finding {
    /// Stable rule identifier.
    pub rule: &'static str,
    /// Ground or flight.
    pub scope: Scope,
    /// Finding severity.
    pub severity: Severity,
    /// Response template identifier.
    pub phrase: &'static str,
    /// Template slots supplied by the rule.
    pub facts: BTreeMap<String, String>,
}

impl Finding {
    /// Whether this finding is serious.
    #[must_use]
    pub fn serious(&self) -> bool {
        self.severity == Severity::Serious
    }
}

/// Policy values after the mode and regional overlays are applied.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Warnings before a ground diversion.
    pub ground_warnings: u8,
    /// Warnings before a flight diversion.
    pub flight_warnings: u8,
    /// Seconds between radio calls for the same rule.
    pub cooldown_seconds: u64,
    /// Flight-plan cross-track tolerance in nautical miles.
    pub off_plan_tolerance_nm: f64,
    /// Serious findings divert immediately instead of adding strikes.
    pub serious_immediate: bool,
    /// Runway entry reaches the warning limit in one event.
    pub runway_entry_immediate: bool,
    /// Strikes added by a serious finding.
    pub serious_strikes: u8,
    /// Airspace classes treated as hard restrictions.
    pub airspace_classes: Vec<String>,
    /// Allow the diversion consequence.
    pub diversion_enabled: bool,
    /// Maximum search range for a diversion airport.
    pub diversion_range_nm: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ground_warnings: 3,
            flight_warnings: 3,
            cooldown_seconds: 15,
            off_plan_tolerance_nm: 2.0,
            serious_immediate: false,
            runway_entry_immediate: true,
            serious_strikes: 2,
            airspace_classes: vec!["P".to_owned(), "R".to_owned()],
            diversion_enabled: true,
            diversion_range_nm: 200.0,
        }
    }
}

/// Optional "number for you to call" exchange.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Number {
    /// Enable the exchange.
    pub enabled: bool,
    /// Digits given to the pilot.
    pub number: String,
}

/// Effective policy for one region.
#[derive(Clone, Debug)]
pub struct Policy {
    /// Selected mode.
    pub mode: Mode,
    /// Mode and regional settings.
    pub settings: Settings,
    /// Rule identifiers that are enabled.
    pub rules: BTreeMap<String, bool>,
    /// Optional number exchange.
    pub number: Number,
}

impl Policy {
    /// Load the policy and overlay one region.
    ///
    /// # Errors
    ///
    /// Invalid TOML, an unknown rule name or a malformed mode table.
    pub fn load(text: &str, region: &str) -> Result<Self, String> {
        let value: toml::Value =
            toml::from_str(text).map_err(|error| format!("invalid deviation policy: {error}"))?;
        let mode = value
            .get("policy")
            .and_then(|policy| policy.get("mode"))
            .and_then(toml::Value::as_str)
            .map_or(Mode::Realistic, Mode::parse);
        let mut overlay = toml::Table::new();
        if let Some(table) = value
            .get("modes")
            .and_then(|modes| modes.get(mode_key(mode)))
            .and_then(toml::Value::as_table)
        {
            merge(&mut overlay, table);
        }
        if let Some(table) = value
            .get("regions")
            .and_then(|regions| regions.get(region))
            .and_then(toml::Value::as_table)
        {
            merge(&mut overlay, table);
        }
        let settings: Settings = toml::Value::Table(overlay)
            .try_into()
            .map_err(|error| format!("invalid deviation settings: {error}"))?;
        let mut rules = BTreeMap::new();
        if let Some(table) = value.get("rules").and_then(toml::Value::as_table) {
            for (name, enabled) in table {
                if !KNOWN_RULES.contains(&name.as_str()) {
                    return Err(format!("unknown deviation rule: {name}"));
                }
                let enabled = enabled
                    .as_bool()
                    .ok_or_else(|| format!("deviation rule {name} must be true or false"))?;
                rules.insert(name.clone(), enabled);
            }
        }
        let number = value
            .get("number_for_you")
            .cloned()
            .map(|value| value.try_into())
            .transpose()
            .map_err(|error| format!("invalid number_for_you: {error}"))?
            .unwrap_or_default();
        Ok(Self {
            mode,
            settings,
            rules,
            number,
        })
    }

    /// Whether a rule is enabled. Missing entries default to enabled.
    #[must_use]
    pub fn rule(&self, rule: &str) -> bool {
        self.rules.get(rule).copied().unwrap_or(true)
    }

    /// Warning limit for a scope.
    #[must_use]
    pub fn limit(&self, scope: Scope) -> u8 {
        match scope {
            Scope::Ground => self.settings.ground_warnings.max(1),
            Scope::Flight => self.settings.flight_warnings.max(1),
        }
    }

    /// Supplemental airspace areas from the policy file.
    ///
    /// # Errors
    ///
    /// Malformed `[[airspace]]` entries.
    pub fn airspace(text: &str) -> Result<Vec<crate::airspace::Area>, String> {
        let value: toml::Value =
            toml::from_str(text).map_err(|error| format!("invalid deviation policy: {error}"))?;
        let areas = value
            .get("airspace")
            .cloned()
            .map(|value| value.try_into())
            .transpose()
            .map_err(|error| format!("invalid supplemental airspace: {error}"))?;
        Ok(areas.unwrap_or_default())
    }
}

fn mode_key(mode: Mode) -> &'static str {
    match mode {
        Mode::Off => "off",
        Mode::Loose => "loose",
        Mode::Realistic => "realistic",
        Mode::Strict => "strict",
    }
}

fn merge(base: &mut toml::Table, overlay: &toml::Table) {
    for (key, value) in overlay {
        if let (Some(toml::Value::Table(existing)), toml::Value::Table(overlay)) =
            (base.get_mut(key), value)
        {
            merge(existing, overlay);
        } else {
            base.insert(key.clone(), value.clone());
        }
    }
}

/// Inputs shared by the built-in rules.
pub struct DeviationContext<'a> {
    /// Current simulator sample.
    pub telemetry: &'a Telemetry,
    /// Filed flight plan.
    pub plan: &'a FlightPlan,
    /// Active taxi clearance.
    pub taxi: &'a TaxiClearance,
    /// Loaded local airport.
    pub airport: Option<&'a crate::airport::Airport>,
    /// Parsed X-Plane and supplemental airspace.
    pub airspace: Option<&'a AirspaceIndex>,
    /// Current flight phase.
    pub phase: PhaseCode,
    /// Next plan fix index.
    pub next_fix: usize,
    /// An assigned vector is active.
    pub vector_active: bool,
    /// A direct-to clearance is active.
    pub direct_active: bool,
    /// Effective policy.
    pub settings: &'a Settings,
}

/// One independently testable deviation rule.
pub trait DeviationRule: Send + Sync {
    /// Stable identifier used in `deviations.toml`.
    fn id(&self) -> &'static str;
    /// Ground or flight.
    fn scope(&self) -> Scope;
    /// Evaluate the current sample.
    fn evaluate(&self, context: &DeviationContext<'_>) -> Option<Finding>;
}

/// Built-in rules in evaluation order.
#[must_use]
pub fn default_rules() -> Vec<Box<dyn DeviationRule>> {
    vec![
        Box::new(HoldShortPassed),
        Box::new(AirspaceViolation),
        Box::new(OffPlan),
    ]
}

/// The player continued beyond a runway holding point without clearance.
pub struct HoldShortPassed;

impl DeviationRule for HoldShortPassed {
    fn id(&self) -> &'static str {
        "hold_short_passed"
    }

    fn scope(&self) -> Scope {
        Scope::Ground
    }

    fn evaluate(&self, context: &DeviationContext<'_>) -> Option<Finding> {
        let airport = context.airport?;
        if context.taxi.hold_short_passed
            || !crate::airport::taxi_hold_passed(airport, context.taxi, context.telemetry)
        {
            return None;
        }
        let mut facts = BTreeMap::new();
        facts.insert("runway".to_owned(), context.taxi.hold_short_runway.clone());
        Some(Finding {
            rule: self.id(),
            scope: self.scope(),
            severity: Severity::Serious,
            phrase: "deviation_hold_short",
            facts,
        })
    }
}

/// The aircraft is inside a prohibited or restricted area.
pub struct AirspaceViolation;

impl DeviationRule for AirspaceViolation {
    fn id(&self) -> &'static str {
        "airspace"
    }

    fn scope(&self) -> Scope {
        Scope::Flight
    }

    fn evaluate(&self, context: &DeviationContext<'_>) -> Option<Finding> {
        let airspace = context.airspace?;
        if context.telemetry.on_ground || !context.telemetry.position_valid {
            return None;
        }
        let area = airspace.find(
            context.telemetry.latitude,
            context.telemetry.longitude,
            context.telemetry.altitude_feet,
            &context.settings.airspace_classes,
        )?;
        let mut facts = BTreeMap::new();
        facts.insert("airspace".to_owned(), area.name.clone());
        Some(Finding {
            rule: self.id(),
            scope: self.scope(),
            severity: if area.class.eq_ignore_ascii_case("P") {
                Severity::Serious
            } else {
                Severity::Correction
            },
            phrase: "deviation_airspace",
            facts,
        })
    }
}

/// The aircraft is off the cleared route without a vector or direct clearance.
pub struct OffPlan;

impl DeviationRule for OffPlan {
    fn id(&self) -> &'static str {
        "off_plan"
    }

    fn scope(&self) -> Scope {
        Scope::Flight
    }

    fn evaluate(&self, context: &DeviationContext<'_>) -> Option<Finding> {
        if context.telemetry.on_ground
            || !context.telemetry.position_valid
            || context.vector_active
            || context.direct_active
            || !matches!(
                context.phase,
                PhaseCode::Departure | PhaseCode::Cruise | PhaseCode::Arrival
            )
        {
            return None;
        }
        let next = context.next_fix.max(1);
        let leg = navigation::leg(&context.plan.fixes, next, context.telemetry)?;
        if leg.cross_track_nm <= context.settings.off_plan_tolerance_nm {
            return None;
        }
        Some(Finding {
            rule: self.id(),
            scope: self.scope(),
            severity: Severity::Correction,
            phrase: "deviation_off_plan",
            facts: BTreeMap::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn realistic_mode_overrides_defaults() {
        let policy = Policy::load(
            r#"
[policy]
mode = "realistic"
[modes.realistic]
flight_warnings = 4
[regions.australia]
flight_warnings = 6
"#,
            "australia",
        )
        .unwrap();
        assert_eq!(policy.mode, Mode::Realistic);
        assert_eq!(policy.limit(Scope::Flight), 6);
        assert_eq!(policy.settings.ground_warnings, 3);
    }

    #[test]
    fn unknown_rule_is_rejected() {
        let error = Policy::load("[rules]\nnot_a_rule = true", "us").unwrap_err();
        assert!(error.contains("unknown deviation rule"));
    }

    #[test]
    fn shipped_policy_parses_for_each_region() {
        let australia = Policy::load(DEFAULT_POLICY, "australia").unwrap();
        assert_eq!(australia.mode, Mode::Realistic);
        assert!(australia.rule("airspace"));
        let us = Policy::load(DEFAULT_POLICY, "us").unwrap();
        assert!(
            us.settings
                .airspace_classes
                .iter()
                .any(|class| class == "D")
        );
        assert!(Policy::airspace(DEFAULT_POLICY).unwrap().is_empty());
    }

    #[test]
    fn supplemental_airspace_is_optional() {
        let areas = Policy::airspace(
            r#"
[[airspace]]
name = "Test area"
class = "R"
lower_ft = 0
upper_ft = 1000
points = [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0]]
"#,
        )
        .unwrap();
        assert_eq!(areas.len(), 1);
    }

    #[test]
    fn off_plan_rule_fires_off_track() {
        let plan = FlightPlan {
            fixes: vec![
                crate::state::RouteFix {
                    identifier: "A".to_owned(),
                    latitude: 0.0,
                    longitude: 0.0,
                    path_terminator: "TF".to_owned(),
                    ..Default::default()
                },
                crate::state::RouteFix {
                    identifier: "B".to_owned(),
                    latitude: 0.0,
                    longitude: 1.0,
                    path_terminator: "TF".to_owned(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let telemetry = Telemetry {
            position_valid: true,
            on_ground: false,
            latitude: 0.1,
            longitude: 0.5,
            altitude_feet: 5000.0,
            ..Default::default()
        };
        let taxi = TaxiClearance::default();
        let settings = Settings::default();
        let context = DeviationContext {
            telemetry: &telemetry,
            plan: &plan,
            taxi: &taxi,
            airport: None,
            airspace: None,
            phase: PhaseCode::Cruise,
            next_fix: 1,
            vector_active: false,
            direct_active: false,
            settings: &settings,
        };
        assert_eq!(
            OffPlan.evaluate(&context).map(|finding| finding.phrase),
            Some("deviation_off_plan")
        );
    }
}
