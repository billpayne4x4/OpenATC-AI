//! Time-of-day and weather-based GA / rotorcraft traffic variation.
//!
//! GA traffic is highest during the day (pleasure and training flights) and drops at night.
//! Rotorcraft (EMS, law enforcement, news) are relatively more active at night.
//! Poor visibility or strong surface wind reduces both GA and rotorcraft traffic.

use openatc_settings::Settings;

/// Classification of the simulator's time of day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeOfDay {
    /// Overnight hours (roughly 21:00-05:00 local).
    Night,
    /// Early morning (roughly 05:00-07:00 local).
    Dawn,
    /// Daytime (roughly 07:00-19:00 local).
    Day,
    /// Evening (roughly 19:00-21:00 local).
    Dusk,
}

/// Classify the simulator time of day from seconds since midnight.
///
/// `seconds` is the X-Plane `simulation_seconds` telemetry field, which wraps each
/// simulated day. Returns `None` when the value is unavailable.
#[must_use]
pub fn classify(seconds: f64) -> Option<TimeOfDay> {
    if !seconds.is_finite() || seconds < 0.0 || seconds >= 86_400.0 {
        return None;
    }
    let seconds = seconds % 86_400.0;
    match seconds {
        0.0..=18_000.0 | 75_600.0..=86_399.999 => Some(TimeOfDay::Night),
        18_000.0..=25_200.0 => Some(TimeOfDay::Dawn),
        25_200.0..=68_400.0 => Some(TimeOfDay::Day),
        68_400.0..=75_600.0 => Some(TimeOfDay::Dusk),
        _ => None,
    }
}

/// GA traffic multiplier for the given time of day.
#[must_use]
pub fn ga_multiplier(time_of_day: TimeOfDay, settings: &Settings) -> f32 {
    match time_of_day {
        TimeOfDay::Day => settings.ga_day_multiplier,
        _ => settings.ga_night_multiplier,
    }
}

/// Rotorcraft traffic multiplier for the given time of day.
#[must_use]
pub fn rotorcraft_multiplier(time_of_day: TimeOfDay, settings: &Settings) -> f32 {
    match time_of_day {
        TimeOfDay::Day => settings.rotorcraft_day_multiplier,
        _ => settings.rotorcraft_night_multiplier,
    }
}

/// Weather-based multiplier for GA and rotorcraft traffic.
///
/// Returns 1.0 when weather is good, tapering toward 0.0 as visibility drops below
/// the configured minimum or wind exceeds the configured maximum. Returns `None`
/// when no weather sample is available (caller should treat as neutral).
#[must_use]
pub fn weather_multiplier(
    visibility_meters: Option<f64>,
    wind_knots: Option<f64>,
    settings: &Settings,
) -> Option<f32> {
    if !settings.traffic_weather_variations {
        return Some(1.0);
    }
    let visibility = visibility_meters?;
    let wind = wind_knots?;
    if !visibility.is_finite() || !wind.is_finite() {
        return Some(1.0);
    }

    // Visibility factor: 1.0 at or above the minimum, scaling down to 0.25 at zero.
    let vis_factor = if visibility >= settings.weather_min_visibility_meters as f64 {
        1.0f32
    } else {
        let min = settings.weather_min_visibility_meters.max(1.0) as f64;
        (0.25f64 + 0.75f64 * (visibility / min).clamp(0.0, 1.0)) as f32
    };

    // Wind factor: 1.0 at or below the maximum, scaling down to 0.25 at double the max.
    let wind_factor = if wind <= settings.weather_max_wind_knots as f64 {
        1.0f32
    } else {
        let max = settings.weather_max_wind_knots.max(1.0) as f64;
        let excess = (wind - max) / max; // 0 at max, 1 at double max
        (1.0f64 - 0.75f64 * excess.clamp(0.0, 1.0)) as f32
    };

    Some(vis_factor.min(wind_factor).max(0.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_settings() -> Settings {
        Settings::default()
    }

    #[test]
    fn classify_midnight_is_night() {
        assert_eq!(classify(0.0), Some(TimeOfDay::Night));
        assert_eq!(classify(3600.0), Some(TimeOfDay::Night));
        assert_eq!(classify(18000.0), Some(TimeOfDay::Night));
    }

    #[test]
    fn classify_dawn() {
        assert_eq!(classify(21600.0), Some(TimeOfDay::Dawn)); // 06:00
    }

    #[test]
    fn classify_day() {
        assert_eq!(classify(43200.0), Some(TimeOfDay::Day)); // 12:00
        assert_eq!(classify(64800.0), Some(TimeOfDay::Day)); // 18:00
    }

    #[test]
    fn classify_dusk() {
        assert_eq!(classify(72000.0), Some(TimeOfDay::Dusk)); // 20:00
    }

    #[test]
    fn classify_invalid_returns_none() {
        assert_eq!(classify(-1.0), None);
        assert_eq!(classify(86400.0), None);
        assert_eq!(classify(f64::NAN), None);
        assert_eq!(classify(f64::INFINITY), None);
    }

    #[test]
    fn ga_multiplier_day_higher_than_night() {
        let settings = test_settings();
        assert!(ga_multiplier(TimeOfDay::Day, &settings) > ga_multiplier(TimeOfDay::Night, &settings));
        assert!(ga_multiplier(TimeOfDay::Dawn, &settings) == settings.ga_night_multiplier);
        assert!(ga_multiplier(TimeOfDay::Dusk, &settings) == settings.ga_night_multiplier);
    }

    #[test]
    fn rotorcraft_multiplier_night_higher_than_day() {
        let settings = test_settings();
        assert!(rotorcraft_multiplier(TimeOfDay::Night, &settings) > rotorcraft_multiplier(TimeOfDay::Day, &settings));
        assert!(rotorcraft_multiplier(TimeOfDay::Dawn, &settings) == settings.rotorcraft_night_multiplier);
        assert!(rotorcraft_multiplier(TimeOfDay::Dusk, &settings) == settings.rotorcraft_night_multiplier);
    }

    #[test]
    fn weather_multiplier_good_weather_is_one() {
        let settings = test_settings();
        assert_eq!(
            weather_multiplier(Some(10000.0), Some(5.0), &settings),
            Some(1.0)
        );
    }

    #[test]
    fn weather_multiplier_poor_visibility_reduces() {
        let settings = test_settings();
        let result = weather_multiplier(Some(1000.0), Some(5.0), &settings);
        assert!(result.is_some_and(|v| v < 1.0));
    }

    #[test]
    fn weather_multiplier_high_wind_reduces() {
        let settings = test_settings();
        let result = weather_multiplier(Some(10000.0), Some(30.0), &settings);
        assert!(result.is_some_and(|v| v < 1.0));
    }

    #[test]
    fn weather_multiplier_disabled_returns_one() {
        let mut settings = test_settings();
        settings.traffic_weather_variations = false;
        assert_eq!(
            weather_multiplier(Some(100.0), Some(50.0), &settings),
            Some(1.0)
        );
    }

    #[test]
    fn weather_multiplier_no_sample_returns_none() {
        let settings = test_settings();
        assert_eq!(weather_multiplier(None, None, &settings), None);
    }
}
