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
    /// Optional display name supplied by the traffic provider.
    pub name: String,
    /// Known tuned COM1 channel in kHz; absent for providers without radio state.
    pub com1_khz: Option<i32>,
    /// Known tuned COM2 channel in kHz; never inferred from proximity.
    pub com2_khz: Option<i32>,
    /// Stable simulator Mode S identity; zero means the provider did not identify it.
    pub id: u32,
    /// Flight ID supplied by the simulator traffic provider.
    pub callsign: String,
    /// ICAO aircraft type supplied by the traffic provider.
    pub aircraft: String,
    /// Ground speed derived from simulator horizontal velocity, knots.
    pub ground_speed_knots: Option<f64>,
    /// Wingspan reported by the traffic provider, if available.
    pub wingspan_meters: Option<f64>,
    /// Simulator wake category: 0 light, 1 medium, 2 heavy, 3 super.
    pub wake_category: Option<u8>,
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
    /// Whether the target is actually on runway pavement. `None` means unknown.
    #[serde(default)]
    pub runway_occupancy: Option<bool>,
}

/// Worker-produced position and model for lightweight simulator instances.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedTraffic {
    /// Actual vertical movement, used by TCAS independently of visual pitch.
    #[serde(default)]
    pub vertical_speed_fpm: f64,
    /// Body orientation can differ from ground track during pushback or in wind.
    #[serde(default)]
    pub body_heading_degrees: Option<f64>,
    /// Operational identity and geographic position.
    pub target: TrafficTarget,
    /// All locally indexed parts of the selected CSL model.
    pub model: std::sync::Arc<crate::traffic::csl::Model>,
    /// Continuous visual state from the simulation worker.
    pub animation: crate::traffic::animation::Animation,
}

/// Per-aircraft generated-traffic debug state for the map and Traffic page.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TrafficDebug {
    /// Stable aircraft id.
    pub id: u32,
    /// Radio callsign.
    pub callsign: String,
    /// Current movement stage.
    pub stage: String,
    /// Human-readable reason for the current state.
    pub reason: String,
    /// Blocking aircraft id, when one exists.
    pub blocker: Option<u32>,
    /// The aircraft is waiting for movement authority.
    pub awaiting_clearance: bool,
    /// Exact planned stop point before a departure clearance, latitude/longitude.
    #[serde(default)]
    pub hold: Option<[f64; 2]>,
}

/// The traffic currently pointed out by the tuned controller.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TrafficCall {
    /// Surveillance identity, not the aircraft's display order.
    pub target_id: u32,
    /// Frequency where the advisory was issued.
    pub frequency_khz: i32,
    /// The pilot has reported this traffic in sight.
    pub sighted: bool,
}

/// Live simulator telemetry snapshot.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default)]
#[allow(clippy::struct_excessive_bools)] // Independent simulator flags, not workflow states.
pub struct Telemetry {
    /// ICAO type of the loaded aircraft, independent of the dispatch plan.
    pub aircraft_icao: String,
    /// Simulator-reported wingspan in metres, absent when unavailable.
    pub wingspan_meters: Option<f64>,
    /// Simulator wake category: 0 light, 1 medium, 2 heavy, 3 super.
    pub wake_category: Option<u8>,
    /// Degrees north.
    pub latitude: f64,
    /// Degrees east.
    pub longitude: f64,
    /// Feet MSL.
    pub altitude_feet: f64,
    /// Pilot's barometric altimeter reading, feet; absent when unavailable.
    pub indicated_altitude_feet: Option<f64>,
    /// Altitude on standard pressure, feet, used for flight-level clearances.
    pub pressure_altitude_feet: Option<f64>,
    /// Knots.
    pub ground_speed_knots: f64,
    /// Degrees true.
    pub heading_degrees: f64,
    /// Ground track degrees true, when supplied by the simulator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ground_track_degrees: Option<f64>,
    /// Indicated airspeed knots, absent when unavailable.
    pub indicated_speed_knots: Option<f64>,
    /// Magnetic heading degrees, absent when unavailable.
    pub magnetic_heading_degrees: Option<f64>,
    /// Weight on wheels.
    #[serde(default = "default_true")]
    pub on_ground: bool,
    /// At least one engine is running.
    pub engines_running: bool,
    /// Simulator paused.
    pub paused: bool,
    /// Elapsed simulator flight time; absent from older clients.
    pub simulation_seconds: Option<f64>,
    /// Simulator UTC seconds since midnight, used for expected further clearance times.
    pub zulu_seconds: Option<f64>,
    /// Local radio playback, microphone or a pending crew radio exchange.
    pub radio_busy: bool,
    /// Latest transmission observed by the simulator audio queue.
    pub radio_sequence_seen: Option<u32>,
    /// Feet per minute.
    pub vertical_speed_fpm: f64,
    /// Feet above ground.
    pub height_agl_feet: f64,
    /// COM1 frequency in kHz.
    pub com1_khz: i32,
    /// COM2 channel in kHz; unavailable radios remain unknown.
    #[serde(default)]
    pub com2_khz: Option<i32>,
    /// Four octal digits as shown on the transponder; absent if unavailable.
    pub transponder_code: Option<i32>,
    /// X-Plane mode: 0 off, 1 standby, 2 A, 3 C, 4 test, 5 ground, 6 TA, 7 TA/RA.
    pub transponder_mode: Option<i32>,
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
            aircraft_icao: String::new(),
            wingspan_meters: None,
            wake_category: None,
            latitude: 0.0,
            longitude: 0.0,
            altitude_feet: 0.0,
            indicated_altitude_feet: None,
            pressure_altitude_feet: None,
            ground_speed_knots: 0.0,
            heading_degrees: 0.0,
            ground_track_degrees: None,
            indicated_speed_knots: None,
            magnetic_heading_degrees: None,
            on_ground: true,
            engines_running: false,
            paused: false,
            simulation_seconds: None,
            zulu_seconds: None,
            radio_busy: false,
            radio_sequence_seen: None,
            vertical_speed_fpm: 0.0,
            height_agl_feet: 0.0,
            com1_khz: 0,
            com2_khz: None,
            transponder_code: None,
            transponder_mode: None,
            radio_power: true,
            traffic: None,
            position_valid: false,
        }
    }
}

impl Telemetry {
    /// Altitude used for pilot reports; older clients fall back to geometric MSL.
    #[must_use]
    pub fn reported_altitude_feet(&self) -> f64 {
        self.indicated_altitude_feet.unwrap_or(self.altitude_feet)
    }

    /// Compare flight levels on standard pressure and lower altitudes on the altimeter.
    #[must_use]
    pub fn clearance_altitude_feet(&self, target: i32, transition: i32) -> f64 {
        if target >= transition {
            self.pressure_altitude_feet
                .unwrap_or_else(|| self.reported_altitude_feet())
        } else {
            self.reported_altitude_feet()
        }
    }
}

/// One route fix.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RouteFix {
    /// Inserted from the selected procedure; rebuild it after a runway/procedure change.
    pub procedure_generated: bool,
    /// Fix identifier.
    pub identifier: String,
    /// Degrees north.
    pub latitude: f64,
    /// Degrees east.
    pub longitude: f64,
    /// Planned altitude, not a crossing restriction.
    pub altitude_feet: f64,
    /// Published lower crossing limit, feet MSL.
    pub minimum_altitude_feet: Option<i32>,
    /// Preserve a published flight-level reference separately from feet on QNH.
    pub minimum_altitude_is_flight_level: Option<bool>,
    /// Published upper crossing limit, feet MSL.
    pub maximum_altitude_feet: Option<i32>,
    /// Preserve the reference for an upper limit or exact crossing altitude.
    pub maximum_altitude_is_flight_level: Option<bool>,
    /// Published minimum indicated airspeed at the fix.
    pub minimum_speed_knots: Option<f64>,
    /// Published maximum indicated airspeed at the fix.
    pub maximum_speed_knots: Option<f64>,
    /// Published leg type; curved and vector legs need their own geometry.
    pub path_terminator: String,
    /// Published fly-over indication; do not treat it as an ordinary fly-by fix.
    pub fly_over: bool,
    /// Navdata origin of this leg or restriction.
    pub constraint_source: String,
    /// Requested step-climb altitude at this fix; it still needs ATC clearance.
    pub step_altitude_feet: Option<i32>,
}

fn default_callsign() -> String {
    String::new()
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
    /// Departure contains published vector or manual-termination legs.
    #[serde(default)]
    pub sid_has_vectors: bool,
    /// Assigned STAR.
    #[serde(default)]
    pub star: String,
    /// STAR transition.
    #[serde(default)]
    pub star_transition: String,
    /// Assigned approach.
    #[serde(default)]
    pub approach: String,
    /// Procedure transition fallback reported to the Planning page.
    #[serde(default)]
    pub procedure_notice: String,
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
            sid_has_vectors: false,
            sid_transition: String::new(),
            star: String::new(),
            star_transition: String::new(),
            approach: String::new(),
            procedure_notice: String::new(),
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

/// Published speed restriction repeated with a level instruction.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeedReadback {
    /// Clearance that carries this restriction.
    pub sequence: u32,
    /// Published crossing fix.
    pub fix: String,
    /// Indicated speed in knots.
    pub knots: f64,
    /// The published speed is exact rather than a maximum.
    pub exact: bool,
}

/// Active ATC clearance.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Clearance {
    /// Assigned altitude feet.
    pub altitude_feet: i32,
    /// Assigned route.
    pub route: String,
    /// Initial departure routing; an empty value preserves legacy full-route readback.
    #[serde(default)]
    pub departure_readback: String,
    /// The initial clearance accepts the filed en-route routing.
    #[serde(default)]
    pub as_filed: bool,
    /// Assigned runway.
    pub runway: String,
    /// Squawk code.
    #[serde(default = "default_squawk")]
    pub squawk: String,
    /// Fields changed by the latest amendment: altitude, direct or runway.
    pub readback_scope: String,
    /// Speed component of the same instruction, tied to its clearance sequence.
    pub readback_speed: Option<SpeedReadback>,
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
    /// Receiving radio; absent on interphone and legacy entries.
    #[serde(default)]
    pub radio: Option<Radio>,
    /// Tuned frequency for background airport chatter.
    #[serde(default)]
    pub frequency: Option<i32>,
    /// Silence after this line before the next background chatter line.
    #[serde(default)]
    pub gap_ms: u32,
    /// An advisory may interrupt speech without replacing a pending instruction.
    #[serde(default)]
    pub information_only: bool,
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

impl Transmission {
    /// The latest instruction remains actionable across informational interruptions.
    #[must_use]
    pub fn is_controller_instruction(&self) -> bool {
        self.speaker == "ATC" && !self.background && !self.information_only
    }
}

/// Aircraft radio selected for an exchange; older clients use COM1.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Radio {
    /// First VHF radio.
    #[default]
    Com1,
    /// Second VHF radio.
    Com2,
}

impl Telemetry {
    /// Return the selected channel without substituting another radio.
    #[must_use]
    pub fn frequency(&self, radio: Radio) -> Option<i32> {
        match radio {
            Radio::Com1 => Some(self.com1_khz).filter(|frequency| *frequency > 0),
            Radio::Com2 => self.com2_khz.filter(|frequency| *frequency > 0),
        }
    }
}

/// Pilot request submitted to the controller.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Request {
    /// Radio carrying this request; interphone roles do not use it.
    pub radio: Radio,
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
    /// Radio carrying the exchange, or no radio for crew interphone.
    pub radio: Option<Radio>,
    /// Tuned frequency for background airport chatter.
    pub frequency: Option<i32>,
    /// Silence after this line before the next background chatter line.
    pub gap_ms: u32,
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
            radio: None,
            frequency: None,
            gap_ms: 0,
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

/// Departure sequencing state at the runway holding point.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TakeoffState {
    /// Not in the takeoff sequence.
    #[default]
    Idle,
    /// At the holding point; entry to the runway is not authorized.
    Held,
    /// Authorized to enter the runway and wait.
    LineUpAndWait,
    /// Cleared for takeoff.
    Cleared,
}

/// Approved taxi path.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default)]
#[allow(clippy::struct_excessive_bools)]
pub struct TaxiClearance {
    /// Arrival stand is occupied or its availability cannot be confirmed.
    pub parking_hold: bool,
    /// Temporary ATC hold for conflicting taxi traffic; runway permissions are unchanged.
    pub traffic_hold: bool,
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
    /// Visual target on the approach side of the scenery holding line.
    pub holding_marker_point: Option<Point>,
    /// Traffic or missing traffic data currently blocks departure.
    pub waiting_for_traffic: bool,
    /// Where the departure stands in the runway takeoff sequence.
    pub takeoff_state: TakeoffState,
    /// Movement window used when re-checking a traffic hold; zero uses the fixed envelope.
    #[serde(default)]
    pub waiting_seconds: f64,
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

/// Surface permission lifecycle, derived from the saved clearance fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceStage {
    /// No permission to follow a route.
    Idle,
    /// Instructions issued; a correct readback is still needed.
    AwaitingReadback,
    /// Following an acknowledged taxi clearance.
    Taxiing,
    /// Crossing the one explicitly authorized runway.
    Crossing,
    /// At the authorized endpoint, waiting for the next instruction.
    AtHold,
    /// Fully clear of the crossed runway; onward taxi must be issued.
    CrossingComplete,
}

impl TaxiClearance {
    /// Readback always precedes movement monitoring, including after a crossing.
    #[must_use]
    pub fn stage(&self) -> SurfaceStage {
        if self.pending_readback {
            return SurfaceStage::AwaitingReadback;
        }
        if !self.approved {
            return SurfaceStage::Idle;
        }
        match (self.crossing_runway.is_empty(), self.guidance_complete) {
            (true, false) => SurfaceStage::Taxiing,
            (false, false) => SurfaceStage::Crossing,
            (true, true) => SurfaceStage::AtHold,
            (false, true) => SurfaceStage::CrossingComplete,
        }
    }
}

/// Airport location requested by the flight planner for a surface observation.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WeatherAirport {
    /// Airport whose surface weather is needed for flight planning.
    pub airport: String,
    /// Sampling latitude in degrees.
    pub latitude: f64,
    /// Sampling longitude in degrees.
    pub longitude: f64,
    /// Airport elevation in feet MSL.
    pub elevation_feet: f64,
}

/// Flight session state shared by the engine and UI.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)] // Independent saved permissions and flight flags.
pub struct State {
    /// Version of the engine that produced this snapshot.
    #[serde(default)]
    pub version: String,
    /// Bounded airport weather requests serviced by the simulator main thread.
    #[serde(default)]
    pub weather_airports: Vec<WeatherAirport>,
    /// Radio carrying the flight's current controller clearances and handoffs.
    #[serde(default)]
    pub control_radio: Radio,
    /// Generated models ready for simulator rendering; observations remain in telemetry.
    #[serde(default)]
    pub generated_traffic: Vec<GeneratedTraffic>,
    /// Next hold of each generated ground aircraft, keyed by aircraft id, latitude then
    /// longitude. Separate from `generated_traffic` because the plugin does not need it.
    #[serde(default)]
    pub generated_holding_points: std::collections::BTreeMap<u32, [f64; 2]>,
    /// Simulator sample clock and session identity for buffered rendering.
    #[serde(default)]
    pub generated_traffic_clock: Option<(u64, f64)>,
    /// Validated per-instance animation channel mapping supplied by the engine.
    #[serde(default)]
    pub generated_animation_channels:
        std::collections::BTreeMap<String, crate::traffic::animation::Channel>,
    /// Frequencies with a live synthetic airport-chatter session.
    #[serde(default)]
    pub chatter_frequencies: std::collections::BTreeSet<i32>,
    /// Bounded model and route diagnostics for the generated fleet.
    #[serde(default)]
    pub generated_traffic_diagnostics: Vec<String>,
    /// Per-aircraft generated-traffic debug state.
    #[serde(default)]
    pub generated_traffic_debug: Vec<TrafficDebug>,
    /// Remaining observed departure wake delay; zero does not override runway occupancy.
    #[serde(default)]
    pub departure_wake_seconds: f64,
    /// Required approach spacing was not present when the preceding aircraft crossed the threshold.
    #[serde(default)]
    pub arrival_wake_conflict: bool,
    /// Traffic advisory awaiting an optional pilot sighting report.
    #[serde(default)]
    pub traffic_call: Option<TrafficCall>,
    /// Optional offer; accepting it requests a separately validated clearance.
    #[serde(default)]
    pub atc_offer: Option<AtcOffer>,
    /// The pilot has declared an emergency on this flight.
    #[serde(default)]
    pub emergency_declared: bool,
    /// Active mission and story outcomes, separate from aircraft permissions.
    #[serde(default)]
    pub mission: crate::missions::Status,
    /// Pending simulator-thread crew actions.
    #[serde(default)]
    pub crew_actions: Vec<crate::crew::Action>,
    /// Wired ramp interphone has disconnected for taxi or flight.
    pub ground_interphone_disconnected: bool,
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
    /// Shared aircraft state for unified pilot/traffic behaviour.
    #[serde(default)]
    pub aircraft: crate::aircraft::AircraftState,
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
            version: String::new(),
            weather_airports: Vec::new(),
            control_radio: Radio::Com1,
            generated_traffic: Vec::new(),
            generated_holding_points: std::collections::BTreeMap::new(),
            generated_traffic_clock: None,
            generated_animation_channels: std::collections::BTreeMap::default(),
            chatter_frequencies: std::collections::BTreeSet::new(),
            generated_traffic_diagnostics: Vec::new(),
            generated_traffic_debug: Vec::new(),
            traffic_call: None,
            departure_wake_seconds: 0.0,
            arrival_wake_conflict: false,
            mission: crate::missions::Status::default(),
            atc_offer: None,
            emergency_declared: false,
            crew_actions: Vec::new(),
            ground_interphone_disconnected: false,
            crew_controls: Vec::new(),
            clearance_cancelled: false,
            plan: FlightPlan::default(),
            telemetry: Telemetry::default(),
            phase: PhaseCode::Parked,
            aircraft: crate::aircraft::AircraftState::default(),
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

impl State {
    /// Actual channel of the controlling radio, without substituting another receiver.
    #[must_use]
    pub fn control_frequency(&self) -> Option<i32> {
        self.telemetry.frequency(self.control_radio)
    }

    /// Choose one receiver for a shared broadcast, even when both radios are tuned to it.
    #[must_use]
    pub fn receiving_radio(&self, frequency: i32) -> Option<Radio> {
        [self.control_radio, Radio::Com1, Radio::Com2]
            .into_iter()
            .find(|radio| self.telemetry.frequency(*radio) == Some(frequency))
    }

    /// Keep an assigned frequency change on the radio that received it.
    #[must_use]
    pub fn handoff_radio(&self) -> Radio {
        if self.frequency_sequence == 0 {
            return self.control_radio;
        }
        self.transcript
            .iter()
            .rev()
            .find(|entry| entry.sequence == self.frequency_sequence)
            .and_then(|entry| entry.radio)
            .unwrap_or(self.control_radio)
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

/// A time-limited offer belongs to one controller and flight.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AtcOffer {
    /// Transmission carrying the offer.
    pub sequence: u32,
    /// Controller frequency in kHz.
    pub frequency_khz: i32,
    /// Known downstream route fix.
    pub waypoint: String,
}

impl crate::aircraft::AircraftBehaviour for State {
    fn state(&self) -> &crate::aircraft::AircraftState {
        &self.aircraft
    }

    fn state_mut(&mut self) -> &mut crate::aircraft::AircraftState {
        &mut self.aircraft
    }

    fn on_event(
        &mut self,
        event: crate::aircraft::AircraftEvent,
    ) -> crate::aircraft::AircraftOutcome {
        use crate::aircraft::{AircraftEvent, AircraftOutcome, AircraftPhase};
        match event {
            AircraftEvent::IfrClearanceReceived => {
                self.phase = PhaseCode::Clearance;
                self.aircraft.phase = AircraftPhase::IfrClearance;
                AircraftOutcome::Advanced(AircraftPhase::IfrClearance)
            }
            AircraftEvent::StartupComplete => {
                self.aircraft.phase = AircraftPhase::Startup;
                AircraftOutcome::Advanced(AircraftPhase::Startup)
            }
            AircraftEvent::PushbackComplete => {
                self.phase = PhaseCode::Pushback;
                self.pushback_approved = true;
                self.aircraft.phase = AircraftPhase::Pushback;
                AircraftOutcome::Advanced(AircraftPhase::Pushback)
            }
            AircraftEvent::Arrival => {
                self.phase = PhaseCode::Arrival;
                self.aircraft.phase = AircraftPhase::Arrival;
                AircraftOutcome::Advanced(AircraftPhase::Arrival)
            }
            AircraftEvent::Departure => {
                self.phase = PhaseCode::Departure;
                self.aircraft.phase = AircraftPhase::Departure;
                AircraftOutcome::Advanced(AircraftPhase::Departure)
            }
            AircraftEvent::Approach => {
                self.phase = PhaseCode::Approach;
                self.aircraft.phase = AircraftPhase::Approach;
                AircraftOutcome::Advanced(AircraftPhase::Approach)
            }
            AircraftEvent::Cruise => {
                self.phase = PhaseCode::Cruise;
                self.aircraft.phase = AircraftPhase::Cruise;
                AircraftOutcome::Advanced(AircraftPhase::Cruise)
            }
            AircraftEvent::Landed => {
                self.phase = PhaseCode::Landed;
                self.aircraft.phase = AircraftPhase::Landed;
                AircraftOutcome::Advanced(AircraftPhase::Landed)
            }
            AircraftEvent::Taxi => {
                self.phase = PhaseCode::Taxi;
                self.aircraft.phase = AircraftPhase::Taxi;
                AircraftOutcome::Advanced(AircraftPhase::Taxi)
            }
            AircraftEvent::TaxiIn => {
                self.phase = PhaseCode::TaxiIn;
                self.aircraft.phase = AircraftPhase::TaxiIn;
                AircraftOutcome::Advanced(AircraftPhase::TaxiIn)
            }
            AircraftEvent::Finished => {
                self.phase = PhaseCode::Finished;
                self.aircraft.phase = AircraftPhase::Finished;
                AircraftOutcome::Advanced(AircraftPhase::Finished)
            }
            AircraftEvent::Parked => {
                self.phase = PhaseCode::Parked;
                self.aircraft.phase = AircraftPhase::Parked;
                AircraftOutcome::Advanced(AircraftPhase::Parked)
            }
            _ => AircraftOutcome::None,
        }
    }

    fn advance(&mut self, seconds: f64) -> crate::aircraft::AircraftOutcome {
        self.aircraft.last_progress_seconds = seconds;
        crate::aircraft::AircraftOutcome::None
    }
}
