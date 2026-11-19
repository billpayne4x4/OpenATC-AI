//! Ground right-of-way checks shared by generated surface traffic.

/// Whether a target on the right needs a lateral yield.
///
/// The immediate route leader and any aircraft on the same route are excluded. Only a
/// crossing path counts: traffic running the same way on a parallel taxiway, or the
/// opposite way across it, is kept apart along the route instead.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub(crate) fn right_conflict(
    latitude: f64,
    longitude: f64,
    heading: f64,
    other_latitude: f64,
    other_longitude: f64,
    other_heading: f64,
    other_id: u32,
    leader_id: Option<u32>,
    same_route: bool,
    maximum_gap: f64,
) -> bool {
    if same_route || Some(other_id) == leader_id {
        return false;
    }
    let crossing = ((other_heading - heading + 180.0).rem_euclid(360.0) - 180.0).abs();
    if !(30.0..=150.0).contains(&crossing) {
        return false;
    }
    let separation = openatc_core::arrival::distance_nm(
        [latitude, longitude],
        [other_latitude, other_longitude],
    ) * 1852.0;
    if separation > maximum_gap {
        return false;
    }
    let bearing =
        openatc_core::arrival::bearing([latitude, longitude], [other_latitude, other_longitude]);
    let relative = (bearing - heading).rem_euclid(360.0);
    let ahead = separation * relative.to_radians().cos();
    ahead > -10.0 && relative > 10.0 && relative < 170.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_route_leader_is_not_a_right_of_way_conflict() {
        assert!(!right_conflict(
            0.0,
            0.0,
            90.0,
            -0.0004,
            0.0008,
            0.0,
            2,
            Some(2),
            false,
            100.0,
        ));
        assert!(!right_conflict(
            0.0, 0.0, 90.0, -0.0004, 0.0008, 0.0, 2, None, true, 100.0,
        ));
        assert!(right_conflict(
            0.0, 0.0, 90.0, -0.0004, 0.0008, 0.0, 2, None, false, 100.0,
        ));
    }

    #[test]
    fn traffic_on_a_parallel_taxiway_is_not_a_right_of_way_conflict() {
        assert!(!right_conflict(
            0.0, 0.0, 90.0, -0.0004, 0.0008, 90.0, 2, None, false, 100.0,
        ));
        assert!(!right_conflict(
            0.0, 0.0, 90.0, -0.0004, 0.0008, 270.0, 2, None, false, 100.0,
        ));
        assert!(right_conflict(
            0.0, 0.0, 90.0, -0.0004, 0.0008, 0.0, 2, None, false, 100.0,
        ));
    }
}
