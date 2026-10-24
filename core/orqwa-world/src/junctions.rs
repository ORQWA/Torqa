//! Where streets meet: a rounded kerb in every corner between them, so a junction looks laid
//! out rather than like strips overlapping at odd angles with sharp corners (#74). The map
//! joins streets at shared points; each street through or ending there is an arm, and between
//! neighbouring arms the corner is filled up to an arc touching both their edges. Where a
//! street meets the road ridden, the road is the arm, at its own line and width.

use std::collections::HashMap;

use crate::drape;
use crate::road::RoadIndex;
use crate::streets::Street;
use crate::{HeightGrid, MeshData, On, ROAD_SINK};

/// Streets this close to a point (metres) meet there.
const MEET_M: f64 = 1.0;
/// An arm's direction is taken this far out from the junction, past the corner...
const ARM_M: f64 = 6.0;
/// ...and it runs on at least this far, or its street ends there.
const ARM_MIN_M: f64 = 2.0;
/// Corners this wide (radians) get a kerb; narrower ones are mostly the streets themselves and
/// wider ones barely a corner.
const CORNER: (f64, f64) = (0.25, 3.0);
/// Acute forks need tangent points farther along both arms to round their nose.
const CORNER_REACH_M: f64 = 40.0;
/// The kerb's radius, as a share of the two arms' half widths together, and its bounds.
const RADIUS_SHARE: f64 = 0.6;
const RADIUS_M: (f64, f64) = (0.8, 4.5);
/// Arc pieces are at most this wide (radians).
const ARC_STEP: f64 = 0.1;
/// A kerb is laid only where its ends and corner lie this close to the streets' real edges:
/// streets curving away right at the junction, or meeting at points a few metres apart, have
/// edges other than the straight ones it is drawn from.
const EDGE_FIT_M: f64 = 0.5;
/// Corners lie this far below the lower of the two streets, so where they overlap the streets
/// show.
const BELOW_M: f64 = 0.003;

/// The corner between two arms of a junction, filled up to its kerb: a fan from the point
/// where their edges meet over the arc, in metres east/north.
pub(crate) struct Corner {
    apex: (f64, f64),
    arc: Vec<(f64, f64)>,
    lift: f64,
    paved: bool,
    min: (f64, f64),
    max: (f64, f64),
    centre: (f64, f64),
    radius: f64,
    end_lifts: [f64; 2],
    footway_bases: [((f64, f64), bool); 2],
}

/// A sidewalk following the same curved edge as its junction's carriageway.
pub(crate) struct FootwayCorner {
    /// Street-centre anchors at the tangent cross-sections; true for the ridden road.
    pub bases: [((f64, f64), bool); 2],
    /// Region replaced by the curved sidewalk, between its tangent cross-sections.
    pub zone: Vec<(f64, f64)>,
    /// Convex pieces, with street heights at their two cross-sections.
    pub sections: Vec<FootwaySection>,
}

/// A convex piece of a curved footway and its two street heights.
pub(crate) struct FootwaySection {
    /// Four corners, with the road-facing edge first.
    pub polygon: Vec<(f64, f64)>,
    /// Street heights at the first and second cross-sections.
    pub lifts: [f64; 2],
    /// Fractions along the complete corner's shared height profile.
    pub span: [f64; 2],
}

impl Corner {
    /// The points along its kerb, to keep plants off.
    pub(crate) fn outline(&self) -> impl Iterator<Item = (f64, f64)> + '_ {
        std::iter::once(self.apex).chain(self.arc.iter().copied())
    }

    /// A constant-width footway on the land side of the existing road fillet.
    pub(crate) fn footway(&self, inset: f64, width: f64) -> Option<FootwayCorner> {
        if !self.paved || self.radius <= inset + width + 0.1 {
            return None;
        }
        let offset = |p: (f64, f64), amount: f64| {
            let share = 1.0 - amount / self.radius;
            (
                self.centre.0 + (p.0 - self.centre.0) * share,
                self.centre.1 + (p.1 - self.centre.1) * share,
            )
        };
        #[allow(clippy::cast_precision_loss)] // a few arc sections
        let fraction = |i: usize| i as f64 / (self.arc.len() - 1) as f64;
        let lift =
            |i: usize| self.end_lifts[0] + (self.end_lifts[1] - self.end_lifts[0]) * fraction(i);
        Some(FootwayCorner {
            bases: self.footway_bases,
            zone: vec![
                self.apex,
                self.arc[0],
                self.centre,
                self.arc[self.arc.len() - 1],
            ],
            sections: self
                .arc
                .windows(2)
                .enumerate()
                .map(|(i, pair)| FootwaySection {
                    polygon: vec![
                        offset(pair[0], inset),
                        offset(pair[1], inset),
                        offset(pair[1], inset + width),
                        offset(pair[0], inset + width),
                    ],
                    lifts: [lift(i), lift(i + 1)],
                    span: [fraction(i), fraction(i + 1)],
                })
                .collect(),
        })
    }
}

/// A street leaving a junction: its centre line near the junction, which way it leaves, its
/// half width, and how it is drawn.
#[derive(Debug, Clone, Copy)]
struct Arm {
    from: (f64, f64),
    direction: (f64, f64),
    half: f64,
    /// How far it runs on from the junction.
    reach: f64,
    lift: f64,
    paved: bool,
    /// Its street, or none for the road ridden.
    street: Option<usize>,
}

/// The corners of every junction of `streets` (bridges left out: their decks stand above the
/// ground), with the road ridden standing in for the stretches of streets drawn as it.
pub(crate) fn corners(streets: &[Street], road: &RoadIndex) -> Vec<Corner> {
    let on_ground: Vec<&Street> = streets
        .iter()
        .filter(|s| !s.on_bridge() && s.points().len() >= 2)
        .collect();
    let index = SegmentIndex::new(&on_ground);
    let mut corners = Vec::new();
    let mut done: HashMap<(i64, i64), Vec<(f64, f64)>> = HashMap::new();
    for at in meeting_points(&on_ground) {
        let key = cell(at, MEET_M * 2.0);
        let near_done = (key.0 - 1..=key.0 + 1).any(|x| {
            (key.1 - 1..=key.1 + 1).any(|y| {
                done.get(&(x, y))
                    .is_some_and(|points| points.iter().any(|&p| distance(p, at) < MEET_M * 2.0))
            })
        });
        if near_done {
            continue;
        }
        done.entry(key).or_default().push(at);
        let arms = arms_at(at, &on_ground, &index, road);
        // How far a point lies from an arm's centre line.
        let off = |arm: &Arm, p: (f64, f64)| match arm.street {
            Some(s) => on_ground[s]
                .points()
                .windows(2)
                .map(|w| distance(project(p, w[0], w[1]).0, p))
                .fold(f64::INFINITY, f64::min),
            None => road
                .nearest(p.0, p.1, RADIUS_M.1 * 4.0)
                .map_or(f64::INFINITY, |(d, _, _)| d),
        };
        corners.extend(fillets(&arms, &off));
    }
    corners
}

/// Streets' ends and the points two streets share: where junctions may be.
fn meeting_points(streets: &[&Street]) -> Vec<(f64, f64)> {
    let mut points = Vec::new();
    let mut shared: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (s, street) in streets.iter().enumerate() {
        let line = street.points();
        points.extend(line.first());
        points.extend(line.last());
        for &p in line {
            let owners = shared.entry(cell(p, 0.05)).or_default();
            if owners.last() != Some(&s) {
                owners.push(s);
            }
        }
    }
    let mut crossings: Vec<(f64, f64)> = Vec::new();
    for (s, street) in streets.iter().enumerate() {
        for &p in street.points() {
            if shared[&cell(p, 0.05)].iter().any(|&o| o != s) {
                crossings.push(p);
            }
        }
    }
    points.extend(crossings);
    points
}

/// The arms leaving the junction at `at`.
fn arms_at(
    at: (f64, f64),
    streets: &[&Street],
    index: &SegmentIndex,
    road: &RoadIndex,
) -> Vec<Arm> {
    let mut arms: Vec<Arm> = Vec::new();
    let mut road_arm = false;
    for (s, (segment, share)) in index.closest(at, MEET_M, streets) {
        let street = streets[s];
        let line = street.points();
        let along = along_line(line, segment, share);
        let total = along_line(line, line.len() - 2, 1.0);
        for (sign, reach) in [(1.0, total - along), (-1.0, along)] {
            if reach < ARM_MIN_M {
                continue;
            }
            let from = point_at(line, along);
            let ahead = point_at(line, along + sign * ARM_M.min(reach));
            let Some(direction) = unit(from, ahead) else {
                continue;
            };
            let half = street.half_width();
            // Drawn as the road ridden here: the road is the arm (once, each way).
            // A branch starts on the road too. Check its line farther out, so a
            // shallow fork is not mistaken for a duplicate of the ridden road.
            if road.runs_along(
                from.0,
                from.1,
                road.half_width_at(from.0, from.1) + half + 0.5,
                direction,
            ) && road.runs_along(ahead.0, ahead.1, 1.0, direction)
            {
                road_arm = true;
                continue;
            }
            arms.push(Arm {
                from,
                direction,
                half,
                reach,
                lift: street.lift(),
                paved: street.paved(),
                street: Some(s),
            });
        }
    }
    if road_arm
        && let Some((from, ahead)) =
            road.heading(at.0, at.1, road.half_width_at(at.0, at.1) + MEET_M)
    {
        for direction in [ahead, (-ahead.0, -ahead.1)] {
            arms.push(Arm {
                from,
                direction,
                half: road.half_width_at(from.0, from.1),
                reach: CORNER_REACH_M,
                lift: ROAD_SINK,
                paved: road.appearance_at(from.0, from.1).paved(),
                street: None,
            });
        }
    }
    // Pieces of one street meeting at a tile's edge, or ways drawn twice: one arm each way.
    arms.sort_by(|a, b| b.half.total_cmp(&a.half));
    let mut kept: Vec<Arm> = Vec::new();
    for arm in arms {
        let same = kept.iter().any(|k| {
            k.direction.0 * arm.direction.0 + k.direction.1 * arm.direction.1 > CORNER.0.cos()
        });
        if !same {
            kept.push(arm);
        }
    }
    kept
}

/// How far a point lies from an arm's centre line.
type Off<'a> = dyn Fn(&Arm, (f64, f64)) -> f64 + 'a;

/// The kerbs between neighbouring arms (counter-clockwise).
fn fillets(arms: &[Arm], off: &Off) -> Vec<Corner> {
    if arms.len() < 2 {
        return Vec::new();
    }
    let mut sorted = arms.to_vec();
    sorted.sort_by(|a, b| heading(a.direction).total_cmp(&heading(b.direction)));
    let mut corners = Vec::new();
    for (k, a) in sorted.iter().enumerate() {
        let b = sorted[(k + 1) % sorted.len()];
        // A continuous mapped street already draws its bends. Treating its two arms
        // as a new junction cuts a second corner through the island inside a loop.
        if a.street.is_some() && a.street == b.street {
            continue;
        }
        let angle = (heading(b.direction) - heading(a.direction)).rem_euclid(std::f64::consts::TAU);
        if !(CORNER.0..=CORNER.1).contains(&angle) {
            continue;
        }
        corners.extend(fillet(a, &b, angle).filter(|c| fits(c, (a, &b), off)));
    }
    corners
}

/// The kerb in the corner from arm `a` counter-clockwise to arm `b`, `angle` apart.
fn fillet(a: &Arm, b: &Arm, angle: f64) -> Option<Corner> {
    // The edges facing the corner: `a`'s left, `b`'s right.
    let left = (-a.direction.1, a.direction.0);
    let right = (b.direction.1, -b.direction.0);
    let edge_a = (a.from.0 + left.0 * a.half, a.from.1 + left.1 * a.half);
    let edge_b = (b.from.0 + right.0 * b.half, b.from.1 + right.1 * b.half);
    // Where they meet: edge_a + t·a = edge_b + s·b.
    let det = a.direction.0 * -b.direction.1 + b.direction.0 * a.direction.1;
    if det.abs() < 1e-6 {
        return None;
    }
    let (de, dn) = (edge_b.0 - edge_a.0, edge_b.1 - edge_a.1);
    let t = (de * -b.direction.1 + b.direction.0 * dn) / det;
    let s = (a.direction.0 * dn - a.direction.1 * de) / det;
    // The edges meet far out or behind the junction: no corner of these two.
    let far = CORNER_REACH_M;
    if !(-1.0..=far).contains(&t) || !(-1.0..=far).contains(&s) {
        return None;
    }
    let apex = (edge_a.0 + a.direction.0 * t, edge_a.1 + a.direction.1 * t);
    let half_angle = angle / 2.0;
    // As round as the arms allow: the kerb ends where they run on.
    let room = (a.reach - t.max(0.0)).min(b.reach - s.max(0.0));
    let radius = ((a.half + b.half) * RADIUS_SHARE)
        .clamp(RADIUS_M.0, RADIUS_M.1)
        .min(room * half_angle.tan());
    if radius < RADIUS_M.0 / 2.0 {
        return None;
    }
    let tangent = radius / half_angle.tan();
    let start = (
        apex.0 + a.direction.0 * tangent,
        apex.1 + a.direction.1 * tangent,
    );
    let end = (
        apex.0 + b.direction.0 * tangent,
        apex.1 + b.direction.1 * tangent,
    );
    let bisector = unit(
        (0.0, 0.0),
        (a.direction.0 + b.direction.0, a.direction.1 + b.direction.1),
    )?;
    let centre_off = radius / half_angle.sin();
    let centre = (
        apex.0 + bisector.0 * centre_off,
        apex.1 + bisector.1 * centre_off,
    );
    let from = (start.1 - centre.1).atan2(start.0 - centre.0);
    let to = (end.1 - centre.1).atan2(end.0 - centre.0);
    // The short way round, the side facing the apex.
    let sweep =
        (to - from + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU) - std::f64::consts::PI;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a few pieces
    let pieces = (sweep.abs() / ARC_STEP).ceil().max(1.0) as usize;
    let arc: Vec<(f64, f64)> = (0..=pieces)
        .map(|k| {
            #[allow(clippy::cast_precision_loss)] // a few pieces
            let angle = from + sweep * k as f64 / pieces as f64;
            (
                centre.0 + radius * angle.cos(),
                centre.1 + radius * angle.sin(),
            )
        })
        .collect();
    let (mut min, mut max) = (apex, apex);
    for &(e, n) in &arc {
        min = (min.0.min(e), min.1.min(n));
        max = (max.0.max(e), max.1.max(n));
    }
    Some(Corner {
        apex,
        arc,
        lift: a.lift.min(b.lift) - BELOW_M,
        paved: a.paved && b.paved,
        min,
        max,
        centre,
        radius,
        end_lifts: [a.lift, b.lift],
        footway_bases: [
            (
                (start.0 - left.0 * a.half, start.1 - left.1 * a.half),
                a.street.is_none(),
            ),
            (
                (end.0 - right.0 * b.half, end.1 - right.1 * b.half),
                b.street.is_none(),
            ),
        ],
    })
}

/// Whether `corner`, between arms `a` and `b`, lies against their real edges: its ends on
/// them, its apex on both, and its kerb outside both streets.
fn fits(corner: &Corner, (a, b): (&Arm, &Arm), off: &Off) -> bool {
    let on_edge = |arm: &Arm, p: (f64, f64)| (off(arm, p) - arm.half).abs() <= EDGE_FIT_M;
    let (Some(&start), Some(&end)) = (corner.arc.first(), corner.arc.last()) else {
        return false;
    };
    on_edge(a, start)
        && on_edge(b, end)
        && on_edge(a, corner.apex)
        && on_edge(b, corner.apex)
        && corner
            .arc
            .iter()
            .all(|&p| off(a, p) >= a.half - EDGE_FIT_M && off(b, p) >= b.half - EDGE_FIT_M)
}

/// The corners within the chunk square `[origin, origin + size]`, relative to `chunk_origin`,
/// draped on the ground: paved and unpaved, as the streets.
pub(crate) fn meshes(
    corners: &[Corner],
    origin: (f64, f64),
    size: f64,
    heights: &HeightGrid,
    chunk_origin: [f64; 3],
) -> (MeshData, MeshData) {
    let (mut paved, mut unpaved) = (MeshData::default(), MeshData::default());
    for corner in corners {
        if corner.max.0 < origin.0
            || corner.min.0 > origin.0 + size
            || corner.max.1 < origin.1
            || corner.min.1 > origin.1 + size
        {
            continue;
        }
        let mesh = if corner.paved {
            &mut paved
        } else {
            &mut unpaved
        };
        // Its edge colour, as a street's (`u` 0), so tracks show no grass strip in it.
        let uv = |_: (f64, f64)| [0.0, 0.0];
        for pair in corner.arc.windows(2) {
            let mut triangle = [corner.apex, pair[0], pair[1]];
            if clockwise_area(&triangle) > 0.0 {
                triangle.swap(1, 2);
            }
            drape::drape_polygon(
                mesh,
                &triangle,
                (On::Ground, corner.lift),
                heights,
                chunk_origin,
                &uv,
            );
            let material = if corner.paved { 0.0 } else { 1.0 / 8.0 };
            mesh.colors
                .resize(mesh.vertices.len(), [material, 6.0 / 32.0, 0.0, 1.0 / 8.0]);
        }
    }
    (paved, unpaved)
}

/// Twice the signed area of a triangle, positive counter-clockwise seen from above.
fn clockwise_area(t: &[(f64, f64); 3]) -> f64 {
    (t[1].0 - t[0].0) * (t[2].1 - t[0].1) - (t[1].1 - t[0].1) * (t[2].0 - t[0].0)
}

/// The streets' segments by cell, for finding the streets at a point.
struct SegmentIndex {
    cells: HashMap<(i64, i64), Vec<(usize, usize)>>,
}

const INDEX_CELL_M: f64 = 10.0;

impl SegmentIndex {
    fn new(streets: &[&Street]) -> Self {
        let mut cells: HashMap<(i64, i64), Vec<(usize, usize)>> = HashMap::new();
        for (s, street) in streets.iter().enumerate() {
            for (k, pair) in street.points().windows(2).enumerate() {
                // Points lie a few metres apart, well within a cell: both ends' cells cover it.
                let (first, second) = (cell(pair[0], INDEX_CELL_M), cell(pair[1], INDEX_CELL_M));
                cells.entry(first).or_default().push((s, k));
                if second != first {
                    cells.entry(second).or_default().push((s, k));
                }
            }
        }
        Self { cells }
    }

    /// Every street within `reach` of `at`: its closest segment and the share along it.
    fn closest(
        &self,
        at: (f64, f64),
        reach: f64,
        streets: &[&Street],
    ) -> Vec<(usize, (usize, f64))> {
        let mut best: HashMap<usize, (f64, usize, f64)> = HashMap::new();
        let (ce, cn) = cell(at, INDEX_CELL_M);
        for x in ce - 1..=ce + 1 {
            for y in cn - 1..=cn + 1 {
                for &(s, k) in self.cells.get(&(x, y)).into_iter().flatten() {
                    let line = streets[s].points();
                    let (on, share) = project(at, line[k], line[k + 1]);
                    let d = distance(on, at);
                    if d <= reach && best.get(&s).is_none_or(|b| d < b.0) {
                        best.insert(s, (d, k, share));
                    }
                }
            }
        }
        let mut found: Vec<(usize, (usize, f64))> = best
            .into_iter()
            .map(|(s, (_, k, share))| (s, (k, share)))
            .collect();
        // A deterministic order, whatever the hash map did.
        found.sort_by_key(|&(s, (k, _))| (s, k));
        found
    }
}

/// Distance along `line` to the point `share` of the way along its segment `segment`.
fn along_line(line: &[(f64, f64)], segment: usize, share: f64) -> f64 {
    let before: f64 = line[..=segment]
        .windows(2)
        .map(|w| distance(w[0], w[1]))
        .sum();
    before + distance(line[segment], line[segment + 1]) * share
}

/// The point `along` metres along `line` (clamped to it).
fn point_at(line: &[(f64, f64)], along: f64) -> (f64, f64) {
    let mut left = along.max(0.0);
    for pair in line.windows(2) {
        let length = distance(pair[0], pair[1]);
        if left <= length {
            let t = if length > 0.0 { left / length } else { 0.0 };
            return (
                pair[0].0 + (pair[1].0 - pair[0].0) * t,
                pair[0].1 + (pair[1].1 - pair[0].1) * t,
            );
        }
        left -= length;
    }
    line[line.len() - 1]
}

/// The point on segment `a`–`b` closest to `p`, and how far along the segment it is (0–1).
fn project(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> ((f64, f64), f64) {
    let (de, dn) = (b.0 - a.0, b.1 - a.1);
    let length_squared = de * de + dn * dn;
    if length_squared < 1e-12 {
        return (a, 0.0);
    }
    let t = (((p.0 - a.0) * de + (p.1 - a.1) * dn) / length_squared).clamp(0.0, 1.0);
    ((a.0 + de * t, a.1 + dn * t), t)
}

fn unit(from: (f64, f64), to: (f64, f64)) -> Option<(f64, f64)> {
    let (de, dn) = (to.0 - from.0, to.1 - from.1);
    let length = de.hypot(dn);
    (length > 1e-6).then(|| (de / length, dn / length))
}

/// Direction as an angle counter-clockwise from east.
fn heading(direction: (f64, f64)) -> f64 {
    direction.1.atan2(direction.0)
}

fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    (b.0 - a.0).hypot(b.1 - a.1)
}

#[allow(clippy::cast_possible_truncation)] // local metres stay far below 2^63 cells
fn cell(p: (f64, f64), size: f64) -> (i64, i64) {
    ((p.0 / size).floor() as i64, (p.1 / size).floor() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuous_street_arms_do_not_add_a_second_corner_through_their_bend() {
        let a = Arm {
            from: (0.0, 0.0),
            direction: (1.0, 0.0),
            half: 3.0,
            reach: 100.0,
            lift: 0.07,
            paved: true,
            street: Some(0),
        };
        let mut b = Arm {
            direction: (0.0, 1.0),
            ..a
        };
        let off = |arm: &Arm, p: (f64, f64)| {
            ((p.0 - arm.from.0) * arm.direction.1 - (p.1 - arm.from.1) * arm.direction.0).abs()
        };
        assert!(
            fillets(&[a, b], &off).is_empty(),
            "a continuous street already has its bend geometry"
        );
        b.street = Some(1);
        assert_eq!(
            fillets(&[a, b], &off).len(),
            1,
            "independent joining streets retain their rounded corner"
        );
    }
}
