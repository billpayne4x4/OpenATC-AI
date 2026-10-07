//! Request matching and altitude, route and emergency phrase parsing.

const DEFINITIONS: [(&str, &str); 23] = [
    ("clearance", "request ifr clearance"),
    ("start", "request start-up"),
    ("start_pushback", "request start-up and pushback"),
    ("pushback", "request pushback"),
    ("taxi", "request taxi"),
    ("backtrack", "request runway backtrack"),
    ("ready", "ready for departure"),
    ("gate", "taxi to parking"),
    ("progressive", "repeat taxi instructions"),
    ("radio_check", "radio check"),
    ("readback", "read back clearance"),
    ("repeat", "say again"),
    ("standby", "stand by"),
    ("unable", "unable"),
    ("frequency", "request frequency change"),
    ("position", "confirm position"),
    ("altitude", "request altitude..."),
    ("direct", "request direct-to..."),
    ("descent", "request descent..."),
    ("cancel_ifr", "cancel ifr"),
    ("approach", "brief arrival approach"),
    ("go_around", "going around"),
    ("emergency", "declare emergency"),
];

/// Parse pilot text, including
/// evaluation order (exacts, emergency substring, altitude/direct regexes,
/// catalogue titles) and the meters-to-feet rounding.
pub(crate) fn interpret(text: &str) -> super::identify::Interpreted {
    let normalized = text.to_ascii_lowercase();
    let mut request = super::identify::Interpreted {
        intent: "conversation".to_owned(),
        ..Default::default()
    };
    match normalized.as_str() {
        "radio check" => "radio_check".clone_into(&mut request.intent),
        "say again" | "repeat" => "repeat".clone_into(&mut request.intent),
        "unable" => "unable".clone_into(&mut request.intent),
        "stand by" => "standby".clone_into(&mut request.intent),
        "request clearance" | "request ifr clearance" => {
            "clearance".clone_into(&mut request.intent);
        }
        "request taxi" => "taxi".clone_into(&mut request.intent),
        "request start" | "request startup" | "request start up" => {
            "start".clone_into(&mut request.intent);
        }
        "request start and pushback" | "request pushback and start" | "request push and start" => {
            "start_pushback".clone_into(&mut request.intent);
        }
        "request pushback" => "pushback".clone_into(&mut request.intent),
        "ready for departure" => "ready".clone_into(&mut request.intent),
        "going around" | "go around" => "go_around".clone_into(&mut request.intent),
        "cancel ifr" => "cancel_ifr".clone_into(&mut request.intent),
        "request altimeter" => "altimeter".clone_into(&mut request.intent),
        "request weather" => "weather".clone_into(&mut request.intent),
        _ => {}
    }
    // Callsign-bearing transmissions use the same deterministic workflow parser.
    for (phrase, intent) in [
        ("request start-up and pushback", "start_pushback"),
        ("request start and pushback", "start_pushback"),
        ("request pushback and start", "start_pushback"),
        ("request push and start", "start_pushback"),
        ("request start up", "start"),
        ("request startup", "start"),
        ("request start", "start"),
        ("request pushback", "pushback"),
        ("request ifr clearance", "clearance"),
        ("request clearance", "clearance"),
        ("request taxi", "taxi"),
        ("request runway backtrack", "backtrack"),
        ("request backtrack", "backtrack"),
    ] {
        if normalized.contains(phrase) {
            intent.clone_into(&mut request.intent);
            break;
        }
    }
    if normalized.contains("mayday")
        || normalized.contains("pan pan")
        || normalized == "emergency"
        || normalized == "declare emergency"
    {
        "emergency".clone_into(&mut request.intent);
    }
    let altitude =
        regex::Regex::new(r"(?:request )?(?:altitude |climb |descend |descent )(?:to )?(fl\s*)?([0-9]{2,5})(?: feet| meters)?")
            .ok();
    if let Some(expression) = altitude
        && let Some(found) = expression.captures(&normalized)
        && let Some(digits) = found.get(2)
        && let Ok(value) = digits.as_str().parse::<i32>()
    {
        let fl = found.get(1).is_some();
        if normalized.contains("desc") {
            "descent".clone_into(&mut request.intent);
        } else {
            "altitude".clone_into(&mut request.intent);
        }
        request.altitude_feet = value * if fl { 100 } else { 1 };
        if normalized.contains("meter") && !fl {
            request.altitude_feet = super::meters_to_feet(request.altitude_feet);
        }
    }
    let direct = regex::Regex::new(r"(?:request )?direct(?: to)? ([a-z0-9]{2,10})").ok();
    if let Some(expression) = direct
        && let Some(found) = expression.captures(&normalized)
        && let Some(waypoint) = found.get(1)
    {
        "direct".clone_into(&mut request.intent);
        request.waypoint = waypoint.as_str().to_ascii_uppercase();
    }
    if request.intent == "conversation" {
        for (intent, title) in DEFINITIONS {
            if title == normalized {
                intent.clone_into(&mut request.intent);
                break;
            }
        }
    }
    request
}
