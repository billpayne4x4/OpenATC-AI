//! Execute profile controls on the simulator thread and confirm their readback.
use openatc_core::crew::{Control, CrewProfile};
use std::{
    collections::BTreeMap,
    ffi::CString,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use xplane::command::{Command, CommandHandler, CommandHandlerResult, RegisteredCommandHandler};

pub struct Runtime {
    aircraft: String,
    last_observation: Option<Instant>,
    last_profile: Option<Instant>,
    pending: Option<(openatc_core::crew::Action, Instant, Option<Instant>)>,
    completed: u32,
    pub role: Arc<Mutex<String>>,
    listeners: Vec<(Command, RegisteredCommandHandler)>,
    available: Vec<String>,
    last_bind: Option<Instant>,
    ack: Option<serde_json::Value>,
    last_ack: Option<Instant>,
}
impl Default for Runtime {
    fn default() -> Self {
        Self {
            aircraft: String::new(),
            last_observation: None,
            last_profile: None,
            pending: None,
            completed: 0,
            role: Arc::new(Mutex::new(String::new())),
            listeners: Vec::new(),
            available: Vec::new(),
            last_bind: None,
            ack: None,
            last_ack: None,
        }
    }
}
struct Listener {
    role: Arc<Mutex<String>>,
    selected: String,
}
impl CommandHandler for Listener {
    fn command_begin(&mut self, _: &mut xplane::XPAPI) -> CommandHandlerResult {
        let mut role = self
            .role
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *role == self.selected {
            role.clear();
        } else {
            role.clone_from(&self.selected);
        }
        CommandHandlerResult::AllowXPlaneProcessing
    }
    fn command_continue(&mut self, _: &mut xplane::XPAPI) -> CommandHandlerResult {
        CommandHandlerResult::AllowXPlaneProcessing
    }
    fn command_end(&mut self, _: &mut xplane::XPAPI) -> CommandHandlerResult {
        CommandHandlerResult::AllowXPlaneProcessing
    }
}
fn ref_for(c: &Control) -> &str {
    if c.readback_ref.is_empty() {
        &c.dataref
    } else {
        &c.readback_ref
    }
}
fn values(name: &str, index: Option<usize>) -> Option<Vec<f64>> {
    let name = CString::new(name).ok()?;
    unsafe {
        let r = xplane_sys::XPLMFindDataRef(name.as_ptr());
        if r.is_null() {
            return None;
        }
        let types = xplane_sys::XPLMGetDataRefTypes(r).0;
        if types & xplane_sys::XPLMDataTypeID::IntArray.0 != 0 {
            let n = xplane_sys::XPLMGetDatavi(r, std::ptr::null_mut(), 0, 0);
            if !(1..=256).contains(&n) {
                return None;
            }
            let mut v = vec![0; n as usize];
            let count = xplane_sys::XPLMGetDatavi(r, v.as_mut_ptr(), 0, n);
            v.truncate(count.max(0) as usize);
            let v = v.into_iter().map(f64::from).collect::<Vec<_>>();
            return select(v, index);
        }
        if types & xplane_sys::XPLMDataTypeID::FloatArray.0 != 0 {
            let n = xplane_sys::XPLMGetDatavf(r, std::ptr::null_mut(), 0, 0);
            if !(1..=256).contains(&n) {
                return None;
            }
            let mut v = vec![0.0; n as usize];
            let count = xplane_sys::XPLMGetDatavf(r, v.as_mut_ptr(), 0, n);
            v.truncate(count.max(0) as usize);
            return select(v.into_iter().map(f64::from).collect(), index);
        }
        if index.is_some() {
            return None;
        }
        let value = if types & xplane_sys::XPLMDataTypeID::Double.0 != 0 {
            xplane_sys::XPLMGetDatad(r)
        } else if types & xplane_sys::XPLMDataTypeID::Float.0 != 0 {
            f64::from(xplane_sys::XPLMGetDataf(r))
        } else if types & xplane_sys::XPLMDataTypeID::Int.0 != 0 {
            f64::from(xplane_sys::XPLMGetDatai(r))
        } else {
            return None;
        };
        value.is_finite().then_some(vec![value])
    }
}
fn select(v: Vec<f64>, index: Option<usize>) -> Option<Vec<f64>> {
    match index {
        Some(i) => v.get(i).copied().map(|n| vec![n]),
        None => Some(v),
    }
}
fn normalized(c: &Control, n: f64) -> f64 {
    if c.readback_positive {
        if n > 0.0 { c.on } else { c.off }
    } else {
        n * c.readback_scale
    }
}
fn confirmed(c: &Control, value: f64) -> bool {
    values(ref_for(c), c.index).is_some_and(|v| {
        !v.is_empty()
            && v.iter()
                .all(|n| (normalized(c, *n) - value).abs() <= c.tolerance.max(0.01))
    })
}
fn write(c: &Control, value: f64, x: &mut xplane::XPAPI) -> Result<(), String> {
    if confirmed(c, value) {
        return Ok(());
    }
    let command = if value == c.off {
        &c.off_command
    } else {
        &c.on_command
    };
    if !command.is_empty() {
        x.command
            .try_find(command)
            .map_err(|_| format!("Missing command {command}"))?
            .trigger();
        return Ok(());
    }
    let value = value * c.write_scale;
    let name = CString::new(c.dataref.clone()).map_err(|e| e.to_string())?;
    unsafe {
        let r = xplane_sys::XPLMFindDataRef(name.as_ptr());
        if r.is_null() {
            return Err(format!("Missing dataref {}", c.dataref));
        }
        if xplane_sys::XPLMCanWriteDataRef(r) == 0 {
            return Err(format!("Read-only dataref {}", c.dataref));
        }
        let types = xplane_sys::XPLMGetDataRefTypes(r).0;
        if let Some(index) = c.index {
            let count = if types & xplane_sys::XPLMDataTypeID::IntArray.0 != 0 {
                xplane_sys::XPLMGetDatavi(r, std::ptr::null_mut(), 0, 0)
            } else {
                xplane_sys::XPLMGetDatavf(r, std::ptr::null_mut(), 0, 0)
            };
            if index >= count.max(0) as usize {
                return Err("Array index outside control".into());
            }
            if types & xplane_sys::XPLMDataTypeID::IntArray.0 != 0 {
                let mut v = value as i32;
                xplane_sys::XPLMSetDatavi(r, &mut v, index as i32, 1);
            } else if types & xplane_sys::XPLMDataTypeID::FloatArray.0 != 0 {
                let mut v = value as f32;
                xplane_sys::XPLMSetDatavf(r, &mut v, index as i32, 1);
            } else {
                return Err("Unsupported array control".into());
            }
        } else if types & xplane_sys::XPLMDataTypeID::Double.0 != 0 {
            xplane_sys::XPLMSetDatad(r, value);
        } else if types & xplane_sys::XPLMDataTypeID::Float.0 != 0 {
            xplane_sys::XPLMSetDataf(r, value as f32);
        } else if types & xplane_sys::XPLMDataTypeID::Int.0 != 0 {
            xplane_sys::XPLMSetDatai(r, value as i32);
        } else {
            return Err("Map an array index or a command for this control".into());
        }
    }
    Ok(())
}
/// Standard mappings adapt to the loaded aircraft; custom TOML entries take precedence.
pub fn aircraft_profile(custom: Option<&CrewProfile>) -> CrewProfile {
    let mut profile = custom
        .cloned()
        .unwrap_or_else(openatc_core::crew::standard_profile);
    if let Some(flaps) = profile.controls.get_mut("flaps")
        && flaps.label == "Flap lever percent"
        && let Some(count) = values("sim/aircraft/controls/acf_flap_detents", None)
            .and_then(|v| v.first().copied())
            .filter(|v| *v >= 1.0 && *v <= 9.0)
    {
        flaps.label = "Flap lever detent".into();
        flaps.max = count;
        flaps.on = count;
        flaps.write_scale = 1.0 / count;
        flaps.readback_scale = count;
        flaps.integer = true;
        flaps.states.insert("full".into(), count);
        for (name, value) in [("one", 1.0), ("two", 2.0), ("three", 3.0), ("four", 4.0)] {
            if value <= count {
                flaps.states.insert(name.into(), value);
            }
        }
    }
    if values("sim/aircraft/gear/acf_gear_retract", None).is_some_and(|v| v.first() == Some(&0.0)) {
        profile.controls.retain(|id, _| {
            id != "gear" && !id.starts_with("stock_sim_flight_controls_landing_gear")
        });
    }
    profile
}
impl Runtime {
    pub fn bind(&mut self, x: &mut xplane::XPAPI, aircraft: &str, profile: &CrewProfile) {
        let changed = self.aircraft != aircraft;
        if !changed
            && self
                .last_bind
                .is_some_and(|t| t.elapsed() < Duration::from_secs(2))
        {
            return;
        }
        if !changed
            && self.listeners.len()
                == profile.cabin_commands.len()
                    + profile.ground_commands.len()
                    + profile.radio_commands.len()
            && self.available.len() == profile.controls.len()
        {
            return;
        }
        self.last_bind = Some(Instant::now());
        self.listeners.clear();
        self.aircraft = aircraft.into();
        if changed {
            self.pending = None;
            self.completed = 0;
            self.ack = None;
            self.last_observation = None;
            self.last_profile = None;
            self.role
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clear();
        }
        self.available = profile
            .controls
            .iter()
            .filter_map(|(id, c)| {
                let ok = if !c.on_command.is_empty() {
                    x.command.try_find(&c.on_command).is_ok()
                        && (c.off_command.is_empty() || x.command.try_find(&c.off_command).is_ok())
                } else {
                    CString::new(c.dataref.clone())
                        .ok()
                        .is_some_and(|name| unsafe {
                            let r = xplane_sys::XPLMFindDataRef(name.as_ptr());
                            !r.is_null() && xplane_sys::XPLMCanWriteDataRef(r) != 0
                        })
                };
                ok.then(|| id.clone())
            })
            .collect();
        crate::debug_log(&format!(
            "OpenATC AI: {} of {} configured crew controls resolved",
            self.available.len(),
            profile.controls.len()
        ));
        for (names, selected) in [
            (&profile.cabin_commands, "cabin"),
            (&profile.ground_commands, "ground"),
            (&profile.radio_commands, ""),
        ] {
            for name in names {
                if let Ok(mut command) = x.command.try_find(name) {
                    let handler = command.handle(
                        Listener {
                            role: self.role.clone(),
                            selected: selected.into(),
                        },
                        false,
                    );
                    self.listeners.push((command, handler));
                } else {
                    crate::debug_log(&format!("OpenATC AI: crew routing command {name} missing"));
                }
            }
        }
    }
    pub fn tick(
        &mut self,
        x: &mut xplane::XPAPI,
        profile: &CrewProfile,
        client: &openatc_ui::client::EngineClient,
    ) {
        if self
            .last_observation
            .is_none_or(|t| t.elapsed() > Duration::from_millis(500))
        {
            let send_profile = self
                .last_profile
                .is_none_or(|t| t.elapsed() > Duration::from_secs(5));
            let readings = profile
                .controls
                .iter()
                .filter_map(|(id, c)| {
                    let v = values(ref_for(c), c.index)?;
                    let first = normalized(c, *v.first()?);
                    v.iter()
                        .all(|n| n.is_finite() && (normalized(c, *n) - first).abs() < 0.01)
                        .then(|| (id.clone(), first))
                })
                .collect::<BTreeMap<_, _>>();
            client.post(
                "/crew/observe",
                serde_json::json!({"aircraft":self.aircraft,"profile":if send_profile {Some(profile)}else{None},"values":readings,"available":self.available}),
            );
            self.last_observation = Some(Instant::now());
            if send_profile {
                self.last_profile = Some(Instant::now());
            }
        }
        if let Some(ack) = self.ack.as_ref() {
            let active = client.state().crew_actions.first().is_some_and(|a| {
                Some(u64::from(a.sequence))
                    == ack.get("sequence").and_then(serde_json::Value::as_u64)
                    && ack.get("aircraft").and_then(serde_json::Value::as_str)
                        == Some(a.aircraft.as_str())
            });
            if active {
                if self
                    .last_ack
                    .is_none_or(|t| t.elapsed() > Duration::from_secs(1))
                {
                    client.post("/crew/ack", ack.clone());
                    self.last_ack = Some(Instant::now());
                }
                return;
            }
            self.ack = None;
        }
        if let Some((action, time, stable)) = self.pending.as_mut() {
            let c = profile.controls.get(&action.control);
            let matches = c.is_some_and(|c| confirmed(c, action.value));
            if !matches {
                *stable = None;
            }
            let ok = matches
                && stable.get_or_insert_with(Instant::now).elapsed() >= Duration::from_millis(750);
            if ok || time.elapsed() > Duration::from_secs(3) {
                let ack = serde_json::json!({"sequence":action.sequence,"aircraft":action.aircraft,"success":ok,"detail":"Control readback after execution"});
                client.post("/crew/ack", ack.clone());
                self.ack = Some(ack);
                self.last_ack = Some(Instant::now());
                self.pending = None;
            }
            return;
        }
        let state = client.state();
        let Some(action) = state
            .crew_actions
            .first()
            .filter(|a| a.aircraft == self.aircraft && a.sequence != self.completed)
            .cloned()
        else {
            return;
        };
        self.completed = action.sequence;
        let c = profile.controls.get(&action.control);
        let t = &state.telemetry;
        let valid = c.is_some_and(|c| {
            openatc_core::crew::validate(profile, &action.role, &action.control, action.value)
                && !t.paused
                && t.position_valid
                && (!c.ground_only || t.on_ground && t.ground_speed_knots <= 0.5)
                && (!c.airborne_only || !t.on_ground)
                && !(action.control == "gear" && action.value == c.off && t.on_ground)
        });
        let result = if valid {
            write(c.unwrap(), action.value, x)
        } else {
            Err("Control is unavailable in the current aircraft state".into())
        };
        match result {
            Ok(()) if c.is_some_and(|c| c.momentary) => {
                let ack = serde_json::json!({"sequence":action.sequence,"aircraft":action.aircraft,"success":true,"detail":"Button command dispatched"});
                client.post("/crew/ack", ack.clone());
                self.ack = Some(ack);
                self.last_ack = Some(Instant::now());
            }
            Ok(()) => self.pending = Some((action, Instant::now(), None)),
            Err(detail) => {
                crate::debug_log(&format!("OpenATC AI crew action: {detail}"));
                let ack = serde_json::json!({"sequence":action.sequence,"aircraft":action.aircraft,"success":false,"detail":detail});
                client.post("/crew/ack", ack.clone());
                self.ack = Some(ack);
                self.last_ack = Some(Instant::now());
            }
        }
    }
}
