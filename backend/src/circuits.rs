use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

use crate::{
    geometry,
    model::{Circuit, RECORD_RULES, RunConfig, RunStage, SIMULATION_HZ, driving_budget},
};

pub fn tour(circuits: &[Circuit], config: &RunConfig) -> Result<Vec<RunStage>> {
    config.validate()?;
    ensure!(!circuits.is_empty(), "No circuits available");
    let seed = config.seed_value()?.to_le_bytes();
    let mut ordered = circuits.to_vec();
    ordered.sort_by_cached_key(|c| {
        let mut hash = Sha256::new();
        hash.update(b"circuit-order-v1");
        hash.update(seed);
        hash.update(c.id.as_bytes());
        (hash.finalize().to_vec(), c.id.clone())
    });
    ordered
        .into_iter()
        .enumerate()
        .map(|(index, circuit)| {
            let mut hash = Sha256::new();
            hash.update(RECORD_RULES);
            hash.update(SIMULATION_HZ.to_le_bytes());
            hash.update(serde_json::to_vec(&circuit.track)?);
            for value in [
                config.vehicle.step_distance,
                config.vehicle.sensor_range,
                config.vehicle.max_heading_change,
            ] {
                hash.update((if value == 0.0 { 0.0 } else { value }).to_le_bytes());
            }
            Ok(RunStage {
                index: index as u32,
                first_generation: index as u32 * config.training.generations_per_circuit,
                max_steps: driving_budget(config, circuit.track.lap_length)?,
                record_key: format!("{:x}", hash.finalize()),
                baseline_record_id: None,
                circuit,
            })
        })
        .collect()
}

/// Fixed layouts: coordinates describe corner intersections, radius rounds each corner.
pub fn catalog() -> Result<Vec<Circuit>> {
    Ok(vec![
        circuit(
            "switchback",
            "Switchback",
            "Hairpins",
            "Repeated hairpins with little time to straighten out.",
            11.,
            22.,
            &[
                [0., 0.],
                [240., 0.],
                [240., 60.],
                [60., 60.],
                [60., 120.],
                [240., 120.],
                [240., 180.],
                [0., 180.],
            ],
        )?,
        circuit(
            "chicane",
            "Chicane",
            "Quick reversals",
            "Two tight chicane sequences demand precise changes of direction.",
            12.,
            18.,
            &[
                [0., 0.],
                [80., 0.],
                [80., 50.],
                [140., 50.],
                [140., 0.],
                [220., 0.],
                [220., 160.],
                [160., 160.],
                [160., 110.],
                [100., 110.],
                [100., 160.],
                [0., 160.],
            ],
        )?,
        circuit(
            "needle",
            "Needle",
            "Tight returns",
            "Long approaches lead into two narrow hairpin returns.",
            10.,
            20.,
            &[
                [0., 0.],
                [330., 0.],
                [330., 44.],
                [120., 44.],
                [120., 105.],
                [270., 105.],
                [270., 149.],
                [0., 149.],
            ],
        )?,
        circuit(
            "esses",
            "Esses",
            "Linked bends",
            "Alternating bends reward a controlled line from one corner into the next.",
            11.,
            18.,
            &[
                [0., 0.],
                [70., -40.],
                [140., 0.],
                [200., -40.],
                [260., 20.],
                [220., 90.],
                [270., 160.],
                [200., 210.],
                [140., 160.],
                [70., 210.],
                [0., 150.],
                [40., 80.],
            ],
        )?,
        circuit(
            "infield",
            "Infield",
            "Technical",
            "A compact sequence of tight corners and deep infield turns.",
            10.,
            18.,
            &[
                [0., 0.],
                [200., 0.],
                [200., 150.],
                [150., 150.],
                [150., 50.],
                [100., 50.],
                [100., 150.],
                [0., 150.],
                [0., 100.],
                [50., 100.],
                [50., 50.],
                [0., 50.],
            ],
        )?,
        circuit(
            "gauntlet",
            "Gauntlet",
            "Mixed challenge",
            "Hairpins, linked bends and an awkward infield test consistency over many laps.",
            10.,
            18.,
            &[
                [0., 0.],
                [130., -20.],
                [240., 0.],
                [270., 60.],
                [210., 90.],
                [270., 150.],
                [230., 210.],
                [150., 210.],
                [150., 130.],
                [90., 130.],
                [90., 210.],
                [0., 180.],
                [35., 105.],
                [-25., 60.],
            ],
        )?,
    ])
}

fn circuit(
    id: &str,
    name: &str,
    character: &str,
    description: &str,
    width: f32,
    radius: f64,
    corners: &[[f64; 2]],
) -> Result<Circuit> {
    let points = rounded_loop(corners, radius).with_context(|| format!("Circuit {id}"))?;
    let track = geometry::from_points(points, width)
        .with_context(|| format!("Circuit {id}"))?
        .track;
    ensure!(
        (600.0..=1200.0).contains(&track.lap_length),
        "Circuit {id} length is outside its budget"
    );
    Ok(Circuit {
        id: id.into(),
        name: name.into(),
        character: character.into(),
        description: description.into(),
        track,
    })
}

fn rounded_loop(corners: &[[f64; 2]], radius: f64) -> Result<Vec<[f32; 2]>> {
    let count = corners.len();
    let mut starts = Vec::with_capacity(count);
    let mut ends = Vec::with_capacity(count);
    let mut centers = Vec::with_capacity(count);
    let mut turns = Vec::with_capacity(count);
    for (i, &p) in corners.iter().enumerate() {
        let previous = corners[(i + count - 1) % count];
        let next = corners[(i + 1) % count];
        let unit = |a: [f64; 2], b: [f64; 2]| {
            let d = [b[0] - a[0], b[1] - a[1]];
            let length = d[0].hypot(d[1]);
            [d[0] / length, d[1] / length]
        };
        let incoming = unit(previous, p);
        let outgoing = unit(p, next);
        let turn = (incoming[0] * outgoing[1] - incoming[1] * outgoing[0])
            .atan2(incoming[0] * outgoing[0] + incoming[1] * outgoing[1]);
        let tangent = radius * (turn.abs() / 2.0).tan();
        let start = [p[0] - incoming[0] * tangent, p[1] - incoming[1] * tangent];
        starts.push(start);
        ends.push([p[0] + outgoing[0] * tangent, p[1] + outgoing[1] * tangent]);
        centers.push([
            start[0] - incoming[1] * radius * turn.signum(),
            start[1] + incoming[0] * radius * turn.signum(),
        ]);
        turns.push(turn);
    }
    let mut points = Vec::new();
    let mut longest_straight = 0.0;
    let mut spawn_index = 0;
    for i in 0..count {
        let previous = (i + count - 1) % count;
        let start = ends[previous];
        let end = starts[i];
        let delta = [end[0] - start[0], end[1] - start[1]];
        let edge = [
            corners[i][0] - corners[previous][0],
            corners[i][1] - corners[previous][1],
        ];
        ensure!(
            delta[0] * edge[0] + delta[1] * edge[1] > 0.0,
            "Corner radii overlap"
        );
        let length = delta[0].hypot(delta[1]);
        let samples = (length / 5.0).ceil() as usize;
        if length > longest_straight {
            longest_straight = length;
            spawn_index = points.len() + samples / 2;
        }
        for j in 0..samples {
            let t = j as f64 / samples as f64;
            points.push([
                (start[0] + t * delta[0]) as f32,
                (start[1] + t * delta[1]) as f32,
            ]);
        }
        let center = centers[i];
        let angle = (end[1] - center[1]).atan2(end[0] - center[0]);
        let samples = (turns[i].abs() * radius / 5.0).ceil() as usize;
        for j in 0..samples {
            let theta = angle + turns[i] * j as f64 / samples as f64;
            points.push([
                (center[0] + radius * theta.cos()) as f32,
                (center[1] + radius * theta.sin()) as f32,
            ]);
        }
    }
    points.rotate_left(spawn_index);
    points.push(points[0]);
    Ok(points)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{RunConfig, driving_budget};

    #[test]
    fn tours_are_seeded_and_records_match_driving_rules() -> Result<()> {
        let circuits = catalog()?;
        let mut config = RunConfig::default();
        let a = tour(&circuits, &config)?;
        let ids = |stages: &[RunStage]| {
            stages
                .iter()
                .map(|s| s.circuit.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&a), ids(&tour(&circuits, &config)?));
        assert_eq!(a.len(), 6);
        assert_eq!(a[5].first_generation, 250);
        config.seed = "43".into();
        config.training.population = 2000;
        config.training.target_laps = 1;
        let b = tour(&circuits, &config)?;
        assert_ne!(ids(&a), ids(&b));
        for stage in &a {
            assert_eq!(
                stage.record_key,
                b.iter()
                    .find(|s| s.circuit.id == stage.circuit.id)
                    .unwrap()
                    .record_key
            );
        }
        config.vehicle.step_distance = 6.0;
        let c = tour(&circuits, &config)?;
        assert_ne!(b[0].record_key, c[0].record_key);
        Ok(())
    }

    #[test]
    fn circuits_are_closed_feasible_and_fit_the_endurance_budget() -> Result<()> {
        let circuits = catalog()?;
        assert_eq!(circuits.len(), 6);
        for circuit in circuits {
            let track = circuit.track;
            assert_eq!(track.points.first(), track.points.last());
            let prepared = geometry::prepare(&track)?;
            assert_eq!(prepared.track.left, track.left);
            assert_eq!(prepared.track.right, track.right);
            let directions: Vec<_> = track
                .points
                .windows(2)
                .map(|p| {
                    let d = [p[1][0] - p[0][0], p[1][1] - p[0][1]];
                    (d[1].atan2(d[0]), d[0].hypot(d[1]))
                })
                .collect();
            for i in 0..directions.len() {
                let (angle, length) = directions[i];
                let (next, next_length) = directions[(i + 1) % directions.len()];
                let turn = (next - angle).sin().atan2((next - angle).cos()).abs();
                // A five-unit step must be achievable with the default steering limit.
                assert!(
                    turn / ((length + next_length) * 0.5) * 5.0 < std::f32::consts::PI / 8.0,
                    "{} has an impossible corner",
                    circuit.id
                );
            }
            let mut config = RunConfig::default();
            config.training.target_laps = 100;
            assert!(driving_budget(&config, track.lap_length)? <= 100_000);
            config.vehicle.step_distance = 0.01;
            assert!(driving_budget(&config, track.lap_length).is_err());
        }
        Ok(())
    }
}
