//! Interpret a whole crew request before queuing any aircraft controls.
use crate::crew::{CrewProfile, parse_many, validate};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
/// All parts of one pilot request, validated together before execution.
pub struct Intent {
    /// Spoken conversation from the role, held separately from executable controls.
    #[serde(rename = "conversation_reply", alias = "reply")]
    pub reply: String,
    /// Compact conversational continuity; never authoritative aircraft state.
    pub memory: String,
    /// Open-ended conversational points extracted from this message, not a service menu.
    pub conversation_points: Vec<String>,
    /// Aircraft controls in execution order.
    pub actions: Vec<ControlAction>,
    /// Null leaves a mapped control alone; a number proposes a new target.
    pub control_targets: std::collections::BTreeMap<String, Option<f64>>,
    /// Verify slide indications after any requested change.
    pub cross_check: bool,
    /// Legacy hint; validated actions determine whether execution is needed.
    pub conversation_only: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
/// A target selected from the loaded aircraft profile.
pub struct ControlAction {
    /// Profile control identifier, never a raw simulator dataref.
    pub control: String,
    /// Requested target in the control's declared units.
    pub value: f64,
}

impl Intent {
    /// Enforce role, range, count and duplicate-control limits.
    pub fn valid(&self, profile: &CrewProfile, role: &str) -> bool {
        if self.conversation_points.len() > 16
            || self
                .conversation_points
                .iter()
                .any(|point| point.len() > 1024)
        {
            return false;
        }
        if self.actions.is_empty()
            && self.control_targets.values().all(Option::is_none)
            && !self.cross_check
        {
            return !self.reply.trim().is_empty() || self.conversation_only;
        }
        self.valid_controls(profile, role)
    }

    /// Validate physical proposals before interpreting their conversational context.
    #[must_use]
    pub fn valid_controls(&self, profile: &CrewProfile, role: &str) -> bool {
        if role != "cabin" && self.cross_check {
            return false;
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut count = 0;
        self.actions
            .iter()
            .map(|action| (&action.control, action.value))
            .chain(
                self.control_targets
                    .iter()
                    .filter_map(|(control, value)| value.map(|value| (control, value))),
            )
            .all(|(control, value)| {
                count += 1;
                count <= 24 && seen.insert(control) && validate(profile, role, control, value)
            })
    }
}

/// Reply checks are separate from language understanding and control mappings.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplyPolicy {
    version: u32,
    narration_reject: Vec<String>,
}
impl ReplyPolicy {
    /// Load editable response constraints, falling back to bundled policy.
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                include_str!("../../../cabin/response-policy.toml").into()
            }
            Err(error) => return Err(error.to_string()),
        };
        if text.len() > 16384 {
            return Err("Reply policy is too large".into());
        }
        let policy: Self =
            crate::configuration::from_str(&text).map_err(|error| error.to_string())?;
        if policy.version != 1 || policy.narration_reject.len() > 16 {
            return Err("Unsupported reply policy".into());
        }
        for pattern in &policy.narration_reject {
            regex::Regex::new(pattern).map_err(|error| error.to_string())?;
        }
        Ok(policy)
    }
    /// Keep role replies from issuing aircraft movement permissions.
    #[must_use]
    pub fn narration_allowed(&self, text: &str) -> bool {
        self.narration_reject
            .iter()
            .all(|pattern| regex::Regex::new(pattern).is_ok_and(|pattern| !pattern.is_match(text)))
    }
}

/// Explicit mapped-control fallback when role conversations are disabled.
#[must_use]
pub fn parse(profile: &CrewProfile, role: &str, text: &str) -> Option<Intent> {
    let intent = Intent {
        actions: parse_many(profile, role, text)?
            .into_iter()
            .map(|(control, value)| ControlAction { control, value })
            .collect(),
        ..Default::default()
    };
    intent.valid(profile, role).then_some(intent)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conversation_has_no_menu_or_control_side_effects() {
        let profile = CrewProfile::default();
        let intent: Intent =
            serde_json::from_str(r#"{"actions":[],"cross_check":false,"conversation_only":true}"#)
                .unwrap();
        assert!(intent.valid(&profile, "cabin"));
        let reply: Intent =
            serde_json::from_str(r#"{"reply":"I'll check the galley for you.","actions":[]}"#)
                .unwrap();
        assert!(reply.valid(&profile, "cabin"));
        assert!(parse(&profile, "cabin", "coffee and a sandwich please").is_none());
        assert!(
            serde_json::from_str::<Intent>(r#"{"services":["dessert","something to drink"]}"#)
                .is_err()
        );
        let invalid: Intent = serde_json::from_str(
            r#"{"actions":[{"control":"unlisted","value":1}],"conversation_only":true}"#,
        )
        .unwrap();
        assert!(!invalid.valid(&profile, "cabin"));
    }
}

#[cfg(test)]
mod target_tests {
    use super::*;

    #[test]
    fn nullable_targets_validate_role_range_and_duplicate_proposals() {
        let profile = crate::crew::standard_profile();
        let intent: Intent = serde_json::from_value(
            serde_json::json!({"control_targets":{"heading":275,"beacon":null}}),
        )
        .unwrap();
        assert!(intent.valid_controls(&profile, "copilot"));
        assert!(!intent.valid_controls(&profile, "cabin"));
        let invalid: Intent =
            serde_json::from_value(serde_json::json!({"control_targets":{"heading":999}})).unwrap();
        assert!(!invalid.valid_controls(&profile, "copilot"));
        let duplicate: Intent = serde_json::from_value(serde_json::json!({"control_targets":{"heading":275},"actions":[{"control":"heading","value":275}]})).unwrap();
        assert!(!duplicate.valid_controls(&profile, "copilot"));
    }
}
