//! Spoken radio identifiers; displayed transmissions keep their original spelling.

/// Editable radiotelephony alphabet and taxi-route speech context.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pronunciation {
    version: u32,
    letters: Vec<String>,
    digits: Vec<String>,
    taxi_introducers: Vec<String>,
    taxi_connectors: Vec<String>,
    maximum_taxi_letters: usize,
    maximum_taxi_digits: usize,
    /// National registration prefixes, without the hyphen.
    #[serde(default)]
    registration_prefixes: Vec<String>,
    /// A local policy may leave these prefixes unspoken.
    #[serde(default)]
    omit_registration_prefixes: Vec<String>,
}
impl Pronunciation {
    /// Parse a bounded pronunciation table without changing operational identifiers.
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > 16384 {
            return Err("Radio pronunciation exceeds 16 KiB".into());
        }
        let value: Self =
            crate::configuration::from_str(text).map_err(|error| error.to_string())?;
        if value.version != 1
            || value.letters.len() != 26
            || value.digits.len() != 10
            || !(1..=3).contains(&value.maximum_taxi_letters)
            || !(1..=4).contains(&value.maximum_taxi_digits)
            || value.taxi_introducers.is_empty()
            || value.taxi_introducers.len() > 16
            || value.taxi_connectors.len() > 16
            || value.registration_prefixes.len() > 200
            || value.omit_registration_prefixes.len() > 200
            || value
                .letters
                .iter()
                .chain(&value.digits)
                .chain(&value.taxi_introducers)
                .chain(&value.taxi_connectors)
                .any(|word| {
                    word.is_empty()
                        || word.len() > 32
                        || !word.chars().all(|c| c.is_ascii_alphabetic() || c == '-')
                })
            || value
                .registration_prefixes
                .iter()
                .chain(&value.omit_registration_prefixes)
                .any(|prefix| {
                    prefix.is_empty()
                        || prefix.len() > 3
                        || !prefix
                            .chars()
                            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                })
        {
            return Err(
                "Radio pronunciation has invalid alphabet, prefix or context entries".into(),
            );
        }
        Ok(value)
    }
    /// Spell a registration, optionally leaving a regional prefix unspoken.
    #[must_use]
    pub fn registration(&self, callsign: &str, omit: &[String]) -> Option<String> {
        let token = callsign.trim().to_ascii_uppercase();
        let (prefix, rest) = if let Some((prefix, rest)) = token.split_once('-') {
            (prefix.to_owned(), rest.to_owned())
        } else {
            let prefix = self
                .registration_prefixes
                .iter()
                .filter(|prefix| token.starts_with(prefix.as_str()))
                .max_by_key(|prefix| prefix.len())?;
            let rest = token[prefix.len()..].to_owned();
            // A no-hyphen registration starts its suffix with a digit. This keeps
            // three-letter airline codes such as BAW123 out of the spelling path.
            if !rest.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                return None;
            }
            (prefix.clone(), rest)
        };
        if prefix.is_empty()
            || prefix.len() > 3
            || !prefix.chars().all(|c| c.is_ascii_alphanumeric())
            || !(3..=6).contains(&rest.len())
            || !rest.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return None;
        }
        let omit = omit.iter().any(|value| value.eq_ignore_ascii_case(&prefix));
        let mut words = Vec::new();
        if !omit {
            for c in prefix.chars() {
                words.push(self.character(c)?);
            }
        }
        for c in rest.chars() {
            words.push(self.character(c)?);
        }
        Some(words.join(" "))
    }

    fn character(&self, c: char) -> Option<&str> {
        if c.is_ascii_alphabetic() {
            Some(self.letters[(c.to_ascii_uppercase() as u8 - b'A') as usize].as_str())
        } else if c.is_ascii_digit() {
            Some(self.digits[(c as u8 - b'0') as usize].as_str())
        } else {
            None
        }
    }

    fn taxiway(&self, token: &str) -> Option<String> {
        let letters = token.bytes().take_while(u8::is_ascii_uppercase).count();
        if letters == 0
            || letters > self.maximum_taxi_letters
            || token.len() - letters > self.maximum_taxi_digits
            || !token.as_bytes()[letters..].iter().all(u8::is_ascii_digit)
        {
            return None;
        }
        Some(
            token
                .bytes()
                .map(|c| {
                    if c.is_ascii_uppercase() {
                        self.letters[(c - b'A') as usize].as_str()
                    } else {
                        self.digits[(c - b'0') as usize].as_str()
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
        )
    }
}
impl Default for Pronunciation {
    fn default() -> Self {
        Self::parse(include_str!("../../../speech/runtime/radiotelephony.toml"))
            .expect("valid bundled radio pronunciation")
    }
}
fn bundled() -> &'static Pronunciation {
    static VALUE: std::sync::OnceLock<Pronunciation> = std::sync::OnceLock::new();
    VALUE.get_or_init(Pronunciation::default)
}

/// Format radio identifiers for speech without changing the transcript.
#[must_use]
pub fn spoken(text: &str, callsign: &str) -> String {
    spoken_with_airports(text, callsign, &[])
}

/// Resolve known airport identifiers to their names in spoken transmissions.
#[must_use]
pub fn spoken_with_airports(text: &str, callsign: &str, airports: &[(String, String)]) -> String {
    spoken_with_pronunciation(text, callsign, airports, bundled())
}

/// Replace registration callsigns with their spoken forms without changing the transcript.
#[must_use]
pub fn spoken_callsigns(
    text: &str,
    callsigns: &std::collections::BTreeMap<String, String>,
) -> String {
    if callsigns.is_empty() {
        return text.to_owned();
    }
    let mut output = String::with_capacity(text.len());
    for part in text.split_inclusive(|c: char| c.is_whitespace() || c == ',') {
        let token = part.trim();
        let bare = token.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        if let Some(spoken) = callsigns.get(&bare.to_ascii_uppercase())
            && let Some(start) = token.find(bare).filter(|_| !bare.is_empty())
        {
            output.push_str(&token[..start]);
            output.push_str(spoken);
            output.push_str(&token[start + bare.len()..]);
            output.push_str(&part[token.len()..]);
            continue;
        }
        output.push_str(part);
    }
    output
}

/// Spoken copy of a flight number for a traffic callsign. The displayed transcript keeps the
/// number; `group` selects Australia/FAA group form instead of digit-by-digit ICAO form.
#[must_use]
pub fn spoken_flight_number(text: &str, speaker: &str, group: bool) -> String {
    let digits = speaker
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    if digits.is_empty() || digits.len() > 4 || text.contains(speaker) {
        return text.to_owned();
    }
    let words = if group {
        group_number(&digits)
    } else {
        digits.chars().map(digit_word).collect::<Vec<_>>().join(" ")
    };
    let mut output = String::with_capacity(text.len());
    for part in text.split_inclusive(|c: char| !c.is_ascii_alphanumeric()) {
        let word = part.trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
        if word == digits {
            output.push_str(&words);
        } else {
            output.push_str(word);
        }
        output.push_str(&part[word.len()..]);
    }
    output
}

fn group_number(digits: &str) -> String {
    match digits.len() {
        1 => digits
            .chars()
            .next()
            .map_or_else(String::new, |digit| digit_word(digit).to_owned()),
        2 => number_words(digits.parse().unwrap_or(0)),
        3 => {
            let first = digits.chars().next().map_or("zero", digit_word);
            let rest = &digits[1..];
            if rest == "00" {
                format!("{first} hundred")
            } else if let Some(rest) = rest.strip_prefix('0') {
                format!("{first} zero {}", number_words(rest.parse().unwrap_or(0)))
            } else {
                format!("{first} {}", number_words(rest.parse().unwrap_or(0)))
            }
        }
        4 => {
            let first = pair_words(&digits[..2]);
            let second = if &digits[2..] == "00" {
                "hundred".to_owned()
            } else if let Some(rest) = digits[2..].strip_prefix('0') {
                format!("zero {}", number_words(rest.parse().unwrap_or(0)))
            } else {
                number_words(digits[2..].parse().unwrap_or(0))
            };
            format!("{first} {second}")
        }
        _ => digits.to_owned(),
    }
}

fn pair_words(pair: &str) -> String {
    if let Some(rest) = pair.strip_prefix('0') {
        format!("zero {}", number_words(rest.parse().unwrap_or(0)))
    } else {
        number_words(pair.parse().unwrap_or(0))
    }
}

fn number_words(value: u32) -> String {
    const ONES: [&str; 20] = [
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
    ];
    const TENS: [&str; 10] = [
        "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
    ];
    if value < 20 {
        return ONES[value as usize].to_owned();
    }
    let tens = TENS[(value / 10) as usize];
    let ones = value % 10;
    if ones == 0 {
        tens.to_owned()
    } else {
        format!("{tens} {}", ONES[ones as usize])
    }
}

fn digit_word(digit: char) -> &'static str {
    match digit {
        '0' => "zero",
        '1' => "one",
        '2' => "two",
        '3' => "three",
        '4' => "four",
        '5' => "five",
        '6' => "six",
        '7' => "seven",
        '8' => "eight",
        '9' => "niner",
        _ => "",
    }
}

/// Use a loaded pronunciation policy for the spoken copy of a transmission.
#[must_use]
pub fn spoken_with_pronunciation(
    text: &str,
    callsign: &str,
    airports: &[(String, String)],
    policy: &Pronunciation,
) -> String {
    let spelling = callsign
        .chars()
        .filter_map(|c| {
            if c.is_ascii_alphabetic() {
                Some(policy.letters[(c.to_ascii_uppercase() as u8 - b'A') as usize].as_str())
            } else if c.is_ascii_digit() {
                Some(policy.digits[(c as u8 - b'0') as usize].as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let mut output = String::new();
    let mut number_context = false;
    let mut taxi_context = false;
    for part in text.split_inclusive(|c: char| c.is_whitespace() || c == ',') {
        let token = part.trim();
        if token.is_empty() {
            output.push_str(part);
            continue;
        }
        let bare = token.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        let runway_suffix = if number_context && bare.len() > 1 {
            match bare.as_bytes().last() {
                Some(b'L') => Some("left"),
                Some(b'R') => Some("right"),
                Some(b'C') => Some("center"),
                _ => None,
            }
        } else {
            None
        };
        let replacement = if let Some(suffix) =
            runway_suffix.filter(|_| bare[..bare.len() - 1].chars().all(|c| c.is_ascii_digit()))
        {
            format!(
                "{} {suffix}",
                bare[..bare.len() - 1]
                    .chars()
                    .map(|c| policy.digits[(c as u8 - b'0') as usize].as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        } else if !callsign.is_empty() && bare.eq_ignore_ascii_case(callsign) {
            spelling.clone()
        } else if (number_context || bare.contains('.'))
            && bare.chars().all(|c| c.is_ascii_digit() || c == '.')
            && bare.chars().any(|c| c.is_ascii_digit())
        {
            bare.chars()
                .map(|c| {
                    if c == '.' {
                        "point"
                    } else {
                        policy.digits[(c as u8 - b'0') as usize].as_str()
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        } else if taxi_context && let Some(taxiway) = policy.taxiway(bare) {
            taxiway
        } else if let Some((_, name)) = airports
            .iter()
            .find(|(id, name)| !name.is_empty() && bare.eq_ignore_ascii_case(id))
        {
            spoken_with_pronunciation(name.trim(), "", &[], policy)
        } else {
            match bare.to_ascii_uppercase().as_str() {
                "INTL" => "International",
                "RWY" => "runway",
                "TWY" => "taxiway",
                "CLNC" => "clearance",
                "DCT" => "direct",
                "ATC" => "air traffic control",
                "MSL" => "mean sea level",
                "AGL" => "above ground level",
                "APT" => "Airport",
                "GND" => "Ground",
                "TWR" => "Tower",
                "DEL" => "Delivery",
                "APP" | "APPR" => "Approach",
                "DEP" => "Departure",
                "CTR" => "Center",
                "ACC" => "Area Control",
                "IFR" => "I F R",
                "VFR" => "V F R",
                "SID" => "sid",
                "STAR" => "star",
                "ILS" => "I L S",
                "ATIS" => "ay tis",
                "FL" => "flight level",
                "NM" => "nautical miles",
                "KT" | "KTS" => "knots",
                "FT" => "feet",
                "HPA" => "hectopascals",
                "MHZ" => "megahertz",
                "QNH" => "Q N H",
                "QFE" => "Q F E",
                _ => bare,
            }
            .to_owned()
        };
        if let Some(start) = token.find(bare).filter(|_| !bare.is_empty()) {
            output.push_str(&token[..start]);
            output.push_str(&replacement);
            output.push_str(&token[start + bare.len()..]);
        } else {
            output.push_str(token);
        }
        output.push_str(&part[token.len()..]);
        let lower = bare.to_ascii_lowercase();
        taxi_context = policy.taxi_introducers.iter().any(|word| word == &lower)
            || (taxi_context
                && (policy.taxiway(bare).is_some()
                    || policy.taxi_connectors.iter().any(|word| word == &lower)));
        if token.ends_with(['.', ';', ':']) {
            taxi_context = false;
        }
        number_context = matches!(
            bare.to_ascii_lowercase().as_str(),
            "runway" | "rwy" | "squawk" | "frequency" | "heading"
        );
    }
    output
}

#[cfg(test)]
mod tests {
    #[test]
    fn taxi_identifiers_are_spoken_in_context_without_changing_log_or_other_numbers() {
        let input =
            "C-GTLT, Taxi via T, A, A1 and AB2, hold short of runway 04L. Maintain 5000 feet.";
        let output = super::spoken(input, "C-GTLT");
        assert!(output.contains("via Tango, Alfa, Alfa one and Alfa Bravo two,"));
        assert!(output.contains("runway zero four left. Maintain 5000 feet."));
        assert!(input.contains("T, A, A1"));
        assert_eq!(
            super::spoken("A passenger is in seat A1. Taxi via T,A,A1.", ""),
            "A passenger is in seat A1. Taxi via Tango,Alfa,Alfa one."
        );
        assert_eq!(
            super::spoken("Taxiway A1, then B. A passenger needs help.", ""),
            "Taxiway Alfa one, then Bravo. A passenger needs help."
        );
        let text =
            include_str!("../../../speech/runtime/radiotelephony.toml").replace("Alfa", "Alpha");
        let policy = super::Pronunciation::parse(&text).unwrap();
        assert_eq!(
            super::spoken_with_pronunciation("Taxi via A1", "", &[], &policy),
            "Taxi via Alpha one"
        );
        assert!(
            super::Pronunciation::parse(
                &text.replace("maximum_taxi_letters = 2", "maximum_taxi_letters = 99")
            )
            .is_err()
        );
    }
    #[test]
    fn registrations_are_spelled_and_regional_prefixes_can_be_omitted() {
        let policy = super::Pronunciation::default();
        assert_eq!(
            policy.registration("VH-EUA", &[]).unwrap(),
            "Victor Hotel Echo Uniform Alfa"
        );
        assert_eq!(
            policy.registration("VH-EUA", &["VH".to_owned()]).unwrap(),
            "Echo Uniform Alfa"
        );
        assert_eq!(
            policy.registration("N123AB", &[]).unwrap(),
            "November one two three Alfa Bravo"
        );
        assert_eq!(
            policy.registration("9V-SKA", &[]).unwrap(),
            "niner Victor Sierra Kilo Alfa"
        );
        assert!(policy.registration("QFA123", &[]).is_none());
        assert!(policy.registration("BAW123", &[]).is_none());
    }

    #[test]
    fn callsign_map_replaces_registration_tokens() {
        let mut callsigns = std::collections::BTreeMap::new();
        callsigns.insert(
            "VH-EUA".to_owned(),
            "Victor Hotel Echo Uniform Alfa".to_owned(),
        );
        assert_eq!(
            super::spoken_callsigns("VH-EUA, line up.", &callsigns),
            "Victor Hotel Echo Uniform Alfa, line up."
        );
        assert_eq!(
            super::spoken_callsigns("QFA123, line up.", &callsigns),
            "QFA123, line up."
        );
    }

    #[test]
    fn expands_radio_names_without_changing_log_text() {
        let text = "Los Angeles GND West, C-GTLT, cleared to KSNA, IFR.";
        let spoken = super::spoken_with_airports(
            text,
            "C-GTLT",
            &[("KSNA".into(), "John Wayne Airport".into())],
        );
        assert!(spoken.contains("Ground West"));
        assert!(spoken.contains("cleared to John Wayne Airport"));
        assert!(spoken.contains("I F R"));
        assert!(text.contains("GND"));
    }

    #[test]
    fn flight_numbers_are_spoken_digit_by_digit() {
        assert_eq!(
            super::spoken_flight_number("Air Canada 160, taxi via Alpha.", "ACA160", false),
            "Air Canada one six zero, taxi via Alpha."
        );
        assert_eq!(
            super::spoken_flight_number("Speedbird 123, hold short.", "BAW123", false),
            "Speedbird one two three, hold short."
        );
        assert_eq!(
            super::spoken_flight_number("ACA160, cleared for takeoff.", "ACA160", false),
            "ACA160, cleared for takeoff.",
            "the compact code itself is left for the tag, not the spoken callsign"
        );
        assert_eq!(
            super::spoken_flight_number("Air Canada 160, taxi via Alpha.", "ACA160", true),
            "Air Canada one sixty, taxi via Alpha."
        );
        assert_eq!(
            super::spoken_flight_number("Qantas 1220, contact Tower.", "QFA1220", true),
            "Qantas twelve twenty, contact Tower."
        );
        assert_eq!(
            super::spoken_flight_number("Velocity 1300, contact Tower.", "VOZ1300", true),
            "Velocity thirteen hundred, contact Tower."
        );
    }

    #[test]
    fn spells_callsign_without_changing_display_or_altitude() {
        let input = "C-GTLT, runway 29L, squawk 2105, maintain 5000 feet, contact 121.400.";
        let spoken = super::spoken(input, "C-GTLT");
        assert!(spoken.starts_with("Charlie Golf Tango Lima Tango,"));
        assert!(spoken.contains("runway two niner left"));
        assert!(spoken.contains("squawk two one zero five"));
        assert!(spoken.contains("maintain 5000 feet"));
        assert!(spoken.contains("one two one point four zero zero"));
    }
}
