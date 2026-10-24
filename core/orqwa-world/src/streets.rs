//! The streets, tracks and paths of the map around the route, other than the road ridden,
//! draped on the terrain of each chunk: cut along the ground's own triangles, so they lie
//! exactly on the ground the rider sees and it never shows through them. Streets joining or
//! crossing the road run up to it and under it, so junctions look joined; bridges are straight
//! decks between their ends; tunnels stay under the ground.

use std::collections::HashMap;

use orqwa_osm::{MapData, RoadClass, StructureKind};
use orqwa_routes::{ElevationModel, LocalProjection, Surface};

use crate::appearance::Appearance;
use crate::junctions::{self, Corner};
use crate::railways::{self, Railway};
use crate::road::{Mouth, RoadIndex};
use crate::structures::Below;
use crate::water::{self, Pool};
use crate::{HeightGrid, MeshData, On, chains, drape};

/// Distance between the points of a street: short enough to tell where it runs along the road
/// ridden and to keep plants off it.
const STEP_M: f64 = 3.0;
/// Streets end round, and bend round where they turn more than this (radians) at a point: a
/// strip alone ends square and leaves a notch outside a sharp bend (#74).
const JOIN_TURN: f64 = 0.3;
/// Corners of those round ends and joins.
const CAP_CORNERS: u32 = 12;
/// They lie this far below their street, so where they overlap it the street shows.
const CAP_BELOW_M: f64 = 0.003;

/// How far a street lies above the ground: bigger roads above smaller ones, so where they
/// overlap at junctions the bigger one shows, and a little more for every street (up to 1 cm)
/// so overlapping ones of a kind never flicker. All stay well below the road ridden, which
/// stands `ROAD_SINK` above the ground beside it, so streets joining it run on under it.
fn lift(class: RoadClass, index: usize) -> f64 {
    let base = match class {
        RoadClass::Major => 0.08,
        RoadClass::Street => 0.07,
        RoadClass::Service => 0.06,
        RoadClass::Track => 0.05,
        RoadClass::Path => 0.04,
    };
    #[allow(clippy::cast_precision_loss)] // a small remainder
    let jitter = (index % 10) as f64 * 0.001;
    base + jitter
}

/// A street of the map in metres east/north, with its bounds for quick chunk tests.
pub(crate) struct Street {
    /// Original map features making up this chain, for pavement ownership.
    sources: Vec<usize>,
    pub(crate) appearance: Appearance,
    class: RoadClass,
    points: Vec<(f64, f64)>,
    min: (f64, f64),
    max: (f64, f64),
    /// For bridges: the deck's height at both ends (the ground's there).
    deck: Option<(f64, f64)>,
    /// Its deck was lowered to meet a ridden ground road; the bank must follow it too.
    joined: bool,
    index: usize,
}

impl Street {
    pub(crate) fn sources(&self) -> &[usize] {
        &self.sources
    }

    /// Its points, a few metres apart.
    pub(crate) fn points(&self) -> &[(f64, f64)] {
        &self.points
    }

    /// Half its width in metres.
    pub(crate) fn half_width(&self) -> f64 {
        self.appearance.width / 2.0
    }

    /// Whether it is a bridge.
    pub(crate) fn on_bridge(&self) -> bool {
        self.deck.is_some()
    }

    /// The ridden pavement owns a complete aligned bridge, even with a ground route tag.
    fn owned_bridge(&self, road: &RoadIndex) -> bool {
        self.on_bridge()
            && road.owns_bridge(
                self.half_width(),
                self.points
                    .iter()
                    .enumerate()
                    .map(|(i, &point)| (point, local_direction(&self.points, i))),
            )
    }

    fn owned_approach(&self, road: &RoadIndex) -> bool {
        !self.on_bridge()
            && road.owns_tunnel_approach(
                self.half_width(),
                self.points
                    .iter()
                    .enumerate()
                    .map(|(i, &point)| (point, local_direction(&self.points, i))),
            )
    }

    /// How far it lies above the ground.
    pub(crate) fn lift(&self) -> f64 {
        lift(self.class, self.index)
    }

    /// Whether it is paved (asphalt) rather than gravel or dirt.
    pub(crate) fn paved(&self) -> bool {
        self.appearance.paved()
    }
}

/// The map's streets around the route, their pieces joined end to end where they go on as one
/// way of a kind (`chains`), densified, with bridge decks' end heights from `model`: a bridge
/// cut by a tile border is one deck between its real ends, not two dipping to the ground where
/// the tiles cut them, crossing each other (#116, #117). Tunnels are left out: draped on the
/// ground, they would run over the mountain.
pub(crate) async fn lines<M: ElevationModel>(
    map: &MapData,
    projection: &LocalProjection,
    model: &mut M,
) -> Vec<Street> {
    let projected: Vec<Vec<(f64, f64)>> = map
        .roads
        .iter()
        .map(|road| {
            road.line
                .iter()
                .map(|&(lat, lon)| projection.project(lat, lon))
                .collect()
        })
        .collect();
    let pieces: Vec<&[(f64, f64)]> = projected.iter().map(Vec::as_slice).collect();
    let alike = |first: usize, second: usize| {
        let (first, second) = (&map.roads[first], &map.roads[second]);
        first.class == second.class
            && first.structure == second.structure
            && first.details == second.details
    };
    let joined = chains::chains(&pieces, &alike);
    let mut streets = Vec::new();
    for (index, chain) in joined.into_iter().enumerate() {
        let road = &map.roads[chain[0].0];
        if road.structure == Some(StructureKind::Tunnel) {
            continue;
        }
        let mut line: Vec<(f64, f64)> = Vec::new();
        for &(piece, reversed) in &chain {
            let mut points = projected[piece].clone();
            if reversed {
                points.reverse();
            }
            // Joined pieces share their end points.
            let skip = usize::from(!line.is_empty());
            line.extend(points.into_iter().skip(skip));
        }
        let points = drape::densify(&line, STEP_M);
        let (Some(&first), Some(&last)) = (line.first(), line.last()) else {
            continue;
        };
        let deck = if road.structure == Some(StructureKind::Bridge) {
            let mut end = async |(east, north): (f64, f64)| {
                let (latitude, longitude) = projection.unproject(east, north);
                model.elevation(latitude, longitude).await.ok()
            };
            match (end(first).await, end(last).await) {
                (Some(a), Some(b)) => Some((a, b)),
                // Without the ground's heights a deck cannot be placed.
                _ => continue,
            }
        } else {
            None
        };
        let (mut min, mut max) = (first, first);
        for &(e, n) in &points {
            min = (min.0.min(e), min.1.min(n));
            max = (max.0.max(e), max.1.max(n));
        }
        streets.push(Street {
            sources: chain.iter().map(|&(source, _)| source).collect(),
            appearance: Appearance::of(road),
            class: road.class,
            points,
            min,
            max,
            deck,
            joined: false,
            index,
        });
    }
    streets
}

/// Resolve complete bridge profiles before either terrain or wearing surfaces use them.
pub(crate) fn connect(streets: &mut [Street], road: &RoadIndex) {
    for street in streets {
        let Some(ends) = street.deck else {
            continue;
        };
        if street.owned_bridge(road) {
            continue;
        }
        let lift = lift(street.class, street.index);
        let meet = |point, height| {
            road.ground_join(point, height + lift, 3.0)
                .map_or(height, |road| road - lift - 0.02)
        };
        let joined = (
            meet(street.points[0], ends.0),
            meet(*street.points.last().expect("street has points"), ends.1),
        );
        street.joined = (joined.0 - ends.0).abs() > 1e-6 || (joined.1 - ends.1).abs() > 1e-6;
        street.deck = Some(joined);
    }
}

/// The paved streets and the unpaved tracks and paths within the chunk square `[origin,
/// origin + size]`, relative to `chunk_origin`, with the `corners` of their junctions.
/// Fully covered stretches running along the road ridden are left out; streets joining or
/// crossing it retain their joining segments and run on under it.
#[allow(clippy::too_many_lines)] // keep pavement and its temporary barrier ranges aligned
pub(crate) fn meshes(
    (streets, corners): (&[Street], &[Corner]),
    origin: (f64, f64),
    size: f64,
    heights: &HeightGrid,
    below: &Below,
    chunk_origin: [f64; 3],
) -> (MeshData, MeshData, [Vec<crate::pavement::Barrier>; 2]) {
    let road = below.road();
    let (mut paved_mesh, mut unpaved_mesh) = (MeshData::default(), MeshData::default());
    let (mut paved_barriers, mut unpaved_barriers) = (Vec::new(), Vec::new());
    let (low, high) = (origin, (origin.0 + size, origin.1 + size));
    for street in streets {
        if street.max.0 < low.0
            || street.min.0 > high.0
            || street.max.1 < low.1
            || street.min.1 > high.1
            || street.owned_bridge(road)
            || street.owned_approach(road)
        {
            continue;
        }
        let half = street.half_width();
        let target = if street.paved() {
            &mut paved_mesh
        } else {
            &mut unpaved_mesh
        };
        let barriers = if street.paved() {
            &mut paved_barriers
        } else {
            &mut unpaved_barriers
        };
        let total = along(&street.points);
        let surface = if street.on_bridge() {
            Surface::Bridge
        } else {
            Surface::Ground
        };
        for piece in drape::pieces(&street.points, low, high) {
            let points: Vec<(f64, f64)> = piece.iter().map(|&(p, _)| p).collect();
            let mut run: Vec<((f64, f64), f64)> = Vec::new();
            let mut previous = None;
            for (i, &(point, distance)) in piece.iter().enumerate() {
                let direction = local_direction(&points, i);
                if road.covers_street(point, half, surface, direction, &street.sources) {
                    if !run.is_empty() {
                        // Keep the transition segment: its visible end must reach a
                        // cross-section inside the ridden road, not stop a sample short.
                        run.push((point, distance));
                        ribbon(
                            target,
                            &run,
                            half,
                            street,
                            total,
                            (heights, below),
                            chunk_origin,
                            barriers,
                        );
                        run.clear();
                    }
                } else {
                    if run.is_empty()
                        && let Some(point) = previous
                    {
                        run.push(point);
                    }
                    run.push((point, distance));
                }
                previous = Some((point, distance));
            }
            ribbon(
                target,
                &run,
                half,
                street,
                total,
                (heights, below),
                chunk_origin,
                barriers,
            );
        }
        if street.deck.is_none() {
            for (point, direction) in rounds(&street.points) {
                let outside = point.0 + half < low.0
                    || point.0 - half > high.0
                    || point.1 + half < low.1
                    || point.1 - half > high.1;
                if outside || road.covers_street(point, half, surface, direction, &street.sources) {
                    continue;
                }
                let disc: Vec<(f64, f64)> = (0..CAP_CORNERS)
                    .map(|k| {
                        // Clockwise seen from above, as the ground's triangles.
                        let angle = -std::f64::consts::TAU * f64::from(k) / f64::from(CAP_CORNERS);
                        (point.0 + half * angle.cos(), point.1 + half * angle.sin())
                    })
                    .collect();
                // The street's edge colour (`u` 0): tracks show no grass strip in it.
                drape::drape_polygon(
                    target,
                    &disc,
                    (On::Ground, street.lift() - CAP_BELOW_M),
                    heights,
                    chunk_origin,
                    &|_| [0.0, 0.0],
                );
                target
                    .colors
                    .resize(target.vertices.len(), street.appearance.plain_color());
            }
        }
    }
    let (paved_corners, unpaved_corners) =
        junctions::meshes(corners, origin, size, heights, chunk_origin);
    paved_mesh.append(paved_corners);
    unpaved_mesh.append(unpaved_corners);
    (paved_mesh, unpaved_mesh, [paved_barriers, unpaved_barriers])
}

/// Where a street is round: its ends and its sharp bends, with its direction there.
fn rounds(points: &[(f64, f64)]) -> Vec<((f64, f64), (f64, f64))> {
    let Some(last) = points.len().checked_sub(1) else {
        return Vec::new();
    };
    let mut found = vec![(points[0], local_direction(points, 0))];
    for i in 1..last {
        let (a, b, c) = (points[i - 1], points[i], points[i + 1]);
        let turn = ((b.0 - a.0).atan2(b.1 - a.1) - (c.0 - b.0).atan2(c.1 - b.1)
            + std::f64::consts::PI)
            .rem_euclid(std::f64::consts::TAU)
            - std::f64::consts::PI;
        if turn.abs() > JOIN_TURN {
            found.push((b, local_direction(points, i)));
        }
    }
    if last > 0 {
        found.push((points[last], local_direction(points, last)));
    }
    found
}

/// Where the streets meet the road ridden (see [`Mouth`]): the road's edge is road there, not
/// shoulder. Fully covered stretches running along the road are no mouths.
pub(crate) fn mouths(streets: &[Street], road: &RoadIndex) -> Vec<Mouth> {
    let mut mouths = Vec::new();
    for street in streets.iter().filter(|s| s.deck.is_none()) {
        let half = street.half_width();
        for (i, &(east, north)) in street.points.iter().enumerate() {
            // Points on the road's surface belong to no side; beside it, a band wider than the
            // points are apart catches every street that reaches the edge.
            let reach = road.half_width_at(east, north) + 3.0;
            let Some((_, along, side)) = road.locate(east, north, reach) else {
                continue;
            };
            let Some((from, forward)) = road.heading(east, north, reach) else {
                continue;
            };
            // Beyond a route endpoint the nearest-point distance includes the distance
            // along its continuation. Only the lateral distance identifies an edge mouth.
            let across = (forward.0 * (north - from.1) - forward.1 * (east - from.0)).abs();
            if across < road.half_width_at(east, north) - 0.5 {
                continue;
            }
            let direction = local_direction(&street.points, i);
            if !road.covers_street(
                (east, north),
                half,
                Surface::Ground,
                direction,
                &street.sources,
            ) {
                mouths.push((along, side, half));
            }
        }
    }
    mouths
}

/// The length of a line.
fn along(points: &[(f64, f64)]) -> f64 {
    points
        .windows(2)
        .map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1))
        .sum()
}

/// The unit direction of a line at its point `i`.
fn local_direction(points: &[(f64, f64)], i: usize) -> (f64, f64) {
    let (a, b) = (
        points[i.saturating_sub(1)],
        points[(i + 1).min(points.len() - 1)],
    );
    let (de, dn) = (b.0 - a.0, b.1 - a.1);
    let length = de.hypot(dn).max(1e-9);
    (de / length, dn / length)
}

/// A strip of `half` width along `run` (points with their distance along the street): draped
/// on the ground, or for a bridge a solid deck straight between its ends' heights.
#[allow(clippy::too_many_arguments)] // terrain context plus temporary barrier ranges
fn ribbon(
    mesh: &mut MeshData,
    run: &[((f64, f64), f64)],
    half: f64,
    street: &Street,
    total: f64,
    (heights, below): (&HeightGrid, &Below),
    origin: [f64; 3],
    barriers: &mut Vec<crate::pavement::Barrier>,
) {
    if let Some(ends) = street.deck {
        let lift = lift(street.class, street.index);
        drape::deck(
            mesh,
            run,
            half,
            street.appearance.color(),
            ends,
            lift,
            total,
            (heights, below),
            origin,
            barriers,
        );
    } else {
        drape::drape(
            mesh,
            run,
            half,
            (On::Ground, lift(street.class, street.index)),
            heights,
            origin,
        );
    }
    mesh.colors
        .resize(mesh.vertices.len(), street.appearance.color());
}

/// Street points with their half widths, by index cell.
type StreetCells = std::collections::HashMap<(i64, i64), Vec<(f64, f64, f64)>>;

/// Where the map's streets, railways and shores are, to keep trees and grass off them.
pub(crate) struct Clearance {
    cells: StreetCells,
    corners: StreetCells,
}

/// Index cell size; larger than half the widest street plus the margins used.
const CLEARANCE_CELL_M: f64 = 10.0;

impl Clearance {
    pub(crate) fn new(
        streets: &[Street],
        corners: &[Corner],
        railways: &[Railway],
        pools: &[Pool],
    ) -> Self {
        let mut cells = StreetCells::new();
        let mut corner_cells = StreetCells::new();
        // Kerbs' outlines and apexes: the corners are filled between them and the streets.
        for (e, n) in corners.iter().flat_map(Corner::outline) {
            corner_cells
                .entry(clearance_cell(e, n))
                .or_default()
                .push((e, n, 0.5));
        }
        let lines = streets
            .iter()
            .map(|s| (&s.points, s.half_width()))
            .chain(railways.iter().map(|r| (&r.points, railways::BED_M / 2.0)))
            .chain(pools.iter().map(|p| (&p.shore, water::SHORE_M / 2.0)));
        for (points, half) in lines {
            for &(e, n) in points {
                cells
                    .entry(clearance_cell(e, n))
                    .or_default()
                    .push((e, n, half));
            }
        }
        Self {
            cells,
            corners: corner_cells,
        }
    }

    /// Like [`Clearance::blocked`], counting only the points `counts` accepts (e.g. not those
    /// of the road ridden itself, beside which things are meant to stand).
    pub(crate) fn blocked_by(
        &self,
        east: f64,
        north: f64,
        margin: f64,
        counts: impl Fn(f64, f64) -> bool,
    ) -> bool {
        Self::blocked_in(&self.cells, east, north, margin, &counts)
            || Self::blocked_in(&self.corners, east, north, margin, &counts)
    }

    /// Street/rail/shore clearance without padded junction corners: footways use
    /// the junction's exact curved outline instead of its vegetation exclusion.
    pub(crate) fn blocked_urban_by(
        &self,
        east: f64,
        north: f64,
        margin: f64,
        counts: impl Fn(f64, f64) -> bool,
    ) -> bool {
        Self::blocked_in(&self.cells, east, north, margin, &counts)
    }

    fn blocked_in(
        cells: &StreetCells,
        east: f64,
        north: f64,
        margin: f64,
        counts: &impl Fn(f64, f64) -> bool,
    ) -> bool {
        let (ce, cn) = clearance_cell(east, north);
        (ce - 1..=ce + 1).any(|x| {
            (cn - 1..=cn + 1).any(|y| {
                cells.get(&(x, y)).is_some_and(|points| {
                    points.iter().any(|&(e, n, half)| {
                        (e - east).hypot(n - north) < half + margin + STEP_M / 2.0 && counts(e, n)
                    })
                })
            })
        })
    }

    /// Whether `(east, north)` lies on a street or within `margin` of its edge.
    pub(crate) fn blocked(&self, east: f64, north: f64, margin: f64) -> bool {
        self.blocked_by(east, north, margin, |_, _| true)
    }
}

#[allow(clippy::cast_possible_truncation)] // local metres stay far below 2^63 cells
fn clearance_cell(east: f64, north: f64) -> (i64, i64) {
    (
        (east / CLEARANCE_CELL_M).floor() as i64,
        (north / CLEARANCE_CELL_M).floor() as i64,
    )
}

/// Paved streets are level across, as built (#116): the ground under them and this far beyond
/// their edges lies at the natural ground's height at their centre line, so every triangle of
/// the fine ground they lie on is level across (the diagonal of a fine piece, and a little)...
pub(crate) const LEVEL_VERGE_M: f64 = crate::FINE * std::f64::consts::SQRT_2 + 0.2;
/// ...beyond, it is cut into the hillside or banked down to the natural ground, natural again
/// this far out, blending into it over the last `LEVEL_FADE_M`.
pub(crate) const LEVEL_REACH_M: f64 = 8.0;
pub(crate) const LEVEL_FADE_M: f64 = 3.0;
/// Index cell size of [`Levels`].
const LEVEL_CELL_M: f64 = 20.0;

/// A piece of a street's centre line, metres east/north: its two ends and the street's half
/// width.
type Piece = ((f64, f64), (f64, f64), f64);

/// The paved streets on the ground, which level the ground across them (#116). Tracks and
/// paths follow the land as it is; bridges stand above it.
pub(crate) struct Levels {
    /// Connected bridge profiles lower their approach banks without filling their valleys.
    pub(crate) bridges: RoadIndex,
    /// Pieces of their centre lines, with their half widths.
    segments: Vec<Piece>,
    cells: HashMap<(i64, i64), Vec<usize>>,
}

impl Levels {
    pub(crate) fn new(streets: &[Street], appearance: &crate::appearance::Index) -> Self {
        let lines: Vec<_> = streets
            .iter()
            .filter(|s| s.joined)
            .map(|street| {
                let (start, end) = street.deck.expect("joined bridge");
                let total = along(&street.points).max(1e-6);
                let mut distance = 0.0;
                street
                    .points
                    .iter()
                    .enumerate()
                    .map(|(i, &position)| {
                        if i > 0 {
                            let previous = street.points[i - 1];
                            distance += (position.0 - previous.0).hypot(position.1 - previous.1);
                        }
                        crate::road::Centre {
                            position,
                            distance,
                            elevation: start
                                + (end - start) * distance / total
                                + lift(street.class, street.index),
                            surface: Surface::Bridge,
                            mapped: true,
                        }
                    })
                    .collect()
            })
            .collect();
        let mut bridges = RoadIndex::from_lines(&lines);
        bridges.set_appearances(appearance);
        let mut segments = Vec::new();
        let mut cells: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        for street in streets.iter().filter(|s| s.paved() && !s.on_bridge()) {
            let half = street.half_width();
            for pair in street.points.windows(2) {
                let index = segments.len();
                segments.push((pair[0], pair[1], half));
                // Pieces are at most a step long: both ends' cells cover them.
                let (first, second) = (level_cell(pair[0]), level_cell(pair[1]));
                cells.entry(first).or_default().push(index);
                if second != first {
                    cells.entry(second).or_default().push(index);
                }
            }
        }
        Self {
            bridges,
            segments,
            cells,
        }
    }

    /// Whether there are no paved streets at all, so the ground is nowhere levelled for one.
    pub(crate) fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// The paved street whose edge lies nearest to (`east`, `north`), if that is within its
    /// levelled ground (`LEVEL_VERGE_M` and `LEVEL_REACH_M` beyond its edge) and `extra`
    /// metres more: the distance from its centre line, its half width and the nearest point of
    /// its centre line.
    pub(crate) fn nearest(
        &self,
        east: f64,
        north: f64,
        extra: f64,
    ) -> Option<(f64, f64, (f64, f64))> {
        let reach = LEVEL_VERGE_M + LEVEL_REACH_M + extra;
        // The widest street's centre, and a piece's far end a step beyond its nearest point.
        let scan = reach + 5.25 + STEP_M;
        let (low, high) = (
            level_cell((east - scan, north - scan)),
            level_cell((east + scan, north + scan)),
        );
        // How far beyond its edge, how far from its centre line, its half width, the nearest
        // point of its centre line.
        let mut best: Option<(f64, f64, f64, (f64, f64))> = None;
        for x in low.0..=high.0 {
            for y in low.1..=high.1 {
                for &index in self.cells.get(&(x, y)).into_iter().flatten() {
                    let (from, to, half) = self.segments[index];
                    let foot = nearest_on(from, to, (east, north));
                    let distance = (east - foot.0).hypot(north - foot.1);
                    let beyond = distance - half;
                    if beyond <= reach && best.is_none_or(|b| beyond < b.0) {
                        best = Some((beyond, distance, half, foot));
                    }
                }
            }
        }
        best.map(|(_, distance, half, foot)| (distance, half, foot))
    }
}

/// The point of segment `from`–`to` nearest to `point`.
fn nearest_on(from: (f64, f64), to: (f64, f64), point: (f64, f64)) -> (f64, f64) {
    let (de, dn) = (to.0 - from.0, to.1 - from.1);
    let length_squared = de * de + dn * dn;
    if length_squared < 1e-12 {
        return from;
    }
    let share =
        (((point.0 - from.0) * de + (point.1 - from.1) * dn) / length_squared).clamp(0.0, 1.0);
    (from.0 + de * share, from.1 + dn * share)
}

#[allow(clippy::cast_possible_truncation)] // local metres stay far below 2^63 cells
fn level_cell((east, north): (f64, f64)) -> (i64, i64) {
    (
        (east / LEVEL_CELL_M).floor() as i64,
        (north / LEVEL_CELL_M).floor() as i64,
    )
}
