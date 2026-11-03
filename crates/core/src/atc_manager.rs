//! Aircraft registry and ATC frequency lifecycle.
//!
//! The manager deliberately separates aircraft identity from frequency membership. Removing an
//! aircraft from the registry does not silently remove it from a frequency; call
//! [`AtcManager::leave_all`] when the aircraft no longer exists.

use crate::stations::Station;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

/// Reserved identity for the player's own aircraft. Simulator traffic uses non-zero Mode S ids.
pub const PILOT_ID: u32 = 0;

/// Distinguishes the player's aircraft from simulated traffic without downcasting.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum AircraftKind {
    /// The player's own aircraft.
    #[default]
    Pilot,
    /// A simulated or simulator-provided traffic aircraft.
    Traffic,
}

impl AircraftKind {
    /// Stable lower-case label used by debug output.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pilot => "pilot",
            Self::Traffic => "traffic",
        }
    }
}

/// Common identity shared by every aircraft in the registry.
pub trait Aircraft: Send + Sync {
    /// Stable identity. The player uses [`PILOT_ID`]; traffic uses its Mode S id.
    fn id(&self) -> u32;
    /// Callsign used on the radio.
    fn callsign(&self) -> &str;
    /// Whether this is the player or simulated traffic.
    fn kind(&self) -> AircraftKind;

    /// Convenience check for a pilot aircraft.
    fn is_pilot(&self) -> bool {
        self.kind() == AircraftKind::Pilot
    }

    /// Convenience check for a traffic aircraft.
    fn is_traffic(&self) -> bool {
        self.kind() == AircraftKind::Traffic
    }
}

/// Marker trait for the player's aircraft.
pub trait Pilot: Aircraft {}

/// Marker trait for simulated traffic.
pub trait Traffic: Aircraft {}

/// The player's own aircraft.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PilotAircraft {
    id: u32,
    callsign: String,
}

impl PilotAircraft {
    /// Create a pilot aircraft with a stable id and callsign.
    #[must_use]
    pub fn new(id: u32, callsign: impl Into<String>) -> Self {
        Self {
            id,
            callsign: callsign.into(),
        }
    }
}

impl Aircraft for PilotAircraft {
    fn id(&self) -> u32 {
        self.id
    }

    fn callsign(&self) -> &str {
        &self.callsign
    }

    fn kind(&self) -> AircraftKind {
        AircraftKind::Pilot
    }
}

impl Pilot for PilotAircraft {}

/// A simulated traffic aircraft.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrafficAircraft {
    id: u32,
    callsign: String,
}

impl TrafficAircraft {
    /// Create a traffic aircraft with a stable id and callsign.
    #[must_use]
    pub fn new(id: u32, callsign: impl Into<String>) -> Self {
        Self {
            id,
            callsign: callsign.into(),
        }
    }
}

impl Aircraft for TrafficAircraft {
    fn id(&self) -> u32 {
        self.id
    }

    fn callsign(&self) -> &str {
        &self.callsign
    }

    fn kind(&self) -> AircraftKind {
        AircraftKind::Traffic
    }
}

impl Traffic for TrafficAircraft {}

/// Registry of aircraft known to the engine.
#[derive(Default)]
pub struct AircraftManager {
    aircraft: HashMap<u32, Arc<dyn Aircraft>>,
}

impl AircraftManager {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an aircraft, replacing any previous entry with the same id.
    pub fn register(&mut self, aircraft: Arc<dyn Aircraft>) -> Option<Arc<dyn Aircraft>> {
        self.aircraft.insert(aircraft.id(), aircraft)
    }

    /// Remove an aircraft from the registry.
    pub fn unregister(&mut self, id: u32) -> Option<Arc<dyn Aircraft>> {
        self.aircraft.remove(&id)
    }

    /// Look up an aircraft by id.
    #[must_use]
    pub fn get(&self, id: u32) -> Option<Arc<dyn Aircraft>> {
        self.aircraft.get(&id).cloned()
    }

    /// Whether an aircraft id is registered.
    #[must_use]
    pub fn contains(&self, id: u32) -> bool {
        self.aircraft.contains_key(&id)
    }

    /// Number of registered aircraft.
    #[must_use]
    pub fn len(&self) -> usize {
        self.aircraft.len()
    }

    /// Whether the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.aircraft.is_empty()
    }

    /// Iterate over every registered aircraft in unspecified order.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Aircraft>> {
        self.aircraft.values()
    }

    /// Iterate over pilot aircraft only.
    pub fn pilots(&self) -> impl Iterator<Item = &Arc<dyn Aircraft>> {
        self.aircraft
            .values()
            .filter(|aircraft| aircraft.is_pilot())
    }

    /// Iterate over traffic aircraft only.
    pub fn traffic(&self) -> impl Iterator<Item = &Arc<dyn Aircraft>> {
        self.aircraft
            .values()
            .filter(|aircraft| aircraft.is_traffic())
    }
}

/// One frequency and the aircraft currently on it.
#[derive(Default)]
pub struct FrequencyChannel {
    /// Published station, retained for display and delivery.
    pub station: Station,
    /// Aircraft currently tuned to this frequency, keyed by id.
    pub aircraft: HashMap<u32, Arc<dyn Aircraft>>,
    /// Most recent exchange on this frequency, retained for debug output.
    pub last: Option<AtcMessage>,
}

impl FrequencyChannel {
    /// Number of aircraft on the frequency.
    #[must_use]
    pub fn aircraft_count(&self) -> usize {
        self.aircraft.len()
    }

    /// Whether the frequency has no aircraft.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.aircraft.is_empty()
    }
}

/// A voice and text exchange transmitted on one frequency.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AtcMessage {
    /// Frequency in kHz.
    pub frequency_khz: i32,
    /// Aircraft id that transmitted.
    pub aircraft_id: u32,
    /// Callsign of the transmitting aircraft.
    pub speaker: String,
    /// Displayed or logged text.
    pub text: String,
    /// Voice identifier used for speech synthesis.
    pub voice: String,
}

/// Owns every active ATC frequency and the aircraft tuned to it.
#[derive(Default)]
pub struct AtcManager {
    frequencies: HashMap<i32, FrequencyChannel>,
}

impl AtcManager {
    /// Create an empty manager.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Transmit text and voice on a frequency, creating it when necessary.
    ///
    /// The aircraft is joined to the frequency before the message is returned. Delivery to other
    /// listeners is the caller's responsibility so the manager stays independent of speech and
    /// transcript state.
    pub fn transmit(
        &mut self,
        frequency: i32,
        station: Station,
        aircraft: Arc<dyn Aircraft>,
        text: &str,
        voice: &str,
    ) -> AtcMessage {
        let aircraft_id = aircraft.id();
        let speaker = aircraft.callsign().to_owned();
        let message = AtcMessage {
            frequency_khz: frequency,
            aircraft_id,
            speaker,
            text: text.to_owned(),
            voice: voice.to_owned(),
        };
        let channel = self.channel_for(frequency, station);
        channel.aircraft.insert(aircraft_id, aircraft);
        channel.last = Some(message.clone());
        message
    }

    /// Join an aircraft to a frequency without transmitting.
    pub fn join(&mut self, frequency: i32, station: Station, aircraft: Arc<dyn Aircraft>) {
        let aircraft_id = aircraft.id();
        self.channel_for(frequency, station)
            .aircraft
            .insert(aircraft_id, aircraft);
    }

    /// Remove an aircraft from a frequency.
    ///
    /// Returns `true` when the removal left the frequency empty and the channel was deleted.
    pub fn leave(&mut self, frequency: i32, aircraft_id: u32) -> bool {
        let Some(channel) = self.frequencies.get_mut(&frequency) else {
            return false;
        };
        channel.aircraft.remove(&aircraft_id);
        if channel.is_empty() {
            self.frequencies.remove(&frequency);
            true
        } else {
            false
        }
    }

    /// Remove an aircraft from every frequency, deleting channels it leaves empty.
    pub fn leave_all(&mut self, aircraft_id: u32) {
        self.leave_all_except(aircraft_id, &[]);
    }

    /// Remove an aircraft from every frequency except the supplied ones.
    ///
    /// Kept frequencies are not created. Empty channels are deleted.
    pub fn leave_all_except(&mut self, aircraft_id: u32, keep: &[i32]) {
        let mut empty = Vec::new();
        for (frequency, channel) in &mut self.frequencies {
            if keep.contains(frequency) {
                continue;
            }
            channel.aircraft.remove(&aircraft_id);
            if channel.is_empty() {
                empty.push(*frequency);
            }
        }
        for frequency in empty {
            self.frequencies.remove(&frequency);
        }
    }

    /// Look up a frequency channel.
    #[must_use]
    pub fn channel(&self, frequency: i32) -> Option<&FrequencyChannel> {
        self.frequencies.get(&frequency)
    }

    /// Look up an aircraft on one frequency.
    #[must_use]
    pub fn aircraft_from_id(&self, frequency: i32, aircraft_id: u32) -> Option<Arc<dyn Aircraft>> {
        self.frequencies
            .get(&frequency)?
            .aircraft
            .get(&aircraft_id)
            .cloned()
    }

    /// Frequencies currently held by the manager, in unspecified order.
    pub fn frequencies(&self) -> impl Iterator<Item = i32> + '_ {
        self.frequencies.keys().copied()
    }

    /// Channels currently held by the manager, in unspecified order.
    pub fn channels(&self) -> impl Iterator<Item = (i32, &FrequencyChannel)> + '_ {
        self.frequencies
            .iter()
            .map(|(frequency, channel)| (*frequency, channel))
    }

    /// Number of active frequencies.
    #[must_use]
    pub fn frequency_count(&self) -> usize {
        self.frequencies.len()
    }

    /// Number of aircraft on one frequency.
    #[must_use]
    pub fn aircraft_count(&self, frequency: i32) -> usize {
        self.frequencies
            .get(&frequency)
            .map_or(0, FrequencyChannel::aircraft_count)
    }

    /// Whether the manager holds no frequencies.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.frequencies.is_empty()
    }

    fn channel_for(&mut self, frequency: i32, station: Station) -> &mut FrequencyChannel {
        let channel = self
            .frequencies
            .entry(frequency)
            .or_insert_with(|| FrequencyChannel {
                station: station.clone(),
                ..FrequencyChannel::default()
            });
        channel.station = station;
        channel
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn station(khz: i32, name: &str) -> Station {
        Station {
            khz,
            name: name.into(),
            service: "Tower".into(),
            airport: "TEST".into(),
            ..Station::default()
        }
    }

    fn pilot(callsign: &str) -> Arc<dyn Aircraft> {
        Arc::new(PilotAircraft::new(PILOT_ID, callsign))
    }

    fn traffic(id: u32, callsign: &str) -> Arc<dyn Aircraft> {
        Arc::new(TrafficAircraft::new(id, callsign))
    }

    #[test]
    fn transmit_creates_a_frequency_and_registers_the_aircraft() {
        let mut manager = AtcManager::new();
        let aircraft = pilot("TEST123");
        let message = manager.transmit(
            118_700,
            station(118_700, "Test Tower"),
            aircraft,
            "cleared to land",
            "tower-voice",
        );

        assert_eq!(message.frequency_khz, 118_700);
        assert_eq!(message.speaker, "TEST123");
        assert_eq!(message.text, "cleared to land");
        assert_eq!(message.voice, "tower-voice");
        assert_eq!(manager.frequency_count(), 1);
        assert_eq!(manager.aircraft_count(118_700), 1);
        assert_eq!(
            manager
                .aircraft_from_id(118_700, PILOT_ID)
                .expect("aircraft joined")
                .kind(),
            AircraftKind::Pilot
        );
    }

    #[test]
    fn transmit_reuses_an_existing_frequency_and_updates_station_metadata() {
        let mut manager = AtcManager::new();
        manager.transmit(
            121_900,
            station(121_900, "Old Ground"),
            pilot("TEST123"),
            "taxi",
            "ground-voice",
        );
        manager.transmit(
            121_900,
            station(121_900, "New Ground"),
            traffic(7, "TRAFFIC7"),
            "hold short",
            "ground-voice",
        );

        assert_eq!(manager.frequency_count(), 1);
        assert_eq!(manager.aircraft_count(121_900), 2);
        assert_eq!(
            manager.channel(121_900).expect("channel").station.name,
            "New Ground"
        );
    }

    #[test]
    fn leaving_the_last_aircraft_removes_the_frequency() {
        let mut manager = AtcManager::new();
        manager.join(118_700, station(118_700, "Test Tower"), pilot("TEST123"));

        assert!(manager.leave(118_700, PILOT_ID));
        assert_eq!(manager.frequency_count(), 0);
        assert!(manager.channel(118_700).is_none());
        assert!(!manager.leave(118_700, PILOT_ID));
    }

    #[test]
    fn a_frequency_survives_until_its_last_aircraft_leaves() {
        let mut manager = AtcManager::new();
        manager.join(118_700, station(118_700, "Test Tower"), pilot("TEST123"));
        manager.join(
            118_700,
            station(118_700, "Test Tower"),
            traffic(7, "TRAFFIC7"),
        );

        assert!(!manager.leave(118_700, PILOT_ID));
        assert_eq!(manager.aircraft_count(118_700), 1);
        assert!(manager.leave(118_700, 7));
        assert_eq!(manager.frequency_count(), 0);
    }

    #[test]
    fn leave_all_except_keeps_only_the_requested_frequency() {
        let mut manager = AtcManager::new();
        let aircraft = pilot("TEST123");
        manager.join(118_700, station(118_700, "Test Tower"), aircraft.clone());
        manager.join(121_900, station(121_900, "Test Ground"), aircraft);

        manager.leave_all_except(PILOT_ID, &[121_900]);

        assert_eq!(manager.frequency_count(), 1);
        assert!(manager.channel(118_700).is_none());
        assert_eq!(manager.aircraft_count(121_900), 1);
    }

    #[test]
    fn aircraft_manager_replaces_and_unregisters_by_id() {
        let mut manager = AircraftManager::new();
        manager.register(pilot("TEST123"));
        manager.register(traffic(7, "TRAFFIC7"));

        assert_eq!(manager.len(), 2);
        assert_eq!(manager.pilots().count(), 1);
        assert_eq!(manager.traffic().count(), 1);

        manager.register(pilot("TEST124"));
        assert_eq!(manager.len(), 2);
        assert_eq!(manager.get(PILOT_ID).expect("pilot").callsign(), "TEST124");

        assert!(manager.unregister(7).is_some());
        assert!(!manager.contains(7));
        assert_eq!(manager.len(), 1);
    }

    #[test]
    fn pilot_and_traffic_aircraft_report_distinct_kinds() {
        let pilot: Arc<dyn Aircraft> = pilot("TEST123");
        let traffic: Arc<dyn Aircraft> = traffic(7, "TRAFFIC7");

        assert_eq!(pilot.kind(), AircraftKind::Pilot);
        assert!(pilot.is_pilot());
        assert!(!pilot.is_traffic());
        assert_eq!(traffic.kind(), AircraftKind::Traffic);
        assert!(traffic.is_traffic());
        assert!(!traffic.is_pilot());
    }
}
