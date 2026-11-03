//! Flight phases, request eligibility, plan validation and navigation helpers.
use crate::aircraft::AircraftBehaviour;

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

/// Check whether the request is available in the current flight phase.
#[must_use]
pub fn request_available(state: &State, intent: &str) -> bool {
    if ["traffic_sighted", "traffic_not_sighted"].contains(&intent) {
        return !state.telemetry.on_ground
            && state.traffic_call.as_ref().is_some_and(|call| {
                Some(call.frequency_khz) == state.control_frequency() && !call.sighted
            });
    }
    if intent == "resume_course" {
        return !state.telemetry.on_ground && state.clearance.is_some();
    }
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
    if intent == "departure_runway" {
        return Some(
            !airborne
                && state.clearance.is_some()
                && !pending
                && !matches!(
                    state.phase,
                    PhaseCode::Departure
                        | PhaseCode::Landed
                        | PhaseCode::TaxiIn
                        | PhaseCode::Finished
                ),
        );
    }
    if intent == "arrival_runway" || intent == "change_approach" {
        return Some(airborne && state.clearance.is_some() && !pending);
    }
    if intent == "clearance" {
        return Some(
            !airborne
                && state.phase == PhaseCode::Parked
                && state.telemetry.ground_speed_knots < 1.0,
        );
    }
    if ["pushback", "start", "start_pushback"].contains(&intent) {
        let outstanding = match intent {
            "start" => !state.startup_approved && !state.telemetry.engines_running,
            "pushback" => !state.pushback_approved,
            _ => {
                !state.startup_approved
                    && !state.pushback_approved
                    && !state.telemetry.engines_running
            }
        };
        return Some(
            outstanding
                && !airborne
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
    for (field, value, min, max) in [
        ("latitude", Some(telemetry.latitude), -90.0, 90.0),
        ("longitude", Some(telemetry.longitude), -180.0, 180.0),
        (
            "altitudeFeet",
            Some(telemetry.altitude_feet),
            f64::NEG_INFINITY,
            f64::INFINITY,
        ),
        (
            "verticalSpeedFpm",
            Some(telemetry.vertical_speed_fpm),
            f64::NEG_INFINITY,
            f64::INFINITY,
        ),
        (
            "indicatedAltitudeFeet",
            telemetry.indicated_altitude_feet,
            f64::NEG_INFINITY,
            f64::INFINITY,
        ),
        (
            "pressureAltitudeFeet",
            telemetry.pressure_altitude_feet,
            f64::NEG_INFINITY,
            f64::INFINITY,
        ),
        (
            "groundSpeedKnots",
            Some(telemetry.ground_speed_knots),
            0.0,
            f64::INFINITY,
        ),
        (
            "headingDegrees",
            Some(telemetry.heading_degrees),
            f64::NEG_INFINITY,
            f64::INFINITY,
        ),
        (
            "heightAglFeet",
            Some(telemetry.height_agl_feet),
            f64::NEG_INFINITY,
            f64::INFINITY,
        ),
        ("zuluSeconds", telemetry.zulu_seconds, 0.0, 86_399.999_999),
        (
            "simulationSeconds",
            telemetry.simulation_seconds,
            0.0,
            f64::INFINITY,
        ),
        (
            "indicatedSpeedKnots",
            telemetry.indicated_speed_knots,
            0.0,
            f64::INFINITY,
        ),
        (
            "magneticHeadingDegrees",
            telemetry.magnetic_heading_degrees,
            f64::NEG_INFINITY,
            f64::INFINITY,
        ),
        (
            "groundTrackDegrees",
            telemetry.ground_track_degrees,
            f64::NEG_INFINITY,
            f64::INFINITY,
        ),
    ] {
        if let Some(value) = value
            && (!value.is_finite() || value < min || value > max)
        {
            return Err(format!("Invalid telemetry: {field} = {value}"));
        }
    }
    if telemetry
        .traffic
        .as_ref()
        .is_some_and(|targets| targets.len() > 63)
    {
        return Err("Invalid telemetry: traffic exceeds 63 targets".to_owned());
    }
    state.telemetry = telemetry.clone();
    if telemetry.paused {
        return Ok(());
    }
    if state.phase == PhaseCode::TaxiIn
        && state.taxi_clearance.approved
        && !state.taxi_clearance.parking_hold
        && (state.demo || crate::parking::endpoint_available(state, telemetry) == Some(true))
        && state.taxi_clearance.to_parking
        && state.taxi_clearance.hold_short_runway.is_empty()
        && state.taxi_clearance.crossing_runway.is_empty()
        && telemetry.on_ground
        && telemetry.position_valid
        && telemetry.ground_speed_knots < 0.5
    {
        let route = &state.taxi_clearance;
        let reference = Airport {
            reference_latitude: route.reference_latitude,
            reference_longitude: route.reference_longitude,
            ..Default::default()
        };
        let here = airport_point(&reference, telemetry.latitude, telemetry.longitude);
        if route
            .points
            .last()
            .is_some_and(|p| (p.east - here.east).hypot(p.north - here.north) <= 8.0)
        {
            state.on_event(crate::aircraft::AircraftEvent::Finished);
            state.taxi_clearance.guidance_complete = true;
            state.taxi_clearance.approved = false;
        }
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

/// Allow callsign edits while parked, before a clearance is issued.
#[must_use]
pub fn planning_callsign<'a>(state: &crate::state::State, proposed: &'a str) -> Option<&'a str> {
    let proposed = proposed.trim();
    (state.phase == crate::state::PhaseCode::Parked
        && state.telemetry.on_ground
        && state.clearance.is_none()
        && !proposed.is_empty()
        && proposed.len() <= 30
        && proposed
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b' '))
    .then_some(proposed)
}

/// Explain why a Planning draft cannot be submitted for IFR clearance.
#[must_use]
pub fn flight_plan_clearance_feedback(plan: &FlightPlan) -> Option<String> {
    let callsign = if plan.callsign.trim().is_empty() {
        "Aircraft"
    } else {
        plan.callsign.trim()
    };
    if plan.departure.trim().is_empty()
        && plan.destination.trim().is_empty()
        && plan.runway.trim().is_empty()
        && plan.fixes.is_empty()
        && (plan.route.trim().is_empty() || plan.route.trim().eq_ignore_ascii_case("DCT"))
    {
        return Some(crate::dialogue::say(
            "clearance_flight_plan_not_filed",
            &[("callsign", callsign.to_owned())],
        ));
    }
    let mut missing = Vec::new();
    for (name, value) in [
        ("callsign", plan.callsign.as_str()),
        ("departure airport", plan.departure.as_str()),
        ("destination airport", plan.destination.as_str()),
        ("departure runway", plan.runway.as_str()),
    ] {
        if value.trim().is_empty() {
            missing.push(name);
        }
    }
    if plan.route.trim().is_empty() && plan.fixes.is_empty() {
        missing.push("route");
    }
    let reason = if missing.is_empty() {
        validate_flight_plan(plan).err()?
    } else {
        crate::dialogue::say(
            "clearance_flight_plan_missing_fields",
            &[("fields", missing.join(", "))],
        )
    };
    Some(crate::dialogue::say(
        "clearance_flight_plan_incomplete",
        &[("callsign", callsign.to_owned()), ("reason", reason)],
    ))
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
            || fix
                .minimum_altitude_feet
                .is_some_and(|v| !(0..=60000).contains(&v))
            || fix
                .maximum_altitude_feet
                .is_some_and(|v| !(0..=60000).contains(&v))
            || matches!((fix.minimum_altitude_feet,fix.maximum_altitude_feet),(Some(a),Some(b)) if a > b)
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

/// Detailed aircraft specifications parsed from an ACF file.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcfSpecs {
    /// Aircraft author string.
    pub author: String,
    /// ICAO aircraft type designator.
    pub icao: String,
    /// Aircraft description/name.
    pub description: String,
    /// Wingspan in meters.
    pub wingspan_meters: Option<f64>,
    /// Length in meters.
    pub length_meters: Option<f64>,
    /// Height in meters.
    pub height_meters: Option<f64>,
    /// Maximum takeoff weight in kg.
    pub max_takeoff_weight_kg: Option<f64>,
    /// Maximum landing weight in kg.
    pub max_landing_weight_kg: Option<f64>,
    /// Maximum zero fuel weight in kg.
    pub max_zero_fuel_weight_kg: Option<f64>,
    /// Maximum fuel capacity in kg.
    pub max_fuel_kg: Option<f64>,
    /// Service ceiling in feet MSL.
    pub ceiling_feet: Option<i32>,
    /// Cruise speed in knots (TAS).
    pub cruise_speed_knots: Option<f64>,
    /// Maximum operating speed (Vmo) in knots.
    pub vmo_knots: Option<f64>,
    /// Maximum operating Mach number (Mmo).
    pub mmo: Option<f64>,
    /// Approach speed (Vref) in knots.
    pub vref_knots: Option<f64>,
    /// Stall speed in landing configuration in knots.
    pub vs0_knots: Option<f64>,
    /// Stall speed in clean configuration in knots.
    pub vs1_knots: Option<f64>,
    /// Maximum climb rate in fpm.
    pub max_climb_rate_fpm: Option<f64>,
    /// Maximum descent rate in fpm.
    pub max_descent_rate_fpm: Option<f64>,
    /// Engine type: "jet", "turboprop", "piston", "electric", "rotorcraft".
    pub engine_type: String,
    /// Number of engines.
    pub engine_count: Option<u8>,
    /// Wake turbulence category: L, M, H, J.
    pub wake_category: Option<char>,
    /// ICAO approach category: A, B, C, D, E.
    pub approach_category: Option<char>,
    /// Whether the aircraft has retractable gear.
    pub retractable_gear: bool,
    /// Whether the aircraft is a rotorcraft.
    pub is_rotorcraft: bool,
    /// Whether the aircraft is a seaplane/amphibian.
    pub is_seaplane: bool,
    /// Minimum runway length in meters (dry, sea level, ISA).
    pub min_runway_length_meters: Option<f64>,
    /// Typical turn-around time in minutes.
    pub turnaround_minutes: Option<u16>,
}

/// Parse detailed aircraft specifications from an ACF file.
pub fn read_acf_specs(path: &std::path::Path) -> Result<AcfSpecs, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("cannot open file: {error}"))?;

    let mut specs = AcfSpecs::default();
    let mut _in_aircraft_section = false;

    for line in text.lines() {
        let line = line.trim();

        if line == "ACF" || line == "AIRCRAFT" {
            _in_aircraft_section = true;
            continue;
        }

        if line.starts_with("P acf/_author ") {
            specs.author = line["P acf/_author ".len()..].trim().to_string();
        } else if line.starts_with("P acf/_ICAO ") {
            specs.icao = line["P acf/_ICAO ".len()..].trim().to_string().to_ascii_uppercase();
        } else if line.starts_with("P acf/_descrip ") {
            specs.description = line["P acf/_descrip ".len()..].trim().to_string();
        } else if line.starts_with("P acf/_wing_span ") {
            specs.wingspan_meters = parse_f64(&line["P acf/_wing_span ".len()..]);
        } else if line.starts_with("P acf/_length ") {
            specs.length_meters = parse_f64(&line["P acf/_length ".len()..]);
        } else if line.starts_with("P acf/_height ") {
            specs.height_meters = parse_f64(&line["P acf/_height ".len()..]);
        } else if line.starts_with("P acf/_max_takeoff_weight ") {
            specs.max_takeoff_weight_kg = parse_f64(&line["P acf/_max_takeoff_weight ".len()..]);
        } else if line.starts_with("P acf/_max_landing_weight ") {
            specs.max_landing_weight_kg = parse_f64(&line["P acf/_max_landing_weight ".len()..]);
        } else if line.starts_with("P acf/_max_zero_fuel_weight ") {
            specs.max_zero_fuel_weight_kg = parse_f64(&line["P acf/_max_zero_fuel_weight ".len()..]);
        } else if line.starts_with("P acf/_max_fuel ") {
            specs.max_fuel_kg = parse_f64(&line["P acf/_max_fuel ".len()..]);
        } else if line.starts_with("P acf/_max_atc_alt_ft ") {
            specs.ceiling_feet = parse_i32(&line["P acf/_max_atc_alt_ft ".len()..]);
        } else if line.starts_with("P acf/_ceiling ") {
            if specs.ceiling_feet.is_none() {
                specs.ceiling_feet = parse_i32(&line["P acf/_ceiling ".len()..]);
            }
        } else if line.starts_with("P acf/_cruise_speed ") {
            specs.cruise_speed_knots = parse_f64(&line["P acf/_cruise_speed ".len()..]);
        } else if line.starts_with("P acf/_vmo ") {
            specs.vmo_knots = parse_f64(&line["P acf/_vmo ".len()..]);
        } else if line.starts_with("P acf/_mmo ") {
            specs.mmo = parse_f64(&line["P acf/_mmo ".len()..]);
        } else if line.starts_with("P acf/_vref ") {
            specs.vref_knots = parse_f64(&line["P acf/_vref ".len()..]);
        } else if line.starts_with("P acf/_vs0 ") {
            specs.vs0_knots = parse_f64(&line["P acf/_vs0 ".len()..]);
        } else if line.starts_with("P acf/_vs1 ") {
            specs.vs1_knots = parse_f64(&line["P acf/_vs1 ".len()..]);
        } else if line.starts_with("P acf/_max_climb_rate ") {
            specs.max_climb_rate_fpm = parse_f64(&line["P acf/_max_climb_rate ".len()..]);
        } else if line.starts_with("P acf/_max_descent_rate ") {
            specs.max_descent_rate_fpm = parse_f64(&line["P acf/_max_descent_rate ".len()..]);
        } else if line.starts_with("P _engn/0/_type ") {
            specs.engine_type = line["P _engn/0/_type ".len()..].trim().to_string();
        } else if line.starts_with("P acf/_engine_type ") {
            if specs.engine_type.is_empty() {
                specs.engine_type = line["P acf/_engine_type ".len()..].trim().to_string();
            }
        } else if line.starts_with("P acf/_engine_count ") {
            specs.engine_count = parse_u8(&line["P acf/_engine_count ".len()..]);
        } else if line.starts_with("P acf/_wake_category ") {
            specs.wake_category = line["P acf/_wake_category ".len()..].trim().chars().next();
        } else if line.starts_with("P acf/_approach_category ") {
            specs.approach_category = line["P acf/_approach_category ".len()..].trim().chars().next();
        } else if line.starts_with("P acf/_retractable_gear ") {
            specs.retractable_gear = line["P acf/_retractable_gear ".len()..].trim() == "1";
        } else if line.starts_with("P acf/_rotorcraft ") {
            specs.is_rotorcraft = line["P acf/_rotorcraft ".len()..].trim() == "1";
        } else if line.starts_with("P acf/_seaplane ") {
            specs.is_seaplane = line["P acf/_seaplane ".len()..].trim() == "1";
        } else if line.starts_with("P acf/_min_runway ") {
            specs.min_runway_length_meters = parse_f64(&line["P acf/_min_runway ".len()..]);
        } else if line.starts_with("P acf/_turnaround ") {
            specs.turnaround_minutes = parse_u16(&line["P acf/_turnaround ".len()..]);
        }
    }

    if specs.author.is_empty() && specs.icao.is_empty() {
        return Err("no aircraft identity found".to_owned());
    }

    Ok(specs)
}

/// Merge runtime dataref values with ACF specs.
/// Uses dataref values where ACF specs are missing or zero.
pub fn merge_acf_specs_with_datarefs(
    mut specs: AcfSpecs,
    dataref_wingspan: Option<f64>,
    dataref_length: Option<f64>,
    dataref_mtow: Option<f64>,
) -> AcfSpecs {
    if let Some(ws) = dataref_wingspan {
        if ws.is_finite() && ws > 0.0 && (specs.wingspan_meters.is_none() || specs.wingspan_meters.unwrap_or(0.0) <= 0.0) {
            specs.wingspan_meters = Some(ws);
        }
    }
    if let Some(len) = dataref_length {
        if len.is_finite() && len > 0.0 && (specs.length_meters.is_none() || specs.length_meters.unwrap_or(0.0) <= 0.0) {
            specs.length_meters = Some(len);
        }
    }
    if let Some(mtow) = dataref_mtow {
        if mtow.is_finite() && mtow > 0.0 && (specs.max_takeoff_weight_kg.is_none() || specs.max_takeoff_weight_kg.unwrap_or(0.0) <= 0.0) {
            specs.max_takeoff_weight_kg = Some(mtow);
        }
    }
    specs
}

fn parse_f64(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v: &f64| v.is_finite() && *v > 0.0)
}

fn parse_i32(s: &str) -> Option<i32> {
    s.trim().parse::<i32>().ok().filter(|v: &i32| *v > 0)
}

fn parse_u8(s: &str) -> Option<u8> {
    s.trim().parse().ok().filter(|v| *v > 0)
}

fn parse_u16(s: &str) -> Option<u16> {
    s.trim().parse().ok().filter(|v| *v > 0)
}

/// Convert AcfSpecs to a traffic profile Aircraft for performance validation.
pub fn acf_specs_to_aircraft(specs: &AcfSpecs) -> crate::traffic::profiles::Aircraft {
    let wake = specs.wake_category.unwrap_or('M');
    let approach_category = specs.approach_category.unwrap_or('C');
    let engine_type = specs.engine_type.to_ascii_lowercase();
    let is_jet = engine_type.contains("jet") || engine_type.contains("turbofan");
    let is_turboprop = engine_type.contains("turboprop") || engine_type.contains("turboshaft");
    let is_piston = engine_type.contains("piston") || engine_type.contains("recip");
    let is_electric = engine_type.contains("electric");
    let is_rotorcraft = specs.is_rotorcraft;

    let (propeller, rnav) = if is_jet {
        (false, true)
    } else if is_turboprop {
        (true, true)
    } else if is_piston {
        (true, true)
    } else if is_electric {
        (false, true)
    } else if is_rotorcraft {
        (false, true)
    } else {
        (true, true)
    };

    let wingspan = specs.wingspan_meters.unwrap_or(15.0);
    let length = specs.length_meters.unwrap_or(wingspan * 0.8);
    let ceiling = specs.ceiling_feet.unwrap_or(25000) as f64;
    let cruise_knots = specs.cruise_speed_knots.unwrap_or(300.0);
    let approach_knots = specs.vref_knots.unwrap_or_else(|| {
        if is_rotorcraft { 60.0 } else if is_jet { 130.0 } else { 90.0 }
    });
    let climb_fpm = specs.max_climb_rate_fpm.unwrap_or(if is_jet { 2500.0 } else { 1000.0 });
    let descent_fpm = specs.max_descent_rate_fpm.unwrap_or(if is_jet { 2000.0 } else { 1000.0 });
    let range_nm = if let (Some(fuel), Some(cruise)) = (specs.max_fuel_kg, specs.cruise_speed_knots) {
        let consumption = if is_jet { cruise * 0.8 } else { cruise * 0.3 };
        (fuel / consumption * 0.8 * 60.0).min(5000.0)
    } else if is_jet { 3000.0 } else if is_turboprop { 1000.0 } else { 600.0 };

    let runway = if is_rotorcraft { 0.0 } else {
        specs.min_runway_length_meters.unwrap_or_else(|| {
            if is_jet { 1800.0 } else if is_turboprop { 1000.0 } else { 600.0 }
        })
    };

    crate::traffic::profiles::Aircraft {
        spacing_speed: None,
        model_aliases: vec![specs.icao.clone()],
        handling: if is_rotorcraft {
            Some(crate::traffic::animation::Kind::Rotorcraft)
        } else if is_jet {
            Some(crate::traffic::animation::Kind::Jet)
        } else if is_turboprop {
            Some(crate::traffic::animation::Kind::Turboprop)
        } else {
            Some(crate::traffic::animation::Kind::Piston)
        },
        fixed_gear: !specs.retractable_gear,
        name: specs.description.clone(),
        wingspan,
        length,
        wake,
        cwt: None,
        approach_category,
        runway,
        range_nm,
        cruise_knots,
        ceiling,
        approach_knots,
        climb_fpm,
        descent_fpm,
        propeller,
        rnav,
    }
}

/// Validate a SID against aircraft performance (climb gradient, speed restrictions).
pub fn validate_sid_performance(
    catalogue: &crate::traffic::procedures::Catalogue,
    sid_name: &str,
    runway: &str,
    transition: &str,
    aircraft: &crate::traffic::profiles::Aircraft,
) -> Result<Vec<String>, String> {
    let mut warnings = Vec::new();
    let departure = catalogue.departure(sid_name, runway, transition);
    match departure {
        Ok(segments) => {
            for segment in segments {
                for leg in segment {
                    // Check climb gradient requirements
                    if let (Some(alt1), Some(alt2)) = (leg.altitude_one, leg.altitude_two) {
                        if alt2 > alt1 {
                            let climb_required = alt2 - alt1;
                            // Assume 3 NM per 1000 ft for standard climb gradient
                            let distance_nm = leg.distance_time.parse::<f64>().unwrap_or(10.0);
                            let gradient_ft_per_nm = climb_required as f64 / distance_nm;
                            let aircraft_gradient = aircraft.climb_fpm / (aircraft.cruise_knots / 60.0);
                            if gradient_ft_per_nm > aircraft_gradient * 1.2 {
                                warnings.push(format!(
                                    "SID {} leg {}: climb gradient {:.0} ft/NM exceeds aircraft capability ({:.0} ft/NM)",
                                    sid_name, leg.identifier, gradient_ft_per_nm, aircraft_gradient
                                ));
                            }
                        }
                    }
                    // Check speed restrictions
                    if let Some(speed) = leg.speed_knots {
                        if speed > aircraft.cruise_knots * 1.1 {
                            warnings.push(format!(
                                "SID {} leg {}: speed restriction {} kts exceeds aircraft cruise {} kts",
                                sid_name, leg.identifier, speed, aircraft.cruise_knots
                            ));
                        }
                    }
                    // Check altitude constraints
                    if let Some(alt) = leg.altitude_one {
                        if alt as f64 > aircraft.ceiling {
                            warnings.push(format!(
                                "SID {} leg {}: altitude {} ft exceeds aircraft ceiling {} ft",
                                sid_name, leg.identifier, alt, aircraft.ceiling as i32
                            ));
                        }
                    }
                }
            }
        }
        Err(e) => warnings.push(format!("SID {} unavailable: {}", sid_name, e)),
    }
    Ok(warnings)
}

/// Validate a STAR against aircraft performance (descent gradient, speed restrictions).
pub fn validate_star_performance(
    catalogue: &crate::traffic::procedures::Catalogue,
    star_name: &str,
    runway: &str,
    transition: &str,
    aircraft: &crate::traffic::profiles::Aircraft,
) -> Result<Vec<String>, String> {
    let mut warnings = Vec::new();
    let arrival = catalogue.arrival(star_name, runway, transition);
    match arrival {
        Ok(segments) => {
            for segment in segments {
                for leg in segment {
                    // Check descent gradient requirements
                    if let (Some(alt1), Some(alt2)) = (leg.altitude_one, leg.altitude_two) {
                        if alt1 > alt2 {
                            let descent_required = alt1 - alt2;
                            let distance_nm = leg.distance_time.parse::<f64>().unwrap_or(10.0);
                            let gradient_ft_per_nm = descent_required as f64 / distance_nm;
                            let aircraft_gradient = aircraft.descent_fpm / (aircraft.cruise_knots / 60.0);
                            if gradient_ft_per_nm > aircraft_gradient * 1.5 {
                                warnings.push(format!(
                                    "STAR {} leg {}: descent gradient {:.0} ft/NM exceeds aircraft capability ({:.0} ft/NM)",
                                    star_name, leg.identifier, gradient_ft_per_nm, aircraft_gradient
                                ));
                            }
                        }
                    }
                    // Check speed restrictions
                    if let Some(speed) = leg.speed_knots {
                        if speed < aircraft.approach_knots * 0.8 {
                            warnings.push(format!(
                                "STAR {} leg {}: speed restriction {} kts below aircraft approach speed {} kts",
                                star_name, leg.identifier, speed, aircraft.approach_knots
                            ));
                        }
                        if speed > aircraft.cruise_knots * 1.1 {
                            warnings.push(format!(
                                "STAR {} leg {}: speed restriction {} kts exceeds aircraft cruise {} kts",
                                star_name, leg.identifier, speed, aircraft.cruise_knots
                            ));
                        }
                    }
                    // Check altitude constraints
                    if let Some(alt) = leg.altitude_one {
                        if alt as f64 > aircraft.ceiling {
                            warnings.push(format!(
                                "STAR {} leg {}: altitude {} ft exceeds aircraft ceiling {} ft",
                                star_name, leg.identifier, alt, aircraft.ceiling as i32
                            ));
                        }
                    }
                }
            }
        }
        Err(e) => warnings.push(format!("STAR {} unavailable: {}", star_name, e)),
    }
    Ok(warnings)
}

/// Validate an approach against aircraft performance (glideslope, minimums, speed).
pub fn validate_approach_performance(
    catalogue: &crate::traffic::procedures::Catalogue,
    approach_name: &str,
    transition: &str,
    aircraft: &crate::traffic::profiles::Aircraft,
) -> Result<Vec<String>, String> {
    let mut warnings = Vec::new();
    let approach = catalogue.approach(approach_name, transition);
    match approach {
        Ok(approach) => {
            for leg in approach.landing {
                // Check glideslope angle
                if let Some(angle) = leg.vertical_angle_degrees {
                    if angle > 3.5 && !aircraft.is_rotorcraft() {
                        warnings.push(format!(
                            "Approach {} leg {}: glideslope angle {:.1}° exceeds standard 3.0°",
                            approach_name, leg.identifier, angle
                        ));
                    }
                }
                // Check speed restrictions
                if let Some(speed) = leg.speed_knots {
                    if speed < aircraft.approach_knots * 0.8 {
                        warnings.push(format!(
                            "Approach {} leg {}: speed restriction {} kts below aircraft approach speed {} kts",
                            approach_name, leg.identifier, speed, aircraft.approach_knots
                        ));
                    }
                    if speed > aircraft.cruise_knots * 1.1 {
                        warnings.push(format!(
                            "Approach {} leg {}: speed restriction {} kts exceeds aircraft cruise {} kts",
                            approach_name, leg.identifier, speed, aircraft.cruise_knots
                        ));
                    }
                }
                // Check altitude constraints
                if let Some(alt) = leg.altitude_one {
                    if alt as f64 > aircraft.ceiling {
                        warnings.push(format!(
                            "Approach {} leg {}: altitude {} ft exceeds aircraft ceiling {} ft",
                            approach_name, leg.identifier, alt, aircraft.ceiling as i32
                        ));
                    }
                }
            }
            // Check missed approach
            for leg in approach.missed {
                if let Some(alt) = leg.altitude_one {
                    if alt as f64 > aircraft.ceiling {
                        warnings.push(format!(
                            "Approach {} missed leg {}: altitude {} ft exceeds aircraft ceiling {} ft",
                            approach_name, leg.identifier, alt, aircraft.ceiling as i32
                        ));
                    }
                }
            }
        }
        Err(e) => warnings.push(format!("Approach {} unavailable: {}", approach_name, e)),
    }
    Ok(warnings)
}

/// Auto-select the best SID for an aircraft based on performance.
pub fn select_best_sid(
    catalogue: &crate::traffic::procedures::Catalogue,
    runway: &str,
    aircraft: &crate::traffic::profiles::Aircraft,
) -> Option<String> {
    let routes: Vec<_> = catalogue
        .0
        .iter()
        .filter(|(key, _)| key.kind == "SID" && key.transition.starts_with("RW"))
        .collect();

    let mut best_sid = None;
    let mut best_score = i32::MAX;

    for (key, legs) in routes {
        if !key.transition.contains(&runway.trim_end_matches(|c| matches!(c, 'L' | 'R' | 'C'))) {
            continue;
        }
        let mut score = 0;
        for leg in legs {
            // Prefer lower climb gradients
            if let (Some(alt1), Some(alt2)) = (leg.altitude_one, leg.altitude_two) {
                if alt2 > alt1 {
                    let climb = alt2 - alt1;
                    let distance = leg.distance_time.parse::<f64>().unwrap_or(10.0);
                    let gradient = climb as f64 / distance;
                    let aircraft_gradient = aircraft.climb_fpm / (aircraft.cruise_knots / 60.0);
                    if gradient > aircraft_gradient {
                        score += 100;
                    } else {
                        score += (gradient / aircraft_gradient * 50.0) as i32;
                    }
                }
            }
            // Prefer RNAV-capable SIDs for RNAV aircraft
            if aircraft.rnav && legs.iter().any(|l| ["DF", "TF", "RF"].contains(&l.terminator.as_str())) {
                score -= 10;
            }
            // Prefer SIDs matching aircraft type
            if legs.iter().any(|l| l.identifier.contains("JET") && aircraft.propeller) {
                score += 50;
            }
            if legs.iter().any(|l| l.identifier.contains("PROP") && !aircraft.propeller) {
                score += 50;
            }
        }
        if score < best_score {
            best_score = score;
            best_sid = Some(key.name.clone());
        }
    }
    best_sid
}

/// Auto-select the best STAR for an aircraft based on performance.
pub fn select_best_star(
    catalogue: &crate::traffic::procedures::Catalogue,
    runway: &str,
    aircraft: &crate::traffic::profiles::Aircraft,
) -> Option<String> {
    let routes: Vec<_> = catalogue
        .0
        .iter()
        .filter(|(key, _)| key.kind == "STAR" && key.transition.starts_with("RW"))
        .collect();

    let mut best_star = None;
    let mut best_score = i32::MAX;

    for (key, legs) in routes {
        if !key.transition.contains(&runway.trim_end_matches(|c| matches!(c, 'L' | 'R' | 'C'))) {
            continue;
        }
        let mut score = 0;
        for leg in legs {
            // Prefer lower descent gradients
            if let (Some(alt1), Some(alt2)) = (leg.altitude_one, leg.altitude_two) {
                if alt1 > alt2 {
                    let descent = alt1 - alt2;
                    let distance = leg.distance_time.parse::<f64>().unwrap_or(10.0);
                    let gradient = descent as f64 / distance;
                    let aircraft_gradient = aircraft.descent_fpm / (aircraft.cruise_knots / 60.0);
                    if gradient > aircraft_gradient {
                        score += 100;
                    } else {
                        score += (gradient / aircraft_gradient * 50.0) as i32;
                    }
                }
            }
            // Prefer RNAV-capable STARs for RNAV aircraft
            if aircraft.rnav && legs.iter().any(|l| ["DF", "TF", "RF"].contains(&l.terminator.as_str())) {
                score -= 10;
            }
        }
        if score < best_score {
            best_score = score;
            best_star = Some(key.name.clone());
        }
    }
    best_star
}

/// Auto-select the best approach for an aircraft based on performance.
pub fn select_best_approach(
    catalogue: &crate::traffic::procedures::Catalogue,
    aircraft: &crate::traffic::profiles::Aircraft,
) -> Option<String> {
    let routes: Vec<_> = catalogue
        .0
        .iter()
        .filter(|(key, _)| key.kind == "APPCH" && key.route_type == "Z")
        .collect();

    let mut best_approach = None;
    let mut best_score = i32::MAX;

    for (key, legs) in routes {
        let mut score = 0;
        for leg in legs {
            // Prefer ILS for precision approach capability
            if leg.terminator == "ILS" || leg.terminator == "GLS" {
                score -= 20;
            }
            // Prefer RNAV approaches for RNAV aircraft
            if aircraft.rnav && ["RF", "TF", "DF"].contains(&leg.terminator.as_str()) {
                score -= 10;
            }
            // Check glideslope
            if let Some(angle) = leg.vertical_angle_degrees {
                if angle > 3.2 {
                    score += 50;
                }
            }
        }
        if score < best_score {
            best_score = score;
            best_approach = Some(key.name.clone());
        }
    }
    best_approach
}

#[cfg(test)]
mod tests {
    #[test]
    fn flight_plan_feedback_distinguishes_empty_incomplete_and_valid_drafts() {
        let mut plan = FlightPlan {
            callsign: "C-GTLT".into(),
            ..Default::default()
        };
        let missing = super::flight_plan_clearance_feedback(&plan).unwrap();
        assert!(missing.starts_with("C-GTLT,"));
        assert!(missing.contains("not filed a flight plan"));
        plan.departure = "LFMN".into();
        let incomplete = super::flight_plan_clearance_feedback(&plan).unwrap();
        assert!(incomplete.contains("incomplete or invalid"));
        assert!(incomplete.contains("destination airport"));
        plan.destination = "LIMC".into();
        plan.runway = "04R".into();
        assert!(super::flight_plan_clearance_feedback(&plan).is_none());
        plan.initial_altitude_feet = plan.cruise_feet + 1000;
        assert!(
            super::flight_plan_clearance_feedback(&plan)
                .unwrap()
                .contains("incomplete or invalid")
        );
    }

    #[test]
    fn planning_callsign_cannot_change_a_cleared_or_airborne_identity() {
        let mut state = crate::state::State::default();
        assert_eq!(super::planning_callsign(&state, " C-GTLT "), Some("C-GTLT"));
        for invalid in [
            "",
            "\n",
            "BAD\nSIGN",
            "<callsign>",
            "ABCDEFGHIJKLMNOPQRSTUVWXYZ12345",
        ] {
            assert!(super::planning_callsign(&state, invalid).is_none());
        }
        state.clearance = Some(crate::state::Clearance::default());
        assert!(super::planning_callsign(&state, "OTHER1").is_none());
        state.clearance = None;
        state.telemetry.on_ground = false;
        assert!(super::planning_callsign(&state, "OTHER1").is_none());
        state.telemetry.on_ground = true;
        state.phase = crate::state::PhaseCode::Pushback;
        assert!(super::planning_callsign(&state, "OTHER1").is_none());
    }
    use super::super::state::{FlightPlan, PhaseCode, State, Telemetry};
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn invalid_motion_does_not_replace_last_valid_flight_state() {
        let mut state = State {
            phase: PhaseCode::Approach,
            ..Default::default()
        };
        let before = state.clone();
        for telemetry in [
            Telemetry {
                heading_degrees: f64::NAN,
                ..Default::default()
            },
            Telemetry {
                ground_track_degrees: Some(f64::INFINITY),
                ..Default::default()
            },
            Telemetry {
                height_agl_feet: f64::NAN,
                ..Default::default()
            },
            Telemetry {
                simulation_seconds: Some(-1.0),
                ..Default::default()
            },
        ] {
            assert!(update_flight_phase(&mut state, &telemetry).is_err());
            assert_eq!(state, before);
        }
    }

    #[test]
    fn parking_completion_requires_the_final_stand_and_a_stopped_aircraft() {
        let mut state = State::default();
        state.on_event(crate::aircraft::AircraftEvent::TaxiIn);
        state.has_departed = true;
        state.taxi_clearance = crate::state::TaxiClearance {
            approved: true,
            to_parking: true,
            points: vec![crate::state::Point::default()],
            ..Default::default()
        };
        let mut telemetry = Telemetry {
            position_valid: true,
            on_ground: true,
            latitude: 0.0002,
            ..Default::default()
        };
        update_flight_phase(&mut state, &telemetry).unwrap();
        assert_eq!(state.phase, PhaseCode::TaxiIn);
        telemetry.latitude = 0.0;
        telemetry.ground_speed_knots = 4.0;
        update_flight_phase(&mut state, &telemetry).unwrap();
        assert_eq!(state.phase, PhaseCode::TaxiIn);
        telemetry.ground_speed_knots = 0.0;
        state.taxi_clearance.hold_short_runway = "09".into();
        update_flight_phase(&mut state, &telemetry).unwrap();
        assert_eq!(state.phase, PhaseCode::TaxiIn);
        state.taxi_clearance.hold_short_runway.clear();
        update_flight_phase(&mut state, &telemetry).unwrap();
        assert_eq!(state.phase, PhaseCode::Finished);
        assert!(!state.taxi_clearance.approved);
    }

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
        state.on_event(crate::aircraft::AircraftEvent::Cruise);
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
