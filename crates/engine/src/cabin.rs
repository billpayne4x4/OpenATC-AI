//! Simulated passenger activity; aircraft controls still require simulator readback.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use crate::EngineState;
use openatc_core::{
    dialogue::{phrases, say},
    state::PhaseCode,
};
use serde::{Deserialize, Serialize};
use std::{io::Read, path::Path, time::Instant};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    version: u32,
    #[serde(default = "default_ground_knots")]
    ground_disconnect_knots: f64,
    #[serde(default = "default_ground_meters")]
    ground_disconnect_meters: f64,
    #[serde(default = "default_mood")]
    passenger_mood: String,
    turbulence_preparation_seconds: f64,
    service_cycle_seconds: f64,
    lavatory_start_seconds: f64,
    lavatory_duration_seconds: f64,
    lavatory_occupants: u32,
    resting_brightness_percent: f64,
    crew_observation_max_age_seconds: f64,
    fallback_tick_limit_seconds: f64,
    simulation_tick_limit_seconds: f64,
}
fn ground_policy_value(key: &str) -> f64 {
    openatc_core::configuration::from_str::<toml::Value>(include_str!(
        "../../../cabin/behaviour.toml"
    ))
    .expect("bundled cabin policy")[key]
        .as_float()
        .expect("ground movement threshold")
}
fn default_ground_knots() -> f64 {
    ground_policy_value("ground_disconnect_knots")
}
fn default_ground_meters() -> f64 {
    ground_policy_value("ground_disconnect_meters")
}
fn default_mood() -> String {
    openatc_core::configuration::from_str::<toml::Value>(include_str!(
        "../../../cabin/behaviour.toml"
    ))
    .ok()
    .and_then(|value| value["passenger_mood"].as_str().map(str::to_owned))
    .unwrap_or_default()
}
impl Policy {
    fn parse(text: &str) -> Result<Self, String> {
        if text.len() > 8192 {
            return Err("Cabin policy exceeds 8 KiB".into());
        }
        let policy: Self =
            openatc_core::configuration::from_str(text).map_err(|error| error.to_string())?;
        let bounded =
            |value: f64, low: f64, high: f64| value.is_finite() && (low..=high).contains(&value);
        if policy.passenger_mood.is_empty()
            || policy.passenger_mood.len() > 64
            || policy.version != 1
            || !bounded(policy.ground_disconnect_knots, 0.5, 5.0)
            || !bounded(policy.ground_disconnect_meters, 1.0, 25.0)
            || !bounded(policy.turbulence_preparation_seconds, 10.0, 600.0)
            || !bounded(policy.service_cycle_seconds, 60.0, 7200.0)
            || !bounded(
                policy.lavatory_start_seconds,
                0.0,
                policy.service_cycle_seconds,
            )
            || !bounded(
                policy.lavatory_duration_seconds,
                1.0,
                policy.service_cycle_seconds - policy.lavatory_start_seconds,
            )
            || !bounded(policy.resting_brightness_percent, 0.0, 100.0)
            || policy.lavatory_occupants > 20
            || !bounded(policy.crew_observation_max_age_seconds, 0.5, 10.0)
            || !bounded(policy.fallback_tick_limit_seconds, 0.1, 5.0)
            || !bounded(policy.simulation_tick_limit_seconds, 1.0, 30.0)
        {
            return Err("Cabin policy has invalid timing or state limits".into());
        }
        Ok(policy)
    }
}

#[derive(Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct Cabin {
    #[serde(skip)]
    policy: Policy,
    /// Planned passenger count; zero means no manifest is available.
    passengers: i32,
    /// Persistent fictional cabin mood, not a sensor reading.
    passenger_mood: String,
    /// Simulated occupants, not a reading from an aircraft lavatory sensor.
    lavatory: u32,
    seated_passengers: Option<i32>,
    pub preparing_for_turbulence: bool,
    pub preparing_for_landing: bool,
    pub watching_passengers: bool,
    #[serde(skip)]
    last_update: Option<Instant>,
    #[serde(skip)]
    last_simulation_seconds: Option<f64>,
    cruise_seconds: f64,
    preparation_seconds: f64,
    #[serde(skip)]
    report_due: bool,
    preparation_reported: bool,
}

impl Default for Cabin {
    fn default() -> Self {
        Self {
            policy: Policy::parse(include_str!("../../../cabin/behaviour.toml"))
                .expect("bundled cabin policy"),
            passengers: 0,
            passenger_mood: default_mood(),
            lavatory: 0,
            seated_passengers: None,
            preparing_for_turbulence: false,
            preparing_for_landing: false,
            watching_passengers: false,
            last_update: None,
            last_simulation_seconds: None,
            cruise_seconds: 0.0,
            preparation_seconds: 0.0,
            report_due: false,
            preparation_reported: false,
        }
    }
}

impl Cabin {
    pub fn load(config: &Path, executable: &Path) -> Result<Self, String> {
        for directory in [Some(config), Some(executable), executable.parent()]
            .into_iter()
            .flatten()
        {
            let path = directory.join("cabin/behaviour.toml");
            let file = match std::fs::File::open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("{}: {error}", path.display())),
            };
            let mut text = String::new();
            file.take(8193)
                .read_to_string(&mut text)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            return Ok(Self {
                policy: Policy::parse(&text)
                    .map_err(|error| format!("{}: {error}", path.display()))?,
                ..Default::default()
            });
        }
        Ok(Self::default())
    }

    pub fn ground_disconnect_limits(&self) -> (f64, f64) {
        (
            self.policy.ground_disconnect_knots,
            self.policy.ground_disconnect_meters,
        )
    }

    pub fn reset(&mut self) {
        *self = Self {
            policy: self.policy.clone(),
            ..Default::default()
        };
    }

    pub fn tick(
        &mut self,
        phase: PhaseCode,
        passengers: i32,
        paused: bool,
        belts: Option<bool>,
        simulation_seconds: Option<f64>,
    ) {
        let now = Instant::now();
        let fallback = self.last_update.replace(now).map_or(0.0, |last| {
            now.duration_since(last)
                .as_secs_f64()
                .min(self.policy.fallback_tick_limit_seconds)
        });
        let elapsed =
            if let Some(seconds) = simulation_seconds.filter(|seconds| seconds.is_finite()) {
                self.last_simulation_seconds
                    .replace(seconds)
                    .map_or(0.0, |previous| {
                        let elapsed = seconds - previous;
                        if (0.0..=self.policy.simulation_tick_limit_seconds).contains(&elapsed) {
                            elapsed
                        } else {
                            0.0
                        }
                    })
            } else {
                self.last_simulation_seconds = None;
                fallback
            };
        self.advance(phase, passengers, if paused { 0.0 } else { elapsed }, belts);
    }

    fn advance(&mut self, phase: PhaseCode, passengers: i32, seconds: f64, belts: Option<bool>) {
        if passengers != self.passengers {
            self.passengers = passengers.max(0);
            self.lavatory = 0;
            self.cruise_seconds = 0.0;
        }
        if self.passengers <= 0 {
            self.lavatory = 0;
            return;
        }
        if self.preparing_for_turbulence {
            self.preparation_seconds += seconds;
            if self.preparation_seconds >= self.policy.turbulence_preparation_seconds {
                self.lavatory = 0;
                if belts == Some(true) && !self.preparation_reported {
                    self.report_due = true;
                }
            }
        } else if phase == PhaseCode::Cruise && belts == Some(false) {
            self.cruise_seconds += seconds;
            // Keep the activity stable between reports in the same service cycle.
            self.lavatory = u32::from(
                self.cruise_seconds % self.policy.service_cycle_seconds
                    >= self.policy.lavatory_start_seconds
                    && self.cruise_seconds % self.policy.service_cycle_seconds
                        < self.policy.lavatory_start_seconds
                            + self.policy.lavatory_duration_seconds,
            ) * self.policy.lavatory_occupants.min(self.passengers as u32);
        } else {
            self.lavatory = 0;
        }
    }
}

fn belts(state: &EngineState) -> Option<bool> {
    state.crew.observed_at.filter(|at| {
        at.elapsed().as_secs_f64() < state.cabin.policy.crew_observation_max_age_seconds
    })?;
    state.crew.values.get("seatbelts").map(|v| *v > 0.0)
}

pub fn tick(state: &mut EngineState) {
    state.cabin.passenger_mood = state.cabin.policy.passenger_mood.clone();
    let belts = belts(state);
    state.cabin.tick(
        state.session.phase,
        state.session.plan.passengers,
        state.session.telemetry.paused,
        belts,
        state.session.telemetry.simulation_seconds,
    );
    state.cabin.seated_passengers = (state.cabin.passengers > 0
        && !matches!(
            state.session.phase,
            PhaseCode::Parked | PhaseCode::Clearance
        ))
    .then_some((state.cabin.passengers - state.cabin.lavatory as i32).max(0));
}

/// Answer a drink or snack request with the published cabin phrasing.
pub fn service_reply(text: &str) -> Option<String> {
    let text = text.to_lowercase();
    let key = if text.contains("coffee") {
        "crew_service_coffee"
    } else if text.contains("tea") {
        "crew_service_tea"
    } else if text.contains("juice") {
        "crew_service_juice"
    } else if text.contains("water") {
        "crew_service_water"
    } else if text.contains("meal") || text.contains("food") {
        "crew_service_meal"
    } else if text.contains("snack") && text.contains("drink") {
        "crew_service_drink_snack"
    } else if text.contains("snack") || text.contains("nibble") {
        "crew_service_snack"
    } else if text.contains("drink") {
        "crew_service_drink_snack"
    } else {
        return None;
    };
    Some(say("crew_service_reply", &[("service", say(key, &[]))]))
}

/// Answer cabin reports and turbulence briefings without granting aircraft permissions.
pub fn report(state: &mut EngineState, text: &str) -> Option<String> {
    let text = text.to_lowercase();
    let matches = |key| phrases(key).iter().any(|p| text.contains(p));
    let warning = matches("cabin_request_turbulence");
    let clear = matches("cabin_request_turbulence_clear");
    let report = matches("cabin_request_passengers");
    let landing = matches("cabin_request_landing");
    let watch = matches("cabin_request_watch");
    let departure = matches("cabin_request_departure");
    if !warning && !clear && !report && !landing && !watch && !departure {
        return None;
    }
    if !state.session.mission.incident.is_empty() {
        return Some(state.session.mission.incident.clone());
    }
    tick(state);
    if clear {
        state.cabin.preparing_for_turbulence = false;
        state.cabin.preparation_seconds = 0.0;
        state.cabin.report_due = false;
        state.cabin.preparation_reported = false;
        return Some(say("cabin_turbulence_clear", &[]));
    }
    let mut reply = String::new();
    if landing {
        state.cabin.preparing_for_landing = true;
        reply.push_str(&say("cabin_landing_preparing", &[]));
    }
    if watch {
        state.cabin.watching_passengers = true;
        if !reply.is_empty() {
            reply.push(' ');
        }
        reply.push_str(&say("cabin_passenger_watch", &[]));
    }
    if departure {
        if !reply.is_empty() {
            reply.push(' ');
        }
        reply.push_str(&say("cabin_departure_preparing", &[]));
    }
    if warning {
        if !state.cabin.preparing_for_turbulence {
            state.cabin.preparation_seconds = 0.0;
            state.cabin.report_due = false;
            state.cabin.preparation_reported = false;
        }
        state.cabin.preparing_for_turbulence = true;
        reply.push_str(&say("cabin_turbulence_preparing", &[]));
        reply.push(' ');
        reply.push_str(&say(
            if belts(state) == Some(true) {
                "cabin_belts_on"
            } else {
                "cabin_belts_pending"
            },
            &[],
        ));
    }
    if report || warning {
        if !reply.is_empty() {
            reply.push(' ');
        }
        if state.cabin.passengers <= 0 {
            reply.push_str(&say("cabin_manifest_unknown", &[]));
        } else {
            let phase = if state.cabin.preparing_for_turbulence {
                "preparing"
            } else {
                match state.session.phase {
                    PhaseCode::Parked | PhaseCode::Clearance => "boarding",
                    PhaseCode::Pushback | PhaseCode::Taxi | PhaseCode::Departure => "departure",
                    PhaseCode::Cruise
                        if state.crew.observed_at.is_some_and(|at| {
                            at.elapsed().as_secs_f64()
                                < state.cabin.policy.crew_observation_max_age_seconds
                        }) && state.crew.values.get("cabin_brightness").is_some_and(|v| {
                            *v <= state.cabin.policy.resting_brightness_percent
                        }) =>
                    {
                        "resting"
                    }
                    PhaseCode::Cruise => "cruise",
                    PhaseCode::Arrival | PhaseCode::Approach => "arrival",
                    PhaseCode::Landed | PhaseCode::TaxiIn => "landed",
                    PhaseCode::Finished => "finished",
                }
            };
            reply.push_str(&say(
                &format!("cabin_passengers_{phase}"),
                &[("passengers", state.cabin.passengers.to_string())],
            ));
            if state.cabin.lavatory > 0 {
                reply.push(' ');
                reply.push_str(&if state.cabin.lavatory == 1 {
                    say("cabin_passenger_lavatory", &[])
                } else {
                    say(
                        "cabin_passengers_lavatory",
                        &[("count", state.cabin.lavatory.to_string())],
                    )
                });
            }
        }
    }
    Some(reply)
}

pub fn follow_up(state: &mut EngineState) {
    if !state.cabin.report_due
        || belts(state) != Some(true)
        || !state.session.crew_actions.is_empty()
    {
        return;
    }
    state.cabin.report_due = false;
    // Keep the warning active until the captain reports clear of the rough air.
    state.cabin.preparation_reported = true;
    crate::crew::speak(state, "cabin", &say("cabin_passengers_seated", &[]));
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    #[test]
    fn editable_policy_survives_reset_and_uses_simulator_time() {
        let root =
            std::env::temp_dir().join(format!("openatc-cabin-policy-{}", std::process::id()));
        let folder = root.join("cabin");
        std::fs::create_dir_all(&folder).unwrap();
        let text = include_str!("../../../cabin/behaviour.toml")
            .replace(
                "service_cycle_seconds = 600.0",
                "service_cycle_seconds = 60.0",
            )
            .replace(
                "lavatory_start_seconds = 120.0",
                "lavatory_start_seconds = 10.0",
            )
            .replace(
                "lavatory_duration_seconds = 90.0",
                "lavatory_duration_seconds = 5.0",
            )
            .replace(
                "turbulence_preparation_seconds = 45.0",
                "turbulence_preparation_seconds = 15.0",
            );
        let path = folder.join("behaviour.toml");
        std::fs::write(&path, &text).unwrap();
        let mut cabin = Cabin::load(&root, &root.join("bin")).unwrap();
        cabin.tick(PhaseCode::Cruise, 150, false, Some(false), Some(0.0));
        cabin.tick(PhaseCode::Cruise, 150, false, Some(false), Some(12.0));
        assert_eq!(cabin.lavatory, 1);
        cabin.tick(PhaseCode::Cruise, 150, true, Some(false), Some(24.0));
        cabin.tick(PhaseCode::Cruise, 150, false, Some(false), Some(26.0));
        assert_eq!(cabin.cruise_seconds, 14.0);
        cabin.tick(PhaseCode::Cruise, 150, false, Some(false), Some(1000.0));
        assert_eq!(cabin.cruise_seconds, 14.0);
        cabin.reset();
        assert_eq!(cabin.policy.turbulence_preparation_seconds, 15.0);
        assert_eq!(cabin.lavatory, 0);
        cabin.preparing_for_turbulence = true;
        cabin.advance(PhaseCode::Cruise, 150, 14.0, Some(true));
        assert!(!cabin.report_due);
        cabin.advance(PhaseCode::Cruise, 150, 1.0, Some(true));
        assert!(cabin.report_due);
        std::fs::write(
            &path,
            text.replace(
                "lavatory_duration_seconds = 5.0",
                "lavatory_duration_seconds = 100.0",
            ),
        )
        .unwrap();
        assert!(Cabin::load(&root, &root.join("bin")).is_err());
        assert!(Policy::parse(&format!("{text}\nunknown_rule = true")).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn lavatory_report_persists_until_preparation_finishes_and_sign_is_on() {
        let mut cabin = Cabin::default();
        cabin.advance(PhaseCode::Cruise, 142, 150.0, Some(false));
        assert_eq!(cabin.lavatory, 1);
        cabin.preparing_for_turbulence = true;
        cabin.advance(PhaseCode::Cruise, 142, 20.0, Some(false));
        assert_eq!(cabin.lavatory, 1);
        assert!(!cabin.report_due);
        cabin.advance(PhaseCode::Cruise, 142, 25.0, Some(false));
        assert_eq!(cabin.lavatory, 0);
        assert!(!cabin.report_due);
        cabin.advance(PhaseCode::Cruise, 142, 1.0, Some(true));
        assert!(cabin.report_due);
    }
    #[test]
    fn drink_and_snack_requests_use_the_published_cabin_phrases() {
        let text = service_reply("can I have a drink or a snack").expect("service reply");
        let lower = text.to_lowercase();
        assert!(lower.contains("drink") && lower.contains("snack"), "{text}");
        assert!(service_reply("how are the passengers").is_none());
        assert!(service_reply("request taxi").is_none());
    }

    #[test]
    fn unknown_manifest_does_not_invent_passengers_or_sleeping_during_startup() {
        let mut state = crate::test_state();
        assert!(
            report(&mut state, "how are the passengers")
                .unwrap()
                .contains("count")
        );
        state.session.plan.passengers = 142;
        let text = report(&mut state, "how are the passengers").unwrap();
        assert!(text.contains("142"));
        assert!(!text.contains("sleep") && !text.contains("landing"));
    }
}
