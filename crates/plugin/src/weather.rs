//! Airport surface weather, sampled on X-Plane's pre-flight main-thread callback.
use openatc_core::stations::Station;
use std::{collections::BTreeMap, time::Instant};
#[derive(Default)]
pub struct Sampler {
    sampled: BTreeMap<String, Instant>,
    last_query: Option<Instant>,
}
impl Sampler {
    /// One SDK query per tick; priority goes to the tuned ATIS airport.
    pub fn update(&mut self, interface: &mut openatc_ui::interface::Interface) {
        if !interface.engine.connected() {
            return;
        }
        if self.last_query.is_some_and(|t| t.elapsed().as_secs() < 2) {
            return;
        }
        let tuned = interface.engine.state().telemetry.com1_khz;
        let nearest = interface
            .nearby_stations
            .iter()
            .find(|s| s.service != "Center")
            .map_or("", |s| s.airport.as_str());
        let mut candidates: Vec<Station> = interface
            .nearby_stations
            .iter()
            .filter(|s| s.service != "Center" && (s.khz == tuned || s.airport == nearest))
            .cloned()
            .collect();
        candidates.sort_by_key(|s| s.khz != tuned);
        let Some(station) = candidates.into_iter().find(|s| {
            self.sampled
                .get(&s.airport)
                .is_none_or(|t| t.elapsed().as_secs() >= 15)
        }) else {
            return;
        };
        self.last_query = Some(Instant::now());
        self.sampled.insert(station.airport.clone(), Instant::now());
        // SDK return 0 means regional best-available weather, not failure. Check the
        // output instead; unavailable/invalid samples must never become fake ATIS.
        let mut info: xplane_sys::XPLMWeatherInfo_t = unsafe { std::mem::zeroed() };
        let Ok(size) = i32::try_from(std::mem::size_of_val(&info)) else {
            return;
        };
        info.structSize = size;
        let airport_metar = unsafe {
            xplane_sys::XPLMGetWeatherAtLocation(
                station.latitude,
                station.longitude,
                station.elevation_feet / 3.280_839_895,
                &raw mut info,
            )
        } != 0;
        if ![
            info.temperature_alt,
            info.dewpoint_alt,
            info.pressure_sl,
            info.wind_dir_alt,
            info.wind_spd_alt,
            info.visibility,
        ]
        .iter()
        .all(|v| v.is_finite())
            || !(80_000.0..110_000.0).contains(&info.pressure_sl)
        {
            return;
        }
        let layers: Vec<String> = info
            .cloud_layers
            .iter()
            .filter(|c| c.coverage.is_finite() && c.coverage > 0.0 && c.alt_base.is_finite())
            .map(|c| {
                let description = if c.coverage <= 0.25 {
                    openatc_core::dialogue::say("weather_few_clouds", &[])
                } else if c.coverage <= 0.5 {
                    openatc_core::dialogue::say("weather_scattered_clouds", &[])
                } else if c.coverage < 0.9 {
                    openatc_core::dialogue::say("weather_broken_cloud", &[])
                } else {
                    openatc_core::dialogue::say("weather_overcast", &[])
                };
                let feet =
                    (f64::from(c.alt_base) * 3.280_839_895 - station.elevation_feet).max(0.0);
                openatc_core::dialogue::say(
                    "weather_cloud_layer",
                    &[
                        ("description", description),
                        ("altitude", format!("{:.0}", (feet / 100.0).round() * 100.0)),
                    ],
                )
            })
            .collect();
        interface.engine.post("/weather/simulator",serde_json::json!({"airport":station.airport,
            "latitude":station.latitude,"longitude":station.longitude,"sampleAltitudeFeet":station.elevation_feet,
            "source":if airport_metar{"simulator-airport"}else{"simulator-region"},
            "temperatureC":info.temperature_alt,"dewpointC":info.dewpoint_alt,
            "pressureHpa":info.pressure_sl/100.0,"windDegrees":info.wind_dir_alt,
            "windKnots":info.wind_spd_alt*1.943_844_5,"visibilityMeters":info.visibility,
            "clouds":if layers.is_empty(){openatc_core::dialogue::say("weather_no_significant_cloud", &[])}else{layers.join(". ")}}));
    }
}
