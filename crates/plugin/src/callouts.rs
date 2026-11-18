//! Preloaded takeoff callouts from airspeed, weight on wheels and climb.
use openatc_audio::Speech;
use openatc_core::state::{PhaseCode, Telemetry};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    version: u32,
    enabled: bool,
    minimum_knots: f64,
    maximum_knots: f64,
    reset_below_knots: f64,
    maximum_agl_feet: f64,
    maximum_delay_seconds: f64,
    #[serde(default)]
    ias_refs: Vec<String>,
    v1_refs: Vec<String>,
    vr_refs: Vec<String>,
    v2_refs: Vec<String>,
    v1_text: String,
    vr_text: String,
    #[serde(default, rename = "v2_text")]
    _legacy_v2_text: Option<String>,
    #[serde(default = "default_roll")]
    roll: RollPolicy,
    #[serde(default = "default_positive")]
    positive: PositivePolicy,
    #[serde(default)]
    v1_vr_text: Option<String>,
    #[serde(default)]
    log_labels: Vec<String>,
    #[serde(default)]
    log_template: Option<String>,
}
const V1: usize = 0;
const ROTATE: usize = 1;
const POSITIVE: usize = 2;
const ALIVE: usize = 3;
const EIGHTY: usize = 4;
const COMBINED: usize = 5;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RollPolicy {
    airspeed_alive_knots: f64,
    eighty_knots: f64,
    airspeed_alive_text: String,
    eighty_knots_text: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PositivePolicy {
    minimum_agl_feet: f64,
    minimum_climb_fpm: f64,
    confirmation_seconds: f64,
    text: String,
}
fn bundled_section<T: serde::de::DeserializeOwned>(name: &str) -> T {
    let value: toml::Value = toml::from_str(include_str!("../../../aircraft/callouts.toml"))
        .expect("bundled callout policy");
    value[name]
        .clone()
        .try_into()
        .expect("bundled callout section")
}
fn default_roll() -> RollPolicy {
    bundled_section("roll")
}
fn default_positive() -> PositivePolicy {
    bundled_section("positive")
}

#[derive(Default)]
struct Sequence {
    fired: [bool; 5],
    previous: Option<f64>,
    roll_seen: bool,
    climbing_since: Option<f64>,
}
impl Sequence {
    fn update(
        &mut self,
        policy: &Policy,
        telemetry: &Telemetry,
        ias: f64,
        speeds: [Option<f64>; 3],
        now: f64,
    ) -> Vec<usize> {
        if telemetry.on_ground
            && ias < policy.reset_below_knots
            && telemetry.ground_speed_knots < policy.reset_below_knots
        {
            *self = Self::default();
        }
        let previous = self.previous.replace(ias);
        let rising = previous.is_some_and(|previous| ias > previous);
        if telemetry.on_ground && rising && ias >= policy.roll.airspeed_alive_knots {
            self.roll_seen = true;
        }
        let mut calls = Vec::new();
        if telemetry.on_ground && rising {
            for (call, speed) in [
                (ALIVE, Some(policy.roll.airspeed_alive_knots)),
                (EIGHTY, Some(policy.roll.eighty_knots)),
                (V1, speeds[0]),
                (ROTATE, speeds[1]),
            ] {
                if speed.is_some_and(|speed| {
                    !self.fired[call]
                        && previous.is_some_and(|previous| previous < speed)
                        && ias >= speed
                }) {
                    self.fired[call] = true;
                    calls.push(call);
                }
            }
        }
        let climbing = self.roll_seen
            && !telemetry.on_ground
            && telemetry.height_agl_feet.is_finite()
            && telemetry.height_agl_feet >= policy.positive.minimum_agl_feet
            && telemetry.vertical_speed_fpm.is_finite()
            && telemetry.vertical_speed_fpm >= policy.positive.minimum_climb_fpm;
        if climbing {
            let since = self.climbing_since.get_or_insert(now);
            if now < *since {
                *since = now;
            }
            if !self.fired[POSITIVE] && now - *since >= policy.positive.confirmation_seconds {
                self.fired[POSITIVE] = true;
                calls.push(POSITIVE);
            }
        } else {
            self.climbing_since = None;
        }
        calls
    }
}

#[derive(Default)]
pub struct Runtime {
    audio: Speech,
    source: String,
    policy: Option<Policy>,
    refs: Vec<Vec<crate::panel::Readings>>,
    ias: Vec<crate::panel::Readings>,
    voice_key: String,
    sequence: Sequence,
    pending: std::collections::VecDeque<(usize, std::time::Instant)>,
    playing: Option<usize>,
    clock: Option<std::time::Instant>,
    rebound: Option<std::time::Instant>,
    status: String,
    pub active: bool,
}
impl Runtime {
    pub fn reset(&mut self) {
        self.audio.clear_preloaded();
        self.pending.clear();
        self.sequence = Sequence::default();
        self.playing = None;
        self.active = false;
        self.voice_key.clear();
        self.clock = None;
    }

    pub fn update(
        &mut self,
        data: &mut xplane::data::DataApi,
        aircraft: &str,
        interface: &mut openatc_ui::interface::Interface,
        telemetry: &Telemetry,
    ) {
        let path = std::path::Path::new(&interface.settings.simulator_root)
            .join("Resources/plugins/OpenATC/aircraft/callouts.toml");
        let source = format!("{aircraft}/{}", path.display());
        if self.source != source {
            self.source = source;
            self.policy = std::fs::read_to_string(&path)
                .ok()
                .or_else(|| Some(include_str!("../../../aircraft/callouts.toml").to_owned()))
                .and_then(|text| toml::from_str::<Policy>(&text).ok())
                .filter(|p| {
                    p.version == 1
                        && p.minimum_knots >= 20.0
                        && p.maximum_knots <= 350.0
                        && p.minimum_knots < p.maximum_knots
                        && p.reset_below_knots > 0.0
                        && p.reset_below_knots < p.minimum_knots
                        && (10.0..=2000.0).contains(&p.maximum_agl_feet)
                        && (0.1..=2.0).contains(&p.maximum_delay_seconds)
                        && (20.0..=60.0).contains(&p.roll.airspeed_alive_knots)
                        && p.roll.eighty_knots == 80.0
                        && (1.0..=50.0).contains(&p.positive.minimum_agl_feet)
                        && (50.0..=500.0).contains(&p.positive.minimum_climb_fpm)
                        && (0.1..=1.0).contains(&p.positive.confirmation_seconds)
                        && [
                            &p.v1_text,
                            &p.vr_text,
                            &p.positive.text,
                            &p.roll.airspeed_alive_text,
                            &p.roll.eighty_knots_text,
                        ]
                        .iter()
                        .all(|t| !t.is_empty() && t.len() < 64)
                });
            self.refs = self
                .policy
                .as_ref()
                .map(|p| {
                    [&p.v1_refs, &p.vr_refs, &p.v2_refs]
                        .into_iter()
                        .map(|names| {
                            names
                                .iter()
                                .take(8)
                                .filter_map(|name| crate::panel::find_readings(data, name))
                                .collect()
                        })
                        .collect()
                })
                .unwrap_or_default();
            self.sequence = Sequence::default();
            self.pending.clear();
            self.audio.clear_preloaded();
            self.voice_key.clear();
        }
        let Some(p) = self.policy.as_ref().filter(|p| p.enabled) else {
            self.audio.stop_speech();
            self.pending.clear();
            self.active = false;
            return;
        };
        if self.rebound.is_none_or(|at| at.elapsed().as_secs() >= 2) {
            self.rebound = Some(std::time::Instant::now());
            self.ias = p
                .ias_refs
                .iter()
                .take(8)
                .filter_map(|name| crate::panel::find_readings(data, name))
                .collect();
            self.refs = [&p.v1_refs, &p.vr_refs, &p.v2_refs]
                .into_iter()
                .map(|names| {
                    names
                        .iter()
                        .take(8)
                        .filter_map(|name| crate::panel::find_readings(data, name))
                        .collect()
                })
                .collect();
        }
        let settings = &interface.settings;
        let state = interface.engine.state();
        let key = format!(
            "{}/{}/{}/{}/{}/{:?}",
            interface.engine.endpoint(),
            settings.tts_url,
            settings.tts_model,
            settings.copilot_voice,
            settings.copilot_speed * settings.tts_speed,
            [
                p.v1_text.as_str(),
                p.vr_text.as_str(),
                p.positive.text.as_str(),
                p.roll.airspeed_alive_text.as_str(),
                p.roll.eighty_knots_text.as_str(),
                p.v1_vr_text.as_deref().unwrap_or("")
            ]
        );
        if self.voice_key != key {
            self.audio.clear_preloaded();
            self.voice_key = key;
        }
        self.audio
            .configure("", &settings.output_device, 1.0, settings.copilot_volume);
        if telemetry.on_ground
            && telemetry.ground_speed_knots < p.reset_below_knots
            && state.phase != PhaseCode::Departure
        {
            self.sequence = Sequence::default();
            self.pending.clear();
        }
        if !interface.engine.connected() || telemetry.paused {
            self.audio.stop_speech();
            self.pending.clear();
            self.sequence.climbing_since = None;
            self.sequence.previous = None;
            self.active = false;
            return;
        }
        if telemetry.on_ground && matches!(state.phase, PhaseCode::Taxi | PhaseCode::Departure) {
            for (i, text) in [
                &p.v1_text,
                &p.vr_text,
                &p.positive.text,
                &p.roll.airspeed_alive_text,
                &p.roll.eighty_knots_text,
            ]
            .into_iter()
            .enumerate()
            {
                self.audio.preload(
                    &i.to_string(),
                    &interface.engine.endpoint(),
                    text,
                    &settings.copilot_voice,
                    settings.copilot_speed * settings.tts_speed,
                );
            }
            if let Some(text) = p
                .v1_vr_text
                .as_ref()
                .filter(|text| !text.is_empty() && text.len() < 64)
            {
                self.audio.preload(
                    &COMBINED.to_string(),
                    &interface.engine.endpoint(),
                    text,
                    &settings.copilot_voice,
                    settings.copilot_speed * settings.tts_speed,
                );
            }
        }
        let speeds: Vec<_> =
            self.refs
                .iter()
                .map(|refs| {
                    refs.iter().find_map(|read| {
                        read().first().copied().filter(|v| {
                            v.is_finite() && *v >= p.minimum_knots && *v <= p.maximum_knots
                        })
                    })
                })
                .collect();
        let observed_speeds: [Option<f64>; 3] = speeds.try_into().unwrap_or([None; 3]);
        let speeds = rotation_compatible_speeds(observed_speeds);
        let valid = speeds[V1]
            .zip(speeds[ROTATE])
            .is_none_or(|(v1, vr)| v1 <= vr);
        let status = format!(
            "V-speeds {:?}; usable {:?}; cache/fired slots [V1, Rotate, Positive rate, Airspeed alive, 80 knots]; cached {:?}; phase {:?}; ordered {valid}; fired {:?}; {}",
            observed_speeds,
            speeds,
            [
                self.audio.preloaded("0"),
                self.audio.preloaded("1"),
                self.audio.preloaded("2"),
                self.audio.preloaded("3"),
                self.audio.preloaded("4")
            ],
            state.phase,
            self.sequence.fired,
            self.audio.status()
        );
        if self.status != status {
            crate::debug_log(&format!("OpenATC callouts: {status}\n"));
            self.status.clone_from(&status);
        }
        interface.callout_status = status;
        self.active = state.phase == PhaseCode::Departure
            && telemetry.height_agl_feet < p.maximum_agl_feet
            && valid;
        if !self.active {
            self.audio.stop_speech();
            self.pending.clear();
            self.sequence.previous = None;
            return;
        }
        let Some(ias) = self
            .ias
            .iter()
            .find_map(|read| {
                read()
                    .first()
                    .copied()
                    .filter(|v| v.is_finite() && *v > 0.0)
            })
            .or(telemetry.indicated_speed_knots.filter(|v| v.is_finite()))
        else {
            return;
        };
        if telemetry.on_ground
            && ias < p.reset_below_knots
            && telemetry.ground_speed_knots < p.reset_below_knots
        {
            self.audio.stop_speech();
            self.pending.clear();
            self.playing = None;
        }
        let now = telemetry
            .simulation_seconds
            .filter(|value| value.is_finite())
            .unwrap_or_else(|| {
                self.clock
                    .get_or_insert_with(std::time::Instant::now)
                    .elapsed()
                    .as_secs_f64()
            });
        let crossed = self.sequence.update(p, telemetry, ias, speeds, now);
        if !self.audio.busy() {
            self.playing = None;
        }
        let rotation_clip = rotation_clip(
            &crossed,
            p.v1_vr_text
                .as_ref()
                .is_some_and(|text| !text.is_empty() && text.len() < 64),
        );
        let combined = rotation_clip == COMBINED;
        for i in crossed {
            let target = match i {
                V1 | ROTATE => speeds[i],
                ALIVE => Some(p.roll.airspeed_alive_knots),
                EIGHTY => Some(p.roll.eighty_knots),
                _ => None,
            };
            crate::debug_log(&format!(
                "OpenATC callout threshold: index={i} target={:?} IAS={ias:.1} AGL={:.1} busy={}\n",
                target,
                telemetry.height_agl_feet,
                self.audio.busy()
            ));
            if combined && i == 0 {
                continue;
            }
            if i == 1 {
                // Rotate takes precedence over an unfinished V1 clip.
                self.pending.retain(|(call, _)| *call == POSITIVE);
                if self.playing != Some(1) && self.audio.busy() {
                    self.audio.stop_speech();
                    self.playing = None;
                }
                self.pending
                    .push_front((rotation_clip, std::time::Instant::now()));
            } else if i == V1 {
                self.pending
                    .retain(|(call, _)| !matches!(*call, ALIVE | EIGHTY));
                if matches!(self.playing, Some(ALIVE | EIGHTY)) {
                    self.audio.stop_speech();
                    self.playing = None;
                }
                self.pending.push_front((i, std::time::Instant::now()));
            } else {
                self.pending.push_back((i, std::time::Instant::now()));
            }
        }
        while self
            .pending
            .front()
            .is_some_and(|(_, at)| at.elapsed().as_secs_f64() > p.maximum_delay_seconds)
        {
            if let Some((index, at)) = self.pending.pop_front() {
                crate::debug_log(&format!(
                    "OpenATC callout expired: clip={index} queued_ms={} IAS={ias:.1}\n",
                    at.elapsed().as_millis()
                ));
            }
        }
        if let Some((i, queued)) = self.pending.front().copied()
            && self
                .audio
                .play_preloaded(&i.to_string(), settings.copilot_volume)
        {
            self.pending.pop_front();
            crate::debug_log(&format!(
                "OpenATC callout playback: clip={i} queued_ms={} IAS={ias:.1}\n",
                queued.elapsed().as_millis()
            ));
            self.playing = Some(i);
            let entries = log_entries(p, speeds, i);
            interface.engine.post(
                "/crew/callout",
                serde_json::json!({"entries":entries,"callsign":state.plan.callsign}),
            );
        }
    }
}
fn log_entries(policy: &Policy, speeds: [Option<f64>; 3], clip: usize) -> Vec<String> {
    match clip {
        POSITIVE => return vec![policy.positive.text.clone()],
        ALIVE => return vec![policy.roll.airspeed_alive_text.clone()],
        EIGHTY => return vec![policy.roll.eighty_knots_text.clone()],
        _ => {}
    }
    let bundled: toml::Value = toml::from_str(include_str!("../../../aircraft/callouts.toml"))
        .expect("bundled callout policy");
    let template = policy
        .log_template
        .as_deref()
        .unwrap_or_else(|| bundled["log_template"].as_str().unwrap());
    let indices: &[usize] = if clip == COMBINED {
        &[V1, ROTATE]
    } else {
        std::slice::from_ref(&clip)
    };
    indices
        .iter()
        .filter_map(|index| {
            let speed = speeds.get(*index).copied().flatten()?;
            let label = policy.log_labels.get(*index).map_or_else(
                || bundled["log_labels"][*index].as_str().unwrap(),
                String::as_str,
            );
            Some(
                template
                    .replace("{callout}", label)
                    .replace("{speed}", &format!("{speed:.0}")),
            )
        })
        .collect()
}
fn rotation_compatible_speeds(mut speeds: [Option<f64>; 3]) -> [Option<f64>; 3] {
    if let Some(vr) = speeds[1] {
        // Ignore a V1 or V2 setting that conflicts with the rotation speed.
        if speeds[0].is_some_and(|v1| v1 > vr) {
            speeds[0] = None;
        }
        if speeds[2].is_some_and(|v2| v2 < vr) {
            speeds[2] = None;
        }
    }
    speeds
}

fn rotation_clip(crossed: &[usize], combined_available: bool) -> usize {
    if combined_available && crossed.contains(&0) && crossed.contains(&1) {
        COMBINED
    } else {
        ROTATE
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> Policy {
        toml::from_str(include_str!("../../../aircraft/callouts.toml")).unwrap()
    }
    fn ground() -> Telemetry {
        Telemetry {
            on_ground: true,
            ground_speed_knots: 100.0,
            ..Default::default()
        }
    }

    #[test]
    fn complete_roll_and_liftoff_never_announce_v2() {
        let p = policy();
        let speeds = [Some(123.0), Some(146.0), Some(148.0)];
        let mut sequence = Sequence::default();
        let mut calls = Vec::new();
        for speed in [
            0.0, 30.0, 35.0, 79.0, 80.0, 122.0, 123.0, 145.0, 146.0, 147.0, 148.0,
        ] {
            calls.extend(sequence.update(&p, &ground(), speed, speeds, speed));
        }
        assert_eq!(calls, vec![ALIVE, EIGHTY, V1, ROTATE]);
        let airborne = Telemetry {
            on_ground: false,
            height_agl_feet: 10.0,
            vertical_speed_fpm: 500.0,
            ..ground()
        };
        assert!(
            sequence
                .update(&p, &airborne, 150.0, speeds, 150.0)
                .is_empty()
        );
        assert_eq!(
            sequence.update(&p, &airborne, 152.0, speeds, 150.4),
            vec![POSITIVE]
        );
        assert!(
            sequence
                .update(&p, &airborne, 155.0, speeds, 151.0)
                .is_empty()
        );
        let text: Vec<_> = [ALIVE, EIGHTY, V1, ROTATE, POSITIVE]
            .into_iter()
            .flat_map(|clip| log_entries(&p, speeds, clip))
            .collect();
        assert_eq!(
            text,
            [
                "Airspeed alive",
                "80 knots",
                "V1 · 123 kt",
                "Rotate · 146 kt",
                "Positive rate"
            ]
        );
        assert!(!text.iter().any(|text| text.contains("V2")));
    }

    #[test]
    fn positive_rate_requires_an_observed_roll_and_sustained_airborne_climb() {
        let p = policy();
        let speeds = [Some(123.0), Some(146.0), Some(148.0)];
        let mut sequence = Sequence::default();
        let mut t = Telemetry {
            height_agl_feet: 10.0,
            vertical_speed_fpm: 500.0,
            ..ground()
        };
        sequence.update(&p, &t, 30.0, speeds, 0.0);
        sequence.update(&p, &t, 40.0, speeds, 0.1);
        assert!(
            sequence
                .update(&p, &t, 150.0, speeds, 1.0)
                .iter()
                .all(|call| *call != POSITIVE)
        );
        t.on_ground = false;
        t.height_agl_feet = 0.5;
        assert!(sequence.update(&p, &t, 150.0, speeds, 1.5).is_empty());
        t.height_agl_feet = 10.0;
        t.vertical_speed_fpm = -50.0;
        assert!(sequence.update(&p, &t, 151.0, speeds, 2.0).is_empty());
        t.vertical_speed_fpm = 500.0;
        assert!(sequence.update(&p, &t, 152.0, speeds, 3.0).is_empty());
        t.on_ground = true;
        sequence.update(&p, &t, 153.0, speeds, 3.2);
        t.on_ground = false;
        assert!(sequence.update(&p, &t, 154.0, speeds, 3.4).is_empty());
        assert_eq!(sequence.update(&p, &t, 155.0, speeds, 3.8), vec![POSITIVE]);
        let mut attached_airborne = Sequence::default();
        assert!(
            attached_airborne
                .update(&p, &t, 155.0, speeds, 4.0)
                .is_empty()
        );
        assert!(
            attached_airborne
                .update(&p, &t, 156.0, speeds, 5.0)
                .is_empty()
        );
    }

    #[test]
    fn legacy_v2_wording_cannot_enable_a_v2_callout() {
        let text = include_str!("../../../aircraft/callouts.toml");
        let root = text.split("[roll]").next().unwrap().to_owned() + "v2_text = \"V two\"\n";
        let p: Policy = toml::from_str(&root).unwrap();
        assert_eq!(p.positive.text, "Positive rate");
        assert_eq!(log_entries(&p, [None; 3], POSITIVE), vec!["Positive rate"]);
    }

    #[test]
    fn rejected_takeoff_rearms_and_missing_speed_bugs_do_not_disable_roll_calls() {
        let p = policy();
        let mut s = Sequence::default();
        s.update(&p, &ground(), 30.0, [None; 3], 0.0);
        assert_eq!(
            s.update(&p, &ground(), 80.0, [None; 3], 1.0),
            vec![ALIVE, EIGHTY]
        );
        let stopped = Telemetry::default();
        s.update(&p, &stopped, 0.0, [None; 3], 2.0);
        s.update(&p, &ground(), 30.0, [None; 3], 3.0);
        assert_eq!(s.update(&p, &ground(), 35.0, [None; 3], 4.0), vec![ALIVE]);
    }

    #[test]
    fn rotate_keeps_its_speed_source_and_combines_equal_v1() {
        let p = policy();
        assert_eq!(p.v1_refs[0], "toliss_airbus/performance/V1");
        assert_eq!(p.vr_refs[0], "toliss_airbus/performance/VR");
        assert_eq!(p.v2_refs[0], "toliss_airbus/performance/V2");
        let speeds = rotation_compatible_speeds([Some(155.0), Some(146.0), Some(148.0)]);
        assert_eq!(speeds, [None, Some(146.0), Some(148.0)]);
        assert_eq!(rotation_clip(&[V1, ROTATE], true), COMBINED);
        assert_eq!(rotation_clip(&[ROTATE], true), ROTATE);
        let mut s = Sequence::default();
        assert!(s.update(&p, &ground(), 160.0, speeds, 0.0).is_empty());
        assert!(s.update(&p, &ground(), 161.0, speeds, 0.1).is_empty());
    }
}
