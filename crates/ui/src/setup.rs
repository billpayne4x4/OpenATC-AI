//! Setup progress shared by the first-run panel and Debug settings.
use crate::interface::Interface;

pub(crate) fn poll(ui: &imgui::Ui, interface: &mut Interface) {
    if let Some(reply) = interface.engine.take_reply("/setup/status") {
        interface.setup_pending = false;
        if reply.success {
            let run = reply.data["setup"]["runId"].as_u64().unwrap_or(0);
            if reply.data["setup"]["errors"]
                .as_array()
                .is_some_and(|errors| !errors.is_empty())
                && interface.setup_status["setup"]["errors"]
                    .as_array()
                    .is_none_or(Vec::is_empty)
            {
                interface.setup_overlay = true;
            }
            if run != interface.setup_seen_run {
                interface.setup_seen_run = run;
                interface.setup_overlay |= should_show(&reply.data);
            }
            interface.setup_status = reply.data;
        } else {
            interface.setup_status["error"] = reply.error.into();
        }
    }
    if let Some(reply) = interface.engine.take_reply("/setup/rebuild") {
        if !reply.success {
            interface.setup_status["error"] = reply.error.into();
            interface.setup_status["setup"]["running"] = false.into();
        }
        interface.setup_checked = -1.0;
    }
    let now = ui.time();
    if interface.engine.connected()
        && (!interface.setup_pending || now - interface.setup_checked > 10.0)
        && (interface.setup_checked < 0.0 || now - interface.setup_checked >= 1.0)
    {
        interface.setup_pending = true;
        interface.setup_checked = now;
        interface
            .engine
            .post("/setup/status", serde_json::json!({}));
    }
}

fn should_show(status: &serde_json::Value) -> bool {
    status["setup"]["firstRun"].as_bool() == Some(true)
        || status["setup"]["reset"].as_bool() == Some(true)
        || status["setup"]["errors"]
            .as_array()
            .is_some_and(|errors| !errors.is_empty())
}

pub(crate) fn controls(ui: &imgui::Ui, interface: &mut Interface) {
    ui.separator();
    ui.text_wrapped("After editing TOML files, use Re-setup to rebuild their cached data. Scenery is cleared and rebuilt as needed. Restart the plugin after completion to load every active policy.");
    let running = interface.setup_status["setup"]["running"]
        .as_bool()
        .unwrap_or(false);
    {
        let _disabled = ui.begin_disabled(!interface.engine.connected() || running);
        if ui.button("Re-setup") {
            interface.setup_status["setup"]["running"] = true.into();
            interface.setup_status["setup"]["phase"] = "Waiting for setup worker".into();
            interface.setup_status["error"] = serde_json::Value::Null;
            interface.setup_overlay = true;
            interface
                .engine
                .post("/setup/rebuild", serde_json::json!({}));
        }
    }
    if !interface.engine.connected() {
        ui.text_wrapped("Connect to the companion engine to re-run setup.");
    }
    if ui.button("Show setup progress") {
        interface.setup_overlay = true;
    }
    details(ui, interface);
}

fn details(ui: &imgui::Ui, interface: &Interface) {
    let status = &interface.setup_status;
    if let Some(phase) = status["setup"]["phase"].as_str() {
        ui.text_wrapped(phase);
    }
    let total = status["setup"]["total"].as_u64().unwrap_or(0);
    let completed = status["setup"]["completed"]
        .as_u64()
        .unwrap_or(0)
        .min(total);
    if total > 0 {
        imgui::ProgressBar::new(completed as f32 / total as f32)
            .size([-1.0, 0.0])
            .overlay_text(format!("{completed} of {total} configuration files"))
            .build(ui);
    }
    if let Some(file) = status["setup"]["currentFile"]
        .as_str()
        .filter(|file| !file.is_empty())
    {
        ui.text_wrapped(file);
    }
    if let Some(errors) = status["setup"]["errors"].as_array() {
        for error in errors {
            if let Some(text) = error.as_str() {
                ui.text_wrapped(text);
            }
        }
    }
    for error in [&status["error"], &status["toml"]["error"]] {
        if let Some(text) = error.as_str().filter(|s| !s.is_empty()) {
            ui.text_wrapped(text);
        }
    }
    ui.text(format!(
        "TOML cache: {} reused | {} parsed | {} saved",
        status["toml"]["hits"].as_u64().unwrap_or(0),
        status["toml"]["misses"].as_u64().unwrap_or(0),
        status["toml"]["writes"].as_u64().unwrap_or(0)
    ));
    if let Some(path) = status["toml"]["path"].as_str() {
        ui.text_wrapped(path);
    }
}

pub(crate) fn overlay(ui: &imgui::Ui, interface: &mut Interface) {
    crate::widgets::section_title(ui, "PLUGIN SETUP");
    let running = interface.setup_status["setup"]["running"]
        .as_bool()
        .unwrap_or(false);
    if !interface.engine.connected() {
        ui.text_wrapped(
            "Waiting for the companion engine. Setup progress will resume when it reconnects.",
        );
    }
    details(ui, interface);
    ui.spacing();
    if running {
        ui.text_wrapped(
            "Preparing the current TOML files. You can keep using the plugin while setup runs.",
        );
    } else if interface.setup_status["setup"]["reset"].as_bool() == Some(true) {
        ui.text_wrapped(
            "Restart the plugin after a successful rebuild to apply all edited policies.",
        );
    }
    if ui.button(if running {
        "Continue in background"
    } else {
        "Continue"
    }) {
        interface.setup_overlay = false;
    }
    ui.spacing();
    crate::traffic::scenery_setup(ui, interface, None);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn first_run_reset_and_failed_setup_are_visible_even_if_the_worker_finished() {
        for flag in ["firstRun", "reset"] {
            let mut status = serde_json::json!({"setup":{"running":false,"errors":[]}});
            status["setup"][flag] = true.into();
            assert!(should_show(&status));
        }
        assert!(should_show(
            &serde_json::json!({"setup":{"errors":["bad TOML"]}})
        ));
        assert!(!should_show(
            &serde_json::json!({"setup":{"running":false,"errors":[]}})
        ));
    }
}
