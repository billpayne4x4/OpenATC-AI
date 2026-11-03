//! Keep nearby visual slots bounded without removing flights from the movement world.
#[allow(unused_imports, clippy::wildcard_imports)]
use super::*;
use super::{BTreeSet, GeneratedTraffic, Operations};
use openatc_core::state::Telemetry;

pub(super) fn select(
    frames: &[GeneratedTraffic],
    telemetry: &Telemetry,
    previous: &BTreeSet<u32>,
    policy: &Operations,
) -> Vec<GeneratedTraffic> {
    if !telemetry.position_valid
        || !telemetry.latitude.is_finite()
        || !telemetry.longitude.is_finite()
        || !(-90.0..=90.0).contains(&telemetry.latitude)
        || !(-180.0..=180.0).contains(&telemetry.longitude)
    {
        return Vec::new();
    }
    let mut nearby: Vec<_> = frames
        .iter()
        .filter_map(|frame| {
            let target = &frame.target;
            if target.id == 0
                || !target.latitude.is_finite()
                || !target.longitude.is_finite()
                || !(-90.0..=90.0).contains(&target.latitude)
                || !(-180.0..=180.0).contains(&target.longitude)
            {
                return None;
            }
            let retained = previous.contains(&target.id);
            let distance = openatc_core::arrival::distance_nm(
                [telemetry.latitude, telemetry.longitude],
                [target.latitude, target.longitude],
            );
            let range = if retained {
                policy.display_leave_nm
            } else {
                policy.display_enter_nm
            };
            (distance <= range).then_some((
                distance
                    - if retained {
                        policy.display_replacement_margin_nm
                    } else {
                        0.0
                    },
                frame,
            ))
        })
        .collect();
    // Stable identity breaks ties independently of scene iteration order. Retained
    // aircraft have a small distance advantage to avoid churn at a full slot budget.
    nearby.sort_by(|a, b| {
        a.0.total_cmp(&b.0)
            .then_with(|| a.1.target.id.cmp(&b.1.target.id))
    });
    let track = telemetry
        .ground_track_degrees
        .unwrap_or(telemetry.heading_degrees);
    let mut ids = BTreeSet::new();
    let mut selected = Vec::with_capacity(policy.visible_aircraft_limit);
    let mut same_direction = 0usize;
    let mut trailing = 0usize;
    for (distance, frame) in nearby {
        if ids.contains(&frame.target.id) {
            continue;
        }
        if distance <= policy.same_direction_range_nm {
            let bearing = openatc_core::arrival::bearing(
                [telemetry.latitude, telemetry.longitude],
                [frame.target.latitude, frame.target.longitude],
            );
            let aligned = openatc_core::navigation::turn(track, frame.target.track_degrees).abs()
                <= policy.same_direction_cone_degrees;
            if aligned {
                let behind = openatc_core::navigation::turn(track, bearing).abs() > 90.0;
                if same_direction >= policy.same_direction_limit
                    || behind && trailing >= policy.trailing_limit
                {
                    continue;
                }
                same_direction += 1;
                if behind {
                    trailing += 1;
                }
            }
        }
        ids.insert(frame.target.id);
        selected.push(frame.clone());
        if selected.len() >= policy.visible_aircraft_limit {
            break;
        }
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(id: u32, nm: f64) -> GeneratedTraffic {
        let coordinate = openatc_core::arrival::destination([40.0, 8.0], 90.0, nm);
        GeneratedTraffic {
            target: TrafficTarget {
                id,
                latitude: coordinate[0],
                longitude: coordinate[1],
                ..Default::default()
            },
            model: Arc::new(openatc_core::traffic::csl::Model {
                id: "TEST".into(),
                objects: vec![],
                vertical_offset: None,
                matches: vec![],
            }),
            vertical_speed_fpm: 0.0,
            body_heading_degrees: None,
            animation: Animation::default(),
        }
    }
    #[test]
    fn spoken_operator_uses_the_same_flight_number_without_replacing_substrings() {
        let mut runtime = Runtime::default();
        runtime
            .spoken_callsigns
            .insert("SIA1975".into(), "Singapore 1975".into());
        assert_eq!(
            runtime.spoken_text("SIA1975, taxi via Alpha. SIA1975."),
            "Singapore 1975, taxi via Alpha. Singapore 1975."
        );
        assert_eq!(
            runtime.spoken_text("SIA19750 XSIA1975 F-GABC"),
            "SIA19750 XSIA1975 F-GABC"
        );
        assert_eq!(
            runtime.spoken_text("Traffic café: SIA1975!"),
            "Traffic café: Singapore 1975!"
        );
    }

    #[test]
    fn display_turnover_keeps_flight_state_and_all_separation_observations() {
        let mut state = crate::test_state();
        state.settings.generated_traffic = true;
        state.generated.epoch = Some(state.session_generation);
        let mut policy =
            parse_operations(include_str!("../../../../traffic/operations.toml")).unwrap();
        policy.visible_aircraft_limit = 1;
        state.generated.policy = Some(policy);
        state.generated.frames = vec![frame(1, 10.0), frame(2, 20.0), frame(3, 80.0)];
        state.session.telemetry = Telemetry {
            position_valid: true,
            latitude: 40.0,
            longitude: 8.0,
            traffic: Some(vec![]),
            ..Default::default()
        };
        step(&mut state);
        assert_eq!(state.session.generated_traffic[0].target.id, 1);
        let coordinate = [
            state.generated.frames[2].target.latitude,
            state.generated.frames[2].target.longitude,
        ];
        state.session.telemetry.latitude = coordinate[0];
        state.session.telemetry.longitude = coordinate[1];
        step(&mut state);
        assert_eq!(state.session.generated_traffic[0].target.id, 3);
        assert_eq!(
            state.generated.frames.len(),
            3,
            "Culling cannot delete flight state"
        );
        let observed = state.session.telemetry.traffic.as_ref().unwrap();
        assert_eq!(
            observed.len(),
            3,
            "Distant targets still protect separation and wake"
        );
        assert!(observed.iter().any(|target| target.id == 1));
        state.session.telemetry.position_valid = false;
        step(&mut state);
        assert!(state.session.generated_traffic.is_empty());
        assert_eq!(state.generated.frames.len(), 3);
    }
    #[test]
    fn same_direction_flow_is_bounded_without_hiding_other_traffic() {
        let mut policy =
            parse_operations(include_str!("../../../../traffic/operations.toml")).unwrap();
        policy.same_direction_limit = 5;
        policy.trailing_limit = 2;
        let own = Telemetry {
            position_valid: true,
            latitude: 40.0,
            longitude: 8.0,
            heading_degrees: 90.0,
            ground_track_degrees: Some(90.0),
            ..Default::default()
        };
        let mut frames = Vec::new();
        for index in 0..3 {
            let mut frame = frame(index + 1, 10.0);
            frame.target.track_degrees = 90.0;
            frames.push(frame);
        }
        for index in 0..5 {
            let mut frame = frame(index + 4, 10.0);
            frame.target.latitude = 40.0;
            frame.target.longitude = 8.0 - 10.0 / 60.0;
            frame.target.track_degrees = 90.0;
            frames.push(frame);
        }
        let selected = select(&frames, &own, &BTreeSet::new(), &policy);
        assert_eq!(selected.len(), 5);
        let behind = selected
            .iter()
            .filter(|frame| frame.target.longitude < own.longitude)
            .count();
        assert_eq!(behind, 2);
    }

    #[test]
    fn range_and_capacity_margins_prevent_churn_without_locking_out_nearer_flights() {
        let mut policy =
            parse_operations(include_str!("../../../../traffic/operations.toml")).unwrap();
        let own = Telemetry {
            position_valid: true,
            latitude: 40.0,
            longitude: 8.0,
            ..Default::default()
        };
        let frames = vec![frame(1, 45.0), frame(2, 51.0)];
        assert!(select(&frames, &own, &BTreeSet::new(), &policy).is_empty());
        assert_eq!(
            select(&frames, &own, &BTreeSet::from([1, 2]), &policy)
                .iter()
                .map(|f| f.target.id)
                .collect::<Vec<_>>(),
            vec![1]
        );
        policy.visible_aircraft_limit = 1;
        let previous = BTreeSet::from([1]);
        assert_eq!(
            select(&[frame(1, 10.0), frame(2, 9.0)], &own, &previous, &policy)[0]
                .target
                .id,
            1
        );
        assert_eq!(
            select(&[frame(1, 10.0), frame(2, 7.0)], &own, &previous, &policy)[0]
                .target
                .id,
            2
        );
        assert_eq!(
            select(
                &[frame(2, 10.0), frame(1, 10.0)],
                &own,
                &BTreeSet::new(),
                &policy
            )[0]
            .target
            .id,
            1
        );
        let mut invalid = frame(4, 1.0);
        invalid.target.latitude = f64::NAN;
        assert!(select(&[invalid], &own, &previous, &policy).is_empty());
    }

    #[test]
    fn publishing_generated_frames_keeps_the_ambient_slice() {
        let mut state = crate::test_state();
        state.settings.generated_traffic = true;
        state.generated.epoch = Some(state.session_generation);
        state.generated.policy =
            Some(parse_operations(include_str!("../../../../traffic/operations.toml")).unwrap());
        state.generated.frames = vec![frame(1, 10.0)];
        state.session.telemetry = Telemetry {
            position_valid: true,
            latitude: 40.0,
            longitude: 8.0,
            traffic: Some(vec![]),
            ..Default::default()
        };
        state.session.generated_traffic = vec![frame(0x00e0_0000, 12.0)];
        step(&mut state);
        assert!(
            state
                .session
                .generated_traffic
                .iter()
                .any(|frame| frame.target.id == 1)
        );
        assert!(
            state
                .session
                .generated_traffic
                .iter()
                .any(|frame| frame.target.id == 0x00e0_0000)
        );

        state.generated.published_ids.insert(99);
        state.session.generated_traffic.push(frame(99, 9.0));
        step(&mut state);
        assert!(
            state
                .session
                .generated_traffic
                .iter()
                .all(|frame| frame.target.id != 99),
            "a retired generated frame must leave the published set"
        );

        state.settings.generated_traffic = false;
        step(&mut state);
        assert!(
            state
                .session
                .generated_traffic
                .iter()
                .all(|frame| frame.target.id != 1)
        );
        assert!(
            state
                .session
                .generated_traffic
                .iter()
                .any(|frame| frame.target.id == 0x00e0_0000),
            "ambient traffic must survive when generated traffic is switched off"
        );
    }
}
