//! Debug-only surface repositioning; SDK writes stay on the simulator thread.
use openatc_core::{
    airport::Airport,
    state::{Point, State},
};
use std::ffi::CString;
use xplane_sys as xp;

fn target(kind: &str, state: &State, airport: &Airport) -> Result<(Point, f64, f64, f64), String> {
    if !state.telemetry.on_ground {
        return Err("Surface jumps require the aircraft to be on the ground.".into());
    }
    if kind == "hold" {
        let route = &state.taxi_clearance;
        if !route.approved || !route.crossing_runway.is_empty() {
            return Err("Read back a taxi clearance before jumping to its holding point.".into());
        }
        let end = *route.points.last().ok_or("No holding point available.")?;
        let before = route
            .points
            .iter()
            .rev()
            .skip(1)
            .find(|p| (p.east - end.east).hypot(p.north - end.north) > 1.0)
            .ok_or("No approach direction available.")?;
        return Ok((
            route.holding_marker_point.unwrap_or(end),
            (end.east - before.east)
                .atan2(end.north - before.north)
                .to_degrees()
                .rem_euclid(360.0),
            route.reference_latitude,
            route.reference_longitude,
        ));
    }
    if state.phase != openatc_core::state::PhaseCode::Departure {
        return Err("Obtain takeoff clearance before jumping onto the runway.".into());
    }
    if airport.icao != state.plan.departure {
        return Err("Load the departure airport on the Taxi page first.".into());
    }
    let r = airport
        .runways
        .iter()
        .find(|r| r.first_name == state.plan.runway || r.second_name == state.plan.runway)
        .ok_or("Departure runway unavailable.")?;
    let (a, b) = if r.first_name == state.plan.runway {
        (r.first, r.second)
    } else {
        (r.second, r.first)
    };
    let (de, dn) = (b.east - a.east, b.north - a.north);
    let length = de.hypot(dn);
    if length < 200.0 {
        return Err("Runway too short for this debug jump.".into());
    }
    Ok((
        Point {
            east: a.east + 50.0 * de / length,
            north: a.north + 50.0 * dn / length,
            ..a
        },
        de.atan2(dn).to_degrees().rem_euclid(360.0),
        airport.reference_latitude,
        airport.reference_longitude,
    ))
}

pub fn reposition(kind: &str, state: &State, airport: &Airport) -> Result<(), String> {
    let (point, heading, lat, lon) = target(kind, state, airport)?;
    unsafe {
        let reference = |name: &str| {
            let name = CString::new(name).expect("SDK name");
            xp::XPLMFindDataRef(name.as_ptr())
        };
        let names = [
            "sim/flightmodel/position/local_x",
            "sim/flightmodel/position/local_y",
            "sim/flightmodel/position/local_z",
        ];
        let refs = names.map(reference);
        let path = reference("sim/operation/override/override_planepath");
        if path.is_null()
            || refs
                .iter()
                .any(|r| r.is_null() || xp::XPLMCanWriteDataRef(*r) == 0)
        {
            return Err("Aircraft position controls unavailable.".into());
        }
        let probe = xp::XPLMCreateProbe(xp::XPLMProbeType::Y);
        if probe.is_null() {
            return Err("Terrain probe unavailable.".into());
        }
        let hit = |x: f64, y: f64, z: f64| {
            let mut info: xp::XPLMProbeInfo_t = std::mem::zeroed();
            info.structSize = std::mem::size_of::<xp::XPLMProbeInfo_t>() as i32;
            (xp::XPLMProbeTerrainXYZ(
                probe,
                x as f32,
                (y + 1000.0) as f32,
                z as f32,
                &raw mut info,
            ) == xp::XPLMProbeResult::HitTerrain
                && info.is_wet == 0)
                .then_some(info.locationY as f64)
        };
        let old = refs.map(|r| xp::XPLMGetDatad(r));
        let mut xyz = [0.0; 3];
        xp::XPLMWorldToLocal(
            lat + point.north / 111320.0,
            lon + point.east / (111320.0 * lat.to_radians().cos()),
            state.telemetry.altitude_feet * 0.3048,
            &raw mut xyz[0],
            &raw mut xyz[1],
            &raw mut xyz[2],
        );
        let heights = hit(old[0], old[1], old[2]).zip(hit(xyz[0], xyz[1], xyz[2]));
        xp::XPLMDestroyProbe(probe);
        let Some((old_ground, ground)) = heights else {
            return Err("No dry terrain at the debug target.".into());
        };
        xyz[1] = ground + (old[1] - old_ground).clamp(0.5, 10.0);
        let mut previous = 0;
        xp::XPLMGetDatavi(path, &raw mut previous, 0, 1);
        let mut active = 1;
        xp::XPLMSetDatavi(path, &raw mut active, 0, 1);
        for (r, v) in refs.into_iter().zip(xyz) {
            xp::XPLMSetDatad(r, v);
        }
        for (name, value) in [
            ("sim/flightmodel/position/psi", heading as f32),
            ("sim/flightmodel/position/theta", 0.0),
            ("sim/flightmodel/position/phi", 0.0),
            ("sim/flightmodel/position/local_vx", 0.0),
            ("sim/flightmodel/position/local_vy", 0.0),
            ("sim/flightmodel/position/local_vz", 0.0),
            ("sim/flightmodel/position/P", 0.0),
            ("sim/flightmodel/position/Q", 0.0),
            ("sim/flightmodel/position/R", 0.0),
            ("sim/cockpit2/controls/parking_brake_ratio", 1.0),
        ] {
            let r = reference(name);
            if !r.is_null() && xp::XPLMCanWriteDataRef(r) != 0 {
                xp::XPLMSetDataf(r, value);
            }
        }
        xp::XPLMSetDatavi(path, &raw mut previous, 0, 1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn jumps_require_surface_permissions_and_use_the_safe_hold_target() {
        let mut state = State::default();
        state.telemetry.on_ground = true;
        let airport = Airport::default();
        assert!(target("hold", &state, &airport).is_err());
        assert!(target("runway", &state, &airport).is_err());
        state.taxi_clearance.approved = true;
        state.taxi_clearance.points = vec![
            Point {
                north: 0.0,
                ..Default::default()
            },
            Point {
                north: 100.0,
                ..Default::default()
            },
        ];
        state.taxi_clearance.holding_marker_point = Some(Point {
            north: 81.0,
            ..Default::default()
        });
        let (position, heading, _, _) = target("hold", &state, &airport).unwrap();
        assert_eq!(position.north, 81.0);
        assert_eq!(heading, 0.0);
        state.telemetry.on_ground = false;
        assert!(target("hold", &state, &airport).is_err());
    }
}
