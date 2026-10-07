//! Controller request handling, clearance state and transcript recording.
//! Network and model calls are handled by the companion engine.

use super::airport::{Airport, airport_point, calculate_taxi_route};
use super::intents::Realism;
use super::ops::{phase_name, request_available};
use super::regions::Region;
use super::state::{
    Clearance, Controller, Controllers, PhaseCode, Request, SpeechTag, State, Transmission,
};
use super::{UnitSystem, altitude_text};

/// Result of a controller request.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RequestOutcome {
    /// Whether the controller accepted the request.
    pub accepted: bool,
    /// Spoken reply text.
    pub message: String,
}

/// Record a transmission and retain the latest 500 entries.
pub fn add_transmission(state: &mut State, speaker: &str, text: &str, tag: &SpeechTag) {
    let entry = Transmission {
        pilot_reply: String::new(),
        background: false,
        speaker: speaker.to_owned(),
        text: text.to_owned(),
        sequence: state.next_sequence,
        position: tag.position.clone(),
        voice: tag.voice.clone(),
        speed: tag.speed,
        delivery: tag.delivery.clone(),
        urgent: tag.urgent,
    };
    state.next_sequence += 1;
    state.transcript.push(entry);
    if state.transcript.len() > 500 {
        state.transcript.remove(0);
    }
}

/// Deterministic mulberry32 generator for controller voice selection.
/// Exact streams differ by design, so fixtures assert membership and ranges.
#[derive(Clone, Debug)]
pub struct SimpleRng {
    state: u32,
}

impl SimpleRng {
    /// Seed the generator.
    #[must_use]
    pub fn new(seed: u32) -> Self {
        Self { state: seed }
    }

    fn next_u32(&mut self) -> u32 {
        self.state = self.state.wrapping_add(0x6D2B_79F5);
        let mut value = self.state;
        value = (value ^ (value >> 15)).wrapping_mul(value | 1);
        value ^= value.wrapping_add((value ^ (value >> 7)).wrapping_mul(value | 0x3D));
        value ^ (value >> 14)
    }

    /// Uniform index below `bound`.
    fn below(&mut self, bound: usize) -> usize {
        let bound = bound.max(1);
        let cap = u32::try_from(bound).unwrap_or(u32::MAX).max(1);
        let reduced = self.next_u32() % cap;
        usize::try_from(reduced).unwrap_or(usize::MAX) % bound
    }

    /// Uniform speed in [low, high], swapping reversed bounds.
    /// Resolution is 1/65536, plenty for a voice pace.
    fn pace(&mut self, low: f32, high: f32) -> f32 {
        let (low, high) = if high < low { (high, low) } else { (low, high) };
        let bits = u16::try_from(self.next_u32() & 0xFFFF).unwrap_or(u16::MAX);
        low + (high - low) * (f32::from(bits) / f32::from(u16::MAX))
    }
}

/// One controller order: everything `assign_controller` needs besides the
/// roster and the dice.
pub struct AssignOrder<'a> {
    /// Airspace key (`ICAO:Service`).
    pub key: &'a str,
    /// Voice pool to draw from.
    pub pool: &'a [String],
    /// Voice when the pool is empty.
    pub fallback_voice: &'a str,
    /// Delivery preset.
    pub delivery: &'a str,
    /// Speed range.
    pub speed_min: f32,
    /// Speed range.
    pub speed_max: f32,
}

/// Remember (or recall) the controller voice for one airspace key: sticky
/// assignments, recent-voice exclusion, bounded recent list.
pub fn assign_controller(
    roster: &mut Controllers,
    order: &AssignOrder<'_>,
    rng: &mut SimpleRng,
) -> Controller {
    if let Some(found) = roster.assignments.get(order.key) {
        return found.clone();
    }
    let candidates: Vec<String> = if order.pool.is_empty() {
        vec![order.fallback_voice.to_owned()]
    } else {
        order.pool.to_vec()
    };
    let exclude = if candidates.len() > 1 {
        4.min(candidates.len() - 1)
    } else {
        0
    };
    let excluded: std::collections::BTreeSet<&String> =
        roster.recent.iter().rev().take(exclude).collect();
    let mut fresh: Vec<String> = candidates
        .iter()
        .filter(|voice| !excluded.contains(voice))
        .cloned()
        .collect();
    if fresh.is_empty() {
        fresh = candidates;
    }
    let controller = Controller {
        voice: fresh[rng.below(fresh.len())].clone(),
        delivery: order.delivery.to_owned(),
        speed: rng.pace(order.speed_min, order.speed_max),
    };
    roster
        .assignments
        .insert(order.key.to_owned(), controller.clone());
    roster.recent.push(controller.voice.clone());
    while roster.recent.len() > 8 {
        roster.recent.remove(0);
    }
    controller
}

/// Intents that bypass the frequency and callsign gates.
#[must_use]
pub fn open_intent(intent: &str) -> bool {
    matches!(
        intent,
        "radio_check" | "repeat" | "standby" | "unable" | "frequency" | "checkin"
    )
}

/// Lowercase helper for the callsign gate.
fn lowercased(text: &str) -> String {
    text.to_lowercase()
}

/// Catalogue titles for the button-bypass in the callsign gate, mirroring
/// the request catalogue titles.
const CATALOGUE_TITLES: [&str; 23] = [
    "request ifr clearance",
    "request start-up",
    "request start-up and pushback",
    "request pushback",
    "request taxi",
    "request runway backtrack",
    "ready for departure",
    "taxi to parking",
    "repeat taxi instructions",
    "radio check",
    "read back clearance",
    "say again",
    "stand by",
    "unable",
    "request frequency change",
    "confirm position",
    "request altitude...",
    "request direct-to...",
    "request descent...",
    "cancel ifr",
    "brief arrival approach",
    "going around",
    "declare emergency",
];

/// Request data, airport, voices and policy used by controller handlers.
pub struct RequestContext<'a> {
    /// The pilot request.
    pub request: &'a Request,
    /// The receiving station has Tower authority (including combined duties).
    pub runway_taxi_authorized: bool,
    /// Loaded airport, if any.
    pub airport: Option<&'a Airport>,
    /// ATC voice tag.
    pub atc_tag: &'a SpeechTag,
    /// Pilot voice tag.
    pub pilot_tag: &'a SpeechTag,
    /// Discipline toggles.
    pub realism: &'a Realism,
    /// Display/speech units.
    pub units: UnitSystem,
    /// Local procedure.
    pub region: &'a Region,
}

/// Record the ATC side of an exchange. Go-arounds and emergencies always go
/// out urgent, like over there.
fn reply(
    state: &mut State,
    ctx: &RequestContext<'_>,
    accepted: bool,
    message: String,
) -> RequestOutcome {
    let mut tag = ctx.atc_tag.clone();
    if ctx.request.intent == "go_around" || ctx.request.intent == "emergency" {
        tag.urgent = true;
        "urgent".clone_into(&mut tag.delivery);
    }
    add_transmission(state, "ATC", &message, &tag);
    let template = match ctx.request.intent.as_str() {
        "start" => "pilot_start_ack",
        "pushback" => "pilot_pushback_ack",
        "start_pushback" => "pilot_start_pushback_ack",
        _ => "",
    };
    if accepted && !template.is_empty() {
        let text = crate::dialogue::say(template, &[("callsign", state.plan.callsign.clone())]);
        if let Some(entry) = state.transcript.last_mut() {
            entry.pilot_reply = text;
        }
    }
    RequestOutcome { accepted, message }
}

/// The always-available small talk: radio checks, repeats, standby, unable.
/// Returns `None` for anything else.
fn apply_simple(state: &mut State, ctx: &RequestContext<'_>) -> Option<RequestOutcome> {
    let intent = ctx.request.intent.as_str();
    if intent == "radio_check" {
        return Some(reply(
            state,
            ctx,
            true,
            crate::dialogue::say("reading_you_five_development_controller_online", &[]),
        ));
    }
    if intent == "repeat" {
        for entry in state.transcript.iter().rev() {
            if entry.speaker == "ATC" {
                return Some(reply(state, ctx, true, entry.text.clone()));
            }
        }
        return Some(reply(
            state,
            ctx,
            false,
            crate::dialogue::say("no_previous_controller_transmission", &[]),
        ));
    }
    if intent == "standby" {
        return Some(reply(
            state,
            ctx,
            true,
            crate::dialogue::say("standing_by", &[]),
        ));
    }
    if intent == "unable" {
        return Some(reply(
            state,
            ctx,
            true,
            crate::dialogue::say(
                "roger_unable_existing_clearance_remains_recorded_request",
                &[],
            ),
        ));
    }
    None
}

/// Readback handling, strict or lenient depending on the realism toggles.
fn apply_readback(state: &mut State, ctx: &RequestContext<'_>) -> RequestOutcome {
    if (ctx.request.readback_kind == "taxi" && !state.taxi_clearance.pending_readback)
        || (ctx.request.readback_kind == "ifr" && state.taxi_clearance.pending_readback)
    {
        return reply(
            state,
            ctx,
            false,
            crate::dialogue::say("readback_scope_changed", &[]),
        );
    }
    if ctx.request.waypoint.starts_with("READBACK ERROR:") {
        return reply(
            state,
            ctx,
            false,
            ctx.request
                .waypoint
                .trim_start_matches("READBACK ERROR:")
                .to_owned(),
        );
    }
    if state.taxi_clearance.pending_readback {
        if ctx.request.clearance_sequence != state.taxi_clearance.sequence
            || ctx.request.waypoint != state.taxi_clearance.instructions
        {
            return reply(
                state,
                ctx,
                false,
                crate::dialogue::say("say_again_your_taxi_readback_including_the", &[]),
            );
        }
        state.taxi_clearance.pending_readback = false;
        state.taxi_clearance.approved = true;
        state.phase = if state.taxi_clearance.to_parking {
            PhaseCode::TaxiIn
        } else {
            PhaseCode::Taxi
        };
        return reply(
            state,
            ctx,
            true,
            crate::dialogue::say(
                if state.taxi_clearance.runway_taxi {
                    "runway_taxi_readback_correct"
                } else {
                    "taxi_readback_correct_follow_the_approved_route"
                },
                &[],
            ),
        );
    }
    let request = ctx.request;
    let Some(clearance) = &state.clearance else {
        return reply(
            state,
            ctx,
            false,
            crate::dialogue::say("no_pending_clearance_to_acknowledge", &[]),
        );
    };
    if clearance.acknowledged {
        return reply(
            state,
            ctx,
            false,
            crate::dialogue::say("no_pending_clearance_to_acknowledge", &[]),
        );
    }
    if request.clearance_sequence != clearance.sequence
        || request.altitude_feet != clearance.altitude_feet
        || request.waypoint != clearance.route
    {
        // Lenient controllers take the sequence alone as good enough.
        if !ctx.realism.strict_readbacks && request.clearance_sequence == clearance.sequence {
            if let Some(clearance) = state.clearance.as_mut() {
                clearance.acknowledged = true;
            }
            return reply(
                state,
                ctx,
                true,
                crate::dialogue::say("readback_correct_for_recorded_altitude_and_route", &[]),
            );
        }
        return reply(
            state,
            ctx,
            false,
            crate::dialogue::say("say_again_your_clearance_readback_check_the", &[]),
        );
    }
    if let Some(clearance) = state.clearance.as_mut() {
        clearance.acknowledged = true;
    }
    reply(
        state,
        ctx,
        true,
        crate::dialogue::say("readback_correct_for_recorded_altitude_and_route", &[]),
    )
}

/// IFR clearance issue, both flavors.
fn apply_clearance(state: &mut State, ctx: &RequestContext<'_>) -> RequestOutcome {
    if !state.telemetry.on_ground || state.phase != PhaseCode::Parked {
        return reply(
            state,
            ctx,
            false,
            crate::dialogue::say("clearance_is_available_only_while_parked", &[]),
        );
    }
    state.clearance = Some(Clearance {
        altitude_feet: state.plan.initial_altitude_feet,
        route: state.plan.route.clone(),
        runway: state.plan.runway.clone(),
        squawk: "2105".to_owned(),
        acknowledged: false,
        sequence: state.next_sequence,
        ..Default::default()
    });
    state.phase = PhaseCode::Clearance;
    let prefix = if state.demo { "DEMO: " } else { "" };
    let altitude = altitude_text(f64::from(state.plan.initial_altitude_feet), ctx.units, true);
    let via = if state.plan.sid.is_empty()
        || state.plan.route.split_whitespace().next() == Some(state.plan.sid.as_str())
    {
        state.plan.route.clone()
    } else {
        crate::dialogue::say(
            "then",
            &[
                ("sid", (state.plan.sid).clone()),
                ("route", (state.plan.route).clone()),
            ],
        )
    };
    if ctx.region.clearance == "initial" {
        return reply(
            state,
            ctx,
            true,
            crate::dialogue::say(
                "cleared_to_via_initial_altitude_squawk_departure",
                &[
                    ("prefix", (prefix).to_string()),
                    ("destination", (state.plan.destination).clone()),
                    ("via", (via).clone()),
                    ("altitude", (altitude).clone()),
                    ("runway", (state.plan.runway).clone()),
                    (
                        "squawk",
                        state
                            .clearance
                            .as_ref()
                            .map_or_else(String::new, |c| c.squawk.clone()),
                    ),
                ],
            ),
        );
    }
    let climb = if state.plan.sid.is_empty() {
        crate::dialogue::say("climb_to", &[("altitude", (altitude).clone())])
    } else {
        crate::dialogue::say(
            "climb_via_to",
            &[
                ("sid", (state.plan.sid).clone()),
                ("altitude", (altitude).clone()),
            ],
        )
    };
    reply(
        state,
        ctx,
        true,
        crate::dialogue::say(
            "cleared_to_via_squawk_departure_runway",
            &[
                ("prefix", (prefix).to_string()),
                ("destination", (state.plan.destination).clone()),
                ("via", (via).clone()),
                ("climb", (climb).clone()),
                ("runway", (state.plan.runway).clone()),
                (
                    "squawk",
                    state
                        .clearance
                        .as_ref()
                        .map_or_else(String::new, |c| c.squawk.clone()),
                ),
            ],
        ),
    )
}

/// Create a departure or arrival taxi route. Demo sessions start at the first stand.
fn apply_taxi(state: &mut State, ctx: &RequestContext<'_>) -> RequestOutcome {
    let request = ctx.request;
    let Some(airport) = ctx.airport else {
        return reply(
            state,
            ctx,
            false,
            crate::dialogue::say("load_this_airport_s_taxi_network_before", &[]),
        );
    };
    let expected = if request.intent == "gate" {
        state.plan.destination.clone()
    } else {
        state.plan.departure.clone()
    };
    if !state.demo && airport.icao != expected {
        return reply(
            state,
            ctx,
            false,
            crate::dialogue::say("the_loaded_surface_airport_does_not_match", &[]),
        );
    }
    let mut position = state.telemetry.clone();
    if state.demo && !position.position_valid && !airport.parking.is_empty() {
        position.latitude = airport.reference_latitude + airport.parking[0].point.north / 111_320.0;
        position.longitude = airport.reference_longitude
            + airport.parking[0].point.east
                / (111_320.0 * (airport.reference_latitude * std::f64::consts::PI / 180.0).cos());
        position.position_valid = true;
    }
    let destination = if request.intent == "gate" {
        state.plan.arrival_stand.clone()
    } else {
        state.plan.runway.clone()
    };
    let prefix = if state.demo { "DEMO: " } else { "" };
    let normal = calculate_taxi_route(
        airport,
        &position,
        &destination,
        request.intent == "gate",
        'C',
    );
    let route = if request.intent == "backtrack"
        || ((normal.is_err() || airport.inferred_taxi || airport.nodes.is_empty())
            && request.intent == "taxi"
            && airport.taxi_hold_lines.is_empty())
    {
        if ctx.runway_taxi_authorized {
            if !state.demo
                && let Some(response) = crate::holding::blocked(airport, &destination, &position)
            {
                return reply(
                    state,
                    ctx,
                    false,
                    crate::dialogue::say(response, &[("runway", destination.clone())]),
                );
            }
            super::airport::calculate_runway_taxi_route(airport, &position, &destination)
        } else {
            Err(crate::dialogue::say("runway_taxi_contact_tower", &[]))
        }
    } else {
        normal
    };
    match route {
        Ok(mut route) => {
            route.approved = false;
            route.pending_readback = true;
            route.sequence = state.next_sequence;
            let instructions = route.instructions.clone();
            state.taxi_clearance = route;
            reply(
                state,
                ctx,
                true,
                crate::dialogue::say(
                    "read_back_the_taxi_route_and_hold",
                    &[
                        ("prefix", (prefix).to_string()),
                        ("instructions", (instructions).clone()),
                    ],
                ),
            )
        }
        Err(error) => reply(state, ctx, false, error),
    }
}

/// Ready for departure. Needs an approved taxi route, and off-demo the pilot
/// has to actually sit at the end of it.
fn apply_ready(state: &mut State, ctx: &RequestContext<'_>) -> RequestOutcome {
    if !state.taxi_clearance.approved || state.taxi_clearance.points.is_empty() {
        return reply(
            state,
            ctx,
            false,
            crate::dialogue::say("a_taxi_clearance_is_required_before_departure", &[]),
        );
    }
    if !state.demo
        && let Some(airport) = ctx.airport
    {
        let position = airport_point(airport, state.telemetry.latitude, state.telemetry.longitude);
        if let Some(end) = state.taxi_clearance.points.last()
            && (position.east - end.east).hypot(position.north - end.north) > 180.0
        {
            return reply(
                state,
                ctx,
                false,
                crate::dialogue::say("continue_to_the_end_of_the_approved", &[]),
            );
        }
    }
    if !state.demo {
        let blocked = ctx.airport.map_or(Some("holding_standby"), |airport| {
            crate::holding::blocked(airport, &state.plan.runway, &state.telemetry)
        });
        if let Some(response) = blocked {
            state.taxi_clearance.waiting_for_traffic = true;
            return reply(
                state,
                ctx,
                false,
                crate::dialogue::say(response, &[("runway", state.plan.runway.clone())]),
            );
        }
    }
    state.taxi_clearance.waiting_for_traffic = false;
    state.phase = PhaseCode::Departure;
    state.taxi_clearance.approved = false;
    let prefix = if state.demo { "DEMO: " } else { "" };
    reply(
        state,
        ctx,
        true,
        crate::dialogue::say(
            "runway_cleared_for_takeoff_openatc_ai_does",
            &[
                ("prefix", (prefix).to_string()),
                ("runway", (state.plan.runway).clone()),
            ],
        ),
    )
}

/// Ground workflows: clearance, pushback, taxi, ready. `None` if not ours.
fn apply_ground(state: &mut State, ctx: &RequestContext<'_>) -> Option<RequestOutcome> {
    let intent = ctx.request.intent.as_str();
    if intent == "clearance" {
        return Some(apply_clearance(state, ctx));
    }
    if intent == "start" {
        state.startup_approved = true;
        return Some(reply(
            state,
            ctx,
            true,
            crate::dialogue::say("start_up_approved_report_ready_for_pushback", &[]),
        ));
    }
    if intent == "pushback" || intent == "start_pushback" {
        if !state.telemetry.on_ground {
            return Some(reply(
                state,
                ctx,
                false,
                crate::dialogue::say("pushback_unavailable_airborne", &[]),
            ));
        }
        state.pushback_approved = true;
        if intent == "start_pushback" {
            state.startup_approved = true;
        }
        state.phase = PhaseCode::Pushback;
        let prefix = if state.demo { "DEMO: " } else { "" };
        return Some(reply(
            state,
            ctx,
            true,
            crate::dialogue::say(
                "approved_advise_ready_to_taxi",
                &[
                    ("prefix", (prefix).to_string()),
                    (
                        "permission",
                        (if intent == "start_pushback" {
                            crate::dialogue::say("start_up_and_pushback", &[])
                        } else {
                            crate::dialogue::say("pushback", &[])
                        })
                        .clone(),
                    ),
                ],
            ),
        ));
    }
    if intent == "taxi" || intent == "gate" || intent == "backtrack" {
        return Some(apply_taxi(state, ctx));
    }
    if intent == "ready" {
        return Some(apply_ready(state, ctx));
    }
    None
}

/// Airborne changes: new altitude, descent, or direct-to. Needs an existing
/// clearance to amend. `None` if not ours.
fn apply_airborne(state: &mut State, ctx: &RequestContext<'_>) -> Option<RequestOutcome> {
    let request = ctx.request;
    let intent = request.intent.as_str();
    if intent != "altitude" && intent != "descent" && intent != "direct" {
        return None;
    }
    if state.telemetry.on_ground || state.clearance.is_none() {
        return Some(reply(
            state,
            ctx,
            false,
            crate::dialogue::say("an_airborne_flight_with_an_existing_clearance", &[]),
        ));
    }
    let mut proposed = state.clearance.clone().unwrap_or_default();
    if intent == "direct" {
        // Procedure identifiers contain uppercase letters and digits.
        if !(2..=10).contains(&request.waypoint.len())
            || !request
                .waypoint
                .chars()
                .all(|value| value.is_ascii_uppercase() || value.is_ascii_digit())
        {
            return Some(reply(
                state,
                ctx,
                false,
                crate::dialogue::say("enter_a_waypoint_identifier", &[]),
            ));
        }
        if !state.demo
            && !state
                .plan
                .fixes
                .iter()
                .any(|fix| fix.identifier == request.waypoint)
        {
            return Some(reply(
                state,
                ctx,
                false,
                crate::dialogue::say("that_fix_is_not_in_the_imported", &[]),
            ));
        }
        request.waypoint.clone_into(&mut proposed.route);
    } else {
        if request.altitude_feet < 1000
            || request.altitude_feet > 45000
            || request.altitude_feet % 100 != 0
        {
            let mut message = crate::dialogue::say(
                "enter_an_altitude_from_to",
                &[
                    (
                        "minimum_altitude",
                        (altitude_text(1000.0, ctx.units, true)).clone(),
                    ),
                    (
                        "maximum_altitude",
                        (altitude_text(45000.0, ctx.units, true)).clone(),
                    ),
                ],
            );
            if ctx.units == UnitSystem::Metric {
                message += &crate::dialogue::say("in_meter_steps", &[]);
            } else {
                message += &crate::dialogue::say("in_foot_increments", &[]);
            }
            return Some(reply(state, ctx, false, message));
        }
        proposed.altitude_feet = request.altitude_feet;
    }
    if intent == "descent" {
        if f64::from(request.altitude_feet) >= state.telemetry.altitude_feet {
            return Some(reply(
                state,
                ctx,
                false,
                crate::dialogue::say("descent_altitude_must_be_below_your_current", &[]),
            ));
        }
        state.phase = PhaseCode::Arrival;
    }
    proposed.acknowledged = false;
    proposed.sequence = state.next_sequence;
    let message = crate::dialogue::say(
        "maintain_route",
        &[
            (
                "prefix",
                (if state.demo { "DEMO: " } else { "" }).to_string(),
            ),
            (
                "altitude",
                (altitude_text(f64::from(proposed.altitude_feet), ctx.units, true)).clone(),
            ),
            ("route", (proposed.route).clone()),
        ],
    );
    state.clearance = Some(proposed);
    Some(reply(state, ctx, true, message))
}

/// Frequency assignment: looks up the right station for the phase and hands
/// over the frequency. `None` if not ours.
fn apply_frequency(state: &mut State, ctx: &RequestContext<'_>) -> Option<RequestOutcome> {
    let intent = ctx.request.intent.as_str();
    if intent != "frequency" && intent != "checkin" {
        return None;
    }
    let Some(airport) = ctx.airport else {
        return Some(reply(
            state,
            ctx,
            false,
            crate::dialogue::say("load_the_airport_frequencies_first", &[]),
        ));
    };
    if state.phase == PhaseCode::Cruise {
        return Some(reply(
            state,
            ctx,
            false,
            crate::dialogue::say("enroute_sector_frequencies_are_not_loaded_no", &[]),
        ));
    }
    let expected_airport = if matches!(
        state.phase,
        PhaseCode::Arrival | PhaseCode::Approach | PhaseCode::Landed | PhaseCode::TaxiIn
    ) {
        state.plan.destination.clone()
    } else {
        state.plan.departure.clone()
    };
    if !state.demo && airport.icao != expected_airport {
        return Some(reply(
            state,
            ctx,
            false,
            crate::dialogue::say(
                "load_before_requesting_its_frequency",
                &[("expected_airport", (expected_airport).clone())],
            ),
        ));
    }
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
    for frequency in &airport.frequencies {
        if frequency.service == service {
            state.recommended_frequency_khz = frequency.khz;
            state.frequency_sequence = state.next_sequence;
            return Some(reply(
                state,
                ctx,
                true,
                crate::dialogue::say(
                    "contact_on_mhz",
                    &[
                        ("station", (frequency.name).clone()),
                        (
                            "frequency",
                            format!("{:.6}", f64::from(frequency.khz) / 1000.0),
                        ),
                    ],
                ),
            ));
        }
    }
    Some(reply(
        state,
        ctx,
        false,
        crate::dialogue::say(
            "no_frequency_is_present_in_the_loaded",
            &[("service", (service).to_string())],
        ),
    ))
}

/// Everything else: cancellations, go-arounds, briefings, position, mayday,
// and the confused-pilot fallbacks at the bottom.
fn apply_service(state: &mut State, ctx: &RequestContext<'_>) -> RequestOutcome {
    let intent = ctx.request.intent.as_str();
    let prefix = if state.demo { "DEMO: " } else { "" };
    if intent == "cancel_ifr" {
        state.ifr = false;
        return reply(
            state,
            ctx,
            true,
            crate::dialogue::say("ifr_cancelled", &[("prefix", (prefix).to_string())]),
        );
    }
    if intent == "go_around" {
        state.phase = PhaseCode::Departure;
        state.phase_evidence = 0;
        return reply(
            state,
            ctx,
            true,
            crate::dialogue::say(
                "go_around_recorded_follow_your_published_missed",
                &[("prefix", (prefix).to_string())],
            ),
        );
    }
    if intent == "progressive" {
        let instructions = state.taxi_clearance.instructions.clone();
        return reply(state, ctx, true, instructions);
    }
    if let Some(outcome) = apply_frequency(state, ctx) {
        return outcome;
    }
    if intent == "approach" {
        state.phase = PhaseCode::Approach;
        return reply(
            state,
            ctx,
            true,
            crate::dialogue::say(
                "approach_briefing_runway_no_vectors_or_procedure",
                &[
                    ("star", (state.plan.star).clone()),
                    ("approach", (state.plan.approach).clone()),
                    ("arrival_runway", (state.plan.arrival_runway).clone()),
                ],
            ),
        );
    }
    if intent == "position" {
        if !state.telemetry.position_valid {
            return reply(
                state,
                ctx,
                false,
                crate::dialogue::say("aircraft_position_unavailable", &[]),
            );
        }
        return reply(
            state,
            ctx,
            true,
            crate::dialogue::say(
                "position",
                &[
                    ("latitude", format!("{:.6}", state.telemetry.latitude)),
                    ("longitude", format!("{:.6}", state.telemetry.longitude)),
                ],
            ),
        );
    }
    if intent == "emergency" {
        if !ctx.realism.practice_emergencies {
            return reply(
                state,
                ctx,
                false,
                crate::dialogue::say("emergency_practice_is_off_enable_practice_emergencies", &[]),
            );
        }
        return reply(
            state,
            ctx,
            true,
            crate::dialogue::say("roger_mayday_squawk_state_intentions_and_souls", &[]),
        );
    }
    if !ctx.realism.strict_phraseology && !ctx.realism.teaching_corrections {
        return reply(
            state,
            ctx,
            false,
            crate::dialogue::say("didn_t_catch_that_try_request_taxi", &[]),
        );
    }
    if ctx.realism.teaching_corrections {
        return reply(
            state,
            ctx,
            false,
            crate::dialogue::say("say_again_with_a_standard_request_for", &[]),
        );
    }
    reply(
        state,
        ctx,
        false,
        crate::dialogue::say("say_again_with_a_standard_request", &[]),
    )
}

/// Record a pilot request, check radio/callsign requirements and dispatch its intent.
pub fn apply_request(state: &mut State, ctx: &RequestContext<'_>) -> RequestOutcome {
    add_transmission(
        state,
        &state.plan.callsign.clone(),
        if ctx.request.text.is_empty() {
            &ctx.request.intent
        } else {
            &ctx.request.text
        },
        ctx.pilot_tag,
    );
    // Open requests skip the gates below.
    let gated = !open_intent(&ctx.request.intent);
    if gated
        && ctx.realism.require_frequency
        && state.frequency_sequence > 0
        && state.telemetry.com1_khz > 0
        && state.telemetry.com1_khz != state.recommended_frequency_khz
    {
        let mut contact = crate::dialogue::say("contact_the_controller", &[]);
        if let Some(airport) = ctx.airport {
            for frequency in &airport.frequencies {
                if frequency.khz == state.recommended_frequency_khz {
                    contact =
                        crate::dialogue::say("contact", &[("name", (frequency.name).clone())]);
                    break;
                }
            }
        }
        let megahertz = format!("{:.3}", f64::from(state.recommended_frequency_khz) / 1000.0);
        let mut message = crate::dialogue::say(
            "on_mhz",
            &[
                ("contact", (contact).clone()),
                ("megahertz", (megahertz).clone()),
            ],
        );
        if ctx.realism.teaching_corrections {
            message += &crate::dialogue::say("tune_com_to_the_assigned_frequency_before", &[]);
        }
        return reply(state, ctx, false, message);
    }
    if gated
        && ctx.request.intent != "readback"
        && ctx.realism.require_callsign
        && !state.plan.callsign.is_empty()
    {
        let text = lowercased(&ctx.request.text);
        let call = lowercased(&state.plan.callsign);
        let button = CATALOGUE_TITLES.contains(&text.as_str());
        if !button && !text.contains(&call) {
            let mut message = crate::dialogue::say("say_callsign", &[]);
            if ctx.realism.teaching_corrections {
                message +=
                    &crate::dialogue::say("include_your_callsign_so_the_controller_knows", &[]);
            }
            return reply(state, ctx, false, message);
        }
    }
    if ctx.request.intent == "acknowledge" {
        let pending = state
            .transcript
            .iter_mut()
            .rev()
            .find(|entry| entry.speaker == "ATC" && !entry.background);
        if let Some(entry) = pending
            && entry.sequence == ctx.request.clearance_sequence
            && !entry.pilot_reply.is_empty()
            && entry.pilot_reply == ctx.request.text
            && entry.position == ctx.atc_tag.position
        {
            entry.pilot_reply.clear();
            return RequestOutcome {
                accepted: true,
                message: String::new(),
            };
        }
        return reply(
            state,
            ctx,
            false,
            crate::dialogue::say("pilot_reply_no_pending", &[]),
        );
    }
    if let Some(outcome) = apply_simple(state, ctx) {
        return outcome;
    }
    if ctx.request.intent == "readback" {
        return apply_readback(state, ctx);
    }
    if matches!(
        ctx.request.intent.as_str(),
        "taxi" | "backtrack" | "start" | "pushback" | "start_pushback" | "conversation"
    ) && state.clearance.as_ref().is_some_and(|c| !c.acknowledged)
    {
        return reply(
            state,
            ctx,
            false,
            crate::dialogue::say("read_back_your_ifr_clearance_first_destination", &[]),
        );
    }
    if !request_available(state, &ctx.request.intent) {
        return reply(
            state,
            ctx,
            false,
            if ctx.request.intent == "clearance" {
                if !state.telemetry.on_ground {
                    crate::dialogue::say("ifr_departure_clearance_requires_the_aircraft_to", &[])
                } else if state.telemetry.ground_speed_knots >= 1.0 {
                    crate::dialogue::say("stop_the_aircraft_before_requesting_ifr_departure", &[])
                } else {
                    crate::dialogue::say("departure_clearance_is_already_active_read_back", &[])
                }
            } else {
                crate::dialogue::say(
                    "that_request_is_not_available_during",
                    &[("phase", (phase_name(state.phase)).clone())],
                )
            },
        );
    }
    if let Some(outcome) = apply_ground(state, ctx) {
        return outcome;
    }
    if let Some(outcome) = apply_airborne(state, ctx) {
        return outcome;
    }
    apply_service(state, ctx)
}

#[cfg(test)]
mod tests {
    use super::super::airport::demo_airport;
    use super::super::state::{Clearance, Point, State, Telemetry};
    use super::*;
    use std::path::PathBuf;

    fn fixtures() -> serde_json::Value {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/apply.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
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

    fn flag(case: &serde_json::Value, key: &str) -> bool {
        case.get(key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }

    fn int_field(case: &serde_json::Value, key: &str, fallback: i64) -> i32 {
        i32::try_from(
            case.get(key)
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(fallback),
        )
        .unwrap_or(0)
    }

    // Build the trial state, the request and its realism tweaks from one
    // fixture case. The airport stays owned here so the borrow lives on.
    // Plan tweaks and the active clearance, if the case sets any.
    fn seed_plan(state: &mut State, case: &serde_json::Value) {
        if let Some(plan) = case.get("plan") {
            if let Some(value) = plan.get("initial").and_then(serde_json::Value::as_i64) {
                state.plan.initial_altitude_feet = i32::try_from(value).unwrap_or(0);
            }
            for (key, target) in [
                ("route", &mut state.plan.route),
                ("runway", &mut state.plan.runway),
                ("destination", &mut state.plan.destination),
                ("sid", &mut state.plan.sid),
                ("arrivalStand", &mut state.plan.arrival_stand),
            ] {
                if let Some(value) = plan.get(key).and_then(serde_json::Value::as_str) {
                    value.clone_into(target);
                }
            }
        }
        if let Some(data) = case.get("clearance") {
            state.clearance = Some(Clearance {
                altitude_feet: int_field(data, "alt", 0),
                route: data
                    .get("route")
                    .and_then(serde_json::Value::as_str)
                    .map_or_else(String::new, ToOwned::to_owned),
                acknowledged: flag(data, "acked"),
                sequence: data
                    .get("seq")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or(0),
                ..Default::default()
            });
        }
    }

    // Transcript lines, taxi approval, and realism toggles from the case.
    fn seed_extras(state: &mut State, realism: &mut Realism, case: &serde_json::Value) {
        if let Some(lines) = case.get("seedTranscript") {
            for line in lines.as_array().unwrap_or(&Vec::new()) {
                add_transmission(
                    state,
                    line["speaker"].as_str().unwrap_or(""),
                    line["text"].as_str().unwrap_or(""),
                    &SpeechTag::default(),
                );
            }
        }
        if let Some(taxi) = case.get("taxi") {
            state.taxi_clearance.approved = true;
            for point in taxi["points"].as_array().unwrap_or(&Vec::new()) {
                state.taxi_clearance.points.push(Point {
                    east: point[0].as_f64().unwrap_or(0.0),
                    north: point[1].as_f64().unwrap_or(0.0),
                    height: 0.0,
                });
            }
        }
        if let Some(rules) = case.get("realism") {
            for (key, target) in [
                ("strictReadbacks", &mut realism.strict_readbacks),
                ("requireFrequency", &mut realism.require_frequency),
                ("requireCallsign", &mut realism.require_callsign),
                ("strictPhraseology", &mut realism.strict_phraseology),
                ("teachingCorrections", &mut realism.teaching_corrections),
                ("practiceEmergencies", &mut realism.practice_emergencies),
            ] {
                if let Some(value) = rules.get(key).and_then(serde_json::Value::as_bool) {
                    *target = value;
                }
            }
        }
    }

    fn build_trial(case: &serde_json::Value) -> (State, Request, Realism, Region, Option<Airport>) {
        let mut state = State {
            demo: true,
            phase: phase(case["phase"].as_i64().unwrap()),
            ..Default::default()
        };
        state.telemetry = Telemetry {
            on_ground: case["onGround"].as_bool().unwrap(),
            altitude_feet: case
                .get("alt")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0),
            ground_speed_knots: case
                .get("speed")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0),
            position_valid: flag(case, "positionValid"),
            com1_khz: int_field(case, "com1", 0),
            ..Default::default()
        };
        // Historical fixture defaults are test data, not a live flight plan.
        state.plan.departure = "YMLT".into();
        state.plan.destination = "YMML".into();
        state.plan.runway = "32".into();
        state.plan.arrival_runway = "27".into();
        seed_plan(&mut state, case);
        let mut realism = Realism::default();
        seed_extras(&mut state, &mut realism, case);
        state.frequency_sequence = case
            .get("freqSeq")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0);
        state.recommended_frequency_khz = int_field(case, "recommended", 0);
        let region = Region {
            name: "icao".to_owned(),
            pressure: "qnh".to_owned(),
            altitude: "feet".to_owned(),
            clearance: case
                .get("region")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("initial")
                .to_owned(),
            transition_feet: 10000,
            center_khz: None,
        };
        let airport = if case.get("airport").and_then(serde_json::Value::as_str) == Some("demo") {
            Some(demo_airport())
        } else {
            None
        };
        let mut slots = Request {
            intent: case["intent"].as_str().unwrap_or("").to_owned(),
            text: case["text"].as_str().unwrap_or("").to_owned(),
            ..Default::default()
        };
        if let Some(data) = case.get("request") {
            slots.altitude_feet = int_field(data, "alt", 0);
            if let Some(value) = data.get("waypoint").and_then(serde_json::Value::as_str) {
                value.clone_into(&mut slots.waypoint);
            }
            slots.clearance_sequence = data
                .get("seq")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(0);
        }
        (state, slots, realism, region, airport)
    }

    // Check the controller result against the shared fixture.
    fn check_case(case: &serde_json::Value) {
        let (mut state, slots, realism, region, airport) = build_trial(case);
        let pilot_tag = SpeechTag::default();
        let atc_tag = SpeechTag::default();
        let ctx = RequestContext {
            runway_taxi_authorized: false,
            request: &slots,
            airport: airport.as_ref(),
            atc_tag: &atc_tag,
            pilot_tag: &pilot_tag,
            realism: &realism,
            units: UnitSystem::Imperial,
            region: &region,
        };
        let outcome = apply_request(&mut state, &ctx);
        let want = &case["expected"];
        let name = case["name"].as_str().unwrap_or("");
        assert_eq!(
            outcome.accepted,
            want["accepted"].as_bool().unwrap_or(false),
            "apply {name}"
        );
        if let Some(message) = want.get("message").and_then(serde_json::Value::as_str) {
            assert_eq!(outcome.message, message, "message {name}");
        }
        if let Some(prefix) = want
            .get("messagePrefix")
            .and_then(serde_json::Value::as_str)
        {
            assert!(outcome.message.starts_with(prefix), "prefix {name}");
        }
        if let Some(code) = want.get("phaseAfter").and_then(serde_json::Value::as_i64) {
            assert_eq!(state.phase, phase(code), "phase {name}");
        }
        if let Some(acked) = want
            .get("acknowledged")
            .and_then(serde_json::Value::as_bool)
        {
            assert_eq!(
                state
                    .clearance
                    .as_ref()
                    .is_some_and(|value| value.acknowledged),
                acked,
                "ack {name}"
            );
        }
        if let Some(alt) = want.get("clearanceAlt").and_then(serde_json::Value::as_i64) {
            assert_eq!(
                state
                    .clearance
                    .as_ref()
                    .map_or(0, |value| value.altitude_feet),
                i32::try_from(alt).unwrap_or(0),
                "clearance alt {name}"
            );
        }
        if let Some(khz) = want
            .get("recommendedKhz")
            .and_then(serde_json::Value::as_i64)
        {
            assert_eq!(
                state.recommended_frequency_khz,
                i32::try_from(khz).unwrap_or(0),
                "frequency {name}"
            );
        }
        if flag(want, "urgent") {
            let last = state.transcript.last().unwrap();
            assert_eq!(last.speaker, "ATC", "urgent speaker {name}");
            assert!(last.urgent, "urgent flag {name}");
            assert_eq!(last.delivery, "urgent", "urgent delivery {name}");
        }
    }

    #[test]
    fn scenarios_match_shared_fixtures() {
        let cases = fixtures();
        let scenarios = cases["scenarios"].as_array().unwrap();
        assert_eq!(scenarios.len(), 45, "all scenarios run");
        for case in scenarios {
            check_case(case);
        }
    }

    #[test]
    fn roster_matches_shared_fixtures() {
        let cases = fixtures();
        let mut roster = Controllers::default();
        let mut count = 0;
        for case in cases["roster"].as_array().unwrap() {
            let pool: Vec<String> = case["pool"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|value| value.as_str().unwrap_or("").to_owned())
                .collect();
            if !flag(case, "recall") {
                roster.recent = case["recent"]
                    .as_array()
                    .unwrap_or(&Vec::new())
                    .iter()
                    .map(|value| value.as_str().unwrap_or("").to_owned())
                    .collect();
            }
            let key = case["key"].as_str().unwrap_or("");
            let fallback = case["fallback"].as_str().unwrap_or("");
            let order = AssignOrder {
                key,
                pool: &pool,
                fallback_voice: fallback,
                delivery: "brisk",
                speed_min: 0.9,
                speed_max: 1.15,
            };
            let mut rng = SimpleRng::new(7);
            let mut controller = assign_controller(&mut roster, &order, &mut rng);
            if flag(case, "recall") {
                let mut again = SimpleRng::new(7);
                controller = assign_controller(&mut roster, &order, &mut again);
            }
            let want = &case["expected"];
            let name = case["name"].as_str().unwrap_or("");
            assert_eq!(
                controller.voice,
                want["voice"].as_str().unwrap_or(""),
                "roster {name}"
            );
            if let Some(delivery) = want.get("delivery").and_then(serde_json::Value::as_str) {
                assert_eq!(controller.delivery, delivery, "delivery {name}");
            }
            if let Some(min) = want.get("speedMin").and_then(serde_json::Value::as_f64) {
                let max = want["speedMax"].as_f64().unwrap_or(2.0);
                let speed = f64::from(controller.speed);
                assert!(speed >= min && speed <= max, "speed {name}");
            }
            count += 1;
        }
        assert_eq!(count, 4, "all roster cases run");
    }
}
