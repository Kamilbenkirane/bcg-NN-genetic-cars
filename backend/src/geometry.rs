use anyhow::{Result, ensure};
use bytemuck::{Pod, Zeroable};

use crate::model::Track;

#[derive(Clone, Copy, Debug, Pod, Zeroable)]
#[repr(C)]
pub struct GpuWall {
    pub a: [f32; 2],
    pub b: [f32; 2],
    pub kind: u32,
    pub _pad: [u32; 3],
}

#[derive(Clone, Copy, Debug, Pod, Zeroable)]
#[repr(C)]
pub struct GpuBvhNode {
    pub bounds: [f32; 4],
    pub first: u32,
    pub count: u32,
    pub escape: u32,
    pub _pad: u32,
}

#[derive(Clone, Copy, Debug, Pod, Zeroable)]
#[repr(C)]
pub struct GpuCenterSegment {
    pub a: [f32; 2],
    pub delta: [f32; 2],
    pub length_sq: f32,
    pub length: f32,
    pub cumulative: f32,
    pub _pad: f32,
}

#[derive(Clone, Debug)]
pub struct PreparedTrack {
    pub track: Track,
    pub walls: Vec<GpuWall>,
    pub nodes: Vec<GpuBvhNode>,
    pub center_segments: Vec<GpuCenterSegment>,
    pub spawn: [f32; 3],
    pub spawn_distance: f32,
    pub lap_length: f32,
    pub finish_direction: [f32; 2],
    pub tolerance: f32,
}

pub fn prepare(track: &Track) -> Result<PreparedTrack> {
    from_points(track.points.clone(), track.width)
}

pub(crate) fn from_points(points: Vec<[f32; 2]>, width: f32) -> Result<PreparedTrack> {
    ensure!(
        (4..=400).contains(&points.len()) && points.first() == points.last(),
        "a circuit must contain 4–400 points with a closed centerline"
    );
    ensure!(width.is_finite() && width > 0.0, "invalid road width");
    ensure!(
        points.iter().flatten().all(|v| v.is_finite()),
        "non-finite road coordinate"
    );
    let max_coordinate = points.iter().flatten().fold(1.0f32, |a, &b| a.max(b.abs()));
    let tolerance = (width * 1e-6).max(max_coordinate * f32::EPSILON * 4.0);
    ensure!(
        tolerance < width * 0.01,
        "road coordinates exceed useful f32 precision"
    );
    let mut center_segments = Vec::with_capacity(points.len() - 1);
    let mut directions = Vec::with_capacity(points.len() - 1);
    let mut total_length = 0.0f32;
    for pair in points.windows(2) {
        let delta = [pair[1][0] - pair[0][0], pair[1][1] - pair[0][1]];
        let length_sq = delta[0] * delta[0] + delta[1] * delta[1];
        let length = length_sq.sqrt();
        ensure!(
            length.is_finite() && length > tolerance * 4.0,
            "collapsed road segment"
        );
        directions.push([delta[0] / length, delta[1] / length]);
        center_segments.push(GpuCenterSegment {
            a: pair[0],
            delta,
            length_sq,
            length,
            cumulative: total_length,
            _pad: 0.0,
        });
        total_length += length;
    }
    let normals: Vec<_> = directions.iter().map(|d| [-d[1], d[0]]).collect();
    let mut left = Vec::with_capacity(points.len());
    let mut right = Vec::with_capacity(points.len());
    for (i, p) in points.iter().enumerate() {
        let offset = {
            let next = i % directions.len();
            let previous = (next + directions.len() - 1) % directions.len();
            let dot = directions[previous][0] * directions[next][0]
                + directions[previous][1] * directions[next][1];
            let denominator = 1.0 + dot;
            ensure!(denominator > 1e-6, "road turns back on itself");
            let offset = [
                (normals[previous][0] + normals[next][0]) / denominator,
                (normals[previous][1] + normals[next][1]) / denominator,
            ];
            ensure!(
                offset[0].hypot(offset[1]) <= 5.0,
                "road join exceeds mitre limit"
            );
            offset
        };
        left.push([
            p[0] + offset[0] * width * 0.5,
            p[1] + offset[1] * width * 0.5,
        ]);
        right.push([
            p[0] - offset[0] * width * 0.5,
            p[1] - offset[1] * width * 0.5,
        ]);
    }
    for side in [&left, &right] {
        for (pair, direction) in side.windows(2).zip(&directions) {
            let projected =
                (pair[1][0] - pair[0][0]) * direction[0] + (pair[1][1] - pair[0][1]) * direction[1];
            ensure!(projected > tolerance, "road join collapses an edge");
        }
    }
    // Validate both closed boundaries at the precision used by the GPU.
    let edges = points.len() - 1;
    let mut walls: Vec<_> = [&left, &right]
        .into_iter()
        .flat_map(|side| side.windows(2))
        .enumerate()
        .map(|(i, pair)| GpuWall {
            a: pair[0],
            b: pair[1],
            kind: 0,
            _pad: [i as u32, 0, 0],
        })
        .collect();
    for i in 0..walls.len() {
        let wall = walls[i];
        ensure!(
            (wall.a[0] - wall.b[0]).hypot(wall.a[1] - wall.b[1]) > tolerance,
            "collapsed road boundary"
        );
        for (j, other) in walls.iter().enumerate().skip(i + 1) {
            if i / edges == j / edges && (j == i + 1 || j - i == edges - 1) {
                continue;
            }
            ensure!(
                !segments_touch(wall.a, wall.b, other.a, other.b, tolerance),
                "road overlaps itself"
            );
        }
    }
    let first = center_segments[0];
    let spawn_distance = first.length * 0.5;
    let spawn = [
        points[0][0] + first.delta[0] * 0.5,
        points[0][1] + first.delta[1] * 0.5,
        directions[0][1].atan2(directions[0][0]),
    ];
    let lap_length = total_length;
    // A passable timing gate, halfway along the first segment. Sensors ignore it.
    walls.push(GpuWall {
        a: [
            (left[0][0] + left[1][0]) * 0.5,
            (left[0][1] + left[1][1]) * 0.5,
        ],
        b: [
            (right[0][0] + right[1][0]) * 0.5,
            (right[0][1] + right[1][1]) * 0.5,
        ],
        kind: 1,
        _pad: [walls.len() as u32, 0, 0],
    });
    let mut nodes = Vec::with_capacity(walls.len() * 2);
    build_bvh(&mut walls, 0, &mut nodes);
    let finish_direction = directions[0];
    Ok(PreparedTrack {
        track: Track {
            points,
            left,
            right,
            width,
            spawn,
            spawn_distance,
            lap_length,
        },
        walls,
        nodes,
        center_segments,
        spawn,
        spawn_distance,
        lap_length,
        finish_direction,
        tolerance,
    })
}

fn segments_touch(a: [f32; 2], b: [f32; 2], c: [f32; 2], d: [f32; 2], tolerance: f32) -> bool {
    let t = f64::from(tolerance);
    for axis in 0..2 {
        if f64::from(a[axis].max(b[axis])) + t < f64::from(c[axis].min(d[axis]))
            || f64::from(c[axis].max(d[axis])) + t < f64::from(a[axis].min(b[axis]))
        {
            return false;
        }
    }
    fn cross(a: [f32; 2], b: [f32; 2], p: [f32; 2]) -> f64 {
        (f64::from(b[0]) - f64::from(a[0])) * (f64::from(p[1]) - f64::from(a[1]))
            - (f64::from(b[1]) - f64::from(a[1])) * (f64::from(p[0]) - f64::from(a[0]))
    }
    let ab_t = t * f64::from((b[0] - a[0]).hypot(b[1] - a[1]));
    let cd_t = t * f64::from((d[0] - c[0]).hypot(d[1] - c[1]));
    let (ac, ad, ca, cb) = (
        cross(a, b, c),
        cross(a, b, d),
        cross(c, d, a),
        cross(c, d, b),
    );
    !((ac > ab_t && ad > ab_t)
        || (ac < -ab_t && ad < -ab_t)
        || (ca > cd_t && cb > cd_t)
        || (ca < -cd_t && cb < -cd_t))
}

fn build_bvh(walls: &mut [GpuWall], offset: usize, nodes: &mut Vec<GpuBvhNode>) {
    let index = nodes.len();
    let mut bounds = [
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    ];
    for wall in walls.iter() {
        for p in [wall.a, wall.b] {
            bounds[0] = bounds[0].min(p[0]);
            bounds[1] = bounds[1].min(p[1]);
            bounds[2] = bounds[2].max(p[0]);
            bounds[3] = bounds[3].max(p[1]);
        }
    }
    nodes.push(GpuBvhNode {
        bounds,
        first: offset as u32,
        count: 0,
        escape: 0,
        _pad: 0,
    });
    if walls.len() <= 4 {
        nodes[index].count = walls.len() as u32;
    } else {
        let axis = usize::from(bounds[3] - bounds[1] > bounds[2] - bounds[0]);
        walls.sort_by(|a, b| {
            (a.a[axis] + a.b[axis])
                .total_cmp(&(b.a[axis] + b.b[axis]))
                .then(a._pad[0].cmp(&b._pad[0]))
        });
        let mid = walls.len() / 2;
        let (left, right) = walls.split_at_mut(mid);
        build_bvh(left, offset, nodes);
        build_bvh(right, offset + mid, nodes);
    }
    nodes[index].escape = nodes.len() as u32;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prepared_track_contract() {
        let circuit = crate::circuits::catalog().unwrap().remove(0);
        let a = prepare(&circuit.track).unwrap();
        let b = prepare(&a.track).unwrap();
        assert_eq!(
            bytemuck::cast_slice::<_, u8>(&a.walls),
            bytemuck::cast_slice::<_, u8>(&b.walls)
        );
        assert_eq!(a.track.points.first(), a.track.points.last());
        assert_eq!(a.track.left.first(), a.track.left.last());
        assert_eq!(a.track.right.first(), a.track.right.last());
        assert!(a.lap_length > 600.0);
        assert_eq!(a.nodes[0].escape as usize, a.nodes.len());
        assert_eq!(a.walls.iter().filter(|wall| wall.kind == 1).count(), 1);
        assert_eq!(std::mem::size_of::<GpuWall>(), 32);
        assert_eq!(std::mem::size_of::<GpuBvhNode>(), 32);
        assert_eq!(std::mem::size_of::<GpuCenterSegment>(), 32);
        assert!(
            from_points(
                vec![[0., 0.], [40., 40.], [0., 40.], [40., 0.], [0., 0.]],
                10.
            )
            .is_err()
        );
    }
}
