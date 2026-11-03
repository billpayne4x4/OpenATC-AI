//! Time-based aircraft animation. Sampling does not call the simulator SDK.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use super::motion::Stage;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Distinct movement envelopes; neither wake class nor parking size selects these.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Light reciprocating aircraft.
    Piston,
    /// Propeller aircraft driven by turbine engines.
    Turboprop,
    /// Regional and business jets.
    RegionalJet,
    /// Mainline narrow-body jets.
    Jet,
    /// Larger transport jets.
    HeavyJet,
    /// Rotary-wing aircraft.
    Rotorcraft,
}

/// Editable nominal rates and speeds used by the generated-traffic planner.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Handling {
    /// Nominal roundout start above the local runway surface.
    pub flare_height_feet: f64,
    /// Nominal sink rate relative to the runway at contact.
    pub touchdown_sink_fpm: f64,
    /// Nose-up rotation target at the nominal liftoff speed.
    pub rotation_pitch_degrees: f64,
    /// Nominal angle of attack added to the climb flight-path angle.
    pub climb_pitch_bias_degrees: f64,
    /// Nominal landing-configuration angle of attack, separate from descent angle.
    pub approach_pitch_bias_degrees: f64,
    /// Ground target speed.
    pub taxi_knots: f64,
    /// Lower speed through a curved taxi segment.
    pub taxi_turn_knots: f64,
    /// Preferred centreline turn radius.
    pub taxi_turn_radius_meters: f64,
    /// Nominal indicated rotation speed; convert to TAS/ground speed before scheduling a roll.
    pub rotation_knots: f64,
    /// Maximum commanded increase in speed per second.
    pub acceleration_knots_sec: f64,
    /// Maximum commanded decrease in speed per second.
    pub braking_knots_sec: f64,
    /// Visual bank limit for ordinary turns.
    pub maximum_bank_degrees: f64,
    /// Bank change per second.
    pub bank_rate_degrees_sec: f64,
    /// Pitch change per second.
    pub pitch_rate_degrees_sec: f64,
    /// Full gear transit time.
    pub gear_seconds: f64,
    /// Full flap transit time.
    pub flap_seconds: f64,
    /// Nominal tyre radius for wheel rotation.
    pub wheel_radius_meters: f64,
    /// Propeller rotation rate when running.
    pub propeller_rpm: f64,
    /// Axle spacing used to estimate steering while taxiing.
    pub wheelbase_meters: f64,
    /// Nose-wheel steering limit.
    pub maximum_steering_degrees: f64,
    /// Normalized model flap setting for departure.
    pub takeoff_flaps: f64,
    /// Normalized model flap setting on final.
    pub landing_flaps: f64,
    /// Time for engine animation to reach running speed.
    pub engine_spool_seconds: f64,
    /// Length of the model's touchdown signal.
    pub touchdown_seconds: f64,
    /// Whether the aircraft has ground spoilers.
    pub rollout_spoilers: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: u32,
    handling: BTreeMap<Kind, Handling>,
}

/// Reject invalid rates before a worker plans or samples a flight.
pub fn parse(text: &str) -> Result<BTreeMap<Kind, Handling>, String> {
    if text.len() > 65536 {
        return Err("Handling catalogue exceeds 64 KiB".into());
    }
    let mut value: toml::Value =
        crate::configuration::from_str(text).map_err(|error| error.to_string())?;
    let defaults: toml::Value =
        crate::configuration::from_str(include_str!("../../../../traffic/handling.toml"))
            .map_err(|error| error.to_string())?;
    if let Some(handling) = value
        .get_mut("handling")
        .and_then(toml::Value::as_table_mut)
    {
        for (kind, fields) in handling {
            if let Some(fields) = fields.as_table_mut() {
                for key in ["flare_height_feet", "touchdown_sink_fpm"] {
                    if let Some(default) = defaults
                        .get("handling")
                        .and_then(|table| table.get(kind))
                        .and_then(|table| table.get(key))
                    {
                        fields
                            .entry(key.to_owned())
                            .or_insert_with(|| default.clone());
                    }
                }
            }
        }
    }
    let file: File = value
        .try_into()
        .map_err(|error: toml::de::Error| error.to_string())?;
    if file.version != 1 || file.handling.len() != 6 {
        return Err("Handling catalogue requires version 1 and all five envelopes".into());
    }
    let bounded =
        |value: f64, low: f64, high: f64| value.is_finite() && (low..=high).contains(&value);
    for h in file.handling.values() {
        if !bounded(h.flare_height_feet, 5.0, 60.0)
            || !bounded(h.touchdown_sink_fpm, 30.0, 300.0)
            || !bounded(h.taxi_knots, 3.0, 25.0)
            || !bounded(h.rotation_pitch_degrees, 3.0, 15.0)
            || !bounded(h.climb_pitch_bias_degrees, 0.0, 5.0)
            || !bounded(h.approach_pitch_bias_degrees, 0.0, 8.0)
            || !bounded(h.taxi_turn_knots, 2.0, h.taxi_knots)
            || !bounded(h.taxi_turn_radius_meters, 2.0, 100.0)
            || !bounded(h.rotation_knots, 35.0, 190.0)
            || !bounded(h.acceleration_knots_sec, 0.1, 10.0)
            || !bounded(h.braking_knots_sec, 0.1, 10.0)
            || !bounded(h.maximum_bank_degrees, 5.0, 30.0)
            || !bounded(h.bank_rate_degrees_sec, 0.1, 20.0)
            || !bounded(h.pitch_rate_degrees_sec, 0.1, 10.0)
            || !bounded(h.gear_seconds, 1.0, 30.0)
            || !bounded(h.flap_seconds, 1.0, 60.0)
            || !bounded(h.wheel_radius_meters, 0.1, 1.5)
            || !bounded(h.propeller_rpm, 0.0, 4000.0)
            || !bounded(h.wheelbase_meters, 1.0, 35.0)
            || !bounded(h.maximum_steering_degrees, 10.0, 80.0)
            || !bounded(h.takeoff_flaps, 0.0, 1.0)
            || !bounded(h.landing_flaps, h.takeoff_flaps, 1.0)
            || !bounded(h.engine_spool_seconds, 1.0, 60.0)
            || !bounded(h.touchdown_seconds, 0.1, 3.0)
        {
            return Err("Handling envelope contains an invalid speed or animation rate".into());
        }
    }
    Ok(file.handling)
}

/// Bundled defaults are also installed for editing.
pub fn builtin() -> Result<BTreeMap<Kind, Handling>, String> {
    parse(include_str!("../../../../traffic/handling.toml"))
}

/// Controller-approved state sampled on a simulation worker.
#[derive(Clone, Copy, Debug)]
pub struct Input {
    /// Current operational segment.
    pub stage: Stage,
    /// Measured ground speed in knots.
    pub knots: f64,
    /// Body-heading change in degrees per second.
    pub turn_degrees_sec: f64,
    /// Actual vertical movement.
    pub vertical_fpm: f64,
    /// Engine state already determined by the flight lifecycle.
    pub engines_running: bool,
    /// Fixed-gear aircraft never retract it.
    pub fixed_gear: bool,
    /// Light state determined by the flight lifecycle.
    pub landing_lights: bool,
}

/// Per-aircraft animation state, retained across snapshots.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Animation {
    /// Degrees nose up.
    pub pitch: f64,
    /// Degrees right-wing down.
    pub bank: f64,
    /// Gear extension, zero retracted and one extended.
    pub gear: f64,
    /// Flap extension in the model's normalized range.
    pub flaps: f64,
    /// Engine spool ratio.
    pub engine: f64,
    /// Propeller rotation, degrees modulo 360.
    pub propeller_angle: f64,
    /// Current propeller speed, for disk/blade visibility.
    pub propeller_rpm: f64,
    /// Wheel rotation, degrees modulo 360.
    pub wheel_angle: f64,
    /// Landing light command.
    pub landing_lights: bool,
    /// Engine-running beacon command.
    pub beacon: bool,
    /// Navigation light command.
    pub navigation_lights: bool,
    /// Airborne/runway strobe command.
    pub strobes: bool,
    /// Normalized ground spoiler extension.
    pub speed_brakes: f64,
    /// Nose-wheel angle in degrees.
    pub steering: f64,
    /// Normalized landing-gear compression.
    pub wheel_compression: f64,
    /// Remaining touchdown indication time.
    pub touchdown: f64,
    /// Used to distinguish touchdown from loading a parked aircraft.
    pub was_airborne: bool,
}
impl Default for Animation {
    fn default() -> Self {
        Self {
            pitch: 0.0,
            bank: 0.0,
            gear: 1.0,
            flaps: 0.0,
            engine: 0.0,
            propeller_angle: 0.0,
            propeller_rpm: 0.0,
            wheel_angle: 0.0,
            landing_lights: false,
            beacon: false,
            navigation_lights: false,
            strobes: false,
            speed_brakes: 0.0,
            steering: 0.0,
            wheel_compression: 0.0,
            touchdown: 0.0,
            was_airborne: false,
        }
    }
}
impl Animation {
    /// Paused or invalid samples do not change state. Large jumps must be handled by the caller.
    pub fn advance(&mut self, input: Input, h: &Handling, seconds: f64) {
        if !seconds.is_finite()
            || !(0.0..=1.0).contains(&seconds)
            || seconds == 0.0
            || !input.knots.is_finite()
            || !input.vertical_fpm.is_finite()
            || !input.turn_degrees_sec.is_finite()
        {
            return;
        }
        let approach = |value: f64, target: f64, rate: f64| {
            value + (target - value).clamp(-rate * seconds, rate * seconds)
        };
        let ground = input.stage.on_ground();
        let bank = if ground {
            0.0
        } else {
            (input.knots.max(0.0) * 0.514_444 * input.turn_degrees_sec.to_radians() / 9.80665)
                .atan()
                .to_degrees()
                .clamp(-h.maximum_bank_degrees, h.maximum_bank_degrees)
        };
        let pitch = if input.stage == Stage::Takeoff {
            h.rotation_pitch_degrees
                * ((input.knots / h.rotation_knots - 0.9) / 0.1).clamp(0.0, 1.0)
        } else if ground {
            0.0
        } else {
            let flight_path = (input.vertical_fpm * 0.00508)
                .atan2(input.knots.max(1.0) * 0.514_444)
                .to_degrees();
            let bias = match input.stage {
                Stage::Approach => h.approach_pitch_bias_degrees,
                Stage::Climb | Stage::MissedApproach => h.climb_pitch_bias_degrees,
                _ => 0.0,
            };
            (flight_path + bias).clamp(-10.0, 15.0)
        };
        self.bank = approach(self.bank, bank, h.bank_rate_degrees_sec);
        self.pitch = approach(self.pitch, pitch, h.pitch_rate_degrees_sec);
        let gear = input.fixed_gear || ground || input.stage == Stage::Approach;
        // A generated aircraft that spawns on the ground has its gear down. Lerping from the
        // default would show it taxiing gear-up for several seconds.
        self.gear = if ground {
            1.0
        } else {
            approach(self.gear, f64::from(gear), 1.0 / h.gear_seconds)
        };
        let flaps = match input.stage {
            Stage::LineUp | Stage::Takeoff | Stage::MissedApproach => h.takeoff_flaps,
            Stage::Approach => h.landing_flaps,
            Stage::Rollout => self.flaps,
            _ => 0.0,
        };
        self.flaps = approach(self.flaps, flaps, 1.0 / h.flap_seconds);
        self.engine = approach(
            self.engine,
            f64::from(input.engines_running),
            1.0 / h.engine_spool_seconds,
        );
        let direction = if input.stage == Stage::Pushback {
            -1.0
        } else {
            1.0
        };
        let signed_speed = input.knots.max(0.0) * 0.514_444 * direction;
        let steering = if ground && input.knots > 0.5 {
            (h.wheelbase_meters * input.turn_degrees_sec.to_radians() / signed_speed)
                .atan()
                .to_degrees()
                .clamp(-h.maximum_steering_degrees, h.maximum_steering_degrees)
        } else {
            0.0
        };
        self.steering = approach(self.steering, steering, h.maximum_steering_degrees);
        self.wheel_compression =
            approach(self.wheel_compression, if ground { 0.5 } else { 0.0 }, 2.0);
        self.speed_brakes = approach(
            self.speed_brakes,
            f64::from(input.stage == Stage::Rollout && h.rollout_spoilers),
            1.0,
        );
        self.touchdown = if ground && self.was_airborne && input.stage == Stage::Rollout {
            h.touchdown_seconds
        } else {
            (self.touchdown - seconds).max(0.0)
        };
        self.was_airborne = !ground;
        self.propeller_rpm = h.propeller_rpm * self.engine;
        self.propeller_angle = (self.propeller_angle
            + h.propeller_rpm * self.engine * 6.0 * seconds)
            .rem_euclid(360.0);
        if ground {
            self.wheel_angle = (self.wheel_angle
                + (signed_speed * seconds / h.wheel_radius_meters).to_degrees())
            .rem_euclid(360.0);
        }
        self.landing_lights = input.landing_lights;
        self.beacon = input.engines_running;
        self.navigation_lights = !matches!(input.stage, Stage::Parked | Stage::Turnaround);
        self.strobes = !ground
            || matches!(
                input.stage,
                Stage::LineUp | Stage::Crossing | Stage::Takeoff | Stage::Rollout
            );
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    #[test]
    fn legacy_handling_profiles_inherit_editable_flare_defaults() {
        let source = include_str!("../../../../traffic/handling.toml");
        let legacy = source
            .lines()
            .filter(|line| {
                !line.starts_with("flare_height_feet") && !line.starts_with("touchdown_sink_fpm")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let handling = parse(&legacy).unwrap();
        assert_eq!(handling[&Kind::Piston].flare_height_feet, 10.0);
        assert_eq!(handling[&Kind::HeavyJet].flare_height_feet, 40.0);
        assert!(
            parse(&source.replacen("flare_height_feet = 10.0", "flare_height_feet = 0.0", 1))
                .is_err()
        );
    }

    #[test]
    fn pushback_reverses_wheels_and_steering_for_the_same_body_turn() {
        let handling = builtin().unwrap();
        let profile = &handling[&Kind::Jet];
        let movement = Input {
            stage: Stage::TaxiOut,
            knots: 3.0,
            turn_degrees_sec: 2.0,
            vertical_fpm: 0.0,
            engines_running: true,
            fixed_gear: false,
            landing_lights: false,
        };
        let mut forward = Animation::default();
        let mut reverse = Animation::default();
        forward.advance(movement, profile, 0.1);
        reverse.advance(
            Input {
                stage: Stage::Pushback,
                ..movement
            },
            profile,
            0.1,
        );
        assert!(forward.steering > 0.0);
        assert!(reverse.steering < 0.0);
        assert!((forward.wheel_angle + reverse.wheel_angle - 360.0).abs() < 1e-6);
        assert_eq!(forward.bank, 0.0);
        assert_eq!(reverse.bank, 0.0);
    }
    #[test]
    fn aircraft_classes_and_continuous_animation_are_distinct() {
        let handling = builtin().unwrap();
        assert!(handling[&Kind::Piston].rotation_knots < handling[&Kind::HeavyJet].rotation_knots);
        assert!(handling[&Kind::Piston].propeller_rpm > 0.0);
        assert_eq!(handling[&Kind::Jet].propeller_rpm, 0.0);
        let jet = &handling[&Kind::Jet];
        let mut rotation = Animation::default();
        let ground = Input {
            stage: Stage::Takeoff,
            knots: jet.rotation_knots,
            turn_degrees_sec: 0.0,
            vertical_fpm: 0.0,
            engines_running: true,
            fixed_gear: false,
            landing_lights: true,
        };
        rotation.advance(ground, jet, 1.0);
        assert!(rotation.pitch > 0.0 && rotation.pitch <= jet.pitch_rate_degrees_sec);
        rotation.advance(
            Input {
                stage: Stage::Rollout,
                ..ground
            },
            jet,
            1.0,
        );
        assert_eq!(rotation.pitch, 0.0);
        let input = Input {
            stage: Stage::Climb,
            knots: 180.0,
            turn_degrees_sec: 2.0,
            vertical_fpm: 1500.0,
            engines_running: true,
            fixed_gear: false,
            landing_lights: true,
        };
        let mut a = Animation::default();
        a.advance(input, &handling[&Kind::Jet], 0.5);
        assert!(a.gear > 0.0 && a.gear < 1.0);
        assert!(a.bank > 0.0 && a.bank <= 2.5);
        assert_eq!(a.propeller_angle, 0.0);
        let mut fixed = Animation::default();
        fixed.advance(
            Input {
                fixed_gear: true,
                ..input
            },
            &handling[&Kind::Piston],
            0.5,
        );
        assert_eq!(fixed.gear, 1.0);
        assert!(fixed.propeller_angle > 0.0);
        let before = serde_json::to_value(fixed).unwrap();
        fixed.advance(input, &handling[&Kind::Piston], 0.0);
        assert_eq!(serde_json::to_value(fixed).unwrap(), before);
    }
    #[test]
    fn steering_and_touchdown_follow_actual_movement() {
        let handling = builtin().unwrap();
        let mut animation = Animation::default();
        let taxi = Input {
            stage: Stage::TaxiOut,
            knots: 10.0,
            turn_degrees_sec: 8.0,
            vertical_fpm: 0.0,
            engines_running: true,
            fixed_gear: false,
            landing_lights: false,
        };
        animation.advance(taxi, &handling[&Kind::Jet], 0.5);
        assert!(animation.channel(Channel::Steering) > 0.0);
        assert!(animation.channel(Channel::WheelCompression) > 0.0);
        assert_eq!(animation.channel(Channel::Touchdown), 0.0);
        let approach = Input {
            stage: Stage::Approach,
            knots: 140.0,
            turn_degrees_sec: 0.0,
            vertical_fpm: -700.0,
            landing_lights: true,
            ..taxi
        };
        animation.advance(approach, &handling[&Kind::Jet], 0.5);
        assert_eq!(animation.wheel_compression, 0.0);
        animation.advance(
            Input {
                stage: Stage::Rollout,
                vertical_fpm: 0.0,
                ..approach
            },
            &handling[&Kind::Jet],
            0.5,
        );
        assert_eq!(animation.channel(Channel::Touchdown), 1.0);
        assert!(animation.speed_brakes > 0.0);
        animation.advance(
            Input {
                stage: Stage::Rollout,
                ..taxi
            },
            &handling[&Kind::Jet],
            0.5,
        );
        assert_eq!(animation.channel(Channel::Touchdown), 0.0);
        let mut piston = Animation::default();
        piston.advance(
            Input {
                stage: Stage::Rollout,
                ..taxi
            },
            &handling[&Kind::Piston],
            0.5,
        );
        assert_eq!(piston.speed_brakes, 0.0);
    }
}

/// Model channel semantics. Names remain editable in the channel catalogue.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// Gear extension.
    Gear,
    /// Flap/slat extension.
    Flaps,
    /// Beacon light.
    Beacon,
    /// Landing light.
    LandingLights,
    /// Navigation light.
    NavigationLights,
    /// Strobe light.
    Strobes,
    /// Tyre phase.
    WheelAngle,
    /// Propeller phase.
    PropellerAngle,
    /// Propeller speed in revolutions per minute.
    PropellerRpm,
    /// Gear-door transit curve.
    GearDoors,
    /// Generated aircraft have a simulated pilot assigned.
    PilotPresent,
    /// Spoilers, supplied by rollout state.
    SpeedBrakes,
    /// Nose-wheel steering ratio.
    Steering,
    /// Ground contact compression.
    WheelCompression,
    /// Short touchdown signal.
    Touchdown,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelFile {
    version: u32,
    channels: BTreeMap<String, Channel>,
}
/// Read bounded model channel names before creating instances.
pub fn channels(text: &str) -> Result<BTreeMap<String, Channel>, String> {
    if text.len() > 65536 {
        return Err("Animation channels exceed 64 KiB".into());
    }
    let file: ChannelFile =
        crate::configuration::from_str(text).map_err(|error| error.to_string())?;
    if file.version != 1
        || file.channels.is_empty()
        || file.channels.len() > 64
        || file.channels.keys().any(|name| {
            !(name.starts_with("libxplanemp/") || name.starts_with("cjs/world_traffic/"))
                || name.len() > 160
                || name.contains('\0')
        })
    {
        return Err("Animation channel catalogue has invalid version, count or names".into());
    }
    Ok(file.channels)
}
impl Animation {
    /// Instance values are separate for every aircraft, never global mutable state.
    #[must_use]
    pub fn channel(self, channel: Channel) -> f32 {
        (match channel {
            Channel::Gear => self.gear,
            Channel::Flaps => self.flaps,
            Channel::Beacon => f64::from(self.beacon),
            Channel::LandingLights => f64::from(self.landing_lights),
            Channel::NavigationLights => f64::from(self.navigation_lights),
            Channel::Strobes => f64::from(self.strobes),
            Channel::WheelAngle => self.wheel_angle,
            Channel::PropellerAngle => self.propeller_angle,
            Channel::PropellerRpm => self.propeller_rpm,
            Channel::GearDoors => (self.gear * std::f64::consts::PI).sin().max(0.0),
            Channel::PilotPresent => 1.0,
            Channel::SpeedBrakes => self.speed_brakes,
            Channel::Steering => self.steering,
            Channel::WheelCompression => self.wheel_compression,
            Channel::Touchdown => f64::from(self.touchdown > 0.0),
        }) as f32
    }
}
