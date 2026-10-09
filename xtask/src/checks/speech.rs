use super::*;
pub(super) fn speech_examples(e: &Engine, m: &Model) -> Result {
    *m.answer.lock().unwrap() = "Test proposal only.".into();
    e.settings(json!({"aiEnabled":true,"aiModel":"local-test-model","aiUrl":m.url()}))?;
    for (airport, rules, phase, tag, region, phrase) in [
        (
            "EGLL",
            "vfr",
            "cruise",
            "basic_service",
            "united_kingdom",
            "basic service",
        ),
        (
            "KJFK",
            "ifr",
            "departure",
            "climb_via",
            "us",
            "climb via SID",
        ),
        (
            "YMLT",
            "ifr",
            "taxi",
            "readback_taxi",
            "australia",
            "holding point",
        ),
    ] {
        let v=e.post("suggest",json!({"role":"atc","airport":airport,"flightRules":rules,"phase":phase,"situations":[tag],"facts":"Testing selection only; do not issue an operational clearance."}))?;
        assert_eq!(v["transmission"], "Test proposal only.");
        let prompt = m.prompt();
        assert!(prompt.contains(&format!("Regional scope: {region}.")));
        assert!(prompt.contains(phrase));
        assert!(prompt.contains(&format!("flight rules {rules}")));
    }
    e.post("suggest",json!({"role":"atc","airport":"EGLL","flightRules":"ifr","facts":"Test wording examples only."}))?;
    let prompt = m.prompt();
    assert!(prompt.matches("\n- ").count() > 18);
    assert!(prompt.contains("rather than copying an example word for word"));
    let before = m.count();
    for body in [
        json!({"role":"atc","airport":"YMLT","flightRules":"vfr","phase":"cruise","situations":["basic_service"]}),
        json!({"role":"atc","airport":"KJFK","flightRules":"ifr","phase":"departure","situations":["conditional_lineup_traffic_identified_and_in_sight"]}),
        json!({"role":"atc","airport":"KJFK","flightRules":"invalid"}),
    ] {
        assert_eq!(e.call("suggest", Some(body))?.0, 400);
    }
    assert_eq!(m.count(), before);
    Ok(())
}
