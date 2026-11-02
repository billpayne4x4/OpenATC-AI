//! Communication, radio, flight planning, airport and settings pages.

// Layout tests compare exact fixture coordinates.
#![allow(clippy::float_cmp)]
// Layout limits bound the numeric casts below.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use crate::interface::Interface;
use crate::widgets;
use openatc_core::aircraft::AircraftBehaviour;
use openatc_core::ops::{phase_name, request_available, AcfSpecs, acf_specs_to_aircraft, select_best_approach, select_best_sid, select_best_star, validate_approach_performance, validate_sid_performance, validate_star_performance};
use openatc_core::state::{Request, State};
use openatc_core::{altitude_text, feet_to_meters, meters_to_feet, resolve_units};
use openatc_settings::Settings;
use std::fmt::Write as _;

/// Definition titles needing the altitude/waypoint modal.
fn needs_modal(intent: &str) -> bool {
    matches!(
        intent,
        "altitude"
            | "direct"
            | "descent"
            | "change_approach"
            | "departure_runway"
            | "arrival_runway"
            | "speed"
            | "deviation"
    )
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
            if ["altitude", "descent"].contains(&interface.modal_intent.as_str()) {
                widgets::muted_wrapped(
                    ui,
                    if metric {
                        "Altitude (m)"
                    } else {
                        "Altitude (ft)"
                    },
                );
                ui.set_next_item_width(-1.0);
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
                ui.input_int("##request-altitude", &mut shown)
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
            } else {
                let label = match interface.modal_intent.as_str() {
                    "departure_runway" | "arrival_runway" => "Runway (e.g. 25L)",
                    "change_approach" => "Published approach ID",
                    "speed" => "Indicated airspeed (knots)",
                    "deviation" => "Direction and degrees (e.g. LEFT 20)",
                    _ => "Waypoint",
                };
                widgets::muted_wrapped(ui, label);
                ui.set_next_item_width(-1.0);
                widgets::edit_text_flags(
                    ui,
                    "##request-value",
                    &mut interface.requested_waypoint,
                    32,
                    imgui::InputTextFlags::CHARS_UPPERCASE,
                );
            }
            let available = request_available(state, &interface.modal_intent);
            if !available {
                ui.text_colored(
                    widgets::warning(ui),
                    "Flight stage changed. Request no longer available.",
                );
            }
            let blocked = !available || !interface.engine.connected() || !interface.radio_power;
            let _disabled = ui.begin_disabled(blocked);
            if ui.button_with_size("Send request", [150.0, 0.0]) {
                let intent = interface.modal_intent.clone();
                let request = Request {
                    intent: intent.clone(),
                    altitude_feet: interface.requested_altitude,
                    waypoint: interface.requested_waypoint.clone(),
                    text: if ["altitude", "descent"].contains(&intent.as_str()) {
                        format!(
                            "Request {intent} {}",
                            altitude_text(f64::from(interface.requested_altitude), units, true)
                        )
                    } else {
                        openatc_core::dialogue::say(
                            &format!("pilot_request_{intent}_change"),
                            &[
                                ("value", interface.requested_waypoint.clone()),
                                ("callsign", state.plan.callsign.clone()),
                            ],
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

/// Active surface clearance and simulator airport take precedence over transmitter distance.
fn default_station_airport(interface: &Interface, state: &State) -> String {
    if state.telemetry.on_ground && !interface.occupied_airport.is_empty() {
        return interface.occupied_airport.clone();
    }
    if state.telemetry.on_ground
        && (state.taxi_clearance.approved || state.taxi_clearance.pending_readback)
        && !state.taxi_clearance.airport.is_empty()
    {
        return state.taxi_clearance.airport.clone();
    }
    for airport in [
        &interface.airport,
        &interface.local_airport,
        &interface.arrival.airport,
    ] {
        if !airport.icao.is_empty()
            && openatc_core::airport::contains_aircraft(airport, &state.telemetry)
        {
            return airport.icao.clone();
        }
    }
    if !interface.current_airport.is_empty() {
        return interface.current_airport.clone();
    }
    interface
        .nearby_stations
        .iter()
        .filter(|s| s.service != "Center")
        .min_by(|a, b| a.distance_nm.total_cmp(&b.distance_nm))
        .map_or_else(String::new, |s| s.airport.clone())
}

/// Surface services at the current airport; receivable stations when airborne.
fn radio_stations(interface: &Interface, state: &State) -> Vec<openatc_core::stations::Station> {
    let nearby = openatc_core::stations::nearby(&interface.nearby_stations, &state.telemetry);
    if state.telemetry.on_ground {
        let airport = default_station_airport(interface, state);
        if nearby.iter().any(|s| s.airport == airport) {
            return nearby
                .into_iter()
                .filter(|s| s.airport == airport)
                .collect();
        }
    }
    nearby
}

/// Radio page: station table and copilot toggles.
pub fn radio_page(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    let stations = radio_stations(interface, state);
    radio_header(ui, interface, state, &stations);
    radio_list(ui, interface, state, &stations);
    radio_footer(ui, interface);
}

/// Airport loader row, COM1 readout and power notices.
fn radio_header(
    ui: &imgui::Ui,
    interface: &mut Interface,
    state: &State,
    stations: &[openatc_core::stations::Station],
) {
    let airport = default_station_airport(interface, state);
    let at_airport = state.telemetry.on_ground
        && !airport.is_empty()
        && stations.iter().any(|s| s.airport == airport)
        && stations.iter().all(|s| s.airport == airport);
    ui.text_colored(
        widgets::accent(ui),
        format!(
            "RADIOS  /  {}  /  {} stations",
            if at_airport {
                airport.as_str()
            } else {
                "Nearby"
            },
            stations.len()
        ),
    );
    if interface.settings.show_com2 {
        let mut selected = usize::from(interface.active_radio == openatc_core::state::Radio::Com2);
        widgets::section_tabs(ui, &["COM1", "COM2"], &mut selected);
        interface.active_radio = if selected == 1 {
            openatc_core::state::Radio::Com2
        } else {
            openatc_core::state::Radio::Com1
        };
    }
    let radio = if interface.active_radio == openatc_core::state::Radio::Com2 {
        "COM2"
    } else {
        "COM1"
    };
    if let Some(frequency) = state.telemetry.frequency(interface.active_radio) {
        ui.text_disabled(format!("{radio}  {:.3} MHz", f64::from(frequency) / 1000.0));
    } else {
        ui.text_disabled(format!("{radio} unavailable"));
    }
    if !interface.radio_power {
        ui.same_line();
        ui.text_colored(widgets::warning(ui), "Radio power off");
    }
    ui.text_wrapped(if interface.simulator_host {
        "Select a station to tune the selected radio. Its current station is highlighted."
    } else {
        "Nearby airport services. Radio tuning is available inside X-Plane."
    });
    ui.text_disabled(if at_airport {
        "Showing this airport's published services."
    } else {
        "Showing nearby receivable stations, nearest first."
    });
    if !interface.aircraft_profile.is_empty() {
        ui.text_disabled(&interface.aircraft_profile);
    }
}

/// Station table with ATIS split and tune-on-select.
fn radio_list(
    ui: &imgui::Ui,
    interface: &mut Interface,
    state: &State,
    stations: &[openatc_core::stations::Station],
) {
    let scale = ui.current_font_size() / 18.0;
    let list_height = (ui.content_region_avail()[1] - 125.0 * scale).max(140.0 * scale);
    ui.child_window("##radio-list")
        .size([0.0, list_height])
        .build(|| {
            for (is_atis, title) in [(false, "CONTROLLERS"), (true, "ATIS / WEATHER")] {
                widgets::section_title(ui, title);
                if ui.content_region_avail()[0] < 650.0 * scale {
                    for frequency in stations {
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
                        let tuned = state.telemetry.frequency(interface.active_radio) == Some(frequency.khz);
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
                                interface.requested_frequency_radio = interface.active_radio;
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
                            if tuned {
                                widgets::accent(ui)
                            } else {
                                widgets::muted(ui)
                            },
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
                    let mut distance = imgui::TableColumnSetup::new("Distance / status");
                    distance.flags = imgui::TableColumnFlags::DEFAULT_SORT
                        | imgui::TableColumnFlags::PREFER_SORT_ASCENDING;
                    ui.table_setup_column_with(distance);
                    ui.table_headers_row();
                    let mut rows = stations.to_vec();
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
                        let tuned = state.telemetry.frequency(interface.active_radio) == Some(frequency.khz);
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
                            interface.requested_frequency_radio = interface.active_radio;
                        }
                        ui.table_next_column();
                        ui.text(if frequency.service == "Clearance" {
                            "Delivery"
                        } else {
                            &frequency.service
                        });
                        ui.table_next_column();
                        ui.text_colored(
                            if tuned {
                                widgets::accent(ui)
                            } else {
                                widgets::muted(ui)
                            },
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
                if !interface.engine.connected() {
                    ui.text_wrapped("Waiting for the local plugin engine. Check its connection in Settings → General.");
                } else if let Some(error) = interface.engine.telemetry_error() {
                    ui.text_wrapped(format!("Stations need an accepted simulator position. {error}"));
                } else if !state.telemetry.position_valid {
                    ui.text_wrapped("Waiting for a valid simulator position. Load a flight to show nearby frequencies.");
                } else if !interface.stations_loading {
                    ui.text_wrapped("No receivable stations found. Check the X-Plane installation folder in Settings → General and the scenery loading message above.");
                }
            }
        });
}

/// Copilot radio toggles with instant save.
fn radio_footer(ui: &imgui::Ui, interface: &mut Interface) {
    if radio_assistance(ui, &mut interface.settings) {
        interface.save_settings();
    }
    // Refresh notices belong below the controls so station rows stay under the pointer.
    if !interface.radio_notice.is_empty() {
        ui.text_wrapped(&interface.radio_notice);
    }
}

fn radio_assistance(ui: &imgui::Ui, settings: &mut openatc_settings::Settings) -> bool {
    let mut changed = ui.checkbox(
        "Copilot reads back clearances",
        &mut settings.copilot_replies,
    );
    let mut tunes = settings.copilot_tunes || settings.auto_tune_handoff;
    if ui.checkbox("Tune the assigned radio on handoff", &mut tunes) {
        settings.copilot_tunes = tunes;
        settings.auto_tune_handoff = tunes;
        changed = true;
    }
    changed
}

/// Top of the ATC page: pin toggle, phase, COM1 readout and the clearance card.
fn header_card(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    ui.text_colored(widgets::accent(ui), phase_name(state.phase));
    if state.telemetry.com1_khz > 0 {
        ui.same_line();
        ui.text_disabled(format!(
            "{} {:.3}",
            if interface.page == 10 { "COM2" } else { "COM1" },
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
                    widgets::accent(ui),
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
                let text = format!(
                    "{}  /  {}  /  Squawk {}  /  Runway {}",
                    altitude_text(f64::from(clearance.altitude_feet), units, false),
                    clearance.route,
                    clearance.squawk,
                    clearance.runway
                );
                if let (Some(TranscriptAction::Squawk(code)), _) = linked_transcript_text(
                    ui,
                    &text,
                    widgets::text_color(ui, [0.94, 0.95, 0.96, 1.0]),
                    false,
                    interface.radio_power,
                ) {
                    interface.requested_squawk = Some(code);
                }
            });
    }
}

fn transcript_on_radio(
    entry: &openatc_core::state::Transmission,
    state: &State,
    radio: openatc_core::state::Radio,
) -> bool {
    crate::interface::on_tuned_frequency(entry, state, radio)
}

fn transcript_view(ui: &imgui::Ui, interface: &mut Interface, state: &State, height: f32) {
    let scale = ui.current_font_size() / 18.0;
    let background_alpha = interface.background_opacity * 0.72;
    let _background = ui.push_style_color(
        imgui::StyleColor::ChildBg,
        widgets::panel_color(ui, background_alpha),
    );
    let _padding = ui.push_style_var(imgui::StyleVar::WindowPadding([12.0 * scale, 10.0 * scale]));
    ui.child_window("Transcript").size([0.0, height]).border(false).always_use_window_padding(true).build(|| {
        let layout = (ui.window_size(), interface.compact);
        let resized = interface.transcript_layout != Some(layout);
        if !resized {
            interface.transcript_follow_latest = ui.scroll_y() >= ui.scroll_max_y() - 8.0 * scale;
            interface.transcript_scroll = ui.scroll_y();
        }
        interface.transcript_layout = Some(layout);
        if state.transcript.is_empty() {
            ui.text_colored(widgets::accent(ui), "Ready when you are");
            ui.text_wrapped("Set up your flight plan, then request clearance. Available requests follow your flight stage.");
        }
        for entry in state
            .transcript
            .iter()
            .filter(|entry| transcript_on_radio(entry, state, interface.active_radio))
        {
            let _identifier = ui.push_id(entry.sequence.to_string());
            let airport = if interface.local_airport.icao.is_empty() { &interface.airport } else { &interface.local_airport };
            let badge = transcript_badge(entry, airport, state.telemetry.com1_khz);
            let color = widgets::text_color(ui, match entry.speaker.as_str() {
                "ATC" | "ATIS" => [0.48, 0.77, 0.94, 1.0],
                "CABIN" | "ATTENDANT" => [0.75, 0.83, 0.77, 1.0],
                "GROUND" => [0.86, 0.79, 0.59, 1.0],
                "COPILOT" => [0.76, 0.79, 0.89, 1.0],
                "SYSTEM" => widgets::AMBER,
                _ => [0.94, 0.95, 0.96, 1.0],
            });
            if let Some(frequency) = transcript_line(ui, &badge, &entry.text, color, scale, entry.speaker == "ATC", interface.radio_power) {
                match frequency {
                    TranscriptAction::Frequency(value) => { interface.requested_frequency_khz = Some(value); interface.requested_frequency_radio = interface.active_radio; }
                    TranscriptAction::Squawk(value) => interface.requested_squawk = Some(value),
                }
            }
        }
        for notice in &interface.local_notices {
            transcript_line(ui, "INFO", &notice.text, widgets::warning(ui), scale, false, false);
        }
        let sequence = state.transcript.last().map_or(0, |e| e.sequence);
        if resized || sequence != interface.last_transcript_sequence {
            if interface.transcript_follow_latest || interface.last_transcript_sequence == 0 {
                ui.set_scroll_here_y_with_ratio(1.0);
            } else if resized {
                ui.set_scroll_y(interface.transcript_scroll);
            }
        }
        interface.last_transcript_sequence = sequence;
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
        "CABIN" | "ATTENDANT" => "Attendant".to_owned(),
        "GROUND" => "Ground services".to_owned(),
        "ATIS" => "ATIS".to_owned(),
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
        _ if entry.position == "Traffic" => format!("TFC · {}", entry.speaker),
        _ => "YOU".to_owned(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum TranscriptAction {
    Frequency(i32),
    Squawk(i32),
}

fn transcript_squawk(words: &[&str], index: usize) -> Option<i32> {
    let code = words[index].trim_matches(|c: char| !c.is_ascii_digit());
    if code.len() != 4 || !code.bytes().all(|value| (b'0'..=b'7').contains(&value)) {
        return None;
    }
    let previous = words[..index]
        .iter()
        .rev()
        .take(2)
        .map(|word| {
            word.trim_matches(|c: char| !c.is_ascii_alphabetic())
                .to_ascii_lowercase()
        })
        .collect::<Vec<_>>();
    previous
        .iter()
        .any(|word| matches!(word.as_str(), "squawk" | "squawking" | "code"))
        .then(|| code.parse().ok())
        .flatten()
}

fn transcript_line(
    ui: &imgui::Ui,
    badge: &str,
    text: &str,
    color: [f32; 4],
    scale: f32,
    links: bool,
    powered: bool,
) -> Option<TranscriptAction> {
    let position = ui.cursor_screen_pos();
    let label_size = ui.calc_text_size(badge);
    let size = [
        (label_size[0] + 20.0 * scale).max(68.0 * scale),
        25.0 * scale,
    ];
    if ui.content_region_avail()[0] < size[0] + 160.0 * scale {
        let _color = ui.push_style_color(imgui::StyleColor::Text, color);
        ui.text_wrapped(badge);
        let (frequency, hovered_link) = linked_transcript_text(ui, text, color, links, powered);
        if frequency.is_none() && !hovered_link {
            selectable_message(ui, text);
        }
        return frequency;
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
    drop(draw);
    ui.dummy(size);
    ui.same_line();
    let _text_color = ui.push_style_color(imgui::StyleColor::Text, color);
    let (frequency, hovered_link) = linked_transcript_text(ui, text, color, links, powered);
    if frequency.is_none() && !hovered_link {
        selectable_message(ui, text);
    }
    frequency
}

/// Decimal COM frequencies only; runway numbers and altitudes remain ordinary text.
fn transcript_frequency(word: &str) -> Option<i32> {
    let number = word.trim_matches(|c: char| !c.is_ascii_digit());
    let (whole, decimal) = number.split_once('.')?;
    if whole.len() != 3
        || !(1..=3).contains(&decimal.len())
        || !whole
            .bytes()
            .chain(decimal.bytes())
            .all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let frequency = whole.parse::<i32>().ok()? * 1000
        + decimal.parse::<i32>().ok()? * 10_i32.pow(3 - decimal.len() as u32);
    (118_000..=136_990)
        .contains(&frequency)
        .then_some(frequency)
}

fn linked_transcript_text(
    ui: &imgui::Ui,
    text: &str,
    color: [f32; 4],
    links: bool,
    powered: bool,
) -> (Option<TranscriptAction>, bool) {
    let words = text.split_whitespace().collect::<Vec<_>>();
    if !words.iter().enumerate().any(|(index, word)| {
        (links && transcript_frequency(word).is_some())
            || transcript_squawk(&words, index).is_some()
    }) {
        ui.text_wrapped(text);
        return (None, false);
    }
    let origin = ui.cursor_screen_pos();
    let width = ui.content_region_avail()[0].max(1.0);
    let line_height = ui.text_line_height();
    let space = ui.calc_text_size(" ")[0];
    let draw = ui.get_window_draw_list();
    let mut cursor = origin;
    let mut clicked = None;
    let mut hovered_link = false;
    for (index, word) in text.split_whitespace().enumerate() {
        let size = ui.calc_text_size(word);
        if cursor[0] > origin[0] && cursor[0] + size[0] > origin[0] + width {
            cursor = [origin[0], cursor[1] + line_height];
        }
        let frequency = if links {
            transcript_frequency(word).map(TranscriptAction::Frequency)
        } else {
            None
        }
        .or_else(|| transcript_squawk(&words, index).map(TranscriptAction::Squawk));
        let mut text_color = color;
        if let Some(frequency) = frequency {
            ui.set_cursor_screen_pos(cursor);
            ui.invisible_button(format!("##frequency-{index}"), [size[0], line_height]);
            let hovered = ui.is_item_hovered();
            hovered_link |= hovered;
            text_color = if hovered && powered {
                widgets::text_color(ui, [0.76, 0.93, 1.0, 1.0])
            } else {
                widgets::accent(ui)
            };
            if hovered {
                ui.set_mouse_cursor(Some(imgui::MouseCursor::Hand));
                ui.tooltip_text(if powered {
                    match frequency {
                        TranscriptAction::Frequency(value) => {
                            format!("Tune to {:.3} MHz", f64::from(value) / 1000.0)
                        }
                        TranscriptAction::Squawk(value) => {
                            format!("Set squawk {value:04}; preserve the mode selector")
                        }
                    }
                } else {
                    "Turn on the radio to tune".to_owned()
                });
            }
            if powered && ui.is_item_clicked() {
                clicked = Some(frequency);
            }
            draw.add_line(
                [cursor[0], cursor[1] + line_height - 1.0],
                [cursor[0] + size[0], cursor[1] + line_height - 1.0],
                text_color,
            )
            .build();
        }
        draw.add_text(cursor, text_color, word);
        cursor[0] += size[0] + space;
    }
    ui.set_cursor_screen_pos(origin);
    ui.dummy([width, cursor[1] - origin[1] + line_height]);
    (clicked, hovered_link)
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
        let _talk_disabled = ui.begin_disabled(!interface.engine.connected());
        if ui.button_with_size("Copilot", [102.0 * scale, 0.0]) {
            interface.transmit_box_to_copilot();
        }
        if ui.is_item_hovered() {
            ui.tooltip_text(
                "Talk to the copilot. Transmit addresses the selected radio or crew member.",
            );
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
            widgets::warning(ui),
            if crew == "atc" {
                "Transmit needs radio power — Talk to the copilot still works."
            } else {
                "Talking to the selected crew member needs electrical power."
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
    widgets::same_line_if_fits(
        ui,
        ui.calc_text_size("Replay ATC")[0] + ui.clone_style().frame_padding[0] * 2.0,
    );
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
    widgets::same_line_if_fits(
        ui,
        ui.calc_text_size("ATC Auto Reply")[0] + ui.clone_style().frame_padding[0] * 2.0,
    );
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
    widgets::muted_wrapped(ui, speech_status);
    drop(rec_disabled);
    if interface.settings.dev_mode {
        ui.new_line();
        if ui.button("Copy ATC conversation") {
            let mut text = crate::debug::conversation(&interface.engine.state());
            let _ = writeln!(text, "Callouts: {}", interface.callout_status);
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

fn quick_request_available(
    state: &State,
    station: Option<&openatc_core::stations::Station>,
    intent: &str,
) -> bool {
    let Some(station) = station else {
        return false;
    };
    if !request_available(state, intent) || !openatc_core::stations::station_serves(station, intent)
    {
        return false;
    }
    if intent == "checkin" {
        return state.telemetry.on_ground
            || !["Clearance", "Ground"].contains(&station.service.as_str());
    }
    if intent == "cross_runway" {
        return state.taxi_clearance.approved
            && !state.taxi_clearance.pending_readback
            && state.taxi_clearance.crossing_runway.is_empty()
            && !state.taxi_clearance.hold_short_runway.is_empty()
            && state.taxi_clearance.hold_short_runway != state.plan.runway;
    }
    if intent == "progressive" {
        return !state.taxi_clearance.points.is_empty();
    }
    true
}

fn quick_request_text(
    interface: &Interface,
    state: &State,
    station: Option<&openatc_core::stations::Station>,
    intent: &str,
    title: &str,
) -> String {
    let Some(station) = station else {
        return title.to_owned();
    };
    if intent == "checkin" {
        return openatc_core::readback::checkin_text(state, &station.name);
    }
    let callsign = if intent == "clearance" {
        &interface.draft.callsign
    } else {
        interface.transmission_callsign(state)
    };
    let text = if intent == "clearance" {
        openatc_core::dialogue::say(
            "pilot_request_ifr_clearance",
            &[
                ("station", station.name.clone()),
                ("callsign", callsign.to_owned()),
                (
                    "dest",
                    if interface.arrival.airport.icao == interface.draft.destination
                        && !interface.arrival.airport.name.is_empty()
                    {
                        interface.arrival.airport.name.clone()
                    } else {
                        interface
                            .map_airports
                            .iter()
                            .find(|a| a.id == interface.draft.destination)
                            .map_or_else(|| interface.draft.destination.clone(), |a| a.name.clone())
                    },
                ),
            ],
        )
    } else if matches!(
        intent,
        "taxi" | "start" | "pushback" | "start_pushback" | "ready"
    ) {
        openatc_core::dialogue::say(
            &format!("pilot_request_{intent}"),
            &[
                ("station", station.name.clone()),
                ("callsign", callsign.to_owned()),
            ],
        )
    } else {
        title.to_owned()
    };
    super::interface::format_typed_transmission(&station.name, callsign, &text)
}

fn request_grid(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    let scale = ui.current_font_size() / 18.0;
    if let Some(reply) = interface.engine.take_reply("/atc/offer")
        && !reply.success
    {
        interface.notice = reply.error;
    }
    if let Some(offer) = &state.atc_offer
        && offer.frequency_khz == state.telemetry.com1_khz
    {
        let _disabled = ui.begin_disabled(state.telemetry.radio_busy || !interface.radio_power);
        if ui.button(format!("Accept direct {}", offer.waypoint)) {
            interface.engine.post(
                "/atc/offer",
                serde_json::json!({"sequence":offer.sequence,"accept":true}),
            );
        }
        ui.same_line();
        if ui.button("Keep planned route") {
            interface.engine.post(
                "/atc/offer",
                serde_json::json!({"sequence":offer.sequence,"accept":false}),
            );
        }
    }
    ui.set_next_item_width((ui.content_region_avail()[0] - 160.0 * scale).max(100.0 * scale));
    widgets::edit_text(ui, "Filter requests", &mut interface.search, 128);
    let filter = interface.search.to_lowercase();
    let nearby = openatc_core::stations::nearby(&interface.nearby_stations, &state.telemetry);
    let station = openatc_core::stations::tuned(&nearby, state.telemetry.com1_khz);
    if let Some(station) = station {
        ui.text_disabled(format!(
            "{} · {:.3} MHz",
            station.name,
            f64::from(station.khz) / 1000.0
        ));
    } else {
        ui.text_wrapped(if interface.page == 10 {
            "No controller is available on COM2. Tune COM2 to a nearby station."
        } else {
            "No controller is available on COM1. Choose a station in Radios."
        });
    }
    if !interface.engine.connected() {
        ui.text_wrapped(interface.engine.status());
    }
    ui.child_window("Requests").size([0.0, 0.0]).build(|| {
        let _alignment = ui.push_style_var(imgui::StyleVar::ButtonTextAlign([0.0, 0.5]));
        if interface
            .pending_request
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() > std::time::Duration::from_secs(2))
        {
            interface.pending_request = None;
        }
        for (intent_id, button_title) in openatc_core::catalog::buttons() {
            let intent = intent_id.as_str();
            let title = button_title.as_str();
            if interface
                .pending_request
                .as_ref()
                .is_some_and(|(pending, _)| pending == intent)
            {
                continue;
            }
            if !quick_request_available(state, station, intent)
                || (!filter.is_empty() && !title.to_lowercase().contains(&filter))
            {
                continue;
            }
            let _disabled =
                ui.begin_disabled(!interface.engine.connected() || !interface.radio_power);
            if ui.button_with_size(title, [ui.content_region_avail()[0], 34.0 * scale]) {
                if needs_modal(intent) {
                    interface.requested_waypoint = match intent {
                        "departure_runway" => state.plan.runway.clone(),
                        "arrival_runway" => state.plan.arrival_runway.clone(),
                        "change_approach" => state.plan.approach.clone(),
                        "deviation" => "LEFT 20".into(),
                        "speed" => state
                            .telemetry
                            .indicated_speed_knots
                            .map_or_else(String::new, |v| format!("{v:.0}")),
                        _ => String::new(),
                    };
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
                    request.text = quick_request_text(interface, state, station, intent, title);
                    interface.pending_request =
                        Some((intent.to_owned(), std::time::Instant::now()));
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
            widgets::muted(ui),
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
                interface.transmission_callsign(state),
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
                ui.text_colored(widgets::warning(ui), &interface.engine_fault);
            }
            request_grid(ui, interface, state);
        });
}

/// Dispatch page: `SimBrief` import and the flight used for clearance.
pub fn plan_page(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    let import_disabled = ui.begin_disabled(interface.importing || !interface.engine.connected());
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
    drop(import_disabled);
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

/// Load procedures catalogue for an airport from the simulator navdata.
fn load_procedures_catalogue(icao: &str, simulator_root: &str) -> Option<openatc_core::traffic::procedures::Catalogue> {
    if icao.is_empty() || icao.len() != 4 {
        return None;
    }
    if simulator_root.is_empty() {
        return None;
    }
    let custom = format!("{simulator_root}/Custom Data/CIFP/{icao}.dat");
    let path = if std::path::Path::new(&custom).is_file() {
        custom
    } else {
        format!("{simulator_root}/Resources/default data/CIFP/{icao}.dat")
    };
    std::fs::read_to_string(&path).ok().and_then(|text| {
        openatc_core::traffic::procedures::Catalogue::parse(&text).ok()
    })
}

/// Labelled text field inside a form table.
fn form_text(ui: &imgui::Ui, label: &str, value: &mut String, capacity: usize) {
    ui.table_next_column();
    widgets::muted_wrapped(ui, label);
    ui.set_next_item_width(-1.0);
    widgets::edit_text(ui, &format!("##{label}"), value, capacity);
}

/// Labelled integer field inside a form table.
fn form_integer(ui: &imgui::Ui, label: &str, value: &mut i32, step: i32) {
    ui.table_next_column();
    widgets::muted_wrapped(ui, label);
    ui.set_next_item_width(-1.0);
    ui.input_int(&format!("##{label}"), value)
        .step(step)
        .build();
}

/// Labelled float field inside a form table. Rounds through f32 like the
/// widget does; dispatch figures carry no fractional kilos.
fn form_number(ui: &imgui::Ui, label: &str, value: &mut f64) {
    ui.table_next_column();
    widgets::muted_wrapped(ui, label);
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
    widgets::muted_wrapped(ui, &title);
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
        widgets::accent(ui),
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
    if ui.checkbox("Follow-me car", &mut interface.settings.show_follow_me) {
        interface.save_settings();
    }
    if !interface.settings.show_ground_taxi_arrows {
        ui.same_line();
        ui.text_disabled("Ground arrows are off");
    }
    if interface.taxi_arrival {
        arrival_parking(ui, interface, state);
    }
    if (state.taxi_clearance.approved || state.taxi_clearance.pending_readback)
        && state.taxi_clearance.airport == interface.airport.icao
    {
        if state.taxi_clearance.pending_readback {
            ui.text_disabled("Taxi clearance awaiting readback");
        }
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
        ("Taxi route", &mut interface.settings.show_taxi_route),
        ("Aircraft", &mut interface.settings.show_ownship),
        ("Show traffic", &mut interface.taxi_traffic_visible),
        ("Labels", &mut interface.settings.show_labels),
        ("Traffic debug", &mut interface.settings.show_traffic_debug),
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
    let Some(size) = openatc_core::parking::aircraft_size(state) else {
        ui.text_wrapped("Aircraft size unavailable. Set wingspan_meters in its aircraft profile.");
        return;
    };
    ui.text(format!("Aircraft size {size} / suitable arrival stands"));
    ui.set_next_item_width(260.0);
    widgets::edit_text(
        ui,
        "Filter stands (C, D, E...)",
        &mut interface.parking_filter,
        64,
    );
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
            if !openatc_core::parking::fits(&parking, size)
                || !parking
                    .name
                    .to_lowercase()
                    .contains(&interface.parking_filter.to_lowercase())
            {
                continue;
            }
            let occupied = openatc_core::parking::occupied(
                &interface.airport,
                &parking,
                &state.telemetry,
                size,
            );
            let suffix = match occupied {
                Some(true) => " — occupied",
                None => " — traffic unavailable",
                _ => "",
            };
            let _disabled = ui.begin_disabled(occupied != Some(false));
            if ui.selectable(format!("{} / {}{}", parking.name, parking.size, suffix)) {
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
    ui.set_next_item_width(100.0 * ui.current_font_size() / 18.0);
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
    if interface.airport.icao.is_empty() {
        ui.text_wrapped(
            "Enter an airport ICAO code above, or open an airport from your flight plan.",
        );
        for (label, icao) in [
            ("Departure", &state.plan.departure),
            ("Destination", &state.plan.destination),
        ] {
            if !icao.is_empty() {
                let _disabled = ui.begin_disabled(interface.loading_airport);
                if ui.button(format!("{label}: {icao}")) {
                    load_airport(interface, icao);
                }
            }
        }
        ui.text_wrapped(&interface.airport_notice);
        return;
    }
    let units = resolve_units(&units_name(interface), &interface.airport.icao);
    ui.text_colored(
        widgets::accent(ui),
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

/// Settings tabs wrap so every section stays within reach.
#[allow(clippy::items_after_statements)]
pub fn settings_page(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    if interface.show_voice_popup {
        voice_popup(ui, interface);
        return;
    }
    {
        let _disabled =
            ui.begin_disabled(!interface.settings_loaded || !interface.engine.connected());
        if ui.button("Save now") {
            interface.save_settings();
        }
    }
    ui.same_line();
    ui.text_disabled(if !interface.settings_loaded {
        "Waiting for saved settings"
    } else if interface.settings_notice.is_empty() {
        "Changes save automatically"
    } else {
        &interface.settings_notice
    });
    const SECTIONS: [&str; 13] = [
        "General",
        "Appearance",
        "Copilot",
        "Controls",
        "AI",
        "STT / TTS",
        "Voices",
        "Audio",
        "Traffic",
        "Realism",
        "Automation",
        "Sessions",
        "About",
    ];
    interface.settings_section = interface.settings_section.min(SECTIONS.len() - 1);
    widgets::section_tabs(ui, &SECTIONS, &mut interface.settings_section);
    {
        let _disabled = ui.begin_disabled(!interface.settings_loaded);
        if interface.settings_section == 0 {
            settings_general(ui, interface, state);
        }
        if interface.settings_section == 1 {
            settings_appearance(ui, interface);
        }
        if interface.settings_section == 2 {
            widgets::section_title(ui, "RADIO ASSISTANCE");
            radio_assistance(ui, &mut interface.settings);
            ui.checkbox(
                "Show COM2 communications",
                &mut interface.settings.show_com2,
            );
            ui.checkbox(
                "Show copilot talk button",
                &mut interface.settings.copilot_button,
            );
            ui.text_wrapped("Readbacks and tuning wait until the current transmission finishes. Enable both for automatic handoff acknowledgement, tuning and check-in. With tuning off, click an underlined frequency or tune the aircraft radio yourself.");
            widgets::section_title(ui, "AUTOPILOT TARGETS");
            ui.checkbox(
                "Copilot sets altitude preselect",
                &mut interface.settings.copilot_flies_ap,
            );
            ui.text_wrapped("With the autopilot engaged, the copilot sets the cleared altitude and announces it. With it off, or when the dials are unreachable, the copilot advises instead. Automatic clearance assistance does not engage the autopilot. Explicit requests to the copilot can operate mapped controls, including the autopilot. Automatic heading and speed changes still require instruction sources.");
            ui.text_wrapped("These options apply to OpenATC AI's controller. They do not intercept X-Plane's built-in ATC, Next ATC or VATSIM.");
        }
        if interface.settings_section == 3 {
            widgets::section_title(ui, "CREW CONTROL MAPPINGS");
            ui.text_wrapped("Stock controls use built-in X-Plane mappings. Third-party controls and checklist order come from the aircraft TOML. Spoken requests and replies come from speech TOMLs. ATTN/CAB selects cabin crew; MECH/INT selects ground services; VHF selects radio ATC. Mapped latching buttons return to ATC when deselected; momentary call commands can retain the selected recipient.");
            intercom_overrides(ui, interface);
            ui.input_text("Find control", &mut interface.crew_control_filter)
                .build();
            let state = interface.engine.state();
            let filter = interface.crew_control_filter.to_lowercase();
            let available = state.crew_controls.iter().filter(|c| c.available).count();
            ui.text(format!(
                "{available} available / {} configured",
                state.crew_controls.len()
            ));
            if let Some(_list) = ui
                .child_window("crew_control_list")
                .size([0.0, 0.0])
                .begin()
            {
                for control in state.crew_controls.iter().filter(|c| {
                    filter.is_empty()
                        || c.label.to_lowercase().contains(&filter)
                        || c.id.contains(&filter)
                        || c.role.contains(&filter)
                }) {
                    let status = if control.available {
                        "Available"
                    } else {
                        "Unavailable"
                    };
                    ui.text(format!(
                        "{}  /  {}  /  {status}",
                        control.label, control.role
                    ));
                    if let Some(value) = control.value {
                        ui.same_line();
                        ui.text_disabled(format!("{value:.2}"));
                    }
                }
            }
        }
        if interface.settings_section == 4 {
            widgets::section_title(ui, "REQUESTS & WORDING");
            ui.checkbox(
                "Use AI for free-form requests",
                &mut interface.settings.ai_enabled,
            );
            {
                let _disabled = ui.begin_disabled(!interface.settings.ai_enabled);
                ui.checkbox(
                    "Natural wording variety",
                    &mut interface.settings.llm_phrase_variety,
                );
            }
            ui.text_wrapped(
                "Off: ATC uses the TOML phrases as written. On: AI uses the phrases as examples for natural wording. Instructions and values stay fixed; invalid wording falls back to TOML.",
            );
            ui.text_wrapped("With AI off, use the available request buttons or recognised standard phrases. The AI interprets requests and varies wording; the ATC engine checks clearances and permissions.");
            if ui.collapsing_header("Advanced AI connection", imgui::TreeNodeFlags::empty()) {
                setting_input(ui, "AI server address", &mut interface.settings.ai_url, 256);
                setting_input(ui, "AI model", &mut interface.settings.ai_model, 128);
                ui.text_wrapped("Enter an HTTP or HTTPS server address without /v1 or an endpoint path. OpenAI-compatible chat completions are supported. Set OPENATC_AI_KEY on the engine if the provider requires a key.");
            }
        }
        if interface.settings_section == 5 {
            ui.text_wrapped("Use General to connect all services to one OpenATC AI server. These addresses can also use separate providers.");
            widgets::section_title(ui, "SPEECH RECOGNITION (MICROPHONE)");
            setting_input(
                ui,
                "Speech recognition server",
                &mut interface.settings.stt_url,
                256,
            );
            setting_input(ui, "STT model", &mut interface.settings.stt_model, 128);
            widgets::section_title(ui, "SPOKEN VOICES");
            setting_input(ui, "Voice server", &mut interface.settings.tts_url, 256);
            setting_input(ui, "TTS model", &mut interface.settings.tts_model, 128);
            ui.text_wrapped("Services use /v1/audio/transcriptions and /v1/audio/speech. Audio requests go through the local engine, which supports HTTP and HTTPS. Set OPENATC_STT_KEY and OPENATC_TTS_KEY on the engine if required. Keys are not saved in project settings.");
        }
        if interface.settings_section == 6 {
            voices_tab(ui, interface);
        }
        if interface.settings_section == 7 {
            audio_tab(ui, interface);
        }
        if interface.settings_section == 8 {
            crate::traffic::settings(ui, interface);
        }
        if interface.settings_section == 9 {
            realism_tab(ui, interface);
        }
        if interface.settings_section == 10 {
            automation_tab(ui, interface);
        }
        if interface.settings_section == 11 {
            session_tab(ui, interface, state);
        }
        if interface.settings_section == 12 {
            settings_about(ui, interface);
        }
    }
}

/// Build information for the plugin, engine and AI server.
fn settings_about(ui: &imgui::Ui, interface: &mut Interface) {
    widgets::section_title(ui, "BUILD INFORMATION");
    ui.text(format!("Plugin: {}", crate::VERSION));
    ui.text(format!(
        "Engine: {}",
        if interface.engine_version.is_empty() {
            "unknown"
        } else {
            &interface.engine_version
        }
    ));
    ui.text(format!(
        "AI server: {}",
        if interface.ai_version.is_empty() {
            "unknown"
        } else {
            &interface.ai_version
        }
    ));
    ui.text_wrapped(
        "The plugin version is compiled into this interface. The engine and AI server versions come from the running processes, so they show whether an older build is still active.",
    );
    if ui.button("Refresh versions") {
        interface
            .engine
            .post("/voice-health", serde_json::Value::Null);
    }
}

/// First-run connections and installed data.
fn settings_general(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    widgets::section_title(ui, "FLIGHT IDENTITY");
    let previous_callsign = interface.settings.callsign.clone();
    setting_input(ui, "Default callsign", &mut interface.settings.callsign, 16);
    if interface.settings.callsign != previous_callsign
        && openatc_core::ops::planning_callsign(state, &interface.settings.callsign).is_some()
        && (interface.draft.callsign.trim().is_empty()
            || interface
                .draft
                .callsign
                .eq_ignore_ascii_case(previous_callsign.trim()))
    {
        interface
            .draft
            .callsign
            .clone_from(&interface.settings.callsign);
    }
    ui.text_wrapped("Used when the flight plan does not provide a callsign. SimBrief import and the dispatch sheet still override it. Changing it before clearance also updates the Planning draft and ATC identity.");
    widgets::section_title(ui, "GETTING STARTED");
    ui.text_wrapped("1. Start the OpenATC AI server on this computer or another computer on your network.\n2. Connect the services below, then test your microphone and a voice in Audio.\n3. Import or enter a flight plan and tune Clearance Delivery (or Ground/Tower if combined). Available requests appear on the ATC page.");
    ui.text_colored(
        if interface.engine.connected() {
            widgets::text_color(ui, [0.3, 0.85, 0.65, 1.0])
        } else {
            widgets::warning(ui)
        },
        if interface.engine.connected() {
            "Plugin engine connected"
        } else {
            "Waiting for the plugin engine"
        },
    );
    if let Some(error) = interface.engine.telemetry_error() {
        ui.text_colored(widgets::warning(ui), "Simulator data needs attention");
        ui.text_wrapped(error);
    }
    widgets::section_title(ui, "CURRENT SERVICE ADDRESSES");
    ui.text_wrapped(format!("AI: {}", interface.settings.ai_url));
    ui.text_wrapped(format!(
        "Speech recognition: {}",
        interface.settings.stt_url
    ));
    ui.text_wrapped(format!("Voices: {}", interface.settings.tts_url));
    ui.text_wrapped(
        "Edit separate providers in AI and STT / TTS, or replace all three addresses below.",
    );
    setting_input(
        ui,
        "Use one server (optional)",
        &mut interface.setup_server_url,
        256,
    );
    if ui.button("Use this server for AI, recognition and voices") {
        interface
            .settings
            .ai_url
            .clone_from(&interface.setup_server_url);
        interface
            .settings
            .stt_url
            .clone_from(&interface.setup_server_url);
        interface
            .settings
            .tts_url
            .clone_from(&interface.setup_server_url);
        interface.save_settings();
    }
    ui.text_wrapped("Use 127.0.0.1 for this computer, or the other computer's network address with its port. This changes the three service addresses; model and voice choices are kept.");
    if ui.button("Test audio & microphone") {
        interface.settings_section = 7;
    }
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
}

fn settings_appearance(ui: &imgui::Ui, interface: &mut Interface) {
    widgets::section_title(ui, "INTERFACE");
    let mut theme = usize::from(interface.settings.light_theme);
    ui.text_disabled("Theme");
    ui.set_next_item_width(-1.0);
    if ui.combo_simple_string("##Appearance", &mut theme, &["Dark", "Light"]) {
        interface.settings.light_theme = theme == 1;
    }
    let mut transparency = (1.0 - interface.settings.window_opacity) * 100.0;
    if labeled_slider(
        ui,
        "Background transparency",
        &mut transparency,
        0.0,
        80.0,
        "%.0f%%",
    ) {
        interface.settings.window_opacity = 1.0 - transparency / 100.0;
    }
    ui.text_wrapped("0% is solid. Higher values reveal more of the simulator behind the window. Text and buttons keep their contrast.");
    labeled_slider(
        ui,
        "Text scale",
        &mut interface.settings.ui_scale,
        0.85,
        1.5,
        "%.2fx",
    );
    ui.checkbox(
        "Keep the full window open",
        &mut interface.settings.pin_open,
    );
    let fade_disabled = ui.begin_disabled(interface.settings.pin_open);
    labeled_slider(
        ui,
        "Collapse after idle seconds",
        &mut interface.settings.fade_delay,
        1.0,
        30.0,
        "%.0f seconds",
    );
    labeled_slider(
        ui,
        "Collapsed background opacity",
        &mut interface.settings.faded_opacity,
        0.1,
        0.85,
        "%.2f",
    );
    drop(fade_disabled);
    widgets::section_title(ui, "UNITS");
    units_combo(ui, interface);
    ui.text_wrapped("Imperial is the aviation standard: feet, knots, nautical miles. Metric uses meters, km/h, kilometers. Region follows local procedure for the departure country. Weights stay in kilograms; pressure and visibility follow regional procedure.");
    if ui.collapsing_header("Developer tools", imgui::TreeNodeFlags::empty()) {
        ui.checkbox("Enable debugging tools", &mut interface.settings.dev_mode);
        ui.text_wrapped("Shows the controller coverage map, phase fast-forward and detailed errors. Fast-forward moves your aircraft; leave this off for normal flights.");
        if interface.settings.dev_mode {
            use openatc_settings::TrafficLog;
            ui.text_disabled("Traffic diagnostics");
            ui.set_next_item_width(-1.0);
            let preview = match interface.settings.traffic_log {
                TrafficLog::Off => "Off",
                TrafficLog::Summary => "Summary",
                TrafficLog::Detail => "Detail",
            };
            if let Some(_combo) = ui.begin_combo("##traffic-log", preview) {
                for option in ["Off", "Summary", "Detail"] {
                    if ui.selectable(option) {
                        interface.settings.traffic_log = match option {
                            "Off" => TrafficLog::Off,
                            "Detail" => TrafficLog::Detail,
                            _ => TrafficLog::Summary,
                        };
                        interface.save_settings();
                    }
                }
            }
            ui.text_wrapped("Summary logs publication, phase changes, gate counts and every refusal. Detail adds per-aircraft routes, clearance boundaries and radio state.");
            if ui.checkbox(
                "Show traffic holding points",
                &mut interface.settings.show_traffic_holding_points,
            ) {
                interface.save_settings();
            }
            ui.text_wrapped("Rings the next hold of every generated ground aircraft: before a runway it must cross, or at the departure runway holding point. The ring is the end of the surface route, so it shows where the clearance is keyed, not where the aircraft happens to have stopped.");
            crate::setup::controls(ui, interface);
        }
    }
}

fn intercom_overrides(ui: &imgui::Ui, interface: &mut Interface) {
    if ui.collapsing_header(
        "Advanced aircraft intercom overrides",
        imgui::TreeNodeFlags::empty(),
    ) {
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
        ui.text_wrapped("Use persistent call-light datarefs, not momentary pushbuttons. Leave these blank to use the aircraft's configured bindings. An active ground call takes priority, followed by cabin crew, then ATC. The Copilot button addresses the copilot directly. Bind openatc/talk and openatc/talk_copilot in X-Plane keyboard/joystick settings to transmit with the window closed.");
    }
}

/// Labelled settings text field.
pub fn setting_input(ui: &imgui::Ui, title: &str, value: &mut String, capacity: usize) {
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
) -> bool {
    ui.text_disabled(title);
    ui.set_next_item_width(-1.0);
    ui.slider_config(format!("##{title}"), minimum, maximum)
        .display_format(format)
        .build(value)
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
    let mut digit_by_digit =
        interface.settings.callsign_numbers == openatc_settings::CallsignNumbers::Digits;
    if ui.checkbox("Read flight numbers digit by digit", &mut digit_by_digit) {
        interface.settings.callsign_numbers = if digit_by_digit {
            openatc_settings::CallsignNumbers::Digits
        } else {
            openatc_settings::CallsignNumbers::Group
        };
    }
    ui.text_wrapped("On: Air Canada 160 is spoken as one six zero. Off: group form, one sixty, as preferred in Australia and the USA.");
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
        interface.refresh_voices();
    }
    if ui.checkbox(
        "English-language voices only",
        &mut interface.settings.english_voices_only,
    ) {
        interface.save_settings();
    }
    ui.text_wrapped("Controllers keep their voices across restarts. Turn off the language filter to try international voices with English text. Controllers use different voices until the pool is exhausted.");
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
    widgets::section_title(ui, "AUTOMATED ATIS");
    setting_input(
        ui,
        "ATIS voice (blank uses controller)",
        &mut interface.settings.atis_voice,
        128,
    );
    labeled_slider(
        ui,
        "ATIS reading speed",
        &mut interface.settings.atis_speed,
        0.5,
        2.0,
        "%.2fx",
    );
    ui.checkbox(
        "Electronic ATIS voice effect",
        &mut interface.settings.atis_electronic,
    );
    ui.text_wrapped("Use a voice from the provider's voice list. The electronic effect approximates an older automated broadcast; it does not flatten the voice's pitch.");
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

/// Browse the provider roster and audition voices with the same English sentence.
fn voice_popup(ui: &imgui::Ui, interface: &mut Interface) {
    widgets::section_title(ui, "VOICE PREVIEW");
    ui.text_wrapped("Listen before adding a voice to your controller pool. International voices still receive English text; pronunciation and accent depend on the model.");
    if ui.button("Refresh voice list") {
        interface.refresh_voices();
    }
    widgets::same_line_if_fits(
        ui,
        ui.calc_text_size("Back to voice settings")[0] + ui.clone_style().frame_padding[0] * 2.0,
    );
    if ui.button("Back to voice settings") {
        interface.show_voice_popup = false;
    }
    widgets::muted_wrapped(ui, &interface.voices_notice);
    if ui.checkbox(
        "English-language voices only",
        &mut interface.settings.english_voices_only,
    ) {
        interface.save_settings();
    }
    setting_input(
        ui,
        "Find voice or language",
        &mut interface.voice_filter,
        128,
    );
    if interface.voice_preview_text.is_empty() {
        interface.voice_preview_text = openatc_core::dialogue::say("voice_preview", &[]);
    }
    setting_input(
        ui,
        "English preview text",
        &mut interface.voice_preview_text,
        2000,
    );
    ui.separator();
    let voices = interface.available_voices.clone();
    let filter = interface.voice_filter.to_lowercase();
    let mut pool = openatc_core::ops::parse_voice_pool(&interface.settings.voice_pool);
    ui.child_window("Provider voices")
        .size([0.0, -30.0])
        .build(|| {
            if let Some(_table) = ui.begin_table_with_flags(
                "voice-roster",
                4,
                imgui::TableFlags::ROW_BG
                    | imgui::TableFlags::BORDERS_INNER_H
                    | imgui::TableFlags::SIZING_STRETCH_PROP,
            ) {
                ui.table_setup_column("Voice");
                ui.table_setup_column("Language");
                ui.table_setup_column("Preview");
                ui.table_setup_column("Controller pool");
                ui.table_headers_row();
                for voice in &voices {
                    if interface.settings.english_voices_only && !voice.english {
                        continue;
                    }
                    if !filter.is_empty()
                        && !format!("{} {} {}", voice.id, voice.name, voice.language)
                            .to_lowercase()
                            .contains(&filter)
                    {
                        continue;
                    }
                    let _id = ui.push_id(&voice.id);
                    ui.table_next_row();
                    ui.table_next_column();
                    ui.text(&voice.name);
                    if voice.name != voice.id {
                        ui.text_disabled(&voice.id);
                    }
                    ui.table_next_column();
                    ui.text(&voice.language);
                    ui.table_next_column();
                    if ui.button("Listen") {
                        interface.speak_entry(&openatc_core::state::Transmission {
                            radio: None,
                            frequency: None,
                            gap_ms: 0,
                            information_only: false,
                            speaker: "ATC".into(),
                            text: interface.voice_preview_text.clone(),
                            voice: voice.id.clone(),
                            delivery: "standard".into(),
                            speed: 1.0,
                            pilot_reply: String::new(),
                            background: false,
                            sequence: 0,
                            position: String::new(),
                            urgent: false,
                        });
                    }
                    ui.table_next_column();
                    let included = pool.iter().any(|id| id == &voice.id);
                    if ui.button(if included { "Remove" } else { "Add" }) {
                        if included {
                            pool.retain(|id| id != &voice.id);
                        } else {
                            pool.push(voice.id.clone());
                        }
                        interface.settings.voice_pool = pool.join(", ");
                        interface.save_settings();
                    }
                }
            }
        });
    ui.text_disabled(interface.speech.status());
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
    widgets::muted_wrapped(ui, title);
    ui.set_next_item_width(-1.0);
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
    let master_before = interface.settings.master_volume;
    labeled_slider(
        ui,
        "Master volume",
        &mut interface.settings.master_volume,
        0.0,
        1.0,
        "%.2f",
    );
    if (interface.settings.master_volume - master_before).abs() > f32::EPSILON {
        // Push the new level into the audio callback immediately so the phrase that is already
        // playing changes volume instead of waiting for the next utterance.
        interface.speech.configure(
            &interface.settings.input_device.clone(),
            &interface.settings.output_device.clone(),
            interface.settings.input_gain,
            interface.settings.master_volume,
        );
    }
    ui.text_wrapped("Master volume scales every utterance, including the one already playing. The role volumes set how loud each kind of message is relative to it.");
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
    labeled_slider(
        ui,
        "Traffic sound volume",
        &mut interface.settings.traffic_sound_volume,
        0.0,
        1.0,
        "%.2f",
    );
    ui.text_wrapped("Traffic sound volume controls the volume of AI-generated aircraft engine, gear, flaps, tire roll, and reverser sounds. Set to 0 to disable traffic sounds entirely.");
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
            radio: None,
            frequency: None,
            gap_ms: 0,
            information_only: false,
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
    widgets::muted_wrapped(
        ui,
        "Microphone level: −60 dBFS (quiet) to 0 dBFS (clipping)",
    );
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
    widgets::section_title(ui, "PILOT CONVERSATION");
    ui.checkbox(
        "Allow informal pilot conversations",
        &mut interface.settings.informal_pilot_chat,
    );
    ui.checkbox(
        "Allow light humour in pilot replies",
        &mut interface.settings.pilot_chat_humour,
    );
    ui.checkbox(
        "Allow other pilots to call me",
        &mut interface.settings.pilots_call_me,
    );
    ui.checkbox(
        "Enable airport chatter on tuned frequencies",
        &mut interface.settings.airport_chatter,
    );
    let mut chat_chance = i32::from(interface.settings.pilot_chat_chance_percent);
    widgets::muted_wrapped(ui, "Chance per eligible opportunity (%)");
    ui.set_next_item_width(-1.0);
    let _chat_disabled = ui.begin_disabled(!interface.settings.pilots_call_me);
    if ui.slider("##pilot-chat-chance", 0, 100, &mut chat_chance) {
        interface.settings.pilot_chat_chance_percent = chat_chance as u8;
    }
    drop(_chat_disabled);
    ui.text_wrapped("Checks for a current reason before rolling: recent strong wind or reduced visibility near the other pilot can prompt a request for a weather report. No reason means no call, even at 100%. Checks are spaced by the conversation interval; 0% disables unsolicited calls. Operational questions remain available independently of informal chat.");
    widgets::section_title(ui, "TAXI GUIDANCE");
    ui.checkbox(
        "Show taxi arrows in plugin map",
        &mut interface.settings.show_taxi_arrows,
    );
    ui.checkbox(
        "Show taxi arrows on simulator ground",
        &mut interface.settings.show_ground_taxi_arrows,
    );
    ui.checkbox(
        "Show holding point on map and simulator ground",
        &mut interface.settings.show_holding_point,
    );
    ui.text_wrapped("Guidance follows the approved taxi route and stops at the clearance limit. Ground arrows are available in X-Plane while on the ground.");
    ui.checkbox(
        "Show follow-me car on approved taxi route",
        &mut interface.settings.show_follow_me,
    );
    ui.text_wrapped("Uses an installed X-Plane scenery vehicle. The guide stops before each clearance limit; following it does not authorize a runway crossing.");
}

/// Automation tab: full flight automation controls.
fn automation_tab(ui: &imgui::Ui, interface: &mut Interface) {
    widgets::section_title(ui, "FLIGHT AUTOMATION");
    let _disabled = ui.begin_disabled(!interface.engine.connected());

    // Fetch automation status
    if let Some(reply) = interface.engine.take_reply("/automation") {
        if reply.success {
            interface.automation_status = reply.data;
        }
    }
    if interface.automation_status["lastRequested"] != "status" {
        interface.engine.post("/automation", serde_json::json!({"action": "status"}));
        interface.automation_status["lastRequested"] = "status".into();
    }

    // Automation status display
    let automation = &interface.automation_status;
    let active = automation["active"].as_bool().unwrap_or(false);
    ui.text_colored(
        if active {
            widgets::accent(ui)
        } else {
            widgets::warning(ui)
        },
        format!("Status: {}", if active { "ACTIVE" } else { "IDLE" })
    );
    ui.text(format!("Phase: {:?}", automation["phase"].as_str().unwrap_or("Idle")));
    ui.text(format!(
        "Flight: {} / {}",
        automation["current_flight"].as_u64().unwrap_or(0),
        automation["total_flights"].as_u64().unwrap_or(0)
    ));
    ui.text(format!("Last action: {}", automation["last_action"].as_str().unwrap_or("")));
    ui.text(format!("Next action: {}", automation["next_action"].as_str().unwrap_or("")));
    ui.spacing();

    // Controls
    if !active {
        if ui.button("Start Automation") {
            interface.engine.post("/automation", serde_json::json!({"action": "start"}));
        }
    } else {
        if ui.button("Stop Automation") {
            interface.engine.post("/automation", serde_json::json!({"action": "stop"}));
        }
        ui.same_line();
        if ui.button("Stop After This Flight") {
            interface.engine.post("/automation", serde_json::json!({"action": "toggle_stop_after_flight"}));
        }
        let stop_after_flight = automation["stop_after_flight"].as_bool().unwrap_or(false);
        ui.text_wrapped(if stop_after_flight {
            "Will stop after current flight completes."
        } else {
            "Will continue to next flight (if randomized flights enabled)."
        });
    }

    ui.spacing();
    ui.separator();
    ui.spacing();

    // Configuration
    ui.checkbox(
        "Enable full flight automation (startup to shutdown)",
        &mut interface.settings.automation_enabled,
    );
    ui.checkbox(
        "VFR mode (visual flight rules)",
        &mut interface.settings.automation_vfr_mode,
    );
    ui.checkbox(
        "Rotorcraft operations mode",
        &mut interface.settings.automation_rotorcraft_mode,
    );
    ui.checkbox(
        "Randomized multi-leg flights",
        &mut interface.settings.automation_randomized_flights,
    );
    if interface.settings.automation_randomized_flights {
        let mut min_interval = i32::from(interface.settings.automation_min_flight_interval_min);
        widgets::muted_wrapped(ui, "Min interval between flights (minutes)");
        ui.set_next_item_width(-1.0);
        if ui.slider("##auto-min-interval", 5, 120, &mut min_interval) {
            interface.settings.automation_min_flight_interval_min = min_interval as u16;
        }
        let mut max_interval = i32::from(interface.settings.automation_max_flight_interval_min);
        widgets::muted_wrapped(ui, "Max interval between flights (minutes)");
        ui.set_next_item_width(-1.0);
        if ui.slider("##auto-max-interval", 5, 180, &mut max_interval) {
            interface.settings.automation_max_flight_interval_min = max_interval as u16;
        }
    }
    ui.text_wrapped("Automation controls the autopilot, radios, and checklists. Requires copilot_flies_ap enabled in Copilot settings. VFR mode skips IFR clearances. Rotorcraft mode adjusts profiles for vertical flight. Randomized flights select compatible airports based on aircraft range and runway requirements.");
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
    let compliance_matches = interface.settings.enforce_clearance_constraints
        == matches!(preset.as_str(), "standard" | "real")
        && [
            interface.settings.monitor_transponder,
            interface.settings.monitor_runway_permissions,
            interface.settings.monitor_taxi_route,
        ]
        .iter()
        .all(|enabled| *enabled == interface.settings.enforce_clearance_constraints);
    let preview = match (preset.as_str(), compliance_matches) {
        ("relaxed", true) => "Relaxed",
        ("real", true) => "Strict",
        ("standard", true) => "Standard",
        _ => "Custom",
    };
    ui.text_disabled("Discipline preset");
    ui.set_next_item_width(-1.0);
    if let Some(_combo) = ui.begin_combo("##Discipline", preview) {
        if ui.selectable("Relaxed") {
            let settings = &mut interface.settings;
            settings.strict_readbacks = false;
            settings.enforce_clearance_constraints = false;
            settings.monitor_transponder = false;
            settings.monitor_runway_permissions = false;
            settings.monitor_taxi_route = false;
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
            settings.enforce_clearance_constraints = true;
            settings.monitor_transponder = true;
            settings.monitor_runway_permissions = true;
            settings.monitor_taxi_route = true;
            settings.require_frequency = true;
            settings.require_callsign = false;
            settings.strict_phraseology = true;
            settings.teaching_corrections = true;
            settings.congestion = openatc_settings::Congestion::Quiet;
            settings.practice_emergencies = false;
        }
        if ui.selectable("Strict") {
            let settings = &mut interface.settings;
            settings.strict_readbacks = true;
            settings.enforce_clearance_constraints = true;
            settings.monitor_transponder = true;
            settings.monitor_runway_permissions = true;
            settings.monitor_taxi_route = true;
            settings.require_frequency = true;
            settings.require_callsign = true;
            settings.strict_phraseology = true;
            settings.teaching_corrections = false;
            settings.congestion = openatc_settings::Congestion::Busy;
            settings.practice_emergencies = true;
        }
    }
    ui.text_wrapped("Relaxed allows casual requests and disables compliance monitoring. Standard checks readbacks, monitors assigned altitude and explains corrections. Strict also requires callsigns and removes teaching hints. Changing a preset setting shows Custom. Radio reception and runway permissions still apply in every preset.");
}

/// Readback and radio toggles.
fn readbacks_row(ui: &imgui::Ui, interface: &mut Interface) {
    widgets::section_title(ui, "ARRIVAL SEQUENCING");
    ui.checkbox(
        "Allow ATC to assign traffic holding",
        &mut interface.settings.player_traffic_holding,
    );
    ui.text_wrapped("Allows a discretionary delay hold when arrival spacing needs more time and a protected holding route is available. Turning this off prevents new optional holds; accepted holds, wake separation, runway protection and go-arounds still apply. Holding and safety instructions do not use a random chance.");
    widgets::section_title(ui, "OPTIONAL ATC OFFERS");
    ui.checkbox(
        "Allow ATC to offer en-route shortcuts",
        &mut interface.settings.atc_shortcut_offers,
    );
    let mut chance = i32::from(interface.settings.atc_shortcut_chance_percent);
    widgets::muted_wrapped(ui, "Chance per eligible opportunity (%)");
    ui.set_next_item_width(-1.0);
    let _disabled = ui.begin_disabled(!interface.settings.atc_shortcut_offers);
    if ui.slider("##shortcut-chance", 0, 100, &mut chance) {
        interface.settings.atc_shortcut_chance_percent = chance as u8;
    }
    drop(_disabled);
    ui.text_wrapped("Rolled only after finding a useful shortcut with checked terrain, crossing restrictions and descent distance. Each route opportunity is tried once, with at least five minutes between checks. 0% disables offers; 100% offers every eligible opportunity. Accepting requests a new clearance. Required traffic spacing, holding, handoffs and go-arounds do not use this percentage.");
    widgets::section_title(ui, "CLEARANCE COMPLIANCE");
    ui.checkbox(
        "Monitor altitude, course, heading and speed",
        &mut interface.settings.enforce_clearance_constraints,
    );
    ui.text_wrapped("Checks recorded instructions and published crossing restrictions. Approved deviations are allowed within their limits. Speed checks use indicated airspeed, not ground speed. You can request amended instructions in any mode.");
    ui.checkbox(
        "Monitor assigned squawk and transponder mode",
        &mut interface.settings.monitor_transponder,
    );
    ui.checkbox(
        "Warn about runway use without clearance",
        &mut interface.settings.monitor_runway_permissions,
    );
    ui.checkbox(
        "Offer help when off the taxi route",
        &mut interface.settings.monitor_taxi_route,
    );
    ui.text_wrapped("Uses observed simulator state. Missing transponder data is ignored. Taxi assistance waits for a sustained route deviation. These reminders never grant runway permission.");
    ui.checkbox(
        "Offer assistance after repeated corrections",
        &mut interface.settings.terminate_after_warnings,
    );
    widgets::muted_wrapped(
        ui,
        "This escalation is a simulator training rule, not an automatic real-world IFR cancellation procedure.",
    );

    widgets::section_title(ui, "READBACKS & RADIO");
    ui.checkbox(
        "Check required readback details",
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
    ui.text_wrapped("Quiet adds short reply delays. Busy adds longer delays and standby calls. Traffic pilot reports are controlled separately in Settings → Traffic.");
}

/// Emergency practice toggle.
fn emergencies_row(ui: &imgui::Ui, interface: &mut Interface) {
    widgets::section_title(ui, "EMERGENCIES");
    ui.checkbox(
        "Practice emergency flows",
        &mut interface.settings.practice_emergencies,
    );
    ui.text_wrapped("Enables emergency declarations for practice. An active urgent or emergency mission also permits a declaration. Cabin reports do not declare an emergency to ATC for you.");
}

/// Session tab: demo controls, flight state and the reset modal.
fn session_tab(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    widgets::section_title(ui, "DEMO CONTROLS");
    let demo_disabled = ui.begin_disabled(!state.demo);
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
    drop(demo_disabled);
    widgets::section_title(ui, "ACTIVE FLIGHT");
    ui.text(format!(
        "Stage: {}",
        openatc_core::ops::phase_name(state.phase)
    ));
    ui.text_wrapped(
        "Use the New flight icon in the window header to start fresh without disabling the plugin.",
    );
    ui.text_wrapped("The controller remembers stages while the engine is running. Reset starts a new flight; live telemetry will identify an airborne start.");
}

fn flight_tab(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    widgets::section_title(ui, "FLIGHT DETAILS");

    // Show loaded aircraft info and allow using it
    if let Some(specs) = &interface.loaded_aircraft_specs {
        ui.text_colored(widgets::accent(ui), "Loaded Aircraft (from simulator):");
        ui.text_wrapped(format!(
            "{} ({}) - Wingspan: {:.1}m, Length: {:.1}m, MTOW: {:.0}kg, Ceiling: {}ft, Engine: {} ({}x)",
            specs.description,
            specs.icao,
            specs.wingspan_meters.unwrap_or(0.0),
            specs.length_meters.unwrap_or(0.0),
            specs.max_takeoff_weight_kg.unwrap_or(0.0),
            specs.ceiling_feet.unwrap_or(0),
            specs.engine_type,
            specs.engine_count.unwrap_or(0)
        ));
        if ui.button("Use loaded aircraft for flight plan") {
            interface.draft.aircraft = specs.icao.clone();
            if let Some(wingspan) = specs.wingspan_meters {
                interface.draft.registration = format!("WS{:.0}m", wingspan);
            }
            if let Some(ceiling) = specs.ceiling_feet {
                interface.draft.cruise_feet = ceiling.min(41000).max(10000);
            }
        }
        ui.spacing();
        ui.separator();
        ui.spacing();
    }

    if let Some(_table) = ui.begin_table("FlightFields", widgets::form_columns(ui)) {
        form_text(ui, "Callsign", &mut interface.draft.callsign, 32);
        #[cfg(test)]
        {
            let (a, b) = (ui.item_rect_min(), ui.item_rect_max());
            interface.plan_callsign_bounds = [a[0], a[1], b[0], b[1]];
        }
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

    // Auto-select procedures based on aircraft performance
    if let Some(specs) = &interface.loaded_aircraft_specs {
        let aircraft = acf_specs_to_aircraft(specs);
        if ui.button("Auto-select procedures for loaded aircraft") {
            if let Some(catalogue) = load_procedures_catalogue(&interface.draft.departure, &interface.settings.simulator_root) {
                if let Some(sid) = select_best_sid(&catalogue, &interface.draft.runway, &aircraft) {
                    interface.draft.sid = sid;
                }
            }
            if let Some(catalogue) = load_procedures_catalogue(&interface.draft.destination, &interface.settings.simulator_root) {
                if let Some(star) = select_best_star(&catalogue, &interface.draft.arrival_runway, &aircraft) {
                    interface.draft.star = star;
                }
                if let Some(approach) = select_best_approach(&catalogue, &aircraft) {
                    interface.draft.approach = approach;
                }
            }
        }
        ui.text_disabled("Selects SID/STAR/Approach based on aircraft climb/descent performance and RNAV capability.");
        ui.spacing();
        ui.separator();
        ui.spacing();
    }

    if let Some(_table) = ui.begin_table("ProcedureFields", widgets::form_columns(ui).min(2)) {
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
    widgets::same_line_if_fits(
        ui,
        ui.calc_text_size("Load arrival procedures")[0] + ui.clone_style().frame_padding[0] * 2.0,
    );
    if ui.button("Load arrival procedures") {
        load_airport(interface, &interface.draft.destination.clone());
    }
    ui.text_wrapped(&interface.airport_notice);
    if !interface.draft.procedure_notice.is_empty() {
        ui.text_wrapped(&interface.draft.procedure_notice);
    }
    ui.text_disabled(format!(
        "Click a procedure from {} to use its name and transition.",
        interface.airport.icao
    ));

    // Show performance validation warnings
    if let Some(specs) = &interface.loaded_aircraft_specs {
        let aircraft = acf_specs_to_aircraft(specs);
        let sim_root = &interface.settings.simulator_root;
        if !interface.draft.sid.is_empty() && !interface.draft.runway.is_empty() {
            if let Some(catalogue) = load_procedures_catalogue(&interface.draft.departure, sim_root) {
                if let Ok(warnings) = validate_sid_performance(&catalogue, &interface.draft.sid, &interface.draft.runway, &interface.draft.sid_transition, &aircraft) {
                    for warning in warnings {
                        ui.text_colored(widgets::warning(ui), format!("⚠ {}", warning));
                    }
                }
            }
        }
        if !interface.draft.star.is_empty() && !interface.draft.arrival_runway.is_empty() {
            if let Some(catalogue) = load_procedures_catalogue(&interface.draft.destination, sim_root) {
                if let Ok(warnings) = validate_star_performance(&catalogue, &interface.draft.star, &interface.draft.arrival_runway, &interface.draft.star_transition, &aircraft) {
                    for warning in warnings {
                        ui.text_colored(widgets::warning(ui), format!("⚠ {}", warning));
                    }
                }
            }
        }
        if !interface.draft.approach.is_empty() {
            if let Some(catalogue) = load_procedures_catalogue(&interface.draft.destination, sim_root) {
                if let Ok(warnings) = validate_approach_performance(&catalogue, &interface.draft.approach, &interface.draft.approach, &aircraft) {
                    for warning in warnings {
                        ui.text_colored(widgets::warning(ui), format!("⚠ {}", warning));
                    }
                }
            }
        }
    }

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
    if let Some(_table) = ui.begin_table("LoadFields", widgets::form_columns(ui)) {
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
    #[test]
    fn squawk_links_accept_reminders_and_reject_altitudes_and_invalid_digits() {
        for text in [
            "Squawk 6111.",
            "verify squawking 6111.",
            "assigned code is 6111.",
            "check transponder code 6111",
        ] {
            let words = text.split_whitespace().collect::<Vec<_>>();
            assert_eq!(
                words
                    .iter()
                    .enumerate()
                    .find_map(|(index, _)| super::transcript_squawk(&words, index)),
                Some(6111)
            );
        }
        for text in [
            "maintain 5000 feet",
            "squawk 8911",
            "squawk 121.705",
            "code 12345",
        ] {
            let words = text.split_whitespace().collect::<Vec<_>>();
            assert!(
                words
                    .iter()
                    .enumerate()
                    .all(|(index, _)| super::transcript_squawk(&words, index).is_none())
            );
        }
    }

    #[test]
    fn transcripts_keep_radio_and_interphone_messages_separate() {
        use openatc_core::state::{Radio, Transmission};
        for (value, receiver) in [
            (serde_json::json!("com1"), Radio::Com1),
            (serde_json::json!("com2"), Radio::Com2),
            (serde_json::Value::Null, Radio::Com1),
        ] {
            let entry: Transmission = serde_json::from_value(
                serde_json::json!({"radio":value,"speaker":"ATIS","text":"Information Alpha"}),
            )
            .unwrap();
            let state = State::default();
            assert!(super::transcript_on_radio(&entry, &state, receiver));
            assert!(!super::transcript_on_radio(
                &entry,
                &state,
                if receiver == Radio::Com1 {
                    Radio::Com2
                } else {
                    Radio::Com1
                }
            ));
        }
    }

    #[test]
    fn transcript_hides_traffic_from_an_untuned_station() {
        use openatc_core::state::{Radio, Transmission};
        let mut state = State::default();
        state.telemetry.com1_khz = 120_160;
        let tower_traffic: Transmission = serde_json::from_value(serde_json::json!({
            "radio": "com1",
            "frequency": 118700,
            "background": true,
            "speaker": "Traffic"
        }))
        .expect("traffic entry");
        assert!(!super::transcript_on_radio(
            &tower_traffic,
            &state,
            Radio::Com1
        ));
        let own_clearance: Transmission = serde_json::from_value(serde_json::json!({
            "radio": "com1",
            "speaker": "ATC"
        }))
        .expect("own clearance");
        assert!(super::transcript_on_radio(
            &own_clearance,
            &state,
            Radio::Com1
        ));
        state.telemetry.com1_khz = 118_700;
        assert!(super::transcript_on_radio(
            &tower_traffic,
            &state,
            Radio::Com1
        ));
    }

    #[test]
    fn center_radio_keeps_departure_requests_available() {
        let mut state = State::default();
        state.on_event(openatc_core::aircraft::AircraftEvent::Departure);
        state.has_departed = true;
        state.clearance = Some(openatc_core::state::Clearance {
            altitude_feet: 5000,
            acknowledged: true,
            ..Default::default()
        });
        state.telemetry = openatc_core::state::Telemetry {
            latitude: 33.914_847,
            longitude: -118.384_915,
            altitude_feet: 3600.0,
            on_ground: false,
            position_valid: true,
            radio_power: true,
            com1_khz: 118_025,
            ..Default::default()
        };
        let stations = vec![openatc_core::stations::Station {
            name: "LOS ANGELES Center".into(),
            service: "Center".into(),
            khz: 118_025,
            latitude: 33.9,
            longitude: -118.4,
            ..Default::default()
        }];
        let nearby = openatc_core::stations::nearby(&stations, &state.telemetry);
        let station = openatc_core::stations::tuned(&nearby, state.telemetry.com1_khz);
        for intent in ["checkin", "radio_check", "frequency", "altitude", "direct"] {
            assert!(quick_request_available(&state, station, intent), "{intent}");
        }
        for intent in ["clearance", "taxi", "ready"] {
            assert!(
                !quick_request_available(&state, station, intent),
                "{intent}"
            );
        }
    }
    #[test]
    #[ignore = "requires OPENATC_TEST_ENGINE_URL pointing to an isolated engine with installed KLAX scenery"]
    fn delivery_buttons_and_requests_follow_live_radio_changes() {
        use std::time::{Duration, Instant};
        let endpoint = std::env::var("OPENATC_TEST_ENGINE_URL").unwrap();
        let mut interface = Interface::new(&endpoint);
        let wait_reply = |path: &str| {
            let end = Instant::now() + Duration::from_secs(40);
            loop {
                if let Some(reply) = interface.engine.take_reply(path) {
                    assert!(reply.success, "{path}: {}", reply.error);
                    break reply.data;
                }
                assert!(Instant::now() < end, "timed out: {path}");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        let mut t = Telemetry {
            latitude: 33.941_596,
            longitude: -118.382_634,
            altitude_feet: 125.0,
            on_ground: true,
            position_valid: true,
            com1_khz: 121_400,
            ..Default::default()
        };
        interface.engine.telemetry(t.clone());
        interface.engine.post(
            "/plan",
            serde_json::json!({
                "departure":"KLAX", "destination":"KLGB", "callsign":"C-GTLT",
                "route":"DCT SLI", "runway":"25L", "cruiseFeet":4000, "initialAltitudeFeet":4000
            }),
        );
        wait_reply("/plan");
        interface
            .engine
            .post("/stations/nearby", serde_json::json!({}));
        let data = wait_reply("/stations/nearby");
        let stations: Vec<openatc_core::stations::Station> =
            serde_json::from_value(data["stations"].clone()).unwrap();
        assert_eq!(data["occupiedAirport"], "KLAX");
        assert!(
            stations
                .iter()
                .any(|station| station.airport == "KLAX" && station.service == "Clearance")
        );
        let available = |intent: &str| {
            let s = interface.engine.state();
            let nearby = openatc_core::stations::nearby(&stations, &s.telemetry);
            let station = openatc_core::stations::tuned(&nearby, s.telemetry.com1_khz);
            quick_request_available(&s, station, intent)
        };
        assert!(!available("clearance"));
        t.com1_khz = 120_350;
        interface.engine.telemetry(t.clone());
        assert!(
            available("clearance"),
            "Delivery must expose clearance immediately after confirmed tuning"
        );
        interface.engine.post(
            "/request",
            serde_json::json!({"intent":"clearance","text":"Request IFR clearance","role":"atc"}),
        );
        let cleared = wait_reply("/request");
        assert_eq!(cleared["result"]["accepted"], true, "{cleared}");
        assert_eq!(cleared["state"]["telemetry"]["com1Khz"], 120_350);
        interface
            .engine
            .post("/request/auto-reply", serde_json::json!({}));
        let ack = wait_reply("/request/auto-reply");
        assert_eq!(ack["result"]["accepted"], true, "{ack}");
        let end = Instant::now() + Duration::from_secs(3);
        while interface
            .engine
            .state()
            .clearance
            .as_ref()
            .is_none_or(|c| !c.acknowledged)
        {
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(20));
        }
        t.com1_khz = 121_400;
        interface.engine.telemetry(t.clone());
        assert!(!available("clearance"));
        assert!(available("start"));
        assert!(available("taxi"));
        t.engines_running = true;
        interface.engine.telemetry(t);
        assert!(!available("start"));
        assert!(available("taxi"));
        interface.search = "taxi".into();
        interface.radio_tuned(120_350);
        assert!(interface.search.is_empty());
    }

    #[test]
    fn parked_requests_include_radio_and_local_information_before_filing() {
        use openatc_core::state::{FlightPlan, PhaseCode};
        use openatc_core::stations::Station;
        let state = State {
            plan: FlightPlan::default(),
            phase: PhaseCode::Parked,
            telemetry: openatc_core::state::Telemetry {
                on_ground: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let buttons = openatc_core::catalog::buttons();
        for service in ["Clearance", "Ground", "Tower"] {
            let station = Station {
                service: service.into(),
                ..Default::default()
            };
            for intent in [
                "radio_check",
                "altimeter",
                "weather",
                "checkin",
                "frequency",
                "position",
                "emergency",
            ] {
                assert!(
                    buttons.iter().any(|(id, _)| id == intent),
                    "missing {intent} button"
                );
                assert!(
                    quick_request_available(&state, Some(&station), intent),
                    "{service}: {intent}"
                );
            }
            for intent in [
                "repeat", "standby", "unable", "readback", "start", "pushback", "taxi", "ready",
                "landing", "altitude", "direct",
            ] {
                assert!(
                    !quick_request_available(&state, Some(&station), intent),
                    "{service}: {intent}"
                );
            }
        }
        for service in ["ATIS", "Unicom"] {
            let station = Station {
                service: service.into(),
                ..Default::default()
            };
            for intent in ["radio_check", "altimeter", "weather", "checkin"] {
                assert!(!quick_request_available(&state, Some(&station), intent));
            }
        }
        for intent in ["radio_check", "altimeter", "weather", "checkin"] {
            assert!(!quick_request_available(&state, None, intent));
        }
    }

    #[test]
    fn quick_requests_follow_phase_controller_and_combined_duties() {
        use openatc_core::state::{Clearance, PhaseCode};
        use openatc_core::stations::Station;
        let mut state = State::default();
        let delivery = Station {
            service: "Clearance".into(),
            ..Default::default()
        };
        let ground = Station {
            service: "Ground".into(),
            ..Default::default()
        };
        let mut tower = Station {
            service: "Tower".into(),
            ..Default::default()
        };
        assert!(quick_request_available(
            &state,
            Some(&delivery),
            "clearance"
        ));
        assert!(!quick_request_available(&state, Some(&ground), "clearance"));
        assert!(!quick_request_available(&state, None, "radio_check"));
        state.on_event(openatc_core::aircraft::AircraftEvent::IfrClearanceReceived);
        state.clearance = Some(Clearance {
            acknowledged: true,
            ..Default::default()
        });
        assert!(quick_request_available(&state, Some(&ground), "taxi"));
        assert!(!quick_request_available(&state, Some(&delivery), "taxi"));
        assert!(!quick_request_available(&state, Some(&tower), "taxi"));
        tower.services.push("Ground".into());
        assert!(quick_request_available(&state, Some(&tower), "taxi"));
        assert!(quick_request_available(&state, Some(&tower), "checkin"));
        assert!(quick_request_available(&state, Some(&delivery), "checkin"));
        state.on_event(openatc_core::aircraft::AircraftEvent::Cruise);
        state.telemetry.on_ground = false;
        let center = Station {
            service: "Center".into(),
            ..Default::default()
        };
        assert!(quick_request_available(&state, Some(&center), "checkin"));
        assert!(quick_request_available(&state, Some(&center), "altitude"));
        assert!(!quick_request_available(&state, Some(&ground), "checkin"));
        assert!(!quick_request_available(&state, Some(&tower), "altitude"));
        assert!(!quick_request_available(&state, Some(&center), "taxi"));
        let atis = Station {
            service: "ATIS".into(),
            ..Default::default()
        };
        assert!(!quick_request_available(&state, Some(&atis), "radio_check"));
    }
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

    #[test]
    fn settings_tabs_wrap_and_the_last_section_can_be_clicked() {
        let _guard = CONTEXT_LOCK.lock().unwrap();
        let mut ctx = headless();
        let mut selected = 0;
        let mut target = [0.0, 0.0];
        for frame in 0..5 {
            if frame > 0 {
                ctx.io_mut().add_mouse_pos_event(target);
                ctx.io_mut()
                    .add_mouse_button_event(imgui::MouseButton::Left, frame == 2);
            }
            let ui = ctx.frame();
            ui.window("Settings tabs")
                .position([0.0, 0.0], imgui::Condition::Always)
                .size([160.0, 600.0], imgui::Condition::Always)
                .build(|| {
                    let first_y = ui.cursor_screen_pos()[1];
                    widgets::section_tabs(
                        ui,
                        &[
                            "General",
                            "Appearance",
                            "Copilot",
                            "Controls",
                            "AI",
                            "STT / TTS",
                            "Voices",
                            "Audio",
                            "Traffic",
                            "Realism",
                            "Sessions",
                        ],
                        &mut selected,
                    );
                    // The final tab ends just above the separator on its own wrapped row.
                    let size = ui.calc_text_size("Sessions");
                    let style = ui.clone_style();
                    target = [
                        style.window_padding[0] + size[0] * 0.5,
                        ui.item_rect_min()[1]
                            - style.item_spacing[1]
                            - size[1] * 0.5
                            - style.frame_padding[1],
                    ];
                    assert!(target[1] > first_y + size[1] * 2.0);
                });
            ctx.render();
        }
        assert_eq!(selected, 10);
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
                radio: Some(openatc_core::state::Radio::Com1),
                frequency: None,
                gap_ms: 0,
                information_only: false,
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
                radio: Some(openatc_core::state::Radio::Com1),
                frequency: None,
                gap_ms: 0,
                information_only: false,
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
        interface.set_airport(openatc_core::airport::demo_airport());
        interface
    }

    #[test]
    fn manual_collapse_stays_small_under_the_mouse_until_explicitly_expanded() {
        let _guard = CONTEXT_LOCK.lock().unwrap();
        let mut ctx = headless();
        for page in [0, 10] {
            let mut interface = interface_with_airport();
            interface.settings.show_com2 = true;
            interface.set_page(page);
            let mut size = [960.0, 780.0];
            let mut frame = |ctx: &mut imgui::Context, interface: &mut Interface| {
                interface.set_window_frame(crate::interface::WindowFrame {
                    native_bounds: None,
                    position: [0.0, 0.0],
                    size,
                    can_pop_out: true,
                    popped_out: false,
                });
                let actions = interface.draw(ctx.frame());
                ctx.render();
                if let Some(height) = actions.height {
                    size[1] = height;
                }
            };
            for _ in 0..3 {
                frame(&mut ctx, &mut interface);
            }
            let b = interface.collapse_bounds;
            let target = [f32::midpoint(b[0], b[2]), f32::midpoint(b[1], b[3])];
            ctx.io_mut().add_mouse_pos_event(target);
            frame(&mut ctx, &mut interface);
            ctx.io_mut()
                .add_mouse_button_event(imgui::MouseButton::Left, true);
            frame(&mut ctx, &mut interface);
            ctx.io_mut()
                .add_mouse_button_event(imgui::MouseButton::Left, false);
            frame(&mut ctx, &mut interface);
            assert!(interface.compact);
            for _ in 0..10 {
                frame(&mut ctx, &mut interface);
                assert!(
                    interface.compact,
                    "hover or the released button expanded the window"
                );
            }
            for position in [[1100.0, 840.0], target] {
                ctx.io_mut().add_mouse_pos_event(position);
                frame(&mut ctx, &mut interface);
                assert!(interface.compact);
            }
            ctx.io_mut()
                .add_mouse_button_event(imgui::MouseButton::Left, true);
            frame(&mut ctx, &mut interface);
            ctx.io_mut()
                .add_mouse_button_event(imgui::MouseButton::Left, false);
            frame(&mut ctx, &mut interface);
            assert!(!interface.compact);
        }
    }

    #[test]
    fn transcript_resize_keeps_the_latest_message_or_the_reading_position() {
        let _guard = CONTEXT_LOCK.lock().unwrap();
        let mut ctx = headless();
        let mut interface = interface_with_airport();
        let mut state = filled_state();
        state.transcript = (1..=60)
            .map(|sequence| Transmission {
                sequence,
                text: format!("Test Ground, message {sequence} with taxi instructions."),
                ..state.transcript[0].clone()
            })
            .collect();
        let draw = |ctx: &mut imgui::Context, interface: &mut Interface, height: f32| {
            let ui = ctx.frame();
            ui.window("Log resize")
                .position([0.0, 0.0], imgui::Condition::Always)
                .size([800.0, 820.0], imgui::Condition::Always)
                .build(|| transcript_view(ui, interface, &state, height));
            ctx.render();
        };
        for _ in 0..4 {
            draw(&mut ctx, &mut interface, 250.0);
        }
        assert!(interface.transcript_follow_latest);
        assert!(interface.transcript_scroll > 1000.0);
        for compact in [true, false, true, false] {
            interface.compact = compact;
            for height in [750.0, 350.0, 350.0, 350.0] {
                draw(&mut ctx, &mut interface, height);
            }
            assert!(
                interface.transcript_follow_latest,
                "lost the latest message on resize"
            );
            assert!(interface.transcript_scroll > 1000.0);
        }
        ctx.io_mut().add_mouse_pos_event([150.0, 130.0]);
        draw(&mut ctx, &mut interface, 350.0);
        ctx.io_mut().add_mouse_wheel_event([0.0, 4.0]);
        for _ in 0..3 {
            draw(&mut ctx, &mut interface, 350.0);
        }
        assert!(
            !interface.transcript_follow_latest,
            "mouse wheel did not scroll the log"
        );
        let reading = interface.transcript_scroll;
        interface.compact = true;
        for height in [600.0, 250.0, 250.0, 250.0] {
            draw(&mut ctx, &mut interface, height);
        }
        assert!(!interface.transcript_follow_latest);
        assert!((interface.transcript_scroll - reading).abs() < 2.0);
    }

    #[test]
    fn all_speakers_follow_the_bottom_without_interrupting_history() {
        let _guard = CONTEXT_LOCK.lock().unwrap();
        let mut ctx = headless();
        let mut interface = interface_with_airport();
        let mut state = filled_state();
        let template = state.transcript[0].clone();
        state.transcript = (1..=80)
            .map(|sequence| Transmission {
                sequence,
                text: format!("Conversation entry {sequence}."),
                ..template.clone()
            })
            .collect();
        let draw = |ctx: &mut imgui::Context, interface: &mut Interface, state: &State| {
            let ui = ctx.frame();
            ui.window("Log actors")
                .position([0.0, 0.0], imgui::Condition::Always)
                .size([800.0, 600.0], imgui::Condition::Always)
                .build(|| transcript_view(ui, interface, state, 300.0));
            ctx.render();
        };
        for _ in 0..4 {
            draw(&mut ctx, &mut interface, &state);
        }
        assert!(interface.transcript_follow_latest);
        ctx.io_mut().add_mouse_pos_event([150.0, 130.0]);
        draw(&mut ctx, &mut interface, &state);
        ctx.io_mut().add_mouse_wheel_event([0.0, 5.0]);
        for _ in 0..3 {
            draw(&mut ctx, &mut interface, &state);
        }
        assert!(!interface.transcript_follow_latest);
        let history = interface.transcript_scroll;
        let actors = ["ATC", "COPILOT", "ATTENDANT", "GROUND", "TEST1", "ATIS"];
        for actor in actors {
            state.transcript.push(Transmission {
                speaker: actor.into(),
                sequence: state.transcript.last().unwrap().sequence + 1,
                text: format!("New message from {actor}."),
                ..template.clone()
            });
            for _ in 0..4 {
                draw(&mut ctx, &mut interface, &state);
            }
            assert!(
                !interface.transcript_follow_latest,
                "{actor} interrupted history"
            );
            assert!((interface.transcript_scroll - history).abs() < 2.0);
        }
        ctx.io_mut().add_mouse_wheel_event([0.0, -1000.0]);
        for _ in 0..4 {
            draw(&mut ctx, &mut interface, &state);
        }
        assert!(interface.transcript_follow_latest);
        for actor in actors {
            state.transcript.push(Transmission {
                speaker: actor.into(),
                sequence: state.transcript.last().unwrap().sequence + 1,
                text: format!("Following {actor}."),
                ..template.clone()
            });
            for _ in 0..4 {
                draw(&mut ctx, &mut interface, &state);
            }
            assert!(
                interface.transcript_follow_latest,
                "{actor} did not follow the bottom"
            );
        }
        assert_eq!(
            transcript_badge(
                &Transmission {
                    speaker: "ATIS".into(),
                    ..template
                },
                &interface.airport,
                118_700
            ),
            "ATIS"
        );
    }

    #[test]
    fn traffic_pilot_badges_preserve_identity_instead_of_showing_you() {
        let mut entry = openatc_core::state::Transmission {
            speaker: "SIA1975".into(),
            text: "Taxi via Alpha, Singapore one nine seven five.".into(),
            position: "Traffic".into(),
            background: true,
            radio: Some(openatc_core::state::Radio::Com1),
            sequence: 1,
            voice: String::new(),
            delivery: "standard".into(),
            speed: 1.0,
            pilot_reply: String::new(),
            urgent: false,
            information_only: false,
            frequency: None,
            gap_ms: 0,
        };
        let entry = &mut entry;
        let airport = openatc_core::airport::Airport::default();
        assert_eq!(transcript_badge(entry, &airport, 118_700), "TFC · SIA1975");
        entry.speaker = "F-GABC".into();
        assert_eq!(transcript_badge(entry, &airport, 118_700), "TFC · F-GABC");
        entry.position.clear();
        entry.background = false;
        assert_eq!(transcript_badge(entry, &airport, 118_700), "YOU");
    }

    #[test]
    fn header_icons_fit_and_navigate_at_each_supported_scale() {
        let _guard = CONTEXT_LOCK.lock().unwrap();
        let mut ctx = headless();
        let mut interface = interface_with_airport();
        interface.settings.pin_open = true;
        interface.settings.dev_mode = true;
        interface.settings.show_com2 = true;
        ctx.io_mut().display_size = [1920.0, 1440.0];
        for scale in [0.85, 1.0, 1.5] {
            widgets::configure_scale(&mut ctx, scale);
            interface.settings.ui_scale = scale;
            for width in [520.0, 960.0] {
                let position = [25.0, 30.0];
                let size = [width * scale, 780.0 * scale];
                interface.set_window_frame(crate::interface::WindowFrame {
                    native_bounds: None,
                    position,
                    size,
                    can_pop_out: true,
                    popped_out: false,
                });
                for _ in 0..3 {
                    interface.draw(ctx.frame());
                    ctx.render();
                }
                for index in 0..12 {
                    let bounds = interface.toolbar_bounds[index];
                    assert!(
                        bounds[0] >= position[0] && bounds[2] <= position[0] + size[0],
                        "clipped icon {index} at width {width}, scale {scale}: {bounds:?}"
                    );
                    assert!(bounds[1] >= position[1] && bounds[3] <= position[1] + 95.0 * scale);
                    let target = [
                        f32::midpoint(bounds[0], bounds[2]),
                        f32::midpoint(bounds[1], bounds[3]),
                    ];
                    ctx.io_mut().add_mouse_pos_event(target);
                    interface.draw(ctx.frame());
                    ctx.render();
                    ctx.io_mut()
                        .add_mouse_button_event(imgui::MouseButton::Left, true);
                    interface.draw(ctx.frame());
                    ctx.render();
                    ctx.io_mut()
                        .add_mouse_button_event(imgui::MouseButton::Left, false);
                    let actions = interface.draw(ctx.frame());
                    ctx.render();
                    if index == 11 {
                        assert_eq!(interface.page, 10);
                        assert_eq!(interface.active_radio, openatc_core::state::Radio::Com2);
                    } else if index < 10 {
                        assert_eq!(
                            interface.page, index,
                            "navigation icon at width {width}, scale {scale}"
                        );
                    } else {
                        // Pop-out must also work while a page other than ATC is selected.
                        assert!(actions.toggle_pop_out);
                    }
                }
            }
        }
    }

    #[test]
    fn offline_planning_fields_are_editable() {
        let _guard = CONTEXT_LOCK.lock().unwrap();
        let mut ctx = headless();
        let mut interface = interface_with_airport();
        let state = filled_state();
        let mut target = [0.0, 0.0];
        for frame in 0..6 {
            if frame > 1 {
                ctx.io_mut().add_mouse_pos_event(target);
                ctx.io_mut()
                    .add_mouse_button_event(imgui::MouseButton::Left, frame == 3);
            }
            if frame == 5 {
                ctx.io_mut().add_input_character('X');
            }
            let ui = ctx.frame();
            ui.window("Offline planning")
                .position([0.0, 0.0], imgui::Condition::Always)
                .size([900.0, 740.0], imgui::Condition::Always)
                .build(|| {
                    plan_page(ui, &mut interface, &state);
                    let b = interface.plan_callsign_bounds;
                    target = [b[0] + 20.0, f32::midpoint(b[1], b[3])];
                });
            ctx.render();
        }
        assert!(interface.draft.callsign.contains('X'));
    }

    #[test]
    fn planning_keeps_keyboard_capture_through_click_release_and_typing() {
        let _guard = CONTEXT_LOCK.lock().unwrap();
        let mut ctx = headless();
        let mut interface = interface_with_airport();
        interface.set_page(1);
        for _ in 0..3 {
            interface.draw(ctx.frame());
            ctx.render();
        }
        let b = interface.plan_callsign_bounds;
        assert!(b[2] > b[0]);
        ctx.io_mut()
            .add_mouse_pos_event([b[0] + 20.0, f32::midpoint(b[1], b[3])]);
        interface.draw(ctx.frame());
        ctx.render();
        ctx.io_mut()
            .add_mouse_button_event(imgui::MouseButton::Left, true);
        let pressed = interface.draw(ctx.frame());
        ctx.render();
        assert!(pressed.capture_keyboard);
        ctx.io_mut()
            .add_mouse_button_event(imgui::MouseButton::Left, false);
        let released = interface.draw(ctx.frame());
        ctx.render();
        assert!(released.capture_keyboard);
        ctx.io_mut().add_input_character('R');
        let typed = interface.draw(ctx.frame());
        ctx.render();
        assert!(typed.capture_keyboard);
        assert!(interface.draft.callsign.contains('R'));
    }

    #[test]
    fn toolbar_drag_works_in_full_compact_and_wrapped_layouts() {
        let _guard = CONTEXT_LOCK.lock().unwrap();
        for (width, compact) in [(960.0, false), (520.0, false), (520.0, true)] {
            let mut ctx = headless();
            let mut interface = interface_with_airport();
            interface.compact = compact;
            interface.window_size = [width, if compact { 240.0 } else { 760.0 }];
            for _ in 0..3 {
                interface.draw(ctx.frame());
                ctx.render();
            }
            let b = interface.toolbar_drag_bounds;
            assert!(b[2] - b[0] > 30.0);
            let point = [f32::midpoint(b[0], b[2]), f32::midpoint(b[1], b[3])];
            ctx.io_mut().add_mouse_pos_event(point);
            interface.draw(ctx.frame());
            ctx.render();
            ctx.io_mut()
                .add_mouse_button_event(imgui::MouseButton::Left, true);
            interface.draw(ctx.frame());
            ctx.render();
            ctx.io_mut()
                .add_mouse_pos_event([point[0] + 25.0, point[1] + 12.0]);
            let actions = interface.draw(ctx.frame());
            ctx.render();
            assert_eq!(
                actions.drag_delta,
                [25.0, 12.0],
                "width={width}, compact={compact}"
            );
        }
    }

    #[test]
    fn chat_frequency_links_accept_only_decimal_com_frequencies() {
        for (text, expected) in [
            ("118.1", 118_100),
            ("(121.900),", 121_900),
            ("136.975", 136_975),
            ("123.45", 123_450),
        ] {
            assert_eq!(transcript_frequency(text), Some(expected));
        }
        for text in [
            "31",
            "5000",
            "110.50",
            "137.0",
            "118.1000",
            "FL380",
            "118.1x2",
            "17.974573",
        ] {
            assert_eq!(transcript_frequency(text), None, "{text}");
        }
    }

    #[test]
    fn clicking_an_atc_frequency_requests_com1_tuning() {
        let _guard = CONTEXT_LOCK.lock().unwrap();
        for (links, powered, expected) in [
            (true, true, Some(121_900)),
            (true, false, None),
            (false, true, None),
        ] {
            let mut ctx = headless();
            let mut target = [0.0, 0.0];
            let mut chosen = None;
            for frame in 0..3 {
                if frame > 0 {
                    ctx.io_mut().add_mouse_pos_event(target);
                    ctx.io_mut()
                        .add_mouse_button_event(imgui::MouseButton::Left, frame == 2);
                }
                let ui = ctx.frame();
                ui.window("Link test")
                    .position([20.0, 20.0], imgui::Condition::Always)
                    .size([440.0, 180.0], imgui::Condition::Always)
                    .build(|| {
                        let origin = ui.cursor_screen_pos();
                        target = [
                            origin[0] + ui.calc_text_size("Contact ")[0] + 10.0,
                            origin[1] + 5.0,
                        ];
                        let (clicked, _) = linked_transcript_text(
                            ui,
                            "Contact 121.900 for ground.",
                            widgets::accent(ui),
                            links,
                            powered,
                        );
                        if clicked.is_some() {
                            chosen = clicked;
                        }
                    });
                ctx.render();
            }
            assert_eq!(chosen, expected.map(TranscriptAction::Frequency));
        }
    }

    #[test]
    fn clicking_a_squawk_preserves_the_requested_code() {
        let _guard = CONTEXT_LOCK.lock().unwrap();
        for (links, powered, expected) in [
            (true, true, Some(6111)),
            (true, false, None),
            (false, true, Some(6111)),
        ] {
            let mut ctx = headless();
            let mut target = [0.0, 0.0];
            let mut chosen = None;
            for frame in 0..3 {
                if frame > 0 {
                    ctx.io_mut().add_mouse_pos_event(target);
                    ctx.io_mut()
                        .add_mouse_button_event(imgui::MouseButton::Left, frame == 2);
                }
                let ui = ctx.frame();
                ui.window("Link test")
                    .position([20.0, 20.0], imgui::Condition::Always)
                    .size([440.0, 180.0], imgui::Condition::Always)
                    .build(|| {
                        let origin = ui.cursor_screen_pos();
                        target = [
                            origin[0] + ui.calc_text_size("Squawk ")[0] + 10.0,
                            origin[1] + 5.0,
                        ];
                        let (clicked, _) = linked_transcript_text(
                            ui,
                            "Squawk 6111.",
                            widgets::accent(ui),
                            links,
                            powered,
                        );
                        if clicked.is_some() {
                            chosen = clicked;
                        }
                    });
                ctx.render();
            }
            assert_eq!(chosen, expected.map(TranscriptAction::Squawk));
        }
    }

    #[test]
    fn airport_surface_wins_over_nearby_heliport_reference() {
        use openatc_core::state::Point;
        let mut interface = Interface::new("http://127.0.0.1:1");
        interface.current_airport = "57CA".into();
        interface.airport.icao = "KLAX".into();
        interface.airport.pavement_triangles = vec![[
            Point {
                east: -100.0,
                north: -100.0,
                ..Default::default()
            },
            Point {
                east: 100.0,
                north: -100.0,
                ..Default::default()
            },
            Point {
                east: 0.0,
                north: 100.0,
                ..Default::default()
            },
        ]];
        let mut state = State::default();
        state.telemetry.on_ground = true;
        state.telemetry.position_valid = true;
        assert_eq!(default_station_airport(&interface, &state), "KLAX");
        state.telemetry.longitude = 1.0;
        assert_eq!(default_station_airport(&interface, &state), "57CA");
    }
    #[test]
    fn surface_station_selection_uses_airport_instead_of_closest_transmitter() {
        let mut interface = Interface::new("http://127.0.0.1:1");
        interface.current_airport = "KLAX".into();
        let mut state = State::default();
        assert_eq!(default_station_airport(&interface, &state), "KLAX");
        interface.current_airport = "KHHR".into();
        state.telemetry.on_ground = true;
        state.taxi_clearance.airport = "KLAX".into();
        state.taxi_clearance.approved = true;
        assert_eq!(default_station_airport(&interface, &state), "KLAX");
    }
    #[test]
    fn station_list_follows_ground_air_and_destination_without_a_filter() {
        use openatc_core::stations::Station;
        let mut interface = Interface::new("http://127.0.0.1:1");
        interface.current_airport = "PAD".into();
        interface.occupied_airport = "FIELD".into();
        interface.nearby_stations = [
            ("PAD", "Tower", 118_100),
            ("FIELD", "Clearance", 120_350),
            ("FIELD", "Ground", 121_400),
            ("DEST", "Tower", 120_500),
        ]
        .into_iter()
        .map(|(airport, service, khz)| Station {
            airport: airport.into(),
            service: service.into(),
            khz,
            receivable: true,
            ..Default::default()
        })
        .collect();
        let mut state = State::default();
        state.telemetry.position_valid = true;
        let ground = radio_stations(&interface, &state);
        assert_eq!(ground.len(), 2);
        assert!(ground.iter().all(|s| s.airport == "FIELD"));
        state.telemetry.on_ground = false;
        assert_eq!(radio_stations(&interface, &state).len(), 4);
        state.telemetry.on_ground = true;
        interface.occupied_airport = "DEST".into();
        let landed = radio_stations(&interface, &state);
        assert_eq!(landed.len(), 1);
        assert_eq!(landed[0].airport, "DEST");
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
            crate::traffic::tab,
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
            crate::traffic::tab,
        ] {
            let ui = ctx.frame();
            ui.window("test")
                .size([1200.0, 860.0], imgui::Condition::Always)
                .build(|| {
                    page(ui, &mut interface, &empty);
                });
            let _ = ctx.render();
        }
        for mission_state in [&state, &empty] {
            let ui = ctx.frame();
            ui.window("missions")
                .size([960.0, 760.0], imgui::Condition::Always)
                .build(|| {
                    interface
                        .missions
                        .page(ui, &interface.engine, mission_state);
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
                        native_bounds: None,
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
    fn new_flight_confirmation_stays_in_host_window_and_buttons_work() {
        let _guard = CONTEXT_LOCK.lock().unwrap();
        let mut ctx = headless();
        ctx.io_mut().display_size = [2560.0, 1440.0];
        let mut interface = Interface::new("http://127.0.0.1:1");
        interface.settings.pin_open = true;
        let offset = [60.0, 90.0];
        let size = [560.0, 650.0];
        interface.set_window_frame(crate::interface::WindowFrame {
            native_bounds: None,
            position: offset,
            size,
            can_pop_out: false,
            popped_out: false,
        });
        interface.confirm_reset = true;
        for _ in 0..3 {
            interface.draw(ctx.frame());
            ctx.render();
        }
        for bounds in interface.reset_button_bounds {
            assert!(bounds[0] >= offset[0] && bounds[1] >= offset[1]);
            assert!(bounds[2] <= offset[0] + size[0] && bounds[3] <= offset[1] + size[1]);
            assert!(bounds[2] - bounds[0] > 120.0);
        }
        let click = |ctx: &mut imgui::Context, interface: &mut Interface, index: usize| {
            let b = interface.reset_button_bounds[index];
            ctx.io_mut()
                .add_mouse_pos_event([f32::midpoint(b[0], b[2]), f32::midpoint(b[1], b[3])]);
            interface.draw(ctx.frame());
            ctx.render();
            ctx.io_mut()
                .add_mouse_button_event(imgui::MouseButton::Left, true);
            interface.draw(ctx.frame());
            ctx.render();
            ctx.io_mut()
                .add_mouse_button_event(imgui::MouseButton::Left, false);
            interface.draw(ctx.frame());
            ctx.render();
        };
        click(&mut ctx, &mut interface, 1);
        assert!(!interface.confirm_reset);
        assert!(!interface.reset_pending);
        assert_eq!(interface.flight_reset_generation, 0);
        interface.confirm_reset = true;
        interface.draw(ctx.frame());
        ctx.render();
        click(&mut ctx, &mut interface, 0);
        assert!(interface.reset_pending);
        assert_eq!(interface.flight_reset_generation, 1);
        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            interface.tick();
            if !interface.reset_pending {
                break;
            }
        }
        assert!(!interface.reset_pending);
        assert!(interface.confirm_reset);
        assert!(!interface.reset_error.is_empty());
        interface.draw(ctx.frame());
        ctx.render();
        click(&mut ctx, &mut interface, 0);
        assert!(interface.reset_pending);
        assert_eq!(interface.flight_reset_generation, 2);
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
    #[test]
    fn clearance_button_uses_planning_callsign_before_plan_submission() {
        let mut interface = Interface::new("http://127.0.0.1:1");
        interface.draft.callsign = "C-GTLT".into();
        interface.draft.destination = "KSNA".into();
        let mut state = State::default();
        state.plan.callsign = "VH-BIL".into();
        let station = openatc_core::stations::Station {
            name: "Los Angeles Delivery".into(),
            ..Default::default()
        };
        let text = quick_request_text(
            &interface,
            &state,
            Some(&station),
            "clearance",
            "Request IFR clearance",
        );
        assert!(text.contains("C-GTLT") && text.contains("KSNA"));
        assert!(!text.contains("VH-BIL"));
        let radio_check = quick_request_text(
            &interface,
            &state,
            Some(&station),
            "radio_check",
            "Radio check",
        );
        assert!(radio_check.contains("C-GTLT"));
        assert!(!radio_check.contains("VH-BIL"));
        state.clearance = Some(Clearance::default());
        assert_eq!(interface.transmission_callsign(&state), "VH-BIL");
    }

    #[test]
    fn debug_map_does_not_redraw_shared_polygons_for_each_frequency() {
        let _guard = CONTEXT_LOCK.lock().unwrap();
        let mut ctx = headless();
        let mut interface = interface_with_airport();
        let mut state = State::default();
        state.telemetry.position_valid = true;
        state.telemetry.altitude_feet = 2000.;
        let volume = openatc_core::stations::Coverage {
            floor: 0.,
            ceiling: 5000.,
            points: (0..300)
                .map(|i| {
                    let a = f64::from(i) * std::f64::consts::TAU / 300.;
                    [a.sin() * 0.1, a.cos() * 0.1]
                })
                .collect(),
        };
        interface.map_stations = (0..500)
            .map(|i| openatc_core::stations::Station {
                name: format!("Test controller {i}"),
                khz: 120_000 + i,
                coverage: vec![volume.clone()],
                ..Default::default()
            })
            .collect();
        let ui = ctx.frame();
        ui.window("map test")
            .size([1200., 860.], imgui::Condition::Always)
            .build(|| {
                crate::sectors::page(ui, &mut interface, &state);
            });
        assert!(ctx.render().total_vtx_count < 20000);
    }
}
