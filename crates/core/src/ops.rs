//! Flight phases, request eligibility, plan validation and navigation helpers.

use super::airport::{Airport, airport_point};
use super::state::{FlightPlan, PhaseCode, State, Telemetry};

/// Display name for a stage. `Arrival` and `TaxiIn` display as
/// Descent and Taxi to parking.
#[must_use]
pub fn phase_name(phase: PhaseCode) -> String {
    match phase {
        PhaseCode::Parked => "Parked",
        PhaseCode::Clearance => "Clearance",
        PhaseCode::Taxi => "Taxi",
        PhaseCode::Departure => "Departure",
        PhaseCode::Cruise => "Cruise",
        PhaseCode::Arrival => "Descent",
        PhaseCode::Approach => "Approach",
        PhaseCode::Landed => "Landed",
        PhaseCode::Pushback => "Pushback",
        PhaseCode::TaxiIn => "Taxi to parking",
        PhaseCode::Finished => "Finished",
    }
    .to_owned()
}

/// Whether an intent is available in the current state.
/// Check whether the request is available in the current flight phase.
#[must_use]
pub fn request_available(state: &State, intent: &str) -> bool {
    if intent == "radio_check" || intent == "emergency" || intent == "position" {
        return true;
    }
    if intent == "repeat" || intent == "standby" || intent == "unable" {
        return !state.transcript.is_empty();
    }
    if intent == "readback" {
        return state.taxi_clearance.pending_readback
            || state
                .clearance
                .as_ref()
                .is_some_and(|clearance| !clearance.acknowledged);
    }
    ground_request_available(state, intent)
        .or_else(|| airborne_request_available(state, intent))
        .unwrap_or(false)
}

/// Ground-workflow half of `request_available`; `None` means "not a ground intent".
fn ground_request_available(state: &State, intent: &str) -> Option<bool> {
    let airborne = !state.telemetry.on_ground;
    let pending = state
        .clearance
        .as_ref()
        .is_some_and(|clearance| !clearance.acknowledged);
    if intent == "clearance" {
        return Some(
            !airborne
                && state.phase == PhaseCode::Parked
                && state.telemetry.ground_speed_knots < 1.0,
        );
    }
    if ["pushback", "start", "start_pushback"].contains(&intent) {
        return Some(
            !airborne
                && (state.phase == PhaseCode::Clearance
                    || (intent == "start" && state.phase == PhaseCode::Pushback))
                && state.clearance.is_some()
                && !pending,
        );
    }
    if intent == "backtrack" {
        return Some(
            !airborne
                && !pending
                && state.clearance.is_some()
                && state.telemetry.ground_speed_knots < 2.0
                && (state.phase == PhaseCode::Clearance
                    || state.phase == PhaseCode::Pushback
                    || (state.phase == PhaseCode::Taxi
                        && state.taxi_clearance.approved
                        && !state.taxi_clearance.pending_readback
                        && state.taxi_clearance.guidance_complete)),
        );
    }
    if intent == "taxi" {
        return Some(
            !airborne
                && (state.phase == PhaseCode::Clearance
                    || state.phase == PhaseCode::Pushback
                    || (state.phase == PhaseCode::Taxi && state.taxi_clearance.guidance_complete))
                && state.clearance.is_some()
                && !pending,
        );
    }
    if intent == "ready" {
        return Some(ready_available(state));
    }
    if intent == "landing" {
        return Some(airborne && matches!(state.phase, PhaseCode::Arrival | PhaseCode::Approach));
    }
    if intent == "gate" {
        return Some(
            !airborne
                && state.phase == PhaseCode::Landed
                && state.telemetry.ground_speed_knots < 30.0,
        );
    }
    if intent == "progressive" || intent == "cross_runway" {
        return Some(
            !airborne
                && (state.phase == PhaseCode::Taxi || state.phase == PhaseCode::TaxiIn)
                && (intent == "progressive" || state.taxi_clearance.guidance_complete),
        );
    }
    if intent == "altimeter" || intent == "weather" || intent == "frequency" || intent == "checkin"
    {
        return Some(state.phase != PhaseCode::Finished);
    }
    None
}

/// Departure-readiness check used by `request_available`: approved taxi route,
/// stopped at its end (demo scenes skip the position check).
fn ready_available(state: &State) -> bool {
    if !state.taxi_clearance.crossing_runway.is_empty()
        || (state.taxi_clearance.runway_taxi && !state.taxi_clearance.guidance_complete)
        || state.taxi_clearance.backtrack_required
        || (!state.taxi_clearance.hold_short_runway.is_empty()
            && state.taxi_clearance.hold_short_runway != state.plan.runway)
        || !state.telemetry.on_ground
        || state.phase != PhaseCode::Taxi
        || !state.taxi_clearance.approved
        || state.taxi_clearance.points.is_empty()
    {
        return false;
    }
    if state.demo {
        return true;
    }
    if !state.telemetry.position_valid || state.telemetry.ground_speed_knots > 5.0 {
        return false;
    }
    let reference = Airport {
        reference_latitude: state.taxi_clearance.reference_latitude,
        reference_longitude: state.taxi_clearance.reference_longitude,
        ..Default::default()
    };
    let position = airport_point(
        &reference,
        state.telemetry.latitude,
        state.telemetry.longitude,
    );
    let end = state
        .taxi_clearance
        .points
        .last()
        .copied()
        .unwrap_or_default();
    (position.east - end.east).hypot(position.north - end.north) < 180.0
}

/// Airborne half of `request_available`; `None` means "not an airborne intent".
fn airborne_request_available(state: &State, intent: &str) -> Option<bool> {
    let airborne = !state.telemetry.on_ground;
    if intent == "go_around" || intent == "visual" || intent == "localizer" {
        return Some(airborne && state.phase == PhaseCode::Approach);
    }
    if intent == "approach" || intent == "runway" {
        return Some(
            airborne && (state.phase == PhaseCode::Arrival || state.phase == PhaseCode::Approach),
        );
    }
    if intent == "altitude"
        || intent == "direct"
        || intent == "route"
        || intent == "deviation"
        || intent == "hold"
        || intent == "speed"
        || intent == "divert"
        || intent == "cancel_ifr"
    {
        return Some(airborne && state.clearance.is_some());
    }
    if intent == "descent" {
        return Some(
            airborne
                && state.clearance.is_some()
                && (state.phase == PhaseCode::Cruise || state.phase == PhaseCode::Arrival),
        );
    }
    if intent == "flight_following"
        || intent == "transit"
        || intent == "circuits"
        || intent == "bearing"
    {
        return Some(airborne);
    }
    None
}

/// Update the flight phase after four consistent telemetry ticks.
/// Reject invalid telemetry before changing state.
pub fn update_flight_phase(state: &mut State, telemetry: &Telemetry) -> Result<(), String> {
    if !telemetry.latitude.is_finite()
        || !telemetry.longitude.is_finite()
        || !telemetry.altitude_feet.is_finite()
        || !telemetry.vertical_speed_fpm.is_finite()
        || !telemetry.ground_speed_knots.is_finite()
        || telemetry.latitude.abs() > 90.0
        || telemetry.longitude.abs() > 180.0
        || telemetry.ground_speed_knots < 0.0
    {
        return Err("Invalid telemetry".to_owned());
    }
    state.telemetry = telemetry.clone();
    if telemetry.paused {
        return Ok(());
    }
    if state.taxi_clearance.approved
        && !state.taxi_clearance.guidance_complete
        && telemetry.position_valid
        && telemetry.on_ground
    {
        let reference = Airport {
            reference_latitude: state.taxi_clearance.reference_latitude,
            reference_longitude: state.taxi_clearance.reference_longitude,
            ..Default::default()
        };
        let here = airport_point(&reference, telemetry.latitude, telemetry.longitude);
        if let Some(end) = state.taxi_clearance.points.last()
            && telemetry.ground_speed_knots < 2.0
            && (here.east - end.east).hypot(here.north - end.north)
                <= if state.taxi_clearance.crossing_runway.is_empty() {
                    crate::airport::HOLD_POINT_RADIUS_METRES
                } else {
                    10.0
                }
        {
            state.taxi_clearance.guidance_complete = true;
        }
    }
    let mut candidate = state.phase;
    if !telemetry.on_ground {
        state.has_departed = true;
        state.taxi_clearance.approved = false;
        if matches!(
            state.phase,
            PhaseCode::Parked
                | PhaseCode::Clearance
                | PhaseCode::Pushback
                | PhaseCode::Taxi
                | PhaseCode::Landed
                | PhaseCode::TaxiIn
                | PhaseCode::Finished
        ) {
            candidate = PhaseCode::Departure;
        }
        if state.phase == PhaseCode::Departure
            && (telemetry.altitude_feet - f64::from(state.plan.cruise_feet)).abs() < 1000.0
            && telemetry.vertical_speed_fpm.abs() < 400.0
        {
            candidate = PhaseCode::Cruise;
        }
        if state.phase == PhaseCode::Cruise && telemetry.vertical_speed_fpm < -500.0 {
            candidate = PhaseCode::Arrival;
        }
        if state.phase == PhaseCode::Arrival
            && telemetry.height_agl_feet < 3000.0
            && telemetry.vertical_speed_fpm < 200.0
        {
            candidate = PhaseCode::Approach;
        }
        if state.phase == PhaseCode::Approach && telemetry.vertical_speed_fpm > 700.0 {
            candidate = PhaseCode::Departure;
        }
    } else if state.has_departed
        && state.phase != PhaseCode::Landed
        && state.phase != PhaseCode::TaxiIn
        && state.phase != PhaseCode::Finished
    {
        candidate = PhaseCode::Landed;
    }
    if candidate == state.phase {
        state.phase_evidence = 0;
        state.candidate_phase = candidate;
        return Ok(());
    }
    if candidate != state.candidate_phase {
        state.candidate_phase = candidate;
        state.phase_evidence = 0;
    }
    if state.phase_evidence + 1 >= 4 {
        state.phase = candidate;
        state.phase_evidence = 0;
        if candidate == PhaseCode::Finished {
            state.taxi_clearance.approved = false;
        }
    } else {
        state.phase_evidence += 1;
    }
    Ok(())
}

/// Validate the flight plan fields.
pub fn validate_flight_plan(plan: &FlightPlan) -> Result<(), String> {
    let airport_code = regex::Regex::new("[A-Z0-9]{4}")
        .map_err(|_| crate::dialogue::say("plan_check_icao_codes_callsign_and_initial", &[]))?;
    let code_ok = |code: &str| {
        airport_code
            .find(code)
            .is_some_and(|found| found.as_str() == code)
    };
    if !code_ok(&plan.departure)
        || !code_ok(&plan.destination)
        || (!plan.alternate.is_empty() && !code_ok(&plan.alternate))
        || plan.callsign.is_empty()
        || plan.callsign.len() > 30
        || plan.cruise_feet < 1000
        || plan.cruise_feet > 45000
        || plan.initial_altitude_feet < 1000
        || plan.initial_altitude_feet > plan.cruise_feet
    {
        return Err(crate::dialogue::say(
            "plan_check_icao_codes_callsign_and_initial",
            &[],
        ));
    }
    if plan.passengers < 0 || plan.passengers > 1000 || plan.cost_index < 0 || plan.cost_index > 999
    {
        return Err(crate::dialogue::say(
            "plan_passengers_or_cost_index_is_outside",
            &[],
        ));
    }
    for value in [
        plan.block_fuel_kg,
        plan.trip_fuel_kg,
        plan.reserve_fuel_kg,
        plan.alternate_fuel_kg,
        plan.taxi_fuel_kg,
        plan.payload_kg,
        plan.cargo_kg,
        plan.zero_fuel_weight_kg,
        plan.takeoff_weight_kg,
        plan.landing_weight_kg,
        plan.estimated_minutes,
    ] {
        if !value.is_finite() || value < 0.0 {
            return Err(crate::dialogue::say(
                "plan_weights_fuel_and_time_must_be",
                &[],
            ));
        }
    }
    if plan.block_fuel_kg > 0.0
        && plan.block_fuel_kg
            < plan.trip_fuel_kg + plan.reserve_fuel_kg + plan.alternate_fuel_kg + plan.taxi_fuel_kg
    {
        return Err(crate::dialogue::say(
            "plan_block_fuel_is_less_than_trip",
            &[],
        ));
    }
    for fix in &plan.fixes {
        if !fix.latitude.is_finite()
            || !fix.longitude.is_finite()
            || !fix.altitude_feet.is_finite()
            || fix.latitude.abs() > 90.0
            || fix.longitude.abs() > 180.0
        {
            return Err(crate::dialogue::say("plan_invalid_route_coordinate", &[]));
        }
    }
    Ok(())
}

/// Great-circle distance in nautical miles (haversine, 3440.065 NM radius).
#[must_use]
pub fn distance_nm(
    first_latitude: f64,
    first_longitude: f64,
    second_latitude: f64,
    second_longitude: f64,
) -> f64 {
    const RADIANS: f64 = std::f64::consts::PI / 180.0;
    let latitude_delta = (second_latitude - first_latitude) * RADIANS;
    let longitude_delta = (second_longitude - first_longitude) * RADIANS;
    let half = (latitude_delta / 2.0).sin().powi(2)
        + (first_latitude * RADIANS).cos()
            * (second_latitude * RADIANS).cos()
            * (longitude_delta / 2.0).sin().powi(2);
    3440.065 * 2.0 * half.clamp(0.0, 1.0).sqrt().asin()
}

/// Calculate descent distance for a constant-angle profile.
pub fn descent_distance_nm(
    altitude_feet: f64,
    target_feet: f64,
    angle_degrees: f64,
) -> Result<f64, String> {
    if !altitude_feet.is_finite()
        || !target_feet.is_finite()
        || !angle_degrees.is_finite()
        || angle_degrees <= 0.0
        || angle_degrees >= 15.0
    {
        return Err("Invalid descent profile input".to_owned());
    }
    Ok((altitude_feet - target_feet).max(0.0)
        / (6076.11549 * (angle_degrees * std::f64::consts::PI / 180.0).tan()))
}

/// ATC service for the current flight stage.
#[must_use]
pub fn controller_service(state: &State) -> String {
    if state.phase == PhaseCode::Parked {
        "Clearance".to_owned()
    } else if state.telemetry.on_ground {
        if state.phase == PhaseCode::Taxi || state.phase == PhaseCode::Departure {
            "Tower".to_owned()
        } else {
            "Ground".to_owned()
        }
    } else if state.phase == PhaseCode::Departure {
        "Departure".to_owned()
    } else {
        "Approach".to_owned()
    }
}

/// Controller assignment key in ICAO:Service format.
#[must_use]
pub fn controller_airspace(state: &State, airport_icao: &str) -> String {
    let icao = if airport_icao.is_empty() {
        let arrival = state.phase == PhaseCode::Arrival
            || state.phase == PhaseCode::Approach
            || state.phase == PhaseCode::Landed
            || state.phase == PhaseCode::TaxiIn;
        if arrival {
            state.plan.destination.clone()
        } else {
            state.plan.departure.clone()
        }
    } else {
        airport_icao.into()
    };
    format!("{icao}:{}", controller_service(state))
}

/// Facility covering the flight right now, if any.
pub struct Coverage {
    /// Station service (Ground, Tower, Approach, Center, ...).
    pub service: String,
    /// Frequency kHz.
    pub khz: i32,
    /// Station name.
    pub name: String,
}

/// Range in nm inside which a loaded airport still answers.
pub const COVERAGE_RANGE_NM: f64 = 100.0;

/// Which facility should answer: the expected airport's station for the
/// phase, else any loaded airport in range, else the region center, else
/// nobody. Mirrors the service mapping of the frequency intent.
#[must_use]
pub fn covering_facility(
    state: &State,
    airport: Option<&Airport>,
    region: &super::regions::Region,
) -> Option<Coverage> {
    let expected = if matches!(
        state.phase,
        PhaseCode::Arrival | PhaseCode::Approach | PhaseCode::Landed | PhaseCode::TaxiIn
    ) {
        state.plan.destination.clone()
    } else {
        state.plan.departure.clone()
    };
    let service = if state.phase == PhaseCode::Parked {
        "Clearance"
    } else if state.telemetry.on_ground {
        if state.phase == PhaseCode::Taxi || state.phase == PhaseCode::Departure {
            "Tower"
        } else {
            "Ground"
        }
    } else if state.phase == PhaseCode::Departure {
        "Departure"
    } else {
        "Approach"
    };
    if let Some(airport) = airport {
        // The expected airport answers on its published station. Without a
        // valid position the flight is assumed to sit at that airport; with
        // one, it must be within range.
        let matching = airport.icao == expected;
        let positioned = state.telemetry.position_valid;
        let range = distance_nm(
            state.telemetry.latitude,
            state.telemetry.longitude,
            airport.reference_latitude,
            airport.reference_longitude,
        );
        let in_range = positioned && range <= COVERAGE_RANGE_NM;
        if (matching && !positioned) || in_range {
            for frequency in &airport.frequencies {
                if frequency.service == service {
                    return Some(Coverage {
                        service: service.to_owned(),
                        khz: frequency.khz,
                        name: frequency.name.clone(),
                    });
                }
            }
        }
    }
    region.center_khz.map(|khz| Coverage {
        service: "Center".to_owned(),
        khz,
        name: format!("{} Center", region.name),
    })
}

/// Split a comma voice pool, trimming entries and dropping empties.
#[must_use]
pub fn parse_voice_pool(pool: &str) -> Vec<String> {
    pool.split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(std::string::ToString::to_string)
        .collect()
}

/// English Kokoro voices and the supported provider aliases.
#[must_use]
pub fn english_voice(voice: &str) -> bool {
    matches!(
        voice,
        "alloy"
            | "echo"
            | "fable"
            | "onyx"
            | "nova"
            | "shimmer"
            | "af_alloy"
            | "af_aoede"
            | "af_bella"
            | "af_heart"
            | "af_jessica"
            | "af_kore"
            | "af_nicole"
            | "af_nova"
            | "af_river"
            | "af_sarah"
            | "af_sky"
            | "am_adam"
            | "am_echo"
            | "am_eric"
            | "am_fenrir"
            | "am_liam"
            | "am_michael"
            | "am_onyx"
            | "am_puck"
            | "am_santa"
            | "bf_alice"
            | "bf_emma"
            | "bf_isabella"
            | "bf_lily"
            | "bm_daniel"
            | "bm_fable"
            | "bm_george"
            | "bm_lewis"
    )
}

/// Treat provider aliases and their Kokoro voices as the same speaker.
#[must_use]
pub fn voice_identity(voice: &str) -> &str {
    match voice {
        "alloy" => "af_alloy",
        "echo" => "am_echo",
        "fable" => "bm_fable",
        "onyx" => "am_onyx",
        "nova" => "af_nova",
        "shimmer" => "af_sky",
        _ => voice,
    }
}

/// Empty or non-English pools use a varied English default.
#[must_use]
pub fn english_voice_pool(pool: &str) -> Vec<String> {
    let mut voices = Vec::new();
    for voice in parse_voice_pool(pool)
        .into_iter()
        .filter(|v| english_voice(v))
    {
        if !voices
            .iter()
            .any(|v: &String| voice_identity(v) == voice_identity(&voice))
        {
            voices.push(voice);
        }
    }
    if voices.is_empty() {
        voices = parse_voice_pool("alloy, echo, fable, onyx, nova, shimmer");
    }
    voices
}

/// Speech pacing preset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeliveryPreset {
    /// Speed multiplier.
    pub speed: f32,
    /// Sentence pause seconds.
    pub sentence_pause: f32,
    /// Clause pause seconds.
    pub clause_pause: f32,
}

/// Delivery preset by name: brisk, urgent, anything else standard.
#[must_use]
pub fn delivery_preset(name: &str) -> DeliveryPreset {
    match name {
        "brisk" => DeliveryPreset {
            speed: 1.1,
            sentence_pause: 0.12,
            clause_pause: 0.05,
        },
        "urgent" => DeliveryPreset {
            speed: 1.22,
            sentence_pause: 0.06,
            clause_pause: 0.02,
        },
        _ => DeliveryPreset {
            speed: 1.0,
            sentence_pause: 0.25,
            clause_pause: 0.1,
        },
    }
}

/// Whether a hazard advisory was already given.
#[must_use]
pub fn advisory_known(state: &State, station: &str, hazard: &str) -> bool {
    state
        .weather_advisories
        .iter()
        .any(|advisory| advisory.station == station && advisory.hazard == hazard)
}

/// Remember a hazard advisory (once-only).
pub fn remember_advisory(state: &mut State, station: &str, hazard: &str, observed: f64) {
    if advisory_known(state, station, hazard) {
        return;
    }
    state
        .weather_advisories
        .push(super::state::WeatherAdvisory {
            station: station.to_owned(),
            hazard: hazard.to_owned(),
            observed,
        });
}

/// Forget a hazard advisory so it may fire again.
pub fn clear_advisory(state: &mut State, station: &str, hazard: &str) {
    state
        .weather_advisories
        .retain(|advisory| !(advisory.station == station && advisory.hazard == hazard));
}

/// Read aircraft author + ICAO from an `.acf` file.
/// Read aircraft identity fields from an ACF file.
pub fn read_acf_identity(path: &std::path::Path) -> Result<(String, String), String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("cannot open file: {error}"))?;
    let mut author = String::new();
    let mut icao = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("P acf/_author ") {
            rest.clone_into(&mut author);
        } else if let Some(rest) = line.strip_prefix("P acf/_ICAO ") {
            rest.clone_into(&mut icao);
        }
        if !author.is_empty() && !icao.is_empty() {
            break;
        }
    }
    if author.is_empty() && icao.is_empty() {
        return Err("no aircraft identity found".to_owned());
    }
    Ok((author, icao))
}

#[cfg(test)]
mod tests {
    use super::super::state::{FlightPlan, PhaseCode, State, Telemetry};
    use super::*;
    use std::path::PathBuf;

    fn fixtures() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
    }

    fn phase(code: i64) -> PhaseCode {
        match code {
            1 => PhaseCode::Clearance,
            2 => PhaseCode::Taxi,
            3 => PhaseCode::Departure,
            4 => PhaseCode::Cruise,
            5 => PhaseCode::Arrival,
            6 => PhaseCode::Approach,
            7 => PhaseCode::Landed,
            8 => PhaseCode::Pushback,
            9 => PhaseCode::TaxiIn,
            10 => PhaseCode::Finished,
            _ => PhaseCode::Parked,
        }
    }

    fn close(actual: f64, expected: f64) -> bool {
        (actual - expected).abs() <= 1e-9 * expected.abs().max(1.0)
    }

    /// Load the shared differential fixtures.
    ///
    /// # Panics
    ///
    /// Panics when the fixture file is missing or unparsable; tests fail then.
    fn ops_fixtures() -> serde_json::Value {
        let dir = fixtures();
        serde_json::from_str(&std::fs::read_to_string(dir.join("ops.json")).unwrap()).unwrap()
    }

    /// Read an integer fixture field.
    ///
    /// # Panics
    ///
    /// Panics when the field is missing or out of `i32` range; tests fail then.
    fn case_i32(case: &serde_json::Value, key: &str) -> i32 {
        i32::try_from(case[key].as_i64().unwrap()).unwrap()
    }

    fn covered_airport() -> super::super::airport::Airport {
        use super::super::airport::{Airport, Frequency};
        Airport {
            icao: "YMLT".to_owned(),
            reference_latitude: -41.0,
            reference_longitude: 146.0,
            frequencies: vec![Frequency {
                service: "Tower".to_owned(),
                name: "MlT Tower".to_owned(),
                khz: 118_100,
            }],
            ..Default::default()
        }
    }

    fn covered_region() -> super::super::regions::Region {
        super::super::regions::Region {
            name: "australia".to_owned(),
            pressure: "qnh".to_owned(),
            altitude: "feet".to_owned(),
            clearance: "sid".to_owned(),
            transition_feet: 10000,
            center_khz: None,
        }
    }

    #[test]
    fn coverage_matches_loaded_airport() {
        // Taxi phase on the ground wants Tower; the loaded airport has it.
        let mut state = State {
            phase: PhaseCode::Taxi,
            ..Default::default()
        };
        state.telemetry.on_ground = true;
        state.telemetry.latitude = -41.0;
        state.telemetry.longitude = 146.0;
        state.plan.departure = "YMLT".to_owned();
        let airport = covered_airport();
        let found = covering_facility(&state, Some(&airport), &covered_region()).unwrap();
        assert_eq!(found.service, "Tower");
        assert_eq!(found.khz, 118_100);
        // Far away with a valid position and no center defined, nobody answers.
        state.telemetry.latitude = 0.0;
        state.telemetry.longitude = 0.0;
        state.telemetry.position_valid = true;
        assert!(covering_facility(&state, Some(&airport), &covered_region()).is_none());
        // A region center covers the gap when defined.
        let mut region = covered_region();
        region.center_khz = Some(125_300);
        let found = covering_facility(&state, Some(&airport), &region).unwrap();
        assert_eq!(found.service, "Center");
        assert_eq!(found.khz, 125_300);
        // Cruise far from home wants Approach; the airport lacks it, so the
        // center answers.
        state.phase = PhaseCode::Cruise;
        state.telemetry.on_ground = false;
        let found = covering_facility(&state, Some(&airport), &region).unwrap();
        assert_eq!(found.service, "Center");
    }

    #[test]
    fn phase_names_match_fixtures() {
        let cases = ops_fixtures();
        let mut count = 0;
        for case in cases["phases"].as_array().unwrap() {
            assert_eq!(
                phase_name(phase(case["code"].as_i64().unwrap())),
                case["expected"].as_str().unwrap()
            );
            count += 1;
        }
        assert_eq!(count, 6, "phases run");
    }

    #[test]
    fn availability_matches_fixtures() {
        let cases = ops_fixtures();
        let mut count = 0;
        for case in cases["available"].as_array().unwrap() {
            let clearance = case
                .get("clearance")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
                .then(|| super::super::state::Clearance {
                    acknowledged: case
                        .get("acked")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    ..Default::default()
                });
            let state = State {
                phase: phase(case["phase"].as_i64().unwrap()),
                telemetry: Telemetry {
                    on_ground: case["onGround"].as_bool().unwrap(),
                    ground_speed_knots: case
                        .get("speed")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(0.0),
                    ..Default::default()
                },
                clearance,
                ..Default::default()
            };
            assert_eq!(
                request_available(&state, case["intent"].as_str().unwrap()),
                case["expected"].as_bool().unwrap(),
                "available {}",
                case["intent"].as_str().unwrap()
            );
            count += 1;
        }
        assert_eq!(count, 14, "availability run");
    }

    #[test]
    fn transitions_match_fixtures() {
        let cases = ops_fixtures();
        let mut count = 0;
        for case in cases["transitions"].as_array().unwrap() {
            let start = &case["start"];
            let mut state = State {
                phase: phase(start["phase"].as_i64().unwrap()),
                has_departed: start
                    .get("hasDeparted")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                ..Default::default()
            };
            let mut errored = false;
            for tick in case["ticks"].as_array().unwrap() {
                let telemetry = Telemetry {
                    on_ground: tick["onGround"].as_bool().unwrap(),
                    altitude_feet: tick
                        .get("alt")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(0.0),
                    ground_speed_knots: tick
                        .get("speed")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(0.0),
                    vertical_speed_fpm: tick
                        .get("vs")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(0.0),
                    height_agl_feet: tick
                        .get("agl")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(0.0),
                    paused: tick
                        .get("paused")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    ..Default::default()
                };
                if update_flight_phase(&mut state, &telemetry).is_err() {
                    errored = true;
                    break;
                }
            }
            if case
                .get("error")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                assert!(errored, "expected telemetry error");
            } else {
                assert_eq!(state.phase, phase(case["expected"].as_i64().unwrap()));
            }
            count += 1;
        }
        assert_eq!(count, 4, "transitions run");
    }

    #[test]
    fn validation_matches_fixtures() {
        let cases = ops_fixtures();
        let mut count = 0;
        for case in cases["validate"].as_array().unwrap() {
            let plan_json = &case["plan"];
            let plan = FlightPlan {
                departure: plan_json["departure"].as_str().unwrap().into(),
                destination: plan_json["destination"].as_str().unwrap().into(),
                callsign: plan_json["callsign"].as_str().unwrap().into(),
                cruise_feet: case_i32(plan_json, "cruiseFeet"),
                initial_altitude_feet: case_i32(plan_json, "initialAltitudeFeet"),
                trip_fuel_kg: plan_json
                    .get("tripFuelKg")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0),
                ..Default::default()
            };
            assert_eq!(
                validate_flight_plan(&plan).is_ok(),
                case["ok"].as_bool().unwrap(),
                "validate {}",
                case["name"].as_str().unwrap()
            );
            count += 1;
        }
        assert_eq!(count, 4, "validations run");
    }

    #[test]
    fn geometry_matches_fixtures() {
        let cases = ops_fixtures();
        let mut count = 0;
        for case in cases["geo"].as_array().unwrap() {
            let value = distance_nm(
                case["a"][0].as_f64().unwrap(),
                case["a"][1].as_f64().unwrap(),
                case["b"][0].as_f64().unwrap(),
                case["b"][1].as_f64().unwrap(),
            );
            assert!(close(value, case["expected"].as_f64().unwrap()), "geo");
            count += 1;
        }
        for case in cases["descent"].as_array().unwrap() {
            let result = descent_distance_nm(
                case["alt"].as_f64().unwrap(),
                case["target"].as_f64().unwrap(),
                case["angle"].as_f64().unwrap(),
            );
            if case
                .get("error")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                assert!(result.is_err());
            } else {
                assert!(close(result.unwrap(), case["expected"].as_f64().unwrap()));
            }
            count += 1;
        }
        assert_eq!(count, 7, "geometry run");
    }

    #[test]
    fn service_matches_fixtures() {
        let cases = ops_fixtures();
        let mut count = 0;
        for case in cases["service"].as_array().unwrap() {
            let state = State {
                phase: phase(case["phase"].as_i64().unwrap()),
                telemetry: Telemetry {
                    on_ground: case["onGround"].as_bool().unwrap(),
                    ..Default::default()
                },
                ..Default::default()
            };
            assert_eq!(
                controller_service(&state),
                case["expected"].as_str().unwrap()
            );
            count += 1;
        }
        assert_eq!(count, 5, "services run");
    }

    #[test]
    fn airspace_matches_fixtures() {
        let cases = ops_fixtures();
        let mut count = 0;
        for case in cases["airspace"].as_array().unwrap() {
            let state = State {
                phase: phase(case["phase"].as_i64().unwrap()),
                telemetry: Telemetry {
                    on_ground: case["onGround"].as_bool().unwrap(),
                    ..Default::default()
                },
                plan: FlightPlan {
                    departure: case["departure"].as_str().unwrap().into(),
                    destination: case["destination"].as_str().unwrap().into(),
                    ..Default::default()
                },
                ..Default::default()
            };
            assert_eq!(
                controller_airspace(&state, case["airport"].as_str().unwrap()),
                case["expected"].as_str().unwrap()
            );
            count += 1;
        }
        assert_eq!(count, 3, "airspaces run");
    }

    #[test]
    fn voices_match_fixtures() {
        let cases = ops_fixtures();
        let mut count = 0;
        for case in cases["voicePool"].as_array().unwrap() {
            let expected: Vec<String> = case["expected"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().into())
                .collect();
            assert_eq!(parse_voice_pool(case["input"].as_str().unwrap()), expected);
            count += 1;
        }
        for case in cases["delivery"].as_array().unwrap() {
            let preset = delivery_preset(case["name"].as_str().unwrap());
            let epsilon = 1e-6;
            assert!((f64::from(preset.speed) - case["speed"].as_f64().unwrap()).abs() < epsilon);
            assert!(
                (f64::from(preset.sentence_pause) - case["sentence"].as_f64().unwrap()).abs()
                    < epsilon
            );
            assert!(
                (f64::from(preset.clause_pause) - case["clause"].as_f64().unwrap()).abs() < epsilon
            );
            count += 1;
        }
        assert_eq!(count, 6, "voices run");
    }

    #[test]
    fn memory_matches_fixtures() {
        let cases = ops_fixtures();
        let case = &cases["memory"][0];
        let station = case["station"].as_str().unwrap();
        let hazard = case["hazard"].as_str().unwrap();
        let mut state = State::default();
        assert!(!advisory_known(&state, station, hazard));
        remember_advisory(&mut state, station, hazard, 1.0);
        assert_eq!(
            advisory_known(&state, station, hazard),
            case["knownAfterRemember"].as_bool().unwrap()
        );
        clear_advisory(&mut state, station, hazard);
        assert_eq!(
            advisory_known(&state, station, hazard),
            case["knownAfterClear"].as_bool().unwrap()
        );
    }

    #[test]
    fn acf_identity_matches_fixtures() {
        let cases = ops_fixtures();
        let path = std::env::temp_dir().join("openatc_acf_probe.acf");
        std::fs::write(&path, cases["acf"].as_str().unwrap()).unwrap();
        let (author, icao) = read_acf_identity(&path).unwrap();
        assert_eq!(author, cases["acfIdentity"]["author"].as_str().unwrap());
        assert_eq!(icao, cases["acfIdentity"]["icao"].as_str().unwrap());
        let _ = std::fs::remove_file(&path);
    }
}
