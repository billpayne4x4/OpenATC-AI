//! Fly telemetry through the real HTTP engine, including arrival runway crossings.
use super::*;

fn require(condition: bool, message: &str) -> Result {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn reply(e: &mut Engine) -> Result {
    let before = e.state()?;
    e.post("request/copilot-prepare", json!({}))?;
    let prepared = e.state()?;
    let prepared_sequence =
        prepared["transcript"].as_array().unwrap().last().unwrap()["sequence"].clone();
    e.post("request/copilot-prepare", json!({}))?;
    require(
        count(&e.state()?) == count(&prepared),
        "Retry duplicated the prepared copilot transmission",
    )?;
    require(
        prepared["transcript"].as_array().unwrap().last().unwrap()["speaker"] == "COPILOT",
        "Prepared reply is not attributed to copilot",
    )?;
    require(
        before["taxiClearance"] == prepared["taxiClearance"],
        "Preparing speech prematurely changed taxi authority",
    )?;
    let frequency = e.frequency();
    e.tune(frequency, json!({"radioBusy":true}))?;
    require(
        count(&e.state()?) == count(&prepared),
        "ATC interrupted copilot playback",
    )?;
    let result = e.post(
        "request/copilot-reply",
        json!({"sequence":prepared_sequence}),
    )?;
    let recorded = result["state"]["transcript"].as_array().unwrap();
    require(
        !recorded
            .iter()
            .skip(count(&prepared))
            .any(|entry| entry["speaker"] == "C-GTLT"),
        "Copilot reply was duplicated as a pilot transmission",
    )?;
    require(
        result["result"]["accepted"] == true,
        &format!("Readback refused: {}", result["result"]),
    )?;
    if before["phase"] != 1
        && (before["taxiClearance"]["pendingReadback"] == true
            || before["clearance"]["acknowledged"] == false)
    {
        require(
            !result["result"]["message"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "Routine readback was not acknowledged",
        )?;
        require(
            recorded.len() == count(&prepared) + 1,
            "Routine readback did not add one controller acknowledgement",
        )?;
        require(
            recorded
                .last()
                .is_some_and(|entry| entry["speaker"] == "ATC" && entry["informationOnly"] == true),
            "Readback acknowledgement must be information-only",
        )?;
    }
    e.telemetry["radioBusy"] = json!(false);
    Ok(())
}

fn checkin(e: &mut Engine, frequency: u32) -> Result {
    e.tune(frequency, json!({"radioBusy":false}))?;
    e.post("request/copilot-checkin", json!({}))?;
    let prepared = e.state()?;
    let result = e.post("request/copilot-reply", json!({}))?;
    require(
        !result["state"]["transcript"]
            .as_array()
            .unwrap()
            .iter()
            .skip(count(&prepared))
            .any(|entry| entry["speaker"] == "C-GTLT"),
        "Copilot check-in duplicated the pilot transcript",
    )?;
    require(
        result["result"]["accepted"] == true,
        "New controller rejected check-in",
    )?;
    if result["state"]["transcript"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["pilotReply"]
        .as_str()
        .is_some_and(|s| !s.is_empty())
    {
        reply(e)?;
    }
    Ok(())
}

fn ticks(e: &mut Engine, frequency: u32, changes: Value, n: usize) -> Result {
    for _ in 0..n {
        e.tune(frequency, changes.clone())?;
    }
    Ok(())
}

fn handoff(e: &mut Engine, from: u32, to: u32, changes: Value) -> Result {
    ticks(e, from, changes, 6)?;
    let state = e.state()?;
    require(
        state["recommendedFrequencyKhz"] == to,
        &format!(
            "Expected handoff {from} -> {to}, got {}: {}",
            state["recommendedFrequencyKhz"],
            latest(&state)
        ),
    )?;
    require(
        latest(&state).contains("C-GTLT"),
        "Handoff omitted the callsign",
    )?;
    reply(e)?;
    checkin(e, to)
}

fn record(e: &Engine, steps: &mut Vec<Value>, name: &str) -> Result {
    let s = e.state()?;
    steps.push(json!({"step":name,"phase":s["phase"],"telemetry":s["telemetry"],"taxiClearance":s["taxiClearance"],"lastTransmission":latest(&s)}));
    println!("  {name}");
    Ok(())
}

fn run(e: &mut Engine, steps: &mut Vec<Value>, variant: &str) -> Result {
    let ground = if variant == "tower-only" {
        118700
    } else {
        121900
    };
    let delivery = if variant == "separate" {
        121800
    } else {
        ground
    };
    let arrival_ground = if variant == "tower-only" {
        118900
    } else {
        121600
    };
    e.reset()?;
    e.settings(json!({"copilotReplies":true,"copilotAutoRespond":true,"aiEnabled":false,"congestion":"off","strictReadbacks":true,"monitorRunwayPermissions":true,"monitorTaxiRoute":true,"monitorTransponder":true}))?;
    e.tune(delivery, json!({"latitude":0.006,"longitude":0.002,"groundSpeedKnots":0,"onGround":true,"traffic":[],"enginesRunning":false,"transponderCode":2105,"transponderMode":3,"aircraftIcao":"A320","wingspanMeters":35.8}))?;
    let mut p = plan();
    merge(
        &mut p,
        json!({"callsign":"C-GTLT","runway":"09R","arrivalRunway":"09R","arrivalStand":"Gate C1","approach":"I09R","cruiseFeet":5000,
            "star":"TEST1","fixes":[{"identifier":"TEST","latitude":0.0,"longitude":0.0},
                {"identifier":"DEPART","latitude":0.0,"longitude":0.4,"pathTerminator":"CF"},
                {"identifier":"ENROUTE","latitude":0.0,"longitude":1.0,"pathTerminator":"TF"},
                {"identifier":"ENTRY","latitude":0.0,"longitude":3.0},
                {"identifier":"DEST","latitude":0.0,"longitude":4.0}]}),
    );
    e.post("plan", p)?;
    if variant == "separate" {
        let source = include_str!("../../../missions/coastal-medical.toml")
            .replace("id = \"coastal-medical\"", "id = \"flight-replay-medical\"")
            .replace("delay_seconds = 20", "delay_seconds = 0");
        e.post("missions/save", json!({"source":source}))?;
        let list = e.post("missions/list", json!({}))?;
        require(
            list["missions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["id"] == "flight-replay-medical"),
            "Saved mission not listed",
        )?;
        e.post("missions/start", json!({"id":"flight-replay-medical"}))?;
    }
    accepted(&e.req("clearance")?);
    reply(e)?;
    record(e, steps, "IFR clearance and readback")?;
    checkin(e, ground)?;
    accepted(&e.req("start_pushback")?);
    reply(e)?;
    e.tune(ground, json!({"enginesRunning":true}))?;
    rejected(&e.req("start")?);
    accepted(&e.req("taxi")?);
    require(
        e.state()?["taxiClearance"]["holdShortRunway"] == "09L",
        "Departure taxi missed intervening runway",
    )?;
    reply(e)?;
    record(e, steps, "Start-up, pushback and taxi to first crossing")?;
    e.tune(
        ground,
        json!({"latitude":0.004,"groundSpeedKnots":8,"traffic":null}),
    )?;
    require(
        e.state()?["taxiClearance"]["waitingForTraffic"] == true,
        "Missing traffic was treated as a clear runway",
    )?;
    e.tune(ground, json!({"traffic":[]}))?;
    require(
        e.state()?["taxiClearance"]["crossingRunway"] == "09L",
        "No departure crossing clearance",
    )?;
    require(
        e.state()?["taxiClearance"]["approved"] == false,
        "Crossing authorized before readback",
    )?;
    reply(e)?;
    e.tune(ground, json!({"latitude":0.003}))?;
    require(
        e.state()?["taxiClearance"]["guidanceComplete"] == false,
        "Runway vacated while still on it",
    )?;
    e.tune(ground, json!({"latitude":0.0018}))?;
    reply(e)?;
    require(
        e.state()?["taxiClearance"]["holdShortRunway"] == "09R",
        "Crossing did not resume departure taxi",
    )?;
    reply(e)?;
    let taxi = e.state()?["taxiClearance"].clone();
    e.endpoint(ground, &taxi, 0., 3.)?;
    require(
        e.state()?["recommendedFrequencyKhz"] == 118700,
        "No Tower handoff at departure hold",
    )?;
    record(
        e,
        steps,
        "Departure crossing, vacated report and final hold",
    )?;
    if ground != 118700 {
        reply(e)?;
    }
    checkin(e, 118700)?;
    require(
        e.state()?["phase"] == 2,
        "Tower cleared takeoff before ready call",
    )?;
    e.tune(118700, json!({"groundSpeedKnots":0}))?;
    accepted(&e.req("ready")?);
    reply(e)?;
    ticks(e, 118700, json!({}), 6)?;
    require(
        e.state()?["recommendedFrequencyKhz"] == 118700,
        "Departure handoff happened on the ground",
    )?;
    record(e, steps, "Ready call and takeoff permission")?;
    handoff(
        e,
        118700,
        119100,
        json!({"latitude":0,"longitude":0.03,"onGround":false,"groundSpeedKnots":240,"indicatedSpeedKnots":230,"headingDegrees":90,"groundTrackDegrees":90,"altitudeFeet":1000,"heightAglFeet":1000,"verticalSpeedFpm":1500}),
    )?;
    record(e, steps, "Airborne handoff and departure check-in")?;
    if variant == "separate" {
        ticks(e, 119100, json!({}), 3)?;
        require(
            e.state()?["mission"]["active_event"] == "cabin_call",
            "Phase-triggered cabin emergency missing",
        )?;
        e.post("missions/choose", json!({"choice":"coordinate"}))?;
        require(
            e.state()?["mission"]["choices"]
                .as_array()
                .unwrap()
                .is_empty(),
            "Mission response not consumed",
        )?;
        accepted(&e.post("request", json!({"role":"atc", "text":"Mayday, C-GTLT, medical emergency, request priority landing."}))?);
        require(
            e.state()?["emergencyDeclared"] == true,
            "Mission emergency was not declared",
        )?;
        record(
            e,
            steps,
            "Mission cabin call, captain response and ATC emergency declaration",
        )?;
    }
    ticks(
        e,
        119100,
        json!({"longitude":0.5,"altitudeFeet":5000,"heightAglFeet":5000,"verticalSpeedFpm":0}),
        6,
    )?;
    require(
        e.state()?["recommendedFrequencyKhz"] == 119100,
        "Premature handoff inside departure sector",
    )?;
    handoff(e, 119100, 123100, json!({"longitude":0.98}))?;
    record(e, steps, "Predicted departure sector boundary to Center")?;
    ticks(e, 123100, json!({"longitude":1.5}), 6)?;
    require(
        e.state()?["recommendedFrequencyKhz"] == 123100,
        "Center ownership changed inside its sector",
    )?;
    handoff(e, 123100, 123200, json!({"longitude":1.98}))?;
    record(e, steps, "Center-to-Center boundary handoff")?;
    ticks(e, 123200, json!({"longitude":3.0}), 1)?;
    ticks(
        e,
        123200,
        json!({"longitude":3.25,"groundSpeedKnots":400,"indicatedSpeedKnots":290}),
        2,
    )?;
    let descent = e.state()?;
    require(
        descent["clearance"]["altitudeFeet"] == 2500
            && descent["clearance"]["acknowledged"] == false
            && latest(&descent).contains("RESTR")
            && latest(&descent).contains("180")
            && descent["clearance"]["readbackSpeed"]["knots"] == 180.0,
        "No proactive descent clearance before the imported STAR ceiling",
    )?;
    reply(e)?;
    let readback = e.state()?;
    let readback = readback["transcript"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|entry| entry["speaker"] == "COPILOT")
        .expect("descent readback");
    require(
        readback["text"].as_str().unwrap().contains("180"),
        "Copilot omitted the speed from its compound descent readback",
    )?;
    ticks(
        e,
        123200,
        json!({"verticalSpeedFpm":-700,"groundSpeedKnots":200,"indicatedSpeedKnots":180}),
        2,
    )?;
    record(
        e,
        steps,
        "Automatic descent and readback before published STAR restriction",
    )?;
    handoff(e, 123200, 119300, json!({"longitude":3.42}))?;
    record(e, steps, "Arrival Approach handoff")?;
    // Descend on the final approach course. No phase is injected into the engine.
    ticks(
        e,
        119300,
        json!({"longitude":3.80,"altitudeFeet":2500,"heightAglFeet":2500,"verticalSpeedFpm":-700,"groundSpeedKnots":150,"indicatedSpeedKnots":150}),
        6,
    )?;
    require(
        latest(&e.state()?).to_lowercase().contains("cleared")
            && latest(&e.state()?).contains("09R"),
        "Approach clearance missing",
    )?;
    reply(e)?;
    handoff(
        e,
        119300,
        118900,
        json!({"longitude":3.85,"altitudeFeet":2000,"heightAglFeet":2000}),
    )?;
    ticks(e, 118900, json!({}), 2)?;
    require(
        e.state()?["transcript"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| {
                entry["speaker"] == "ATC"
                    && entry["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("cleared to land"))
            }),
        "Tower did not issue landing clearance after check-in",
    )?;
    // Tower may now answer check-in with landing permission, which checkin()
    // reads back immediately. A later standalone grant still needs one reply.
    let landing = e.state()?;
    if landing["transcript"].as_array().unwrap().last().unwrap()["pilotReply"]
        .as_str()
        .is_some_and(|reply| !reply.is_empty())
    {
        reply(e)?;
    }
    let landing = e.state()?;
    let transcript = landing["transcript"].as_array().unwrap();
    let grants = transcript
        .iter()
        .filter(|entry| {
            entry["speaker"] == "ATC"
                && entry["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("cleared to land"))
        })
        .count();
    let readbacks = transcript
        .iter()
        .filter(|entry| {
            entry["speaker"] == "COPILOT"
                && entry["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("cleared to land"))
        })
        .count();
    require(
        grants == 1 && readbacks == 1,
        "Landing clearance and readback must each occur exactly once",
    )?;
    require(
        !transcript.iter().any(|entry| {
            entry["speaker"] == "COPILOT"
                && entry["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("Arrival Tower") && text.contains("climb"))
        }),
        "Final Tower check-in claimed a climb back to the intercept altitude",
    )?;
    record(
        e,
        steps,
        "Approach clearance, Tower check-in and landing clearance",
    )?;
    ticks(
        e,
        118900,
        json!({"longitude":4.006,"onGround":true,"altitudeFeet":0,"heightAglFeet":0,"verticalSpeedFpm":0,"groundSpeedKnots":80}),
        6,
    )?;
    require(e.state()?["phase"] == 7, "Touchdown was not detected")?;
    require(
        e.state()?["recommendedFrequencyKhz"] == 118900,
        "Ground handoff before runway exit",
    )?;
    require(
        !latest(&e.state()?).to_lowercase().contains("vacat")
            && !latest(&e.state()?).to_lowercase().contains("exit runway"),
        "Routine runway-exit call interrupted high-speed landing rollout",
    )?;
    e.tune(118900, json!({"groundSpeedKnots":25}))?;
    let exit = latest(&e.state()?).to_lowercase();
    require(
        exit.contains("vacat") || exit.contains("exit runway"),
        "No runway-vacate instruction after slowing on rollout",
    )?;
    ticks(
        e,
        118900,
        json!({"latitude":0.0004,"longitude":4.012,"groundSpeedKnots":15,"headingDegrees":0,"groundTrackDegrees":0}),
        3,
    )?;
    require(
        e.state()?["recommendedFrequencyKhz"] == 118900,
        "Ground handoff while the tail still overlaps the runway",
    )?;
    if arrival_ground != 118900 {
        handoff(
            e,
            118900,
            arrival_ground,
            json!({"latitude":0.001,"longitude":4.012,"groundSpeedKnots":15,"headingDegrees":0,"groundTrackDegrees":0}),
        )?;
    } else {
        ticks(
            e,
            118900,
            json!({"latitude":0.001,"longitude":4.012,"groundSpeedKnots":15,"headingDegrees":0,"groundTrackDegrees":0}),
            6,
        )?;
        require(
            e.state()?["recommendedFrequencyKhz"] == 118900,
            "Combined Tower invented a Ground handoff",
        )?;
    }
    record(
        e,
        steps,
        "Touchdown, observed runway exit and Ground handoff",
    )?;
    accepted(&e.req("gate")?);
    require(
        e.state()?["taxiClearance"]["holdShortRunway"] == "09L",
        "Taxi to gate skipped intervening runway",
    )?;
    reply(e)?;
    require(
        e.state()?["phase"] == 9,
        "Arrival taxi did not enter TaxiIn",
    )?;
    e.tune(arrival_ground, json!({"latitude":0.002,"groundSpeedKnots":8,"traffic":[{"latitude":0.003,"longitude":4.012,"onGround":true,"altitudeFeet":0,"trackDegrees":90}]}))?;
    require(
        e.state()?["taxiClearance"]["waitingForTraffic"] == true,
        "Arrival crossing ignored runway traffic",
    )?;
    require(
        e.state()?["taxiClearance"]["crossingRunway"] == "",
        "Arrival crossing approved through occupied runway",
    )?;
    e.tune(arrival_ground, json!({"traffic":[]}))?;
    require(
        e.state()?["taxiClearance"]["crossingRunway"] == "09L",
        "Arrival crossing not offered after traffic cleared",
    )?;
    reply(e)?;
    e.tune(arrival_ground, json!({"latitude":0.003}))?;
    require(
        e.state()?["phase"] == 9,
        "Flight ended on the arrival crossing",
    )?;
    e.tune(arrival_ground, json!({"latitude":0.0042}))?;
    reply(e)?;
    require(
        e.state()?["taxiClearance"]["destination"] == "Gate C1",
        "Arrival crossing lost selected gate",
    )?;
    require(
        e.state()?["taxiClearance"]["holdShortRunway"] == "",
        "Completed arrival crossing remained a hold",
    )?;
    reply(e)?;
    let taxi = e.state()?["taxiClearance"].clone();
    e.endpoint(arrival_ground, &taxi, 0., 0.)?;
    require(
        e.state()?["phase"] == 10,
        "Flight did not complete at assigned gate",
    )?;
    record(
        e,
        steps,
        "Arrival crossing, onward taxi and parked at assigned gate",
    )?;
    if variant == "separate" {
        ticks(e, arrival_ground, json!({}), 2)?;
        require(
            e.state()?["mission"]["running"] == false,
            "Mission did not finish at gate",
        )?;
    }
    Ok(())
}

pub(super) fn flight(e: &mut Engine) -> Result {
    let output = repo().join("packages/flight-validation");
    fs::create_dir_all(&output)?;
    let mut reports = Vec::new();
    for (variant, radio) in [
        ("separate", openatc_core::state::Radio::Com1),
        ("ground-tower", openatc_core::state::Radio::Com1),
        ("tower-only", openatc_core::state::Radio::Com1),
        ("separate", openatc_core::state::Radio::Com2),
    ] {
        e.radio = radio;
        if radio == openatc_core::state::Radio::Com2 {
            e.telemetry["com1Khz"] = json!(123450);
        }
        let sim = e.folder.path().join("sim");
        let apt = include_str!("../../fixtures/flight-apt.dat")
            .lines()
            .filter(|line| {
                !(variant != "separate" && line.starts_with("1052 ")
                    || variant == "tower-only" && line.starts_with("1053 "))
            })
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(
            sim.join("Global Scenery/Global Airports/Earth nav data/apt.dat"),
            apt,
        )?;
        e.restart()?;
        println!("Flight with {variant} controller duties on {radio:?}");
        let mut steps = Vec::new();
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(e, &mut steps, variant)))
                .unwrap_or_else(|_| {
                    Err("Flight assertion failed; see finalState in report.json".into())
                });
        reports.push(json!({"variant":variant,"radio":radio,"passed":result.is_ok(),"error":result.as_ref().err().map(ToString::to_string),"steps":steps,"finalState":e.state()?}));
        if radio == openatc_core::state::Radio::Com2 {
            require(
                e.state()?["telemetry"]["com1Khz"] == 123450,
                "COM2 flight unexpectedly retuned COM1",
            )?;
        }
        fs::write(
            output.join("report.json"),
            serde_json::to_vec_pretty(&reports)?,
        )?;
        fs::copy(
            e.folder.path().join("engine.log"),
            output.join("engine.log"),
        )?;
        println!("  Report: {}", output.join("report.json").display());
        result?;
    }
    Ok(())
}
