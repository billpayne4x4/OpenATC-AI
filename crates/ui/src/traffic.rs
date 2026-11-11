//! Traffic actually reported by the simulator, including runway-blocking diagnostics.

// Layout limits bound the numeric casts below.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use crate::widgets;
use openatc_core::state::State;

fn frequency_text(khz: Option<i32>) -> String {
    khz.filter(|frequency| *frequency > 0).map_or_else(
        || "—".to_owned(),
        |frequency| format!("{:.3}", f64::from(frequency) / 1000.0),
    )
}

/// Draw generated poses alongside fresh simulator telemetry, preferring the engine pose by ID.
pub(crate) fn targets(state: &State) -> impl Iterator<Item = &openatc_core::state::TrafficTarget> {
    let ids: std::collections::BTreeSet<u32> = state
        .generated_traffic
        .iter()
        .map(|frame| frame.target.id)
        .collect();
    let callsigns: std::collections::BTreeSet<String> = state
        .generated_traffic
        .iter()
        .map(|frame| openatc_core::ops::callsign_key(&frame.target.callsign))
        .filter(|callsign| !callsign.is_empty())
        .collect();
    state
        .telemetry
        .traffic
        .iter()
        .flatten()
        .filter(move |target| {
            !ids.contains(&target.id) && {
                let callsign = openatc_core::ops::callsign_key(&target.callsign);
                callsign.is_empty() || !callsigns.contains(&callsign)
            }
        })
        .chain(state.generated_traffic.iter().map(|frame| &frame.target))
}

/// How often the generated traffic is drawn, in frames or in hertz.
fn traffic_rate(ui: &imgui::Ui, interface: &mut crate::interface::Interface) {
    use openatc_settings::TrafficRate;
    let preview = match interface.settings.traffic_rate {
        TrafficRate::EveryFrame => "Every frame".to_owned(),
        TrafficRate::EveryFrames(frames) => format!("Every {frames} frames"),
        TrafficRate::Hertz(hertz) => format!("{hertz} frames per second"),
    };
    ui.text_disabled("Traffic update rate");
    ui.set_next_item_width(-1.0);
    if let Some(_combo) = ui.begin_combo("##traffic-rate", preview) {
        for option in ["Every frame", "Every N frames", "Frames per second"] {
            if ui.selectable(option) {
                interface.settings.traffic_rate = match option {
                    "Every frame" => TrafficRate::EveryFrame,
                    "Every N frames" => TrafficRate::EveryFrames(2),
                    _ => TrafficRate::Hertz(20),
                };
            }
        }
    }
    match interface.settings.traffic_rate {
        TrafficRate::EveryFrame => {
            ui.text_wrapped("The traffic is drawn on every simulator frame. This is the smoothest and the most expensive; the cost grows with the frame rate and the number of aircraft.");
        }
        TrafficRate::EveryFrames(frames) => {
            let mut frames = i32::from(frames);
            ui.text_disabled("Frames between draws");
            ui.set_next_item_width(-1.0);
            if ui.slider("##traffic-frames", 1, 10, &mut frames) {
                interface.settings.traffic_rate =
                    TrafficRate::EveryFrames(frames.clamp(1, 10) as u8);
            }
            ui.text_wrapped("The traffic is drawn on every n-th simulator frame.");
        }
        TrafficRate::Hertz(hertz) => {
            let mut hertz = i32::from(hertz);
            ui.text_disabled("Frames per second");
            ui.set_next_item_width(-1.0);
            if ui.slider("##traffic-hertz", 10, 60, &mut hertz) {
                interface.settings.traffic_rate = TrafficRate::Hertz(hertz.clamp(10, 60) as u8);
            }
            ui.text_wrapped("The traffic is drawn at a fixed rate. The engine publishes the traffic twice a second, so a higher rate only interpolates between those snapshots.");
        }
    }
}

fn wake_standard(ui: &imgui::Ui, interface: &mut crate::interface::Interface) {
    let options = [
        ("Auto by region", "auto"),
        ("ICAO Doc 4444", "icao"),
        ("EUROCONTROL RECAT-EU", "recat_eu"),
        ("FAA CWT", "faa_cwt"),
        ("Australian MATS", "mats"),
    ];
    let preview = options
        .iter()
        .find(|(_, value)| *value == interface.settings.wake_standard)
        .map_or("Auto by region", |(label, _)| *label);
    ui.text_disabled("Wake separation");
    ui.set_next_item_width(-1.0);
    if let Some(_combo) = ui.begin_combo("##wake-standard", preview) {
        for (label, value) in options {
            if ui.selectable(label) && interface.settings.wake_standard != value {
                interface.settings.wake_standard = value.into();
                interface.save_settings();
            }
        }
    }
    ui.text_wrapped("Auto uses FAA CWT at US airports, Australian MATS at Y airports, and ICAO elsewhere. The tables live in traffic/wake.toml and can be edited without rebuilding.");
}

pub fn settings(ui: &imgui::Ui, interface: &mut crate::interface::Interface, state: &State) {
    scenery_setup(ui, interface, Some(state));
    widgets::section_title(ui, "TRAFFIC MOTION");
    traffic_rate(ui, interface);
    wake_standard(ui, interface);
    widgets::section_title(ui, "TRAFFIC MIX");
    let mut mix_changed = false;
    let mut airline = i32::from(interface.settings.airline_traffic);
    ui.set_next_item_width(-1.0);
    if ui.slider("##airline-traffic", 0, 10, &mut airline) {
        interface.settings.airline_traffic = airline.clamp(0, 10) as u8;
        mix_changed = true;
    }
    ui.text_wrapped("Airliner share");
    let mut regional = i32::from(interface.settings.regional_traffic);
    ui.set_next_item_width(-1.0);
    if ui.slider("##regional-traffic", 0, 10, &mut regional) {
        interface.settings.regional_traffic = regional.clamp(0, 10) as u8;
        mix_changed = true;
    }
    ui.text_wrapped(
        "Regional airline share, including turboprops and regional jets. Set to 0 to remove them.",
    );
    let mut cargo = i32::from(interface.settings.cargo_traffic);
    ui.set_next_item_width(-1.0);
    if ui.slider("##cargo-traffic", 0, 10, &mut cargo) {
        interface.settings.cargo_traffic = cargo.clamp(0, 10) as u8;
        mix_changed = true;
    }
    ui.text_wrapped("Freight share");
    let mut ga = i32::from(interface.settings.ga_traffic);
    ui.set_next_item_width(-1.0);
    if ui.slider("##ga-traffic", 0, 10, &mut ga) {
        interface.settings.ga_traffic = ga.clamp(0, 10) as u8;
        mix_changed = true;
    }
    ui.text_wrapped("General aviation share, including light aircraft, turboprops and business jets. Set to 0 to remove GA.");
    if mix_changed {
        interface.save_settings();
    }
    ui.checkbox(
        "Add local pattern traffic",
        &mut interface.settings.local_pattern_traffic,
    );
    ui.text_wrapped("Adds a local circuit arrival and two departures at your airport when the network released fewer than two flights. The pattern flies on the tower frequency with a clearance and a readback.");
    ui.checkbox(
        "Start the first flight on approach",
        &mut interface.settings.start_first_flight_on_approach,
    );
    ui.text_wrapped("Allows an approach start even when only one protected flight is available. Larger populations automatically mix nearby arrivals, flying traffic and ground departures.");
    if ui.collapsing_header("Traffic lifecycle", imgui::TreeNodeFlags::empty()) {
        let mut changed = false;
        let mut taxi_min = i32::from(interface.settings.traffic_taxi_min);
        let mut taxi_max = i32::from(interface.settings.traffic_taxi_max);
        ui.text_disabled("Moving ground aircraft when traffic is full");
        ui.set_next_item_width(-1.0);
        if ui.slider("##taxi-min", 0, 16, &mut taxi_min) {
            interface.settings.traffic_taxi_min = taxi_min.clamp(0, 16) as u8;
            changed = true;
        }
        ui.text_wrapped("Minimum taxiing aircraft");
        ui.set_next_item_width(-1.0);
        if ui.slider("##taxi-max", 0, 16, &mut taxi_max) {
            interface.settings.traffic_taxi_max = taxi_max.clamp(0, 16) as u8;
            changed = true;
        }
        ui.text_wrapped("Maximum taxiing aircraft. The actual target is rolled once per session inside this range.");
        if interface.settings.traffic_taxi_min > interface.settings.traffic_taxi_max {
            interface.settings.traffic_taxi_max = interface.settings.traffic_taxi_min;
        }
        let mut jet = interface.settings.traffic_circuit_jet_nm as f32;
        ui.set_next_item_width(-1.0);
        if ui.slider("##circuit-jet", 1.0, 100.0, &mut jet) {
            interface.settings.traffic_circuit_jet_nm = f64::from(jet);
            changed = true;
        }
        ui.text_wrapped("Airliner circuit retention radius (NM)");
        let mut turboprop = interface.settings.traffic_circuit_turboprop_nm as f32;
        ui.set_next_item_width(-1.0);
        if ui.slider("##circuit-turboprop", 1.0, 100.0, &mut turboprop) {
            interface.settings.traffic_circuit_turboprop_nm = f64::from(turboprop);
            changed = true;
        }
        ui.text_wrapped("Turboprop circuit retention radius (NM)");
        let mut ga = interface.settings.traffic_circuit_ga_nm as f32;
        ui.set_next_item_width(-1.0);
        if ui.slider("##circuit-ga", 1.0, 100.0, &mut ga) {
            interface.settings.traffic_circuit_ga_nm = f64::from(ga);
            changed = true;
        }
        ui.text_wrapped("Light-aircraft circuit retention radius (NM). Ground, approach and departing traffic stays alive inside this radius; beyond it those slots become airborne traffic near you.");
        if changed {
            interface.save_settings();
        }
    }
    widgets::section_title(ui, "TRAFFIC SOURCE");
    ui.text_wrapped("Airport traffic runs without a flight plan: taxiing aircraft, departures and arrivals around the loaded airport. Generated traffic adds complete planned flights around your route and destination STAR. Native X-Plane AI is optional; installing Bluebell models alone does not create flights.");
    if let Some(reply) = interface.engine.take_reply("/traffic/models/index") {
        interface.traffic_notice = if reply.success {
            format!(
                "{} compatible models · {} aircraft types · {} operators · {} background CPU workers. {} catalogue warnings.",
                reply.data["models"],
                reply.data["types"].as_array().map_or(0, Vec::len),
                reply.data["operators"].as_array().map_or(0, Vec::len),
                reply.data["cpuWorkers"],
                reply.data["warnings"].as_array().map_or(0, Vec::len)
            )
        } else {
            reply.error
        };
    }
    if ui.collapsing_header("Advanced CPU budget", imgui::TreeNodeFlags::empty()) {
        let mut workers = i32::from(interface.settings.cpu_workers);
        widgets::muted_wrapped(ui, "Background CPU workers (0 = automatic)");
        ui.set_next_item_width(-1.0);
        if ui.slider("##cpu-workers", 0, 16, &mut workers) {
            interface.settings.cpu_workers = workers as u8;
        }
        ui.text_wrapped("Automatic uses up to half the available CPU threads, capped at four. One worker limits background CPU use. Changes apply when the engine restarts; simulator SDK calls stay on the simulator thread.");
    }
    {
        let _disabled = ui.begin_disabled(!interface.simulator_host);
        ui.checkbox(
            "Suppress X-Plane AI traffic",
            &mut interface.settings.suppress_native_traffic,
        );
        ui.text_wrapped("Keeps native AI out of OpenATC's traffic flow. Other traffic plugins retain ownership. X-Plane's Draw Parked Aircraft option is separate.");
        ui.checkbox(
            "Mute X-Plane radio audio while connected",
            &mut interface.settings.mute_native_radio,
        );
        ui.text_wrapped("Turn this off when another plugin, such as a pushback helper, speaks through the radio channel. OpenATC speech has its own output and is unaffected.");
        let _native_disabled = ui.begin_disabled(interface.settings.suppress_native_traffic);
        ui.checkbox(
            "Set X-Plane AI traffic amount",
            &mut interface.settings.manage_native_traffic,
        );
        let _count_disabled = ui.begin_disabled(!interface.settings.manage_native_traffic);
        let mut count = i32::from(interface.settings.native_traffic_count);
        widgets::muted_wrapped(ui, "AI aircraft");
        ui.set_next_item_width(-1.0);
        if ui.slider("##ai-aircraft", 0, 19, &mut count) {
            interface.settings.native_traffic_count = count as u8;
        }
    }
    ui.text_wrapped("Native AI uses X-Plane flight models. Start with a small count and measure FPS. Your existing aircraft models and simulator routes are used; another traffic provider always keeps priority.");
    if !interface.native_traffic_status.is_empty() {
        ui.text_wrapped(&interface.native_traffic_status);
    }
    widgets::section_title(ui, "TRAFFIC RADIO");
    if ui.checkbox(
        "Airport radio chatter",
        &mut interface.settings.airport_chatter,
    ) {
        interface.save_settings();
    }
    ui.text_wrapped("When airborne above a receivable Ground or Tower station, you may hear other aircraft taxi and depart. The calls use the installed airport's runways and taxiways. Chatter stops when you retune, leave reception or descend below 1,000 ft above the station.");
    ui.checkbox(
        "Hear simulated traffic pilot reports",
        &mut interface.settings.hear_traffic_radio,
    );
    ui.checkbox(
        "Talk to simulated traffic pilots",
        &mut interface.settings.talk_to_traffic,
    );
    ui.text_wrapped("Pilot conversations use observed simulator traffic and weather. They are simulated replies, not communications with multiplayer pilots. Runway and taxi protection remain active with traffic voices off.");
    if ui.collapsing_header(
        "Bluebell model installation",
        imgui::TreeNodeFlags::DEFAULT_OPEN,
    ) {
        model_installer(ui, interface);
        if ui.button("Scan installed model catalogue") {
            interface
                .engine
                .post("/traffic/models/index", serde_json::json!({"refresh":true}));
        }
    }
    if ui.collapsing_header(
        "Airline, hub and aircraft preferences",
        imgui::TreeNodeFlags::empty(),
    ) {
        profiles(ui, interface);
    }
    if !interface.traffic_notice.is_empty() {
        ui.text_wrapped(&interface.traffic_notice);
    }
}

pub fn tab(ui: &imgui::Ui, interface: &mut crate::interface::Interface, state: &State) {
    if ui.button("Traffic settings & model downloads") {
        interface.show_settings_section(8);
    }
    if let Some(reply) = interface.engine.take_reply("/traffic/talk") {
        interface.traffic_notice = if reply.success {
            String::new()
        } else {
            reply.error
        };
    }
    if !interface.traffic_notice.is_empty() {
        ui.text_wrapped(&interface.traffic_notice);
    }
    ui.text_wrapped("OpenATC checks simulator-reported aircraft before issuing runway crossing, takeoff and landing clearances.");
    ui.checkbox(
        "En-route generated traffic",
        &mut interface.settings.generated_traffic,
    );
    if interface.settings.generated_traffic {
        ui.text_wrapped("ON. Complete flights are prepared around your plan and destination STAR.");
    } else {
        ui.text_wrapped("OFF. Ambient traffic only stays around airports; switch it on for traffic around your route and STAR.");
    }
    ui.checkbox(
        "Enable generated traffic automatically for a loaded plan",
        &mut interface.settings.auto_generated_traffic,
    );
    if state.telemetry.traffic.is_none() {
        ui.text_colored(widgets::warning(ui), "Traffic feed unavailable");
        ui.text_wrapped("Runway checks cannot confirm clear traffic. Check the simulator connection and the traffic provider's TCAS output.");
        if state.generated_traffic.is_empty() {
            return;
        }
    }
    let targets: Vec<_> = targets(state).take(126).collect();
    ui.text(format!("{} aircraft reported", targets.len()));
    if state.taxi_clearance.waiting_for_traffic {
        ui.text_colored(
            widgets::warning(ui),
            format!(
                "Waiting to enter runway {}",
                state.taxi_clearance.hold_short_runway
            ),
        );
    }
    ui.spacing();
    ui.text_wrapped("Lightweight OpenATC-controlled CSL traffic is still in development. Native AI does not use Bluebell models or accept OpenATC flight plans.");
    ui.text_wrapped("Bluebell contains visual CSL aircraft models; it does not generate or control flights by itself.");
    ui.spacing();
    widgets::section_title(ui, "NEARBY AIRCRAFT");
    if let Some(_table) = ui.begin_table_with_flags(
        "TrafficTargets",
        8,
        imgui::TableFlags::ROW_BG
            | imgui::TableFlags::BORDERS_INNER_H
            | imgui::TableFlags::SORTABLE
            | imgui::TableFlags::SIZING_STRETCH_PROP,
    ) {
        for column in [
            "Aircraft",
            "Distance (NM)",
            "Altitude (ft MSL)",
            "Track",
            "Position",
            "GS (kt)",
            "COM1",
            "COM2",
        ] {
            ui.table_setup_column(column);
        }
        ui.table_headers_row();
        let mut rows: Vec<_> = targets
            .iter()
            .map(|t| {
                (
                    openatc_core::ops::distance_nm(
                        state.telemetry.latitude,
                        state.telemetry.longitude,
                        t.latitude,
                        t.longitude,
                    ),
                    t,
                )
            })
            .collect();
        let (column, descending) = ui
            .table_sort_specs_mut()
            .and_then(|s| {
                s.specs().iter().next().map(|sort| {
                    (
                        sort.column_idx(),
                        sort.sort_direction() == Some(imgui::TableSortDirection::Descending),
                    )
                })
            })
            .unwrap_or((0, false));
        rows.sort_by(|a, b| {
            let order = match column {
                0 => a.1.callsign.cmp(&b.1.callsign),
                2 => a.1.altitude_feet.total_cmp(&b.1.altitude_feet),
                3 => a.1.track_degrees.total_cmp(&b.1.track_degrees),
                4 => a.1.on_ground.cmp(&b.1.on_ground),
                _ => a.0.total_cmp(&b.0),
            };
            if descending { order.reverse() } else { order }
        });
        for (distance, target) in rows {
            ui.table_next_row();
            ui.table_next_column();
            let _id = ui.push_id_int(target.id as i32);
            if target.callsign.is_empty() {
                ui.text_disabled("Unidentified");
            } else if ui
                .selectable_config(&target.callsign)
                .selected(interface.traffic_selected == target.id)
                .build()
            {
                interface.traffic_selected = target.id;
            }
            ui.table_next_column();
            ui.text(format!("{distance:.1}"));
            ui.table_next_column();
            ui.text(format!("{:.0}", target.altitude_feet));
            ui.table_next_column();
            ui.text(format!("{:03.0}°", target.track_degrees));
            ui.table_next_column();
            ui.text(if target.on_ground {
                "On ground"
            } else {
                "Airborne"
            });
            ui.table_next_column();
            ui.text(
                target
                    .ground_speed_knots
                    .map_or_else(|| "—".to_owned(), |speed| format!("{speed:.0}")),
            );
            ui.table_next_column();
            ui.text(frequency_text(target.com1_khz));
            ui.table_next_column();
            ui.text(frequency_text(target.com2_khz));
        }
    }
    if interface.settings.dev_mode
        && interface.settings.show_traffic_debug
        && !state.generated_traffic_debug.is_empty()
    {
        ui.separator();
        widgets::section_title(ui, "TRAFFIC DEBUG");
        if let Some(_table) = ui.begin_table_with_flags(
            "GeneratedTrafficDebug",
            4,
            imgui::TableFlags::ROW_BG | imgui::TableFlags::BORDERS_INNER_H,
        ) {
            for column in ["Aircraft", "Stage", "Reason", "Blocker"] {
                ui.table_setup_column(column);
            }
            ui.table_headers_row();
            for debug in &state.generated_traffic_debug {
                ui.table_next_row();
                ui.table_next_column();
                ui.text(&debug.callsign);
                ui.table_next_column();
                ui.text(&debug.stage);
                ui.table_next_column();
                ui.text(&debug.reason);
                ui.table_next_column();
                if let Some(blocker) = debug.blocker {
                    ui.text(blocker.to_string());
                } else {
                    ui.text_disabled("-");
                }
            }
        }
    }
    if let Some(target) = targets
        .iter()
        .find(|t| t.id != 0 && t.id == interface.traffic_selected)
    {
        ui.separator();
        ui.text(format!("Talk to {} · {}", target.callsign, target.aircraft));
        let _disabled =
            ui.begin_disabled(!interface.settings.talk_to_traffic || state.telemetry.radio_busy);
        for (label, topic) in [
            ("Ask about weather", "weather"),
            ("Ask about landing", "landing"),
            ("Ask about crosswind", "crosswind"),
        ] {
            if ui.button(label) {
                interface.engine.post(
                    "/traffic/talk",
                    serde_json::json!({"id":target.id,"topic":topic}),
                );
            }
        }
        widgets::muted_wrapped(ui, "Message this pilot");
        ui.set_next_item_width(-1.0);
        widgets::edit_text(
            ui,
            "##traffic-message",
            &mut interface.traffic_message,
            1000,
        );
        let _chat_disabled = ui.begin_disabled(
            !interface.settings.informal_pilot_chat || interface.traffic_message.trim().is_empty(),
        );
        if ui.button("Send to pilot") {
            interface.engine.post(
                "/traffic/talk",
                serde_json::json!({"id":target.id,"message":interface.traffic_message}),
            );
            interface.traffic_message.clear();
        }
        ui.text_wrapped("Enable informal conversation under Realism for free text. This talks to a simulated pilot, not a multiplayer user.");
    }
}

fn profiles(ui: &imgui::Ui, interface: &mut crate::interface::Interface) {
    if let Some(reply) = interface.engine.take_reply("/traffic/profiles") {
        interface.traffic_profiles = if reply.success {
            reply.data
        } else {
            serde_json::json!({"errors":[reply.error]})
        };
    }
    if ui.button("Load / validate profile files") {
        interface
            .engine
            .post("/traffic/profiles", serde_json::json!({"refresh":true}));
        interface.traffic_profiles = serde_json::json!({"loading":true});
    }
    if interface.traffic_profiles["loading"] == true {
        ui.text("Reading traffic preferences...");
    }
    ui.text_wrapped("Edit traffic/airlines.toml, hubs.toml and aircraft.toml to change the simulated mix. Preferences are approximate, not live schedules. Invalid edits are rejected and the last valid set is kept.");
    let p = &interface.traffic_profiles;
    if let Some(errors) = p["errors"].as_array() {
        for error in errors {
            ui.text_colored(
                widgets::warning(ui),
                error.as_str().unwrap_or("Profile error"),
            );
        }
    }
    if let Some(airlines) = p["airlines"].as_object() {
        ui.text(format!(
            "{} fleets · {} airport preferences · {} aircraft types",
            airlines.len(),
            p["hubs"].as_object().map_or(0, serde_json::Map::len),
            p["aircraft"].as_object().map_or(0, serde_json::Map::len)
        ));
        if let Some(_table) = ui.begin_table_with_flags(
            "AirlineProfiles",
            3,
            imgui::TableFlags::ROW_BG | imgui::TableFlags::SIZING_STRETCH_PROP,
        ) {
            for heading in ["Operator", "Activity", "Aircraft types"] {
                ui.table_setup_column(heading);
            }
            ui.table_headers_row();
            for airline in airlines.values() {
                ui.table_next_row();
                ui.table_next_column();
                ui.text_wrapped(airline["name"].as_str().unwrap_or(""));
                ui.table_next_column();
                ui.text(airline["category"].as_str().unwrap_or("").replace('_', " "));
                ui.table_next_column();
                ui.text_wrapped(
                    airline["fleet"]
                        .as_object()
                        .map(|v| v.keys().cloned().collect::<Vec<_>>().join(", "))
                        .unwrap_or_default(),
                );
            }
        }
    }
}

fn model_installer(ui: &imgui::Ui, interface: &mut crate::interface::Interface) {
    for path in [
        "/traffic/models/status",
        "/traffic/models/install",
        "/traffic/models/cancel",
    ] {
        if let Some(reply) = interface.engine.take_reply(path) {
            if path == "/traffic/models/status" {
                interface.model_install_pending = false;
            }
            if !reply.success {
                interface.model_install_status["stage"] = "failed".into();
                interface.model_install_status["error"] = reply.error.into();
            } else if path != "/traffic/models/cancel" {
                interface.model_install_status = reply.data;
            }
        }
    }
    let now = ui.time();
    if interface.engine.connected()
        && !interface.model_install_pending
        && (interface.model_install_checked == 0.0 || now - interface.model_install_checked >= 0.5)
    {
        interface.model_install_pending = true;
        interface.model_install_checked = now;
        interface
            .engine
            .post("/traffic/models/status", serde_json::json!({}));
    }
    let status = &interface.model_install_status;
    let stage = status["stage"].as_str().unwrap_or("");
    let busy = matches!(
        stage,
        "preparing" | "downloading" | "extracting" | "installing" | "cancelling"
    );
    widgets::section_title(ui, "BLUEBELL AIRCRAFT MODELS");
    if busy {
        ui.text(format!(
            "{} — archive {} of {}",
            match stage {
                "downloading" => "Downloading",
                "extracting" => "Extracting",
                "installing" => "Installing",
                "cancelling" => "Cancelling",
                _ => "Preparing download",
            },
            (status["completed"].as_u64().unwrap_or(0) + 1)
                .min(status["archiveCount"].as_u64().unwrap_or(12)),
            status["archiveCount"].as_u64().unwrap_or(12)
        ));
        ui.text(status["archive"].as_str().unwrap_or(""));
        let downloaded = status["downloaded"].as_u64().unwrap_or(0) as f64 / 1_048_576.0;
        let total = status["total"].as_u64().unwrap_or(0) as f64 / 1_048_576.0;
        if total > 0.0 {
            imgui::ProgressBar::new((downloaded / total).min(1.0) as f32)
                .size([ui.content_region_avail()[0], 0.0])
                .overlay_text(format!("{downloaded:.1} / {total:.1} MiB"))
                .build(ui);
        } else {
            ui.text(format!("{downloaded:.1} MiB downloaded"));
        }
        ui.text(format!(
            "{} files extracted",
            status["files"].as_u64().unwrap_or(0)
        ));
        if ui.button("Cancel download") {
            interface
                .engine
                .post("/traffic/models/cancel", serde_json::json!({}));
        }
    } else if stage == "installed" {
        ui.text_colored(
            widgets::accent(ui),
            format!(
                "Bluebell installed — {} model objects in {} packages",
                status["objects"].as_u64().unwrap_or(0),
                status["packages"].as_u64().unwrap_or(0)
            ),
        );
        ui.text_wrapped(status["path"].as_str().unwrap_or(""));
    } else {
        if stage == "failed" || stage == "cancelled" {
            ui.text_wrapped(status["error"].as_str().unwrap_or("Download cancelled."));
        }
        let _disabled = ui.begin_disabled(!interface.engine.connected());
        if ui.button(if stage.is_empty() {
            "Download and install Bluebell"
        } else {
            "Retry Bluebell installation"
        }) {
            interface
                .engine
                .post("/traffic/models/install", serde_json::json!({}));
            interface.model_install_status =
                serde_json::json!({"stage":"preparing","archiveCount":12});
        }
    }
    ui.text_wrapped("Downloads the publisher's DDS model packs and installs them in OpenATC's CSL folder. Allow several GB of disk space. Completed downloads are kept for retry if interrupted. Models retain their original licences.");
    ui.spacing();
}

/// Worker progress is polled only while this page is visible. No files or database on the UI thread.
pub(crate) fn scenery_setup(
    ui: &imgui::Ui,
    interface: &mut crate::interface::Interface,
    state: Option<&State>,
) {
    widgets::section_title(ui, "TRAFFIC SETUP");
    if let Some(reply) = interface.engine.take_reply("/traffic/scenery/status") {
        interface.scenery_pending = false;
        if reply.success {
            interface.scenery_status = reply.data;
        } else {
            interface.scenery_status["error"] = reply.error.into();
        }
    }
    if let Some(reply) = interface.engine.take_reply("/traffic/scenery/clear") {
        interface.traffic_notice = if reply.success {
            "Scenery index cleared. It rebuilds as new traffic is prepared; active flights are retained.".into()
        } else {
            reply.error
        };
        interface.scenery_checked = -1.0;
    }
    let now = ui.time();
    if interface.engine.connected()
        && (!interface.scenery_pending || now - interface.scenery_checked > 10.0)
        && (interface.scenery_checked < 0.0 || now - interface.scenery_checked >= 1.0)
    {
        interface.scenery_pending = true;
        interface.scenery_checked = now;
        interface
            .engine
            .post("/traffic/scenery/status", serde_json::json!({}));
    }
    if !interface.engine.connected() {
        ui.text_wrapped("Waiting for the companion engine. Traffic preparation starts after the simulator and airport data are available.");
        return;
    }
    let status = &interface.scenery_status;
    let no_plan = state
        .is_some_and(|state| state.plan.departure.is_empty() || state.plan.destination.is_empty());
    if no_plan {
        ui.text_wrapped(format!(
            "Airport traffic, no flight plan — {} aircraft around you.",
            state.map_or(0, |state| state.generated_traffic.len())
        ));
        if state.is_some_and(|state| state.generated_traffic.is_empty()) {
            ui.text_wrapped(
                "No aircraft are visible yet. Check that the local airport loaded and that Bluebell models are installed below.",
            );
        }
    } else {
        ui.text_wrapped(
            status["traffic"]
                .as_str()
                .filter(|text| !text.is_empty())
                .unwrap_or("Waiting for simulator position and airport data"),
        );
    }
    let cache = &status["cache"];
    ui.text_wrapped(cache["phase"].as_str().filter(|text| !text.is_empty()).unwrap_or("Scenery is indexed on demand. The first visit takes longer; later visits reuse unchanged scenery."));
    if let Some(tile) = cache["currentTile"]
        .as_str()
        .filter(|tile| !tile.is_empty())
    {
        ui.text(format!("Scenery tile: {tile}"));
    }
    ui.text(format!(
        "Cached tiles used: {} | Prepared: {} | Saved: {}",
        cache["hits"].as_u64().unwrap_or(0),
        cache["misses"].as_u64().unwrap_or(0),
        cache["writes"].as_u64().unwrap_or(0)
    ));
    if let Some(error) = cache["error"].as_str().filter(|text| !text.is_empty()) {
        ui.text_wrapped(error);
    }
    if let Some(error) = status["error"].as_str() {
        ui.text_wrapped(error);
    }
    if status["enabled"].as_bool() == Some(false) {
        ui.text_wrapped("Generated traffic is switched off. Enable it below to prepare flights.");
    }
    if ui.collapsing_header("Scenery cache details", imgui::TreeNodeFlags::empty()) {
        ui.text_wrapped(
            cache["path"]
                .as_str()
                .unwrap_or("The cache has not opened yet"),
        );
        ui.text(format!(
            "Retained scenery: {:.1} MiB / 512 MiB per planner",
            cache["retainedBytes"].as_u64().unwrap_or(0) as f64 / 1_048_576.0
        ));
        ui.text_wrapped("Changed scenery is rebuilt automatically. Clearing the index keeps your settings and flight plans. Cache readiness does not mean all routes have passed the traffic checks.");
        let _disabled = ui.begin_disabled(status["preparing"].as_bool().unwrap_or(false));
        if ui.button("Clear scenery index") {
            interface
                .engine
                .post("/traffic/scenery/clear", serde_json::json!({}));
        }
    }
}

#[cfg(test)]
mod map_tests {
    use super::*;
    use openatc_core::state::{GeneratedTraffic, TrafficTarget};

    #[test]
    fn frequencies_are_formatted_or_dashed() {
        assert_eq!(frequency_text(Some(118_700)), "118.700");
        assert_eq!(frequency_text(None), "—");
        assert_eq!(frequency_text(Some(0)), "—");
    }

    #[test]
    fn punctuation_does_not_duplicate_a_generated_target() {
        let mut state = State::default();
        state.telemetry.traffic = Some(vec![TrafficTarget {
            id: 7,
            callsign: "FOMBT".into(),
            ..Default::default()
        }]);
        state.generated_traffic.push(GeneratedTraffic {
            target: TrafficTarget {
                id: 14_680_064,
                callsign: "F-OMBT".into(),
                ..Default::default()
            },
            model: std::sync::Arc::new(openatc_core::traffic::csl::Model {
                id: "TEST".into(),
                objects: vec![],
                vertical_offset: None,
                matches: vec![],
            }),
            vertical_speed_fpm: 0.0,
            body_heading_degrees: None,
            animation: Default::default(),
        });
        let shown: Vec<_> = targets(&state).collect();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].id, 14_680_064);
    }

    #[test]
    fn fresh_simulator_samples_keep_generated_map_targets_without_duplicates() {
        let mut state = State::default();
        state.telemetry.traffic = Some(vec![
            TrafficTarget {
                id: 1,
                ..Default::default()
            },
            TrafficTarget {
                id: 2,
                latitude: 1.0,
                ..Default::default()
            },
        ]);
        state.generated_traffic.push(GeneratedTraffic {
            target: TrafficTarget {
                id: 2,
                latitude: 2.0,
                ..Default::default()
            },
            model: std::sync::Arc::new(openatc_core::traffic::csl::Model {
                id: "TEST".into(),
                objects: vec![],
                vertical_offset: None,
                matches: vec![],
            }),
            vertical_speed_fpm: 0.0,
            body_heading_degrees: None,
            animation: Default::default(),
        });
        let shown: Vec<_> = targets(&state).collect();
        assert_eq!(shown.len(), 2);
        assert_eq!(
            shown.iter().find(|target| target.id == 2).unwrap().latitude,
            2.0
        );
        state.telemetry.traffic = None;
        assert_eq!(targets(&state).count(), 1);
        assert!(
            state.telemetry.traffic.is_none(),
            "Do not hide an unavailable native feed"
        );
    }
}
