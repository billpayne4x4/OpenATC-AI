//! Communication, radio, flight planning, airport and settings pages.

use crate::interface::Interface;
use crate::widgets::{self, AMBER, MUTED};
use openatc_core::ops::{phase_name, request_available};
use openatc_core::state::{Request, State};
use openatc_core::{altitude_text, feet_to_meters, meters_to_feet, resolve_units};
use std::fmt::Write as _;

/// Request button identifiers and display titles.
const REQUEST_BUTTONS: [(&str, &str); 23] = [
    ("clearance", "Request IFR clearance"),
    ("start", "Request start-up"),
    ("pushback", "Request pushback"),
    ("start_pushback", "Request start-up and pushback"),
    ("taxi", "Request taxi"),
    ("backtrack", "Request runway backtrack"),
    ("ready", "Ready for departure"),
    ("gate", "Taxi to parking"),
    ("progressive", "Repeat taxi instructions"),
    ("radio_check", "Radio check"),
    ("readback", "Read back clearance"),
    ("repeat", "Say again"),
    ("standby", "Stand by"),
    ("unable", "Unable"),
    ("frequency", "Request frequency change"),
    ("position", "Confirm position"),
    ("altitude", "Request altitude..."),
    ("direct", "Request direct-to..."),
    ("descent", "Request descent..."),
    ("cancel_ifr", "Cancel IFR"),
    ("approach", "Brief arrival approach"),
    ("go_around", "Going around"),
    ("emergency", "Declare emergency"),
];

/// Definition titles needing the altitude/waypoint modal.
fn needs_modal(intent: &str) -> bool {
    matches!(intent, "altitude" | "direct" | "descent")
}

/// Request editor with live availability, contained inside the plugin window.
fn draw_request_editor(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    if interface.modal_intent.is_empty() {
        return;
    }
    let units = resolve_units(&units_name(interface), &state.plan.departure);
    let metric = units == openatc_core::UnitSystem::Metric;
    ui.child_window("ATC request")
        .size([0.0, 0.0])
        .border(true)
        .build(|| {
            ui.text(&interface.modal_title);
            ui.spacing();
            if interface.modal_intent == "direct" {
                ui.set_next_item_width(320.0);
                widgets::edit_text_flags(
                    ui,
                    "Waypoint",
                    &mut interface.requested_waypoint,
                    32,
                    imgui::InputTextFlags::CHARS_UPPERCASE,
                );
            } else {
                ui.set_next_item_width(230.0);
                let mut shown = if metric {
                    // Integral by construction (rounded tens of meters), tiny range.
                    #[allow(clippy::cast_possible_truncation)]
                    let meters = (feet_to_meters(f64::from(interface.requested_altitude)) / 10.0)
                        .round() as i32
                        * 10;
                    meters
                } else {
                    interface.requested_altitude
                };
                let before = shown;
                ui.input_int(
                    if metric {
                        "Altitude (m)"
                    } else {
                        "Altitude (ft)"
                    },
                    &mut shown,
                )
                .step(if metric { 10 } else { 1000 })
                .build();
                if shown != before {
                    interface.requested_altitude =
                        if metric { meters_to_feet(shown) } else { shown };
                }
                ui.text_disabled(format!(
                    "Current altitude: {}",
                    altitude_text(state.telemetry.altitude_feet, units, false)
                ));
            }
            let available = request_available(state, &interface.modal_intent);
            if !available {
                ui.text_colored(AMBER, "Flight stage changed. Request no longer available.");
            }
            let blocked = !available || !interface.engine.connected() || !interface.radio_power;
            let _disabled = ui.begin_disabled(blocked);
            if ui.button_with_size("Send request", [150.0, 0.0]) {
                let intent = interface.modal_intent.clone();
                let request = Request {
                    intent: intent.clone(),
                    altitude_feet: interface.requested_altitude,
                    waypoint: interface.requested_waypoint.clone(),
                    text: if intent == "direct" {
                        format!("Request direct to {}", interface.requested_waypoint)
                    } else {
                        format!(
                            "Request {intent} {}",
                            altitude_text(f64::from(interface.requested_altitude), units, true)
                        )
                    },
                    ..Default::default()
                };
                interface.send(&request);
                interface.modal_intent.clear();
            }
            ui.same_line();
            if ui.button_with_size("Cancel", [100.0, 0.0]) {
                interface.modal_intent.clear();
            }
        });
}

fn units_name(interface: &Interface) -> String {
    format!("{:?}", interface.settings.units).to_lowercase()
}

/// Radio page: airport loader, station table and copilot toggles.
pub fn radio_page(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    if !interface.station_filter_initialized
        && let Some(station) = interface
            .nearby_stations
            .iter()
            .filter(|s| s.service != "Center")
            .min_by(|a, b| a.distance_nm.total_cmp(&b.distance_nm))
    {
        interface.station_filter.clone_from(&station.airport);
        interface.station_filter_initialized = true;
    }
    radio_header(ui, interface, state);
    ui.text_disabled("Filter stations / airport");
    ui.set_next_item_width((ui.content_region_avail()[0] - 105.0).max(100.0));
    if ui
        .input_text("##station-filter", &mut interface.station_filter)
        .build()
    {
        interface.station_filter_initialized = true;
    }
    ui.same_line();
    if ui.button("Show all") {
        interface.station_filter.clear();
        interface.station_filter_initialized = true;
    }
    radio_list(ui, interface, state);
    radio_footer(ui, interface);
}

/// Airport loader row, COM1 readout and power notices.
fn radio_header(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    ui.text_colored(
        widgets::ACCENT,
        format!(
            "CHANNELS  /  {} stations in reception range",
            interface.nearby_stations.len()
        ),
    );
    ui.text_disabled(format!(
        "COM1  {:.3} MHz",
        f64::from(state.telemetry.com1_khz) / 1000.0
    ));
    ui.text_wrapped(if interface.simulator_host {
        "Select a station to tune COM1. The current station is highlighted."
    } else {
        "Nearby airport services. COM1 tuning is available inside X-Plane."
    });
    if !interface.aircraft_profile.is_empty() {
        ui.text_disabled(&interface.aircraft_profile);
    }
    if !interface.radio_power {
        ui.text_colored(AMBER, "Aircraft radio power is off.");
    }
    if interface.stations_loading {
        ui.text_disabled("Loading nearby stations…");
    }
    if !interface.radio_notice.is_empty() {
        ui.text_wrapped(&interface.radio_notice);
    }
}

/// Station table with ATIS split and tune-on-select.
fn radio_list(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    let search = interface.station_filter.trim().to_lowercase();
    let stations: Vec<_> = interface
        .nearby_stations
        .iter()
        .filter(|s| {
            format!("{} {} {} {}", s.airport, s.airport_name, s.name, s.service)
                .to_lowercase()
                .contains(&search)
        })
        .cloned()
        .collect();
    let scale = ui.current_font_size() / 18.0;
    let list_height = (ui.content_region_avail()[1] - 125.0 * scale).max(140.0 * scale);
    ui.child_window("##radio-list")
        .size([0.0, list_height])
        .build(|| {
            for (is_atis, title) in [(false, "CONTROLLERS"), (true, "ATIS / WEATHER")] {
                widgets::section_title(ui, title);
                if ui.content_region_avail()[0] < 650.0 * scale {
                    for frequency in &stations {
                        if (frequency.service == "ATIS") != is_atis {
                            continue;
                        }
                        let _id = ui.push_id(format!(
                            "{}-{}-{}",
                            frequency.airport, frequency.khz, frequency.service
                        ));
                        let label = format!("{} • {}", frequency.airport, frequency.name);
                        let pos = ui.cursor_screen_pos();
                        let width = ui.content_region_avail()[0];
                        let height =
                            ui.calc_text_size_with_opts(&label, false, width - 16.0 * scale)[1]
                                + 34.0 * scale;
                        let tuned = state.telemetry.com1_khz == frequency.khz;
                        {
                            let _disabled = ui.begin_disabled(
                                !interface.simulator_host || !interface.radio_power,
                            );
                            if ui
                                .selectable_config("##station")
                                .selected(tuned)
                                .size([width, height])
                                .build()
                            {
                                interface.requested_frequency_khz = Some(frequency.khz);
                            }
                        }
                        let next = ui.cursor_screen_pos();
                        ui.set_cursor_screen_pos([pos[0] + 8.0 * scale, pos[1] + 3.0 * scale]);
                        ui.text_wrapped(&label);
                        let service = if frequency.service == "Clearance" {
                            "Delivery"
                        } else {
                            &frequency.service
                        };
                        ui.text_colored(
                            if tuned { widgets::ACCENT } else { MUTED },
                            format!(
                                "{service}  •  {:.3} MHz  •  {}{}",
                                f64::from(frequency.khz) / 1000.0,
                                if frequency.service == "Center" {
                                    "Sector coverage".to_owned()
                                } else {
                                    format!("{:.1} NM", frequency.distance_nm)
                                },
                                if tuned { " • tuned" } else { "" }
                            ),
                        );
                        ui.set_cursor_screen_pos(next);
                        ui.separator();
                    }
                    continue;
                }
                let flags = imgui::TableFlags::ROW_BG
                    | imgui::TableFlags::SORTABLE
                    | imgui::TableFlags::SIZING_STRETCH_PROP;
                if let Some(_table) = ui.begin_table_with_flags(title, 4, flags) {
                    ui.table_setup_column("Station");
                    ui.table_setup_column("Service");
                    ui.table_setup_column("Frequency");
                    ui.table_setup_column("Distance / status");
                    ui.table_headers_row();
                    let mut rows = stations.clone();
                    sort_table(ui, &mut rows, |s, column| match column {
                        0 => format!("{} {}", s.airport, s.name),
                        1 => s.service.clone(),
                        2 => format!("{:09}", s.khz),
                        _ => format!("{:020.6}", s.distance_nm),
                    });
                    for frequency in &rows {
                        let service = frequency.service.to_ascii_lowercase();
                        let weather = service.contains("atis")
                            || service.contains("awos")
                            || service.contains("asos");
                        if weather != is_atis {
                            continue;
                        }
                        let _identifier = ui.push_id(format!(
                            "{}-{}-{}",
                            frequency.khz, frequency.service, frequency.name
                        ));
                        let recommended = state.recommended_frequency_khz == frequency.khz;
                        let tuned = state.telemetry.com1_khz == frequency.khz;
                        ui.table_next_row();
                        ui.table_next_column();
                        let label = format!(
                            "{} • {}{}",
                            frequency.airport,
                            frequency.name,
                            if recommended { " *" } else { "" }
                        );
                        let _disabled =
                            ui.begin_disabled(!interface.simulator_host || !interface.radio_power);
                        if ui
                            .selectable_config(&label)
                            .selected(tuned)
                            .span_all_columns(true)
                            .build()
                        {
                            interface.requested_frequency_khz = Some(frequency.khz);
                        }
                        ui.table_next_column();
                        ui.text(if frequency.service == "Clearance" {
                            "Delivery"
                        } else {
                            &frequency.service
                        });
                        ui.table_next_column();
                        ui.text_colored(
                            if tuned { widgets::ACCENT } else { MUTED },
                            format!("{:.3}", f64::from(frequency.khz) / 1000.0),
                        );
                        ui.table_next_column();
                        ui.text(format!(
                            "{}{}",
                            if frequency.service == "Center" {
                                "Sector coverage".to_owned()
                            } else {
                                format!("{:.1} NM", frequency.distance_nm)
                            },
                            if tuned { " • tuned" } else { "" }
                        ));
                    }
                }
            }
            if interface.nearby_stations.is_empty() {
                ui.text_wrapped(
                    "Nearby frequencies will appear when the simulator position is available.",
                );
            }
        });
}

/// Copilot radio toggles with instant save.
fn radio_footer(ui: &imgui::Ui, interface: &mut Interface) {
    let mut changed = ui.checkbox(
        "Copilot reads back clearances",
        &mut interface.settings.copilot_replies,
    );
    changed |= ui.checkbox(
        "Copilot tunes assigned COM1 frequency",
        &mut interface.settings.copilot_tunes,
    );
    changed |= ui.checkbox(
        "Copilot retunes on ATC handoffs",
        &mut interface.settings.auto_tune_handoff,
    );
    if changed {
        interface.save_settings();
    }
}

/// Top of the ATC page: pin toggle, phase, COM1 readout and the clearance card.
fn header_card(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    ui.text_colored(widgets::ACCENT, phase_name(state.phase));
    if state.telemetry.com1_khz > 0 {
        ui.same_line();
        ui.text_disabled(format!(
            "COM1 {:.3}",
            f64::from(state.telemetry.com1_khz) / 1000.0
        ));
    }
    if let Some(clearance) = &state.clearance {
        let units = resolve_units(&units_name(interface), &state.plan.departure);
        ui.child_window("Clearance")
            .size([0.0, 44.0 + 40.0 * interface.settings.ui_scale])
            .border(true)
            .scroll_bar(false)
            .build(|| {
                ui.text_colored(
                    widgets::ACCENT,
                    format!(
                        "CLEARANCE #{}   {}",
                        clearance.sequence,
                        if clearance.acknowledged {
                            "READ BACK"
                        } else {
                            "READBACK REQUIRED"
                        }
                    ),
                );
                ui.text_wrapped(format!(
                    "{}  /  {}  /  Squawk {}  /  Runway {}",
                    altitude_text(f64::from(clearance.altitude_feet), units, false),
                    clearance.route,
                    clearance.squawk,
                    clearance.runway
                ));
            });
    }
}

fn transcript_view(ui: &imgui::Ui, interface: &mut Interface, state: &State, height: f32) {
    let scale = ui.current_font_size() / 18.0;
    let background_alpha = interface.background_opacity * 0.72;
    let _background = ui.push_style_color(
        imgui::StyleColor::ChildBg,
        [0.10, 0.12, 0.14, background_alpha],
    );
    let _padding = ui.push_style_var(imgui::StyleVar::WindowPadding([12.0 * scale, 10.0 * scale]));
    ui.child_window("Transcript").size([0.0, height]).border(false).always_use_window_padding(true).build(|| {
        let following_latest = ui.scroll_y() >= ui.scroll_max_y() - 8.0 * scale;
        if state.transcript.is_empty() {
            ui.text_colored(widgets::ACCENT, "Ready when you are");
            ui.text_wrapped("Set up your flight plan, then request clearance. Available requests follow your flight stage.");
        }
        for entry in &state.transcript {
            let _identifier = ui.push_id(entry.sequence.to_string());
            let airport = if interface.local_airport.icao.is_empty() { &interface.airport } else { &interface.local_airport };
            let badge = transcript_badge(entry, airport, state.telemetry.com1_khz);
            let color = match entry.speaker.as_str() {
                "ATC" => [0.48, 0.77, 0.94, 1.0],
                "CABIN" => [0.75, 0.83, 0.77, 1.0],
                "GROUND" => [0.86, 0.79, 0.59, 1.0],
                "COPILOT" => [0.76, 0.79, 0.89, 1.0],
                "SYSTEM" => AMBER,
                _ => [0.94, 0.95, 0.96, 1.0],
            };
            transcript_line(ui, &badge, &entry.text, color, scale);
        }
        for notice in &interface.local_notices {
            transcript_line(ui, "INFO", &notice.text, AMBER, scale);
        }
        if let Some(last_entry) = state.transcript.last()
            && last_entry.sequence != interface.last_transcript_sequence
        {
            if following_latest || interface.last_transcript_sequence == 0 {
                ui.set_scroll_here_y_with_ratio(1.0);
            }
            interface.last_transcript_sequence = last_entry.sequence;
        }
    });
}

/// Prefer the recorded station; older entries store just a service name.
fn transcript_badge(
    entry: &openatc_core::state::Transmission,
    airport: &openatc_core::airport::Airport,
    tuned_khz: i32,
) -> String {
    match entry.speaker.as_str() {
        "COPILOT" => "Copilot".to_owned(),
        "CABIN" => "Attendant".to_owned(),
        "GROUND" => "Ground services".to_owned(),
        "SYSTEM" => "INFO".to_owned(),
        "ATC" => {
            let position = entry.position.trim();
            let station = airport
                .frequencies
                .iter()
                .filter(|frequency| {
                    if position.is_empty() {
                        frequency.khz == tuned_khz
                    } else {
                        frequency.service.eq_ignore_ascii_case(position)
                    }
                })
                .min_by_key(|frequency| frequency.khz != tuned_khz);
            let name = station.map_or(position, |frequency| {
                if frequency.name.trim().is_empty() {
                    frequency.service.trim()
                } else {
                    frequency.name.trim()
                }
            });
            if name.is_empty() {
                "ATC".to_owned()
            } else {
                format!("ATC - {name}")
            }
        }
        _ => "YOU".to_owned(),
    }
}

fn transcript_line(ui: &imgui::Ui, badge: &str, text: &str, color: [f32; 4], scale: f32) {
    let position = ui.cursor_screen_pos();
    let label_size = ui.calc_text_size(badge);
    let size = [
        (label_size[0] + 20.0 * scale).max(68.0 * scale),
        25.0 * scale,
    ];
    if ui.content_region_avail()[0] < size[0] + 160.0 * scale {
        let _color = ui.push_style_color(imgui::StyleColor::Text, color);
        ui.text_wrapped(badge);
        ui.text_wrapped(text);
        selectable_message(ui, text);
        return;
    }
    let draw = ui.get_window_draw_list();
    draw.add_rect(
        position,
        [position[0] + size[0], position[1] + size[1]],
        [color[0], color[1], color[2], 0.16],
    )
    .filled(true)
    .rounding(2.0 * scale)
    .build();
    draw.add_rect(
        position,
        [position[0] + size[0], position[1] + size[1]],
        color,
    )
    .rounding(2.0 * scale)
    .build();
    draw.add_text(
        [
            position[0] + (size[0] - label_size[0]) * 0.5,
            position[1] + (size[1] - label_size[1]) * 0.5,
        ],
        color,
        badge,
    );
    ui.dummy(size);
    ui.same_line();
    let _text_color = ui.push_style_color(imgui::StyleColor::Text, color);
    ui.text_wrapped(text);
    selectable_message(ui, text);
}

fn selectable_message(ui: &imgui::Ui, text: &str) {
    if ui.is_item_clicked() {
        ui.open_popup("Select message");
    }
    if ui.is_item_hovered() {
        ui.tooltip_text("Click to select or copy this message");
    }
    if let Some(_popup) = ui.begin_popup("Select message") {
        ui.text_disabled("Select text and press Ctrl+C, or copy the whole message.");
        if ui.button("Copy message") {
            ui.set_clipboard_text(text);
        }
        let mut selected = text.to_owned();
        ui.input_text_multiline(
            "##selected_message",
            &mut selected,
            [(ui.io().display_size[0] - 64.0).clamp(220.0, 600.0), 180.0],
        )
        .read_only(true)
        .build();
    }
}

/// Message box with Talk and Transmit plus the power note.
fn talk_row(ui: &imgui::Ui, interface: &mut Interface, crew: &str) {
    let scale = ui.current_font_size() / 18.0;
    let button_space = if interface.settings.copilot_button {
        236.0
    } else {
        126.0
    };
    ui.set_next_item_width(
        (ui.content_region_avail()[0] - button_space * scale).max(100.0 * scale),
    );
    let mut message = std::mem::take(&mut interface.message);
    let submitted = widgets::edit_text_flags(
        ui,
        "##message",
        &mut message,
        4096,
        imgui::InputTextFlags::ENTER_RETURNS_TRUE,
    );
    interface.message = message;
    ui.same_line();
    let power_ok =
        openatc_core::intents::can_transmit(crew, interface.radio_power, interface.bus_power);
    if interface.settings.copilot_button {
        let enabled = interface.engine.connected();
        let _talk_disabled = ui.begin_disabled(!enabled);
        if ui.button_with_size("Talk", [102.0 * scale, 0.0]) {
            interface.transmit_box_to_copilot();
        }
        ui.same_line();
    }
    let blocked = !interface.engine.connected() || !power_ok;
    let _tx_disabled = ui.begin_disabled(blocked);
    let transmit = ui.button_with_size("Transmit", [118.0 * scale, 0.0]) || submitted;
    if transmit {
        interface.transmit_box();
    }
    if interface.engine.connected() && !power_ok {
        ui.text_colored(
            AMBER,
            if crew == "atc" {
                "Transmit needs radio power — Talk to the copilot still works."
            } else {
                "Transmit needs electrical power — Talk to the copilot still works."
            },
        );
    }
}

/// Record and replay row with the mic status and dev readout.
fn record_row(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    let speech_busy = interface.speech.busy() || interface.speech.monitoring();
    let rec_disabled = ui.begin_disabled(speech_busy);
    if ui.button(if interface.speech.recording() {
        "Stop + transcribe"
    } else {
        "Record request"
    }) {
        if interface.speech.recording() {
            let endpoint = interface.engine.endpoint();
            interface.speech.transcribe(&endpoint);
        } else {
            interface.speech.start_recording(false);
        }
    }
    ui.same_line();
    if ui.button("Replay ATC")
        && let Some(entry) = state
            .transcript
            .iter()
            .rev()
            .find(|entry| entry.speaker == "ATC")
    {
        let entry = entry.clone();
        interface.speak_entry(&entry);
    }
    ui.same_line();
    let reply = openatc_core::readback::auto_reply(state);
    {
        let _disabled = ui.begin_disabled(
            reply.is_none()
                || interface.auto_reply_pending
                || !interface.engine.connected()
                || !interface.radio_power,
        );
        if ui.button("ATC Auto Reply") {
            interface.auto_reply_pending = true;
            "Sending current ATC readback".clone_into(&mut interface.last_action);
            interface
                .engine
                .post("/request/auto-reply", serde_json::json!({}));
        }
    }
    if ui.is_item_hovered_with_flags(imgui::ItemHoveredFlags::ALLOW_WHEN_DISABLED) {
        ui.tooltip_text("Reply to your IFR, taxi, start-up or pushback approval. Background traffic and messages with no pending instruction do not need an automatic reply.");
    }
    ui.same_line();
    let speech_status = interface.speech.status();
    if ui.content_region_avail()[0] < ui.calc_text_size(&speech_status)[0] {
        ui.new_line();
    }
    ui.text_disabled(speech_status);
    drop(rec_disabled);
    if interface.settings.dev_mode {
        ui.new_line();
        if ui.button("Copy ATC conversation") {
            let mut text = crate::debug::conversation(state);
            let _ = writeln!(
                text,
                "\nDispatch draft: {} -> {} | runway {} | imported {}",
                interface.draft.departure,
                interface.draft.destination,
                interface.draft.runway,
                interface.plan_loaded
            );
            let copied = (|| {
                if interface.clipboard.is_none() {
                    interface.clipboard = arboard::Clipboard::new().ok();
                }
                if let Some(clipboard) = interface.clipboard.as_mut()
                    && clipboard.set_text(text.clone()).is_ok()
                {
                    return Ok(());
                }
                crate::debug::copy(&text)
            })();
            interface.notice = match copied {
                Ok(()) => "Conversation and flight/radio diagnostics copied.".to_owned(),
                Err(error) => {
                    let path = std::env::var_os("HOME")
                        .map_or_else(std::env::temp_dir, std::path::PathBuf::from)
                        .join("openatc-conversation.txt");
                    match std::fs::write(&path, text) {
                        Ok(()) => format!("{error} Conversation saved to {}", path.display()),
                        Err(write_error) => format!("{error} Export failed: {write_error}"),
                    }
                }
            };
        }
        ui.same_line();
        ui.text_disabled(format!(
            "DBG: {}",
            if interface.last_action.is_empty() {
                "idle"
            } else {
                &interface.last_action
            }
        ));
    }
    if !interface.notice.is_empty() {
        let notice = interface.notice.clone();
        ui.text_wrapped(&notice);
    }
}

fn request_grid(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    let scale = ui.current_font_size() / 18.0;
    ui.set_next_item_width((ui.content_region_avail()[0] - 160.0 * scale).max(100.0 * scale));
    widgets::edit_text(ui, "Filter requests", &mut interface.search, 128);
    let filter = interface.search.to_lowercase();
    ui.child_window("Requests").size([0.0, 0.0]).build(|| {
        let _alignment = ui.push_style_var(imgui::StyleVar::ButtonTextAlign([0.0, 0.5]));
        for (intent, title) in REQUEST_BUTTONS {
            if !request_available(state, intent)
                || (!filter.is_empty() && !title.to_lowercase().contains(&filter))
            {
                continue;
            }
            let _disabled =
                ui.begin_disabled(!interface.engine.connected() || !interface.radio_power);
            if ui.button_with_size(title, [ui.content_region_avail()[0], 34.0 * scale]) {
                if needs_modal(intent) {
                    intent.clone_into(&mut interface.modal_intent);
                    title.clone_into(&mut interface.modal_title);
                    #[allow(clippy::cast_possible_truncation)]
                    let below = state.telemetry.altitude_feet as i32 - 4000;
                    interface.requested_altitude = if intent == "descent" {
                        below.max(1000)
                    } else {
                        state.plan.cruise_feet
                    };
                } else {
                    let mut request = Request {
                        intent: intent.to_owned(),
                        text: title.to_owned(),
                        ..Default::default()
                    };
                    if intent == "readback" && state.taxi_clearance.pending_readback {
                        request.clearance_sequence = state.taxi_clearance.sequence;
                        request
                            .waypoint
                            .clone_from(&state.taxi_clearance.instructions);
                    } else if intent == "readback"
                        && let Some(clearance) = &state.clearance
                    {
                        request.clearance_sequence = clearance.sequence;
                        request.altitude_feet = clearance.altitude_feet;
                        request.waypoint.clone_from(&clearance.route);
                    }
                    interface.send(&request);
                }
            }
        }
    });
}

/// ATC page: clearance card, transcript, talk box and request grid.
pub fn atc_page(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    let scale = ui.current_font_size() / 18.0;
    if interface.compact {
        transcript_view(ui, interface, state, 0.0);
        return;
    }
    header_card(ui, interface, state);
    if state.taxi_clearance.pending_readback {
        ui.text_colored(
            AMBER,
            "TAXI READBACK REQUIRED — guidance starts after acceptance",
        );
        ui.text_wrapped(&state.taxi_clearance.instructions);
    } else if state.taxi_clearance.guidance_complete {
        ui.text_colored(
            AMBER,
            if state.taxi_clearance.backtrack_required {
                "HOLD POINT REACHED — contact Tower and request runway backtrack to the departure end."
            } else if state.taxi_clearance.runway_taxi {
                "DEPARTURE END REACHED — hold position; report ready to Tower. Takeoff clearance is required."
            } else { "HOLD POINT REACHED — hold position; contact Tower when ready for departure." },
        );
    }
    if !interface.modal_intent.is_empty() {
        draw_request_editor(ui, interface, state);
        return;
    }
    let available_height = ui.content_region_avail()[1];
    let transcript_height = (available_height * interface.transcript_fraction)
        .min((available_height - 215.0 * scale).max(80.0 * scale))
        .max(80.0 * scale);
    transcript_view(ui, interface, state, transcript_height);
    let splitter_position = ui.cursor_screen_pos();
    let splitter_width = ui.content_region_avail()[0];
    ui.invisible_button("##transcript-divider", [splitter_width, 8.0 * scale]);
    if ui.is_item_hovered() || ui.is_item_active() {
        ui.set_mouse_cursor(Some(imgui::MouseCursor::ResizeNS));
    }
    if ui.is_item_active() {
        interface.transcript_fraction = ((transcript_height + ui.io().mouse_delta[1])
            / available_height.max(1.0))
        .clamp(0.2, 0.8);
    }
    let draw = ui.get_window_draw_list();
    for offset in [-7.0, 0.0, 7.0] {
        draw.add_circle(
            [
                splitter_position[0] + splitter_width * 0.5 + offset * scale,
                splitter_position[1] + 4.0 * scale,
            ],
            1.5 * scale,
            MUTED,
        )
        .filled(true)
        .build();
    }
    ui.child_window("##communication-controls")
        .size([0.0, 0.0])
        .build(|| {
            let crew = interface.crew_role();
            ui.text_disabled(format!(
                "{}   /   {}",
                state.plan.callsign,
                if crew == "ground" {
                    "GROUND CREW"
                } else if crew == "cabin" {
                    "CABIN"
                } else {
                    "ATC"
                }
            ));
            talk_row(ui, interface, &crew);
            record_row(ui, interface, state);
            if !interface.engine_fault.is_empty() {
                ui.text_colored(AMBER, &interface.engine_fault);
            }
            request_grid(ui, interface, state);
        });
}

/// Dispatch page: `SimBrief` import and the flight used for clearance.
pub fn plan_page(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    let _import_disabled = ui.begin_disabled(interface.importing || !interface.engine.connected());
    if ui.button(if interface.importing {
        "Downloading OFP..."
    } else {
        "Import from SimBrief"
    }) {
        interface.importing = true;
        interface.engine.post(
            "/simbrief",
            serde_json::json!({"userid": interface.settings.simbrief_id}),
        );
    }
    if ui.content_region_avail()[0] >= 700.0 {
        ui.same_line();
    }
    ui.text_wrapped(format!(
        "Latest generated OFP / Pilot ID: {}",
        if interface.settings.simbrief_id.is_empty() {
            "set in Settings"
        } else {
            &interface.settings.simbrief_id
        }
    ));
    if !interface.plan_notice.is_empty() {
        ui.text_wrapped(&interface.plan_notice);
    }
    if let Some(_tabs) = ui.tab_bar("DispatchTabs") {
        if let Some(_tab_item) = ui.tab_item("Flight & route") {
            flight_tab(ui, interface, state);
        }
        if let Some(_tab_item) = ui.tab_item("Procedures") {
            procedures_tab(ui, interface);
        }
        if let Some(_tab_item) = ui.tab_item("Fuel & payload") {
            fuel_tab(ui, interface);
        }
    }
    ui.spacing();
    ui.separator();
    ui.text_wrapped("This is the flight used when you request IFR clearance. Review the airports, runways and route before transmitting.");
    let _ = state;
}

/// Load airport data for an ICAO through the engine.
fn load_airport(interface: &mut Interface, icao: &str) {
    icao.clone_into(&mut interface.airport_code);
    interface.loading_airport = true;
    "Loading airport data...".clone_into(&mut interface.airport_notice);
    interface.engine.post(
        "/airport/load",
        serde_json::json!({"icao": icao,"active":false}),
    );
}

/// Labelled text field inside a 3-column form table.
fn form_text(ui: &imgui::Ui, label: &str, value: &mut String, capacity: usize) {
    ui.table_next_column();
    ui.text_disabled(label);
    ui.set_next_item_width(-1.0);
    widgets::edit_text(ui, &format!("##{label}"), value, capacity);
}

/// Labelled integer field inside a form table.
fn form_integer(ui: &imgui::Ui, label: &str, value: &mut i32, step: i32) {
    ui.table_next_column();
    ui.text_disabled(label);
    ui.set_next_item_width(-1.0);
    ui.input_int(&format!("##{label}"), value)
        .step(step)
        .build();
}

/// Labelled float field inside a form table. Rounds through f32 like the
/// widget does; dispatch figures carry no fractional kilos.
fn form_number(ui: &imgui::Ui, label: &str, value: &mut f64) {
    ui.table_next_column();
    ui.text_disabled(label);
    ui.set_next_item_width(-1.0);
    // f32 holds whole kilos exactly well past takeoff weights.
    #[allow(clippy::cast_possible_truncation)]
    let mut shown = *value as f32;
    let before = shown;
    ui.input_float(&format!("##{label}"), &mut shown)
        .display_format("%.0f")
        .build();
    if (shown - before).abs() > f32::EPSILON {
        *value = f64::from(shown);
    }
}

/// Altitude field honouring the metric toggle.
fn form_altitude(ui: &imgui::Ui, label: &str, feet: &mut i32, metric: bool) {
    ui.table_next_column();
    let title = format!("{label}{}", if metric { " (m)" } else { " (ft)" });
    ui.text_disabled(&title);
    ui.set_next_item_width(-1.0);
    let mut display = if metric {
        // Round airport elevation to tens of meters.
        #[allow(clippy::cast_possible_truncation)]
        let meters = (openatc_core::feet_to_meters(f64::from(*feet)) / 10.0).round() as i32 * 10;
        meters
    } else {
        *feet
    };
    let before = display;
    ui.input_int(&format!("##{title}"), &mut display)
        .step(if metric { 10 } else { 1000 })
        .build();
    if display != before {
        *feet = if metric {
            openatc_core::meters_to_feet(display)
        } else {
            display
        };
    }
}

/// Taxi page: airport loaders, layer toggles, arrival stand picker and the map.
pub fn taxi_page(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    let selected = taxi_airport(interface, state).to_owned();
    if !selected.is_empty()
        && interface.taxi_requested != selected
        && !interface.loading_airport
        && interface.engine.connected()
        && !interface.settings.simulator_root.is_empty()
    {
        interface.taxi_requested.clone_from(&selected);
        load_airport(interface, &selected);
    }
    ui.text_colored(
        widgets::ACCENT,
        format!(
            "{}  /  {}  {}",
            if interface.taxi_arrival {
                "ARRIVAL"
            } else {
                "DEPARTURE"
            },
            if selected.is_empty() {
                "Current airport"
            } else {
                &selected
            },
            if interface.airport.icao == selected {
                &interface.airport.name
            } else {
                ""
            }
        ),
    );
    let load_disabled = ui.begin_disabled(interface.loading_airport);
    if ui.button(if interface.taxi_arrival {
        "Departure airport"
    } else {
        "Departure airport •"
    }) {
        interface.taxi_arrival = false;
        interface.taxi_use_current = false;
        interface.taxi_requested.clear();
    }
    ui.same_line();
    if ui.button(if interface.taxi_arrival {
        "Arrival airport •"
    } else {
        "Arrival airport"
    }) {
        interface.taxi_arrival = true;
        interface.taxi_requested.clear();
    }
    drop(load_disabled);
    ui.same_line();
    if ui.button("Fit map") {
        interface.zoom = 1.0;
        interface.pan = [0.0, 0.0];
    }
    ui.text_wrapped(&interface.airport_notice);
    taxi_layers(ui, interface);
    if !selected.is_empty() && interface.airport.icao != selected {
        ui.text_disabled("The selected airport map is not loaded yet.");
        return;
    }
    ui.separator();
    if ui.checkbox(
        "Ground arrows",
        &mut interface.settings.show_ground_taxi_arrows,
    ) {
        interface.save_settings();
    }
    if !interface.settings.show_ground_taxi_arrows {
        ui.same_line();
        ui.text_disabled("Simulator ground guidance is off");
    }
    if interface.taxi_arrival {
        arrival_parking(ui, interface, state);
    }
    if state.taxi_clearance.approved && state.taxi_clearance.airport == interface.airport.icao {
        ui.text_wrapped(&state.taxi_clearance.instructions);
    } else {
        ui.text_disabled("No approved taxi route for this airport. Request taxi on the ATC page.");
    }
    crate::maps::airport_canvas(ui, interface, state, false, 0.0);
}

/// Initial selection uses the actual simulator airport; explicit selectors use the plan.
fn taxi_airport<'a>(interface: &'a Interface, state: &'a State) -> &'a str {
    if interface.taxi_arrival {
        &state.plan.destination
    } else if interface.taxi_use_current && !interface.current_airport.is_empty() {
        &interface.current_airport
    } else if !state.plan.departure.is_empty() {
        &state.plan.departure
    } else {
        &interface.current_airport
    }
}

/// Wrap map layers to the available panel width.
fn taxi_layers(ui: &imgui::Ui, interface: &mut Interface) {
    let right = ui.cursor_screen_pos()[0] + ui.content_region_avail()[0];
    for (index, (label, value)) in [
        ("Taxiways", &mut interface.settings.show_taxiways),
        ("Parking", &mut interface.settings.show_parking),
        ("Approved route", &mut interface.settings.show_taxi_route),
        ("Aircraft", &mut interface.settings.show_ownship),
        ("Labels", &mut interface.settings.show_labels),
    ]
    .into_iter()
    .enumerate()
    {
        if index > 0 && ui.item_rect_max()[0] + ui.calc_text_size(label)[0] + 48.0 < right {
            ui.same_line();
        }
        ui.checkbox(label, value);
    }
}

/// Select the arrival stand from the displayed airport.
fn arrival_parking(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    ui.set_next_item_width(260.0);
    if let Some(_parking) = ui.begin_combo(
        "Arrival parking",
        if state.plan.arrival_stand.is_empty() {
            "Select a stand"
        } else {
            &state.plan.arrival_stand
        },
    ) {
        for parking in interface.airport.parking.clone() {
            if ui.selectable(&parking.name) {
                interface.draft.arrival_stand.clone_from(&parking.name);
                interface
                    .engine
                    .post("/plan/parking", serde_json::json!({"stand": parking.name}));
            }
        }
    }
}

/// Destination-anchored arrival profile with live terrain and descent projection.
pub fn arrival_page(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    crate::arrival::draw(ui, interface, state);
}

/// Airport browser: loader, overview canvas, frequencies, navaids, runways
/// and procedures tabs.
pub fn airports_page(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    ui.set_next_item_width(100.0);
    widgets::edit_text(ui, "ICAO", &mut interface.airport_code, 8);
    ui.same_line();
    let load_disabled = ui.begin_disabled(interface.loading_airport);
    if ui.button(if interface.loading_airport {
        "Loading..."
    } else {
        "Load airport"
    }) {
        load_airport(interface, &interface.airport_code.clone());
    }
    drop(load_disabled);
    let units = resolve_units(&units_name(interface), &interface.airport.icao);
    ui.text_colored(
        widgets::ACCENT,
        format!(
            "{}   {}   /   {}",
            interface.airport.icao,
            interface.airport.name,
            openatc_core::altitude_text(interface.airport.elevation_feet, units, false)
        ),
    );
    ui.text_wrapped(&interface.airport_notice);
    if let Some(_tabs) = ui.tab_bar("AirportTabs") {
        if let Some(_tab_item) = ui.tab_item("Overview") {
            overview_tab(ui, interface, state);
        }
        if let Some(_tab_item) = ui.tab_item("Frequencies") {
            frequencies_tab(ui, interface);
        }
        if let Some(_tab_item) = ui.tab_item("ILS / navaids") {
            navaids_tab(ui, interface);
        }
        if let Some(_tab_item) = ui.tab_item("Runways & stands") {
            runways_tab(ui, interface);
        }
        if let Some(_tab_item) = ui.tab_item("Procedures") {
            airport_procedures_tab(ui, interface);
        }
    }
}
/// Overview tab: layer toggles and the map canvas.
fn overview_tab(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    let right = ui.cursor_screen_pos()[0] + ui.content_region_avail()[0];
    for (index, (label, value)) in [
        ("3D orbit", &mut interface.orbit_view),
        ("ILS / glideslope", &mut interface.settings.show_ils_beams),
        ("Runways", &mut interface.show_runways),
        ("Taxiways", &mut interface.settings.show_taxiways),
        ("Parking", &mut interface.settings.show_parking),
        ("Navaids", &mut interface.show_navaids),
        ("Labels", &mut interface.settings.show_labels),
    ]
    .into_iter()
    .enumerate()
    {
        if index > 0 && ui.item_rect_max()[0] + ui.calc_text_size(label)[0] + 48.0 < right {
            ui.same_line();
        }
        ui.checkbox(label, value);
    }
    if interface.airport.icao == "DEMO" {
        ui.checkbox("Demo buildings", &mut interface.show_buildings);
    }
    if interface.settings.show_ils_beams {
        ui.text_wrapped("Published ILS courses / glide angle / 4 NM approach illustration");
    }
    crate::maps::airport_canvas(ui, interface, state, true, 0.0);
}

/// Frequencies tab: the loaded airport stations.
fn frequencies_tab(ui: &imgui::Ui, interface: &mut Interface) {
    if let Some(_table) = ui.begin_table_with_flags(
        "Frequencies",
        3,
        imgui::TableFlags::ROW_BG
            | imgui::TableFlags::SORTABLE
            | imgui::TableFlags::BORDERS_INNER_H
            | imgui::TableFlags::SIZING_STRETCH_PROP,
    ) {
        ui.table_setup_column("Service");
        ui.table_setup_column("Station");
        ui.table_setup_column("MHz");
        ui.table_headers_row();
        let mut rows = interface.airport.frequencies.clone();
        sort_table(ui, &mut rows, |r, column| match column {
            0 => r.service.clone(),
            1 => r.name.clone(),
            _ => format!("{:09}", r.khz),
        });
        for frequency in rows {
            ui.table_next_row();
            ui.table_next_column();
            ui.text(&frequency.service);
            ui.table_next_column();
            ui.text_wrapped(&frequency.name);
            ui.table_next_column();
            ui.text(format!("{:.3}", f64::from(frequency.khz) / 1000.0));
        }
    }
    if interface.airport.frequencies.is_empty() {
        ui.text_wrapped("No communications frequencies in this airport's scenery data.");
    }
}

/// Navaids tab: ILS, VOR, NDB and DME near the airport.
fn navaids_tab(ui: &imgui::Ui, interface: &mut Interface) {
    if let Some(_table) = ui.begin_table_with_flags(
        "Navaids",
        5,
        imgui::TableFlags::ROW_BG
            | imgui::TableFlags::SORTABLE
            | imgui::TableFlags::BORDERS_INNER_H
            | imgui::TableFlags::SIZING_STRETCH_PROP,
    ) {
        ui.table_setup_column("Type");
        ui.table_setup_column("ID");
        ui.table_setup_column("Frequency");
        ui.table_setup_column("Runway");
        ui.table_setup_column("Details");
        ui.table_headers_row();
        let mut rows = interface.airport.navaids.clone();
        sort_table(ui, &mut rows, |r, column| match column {
            0 => r.kind.clone(),
            1 => r.identifier.clone(),
            2 => format!("{:020.6}", r.frequency),
            3 => r.runway.clone(),
            _ => r.name.clone(),
        });
        for navaid in rows {
            ui.table_next_row();
            ui.table_next_column();
            ui.text(&navaid.kind);
            ui.table_next_column();
            ui.text(&navaid.identifier);
            ui.table_next_column();
            if navaid.frequency > 0.0 {
                ui.text(if navaid.kind == "NDB" {
                    format!("{:.0} kHz", navaid.frequency)
                } else {
                    format!("{:.2} MHz", navaid.frequency)
                });
            } else {
                ui.text("-");
            }
            ui.table_next_column();
            ui.text(&navaid.runway);
            ui.table_next_column();
            ui.text_wrapped(&navaid.name);
            if navaid.kind == "LOC" || navaid.kind == "ILS LOC" || navaid.kind == "GS" {
                ui.text(format!("Course {:.1} true", navaid.bearing));
            }
            if navaid.glide_angle > 0.0 {
                ui.text(format!("Glide angle {:.2}", navaid.glide_angle));
            }
        }
    }
}

/// Runways tab: runway dimensions and parking stands.
fn runways_tab(ui: &imgui::Ui, interface: &mut Interface) {
    for runway in interface.airport.runways.clone() {
        ui.text(format!(
            "{} / {}   {:.0} x {:.0} m",
            runway.first_name,
            runway.second_name,
            (runway.second.east - runway.first.east)
                .hypot(runway.second.north - runway.first.north),
            runway.width
        ));
    }
    widgets::section_title(ui, "PARKING LOCATIONS");
    if let Some(_table) = ui.begin_table_with_flags(
        "Stands",
        4,
        imgui::TableFlags::ROW_BG
            | imgui::TableFlags::SORTABLE
            | imgui::TableFlags::BORDERS_INNER_H
            | imgui::TableFlags::SIZING_STRETCH_PROP,
    ) {
        ui.table_setup_column("Stand");
        ui.table_setup_column("Type");
        ui.table_setup_column("Size");
        ui.table_setup_column("Equipment");
        ui.table_headers_row();
        let mut rows = interface.airport.parking.clone();
        sort_table(ui, &mut rows, |r, column| match column {
            0 => r.name.clone(),
            1 => r.kind.clone(),
            2 => r.size.clone(),
            _ => r.equipment.clone(),
        });
        for parking in rows {
            ui.table_next_row();
            ui.table_next_column();
            ui.text_wrapped(&parking.name);
            ui.table_next_column();
            ui.text(&parking.kind);
            ui.table_next_column();
            ui.text(&parking.size);
            ui.table_next_column();
            ui.text(&parking.equipment);
        }
    }
}

/// Procedures tab: loaded SID/STAR/approach headers.
fn airport_procedures_tab(ui: &imgui::Ui, interface: &mut Interface) {
    if let Some(_table) = ui.begin_table_with_flags(
        "AirportProcedures",
        3,
        imgui::TableFlags::ROW_BG
            | imgui::TableFlags::SORTABLE
            | imgui::TableFlags::BORDERS_INNER_H
            | imgui::TableFlags::SIZING_STRETCH_PROP,
    ) {
        ui.table_setup_column("Type");
        ui.table_setup_column("Procedure");
        ui.table_setup_column("Transition");
        ui.table_headers_row();
        let mut rows = interface.airport.procedures.clone();
        sort_table(ui, &mut rows, |r, column| match column {
            0 => r.kind.clone(),
            1 => r.name.clone(),
            _ => r.transition.clone(),
        });
        for procedure in rows {
            ui.table_next_row();
            ui.table_next_column();
            ui.text(&procedure.kind);
            ui.table_next_column();
            ui.text(&procedure.name);
            ui.table_next_column();
            ui.text(&procedure.transition);
        }
    }
}

/// Settings: apply button plus the eight configuration tabs.
pub fn settings_page(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    if interface.show_voice_popup {
        voice_popup(ui, interface);
        return;
    }
    if ui.button("Apply settings") {
        interface.save_settings();
    }
    ui.same_line();
    ui.text_disabled(if interface.settings_notice.is_empty() {
        "Changes also save automatically"
    } else {
        &interface.settings_notice
    });
    if let Some(_tabs) = ui.tab_bar("SettingsTabs") {
        if let Some(_tab_item) = ui.tab_item("General") {
            settings_general(ui, interface);
        }
        if let Some(_tab_item) = ui.tab_item("Copilot") {
            widgets::section_title(ui, "RADIO ASSISTANCE");
            ui.checkbox(
                "Copilot reads back clearances",
                &mut interface.settings.copilot_replies,
            );
            ui.checkbox(
                "Copilot answers routine ATC calls",
                &mut interface.settings.copilot_auto_respond,
            );
            ui.checkbox(
                "Copilot tunes assigned COM1 frequency",
                &mut interface.settings.copilot_tunes,
            );
            ui.checkbox(
                "Copilot retunes on ATC handoffs",
                &mut interface.settings.auto_tune_handoff,
            );
            ui.text_wrapped("Readbacks use the recorded clearance sequence, altitude and route. Frequency tuning uses structured OpenATC AI assignments and runs in the X-Plane plugin. It does not change your barometer or autopilot.");
            widgets::section_title(ui, "AUTOPILOT TARGETS");
            ui.checkbox(
                "Copilot sets altitude preselect",
                &mut interface.settings.copilot_flies_ap,
            );
            ui.text_wrapped("With the autopilot engaged, the copilot sets the cleared altitude and announces it. With it off, or when the dials are unreachable, the copilot advises instead. It never engages the autopilot itself. Heading and speed follow once their instruction sources exist.");
            ui.text_wrapped("These options apply to OpenATC AI's controller. They do not intercept X-Plane's built-in ATC, Next ATC or VATSIM.");
        }
        if let Some(_tab_item) = ui.tab_item("AI") {
            widgets::section_title(ui, "INTENT CLASSIFICATION");
            ui.checkbox(
                "Use AI for free-form requests",
                &mut interface.settings.ai_enabled,
            );
            ui.checkbox(
                "Allow AI wording variety",
                &mut interface.settings.llm_phrase_variety,
            );
            ui.text_wrapped(
                "Keeps clearance values fixed; invalid wording falls back to the template.",
            );
            setting_input(ui, "AI base URL", &mut interface.settings.ai_url, 256);
            setting_input(ui, "AI model", &mut interface.settings.ai_model, 128);
            ui.text_wrapped("OpenAI-compatible /v1/chat/completions. Enter a base origin, such as http://127.0.0.1:11434. The engine also supports HTTPS services.");
            ui.text_wrapped("The model interprets requests; flight-stage rules and recorded clearances remain in the controller. Set OPENATC_AI_KEY in the engine environment if your provider requires a key.");
        }
        if let Some(_tab_item) = ui.tab_item("STT / TTS") {
            widgets::section_title(ui, "SPEECH TO TEXT");
            setting_input(ui, "STT base URL", &mut interface.settings.stt_url, 256);
            setting_input(ui, "STT model", &mut interface.settings.stt_model, 128);
            widgets::section_title(ui, "TEXT TO SPEECH");
            setting_input(ui, "TTS base URL", &mut interface.settings.tts_url, 256);
            setting_input(ui, "TTS model", &mut interface.settings.tts_model, 128);
            ui.text_wrapped("Services use /v1/audio/transcriptions and /v1/audio/speech. Audio requests go through the local engine, which supports HTTP and HTTPS. Set OPENATC_STT_KEY and OPENATC_TTS_KEY on the engine if required. Keys are not saved in project settings.");
            widgets::section_title(ui, "DEVELOPER");
            ui.checkbox(
                "Developer mode (verbose errors, error copy)",
                &mut interface.settings.dev_mode,
            );
            ui.text_wrapped("Dev on: status errors carry endpoint and key context, failures auto-log to Log.txt, and openatc/copy_error dumps the current error on demand.");
        }
        if let Some(_tab_item) = ui.tab_item("Voices") {
            voices_tab(ui, interface);
        }
        if let Some(_tab_item) = ui.tab_item("Audio") {
            audio_tab(ui, interface);
        }
        if let Some(_tab_item) = ui.tab_item("Realism") {
            realism_tab(ui, interface);
        }
        if let Some(_tab_item) = ui.tab_item("Session") {
            session_tab(ui, interface, state);
        }
    }
}

/// General tab: accounts, interface, units, crew calls.
fn settings_general(ui: &imgui::Ui, interface: &mut Interface) {
    widgets::section_title(ui, "ACCOUNTS & INSTALLED DATA");
    setting_input(
        ui,
        "SimBrief Pilot ID",
        &mut interface.settings.simbrief_id,
        32,
    );
    ui.text_wrapped("Use the numeric Pilot ID from SimBrief account settings. Downloads your latest generated flight; no Navigraph login is needed for this import.");
    setting_input(
        ui,
        "X-Plane installation folder",
        &mut interface.settings.simulator_root,
        1024,
    );
    ui.text_wrapped("The plugin detects this folder automatically. Airport data uses enabled scenery first, then Global Airports. Navaids and procedures use Custom Data when installed.");
    widgets::section_title(ui, "NAVIGRAPH CHARTS");
    ui.text_wrapped("Charts are not connected. A chart integration requires Navigraph-issued application credentials and a user sign-in with an eligible subscription. Installed X-Plane navdata and the SimBrief download work independently.");
    widgets::section_title(ui, "INTERFACE");
    ui.slider("Text scale", 0.85, 1.5, &mut interface.settings.ui_scale);
    ui.checkbox("Keep ATC open", &mut interface.settings.pin_open);
    ui.slider("Fade delay", 1.0, 30.0, &mut interface.settings.fade_delay);
    ui.slider(
        "Faded background",
        0.1,
        0.85,
        &mut interface.settings.faded_opacity,
    );
    widgets::section_title(ui, "UNITS");
    units_combo(ui, interface);
    ui.text_wrapped("Imperial is the aviation standard: feet, knots, nautical miles. Metric uses meters, km/h, kilometers. Region follows local procedure for the departure country. Weights stay in kilograms; pressure and visibility follow regional procedure.");
    widgets::section_title(ui, "CREW CALLS");
    setting_input(
        ui,
        "Attendant call dataref",
        &mut interface.settings.attendant_ref,
        256,
    );
    setting_input(
        ui,
        "Ground crew call dataref",
        &mut interface.settings.ground_ref,
        256,
    );
    ui.checkbox(
        "Show copilot talk button",
        &mut interface.settings.copilot_button,
    );
    ui.text_wrapped("Point these at the call LIGHT datarefs from your aircraft (DataRefTool search: attendant, cabin call, ground crew) — not the momentary pushbuttons. While a light is lit, Transmit routes there: ground call wins, then attendant, else ATC. Empty fields mean every transmission goes to ATC. The Copilot button addresses the copilot directly. Bind openatc/talk and openatc/talk_copilot in X-Plane keyboard/joystick settings to transmit with the window closed.");
}

/// Labelled settings text field.
fn setting_input(ui: &imgui::Ui, title: &str, value: &mut String, capacity: usize) {
    ui.text_disabled(title);
    ui.set_next_item_width(-1.0);
    widgets::edit_text(ui, &format!("##{title}"), value, capacity);
}

/// Labelled float slider.
fn labeled_slider(
    ui: &imgui::Ui,
    title: &str,
    value: &mut f32,
    minimum: f32,
    maximum: f32,
    format: &str,
) {
    ui.text_disabled(title);
    ui.set_next_item_width(-1.0);
    ui.slider_config(title, minimum, maximum)
        .display_format(format)
        .build(value);
}

/// Units combo box.
fn units_combo(ui: &imgui::Ui, interface: &mut Interface) {
    use openatc_settings::Units;
    let preview = match interface.settings.units {
        Units::Metric => "Metric",
        Units::Region => "Region",
        Units::Imperial => "Imperial",
    };
    ui.text_disabled("Units");
    ui.set_next_item_width(-1.0);
    if let Some(_combo) = ui.begin_combo("##Units", preview) {
        for option in ["Imperial", "Metric", "Region"] {
            let units = match option {
                "Metric" => Units::Metric,
                "Region" => Units::Region,
                _ => Units::Imperial,
            };
            if ui.selectable(option) {
                interface.settings.units = units;
            }
        }
    }
}

/// Delivery combo box shared by controller and copilot rows.
fn delivery_combo(ui: &imgui::Ui, title: &str, value: &mut openatc_settings::Delivery) {
    use openatc_settings::Delivery;
    let preview = match value {
        Delivery::Brisk => "Brisk ATC",
        Delivery::Urgent => "Urgent",
        Delivery::Standard => "Standard",
    };
    ui.text_disabled(title);
    ui.set_next_item_width(-1.0);
    if let Some(_delivery) = ui.begin_combo(format!("##{title}"), preview) {
        for option in ["Standard", "Brisk", "Urgent"] {
            let delivery = match option {
                "Brisk" => Delivery::Brisk,
                "Urgent" => Delivery::Urgent,
                _ => Delivery::Standard,
            };
            if ui.selectable(option) {
                *value = delivery;
            }
        }
    }
}

/// Copilot personality presets.
fn persona_presets() -> [(&'static str, &'static str); 6] {
    [
        ("Classic (fixed)", ""),
        (
            "By-the-book",
            "You are a strictly professional airline copilot. Phraseology only, no extra words, no jokes ever — even when asked, deflect politely to duty. Readbacks are verbatim.",
        ),
        (
            "Veteran dry humor",
            "You are a veteran copilot with dry wit and ten thousand hours. Readbacks stay correct and complete with at most one short dry remark. Jokes on request, never unprompted.",
        ),
        (
            "Humorous",
            "You are a warm, funny copilot who enjoys the cruise. Jokes and banter when the captain invites it; operations always correct and complete; readbacks verbatim with a smile in the voice.",
        ),
        (
            "Super Silly",
            "You are a delightfully silly copilot — playful, theatrical, full of jokes — but ONLY with the captain in the cockpit. Clearances, readbacks and procedures stay strictly verbatim and professional; silliness never touches operations or the radio.",
        ),
        (
            "Nervous student",
            "You are a nervous student pilot who always uses correct phraseology. Readbacks are careful, complete and slightly hesitant. Too nervous to joke; deflects humor with an anxious laugh and returns to duty.",
        ),
    ]
}

/// Voices tab: personality preset, custom text, spoken toggles, voice pool.
fn voices_tab(ui: &imgui::Ui, interface: &mut Interface) {
    persona_voices(ui, interface);
    spoken_voices(ui, interface);
    controller_voices(ui, interface);
    crew_voices(ui, interface);
}

/// Copilot personality picker and custom text.
fn persona_voices(ui: &imgui::Ui, interface: &mut Interface) {
    widgets::section_title(ui, "COPILOT PERSONALITY");
    let presets = persona_presets();
    let preview = presets
        .iter()
        .find(|(_, text)| *text == interface.settings.copilot_personality)
        .map_or("Custom", |(name, _)| *name);
    ui.text_disabled("Personality preset");
    ui.set_next_item_width(-1.0);
    if let Some(_combo) = ui.begin_combo("##CopilotPersona", preview) {
        for (name, text) in presets {
            if ui.selectable(name) {
                text.clone_into(&mut interface.settings.copilot_personality);
            }
        }
    }
    ui.text_wrapped("Editing the text below switches this to Custom.");
    widgets::edit_paragraph(
        ui,
        "Custom personality (AI readbacks + copilot chat)",
        &mut interface.settings.copilot_personality,
        60.0,
        2000,
    );
    ui.text_wrapped("Empty personality uses the classic fixed readback. Otherwise the AI drafts readbacks in this voice when intent classification is on; any failure falls back to the fixed text.");
}

/// Which roles get spoken aloud, with volumes.
fn spoken_voices(ui: &imgui::Ui, interface: &mut Interface) {
    widgets::section_title(ui, "SPOKEN MESSAGES");
    ui.checkbox(
        "Speak controller messages",
        &mut interface.settings.controller_speech,
    );
    ui.checkbox(
        "Speak copilot readbacks",
        &mut interface.settings.copilot_speech,
    );
    labeled_slider(
        ui,
        "Controller volume",
        &mut interface.settings.controller_volume,
        0.0,
        1.0,
        "%.2f",
    );
    labeled_slider(
        ui,
        "Copilot volume",
        &mut interface.settings.copilot_volume,
        0.0,
        1.0,
        "%.2f",
    );
    labeled_slider(
        ui,
        "Pilot volume",
        &mut interface.settings.pilot_volume,
        0.0,
        1.0,
        "%.2f",
    );
}

/// Controller fallback, pool, delivery and speed range.
fn controller_voices(ui: &imgui::Ui, interface: &mut Interface) {
    widgets::section_title(ui, "CONTROLLER VOICES");
    setting_input(
        ui,
        "Fallback controller voice",
        &mut interface.settings.voice,
        128,
    );
    setting_input(
        ui,
        "Voice pool (comma-separated)",
        &mut interface.settings.voice_pool,
        1024,
    );
    if ui.button("Test voices...") {
        interface.show_voice_popup = true;
    }
    ui.text_wrapped("New airspaces draw a voice from the pool, skipping recently used ones, and remember it across restarts. Empty pool uses the fallback voice.");
    delivery_combo(
        ui,
        "Controller delivery",
        &mut interface.settings.controller_delivery,
    );
    ui.checkbox(
        "Randomize delivery per controller",
        &mut interface.settings.randomize_delivery,
    );
    ui.text_disabled("Controller speed range (fixed draw per controller)");
    ui.set_next_item_width(-1.0);
    imgui::DragRange::<f32, _>::new("##controllerrange")
        .speed(0.01)
        .range(0.5, 2.0)
        .display_format("Min: %.2fx")
        .build(
            ui,
            &mut interface.settings.controller_speed_min,
            &mut interface.settings.controller_speed_max,
        );
    if interface.settings.controller_speed_min > interface.settings.controller_speed_max {
        std::mem::swap(
            &mut interface.settings.controller_speed_min,
            &mut interface.settings.controller_speed_max,
        );
    }
}

/// Copilot, pilot and cabin voice rows.
fn crew_voices(ui: &imgui::Ui, interface: &mut Interface) {
    widgets::section_title(ui, "COPILOT VOICE");
    setting_input(
        ui,
        "Copilot voice",
        &mut interface.settings.copilot_voice,
        128,
    );
    labeled_slider(
        ui,
        "Copilot speed",
        &mut interface.settings.copilot_speed,
        0.5,
        2.0,
        "%.2fx",
    );
    delivery_combo(
        ui,
        "Copilot delivery",
        &mut interface.settings.copilot_delivery,
    );
    widgets::section_title(ui, "PILOT (OWN TRANSMISSIONS)");
    ui.checkbox(
        "Speak my transmitted requests",
        &mut interface.settings.pilot_speech,
    );
    setting_input(ui, "Pilot voice", &mut interface.settings.pilot_voice, 128);
    labeled_slider(
        ui,
        "Pilot speed",
        &mut interface.settings.pilot_speed,
        0.5,
        2.0,
        "%.2fx",
    );
    widgets::section_title(ui, "CABIN & GROUND CREW");
    ui.text_wrapped("Cabin intercom and the wired ground interphone are clean audio: radio effects stay off unless you enable them here.");
    ui.checkbox(
        "Radio effects on cabin intercom",
        &mut interface.settings.radio_fx_cabin,
    );
    ui.checkbox(
        "Radio effects on ground interphone",
        &mut interface.settings.radio_fx_ground,
    );
    setting_input(
        ui,
        "Flight attendant voice",
        &mut interface.settings.attendant_voice,
        128,
    );
}

/// Voice pool popup: play and remove pooled voices.
fn voice_popup(ui: &imgui::Ui, interface: &mut Interface) {
    widgets::section_title(ui, "TEST VOICES");
    ui.child_window("Test voices").size([0.0, 0.0]).build(|| {
        let pool = openatc_core::ops::parse_voice_pool(&interface.settings.voice_pool);
        if pool.is_empty() {
            ui.text_disabled("The voice pool is empty. Add comma-separated voice names first.");
        }
        let mut remove = None;
        for (index, voice) in pool.iter().enumerate() {
            let _id = ui.push_id(index.to_string());
            ui.text(voice);
            ui.same_line_with_pos(260.0);
            if ui.button("Play") {
                interface.speak_entry(&openatc_core::state::Transmission {
                    pilot_reply: String::new(),
                    background: false,
                    speaker: "ATC".to_owned(),
                    text: "OpenATC radio check. Reading you five.".to_owned(),
                    sequence: 0,
                    position: String::new(),
                    voice: voice.clone(),
                    delivery: "standard".to_owned(),
                    speed: 1.0,
                    urgent: false,
                });
            }
            ui.same_line();
            if ui.button("Remove") {
                remove = Some(index);
            }
        }
        if let Some(index) = remove {
            let mut pool = pool;
            pool.remove(index);
            interface.settings.voice_pool = pool.join(", ");
        }
        ui.separator();
        ui.text_disabled(interface.speech.status());
        if ui.button_with_size("Close", [120.0, 0.0]) {
            interface.show_voice_popup = false;
        }
    });
}

/// Audio device picker with the system default on top.
fn device_picker(
    ui: &imgui::Ui,
    title: &str,
    selected: &mut String,
    devices: &[openatc_audio::AudioDevice],
) {
    let label = if selected.is_empty() {
        "System default".to_owned()
    } else {
        devices
            .iter()
            .find(|device| device.key == *selected)
            .map_or_else(|| selected.clone(), |device| device.name.clone())
    };
    ui.set_next_item_width(-1.0);
    ui.text_disabled(title);
    if let Some(_combo) = ui.begin_combo(format!("##{title}"), &label) {
        for (index, device) in devices.iter().enumerate() {
            // Integer IDs: this imgui build dereferences empty string IDs.
            let _id = ui.push_id_int(i32::try_from(index).unwrap_or(0));
            if ui.selectable(&device.name) {
                device.key.clone_into(selected);
            }
        }
    }
}

/// Audio tab: devices, mic test, TTS test, radio effects.
fn audio_tab(ui: &imgui::Ui, interface: &mut Interface) {
    audio_devices_row(ui, interface);
    audio_levels_row(ui, interface);
    audio_effects_row(ui, interface);
}

/// Device pickers and test buttons.
fn audio_devices_row(ui: &imgui::Ui, interface: &mut Interface) {
    widgets::section_title(ui, "AUDIO DEVICES");
    let audio_busy =
        interface.speech.busy() || interface.speech.recording() || interface.speech.monitoring();
    let _devices_disabled = ui.begin_disabled(audio_busy);
    if ui.button("Refresh audio devices") {
        interface.speech.refresh_devices();
    }
    let inputs = interface.speech.input_devices();
    let outputs = interface.speech.output_devices();
    device_picker(
        ui,
        "Microphone",
        &mut interface.settings.input_device,
        &inputs,
    );
    device_picker(
        ui,
        "Output device",
        &mut interface.settings.output_device,
        &outputs,
    );
}

/// Master volume, input gain and the mic meter.
fn audio_levels_row(ui: &imgui::Ui, interface: &mut Interface) {
    widgets::section_title(ui, "LEVELS");
    labeled_slider(
        ui,
        "Master volume",
        &mut interface.settings.master_volume,
        0.0,
        1.0,
        "%.2f",
    );
    labeled_slider(
        ui,
        "Input gain",
        &mut interface.settings.input_gain,
        0.0,
        4.0,
        "%.2fx",
    );
    let _monitor_disabled =
        ui.begin_disabled(interface.speech.busy() || interface.speech.recording());
    if ui.button(if interface.speech.monitoring() {
        "Stop microphone test"
    } else {
        "Test microphone"
    }) {
        if interface.speech.monitoring() {
            interface.speech.stop_monitoring();
        } else {
            interface.speech.start_recording(true);
        }
    }
    ui.same_line();
    let _test_disabled = ui.begin_disabled(interface.speech.monitoring());
    if ui.button("Test controller voice") {
        interface.speak_entry(&openatc_core::state::Transmission {
            pilot_reply: String::new(),
            background: false,
            speaker: "ATC".to_owned(),
            text: "OpenATC AI radio check. Reading you five.".to_owned(),
            sequence: 0,
            position: String::new(),
            voice: String::new(),
            delivery: "standard".to_owned(),
            speed: 1.0,
            urgent: false,
        });
    }
    let level = interface.speech.input_level();
    let clipping = interface.speech.clipping();
    let _plot = ui.push_style_color(
        imgui::StyleColor::PlotHistogram,
        if clipping {
            [0.95, 0.3, 0.3, 1.0]
        } else {
            [0.25, 0.8, 0.62, 1.0]
        },
    );
    imgui::ProgressBar::new(level)
        .size([-1.0, 26.0])
        .overlay_text(
            if interface.speech.monitoring() || interface.speech.recording() {
                format!(
                    "{:.1} dBFS{}",
                    interface.speech.input_db(),
                    if clipping { "  CLIPPING" } else { "" }
                )
            } else {
                "Microphone off".to_owned()
            },
        )
        .build(ui);
    ui.text_disabled("-60 dBFS                                            -30                                            0");
    ui.text_wrapped("RMS input level after gain. The test stays on this computer and sends no audio. Recording on the ATC page sends audio for transcription only when you select Stop + transcribe.");
    ui.text_wrapped(interface.speech.status());
}

/// Radio effect toggles and levels.
fn audio_effects_row(ui: &imgui::Ui, interface: &mut Interface) {
    widgets::section_title(ui, "RADIO EFFECTS");
    ui.checkbox(
        "Radio hiss, crackle and static",
        &mut interface.settings.radio_effects,
    );
    ui.checkbox(
        "Telephone bandpass (300-3400 Hz)",
        &mut interface.settings.radio_bandpass,
    );
    labeled_slider(
        ui,
        "Hiss",
        &mut interface.settings.radio_hiss,
        0.0,
        1.0,
        "%.2f",
    );
    labeled_slider(
        ui,
        "Crackle",
        &mut interface.settings.radio_crackle,
        0.0,
        1.0,
        "%.2f",
    );
    labeled_slider(
        ui,
        "Static bursts",
        &mut interface.settings.radio_static,
        0.0,
        1.0,
        "%.2f",
    );
    ui.text_wrapped("Effects are baked into every spoken message by the speech service. Check them with Test controller voice above.");
}

/// Realism tab: discipline preset plus the individual toggles.
fn realism_tab(ui: &imgui::Ui, interface: &mut Interface) {
    discipline_preset_row(ui, interface);
    readbacks_row(ui, interface);
    phraseology_row(ui, interface);
    congestion_row(ui, interface);
    emergencies_row(ui, interface);
    widgets::section_title(ui, "TAXI GUIDANCE");
    ui.checkbox(
        "Show taxi arrows in plugin map",
        &mut interface.settings.show_taxi_arrows,
    );
    ui.checkbox(
        "Show illuminated taxi arrows on simulator ground",
        &mut interface.settings.show_ground_taxi_arrows,
    );
    ui.text_wrapped("Guidance follows the approved taxi route and stops at the clearance limit. Ground arrows are available in X-Plane while on the ground.");
}

/// Discipline preset picker.
fn discipline_preset_row(ui: &imgui::Ui, interface: &mut Interface) {
    widgets::section_title(ui, "DISCIPLINE PRESET");
    let preset = openatc_core::intents::discipline_preset(
        openatc_core::intents::Realism {
            strict_readbacks: interface.settings.strict_readbacks,
            require_frequency: interface.settings.require_frequency,
            require_callsign: interface.settings.require_callsign,
            strict_phraseology: interface.settings.strict_phraseology,
            teaching_corrections: interface.settings.teaching_corrections,
            practice_emergencies: interface.settings.practice_emergencies,
        },
        match interface.settings.congestion {
            openatc_settings::Congestion::Busy => "busy",
            openatc_settings::Congestion::Off => "off",
            openatc_settings::Congestion::Quiet => "quiet",
        },
    );
    let preview = match preset.as_str() {
        "relaxed" => "Relaxed",
        "real" => "Real",
        "standard" => "Standard",
        _ => "Custom",
    };
    ui.text_disabled("Discipline preset");
    ui.set_next_item_width(-1.0);
    if let Some(_combo) = ui.begin_combo("##Discipline", preview) {
        if ui.selectable("Relaxed") {
            let settings = &mut interface.settings;
            settings.strict_readbacks = false;
            settings.require_frequency = false;
            settings.require_callsign = false;
            settings.strict_phraseology = false;
            settings.teaching_corrections = false;
            settings.congestion = openatc_settings::Congestion::Off;
            settings.practice_emergencies = false;
        }
        if ui.selectable("Standard") {
            let settings = &mut interface.settings;
            settings.strict_readbacks = true;
            settings.require_frequency = true;
            settings.require_callsign = false;
            settings.strict_phraseology = true;
            settings.teaching_corrections = true;
            settings.congestion = openatc_settings::Congestion::Quiet;
            settings.practice_emergencies = false;
        }
        if ui.selectable("Real") {
            let settings = &mut interface.settings;
            settings.strict_readbacks = true;
            settings.require_frequency = true;
            settings.require_callsign = true;
            settings.strict_phraseology = true;
            settings.teaching_corrections = false;
            settings.congestion = openatc_settings::Congestion::Busy;
            settings.practice_emergencies = true;
        }
    }
    ui.text_wrapped("Relaxed accepts casual phrasing and never corrects. Standard is the default: disciplined readbacks and corrections with explanations. Real enforces full procedures with no hints. Editing any toggle below switches this to Custom.");
}

/// Readback and radio toggles.
fn readbacks_row(ui: &imgui::Ui, interface: &mut Interface) {
    widgets::section_title(ui, "READBACKS & RADIO");
    ui.checkbox(
        "Verbatim readbacks required",
        &mut interface.settings.strict_readbacks,
    );
    ui.checkbox(
        "Callsign required in requests",
        &mut interface.settings.require_callsign,
    );
    ui.text_wrapped("Radio ATC always requires a powered COM1 tuned to a published station in reception range. Button requests may omit the callsign when they match a catalogue title.");
}

/// Phraseology toggles.
fn phraseology_row(ui: &imgui::Ui, interface: &mut Interface) {
    widgets::section_title(ui, "PHRASEOLOGY");
    ui.checkbox(
        "Correct nonstandard phrasing",
        &mut interface.settings.strict_phraseology,
    );
    ui.checkbox(
        "Explain corrections (student mode)",
        &mut interface.settings.teaching_corrections,
    );
}

/// Congestion level picker.
fn congestion_row(ui: &imgui::Ui, interface: &mut Interface) {
    widgets::section_title(ui, "FREQUENCY CONGESTION");
    ui.text_disabled("Congestion level");
    ui.set_next_item_width(-1.0);
    if let Some(_congestion) = ui.begin_combo(
        "##congestion",
        match interface.settings.congestion {
            openatc_settings::Congestion::Busy => "Busy",
            openatc_settings::Congestion::Off => "Off",
            openatc_settings::Congestion::Quiet => "Quiet",
        },
    ) {
        for option in ["Off", "Quiet", "Busy"] {
            if ui.selectable(option) {
                interface.settings.congestion = match option {
                    "Busy" => openatc_settings::Congestion::Busy,
                    "Off" => openatc_settings::Congestion::Off,
                    _ => openatc_settings::Congestion::Quiet,
                };
            }
        }
    }
    ui.text_wrapped("Quiet adds short reply delays. Busy adds longer delays, standby calls and background chatter from other aircraft on the frequency.");
}

/// Emergency practice toggle.
fn emergencies_row(ui: &imgui::Ui, interface: &mut Interface) {
    widgets::section_title(ui, "EMERGENCIES");
    ui.checkbox(
        "Practice emergency flows",
        &mut interface.settings.practice_emergencies,
    );
    ui.text_wrapped("Enables mayday practice ('mayday', 'pan pan'). Off by default; emergency requests are refused with a pointer here.");
}

/// Session tab: demo controls, flight state and the reset modal.
fn session_tab(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    widgets::section_title(ui, "DEMO CONTROLS");
    let _demo_disabled = ui.begin_disabled(!state.demo);
    if ui.button("Airborne demo") {
        interface.engine.post(
            "/demo",
            serde_json::json!({"telemetry": {
                "onGround": false, "altitudeFeet": 32000.0, "heightAglFeet": 30000.0,
                "groundSpeedKnots": 440.0, "latitude": -40.4, "longitude": 145.8,
                "headingDegrees": 320.0, "positionValid": true,
            }, "phase": 4}),
        );
    }
    ui.same_line();
    if ui.button("Ground demo") {
        interface.engine.post(
            "/demo",
            serde_json::json!({"telemetry": {
                "onGround": true, "altitudeFeet": 0.0, "heightAglFeet": 0.0,
                "groundSpeedKnots": 0.0, "latitude": 0.0, "longitude": 0.0,
                "headingDegrees": 0.0, "positionValid": false,
            }, "phase": 0}),
        );
    }
    if ui.button("Load synthetic airport") {
        interface.loading_airport = true;
        interface.engine.post(
            "/airport/load",
            serde_json::json!({"icao": "DEMO", "demo": true}),
        );
    }
    widgets::section_title(ui, "ACTIVE FLIGHT");
    ui.text(format!(
        "Stage: {}",
        openatc_core::ops::phase_name(state.phase)
    ));
    if ui.button("Reset flight...") {
        interface.confirm_reset = true;
    }
    if interface.confirm_reset {
        ui.text_wrapped("Clear the plan, clearances and transcript for this flight?");
        if ui.button("Reset") {
            interface
                .engine
                .post("/session/reset", serde_json::Value::Null);
            interface.confirm_reset = false;
        }
        ui.same_line();
        if ui.button("Cancel") {
            interface.confirm_reset = false;
        }
    }
    ui.text_wrapped("The controller remembers stages while the engine is running. Reset starts a new flight; live telemetry will identify an airborne start.");
}

/// Sidebar page switcher shared by desktop and plugin hosts.
pub fn page_buttons(ui: &imgui::Ui, interface: &mut Interface) {
    for (index, name) in crate::interface::PAGES.iter().enumerate() {
        if ui.button(name) {
            interface.set_page(index);
        }
        ui.same_line();
    }
    ui.new_line();
}

fn flight_tab(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    widgets::section_title(ui, "FLIGHT DETAILS");
    if let Some(_table) = ui.begin_table(
        "FlightFields",
        if ui.content_region_avail()[0] < 650.0 {
            2
        } else {
            3
        },
    ) {
        form_text(ui, "Callsign", &mut interface.draft.callsign, 32);
        form_text(ui, "Aircraft ICAO", &mut interface.draft.aircraft, 12);
        form_text(ui, "Registration", &mut interface.draft.registration, 32);
        form_text(ui, "Departure ICAO", &mut interface.draft.departure, 8);
        form_text(ui, "Destination ICAO", &mut interface.draft.destination, 8);
        form_text(ui, "Alternate ICAO", &mut interface.draft.alternate, 8);
        form_text(ui, "Departure runway", &mut interface.draft.runway, 12);
        form_text(
            ui,
            "Arrival runway",
            &mut interface.draft.arrival_runway,
            12,
        );
        form_integer(ui, "Cost index", &mut interface.draft.cost_index, 1);
        let metric = resolve_units(&units_name(interface), &state.plan.departure)
            == openatc_core::UnitSystem::Metric;
        form_altitude(
            ui,
            "Cruise altitude",
            &mut interface.draft.cruise_feet,
            metric,
        );
        form_altitude(
            ui,
            "Initial altitude",
            &mut interface.draft.initial_altitude_feet,
            metric,
        );
        form_number(
            ui,
            "Enroute time (min)",
            &mut interface.draft.estimated_minutes,
        );
        form_text(
            ui,
            "Departure stand",
            &mut interface.draft.departure_stand,
            32,
        );
        form_text(ui, "Arrival stand", &mut interface.draft.arrival_stand, 32);
    }
    widgets::section_title(ui, "ROUTE");
    widgets::edit_paragraph(ui, "##Route", &mut interface.draft.route, 86.0, 4096);
    ui.text_disabled(format!(
        "Source: {}   AIRAC: {}   {} mapped fixes",
        interface.draft.source,
        if interface.draft.airac.is_empty() {
            "unknown"
        } else {
            &interface.draft.airac
        },
        interface.draft.fixes.len()
    ));
}

/// Procedures tab: SID/STAR picker backed by loaded CIFP data.
fn procedures_tab(ui: &imgui::Ui, interface: &mut Interface) {
    widgets::section_title(ui, "DEPARTURE & ARRIVAL");
    if let Some(_table) = ui.begin_table("ProcedureFields", 2) {
        form_text(ui, "SID", &mut interface.draft.sid, 32);
        form_text(
            ui,
            "SID transition",
            &mut interface.draft.sid_transition,
            32,
        );
        form_text(ui, "STAR", &mut interface.draft.star, 32);
        form_text(
            ui,
            "STAR transition",
            &mut interface.draft.star_transition,
            32,
        );
        form_text(ui, "Approach", &mut interface.draft.approach, 32);
    }
    let _load_disabled = ui.begin_disabled(interface.loading_airport);
    if ui.button("Load departure procedures") {
        load_airport(interface, &interface.draft.departure.clone());
    }
    ui.same_line();
    if ui.button("Load arrival procedures") {
        load_airport(interface, &interface.draft.destination.clone());
    }
    ui.text_wrapped(&interface.airport_notice);
    ui.text_disabled(format!(
        "Click a procedure from {} to use its name and transition.",
        interface.airport.icao
    ));
    if let Some(_table) = ui.begin_table_with_flags(
        "ProcedureList",
        3,
        imgui::TableFlags::ROW_BG
            | imgui::TableFlags::SORTABLE
            | imgui::TableFlags::BORDERS_INNER_H
            | imgui::TableFlags::SIZING_STRETCH_PROP,
    ) {
        ui.table_setup_column("Type");
        ui.table_setup_column("Procedure");
        ui.table_setup_column("Transition");
        ui.table_headers_row();
        let mut rows = interface.airport.procedures.clone();
        sort_table(ui, &mut rows, |r, column| match column {
            0 => r.kind.clone(),
            1 => r.name.clone(),
            _ => r.transition.clone(),
        });
        for (index, procedure) in rows.into_iter().enumerate() {
            let _id = ui.push_id(index.to_string());
            ui.table_next_row();
            ui.table_next_column();
            ui.text(&procedure.kind);
            ui.table_next_column();
            if ui.selectable(&procedure.name) {
                if procedure.kind == "SID" && interface.airport.icao == interface.draft.departure {
                    interface.draft.sid.clone_from(&procedure.name);
                    interface
                        .draft
                        .sid_transition
                        .clone_from(&procedure.transition);
                } else if procedure.kind == "STAR"
                    && interface.airport.icao == interface.draft.destination
                {
                    interface.draft.star.clone_from(&procedure.name);
                    interface
                        .draft
                        .star_transition
                        .clone_from(&procedure.transition);
                } else if procedure.kind == "APPCH"
                    && interface.airport.icao == interface.draft.destination
                {
                    interface.draft.approach.clone_from(&procedure.name);
                }
            }
            ui.table_next_column();
            ui.text(&procedure.transition);
        }
    }
    ui.text_wrapped(
                "Procedure names are read from installed CIFP data. Leg constraints and aircraft performance are not calculated here.",
            );
}

/// Fuel tab: the load sheet in kilograms.
fn fuel_tab(ui: &imgui::Ui, interface: &mut Interface) {
    widgets::section_title(ui, "LOAD SHEET / KILOGRAMS");
    if let Some(_table) = ui.begin_table("LoadFields", 3) {
        form_integer(ui, "Passengers", &mut interface.draft.passengers, 1);
        form_number(ui, "Payload (kg)", &mut interface.draft.payload_kg);
        form_number(ui, "Cargo (kg)", &mut interface.draft.cargo_kg);
        form_number(ui, "Block fuel (kg)", &mut interface.draft.block_fuel_kg);
        form_number(ui, "Trip fuel (kg)", &mut interface.draft.trip_fuel_kg);
        form_number(ui, "Taxi fuel (kg)", &mut interface.draft.taxi_fuel_kg);
        form_number(
            ui,
            "Reserve + contingency (kg)",
            &mut interface.draft.reserve_fuel_kg,
        );
        form_number(
            ui,
            "Alternate fuel (kg)",
            &mut interface.draft.alternate_fuel_kg,
        );
        form_number(
            ui,
            "Zero fuel weight (kg)",
            &mut interface.draft.zero_fuel_weight_kg,
        );
        form_number(
            ui,
            "Takeoff weight (kg)",
            &mut interface.draft.takeoff_weight_kg,
        );
        form_number(
            ui,
            "Landing weight (kg)",
            &mut interface.draft.landing_weight_kg,
        );
    }
    ui.text_wrapped("Imported values are normalized to kg. This dispatch sheet does not load fuel or passengers into the aircraft. SimBrief remains the performance and fuel-planning source.");
}
/// Headless render tests: every page executes without a window.
/// Apply the active header sort every frame so refreshed lists stay ordered.
fn sort_table<T>(ui: &imgui::Ui, rows: &mut [T], key: impl Fn(&T, usize) -> String) {
    if let Some(mut specs) = ui.table_sort_specs_mut() {
        if let Some(spec) = specs.specs().iter().next() {
            let column = spec.column_idx();
            let descending = spec.sort_direction() == Some(imgui::TableSortDirection::Descending);
            rows.sort_by_cached_key(|row| key(row, column).to_lowercase());
            if descending {
                rows.reverse();
            }
        }
        specs.set_sorted();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// imgui allows a single live context process-wide; serialize the
    /// headless render tests.
    static CONTEXT_LOCK: Mutex<()> = Mutex::new(());
    use openatc_core::state::{Clearance, State, Telemetry, Transmission};

    fn headless() -> imgui::Context {
        let mut ctx = imgui::Context::create();
        ctx.set_ini_filename(None);
        ctx.io_mut().display_size = [1200.0, 860.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        widgets::configure_fonts(&mut ctx);
        ctx.fonts().build_rgba32_texture();
        ctx
    }

    fn filled_state() -> State {
        let clearance = Some(Clearance {
            altitude_feet: 5000,
            route: "DCT".to_owned(),
            runway: "09".to_owned(),
            squawk: "2105".to_owned(),
            acknowledged: false,
            sequence: 2,
            ..Default::default()
        });
        let transcript = vec![
            Transmission {
                pilot_reply: String::new(),
                background: false,
                speaker: "VH-BIL".to_owned(),
                text: "request ifr clearance".to_owned(),
                sequence: 1,
                position: String::new(),
                voice: String::new(),
                delivery: "standard".to_owned(),
                speed: 1.0,
                urgent: false,
            },
            Transmission {
                pilot_reply: String::new(),
                background: false,
                speaker: "ATC".to_owned(),
                text: "Cleared.".to_owned(),
                sequence: 2,
                position: String::new(),
                voice: String::new(),
                delivery: "standard".to_owned(),
                speed: 1.0,
                urgent: false,
            },
        ];
        let telemetry = Telemetry {
            com1_khz: 121_900,
            ..Default::default()
        };
        State {
            clearance,
            transcript,
            telemetry,
            ..Default::default()
        }
    }

    fn interface_with_airport() -> Interface {
        let mut interface = Interface::new("http://127.0.0.1:9");
        interface.airport = openatc_core::airport::demo_airport();
        interface
    }

    #[test]
    fn taxi_defaults_to_current_airport_and_respects_plan_selectors() {
        let mut interface = interface_with_airport();
        let mut state = filled_state();
        state.plan.departure = "TEST".to_owned();
        state.plan.destination = "DEST".to_owned();
        interface.current_airport = "EGLL".to_owned();
        assert_eq!(taxi_airport(&interface, &state), "EGLL");
        interface.taxi_use_current = false;
        assert_eq!(taxi_airport(&interface, &state), state.plan.departure);
        interface.taxi_arrival = true;
        assert_eq!(taxi_airport(&interface, &state), state.plan.destination);
        interface.taxi_arrival = false;
        state.plan.departure.clear();
        assert_eq!(taxi_airport(&interface, &state), "EGLL");
    }

    #[test]
    fn pages_render_headless() {
        let _guard = CONTEXT_LOCK.lock().unwrap();
        let mut ctx = headless();
        let mut interface = interface_with_airport();
        let state = filled_state();
        // ATC page with clearance, transcript, frequencies and the grid.
        {
            let ui = ctx.frame();
            ui.window("test")
                .size([1200.0, 860.0], imgui::Condition::Always)
                .build(|| {
                    atc_page(ui, &mut interface, &state);
                });
            let _ = ctx.render();
        }
        // Altitude modal open on top of the page.
        interface.modal_intent = "altitude".to_owned();
        interface.modal_title = "Request altitude...".to_owned();
        {
            let ui = ctx.frame();
            ui.window("test")
                .size([1200.0, 860.0], imgui::Condition::Always)
                .build(|| {
                    atc_page(ui, &mut interface, &state);
                });
            let _ = ctx.render();
        }
        // Every page with a loaded demo airport behind it.
        let empty = State::default();
        for page in [
            plan_page,
            radio_page,
            taxi_page,
            arrival_page,
            airports_page,
            settings_page,
        ] {
            let ui = ctx.frame();
            ui.window("test")
                .size([1200.0, 860.0], imgui::Condition::Always)
                .build(|| {
                    page(ui, &mut interface, &state);
                });
            let _ = ctx.render();
        }
        // And once more with empty state for the no-data paths.
        for page in [
            plan_page,
            radio_page,
            taxi_page,
            arrival_page,
            airports_page,
            settings_page,
        ] {
            let ui = ctx.frame();
            ui.window("test")
                .size([1200.0, 860.0], imgui::Condition::Always)
                .build(|| {
                    page(ui, &mut interface, &empty);
                });
            let _ = ctx.render();
        }
        interface.modal_intent.clear();
        interface.page = 0;
        interface.settings.pin_open = true;
        for scale in [0.85, 1.0, 1.5] {
            interface.settings.ui_scale = scale;
            widgets::configure_scale(&mut ctx, scale);
            for compact in [false, true] {
                for size in [[960.0, 760.0], [780.0, 690.0], [780.0, 300.0]] {
                    interface.compact = compact;
                    interface.set_window_frame(crate::interface::WindowFrame {
                        position: [60.0, 40.0],
                        size,
                        can_pop_out: true,
                        popped_out: false,
                    });
                    let ui = ctx.frame();
                    let actions = interface.draw(ui);
                    assert!(!actions.close);
                    let draw_data = ctx.render();
                    assert!(draw_data.total_vtx_count > 0);
                    for draw_list in draw_data.draw_lists() {
                        for vertex in draw_list.vtx_buffer() {
                            assert!(vertex.pos.iter().all(|coordinate| coordinate.is_finite()));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn arrival_planned_and_current_layers_render_independently() {
        let _guard = CONTEXT_LOCK.lock().unwrap();
        let mut ctx = headless();
        let mut interface = interface_with_airport();
        let mut state = filled_state();
        interface.arrival.airport = interface.airport.clone();
        interface.arrival.transition_feet = 10000;
        state.plan.destination.clone_from(&interface.airport.icao);
        state
            .plan
            .arrival_runway
            .clone_from(&interface.airport.runways[0].first_name);
        state.telemetry = Telemetry {
            latitude: interface.airport.reference_latitude,
            longitude: interface.airport.reference_longitude - 0.8,
            altitude_feet: 8000.0,
            ground_speed_knots: 180.0,
            heading_degrees: 90.0,
            ground_track_degrees: Some(90.0),
            vertical_speed_fpm: -800.0,
            on_ground: false,
            position_valid: true,
            ..Telemetry::default()
        };
        let target =
            openatc_core::arrival::target(&interface.arrival, &state.plan.arrival_runway, "")
                .unwrap();
        assert!(openatc_core::arrival::current_gradient(&state.telemetry, &target).is_some());
        let mut counts = Vec::new();
        let mut current_vertices = Vec::new();
        for (planned, current) in [(false, false), (true, false), (false, true), (true, true)] {
            interface.settings.show_planned_descent = planned;
            interface.settings.show_current_descent = current;
            let mut count = 0;
            let mut orange = 0;
            for _ in 0..2 {
                let ui = ctx.frame();
                ui.window("arrival-layers")
                    .size([1200.0, 860.0], imgui::Condition::Always)
                    .build(|| arrival_page(ui, &mut interface, &state));
                let data = ctx.render();
                count = data.total_vtx_count;
                orange = 0;
                for list in data.draw_lists() {
                    for vertex in list.vtx_buffer() {
                        if vertex.col[0] > 240
                            && (185..=200).contains(&vertex.col[1])
                            && (90..=105).contains(&vertex.col[2])
                        {
                            orange += 1;
                        }
                        assert!(vertex.pos.iter().all(|p| p.is_finite()));
                    }
                }
            }
            counts.push(count);
            current_vertices.push(orange);
        }
        assert!(
            counts[1] > counts[0] + 100,
            "planned profile contributes actual curve geometry"
        );
        assert!(
            current_vertices[2] > current_vertices[0],
            "current projection is visible independently"
        );
        assert!(
            current_vertices[3] > current_vertices[1],
            "both layers remain visible together"
        );
    }

    #[test]
    fn audio_dropdown_with_real_devices() {
        let _guard = CONTEXT_LOCK.lock().unwrap();
        // Repro for the in-sim crash: real backend enumeration, then the
        // audio tab plus an open combo listing every cached device name.
        let mut ctx = headless();
        let mut interface = interface_with_airport();
        interface.speech.refresh_devices();
        let state = filled_state();
        for _ in 0..3 {
            let ui = ctx.frame();
            ui.window("test")
                .size([1200.0, 860.0], imgui::Condition::Always)
                .build(|| {
                    audio_tab(ui, &mut interface);
                });
            let _ = ctx.render();
        }
        let devices = interface.speech.input_devices();
        assert!(!devices.is_empty(), "system default always listed");
        let ui = ctx.frame();
        ui.window("test")
            .size([1200.0, 860.0], imgui::Condition::Always)
            .build(|| {
                ui.open_popup("MicPicker");
                ui.modal_popup("MicPicker", || {
                    for (index, device) in devices.iter().enumerate() {
                        let _id = ui.push_id_int(i32::try_from(index).unwrap_or(0));
                        ui.selectable(&device.name);
                    }
                });
            });
        let _ = ctx.render();
        let _ = state;
    }
    #[test]
    fn resize_grip_emits_resize_when_dragged() {
        let _guard = CONTEXT_LOCK.lock().unwrap();
        let mut ctx = headless();
        let mut interface = interface_with_airport();
        interface.settings.pin_open = true;
        ctx.io_mut().add_mouse_pos_event([930.0, 730.0]);
        let _ = interface.draw(ctx.frame());
        let _ = ctx.render();
        let _ = interface.draw(ctx.frame());
        let _ = ctx.render();
        ctx.io_mut()
            .add_mouse_button_event(imgui::MouseButton::Left, true);
        let _ = interface.draw(ctx.frame());
        let _ = ctx.render();
        ctx.io_mut().add_mouse_pos_event([944.0, 740.0]);
        let actions = interface.draw(ctx.frame());
        let _ = ctx.render();
        assert!(
            actions.resize_delta[0] > 0.0 && actions.resize_delta[1] > 0.0,
            "grip did not respond to drag: {:?}",
            actions.resize_delta
        );
    }
}
