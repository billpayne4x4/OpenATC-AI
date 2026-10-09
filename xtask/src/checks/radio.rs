use super::*;
pub(super) fn radio(e: &mut Engine, m: &Model) -> Result {
    e.tune(123450, json!({}))?;
    let stations = e.post("stations/nearby", json!({}))?["stations"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(stations.len(), 12);
    for intent in ["radio_check", "clearance", "emergency"] {
        let v = e.req(intent)?;
        assert_eq!(v["result"]["silent"], true);
        assert_eq!(v["result"]["message"], "");
        assert_eq!(count(&v["state"]), 0);
    }
    e.post("plan", plan())?;
    e.tune(121900, json!({}))?;
    rejected(&e.req("clearance")?);
    e.tune(121800, json!({}))?;
    let mut wrong = plan();
    wrong["departure"] = json!("DEST");
    wrong["destination"] = json!("TEST");
    wrong["route"] = json!("DCT TEST");
    e.post("plan", wrong)?;
    let refused = e.req("clearance")?;
    rejected(&refused);
    contains(&refused, "Your flight departs DEST");
    e.post("plan", plan())?;
    let v = e.req("clearance")?;
    accepted(&v);
    for item in ["VH-BIL", "DEST", "5000", "2105", "09", "DCT DEST"] {
        contains(&v, item);
    }
    assert_eq!(v["state"]["clearance"]["acknowledged"], false);
    e.tune(121900, json!({}))?;
    rejected(&e.req("start")?);
    e.tune(121800, json!({}))?;
    let bad=e.post("request",json!({"intent":"conversation","role":"atc","text":"Victor Hotel Bravo India Lima cleared DEST five hundred feet squawk two one zero five runway zero nine"}))?;
    rejected(&bad);
    assert!(
        bad["result"]["message"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("say again")
    );
    let good=e.post("request",json!({"intent":"conversation","role":"atc","text":"Cleared to DEST, route DCT DEST, initial altitude 5000 feet, squawk 2105, runway 09, VH-BIL"}))?;
    accepted(&good);
    contains(&good, "Custom TOML");
    assert_eq!(good["state"]["clearance"]["acknowledged"], true);
    e.tune(121900, json!({}))?;
    for (intent, field) in [
        ("start", "startupApproved"),
        ("pushback", "pushbackApproved"),
    ] {
        let v = e.req(intent)?;
        accepted(&v);
        assert_eq!(v["state"][field], true);
    }
    accepted(&e.req("start")?);
    let taxi = e.post(
        "request",
        json!({"intent":"conversation","role":"atc","text":"Test Ground, VH-BIL, reqest taxy"}),
    )?;
    accepted(&taxi);
    let path = taxi["state"]["taxiClearance"].clone();
    assert!(path["points"].as_array().unwrap().len() > 1);
    assert_eq!(path["pendingReadback"], true);
    assert_eq!(path["approved"], false);
    assert!(
        !taxi["state"]["transcript"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["speaker"] == "COPILOT")
    );
    let ack = e.post("request/auto-reply", json!({}))?;
    accepted(&ack);
    assert!(
        !ack["result"]["message"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("altitude")
    );
    assert_eq!(e.call("request/auto-reply", Some(json!({})))?.0, 400);
    e.endpoint(121900, &path, 35., 0.)?;
    assert_eq!(e.state()?["taxiClearance"]["guidanceComplete"], true);
    assert!(latest(&e.state()?).contains("118.700"));
    let incoming = json!({"latitude":0,"longitude":-0.02,"altitudeFeet":500,"trackDegrees":90,"onGround":false});
    e.tune(118700, json!({"paused":true,"traffic":[incoming]}))?;
    assert_eq!(e.state()?["phase"], 2);
    e.tune(118700, json!({"paused":false}))?;
    assert_eq!(e.state()?["phase"], 2);
    e.req("ready")?;
    let state = e.state()?;
    assert_eq!(state["phase"], 2);
    assert!(latest(&state).to_lowercase().contains("traffic on final"));
    e.tune(118700, json!({"traffic":[incoming]}))?;
    assert_eq!(count(&e.state()?), count(&state));
    e.tune(118700, json!({"traffic":[]}))?;
    assert_eq!(e.state()?["phase"], 3);
    assert_eq!(e.state()?["taxiClearance"]["approved"], false);
    assert!(latest(&e.state()?).contains("cleared for takeoff"));
    e.tune(118700, json!({"radioPower":false}))?;
    assert_eq!(e.req("radio_check")?["result"]["silent"], true);
    e.tune(118700, json!({"radioPower":true,"latitude":5.0}))?;
    assert_eq!(e.req("emergency")?["result"]["silent"], true);
    e.tune(118000, json!({"latitude":0.003,"longitude":-0.003}))?;
    assert_eq!(e.call("atis", Some(json!({})))?.0, 400);
    let station = stations
        .iter()
        .find(|s| s["airport"] == "TEST" && s["service"] == "ATIS")
        .unwrap();
    let mut weather = json!({"airport":"TEST","latitude":station["latitude"],"longitude":station["longitude"],"sampleAltitudeFeet":station["elevationFeet"],"temperatureC":19,"dewpointC":10,"pressureHpa":1005,"windDegrees":270,"windKnots":17,"visibilityMeters":6500,"clouds":"Broken cloud at 2500 feet above airport","source":"simulator-region"});
    e.post("weather/simulator", weather.clone())?;
    let report = e.post("atis", json!({}))?;
    assert_eq!(report["information"], "Alpha");
    assert!(report["text"].as_str().unwrap().contains("1005"));
    assert!(report["text"].as_str().unwrap().contains("17 knots"));
    assert_eq!(report["source"], "simulator");
    e.post("weather/simulator", weather.clone())?;
    assert_eq!(e.post("atis", json!({}))?["information"], "Alpha");
    weather["pressureHpa"] = json!(1006);
    e.post("weather/simulator", weather.clone())?;
    assert_eq!(e.post("atis", json!({}))?["information"], "Bravo");
    let mut high = weather.clone();
    high["sampleAltitudeFeet"] = json!(10000);
    assert_eq!(e.call("weather/simulator", Some(high))?.0, 400);
    e.tune(121900, json!({}))?;
    assert_eq!(e.call("atis", Some(json!({})))?.0, 400);
    e.tune(118100, json!({}))?;
    assert_eq!(e.call("atis", Some(json!({})))?.0, 400);
    let dest = stations
        .iter()
        .find(|s| s["airport"] == "DEST" && s["service"] == "ATIS")
        .unwrap();
    merge(
        &mut weather,
        json!({"airport":"DEST","latitude":dest["latitude"],"longitude":dest["longitude"],"sampleAltitudeFeet":0,"pressureHpa":1020}),
    );
    e.post("weather/simulator", weather)?;
    assert!(
        e.post("atis", json!({}))?["text"]
            .as_str()
            .unwrap()
            .contains("1020")
    );
    assert_eq!(
        e.post("weather", json!({"stations":"DEST"}))?["source"],
        "simulator"
    );
    e.tune(121800, json!({}))?;
    let pos = e.req("position")?;
    accepted(&pos);
    assert!(
        !pos["result"]["message"]
            .as_str()
            .unwrap()
            .contains("0.003000")
    );
    e.reset()?;
    e.tune(121800, json!({}))?;
    e.post("plan", plan())?;
    e.settings(json!({"aiEnabled":true,"llmPhraseVariety":false,"aiModel":"fake","aiUrl":m.url(),"congestion":"off"}))?;
    let before = m.count();
    let strict = e.req("clearance")?;
    accepted(&strict);
    assert!(
        !strict["result"]["message"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("read back")
    );
    assert_eq!(m.count(), before);
    let mut wordings = std::collections::HashSet::new();
    let mut last = String::new();
    for _ in 0..10 {
        e.reset()?;
        e.tune(121800, json!({}))?;
        e.post("plan", plan())?;
        let v = e.req("clearance")?;
        accepted(&v);
        last = v["result"]["message"].as_str().unwrap().into();
        wordings.insert(last.clone());
    }
    assert_eq!(wordings.len(), 10);
    assert_eq!(m.count(), before);
    let history: Value =
        serde_json::from_str(&fs::read_to_string(e.config.join("phrase-history.json"))?)?;
    assert!(
        history["last"]
            .as_object()
            .unwrap()
            .values()
            .any(|v| v == &last)
    );
    e.reset()?;
    e.tune(121800, json!({}))?;
    e.post("plan", plan())?;
    e.settings(json!({"llmPhraseVariety":true}))?;
    let v = e.req("clearance")?;
    accepted(&v);
    contains(&v, "5000");
    assert!(!v["result"]["message"].as_str().unwrap().contains("99000"));
    let prompt = m.prompt();
    for expected in [
        "Style examples",
        "clearance-delivery controller",
        "current task is clearance",
        &last,
    ] {
        assert!(prompt.contains(expected), "{prompt}");
    }
    e.settings(json!({"ttsUrl":m.url()}))?;
    let phrase = "Position approximately three miles northwest of Vientiane.";
    assert_eq!(
        e.call("speech/speak", Some(json!({"text":phrase,"speaker":"atc"})))?
            .0,
        400
    );
    let log = fs::read_to_string(e.folder.path().join("engine.log"))?;
    assert!(log.contains(phrase) && log.contains("503"));
    e.settings(json!({"aiEnabled":false}))?;
    let c = &v["state"]["clearance"];
    let ack=e.post("request",json!({"intent":"readback","role":"atc","altitudeFeet":5000,"waypoint":"DCT DEST","clearanceSequence":c["sequence"]}))?;
    accepted(&ack);
    contains(&ack, "121.900");
    assert!(
        ack["result"]["message"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("when ready")
    );
    let delivery_voice = voice(&ack["state"]);
    e.tune(121900, json!({}))?;
    let combined = e.req("start_pushback")?;
    accepted(&combined);
    assert_eq!(combined["state"]["startupApproved"], true);
    assert_eq!(combined["state"]["pushbackApproved"], true);
    let ground_voice = voice(&combined["state"]);
    assert_ne!(delivery_voice, ground_voice);
    let directed = combined["state"]["transcript"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|t| t["speaker"] == "ATC" && !t["background"].as_bool().unwrap_or(false))
        .unwrap();
    let acknowledgment = json!({"intent":"acknowledge","role":"atc","text":directed["pilotReply"],"clearanceSequence":directed["sequence"]});
    accepted(&e.post("request", acknowledgment.clone())?);
    rejected(&e.post("request", acknowledgment)?);
    assert_eq!(voice(&e.req("radio_check")?["state"]), ground_voice);
    // Airports without a taxi graph need explicit runway authority and an endpoint check.
    e.reset()?;
    let mut runway_plan = plan();
    runway_plan["departure"] = json!("RWYO");
    e.post("plan", runway_plan)?;
    e.tune(
        122000,
        json!({"latitude":0.0003,"longitude":0.008,"groundSpeedKnots":0}),
    )?;
    clearance(e)?;
    let v = e.req("taxi")?;
    rejected(&v);
    contains(&v, "Tower");
    e.tune(122100, json!({}))?;
    let v = e.req("backtrack")?;
    accepted(&v);
    let taxi = v["state"]["taxiClearance"].clone();
    assert_eq!(taxi["runwayTaxi"], true);
    let ack = e.post("request/auto-reply", json!({}))?;
    accepted(&ack);
    assert!(
        !ack["result"]["message"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("altitude")
    );
    rejected(&e.req("ready")?);
    e.endpoint(122100, &taxi, 0., 0.)?;
    assert_eq!(e.state()?["phase"], 2);
    accepted(&e.req("ready")?);
    assert_eq!(e.state()?["phase"], 3);
    let n = count(&e.state()?);
    for _ in 0..5 {
        e.tune(122100, json!({}))?;
    }
    assert_eq!(count(&e.state()?), n);
    assert!(
        e.state()?["transcript"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["speaker"] == "ATC")
            .all(|t| t["text"].as_str().unwrap().contains("VH-BIL"))
    );
    e.settings(json!({"copilotReplies":true}))?;
    e.post("request/copilot-prepare", json!({}))?;
    e.post("request/copilot-reply", json!({}))?;
    let state = e.state()?;
    assert_eq!(
        state["transcript"].as_array().unwrap().last().unwrap()["speaker"],
        "COPILOT"
    );
    assert!(
        latest(&state)
            .to_lowercase()
            .contains("cleared for takeoff")
    );
    e.settings(json!({"copilotReplies":false}))?;
    let again = e.req("clearance")?;
    rejected(&again);
    assert!(
        !again["result"]["message"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("contact")
    );
    e.tune(
        122100,
        json!({"onGround":false,"altitudeFeet":1500,"groundSpeedKnots":180}),
    )?;
    let checkin = e.post(
        "request",
        json!({"intent":"checkin","role":"atc","text":"Tower, VH-BIL, 1500 feet."}),
    )?;
    accepted(&checkin);
    contains(&checkin, "1500");
    e.tune(123450, json!({}))?;
    let before = m.count();
    assert_eq!(e.req("conversation")?["result"]["silent"], true);
    assert_eq!(m.count(), before);
    e.reset()?;
    e.settings(json!({"copilotReplies":true,"copilotAutoRespond":true,"congestion":"off"}))?;
    e.tune(121800,json!({"latitude":0.003,"longitude":-0.003,"traffic":[],"onGround":true,"groundSpeedKnots":0}))?;
    e.post("plan", plan())?;
    assert_eq!(
        e.req("clearance")?["state"]["clearance"]["acknowledged"],
        false
    );
    e.post("request/copilot-prepare", json!({}))?;
    assert_eq!(e.state()?["clearance"]["acknowledged"], false);
    e.post("request/copilot-reply", json!({}))?;
    assert_eq!(e.state()?["clearance"]["acknowledged"], true);
    e.tune(121900, json!({}))?;
    assert_eq!(
        e.req("taxi")?["state"]["taxiClearance"]["pendingReadback"],
        true
    );
    e.post("request/copilot-prepare", json!({}))?;
    e.post("request/copilot-reply", json!({}))?;
    let state = e.state()?;
    assert_eq!(state["taxiClearance"]["approved"], true);
    assert!(
        !state["transcript"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["background"].as_bool().unwrap_or(false))
    );
    for t in state["transcript"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["speaker"] == "COPILOT")
    {
        assert!(t["position"].as_str().unwrap_or("").is_empty());
        assert!(!t["text"].as_str().unwrap().contains("Read back"));
    }
    crossing(e)?;
    let previous = e.state()?;
    let reset = e.reset()?;
    assert_eq!(count(&reset), 0);
    assert_eq!(reset["plan"]["departure"], "");
    assert_eq!(reset["plan"]["destination"], "");
    assert!(reset["clearance"].is_null());
    assert_eq!(reset["taxiClearance"]["approved"], false);
    assert!(
        reset["taxiClearance"]["points"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(reset["crewActions"].as_array().unwrap().is_empty());
    assert_eq!(reset["nextSequence"], previous["nextSequence"]);
    assert_eq!(
        reset["telemetry"]["latitude"],
        previous["telemetry"]["latitude"]
    );
    assert_eq!(count(&e.reset()?), 0);
    // A late model result must not restore a conversation from before reset.
    e.settings(json!({"aiEnabled":true,"llmPhraseVariety":true}))?;
    e.tune(
        121800,
        json!({"latitude":0.003,"longitude":-0.003,"traffic":[],"groundSpeedKnots":0}),
    )?;
    e.post("plan", plan())?;
    m.slow.store(true, Ordering::SeqCst);
    let client = e.client.clone();
    let url = e.url.clone();
    let worker = thread::spawn(move || {
        client
            .post(format!("{url}/request"))
            .json(&json!({"intent":"clearance","role":"atc","text":"Request IFR clearance"}))
            .send()
            .map(|v| v.status().as_u16())
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !m.started.load(Ordering::SeqCst) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(m.started.load(Ordering::SeqCst));
    assert_eq!(count(&e.reset()?), 0);
    m.release.store(true, Ordering::SeqCst);
    assert_eq!(worker.join().unwrap()?, 400);
    assert_eq!(count(&e.state()?), 0);
    let roster = fs::read_to_string(e.config.join("controllers.json"))?;
    e.settings(json!({"aiEnabled":false}))?;
    e.restart()?;
    e.tune(
        121900,
        json!({"latitude":0.003,"longitude":-0.003,"traffic":[]}),
    )?;
    e.post("plan", plan())?;
    e.post("stations/nearby", json!({}))?;
    assert_eq!(voice(&e.req("radio_check")?["state"]), ground_voice);
    assert_eq!(
        fs::read_to_string(e.config.join("controllers.json"))?,
        roster
    );
    for (path, body) in [
        ("simbrief", json!({"userid":"bad&userid=1"})),
        ("weather", json!({"stations":"YMLT&format=xml"})),
    ] {
        assert_eq!(e.call(path, Some(body))?.0, 400);
    }
    let mut settings = e.state()?["settings"].clone();
    settings["masterVolume"] = json!(-1);
    assert_eq!(e.call("settings", Some(settings))?.0, 400);
    Ok(())
}
fn voice(state: &Value) -> String {
    state["transcript"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|t| t["speaker"] == "ATC")
        .unwrap()["voice"]
        .as_str()
        .unwrap()
        .into()
}
fn crossing(e: &mut Engine) -> Result {
    e.reset()?;
    e.settings(json!({"copilotReplies":false,"copilotAutoRespond":false}))?;
    e.tune(
        124500,
        json!({"latitude":0.003,"longitude":0.002,"traffic":[],"groundSpeedKnots":0}),
    )?;
    let mut p = plan();
    p["departure"] = json!("PARA");
    p["runway"] = json!("25L");
    e.post("plan", p)?;
    clearance(e)?;
    let taxi = e.req("taxi")?;
    accepted(&taxi);
    assert_eq!(taxi["state"]["taxiClearance"]["holdShortRunway"], "25R");
    accepted(&e.post("request/auto-reply", json!({}))?);
    let n = count(&e.state()?);
    e.tune(
        124500,
        json!({"latitude":0.001,"groundSpeedKnots":8,"radioBusy":true}),
    )?;
    assert_eq!(count(&e.state()?), n);
    e.tune(124500, json!({"radioBusy":false,"radioSequenceSeen":0}))?;
    assert_eq!(count(&e.state()?), n);
    e.tune(
        124500,
        json!({"latitude":0.001,"traffic":null,"radioSequenceSeen":null}),
    )?;
    assert_eq!(e.state()?["taxiClearance"]["waitingForTraffic"], true);
    e.tune(124500,json!({"traffic":[{"latitude":0,"longitude":0.002,"altitudeFeet":0,"onGround":true,"trackDegrees":270,"groundSpeedKnots":5}]}))?;
    assert_eq!(e.state()?["taxiClearance"]["crossingRunway"], "");
    e.tune(124500, json!({"traffic":[],"groundSpeedKnots":8}))?;
    let state = e.state()?;
    assert_eq!(state["taxiClearance"]["crossingRunway"], "25R");
    assert_eq!(state["taxiClearance"]["destination"], "25L");
    assert_eq!(state["taxiClearance"]["pendingReadback"], true);
    rejected(&e.req("ready")?);
    let n = e.state()?["transcript"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["speaker"] == "ATC")
        .count();
    let ack = e.post("request/auto-reply", json!({}))?;
    accepted(&ack);
    assert_eq!(ack["result"]["message"], "");
    assert_eq!(
        ack["state"]["transcript"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["speaker"] == "ATC")
            .count(),
        n
    );
    e.tune(124500, json!({"latitude":0}))?;
    assert_eq!(e.state()?["taxiClearance"]["crossingRunway"], "25R");
    e.tune(124500, json!({"latitude":-0.0012,"groundSpeedKnots":8}))?;
    let onward = e.state()?["taxiClearance"].clone();
    assert_eq!(onward["crossingRunway"], "");
    assert_eq!(onward["holdShortRunway"], "25L");
    assert_eq!(onward["pendingReadback"], true);
    let vacated = e.post(
        "request",
        json!({"intent":"conversation","role":"atc","text":"VACTED"}),
    )?;
    assert_eq!(vacated["result"]["message"], onward["instructions"]);
    assert!(
        !vacated["result"]["message"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("readback correct")
    );
    accepted(&e.post("request/auto-reply", json!({}))?);
    e.endpoint(124500, &onward, 0., 0.)?;
    let state = e.state()?;
    assert_eq!(state["taxiClearance"]["guidanceComplete"], true);
    assert!(
        state["transcript"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["text"].as_str().unwrap().contains("124.600"))
    );
    for t in state["transcript"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["speaker"] == "ATC")
    {
        assert!(
            ["alloy", "echo", "fable", "onyx", "nova", "shimmer"]
                .contains(&t["voice"].as_str().unwrap())
        );
    }
    Ok(())
}
