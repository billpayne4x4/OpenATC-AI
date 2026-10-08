//! Flight state, simulator telemetry, clearances and transcript serialization.

use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

/// Serialized flight stage. `Arrival` (5) displays as Descent;
/// `TaxiIn` (9) displays as Taxi to parking.
#[derive(Clone, Copy, Debug, Default, Deserialize_repr, Eq, PartialEq, Serialize_repr)]
#[repr(u8)]
pub enum PhaseCode {
    /// On stand, engines may be off.
    #[default]
    Parked = 0,
    /// IFR clearance received, pre-taxi.
    Clearance = 1,
    /// Moving under own power to the runway.
    Taxi = 2,
    /// Takeoff roll through initial climb.
    Departure = 3,
    /// Enroute cruise.
    Cruise = 4,
    /// Arrival phase (displayed as Descent).
    Arrival = 5,
    /// Approach phase, windshear advisories arm.
    Approach = 6,
    /// Wheels down, on the runway.
    Landed = 7,
    /// Pushback from stand.
    Pushback = 8,
    /// Taxi from runway to parking.
    TaxiIn = 9,
    /// Flight complete, state kept for review.
    Finished = 10,
}

/// Simulator-reported traffic target, excluding ownship.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TrafficTarget {
    /// Degrees north.
    pub latitude: f64,
    /// Degrees east.
    pub longitude: f64,
    /// Altitude in feet MSL.
    pub altitude_feet: f64,
    /// Ground track in degrees true.
    pub track_degrees: f64,
    /// Simulator weight-on-wheels flag.
    pub on_ground: bool,
}

/// Live simulator telemetry snapshot.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default)]
#[allow(clippy::struct_excessive_bools)] // Independent simulator flags, not workflow states.
pub struct Telemetry {
    /// Degrees north.
    pub latitude: f64,
    /// Degrees east.
    pub longitude: f64,
    /// Feet MSL.
    pub altitude_feet: f64,
    /// Knots.
    pub ground_speed_knots: f64,
    /// Degrees true.
    pub heading_degrees: f64,
    /// Ground track degrees true, when supplied by the simulator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ground_track_degrees: Option<f64>,
    /// Weight on wheels.
    #[serde(default = "default_true")]
    pub on_ground: bool,
    /// Simulator paused.
    pub paused: bool,
    /// Feet per minute.
    pub vertical_speed_fpm: f64,
    /// Feet above ground.
    pub height_agl_feet: f64,
    /// COM1 frequency in kHz.
    pub com1_khz: i32,
    /// Aircraft radio powered; absent legacy telemetry defaults to true.
    #[serde(default = "default_true")]
    pub radio_power: bool,
    /// None means traffic data unavailable; an empty list is a valid clear scan.
    pub traffic: Option<Vec<TrafficTarget>>,
    /// Position data valid.
    pub position_valid: bool,
}

impl Default for Telemetry {
    /// Default telemetry before simulator data arrives.
    fn default() -> Self {
        Self {
            latitude: 0.0,
            longitude: 0.0,
            altitude_feet: 0.0,
            ground_speed_knots: 0.0,
            heading_degrees: 0.0,
            ground_track_degrees: None,
            on_ground: true,
            paused: false,
            vertical_speed_fpm: 0.0,
            height_agl_feet: 0.0,
            com1_khz: 0,
            radio_power: true,
            traffic: None,
            position_valid: false,
        }
    }
}

/// One route fix.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RouteFix {
    /// Fix identifier.
    pub identifier: String,
    /// Degrees north.
    pub latitude: f64,
    /// Degrees east.
    pub longitude: f64,
    /// Feet MSL.
    pub altitude_feet: f64,
}

fn default_callsign() -> String {
    "VH-BIL".to_owned()
}

fn default_departure() -> String {
    String::new()
}

fn default_destination() -> String {
    String::new()
}

fn default_route() -> String {
    "DCT".to_owned()
}

fn default_runway() -> String {
    String::new()
}

fn default_arrival_runway() -> String {
    String::new()
}

fn default_cruise_feet() -> i32 {
    32_000
}

fn default_initial_altitude_feet() -> i32 {
    5000
}

fn default_cost_index() -> i32 {
    5
}

fn default_source() -> String {
    "Manual".to_owned()
}

fn default_aircraft() -> String {
    "A20N".to_owned()
}

/// Planned flight and dispatch fields.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlightPlan {
    /// Aircraft callsign.
    #[serde(default = "default_callsign")]
    pub callsign: String,
    /// Departure ICAO.
    #[serde(default = "default_departure")]
    pub departure: String,
    /// Destination ICAO.
    #[serde(default = "default_destination")]
    pub destination: String,
    /// Route string.
    #[serde(default = "default_route")]
    pub route: String,
    /// Departure runway.
    #[serde(default = "default_runway")]
    pub runway: String,
    /// Arrival runway.
    #[serde(default = "default_arrival_runway")]
    pub arrival_runway: String,
    /// Planned cruise feet.
    #[serde(default = "default_cruise_feet")]
    pub cruise_feet: i32,
    /// Alternate ICAO.
    #[serde(default)]
    pub alternate: String,
    /// Aircraft type.
    #[serde(default = "default_aircraft")]
    pub aircraft: String,
    /// Registration.
    #[serde(default = "default_callsign")]
    pub registration: String,
    /// Assigned SID.
    #[serde(default)]
    pub sid: String,
    /// SID transition.
    #[serde(default)]
    pub sid_transition: String,
    /// Assigned STAR.
    #[serde(default)]
    pub star: String,
    /// STAR transition.
    #[serde(default)]
    pub star_transition: String,
    /// Assigned approach.
    #[serde(default)]
    pub approach: String,
    /// Departure stand.
    #[serde(default)]
    pub departure_stand: String,
    /// Arrival stand.
    #[serde(default)]
    pub arrival_stand: String,
    /// AIRAC cycle.
    #[serde(default)]
    pub airac: String,
    /// Plan source.
    #[serde(default = "default_source")]
    pub source: String,
    /// Initial altitude feet.
    #[serde(default = "default_initial_altitude_feet")]
    pub initial_altitude_feet: i32,
    /// Passenger count.
    #[serde(default)]
    pub passengers: i32,
    /// Cost index.
    #[serde(default = "default_cost_index")]
    pub cost_index: i32,
    /// Kilograms of block fuel.
    #[serde(default)]
    pub block_fuel_kg: f64,
    /// Kilograms of trip fuel.
    #[serde(default)]
    pub trip_fuel_kg: f64,
    /// Kilograms of reserve fuel.
    #[serde(default)]
    pub reserve_fuel_kg: f64,
    /// Kilograms of alternate fuel.
    #[serde(default)]
    pub alternate_fuel_kg: f64,
    /// Kilograms of taxi fuel.
    #[serde(default)]
    pub taxi_fuel_kg: f64,
    /// Kilograms of payload.
    #[serde(default)]
    pub payload_kg: f64,
    /// Kilograms of cargo.
    #[serde(default)]
    pub cargo_kg: f64,
    /// Kilograms of zero-fuel weight.
    #[serde(default)]
    pub zero_fuel_weight_kg: f64,
    /// Kilograms of takeoff weight.
    #[serde(default)]
    pub takeoff_weight_kg: f64,
    /// Kilograms of landing weight.
    #[serde(default)]
    pub landing_weight_kg: f64,
    /// Estimated minutes enroute.
    #[serde(default)]
    pub estimated_minutes: f64,
    /// Route fixes.
    #[serde(default)]
    pub fixes: Vec<RouteFix>,
}

impl Default for FlightPlan {
    fn default() -> Self {
        Self {
            callsign: default_callsign(),
            departure: default_departure(),
            destination: default_destination(),
            route: default_route(),
            runway: default_runway(),
            arrival_runway: default_arrival_runway(),
            cruise_feet: default_cruise_feet(),
            alternate: String::new(),
            aircraft: default_aircraft(),
            registration: default_callsign(),
            sid: String::new(),
            sid_transition: String::new(),
            star: String::new(),
            star_transition: String::new(),
            approach: String::new(),
            departure_stand: String::new(),
            arrival_stand: String::new(),
            airac: String::new(),
            source: default_source(),
            initial_altitude_feet: default_initial_altitude_feet(),
            passengers: 0,
            cost_index: default_cost_index(),
            block_fuel_kg: 0.0,
            trip_fuel_kg: 0.0,
            reserve_fuel_kg: 0.0,
            alternate_fuel_kg: 0.0,
            taxi_fuel_kg: 0.0,
            payload_kg: 0.0,
            cargo_kg: 0.0,
            zero_fuel_weight_kg: 0.0,
            takeoff_weight_kg: 0.0,
            landing_weight_kg: 0.0,
            estimated_minutes: 0.0,
            fixes: Vec::new(),
        }
    }
}

fn default_squawk() -> String {
    "2000".to_owned()
}

fn default_delivery() -> String {
    "standard".to_owned()
}

fn default_speed() -> f32 {
    1.0
}

/// Active ATC clearance.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Clearance {
    /// Assigned altitude feet.
    pub altitude_feet: i32,
    /// Assigned route.
    pub route: String,
    /// Assigned runway.
    pub runway: String,
    /// Squawk code.
    #[serde(default = "default_squawk")]
    pub squawk: String,
    /// Pilot read it back.
    #[serde(default)]
    pub acknowledged: bool,
    /// Clearance sequence number.
    #[serde(default)]
    pub sequence: u32,
    /// Recommended frequency in kHz.
    #[serde(default)]
    pub frequency_khz: i32,
}

/// One transcript row.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Transmission {
    /// Deterministic pilot acknowledgment, empty when none is expected.
    #[serde(default)]
    pub pilot_reply: String,
    /// Radio exchange addressed to another aircraft.
    #[serde(default)]
    pub background: bool,
    /// Speaker tag (`ATC`, callsign, `COPILOT`, …).
    #[serde(default)]
    pub speaker: String,
    /// Spoken/displayed text.
    #[serde(default)]
    pub text: String,
    /// Transcript sequence number.
    #[serde(default)]
    pub sequence: u32,
    /// ATC position, empty when none.
    #[serde(default)]
    pub position: String,
    /// Voice id.
    #[serde(default)]
    pub voice: String,
    /// Delivery preset.
    #[serde(default = "default_delivery")]
    pub delivery: String,
    /// Speech speed multiplier.
    #[serde(default = "default_speed")]
    pub speed: f32,
    /// Urgent delivery.
    #[serde(default)]
    pub urgent: bool,
}

/// Pilot request submitted to the controller.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Request {
    /// Internal controller initiative; never accepted from an HTTP request.
    #[serde(skip)]
    pub controller_initiated: bool,
    /// Intent key (`taxi`, `altitude`, …).
    pub intent: String,
    /// Raw request text.
    pub text: String,
    /// Requested altitude feet.
    pub altitude_feet: i32,
    /// Requested waypoint.
    pub waypoint: String,
    /// Clearance sequence being read back.
    pub clearance_sequence: u32,
    /// Explicit automatic-readback scope: `ifr` or `taxi`; empty for legacy requests.
    pub readback_kind: String,
    /// Requesting role (`atc` default).
    pub role: String,
}

/// Voice and delivery settings attached to a transmission.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct SpeechTag {
    /// ATC position, empty when none.
    pub position: String,
    /// Voice id.
    pub voice: String,
    /// Delivery preset.
    pub delivery: String,
    /// Speech speed multiplier.
    pub speed: f32,
    /// Urgent delivery.
    pub urgent: bool,
}

impl Default for SpeechTag {
    fn default() -> Self {
        Self {
            position: String::new(),
            voice: String::new(),
            delivery: "standard".to_owned(),
            speed: 1.0,
            urgent: false,
        }
    }
}

/// Remembered controller voice for one airspace (`ICAO:Service`).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Controller {
    /// Voice id.
    pub voice: String,
    /// Delivery preset.
    pub delivery: String,
    /// Speech speed multiplier.
    pub speed: f32,
}

/// Controller roster with sticky assignments and recent voices.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Controllers {
    /// Voice per airspace key.
    pub assignments: std::collections::BTreeMap<String, Controller>,
    /// Recently used voices, newest last, capped at 8.
    pub recent: Vec<String>,
}

/// Remembered weather advisory (station + hazard kind).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct WeatherAdvisory {
    /// Reporting station.
    pub station: String,
    /// Hazard kind.
    pub hazard: String,
    /// Observation time (unix seconds).
    pub observed: f64,
}

/// Point in local east, north and height coordinates.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Point {
    /// Meters east of reference.
    pub east: f64,
    /// Meters north of reference.
    pub north: f64,
    /// Meters above reference.
    pub height: f64,
}

/// Approved taxi path.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default)]
#[allow(clippy::struct_excessive_bools)]
pub struct TaxiClearance {
    /// Explicit permission for this runway crossing only.
    pub crossing_runway: String,
    /// Airport ICAO.
    pub airport: String,
    /// Destination (runway or `PARKING`).
    pub destination: String,
    /// Controller instructions text.
    pub instructions: String,
    /// Ordered taxiway identifiers, independent of editable phrase wording.
    pub via: String,
    /// Path points.
    pub points: Vec<Point>,
    /// Controller approved the computed path after readback.
    pub approved: bool,
    /// Issued taxi instructions awaiting readback.
    pub pending_readback: bool,
    /// Further tower clearance is needed to reach the departure end via runway.
    #[serde(default)]
    pub backtrack_required: bool,
    /// This clearance explicitly authorizes runway taxi/backtracking, not takeoff.
    #[serde(default)]
    pub runway_taxi: bool,
    /// Physical runway end nearest the authorized entry, distinct from departure end.
    #[serde(default)]
    pub entry_runway: String,
    /// Channel already notified at this hold point; prevents repeated handoffs.
    pub holding_channel: i32,
    /// Traffic or missing traffic data currently blocks departure.
    pub waiting_for_traffic: bool,
    /// Guidance ends at the authorized hold point; clearance remains recorded.
    pub guidance_complete: bool,
    /// Runway at the next authorized hold point (may precede destination).
    pub hold_short_runway: String,
    /// Clearance sequence number.
    pub sequence: u32,
    /// Reference latitude for local projection.
    pub reference_latitude: f64,
    /// Reference longitude for local projection.
    pub reference_longitude: f64,
    /// Destination coordinates.
    pub destination_point: Point,
    /// Destination is parking, not a runway.
    pub to_parking: bool,
}

/// Flight session state shared by the engine and UI.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)] // Independent saved permissions and flight flags.
pub struct State {
    /// Pending simulator-thread crew actions.
    #[serde(default)]
    pub crew_actions: Vec<crate::crew::Action>,
    /// Aircraft controls and live availability.
    #[serde(default)]
    pub crew_controls: Vec<crate::crew::Capability>,
    /// Controller permissions were withdrawn after unresolved noncompliance.
    #[serde(default)]
    pub clearance_cancelled: bool,
    /// Flight plan.
    pub plan: FlightPlan,
    /// Latest telemetry.
    pub telemetry: Telemetry,
    /// Flight stage, integer-coded.
    pub phase: PhaseCode,
    /// Active clearance, if any.
    #[serde(default)]
    pub clearance: Option<Clearance>,
    /// Transcript rows.
    #[serde(default)]
    pub transcript: Vec<Transmission>,
    /// Demo session (no sim).
    #[serde(default = "default_true")]
    pub demo: bool,
    /// IFR flight.
    #[serde(default = "default_true")]
    pub ifr: bool,
    /// Next sequence number.
    #[serde(default = "default_sequence")]
    pub next_sequence: u32,
    /// Departure detected.
    #[serde(default)]
    pub has_departed: bool,
    /// Engine start permission; approval does not operate aircraft controls.
    #[serde(default)]
    pub startup_approved: bool,
    /// Pushback permission; approval does not move the aircraft.
    #[serde(default)]
    pub pushback_approved: bool,
    /// Consecutive ticks supporting the candidate flight stage.
    #[serde(skip)]
    pub phase_evidence: i32,
    /// Flight stage awaiting enough consistent telemetry.
    #[serde(skip)]
    pub candidate_phase: PhaseCode,
    /// Approved taxi path.
    #[serde(default)]
    pub taxi_clearance: TaxiClearance,
    /// Recommended frequency in kHz.
    #[serde(default)]
    pub recommended_frequency_khz: i32,
    /// Frequency recommendation sequence.
    #[serde(default)]
    pub frequency_sequence: u32,
    /// Remembered weather advisories.
    #[serde(default)]
    pub weather_advisories: Vec<WeatherAdvisory>,
}

fn default_true() -> bool {
    true
}

fn default_sequence() -> u32 {
    1
}

impl Default for State {
    fn default() -> Self {
        Self {
            crew_actions: Vec::new(),
            crew_controls: Vec::new(),
            clearance_cancelled: false,
            plan: FlightPlan::default(),
            telemetry: Telemetry::default(),
            phase: PhaseCode::Parked,
            clearance: None,
            transcript: Vec::new(),
            demo: true,
            ifr: true,
            next_sequence: 1,
            has_departed: false,
            startup_approved: false,
            pushback_approved: false,
            phase_evidence: 0,
            candidate_phase: PhaseCode::Parked,
            taxi_clearance: TaxiClearance::default(),
            recommended_frequency_khz: 0,
            frequency_sequence: 0,
            weather_advisories: Vec::new(),
        }
    }
}

/// Controller reply: accepted flag plus spoken/displayed message.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ApplyResult {
    /// Request accepted.
    pub accepted: bool,
    /// Controller message.
    pub message: String,
}
