use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    // A build run from xtask supplies a unique value so the stamp refreshes even
    // when Cargo can reuse the rest of the build-script output.
    println!("cargo:rerun-if-env-changed=OPENATC_BUILD_FORCE");
    let forced = std::env::var("OPENATC_BUILD_FORCE")
        .ok()
        .and_then(|value| value.parse::<u64>().ok());
    let seconds = forced.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs())
    });
    let days = i64::try_from(seconds / 86_400).unwrap_or(0);
    let remainder = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let stamp = format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        remainder / 3_600,
        (remainder % 3_600) / 60
    );
    println!("cargo:rustc-env=OPENATC_BUILD={stamp}");
}

/// Convert days since 1970-01-01 to a Gregorian date.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}
