//! Offline nearby settlement references for spoken position reports.
use std::sync::OnceLock;

struct Place {
    name: String,
    latitude: f64,
    longitude: f64,
    population: u64,
}

fn places() -> &'static [Place] {
    static PLACES: OnceLock<Vec<Place>> = OnceLock::new();
    PLACES.get_or_init(|| {
        include_str!("../../../assets/geography/places.tsv")
            .lines()
            .filter_map(|line| {
                let fields: Vec<_> = line.split('\t').collect();
                Some(Place {
                    name: fields.first()?.to_string(),
                    latitude: fields.get(1)?.parse().ok()?,
                    longitude: fields.get(2)?.parse().ok()?,
                    population: fields.get(4)?.parse().ok()?,
                })
            })
            .collect()
    })
}

/// Report a nearby town or city, with distance and direction when outside its vicinity.
/// A nearby major city is preferred to a smaller suburb; this does not establish city boundaries.
#[must_use]
pub fn report(latitude: f64, longitude: f64, on_ground: bool) -> Option<String> {
    if !latitude.is_finite()
        || !longitude.is_finite()
        || latitude.abs() > 90.0
        || longitude.abs() > 180.0
    {
        return None;
    }
    let candidates: Vec<_> = places()
        .iter()
        .filter(|p| (p.latitude - latitude).abs() < 1.0)
        .map(|p| {
            (
                p,
                crate::ops::distance_nm(latitude, longitude, p.latitude, p.longitude),
            )
        })
        .filter(|(_, distance)| *distance <= 50.0)
        .collect();
    let selected = candidates
        .iter()
        .filter(|(p, d)| p.population >= 50_000 && *d <= 10.0)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .or_else(|| candidates.iter().min_by(|a, b| a.1.total_cmp(&b.1)))?;
    let (place, distance) = selected;
    if *distance < 1.0 {
        return Some(crate::dialogue::say(
            if on_ground {
                "position_at_place"
            } else {
                "position_over_place"
            },
            &[("place", place.name.clone())],
        ));
    }
    let lat1 = place.latitude.to_radians();
    let lat2 = latitude.to_radians();
    let delta = (longitude - place.longitude).to_radians();
    let bearing = (delta.sin() * lat2.cos())
        .atan2(lat1.cos() * lat2.sin() - lat1.sin() * lat2.cos() * delta.cos())
        .to_degrees()
        .rem_euclid(360.0);
    let direction = if !(22.5..337.5).contains(&bearing) {
        "north"
    } else if bearing < 67.5 {
        "northeast"
    } else if bearing < 112.5 {
        "east"
    } else if bearing < 157.5 {
        "southeast"
    } else if bearing < 202.5 {
        "south"
    } else if bearing < 247.5 {
        "southwest"
    } else if bearing < 292.5 {
        "west"
    } else {
        "northwest"
    };
    Some(crate::dialogue::say(
        "position_relative_place",
        &[
            ("place", place.name.clone()),
            ("distance", format!("{distance:.0}")),
            ("direction", direction.to_owned()),
        ],
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn reports_real_places_without_coordinates() {
        let report = super::report(17.974_573, 102.570_646, true).unwrap();
        assert!(report.contains("Vientiane"), "{report}");
        assert!(!report.contains("17.974573"));
        let report = super::report(-42.8821, 147.3272, false).unwrap();
        assert!(
            report.contains("Hobart") && report.contains("over"),
            "{report}"
        );
        let ground = super::report(-42.8821, 147.3272, true).unwrap();
        assert!(
            ground.contains("vicinity") && !ground.contains("over"),
            "{ground}"
        );
        assert!(super::report(f64::NAN, 0.0, true).is_none());
        assert!(super::report(0.0, -140.0, false).is_none());
    }
}
