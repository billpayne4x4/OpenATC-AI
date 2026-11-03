//! Frequency-scoped traffic services.
//!
//! A station is one published frequency. It owns every role lane on that frequency,
//! one watch set, one clearance queue and one transmitter. Different frequencies run
//! in separate OS threads; same-frequency roles cannot transmit over each other.

#![allow(dead_code)]

use openatc_core::state::SpeechTag;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{
        Arc, Mutex, PoisonError,
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// Operational phase used to decide which frequency owns a session.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Phase {
    AtGate,
    IfrClearance,
    Pushback,
    Start,
    Taxi,
    Hold,
    Crossing,
    LineUp,
    Takeoff,
    Airborne,
    Approach,
    Landing,
    TaxiIn,
    AtGateIn,
}

/// A phase transition or hold-line crossing. The timestamp is simulator time.
#[derive(Clone, Debug)]
pub(crate) struct Trigger {
    pub(crate) session: u32,
    pub(crate) phase: Phase,
    pub(crate) frequency: i32,
    pub(crate) at: f64,
}

/// Commands accepted by one frequency thread.
#[derive(Clone, Debug)]
pub(crate) enum StationCommand {
    /// Seed the local watch set when a fresh thread takes over a durable station.
    Restore(Vec<(u32, Phase)>),
    Watch {
        session: u32,
        phase: Phase,
    },
    Trigger(Trigger),
    Release {
        session: u32,
    },
    Transmit(Transmission),
    Shutdown,
}

/// One transmission waiting on a frequency. The station thread owns its timing so a second
/// transmission on the same frequency cannot overlap it.
#[derive(Clone, Debug)]
pub(crate) struct Transmission {
    pub(crate) session: u32,
    pub(crate) speaker: String,
    pub(crate) text: String,
    pub(crate) controller: bool,
    pub(crate) tag: SpeechTag,
    pub(crate) due: Instant,
}

/// Events published by a frequency thread. The radio and movement layers consume these.
#[derive(Clone, Debug)]
pub(crate) enum StationEvent {
    Started {
        frequency: i32,
    },
    Watch {
        frequency: i32,
        session: u32,
    },
    Trigger {
        frequency: i32,
        trigger: Trigger,
    },
    Transmitted {
        frequency: i32,
        transmission: Transmission,
    },
    Closed {
        frequency: i32,
    },
}

struct RunningStation {
    commands: Sender<StationCommand>,
    /// True while the thread accepts commands. The supervisor and the thread take this lock
    /// before deciding whether a command can still be accepted, which closes the race
    /// between an empty queue and a newly arriving aircraft.
    gate: Arc<Mutex<bool>>,
    join: JoinHandle<()>,
}

struct Station {
    frequency: i32,
    events: Sender<StationEvent>,
    running: Option<RunningStation>,
    /// Durable state: it outlives a short-lived worker thread.
    watch: BTreeMap<u32, Phase>,
    /// Commands that arrived after the previous worker closed its gate.
    pending: VecDeque<StationCommand>,
}

/// Owns one durable station per demanded frequency. The station record survives; the OS
/// thread does not. A thread exits as soon as its trigger and transmission queues are empty,
/// and the next aircraft, trigger or transmission starts a fresh thread with the restored
/// watch set.
#[derive(Default)]
pub(crate) struct StationSupervisor {
    stations: BTreeMap<i32, Station>,
}

impl StationSupervisor {
    /// Number of station worker threads currently alive.
    pub(crate) fn live(&self) -> usize {
        self.stations
            .values()
            .filter(|station| station.running.is_some())
            .count()
    }

    /// Number of durable station records, including idle frequencies.
    pub(crate) fn stations(&self) -> usize {
        self.stations.len()
    }

    /// Start a frequency worker if it is not already running, keeping the station record.
    pub(crate) fn demand(&mut self, frequency: i32, events: Sender<StationEvent>) {
        self.station(frequency, events);
        self.spawn(frequency);
    }

    fn station(&mut self, frequency: i32, events: Sender<StationEvent>) -> &mut Station {
        self.stations.entry(frequency).or_insert_with(|| Station {
            frequency,
            events,
            running: None,
            watch: BTreeMap::new(),
            pending: VecDeque::new(),
        })
    }

    /// Assign a session to a frequency, starting a worker if the station is idle.
    pub(crate) fn watch(
        &mut self,
        frequency: i32,
        session: u32,
        phase: Phase,
        events: Sender<StationEvent>,
    ) {
        let station = self.station(frequency, events);
        station.watch.insert(session, phase);
        self.send(frequency, StationCommand::Watch { session, phase });
    }

    /// Deliver a trigger immediately to the owning frequency. A trigger for a released
    /// session is ignored rather than waking a dead station.
    pub(crate) fn trigger(&mut self, trigger: Trigger) {
        let Some(station) = self.stations.get(&trigger.frequency) else {
            return;
        };
        if station.watch.get(&trigger.session) != Some(&trigger.phase) {
            return;
        }
        self.send(trigger.frequency, StationCommand::Trigger(trigger));
    }

    /// Remove a completed or retired session from a station.
    pub(crate) fn release(&mut self, frequency: i32, session: u32) {
        if let Some(station) = self.stations.get_mut(&frequency) {
            station.watch.remove(&session);
            // An idle station has no worker to notify; the durable watch set is authoritative.
            if station.running.is_some() {
                self.send(frequency, StationCommand::Release { session });
            }
        }
    }

    /// Queue a transmission on its frequency. One station thread serializes the queue.
    pub(crate) fn transmit(&mut self, frequency: i32, transmission: Transmission) {
        self.send(frequency, StationCommand::Transmit(transmission));
    }

    fn send(&mut self, frequency: i32, command: StationCommand) {
        let mut retry = false;
        if let Some(station) = self.stations.get_mut(&frequency) {
            if let Some(running) = &station.running {
                let mut open = running.gate.lock().unwrap_or_else(PoisonError::into_inner);
                if *open {
                    match running.commands.send(command.clone()) {
                        Ok(()) => return,
                        Err(_) => *open = false,
                    }
                }
            }
            station.pending.push_back(command);
            retry = true;
        }
        if retry {
            self.spawn(frequency);
        }
    }

    fn spawn(&mut self, frequency: i32) {
        let Some(station) = self.stations.get_mut(&frequency) else {
            return;
        };
        if let Some(running) = &station.running {
            let open = *running.gate.lock().unwrap_or_else(PoisonError::into_inner);
            if open {
                return;
            }
            if let Some(old) = station.running.take() {
                let _ = old.join.join();
            }
        }
        let (commands, receiver) = mpsc::channel();
        let gate = Arc::new(Mutex::new(true));
        let thread_gate = gate.clone();
        let events = station.events.clone();
        let join = thread::Builder::new()
            .name(format!("openatc-station-{frequency}"))
            .spawn(move || run_station(frequency, receiver, events, thread_gate))
            .expect("station thread");
        let restore = station
            .watch
            .iter()
            .map(|(session, phase)| (*session, *phase))
            .collect();
        let _ = commands.send(StationCommand::Restore(restore));
        for command in station.pending.drain(..) {
            let _ = commands.send(command);
        }
        station.running = Some(RunningStation {
            commands,
            gate,
            join,
        });
    }

    /// Reap workers that have exited. A station with watch state is retained so the next
    /// trigger can start it again; a station with no sessions and no pending work is dropped.
    pub(crate) fn reap(&mut self) {
        let finished: Vec<i32> = self
            .stations
            .iter()
            .filter(|(_, station)| {
                station
                    .running
                    .as_ref()
                    .is_some_and(|running| running.join.is_finished())
            })
            .map(|(frequency, _)| *frequency)
            .collect();
        for frequency in finished {
            let remove = if let Some(station) = self.stations.get_mut(&frequency) {
                if let Some(running) = station.running.take() {
                    let _ = running.join.join();
                }
                station.watch.is_empty() && station.pending.is_empty()
            } else {
                false
            };
            if remove {
                self.stations.remove(&frequency);
            }
        }
    }

    /// Stop every station and wait for its thread.
    pub(crate) fn shutdown(&mut self) {
        for (_, mut station) in std::mem::take(&mut self.stations) {
            if let Some(running) = station.running.take() {
                let _ = running.commands.send(StationCommand::Shutdown);
                let _ = running.join.join();
                let _ = station.events.send(StationEvent::Closed {
                    frequency: station.frequency,
                });
            }
        }
    }
}

impl Drop for StationSupervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Apply one command. Returns `None` for shutdown, otherwise whether it queued work.
fn apply_command(
    frequency: i32,
    command: StationCommand,
    watch: &mut BTreeMap<u32, Phase>,
    triggers: &mut VecDeque<Trigger>,
    transmissions: &mut VecDeque<Transmission>,
    events: &Sender<StationEvent>,
) -> Option<bool> {
    match command {
        StationCommand::Restore(sessions) => {
            *watch = sessions.into_iter().collect();
            Some(false)
        }
        StationCommand::Watch { session, phase } => {
            watch.insert(session, phase);
            let _ = events.send(StationEvent::Watch { frequency, session });
            Some(false)
        }
        StationCommand::Trigger(trigger) => {
            if watch.get(&trigger.session) == Some(&trigger.phase) {
                triggers.push_back(trigger);
                Some(true)
            } else {
                Some(false)
            }
        }
        StationCommand::Release { session } => {
            watch.remove(&session);
            Some(false)
        }
        StationCommand::Transmit(transmission) => {
            transmissions.push_back(transmission);
            Some(true)
        }
        StationCommand::Shutdown => None,
    }
}

fn run_station(
    frequency: i32,
    commands: Receiver<StationCommand>,
    events: Sender<StationEvent>,
    gate: Arc<Mutex<bool>>,
) {
    let _ = events.send(StationEvent::Started { frequency });
    let mut watch = BTreeMap::new();
    let mut triggers = VecDeque::new();
    let mut transmissions: VecDeque<Transmission> = VecDeque::new();
    loop {
        let now = Instant::now();
        while let Some(transmission) = transmissions.front() {
            if transmission.due > now {
                break;
            }
            let transmission = transmissions.pop_front().unwrap();
            let _ = events.send(StationEvent::Transmitted {
                frequency,
                transmission,
            });
        }
        while let Some(trigger) = triggers.pop_front() {
            let _ = events.send(StationEvent::Trigger { frequency, trigger });
        }
        if triggers.is_empty() && transmissions.is_empty() {
            // Decide to exit while holding the same gate the supervisor uses to enqueue
            // work. A command that wins the race is drained here; a command that loses it
            // sees the closed gate and starts a fresh worker.
            let mut open = gate.lock().unwrap_or_else(PoisonError::into_inner);
            if !*open {
                break;
            }
            let mut worked = false;
            let mut stop = false;
            while let Ok(command) = commands.try_recv() {
                match apply_command(
                    frequency,
                    command,
                    &mut watch,
                    &mut triggers,
                    &mut transmissions,
                    &events,
                ) {
                    Some(queued) => worked |= queued,
                    None => {
                        stop = true;
                        break;
                    }
                }
            }
            if stop || (!worked && triggers.is_empty() && transmissions.is_empty()) {
                *open = false;
                break;
            }
            drop(open);
            continue;
        }
        let wait = transmissions
            .front()
            .map(|transmission| transmission.due.saturating_duration_since(now))
            .unwrap_or(Duration::from_millis(50))
            .min(Duration::from_millis(50));
        match commands.recv_timeout(wait) {
            Ok(StationCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                if let Ok(mut open) = gate.lock() {
                    *open = false;
                }
                break;
            }
            Ok(command) => {
                let _ = apply_command(
                    frequency,
                    command,
                    &mut watch,
                    &mut triggers,
                    &mut transmissions,
                    &events,
                );
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
    let _ = events.send(StationEvent::Closed { frequency });
}

/// Owns the live frequency threads and forwards their transmissions to the engine.
///
/// This layer orders speech only. Movement authority is still granted by the flight and radio
/// paths after the runway-aware separation check, so a station thread cannot clear an aircraft
/// onto a runway on its own.
pub(crate) struct StationBus {
    supervisor: StationSupervisor,
    events: Sender<StationEvent>,
    received: Mutex<Receiver<StationEvent>>,
    assigned: BTreeMap<(i32, u32), Phase>,
    triggers: VecDeque<Trigger>,
}

impl Default for StationBus {
    fn default() -> Self {
        let (events, received) = mpsc::channel();
        Self {
            supervisor: StationSupervisor::default(),
            events,
            received: Mutex::new(received),
            assigned: BTreeMap::new(),
            triggers: VecDeque::new(),
        }
    }
}

impl StationBus {
    pub(crate) fn assign(&mut self, frequency: i32, session: u32, phase: Phase) {
        self.supervisor
            .watch(frequency, session, phase, self.events.clone());
    }

    pub(crate) fn release(&mut self, frequency: i32, session: u32) {
        self.supervisor.release(frequency, session);
    }

    pub(crate) fn trigger(&mut self, trigger: Trigger) {
        self.supervisor.trigger(trigger);
    }

    /// Reconcile the durable station watch sets with the sessions that currently exist.
    /// A new aircraft or phase transition starts an idle station; a vanished session releases
    /// it. The station worker exits again as soon as its trigger and transmission queues drain.
    pub(crate) fn sync_sessions(&mut self, sessions: Vec<(u32, i32, Phase)>, at: f64) {
        let desired: BTreeMap<(i32, u32), Phase> = sessions
            .into_iter()
            .map(|(session, frequency, phase)| ((frequency, session), phase))
            .collect();
        let stale: Vec<(i32, u32)> = self
            .assigned
            .keys()
            .filter(|key| !desired.contains_key(key))
            .copied()
            .collect();
        for (frequency, session) in stale {
            self.supervisor.release(frequency, session);
        }
        for (&(frequency, session), &phase) in &desired {
            if self.assigned.get(&(frequency, session)) == Some(&phase) {
                continue;
            }
            self.supervisor
                .watch(frequency, session, phase, self.events.clone());
            self.supervisor.trigger(Trigger {
                session,
                phase,
                frequency,
                at,
            });
        }
        self.assigned = desired;
    }

    /// Drain phase triggers produced by station workers. The selector consumes these on the
    /// next pass so a hold-line or phase transition does not wait for a polling interval.
    pub(crate) fn take_triggers(&mut self) -> Vec<Trigger> {
        self.triggers.drain(..).collect()
    }

    pub(crate) fn transmit(&mut self, frequency: i32, transmission: Transmission) {
        self.supervisor.demand(frequency, self.events.clone());
        self.supervisor.transmit(frequency, transmission);
    }

    /// Drain transmission events. Started/Watch/Trigger/Closed events remain owned by the
    /// station layer and are ignored here.
    pub(crate) fn drain(&mut self) -> Vec<(i32, Transmission)> {
        self.supervisor.reap();
        let mut transmissions = Vec::new();
        let received = self.received.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            match received.try_recv() {
                Ok(StationEvent::Transmitted {
                    frequency,
                    transmission,
                }) => transmissions.push((frequency, transmission)),
                Ok(StationEvent::Trigger { trigger, .. }) => self.triggers.push_back(trigger),
                Ok(_) => {}
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
            }
        }
        transmissions
    }

    /// Wait briefly for a just-queued transmission so the transcript keeps its original
    /// controller-then-pilot order in synchronous callers.
    pub(crate) fn flush(&mut self, timeout: Duration) -> Vec<(i32, Transmission)> {
        self.supervisor.reap();
        let mut transmissions = self.drain();
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let received = self.received.lock().unwrap_or_else(PoisonError::into_inner);
            match received.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(StationEvent::Transmitted {
                    frequency,
                    transmission,
                }) => transmissions.push((frequency, transmission)),
                Ok(StationEvent::Trigger { trigger, .. }) => self.triggers.push_back(trigger),
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
            }
        }
        self.supervisor.reap();
        transmissions
    }

    /// Expose the running thread count for health checks and tests.
    pub(crate) fn live(&self) -> usize {
        self.supervisor.live()
    }
}

impl Drop for StationBus {
    fn drop(&mut self) {
        self.supervisor.shutdown();
    }
}

/// One aircraft's authoritative phase and liveness record.
#[derive(Clone, Debug)]
pub(crate) struct SessionState {
    pub(crate) frequency: i32,
    pub(crate) phase: Phase,
    pub(crate) last_progress: f64,
    pub(crate) last_phase_change: f64,
    recoveries: u8,
    pub(crate) aircraft: openatc_core::aircraft::AircraftState,
}

#[derive(Clone, Default)]
pub(crate) struct SessionRegistry {
    sessions: BTreeMap<u32, SessionState>,
}

impl SessionRegistry {
    pub(crate) fn add(&mut self, id: u32, frequency: i32, phase: Phase, at: f64) {
        self.sessions.insert(
            id,
            SessionState {
                frequency,
                phase,
                last_progress: at,
                last_phase_change: at,
                recoveries: 0,
                aircraft: openatc_core::aircraft::AircraftState::default(),
            },
        );
    }

    pub(crate) fn phase(&mut self, id: u32, phase: Phase, at: f64) {
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };
        if session.phase != phase {
            session.phase = phase;
            session.last_phase_change = at;
            session.last_progress = at;
            session.recoveries = 0;
        }
    }

    pub(crate) fn progress(&mut self, id: u32, at: f64) {
        if let Some(session) = self.sessions.get_mut(&id) {
            session.last_progress = at;
            session.recoveries = 0;
        }
    }

    pub(crate) fn retire(&mut self, id: u32) -> Option<SessionState> {
        self.sessions.remove(&id)
    }

    pub(crate) fn retain(&mut self, ids: &BTreeSet<u32>) {
        self.sessions.retain(|id, _| ids.contains(id));
    }

    pub(crate) fn contains(&self, id: u32) -> bool {
        self.sessions.contains_key(&id)
    }

    pub(crate) fn watch_set(&self, frequency: i32) -> BTreeSet<u32> {
        self.sessions
            .iter()
            .filter(|(_, session)| session.frequency == frequency)
            .map(|(id, _)| *id)
            .collect()
    }

    pub(crate) fn sessions(&self) -> impl Iterator<Item = (&u32, &SessionState)> {
        self.sessions.iter()
    }
}

/// Escalation for a session that has stopped producing progress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HealthAction {
    Retrigger(u32),
    Release(u32),
    Retire(u32),
}

#[derive(Clone)]
pub(crate) struct TrafficHealth {
    deadlines: BTreeMap<Phase, f64>,
}

impl Default for TrafficHealth {
    fn default() -> Self {
        let mut deadlines = BTreeMap::new();
        deadlines.insert(Phase::IfrClearance, 300.0);
        deadlines.insert(Phase::Pushback, 300.0);
        deadlines.insert(Phase::Start, 300.0);
        deadlines.insert(Phase::Taxi, 300.0);
        deadlines.insert(Phase::Hold, 300.0);
        deadlines.insert(Phase::Crossing, 45.0);
        deadlines.insert(Phase::LineUp, 300.0);
        deadlines.insert(Phase::Takeoff, 300.0);
        deadlines.insert(Phase::Airborne, 600.0);
        deadlines.insert(Phase::Approach, 600.0);
        deadlines.insert(Phase::Landing, 300.0);
        deadlines.insert(Phase::TaxiIn, 300.0);
        Self { deadlines }
    }
}

impl TrafficHealth {
    /// Escalate in stages: re-trigger, release movement authority, then retire. The
    /// player is never retired.
    pub(crate) fn check(&self, registry: &mut SessionRegistry, at: f64) -> Vec<HealthAction> {
        let mut actions = Vec::new();
        for (&id, session) in registry.sessions.iter_mut() {
            if id == 0 || !at.is_finite() || at < session.last_progress {
                continue;
            }
            let Some(deadline) = self.deadlines.get(&session.phase).copied() else {
                continue;
            };
            let elapsed = at - session.last_progress;
            if elapsed < deadline {
                continue;
            }
            session.recoveries = session.recoveries.saturating_add(1);
            match session.recoveries {
                1 => actions.push(HealthAction::Retrigger(id)),
                2 => actions.push(HealthAction::Release(id)),
                _ => actions.push(HealthAction::Retire(id)),
            }
            session.last_progress = at;
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn serial() -> MutexGuard<'static, ()> {
        TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn receive(events: &Receiver<StationEvent>, timeout: Duration) -> StationEvent {
        // Worker start-up can be delayed by other test binaries on a busy machine. The
        // event still arrives immediately in the normal case; the longer bound only
        // prevents a loaded test host from reporting a product failure.
        events
            .recv_timeout(timeout.max(Duration::from_secs(120)))
            .expect("station event")
    }

    #[test]
    fn independent_frequencies_run_in_separate_threads_and_drain_independently() {
        let _serial = serial();
        let (events, received) = mpsc::channel();
        let mut supervisor = StationSupervisor::default();
        supervisor.watch(121_705, 1, Phase::Taxi, events.clone());
        supervisor.watch(118_700, 2, Phase::Hold, events.clone());
        assert_eq!(supervisor.live(), 2);
        let mut started = BTreeSet::new();
        while started.len() < 2 {
            if let StationEvent::Started { frequency } = receive(&received, Duration::from_secs(1))
            {
                started.insert(frequency);
            }
        }
        assert_eq!(started, BTreeSet::from([118_700, 121_705]));

        let at = Instant::now();
        supervisor.trigger(Trigger {
            session: 1,
            phase: Phase::Taxi,
            frequency: 121_705,
            at: 10.0,
        });
        supervisor.trigger(Trigger {
            session: 2,
            phase: Phase::Hold,
            frequency: 118_700,
            at: 10.0,
        });
        let mut seen = BTreeSet::new();
        while seen.len() < 2 {
            if let StationEvent::Trigger { frequency, .. } =
                receive(&received, Duration::from_secs(1))
            {
                seen.insert(frequency);
            }
        }
        assert_eq!(seen, BTreeSet::from([118_700, 121_705]));
        assert!(at.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn release_removes_the_watch_and_shutdown_joins_the_threads() {
        let _serial = serial();
        let (events, received) = mpsc::channel();
        let mut supervisor = StationSupervisor::default();
        supervisor.watch(121_705, 7, Phase::Taxi, events);
        assert_eq!(supervisor.live(), 1);
        supervisor.release(121_705, 7);
        supervisor.shutdown();
        assert_eq!(supervisor.live(), 0);
        assert!(received.try_iter().any(
            |event| matches!(event, StationEvent::Closed { frequency } if frequency == 121_705)
        ));
    }

    #[test]
    fn a_stalled_crossing_escalates_and_progress_resets_it() {
        let _serial = serial();
        let mut registry = SessionRegistry::default();
        registry.add(4, 121_705, Phase::Crossing, 10.0);
        let health = TrafficHealth::default();
        assert!(health.check(&mut registry, 54.0).is_empty());
        assert_eq!(
            health.check(&mut registry, 56.0),
            vec![HealthAction::Retrigger(4)]
        );
        registry.progress(4, 57.0);
        assert!(health.check(&mut registry, 100.0).is_empty());
        assert_eq!(
            health.check(&mut registry, 102.0),
            vec![HealthAction::Retrigger(4)]
        );
        assert_eq!(
            health.check(&mut registry, 147.0),
            vec![HealthAction::Release(4)]
        );
        assert_eq!(
            health.check(&mut registry, 192.0),
            vec![HealthAction::Retire(4)]
        );
    }

    #[test]
    fn the_player_is_never_escalated() {
        let _serial = serial();
        let mut registry = SessionRegistry::default();
        registry.add(0, 118_700, Phase::Taxi, 0.0);
        let health = TrafficHealth::default();
        assert!(health.check(&mut registry, 10_000.0).is_empty());
        assert!(registry.retire(0).is_some());
    }

    #[test]
    fn watch_sets_are_partitioned_by_frequency() {
        let _serial = serial();
        let mut registry = SessionRegistry::default();
        registry.add(1, 121_705, Phase::Taxi, 0.0);
        registry.add(2, 118_700, Phase::Hold, 0.0);
        registry.add(3, 121_705, Phase::Pushback, 0.0);
        assert_eq!(registry.watch_set(121_705), BTreeSet::from([1, 3]));
        assert_eq!(registry.watch_set(118_700), BTreeSet::from([2]));
    }

    fn wait_for_idle(supervisor: &mut StationSupervisor) {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            supervisor.reap();
            if supervisor.live() == 0 {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "station worker did not exit on an empty queue"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn wait_for_closed(events: &Receiver<StationEvent>, frequency: i32) {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match receive(events, remaining.max(Duration::from_millis(1))) {
                StationEvent::Closed { frequency: closed } if closed == frequency => return,
                _ => {}
            }
        }
    }

    #[test]
    fn an_empty_station_exits_its_thread_and_keeps_the_session() {
        let _serial = serial();
        let (events, _received) = mpsc::channel();
        let mut supervisor = StationSupervisor::default();
        supervisor.watch(121_705, 7, Phase::Taxi, events);
        wait_for_idle(&mut supervisor);
        assert_eq!(supervisor.live(), 0);
        assert_eq!(supervisor.stations(), 1, "the durable watch set remains");
    }

    #[test]
    fn a_new_transmission_starts_an_idle_station_again() {
        let _serial = serial();
        let (events, received) = mpsc::channel();
        let mut supervisor = StationSupervisor::default();
        supervisor.watch(121_705, 7, Phase::Taxi, events);
        wait_for_idle(&mut supervisor);
        supervisor.transmit(
            121_705,
            Transmission {
                session: 7,
                speaker: "ATC".into(),
                text: "taxi via A".into(),
                controller: true,
                tag: SpeechTag::default(),
                due: Instant::now(),
            },
        );
        let mut started = false;
        let mut heard = false;
        while !(started && heard) {
            match receive(&received, Duration::from_secs(1)) {
                StationEvent::Started { frequency } if frequency == 121_705 => started = true,
                StationEvent::Transmitted { transmission, .. } => {
                    assert_eq!(transmission.text, "taxi via A");
                    heard = true;
                }
                _ => {}
            }
        }
        wait_for_idle(&mut supervisor);
        assert_eq!(supervisor.live(), 0);
    }

    #[test]
    fn a_trigger_restarts_the_station_with_its_watch_set() {
        let _serial = serial();
        let (events, received) = mpsc::channel();
        let mut supervisor = StationSupervisor::default();
        supervisor.watch(118_700, 9, Phase::Hold, events);
        wait_for_idle(&mut supervisor);
        supervisor.trigger(Trigger {
            session: 9,
            phase: Phase::Hold,
            frequency: 118_700,
            at: 12.0,
        });
        loop {
            if let StationEvent::Trigger { trigger, .. } =
                receive(&received, Duration::from_secs(60))
            {
                assert_eq!(trigger.session, 9);
                break;
            }
        }
        wait_for_idle(&mut supervisor);
        // A released session must not wake the station.
        supervisor.release(118_700, 9);
        supervisor.trigger(Trigger {
            session: 9,
            phase: Phase::Hold,
            frequency: 118_700,
            at: 13.0,
        });
        assert_eq!(supervisor.live(), 0);
    }

    #[test]
    fn same_frequency_order_survives_a_worker_restart() {
        let _serial = serial();
        let (events, received) = mpsc::channel();
        let mut supervisor = StationSupervisor::default();
        supervisor.watch(118_700, 1, Phase::Takeoff, events);
        wait_for_idle(&mut supervisor);
        for text in ["cleared for takeoff", "line up and wait"] {
            supervisor.transmit(
                118_700,
                Transmission {
                    session: 1,
                    speaker: "ATC".into(),
                    text: text.into(),
                    controller: true,
                    tag: SpeechTag::default(),
                    due: Instant::now(),
                },
            );
        }
        let mut heard = Vec::new();
        while heard.len() < 2 {
            if let StationEvent::Transmitted { transmission, .. } =
                receive(&received, Duration::from_secs(1))
            {
                heard.push(transmission.text);
            }
        }
        assert_eq!(heard, ["cleared for takeoff", "line up and wait"]);
        wait_for_idle(&mut supervisor);
        assert_eq!(supervisor.live(), 0);
    }

    #[test]
    fn session_sync_starts_a_worker_and_only_triggers_real_changes() {
        let _serial = serial();
        let mut bus = StationBus::default();
        bus.sync_sessions(vec![(7, 118_700, Phase::Takeoff)], 10.0);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut trigger = None;
        while trigger.is_none() {
            bus.drain();
            trigger = bus.take_triggers().into_iter().next();
            assert!(Instant::now() < deadline, "phase trigger did not arrive");
            thread::sleep(Duration::from_millis(2));
        }
        let trigger = trigger.unwrap();
        assert_eq!(trigger.session, 7);
        assert_eq!(trigger.phase, Phase::Takeoff);
        assert_eq!(trigger.frequency, 118_700);

        // Re-reporting the same phase must not start another exchange.
        bus.sync_sessions(vec![(7, 118_700, Phase::Takeoff)], 11.0);
        thread::sleep(Duration::from_millis(20));
        bus.drain();
        assert!(bus.take_triggers().is_empty());

        // A real phase transition does.
        bus.sync_sessions(vec![(7, 118_700, Phase::Airborne)], 12.0);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut changed = None;
        while changed.is_none() {
            bus.drain();
            changed = bus.take_triggers().into_iter().next();
            assert!(
                Instant::now() < deadline,
                "phase change trigger did not arrive"
            );
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(changed.unwrap().phase, Phase::Airborne);
    }

    #[test]
    fn one_frequency_serializes_its_transmissions() {
        let _serial = serial();
        let (events, received) = mpsc::channel();
        let mut supervisor = StationSupervisor::default();
        supervisor.watch(118_700, 1, Phase::Takeoff, events);
        supervisor.transmit(
            118_700,
            Transmission {
                session: 1,
                speaker: "ATC".into(),
                text: "cleared for takeoff".into(),
                controller: true,
                tag: SpeechTag::default(),
                due: Instant::now(),
            },
        );
        supervisor.transmit(
            118_700,
            Transmission {
                session: 2,
                speaker: "ATC".into(),
                text: "line up and wait".into(),
                controller: true,
                tag: SpeechTag::default(),
                due: Instant::now(),
            },
        );
        let mut heard = Vec::new();
        while heard.len() < 2 {
            if let StationEvent::Transmitted { transmission, .. } =
                receive(&received, Duration::from_secs(1))
            {
                heard.push(transmission.text);
            }
        }
        assert_eq!(heard, ["cleared for takeoff", "line up and wait"]);
        supervisor.shutdown();
    }

    #[test]
    fn different_frequencies_transmit_independently() {
        let _serial = serial();
        let (events, received) = mpsc::channel();
        let mut supervisor = StationSupervisor::default();
        supervisor.watch(118_700, 1, Phase::Takeoff, events.clone());
        supervisor.watch(121_705, 2, Phase::Taxi, events);
        supervisor.transmit(
            118_700,
            Transmission {
                session: 1,
                speaker: "ATC".into(),
                text: "tower".into(),
                controller: true,
                tag: SpeechTag::default(),
                due: Instant::now(),
            },
        );
        supervisor.transmit(
            121_705,
            Transmission {
                session: 2,
                speaker: "ATC".into(),
                text: "ground".into(),
                controller: true,
                tag: SpeechTag::default(),
                due: Instant::now(),
            },
        );
        let mut heard = BTreeSet::new();
        while heard.len() < 2 {
            if let StationEvent::Transmitted { frequency, .. } =
                receive(&received, Duration::from_secs(1))
            {
                heard.insert(frequency);
            }
        }
        assert_eq!(heard, BTreeSet::from([118_700, 121_705]));
        supervisor.shutdown();
    }
}

impl openatc_core::aircraft::AircraftBehaviour for SessionState {
    fn state(&self) -> &openatc_core::aircraft::AircraftState {
        &self.aircraft
    }

    fn state_mut(&mut self) -> &mut openatc_core::aircraft::AircraftState {
        &mut self.aircraft
    }

    fn on_event(
        &mut self,
        event: openatc_core::aircraft::AircraftEvent,
    ) -> openatc_core::aircraft::AircraftOutcome {
        use openatc_core::aircraft::{AircraftEvent, AircraftOutcome, AircraftPhase};
        match event {
            AircraftEvent::IfrClearanceReceived => {
                self.phase = Phase::IfrClearance;
                self.aircraft.phase = AircraftPhase::IfrClearance;
                AircraftOutcome::Advanced(AircraftPhase::IfrClearance)
            }
            AircraftEvent::StartupComplete => {
                self.phase = Phase::AtGate;
                self.aircraft.phase = AircraftPhase::Startup;
                AircraftOutcome::Advanced(AircraftPhase::Startup)
            }
            AircraftEvent::PushbackComplete => {
                self.phase = Phase::Pushback;
                self.aircraft.phase = AircraftPhase::Pushback;
                AircraftOutcome::Advanced(AircraftPhase::Pushback)
            }
            AircraftEvent::Taxi => {
                self.phase = Phase::Taxi;
                self.aircraft.phase = AircraftPhase::Taxi;
                AircraftOutcome::Advanced(AircraftPhase::Taxi)
            }
            AircraftEvent::TaxiIn => {
                self.phase = Phase::TaxiIn;
                self.aircraft.phase = AircraftPhase::TaxiIn;
                AircraftOutcome::Advanced(AircraftPhase::TaxiIn)
            }
            AircraftEvent::Approach => {
                self.phase = Phase::Approach;
                self.aircraft.phase = AircraftPhase::Approach;
                AircraftOutcome::Advanced(AircraftPhase::Approach)
            }
            AircraftEvent::Finished => {
                self.aircraft.phase = AircraftPhase::Finished;
                AircraftOutcome::Advanced(AircraftPhase::Finished)
            }
            AircraftEvent::Arrival => {
                self.aircraft.phase = AircraftPhase::Arrival;
                AircraftOutcome::Advanced(AircraftPhase::Arrival)
            }
            AircraftEvent::Departure => {
                self.aircraft.phase = AircraftPhase::Departure;
                AircraftOutcome::Advanced(AircraftPhase::Departure)
            }
            AircraftEvent::Cruise => {
                self.aircraft.phase = AircraftPhase::Cruise;
                AircraftOutcome::Advanced(AircraftPhase::Cruise)
            }
            AircraftEvent::Landed => {
                self.phase = Phase::Landing;
                self.aircraft.phase = AircraftPhase::Landed;
                AircraftOutcome::Advanced(AircraftPhase::Landed)
            }
            AircraftEvent::Parked => {
                self.phase = Phase::AtGate;
                self.aircraft.phase = AircraftPhase::Parked;
                AircraftOutcome::Advanced(AircraftPhase::Parked)
            }
            _ => AircraftOutcome::None,
        }
    }

    fn advance(&mut self, seconds: f64) -> openatc_core::aircraft::AircraftOutcome {
        self.last_progress = seconds;
        self.aircraft.last_progress_seconds = seconds;
        openatc_core::aircraft::AircraftOutcome::None
    }
}
