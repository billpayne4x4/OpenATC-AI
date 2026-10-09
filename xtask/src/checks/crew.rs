use super::*;
struct Crew<'a> {
    engine: &'a Engine,
    profile: Value,
    values: Value,
}
impl Crew<'_> {
    fn observe(&self, aircraft: &str) -> Result<Value> {
        self.engine.post("crew/observe",json!({"aircraft":aircraft,"profile":self.profile,"values":self.values,"available":self.profile["controls"].as_object().unwrap().keys().collect::<Vec<_>>()}))
    }
    fn request(&self, role: &str, text: &str) -> Result<Value> {
        self.observe("ToLiss neo")?;
        self.engine.post(
            "request",
            json!({"role":role,"intent":"conversation","text":text}),
        )
    }
    fn ack(&self, task: &Value, success: bool) -> Result<Value> {
        self.engine.post("crew/ack",json!({"sequence":task["sequence"],"aircraft":task["aircraft"],"success":success,"detail":"test simulator confirmation"}))
    }
    fn action(&mut self, role: &str, text: &str, control: &str, value: f64) -> Result<Value> {
        let v = self.request(role, text)?;
        assert_eq!(v["result"]["pending"], true, "{v}");
        let tasks = v["state"]["crewActions"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["control"], control);
        assert_eq!(tasks[0]["value"].as_f64().unwrap(), value);
        let done = self.ack(&tasks[0], true)?;
        assert_eq!(done["accepted"], true);
        let state = self.engine.state()?;
        let last = state["transcript"].as_array().unwrap().last().unwrap();
        assert!(["COPILOT", "ATTENDANT", "GROUND"].contains(&last["speaker"].as_str().unwrap()));
        assert_eq!(self.ack(&tasks[0], true)?["accepted"], false);
        assert_eq!(count(&self.engine.state()?), count(&state));
        self.values[control] = json!(value);
        Ok(done)
    }
    fn denied(&self, role: &str, text: &str) -> Result {
        let v = self.request(role, text)?;
        rejected(&v);
        assert!(
            v["state"]["crewActions"].as_array().unwrap().is_empty(),
            "{v}"
        );
        Ok(())
    }
}
pub(super) fn crew(e: &mut Engine, m: &Model) -> Result {
    let profile: toml::Value = toml::from_str(&fs::read_to_string(
        repo().join("aircraft/toliss_a320.toml"),
    )?)?;
    let profile = serde_json::to_value(&profile["aircraft"]["crew"])?;
    e.tune(
        118100,
        json!({"latitude":17.974,"longitude":102.57,"groundSpeedKnots":0}),
    )?;
    let mut c = Crew {
        engine: e,
        profile,
        values: json!({"heading":0,"altitude":5000,"gear":1,"beacon":0,"seatbelts":0,"slides":0,"cabin_brightness":100,"chocks":0}),
    };
    for (role, text, control, value) in [
        ("copilot", "set heading 270", "heading", 270.),
        ("copilot", "set heading 360", "heading", 0.),
        (
            "copilot",
            "set altitude flight level 240",
            "altitude",
            24000.,
        ),
        ("ground", "add chocks", "chocks", 1.),
        ("ground", "remove chocks", "chocks", 0.),
        ("ground", "Remove chalks.", "chocks", 0.),
    ] {
        c.action(role, text, control, value)?;
    }
    let v = c.request("ground", "remove chocks and external power")?;
    let tasks = v["state"]["crewActions"].as_array().unwrap();
    assert_eq!(tasks.len(), 2);
    assert!(tasks.iter().all(|t| t["value"].as_f64() == Some(0.)));
    assert!(
        tasks.iter().any(|t| t["control"] == "chocks")
            && tasks.iter().any(|t| t["control"] == "external_power")
    );
    for (i, task) in tasks.iter().enumerate() {
        assert_eq!(
            c.ack(task, true)?["state"]["crewActions"]
                .as_array()
                .unwrap()
                .len(),
            1 - i
        );
    }
    c.denied("ground", "remove chocks and connect imaginary thing")?;
    c.action(
        "cabin",
        "cabin brightness 50 percent",
        "cabin_brightness",
        50.,
    )?;
    let armed = c.action("cabin", "arm slides and crosscheck", "slides", 1.)?;
    assert!(
        latest(&armed["state"])
            .to_lowercase()
            .contains("armed and cross-checked")
    );
    let query = c.request("cabin", "crosscheck")?;
    accepted(&query);
    assert!(query["state"]["crewActions"].as_array().unwrap().is_empty());
    c.action("cabin", "doors to manual", "slides", 0.)?;
    for text in ["is beacon on?", "don't turn beacon on"] {
        assert!(
            c.request("copilot", text)?["state"]["crewActions"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    for (role, text) in [
        ("copilot", "gear up"),
        ("copilot", "set heading 999"),
        ("cabin", "set heading 270"),
    ] {
        c.denied(role, text)?;
    }
    let started = c.request("copilot", "read before start checklist")?;
    contains(&started, "Seat belt");
    rejected(&c.request("copilot", "on")?);
    c.values["seatbelts"] = json!(1);
    contains(&c.request("copilot", "on")?, "Beacon");
    c.values["beacon"] = json!(1);
    contains(&c.request("copilot", "set")?, "complete");
    c.values["seatbelts"] = json!(0);
    c.values["beacon"] = json!(0);
    let start = c.request("copilot", "perform before start checklist")?;
    let task = &start["state"]["crewActions"][0];
    assert_eq!(task["control"], "seatbelts");
    let next = c.ack(task, true)?;
    let task = &next["state"]["crewActions"][0];
    assert_eq!(task["control"], "beacon");
    let failed = c.ack(task, false)?;
    assert!(
        failed["state"]["crewActions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(latest(&failed["state"]).to_lowercase().contains("not"));
    let button = c.profile["controls"]
        .as_object()
        .unwrap()
        .values()
        .find(|v| v["momentary"].as_bool() == Some(true))
        .unwrap();
    let text = format!(
        "please {}",
        button["label"].as_str().unwrap().to_lowercase()
    );
    let v = c.request("copilot", &text)?;
    let task = &v["state"]["crewActions"][0];
    assert!(
        latest(&c.ack(task, true)?["state"])
            .to_lowercase()
            .contains("button pressed")
    );
    let hello = c.request("cabin", "How are you?")?;
    accepted(&hello);
    assert!(
        !hello["result"]["message"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("landing")
    );
    c.action("ground", "start pushback", "pushback_start", 1.)?;
    for text in [
        "don't start pushback",
        "stop pushback",
        "can you start pushback?",
    ] {
        assert!(
            c.request("ground", text)?["state"]["crewActions"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    c.action(
        "ground",
        "request pushback 80 meters 60 degrees right",
        "pushback_start",
        1.,
    )?;
    c.denied("ground", "connect high pressure air")?;
    m.set(json!({"control":"heading","value":275}));
    e.settings(json!({"aiEnabled":true,"aiModel":"test","aiUrl":m.url()}))?;
    c.action(
        "copilot",
        "please choose heading two seven five",
        "heading",
        275.,
    )?;
    assert!(m.prompt().contains("Pilot wording examples"));
    m.set(
        json!({"actions":[{"control":"chocks","value":0},{"control":"external_power","value":0}]}),
    );
    let typo = c.request("ground", "remuve choks and exernal pwer")?;
    assert_eq!(typo["state"]["crewActions"].as_array().unwrap().len(), 2);
    let failed = c.ack(&typo["state"]["crewActions"][0], false)?;
    assert!(
        failed["state"]["crewActions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    for proposal in [
        json!({"control":"heading","value":999}),
        json!({"control":"slides","value":1}),
        json!({"control":"sim/custom/anything","value":1}),
    ] {
        m.set(proposal);
        c.denied("copilot", "please choose heading two seven five")?;
    }
    e.settings(json!({"aiEnabled":false}))?;
    let queued = c.request("copilot", "beacon on")?;
    let task = &queued["state"]["crewActions"][0];
    let cleared = e.reset()?;
    assert!(cleared["crewActions"].as_array().unwrap().is_empty());
    assert_eq!(count(&cleared), 0);
    assert_eq!(c.ack(task, true)?["accepted"], false);
    c.observe("different aircraft")?;
    assert!(e.state()?["crewActions"].as_array().unwrap().is_empty());
    Ok(())
}
