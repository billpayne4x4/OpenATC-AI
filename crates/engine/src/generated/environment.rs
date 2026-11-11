//! Airport observations retained with their age for generated route planning.
#[allow(unused_imports, clippy::wildcard_imports)]
use super::*;
use super::{Airport, Arc, BTreeMap, Duration, Instant};
use openatc_core::traffic::air::Atmosphere;

pub(super) type Samples = BTreeMap<String, (serde_json::Value, Instant)>;
pub(super) type Observations = Arc<std::sync::RwLock<Samples>>;

const PLANNING_FIELDS: [&str; 8] = [
    "latitude",
    "longitude",
    "sampleAltitudeFeet",
    "pressureHpa",
    "temperatureC",
    "magneticVariationDegrees",
    "windDegrees",
    "windKnots",
];

/// Largest drift in each planning field that a prepared scene still accepts. Position is
/// exact. The simulator's weather moves in small steps all the time, and a scene that took
/// seconds to prepare should not be thrown away for a wind change of a few degrees.
const PLANNING_TOLERANCE: [f64; 8] = [0.0, 0.0, 50.0, 1.5, 3.0, 0.5, 15.0, 4.0];

fn planning_drift_within(index: usize, live: f64, expected: f64) -> bool {
    if PLANNING_FIELDS[index] == "windDegrees" {
        let difference = ((live - expected + 180.0).rem_euclid(360.0) - 180.0).abs();
        return difference <= PLANNING_TOLERANCE[index];
    }
    (live - expected).abs() <= PLANNING_TOLERANCE[index]
}

#[derive(Clone)]
pub(super) struct Environment {
    pub atmosphere: Atmosphere,
    pub variation: f64,
    pub wind_from: f64,
    pub wind_knots: f64,
    observed: Instant,
    live: Option<(Observations, String, [f64; 8])>,
}
impl Environment {
    pub fn from_sample(
        airport: &Airport,
        sample: &(serde_json::Value, Instant),
    ) -> Result<Self, String> {
        let (value, observed) = sample;
        let number = |key: &str| {
            value[key]
                .as_f64()
                .filter(|v| v.is_finite())
                .ok_or_else(|| format!("Missing airport planning weather field {key}"))
        };
        if value["airport"].as_str() != Some(airport.icao.as_str())
            || (number("latitude")? - airport.reference_latitude).abs() > 0.001
            || (number("longitude")? - airport.reference_longitude).abs() > 0.001
            || (number("sampleAltitudeFeet")? - airport.elevation_feet).abs() > 50.0
        {
            return Err("Planning weather does not match the airport surface".into());
        }
        let environment = Self {
            atmosphere: Atmosphere::surface(
                number("pressureHpa")?,
                number("temperatureC")?,
                airport.elevation_feet,
            )?,
            variation: number("magneticVariationDegrees")?,
            wind_from: number("windDegrees")?,
            wind_knots: number("windKnots")?,
            observed: *observed,
            live: None,
        };
        if !(-180.0..=180.0).contains(&environment.variation)
            || !(0.0..=360.0).contains(&environment.wind_from)
            || !(0.0..=400.0).contains(&environment.wind_knots)
            || !environment.fresh()
        {
            return Err("Airport planning weather is stale or outside its envelope".into());
        }
        Ok(environment)
    }
    pub fn follow(
        mut self,
        airport: &str,
        observations: &Observations,
        sample: &serde_json::Value,
    ) -> Self {
        self.live = Some((
            observations.clone(),
            airport.to_owned(),
            PLANNING_FIELDS.map(|key| sample[key].as_f64().unwrap_or(f64::NAN)),
        ));
        self
    }
    /// Convert an indicated runway speed into movement speed using fresh local weather.
    pub fn runway_speed(
        &self,
        airport: &Airport,
        runway_name: &str,
        indicated_knots: f64,
        geometric_feet: f64,
    ) -> Result<f64, String> {
        if !self.fresh() {
            return Err("Airport planning weather expired before runway scheduling".into());
        }
        let runway = airport
            .runways
            .iter()
            .find(|runway| runway.first_name == runway_name || runway.second_name == runway_name)
            .ok_or("Runway speed needs a known runway end")?;
        let (a, b) = if runway.first_name == runway_name {
            (runway.first, runway.second)
        } else {
            (runway.second, runway.first)
        };
        let heading = (b.east - a.east)
            .atan2(b.north - a.north)
            .to_degrees()
            .rem_euclid(360.0);
        let true_knots = self
            .atmosphere
            .true_knots(indicated_knots, geometric_feet)?;
        openatc_core::traffic::air::wind(true_knots, heading, self.wind_from, self.wind_knots)
            .map(|(ground, _)| ground)
    }
    pub fn fresh(&self) -> bool {
        if let Some((observations, airport, expected)) = &self.live {
            return observations.read().is_ok_and(|samples| {
                samples.get(airport).is_some_and(|(sample, observed)| {
                    observed.elapsed() < Duration::from_secs(45)
                        && sample["airport"].as_str() == Some(airport.as_str())
                        && PLANNING_FIELDS.iter().enumerate().zip(expected).all(
                            |((index, key), expected)| {
                                sample[*key].as_f64().is_some_and(|live| {
                                    planning_drift_within(index, live, *expected)
                                })
                            },
                        )
                })
            });
        }
        self.observed.elapsed() < Duration::from_secs(45)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_observations_revalidate_only_planning_drift_beyond_tolerance() {
        let airport = Airport {
            icao: "TEST".into(),
            reference_latitude: 43.0,
            reference_longitude: 7.0,
            elevation_feet: 25.0,
            ..Default::default()
        };
        let sample = (
            serde_json::json!({
                "airport":"TEST", "latitude":43.0, "longitude":7.0,
                "sampleAltitudeFeet":25.0, "pressureHpa":1013.25, "temperatureC":15.0,
                "magneticVariationDegrees":2.0, "windDegrees":220.0, "windKnots":12.0
            }),
            Instant::now(),
        );
        let observations = Observations::default();
        let mut weather = Environment::from_sample(&airport, &sample).unwrap().follow(
            "TEST",
            &observations,
            &sample.0,
        );
        weather.observed = Instant::now().checked_sub(Duration::from_secs(90)).unwrap();
        assert!(!weather.fresh());
        observations
            .write()
            .unwrap()
            .insert("TEST".into(), sample.clone());
        assert!(weather.fresh());
        for (index, field) in PLANNING_FIELDS.iter().enumerate() {
            let mut changed = sample.clone();
            let value = changed.0[field].as_f64().unwrap();
            changed.0[field] = serde_json::json!(value + PLANNING_TOLERANCE[index] + 0.01);
            observations.write().unwrap().insert("TEST".into(), changed);
            assert!(
                !weather.fresh(),
                "a change in {field} beyond its tolerance must invalidate the planned trajectory"
            );
        }
        for (index, field) in PLANNING_FIELDS.iter().enumerate().skip(2) {
            let mut drift = sample.clone();
            let value = drift.0[field].as_f64().unwrap();
            drift.0[field] = serde_json::json!(value + PLANNING_TOLERANCE[index] * 0.5);
            observations.write().unwrap().insert("TEST".into(), drift);
            assert!(
                weather.fresh(),
                "a drift in {field} inside its tolerance keeps the prepared scene"
            );
        }
        let mut wrapped = sample.clone();
        wrapped.0["windDegrees"] = serde_json::json!(227.0);
        wrapped.0["windKnots"] = serde_json::json!(12.0);
        observations.write().unwrap().insert("TEST".into(), wrapped);
        assert!(weather.fresh(), "a 7 degree wind shift is inside tolerance");
        let mut wrapped_far = sample.clone();
        wrapped_far.0["windDegrees"] = serde_json::json!(20.0);
        observations
            .write()
            .unwrap()
            .insert("TEST".into(), wrapped_far);
        assert!(
            !weather.fresh(),
            "a 160 degree wind shift is beyond tolerance"
        );
        let mut stale = sample.clone();
        stale.1 = Instant::now().checked_sub(Duration::from_secs(46)).unwrap();
        observations.write().unwrap().insert("TEST".into(), stale);
        assert!(!weather.fresh());
        let mut wrong_airport = sample.clone();
        wrong_airport.0["airport"] = serde_json::json!("OTHER");
        observations
            .write()
            .unwrap()
            .insert("TEST".into(), wrong_airport);
        assert!(!weather.fresh());
        let mut unrelated = sample;
        unrelated.0["visibilityMeters"] = serde_json::json!(10000);
        observations
            .write()
            .unwrap()
            .insert("TEST".into(), unrelated);
        assert!(weather.fresh());
    }

    #[test]
    fn runway_roll_speeds_use_density_wind_and_the_selected_end() {
        let airport = Airport {
            elevation_feet: 5000.0,
            runways: vec![openatc_core::airport::Runway {
                first_name: "04".into(),
                second_name: "22".into(),
                first: Point::default(),
                second: Point {
                    north: 3000.0,
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut weather = Environment {
            atmosphere: Atmosphere::surface(1013.25, 25.0, 5000.0).unwrap(),
            variation: 0.0,
            wind_from: 0.0,
            wind_knots: 20.0,
            observed: Instant::now(),
            live: None,
        };
        for indicated in [60.0, 110.0, 150.0] {
            let head = weather
                .runway_speed(&airport, "04", indicated, 5000.0)
                .unwrap();
            let tail = weather
                .runway_speed(&airport, "22", indicated, 5000.0)
                .unwrap();
            assert!((tail - head - 40.0).abs() < 1e-6);
            assert!(
                f64::midpoint(head, tail) > indicated,
                "Density must change TAS at this warm elevated airport"
            );
        }
        assert!(
            weather.runway_speed(&airport, "04", 150.0, 6000.0).unwrap()
                > weather.runway_speed(&airport, "04", 150.0, 5000.0).unwrap(),
            "Density conversion must use the runway height, not the airport maximum"
        );
        assert!(weather.runway_speed(&airport, "99", 150.0, 5000.0).is_err());
        weather.observed = Instant::now().checked_sub(Duration::from_secs(46)).unwrap();
        assert!(weather.runway_speed(&airport, "04", 150.0, 5000.0).is_err());
    }
    #[test]
    #[allow(clippy::float_cmp)]
    fn planner_requires_current_weather_at_the_correct_airport() {
        let airport = Airport {
            icao: "TEST".into(),
            reference_latitude: 43.0,
            reference_longitude: 7.0,
            elevation_feet: 25.0,
            ..Default::default()
        };
        let mut sample = (
            serde_json::json!({"airport":"TEST","latitude":43.0,"longitude":7.0,
            "sampleAltitudeFeet":25.0,"pressureHpa":1023.0,"temperatureC":20.0,
            "magneticVariationDegrees":2.0,"windDegrees":220.0,"windKnots":12.0}),
            Instant::now(),
        );
        let weather = Environment::from_sample(&airport, &sample).unwrap();
        assert_eq!(weather.wind_from, 220.0);
        assert_eq!(weather.variation, 2.0);
        let height = weather.atmosphere.geometric(12000.0, true).unwrap();
        assert!((weather.atmosphere.reported(height, true).unwrap() - 12000.0).abs() < 0.01);
        sample.0["latitude"] = serde_json::json!(44.0);
        assert!(Environment::from_sample(&airport, &sample).is_err());
        sample.0["latitude"] = serde_json::json!(43.0);
        sample.1 = Instant::now().checked_sub(Duration::from_secs(46)).unwrap();
        assert!(Environment::from_sample(&airport, &sample).is_err());
        sample.1 = Instant::now();
        sample
            .0
            .as_object_mut()
            .unwrap()
            .remove("magneticVariationDegrees");
        assert!(Environment::from_sample(&airport, &sample).is_err());
    }
}
