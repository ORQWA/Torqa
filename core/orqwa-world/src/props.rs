//! What stands by the road ridden (R92): guard rails where the ground falls away beside it,
//! delineator posts along country roads and snow poles on mountain roads; in towns street
//! lamps, benches with bins and bus shelters; in the countryside pasture fences, hay bales and
//! wayside crosses and power lines; road signs with places' names and warnings. Placed by
//! rules from
//! the road's shape, the ground beside it and the land around, as the Blender models of
//! `art/props` (`app/assets/models/props`).

use std::collections::BTreeMap;

use orqwa_osm::LandCover;
use orqwa_routes::Surface;

use crate::landcover::LandIndex;
use crate::road::{Mouth, RoadIndex, Station};
use crate::streets::Clearance;
use crate::vegetation::Footprint;
use crate::{HeightGrid, MeshData, ROAD_HALF_WIDTH, ROAD_SINK};

/// A guard rail's span (`guard_rail`, one post at its middle) and how far its face stands from
/// the road's centre line.
const RAIL_SPAN: f64 = 4.0;
const RAIL_OFFSET: f64 = ROAD_HALF_WIDTH + 0.6;
/// The ground this far beyond the road's edge, this much below the road, calls for a rail.
const DROP_PROBE: f64 = ROAD_HALF_WIDTH + 8.5;
const RAIL_DROP: f64 = 2.0;
/// Delineator posts stand this far apart and this far from the centre line, on both sides.
const POST_SPACING: f64 = 50.0;
const POST_OFFSET: f64 = ROAD_HALF_WIDTH + 0.8;
/// Above this height (metres) snow poles mark the road's edges instead, closer together and a
/// little further out.
const SNOW_LINE: f64 = 1300.0;
const POLE_SPACING: f64 = 25.0;
const POLE_OFFSET: f64 = ROAD_HALF_WIDTH + 1.2;
/// Nothing stands this far either side of where a street joins the road, beyond its half width.
const MOUTH_CLEAR: f64 = 6.0;
/// Street lamps in towns: this far apart on each side, staggered (one side's halfway between
/// the other's), at the road's edge.
const LAMP_SPACING: f64 = 60.0;
const LAMP_OFFSET: f64 = ROAD_HALF_WIDTH + 0.7;
/// Bench candidates every 120 m, offset from lamps and bus stops, alternate sides. Their
/// backs sit at the sidewalk's outer edge, leaving the inner part open for pedestrians.
const TOWN_STEP: f64 = 10.0;
const BENCH_SPACING: f64 = 120.0;
const BENCH_PHASE: f64 = 20.0;
const BENCH_OFFSET: f64 = ROAD_HALF_WIDTH + 1.55;
const BIN_ALONG: f64 = 1.4;
/// A bus stop's shelter every `SHELTER_SPACING` of road in town, on the right of travel, its
/// front this far out.
const SHELTER_SPACING: f64 = 400.0;
const SHELTER_OFFSET: f64 = ROAD_HALF_WIDTH + 2.4;
/// What stands in town keeps this clear of buildings and side streets.
const TOWN_CLEAR: f64 = 1.0;
/// Where the map has no built-up land around a village, its buildings make it one: this many
/// within this far of the road's point.
const VILLAGE_BUILDINGS: usize = 4;
const VILLAGE_REACH: f64 = 60.0;
/// Pasture fences: spans this long, this far out, along stretches of meadow or field; a
/// stretch is `FENCE_STRETCH` long and about one in `FENCE_SHARE` is fenced on each side.
const FENCE_SPAN: f64 = 3.0;
const FENCE_OFFSET: f64 = ROAD_HALF_WIDTH + 3.0;
const FENCE_STRETCH: f64 = 120.0;
const FENCE_SHARE: f64 = 0.3;
/// Hay bales lie in fields between these distances from the road, at about one spot in
/// `BALE_SHARE` of a `BALE_GRID` grid, one to three together.
const BALE_GRID: f64 = 18.0;
const BALE_NEAR: f64 = 12.0;
const BALE_FAR: f64 = 90.0;
const BALE_SHARE: f64 = 0.06;
/// A wayside cross stands beside about one in `CROSS_SHARE` of the country lanes joining the
/// road, just past the lane, this far out.
const CROSS_SHARE: f64 = 0.2;
const CROSS_OFFSET: f64 = ROAD_HALF_WIDTH + 2.5;
/// Road signs stand this far out on the right; the road is looked along `SIGN_STEP` at a
/// time, `SIGN_LOOK` around the chunk, and a town is entered or left where `SIGN_STEADY` steps
/// either side agree.
const SIGN_OFFSET: f64 = ROAD_HALF_WIDTH + 1.2;
const SIGN_STEP: f64 = 10.0;
const SIGN_LOOK: f64 = 400.0;
const SIGN_STEADY: i64 = 3;
/// Whether the road is in town is taken by majority over this many steps either side.
const TOWN_SMOOTH: i64 = 10;
/// A place's sign carries the name of the nearest place within this distance.
const PLACE_REACH: f64 = 2500.0;
/// A hairpin: the road turns more than this (radians, 100°) over `BEND_SPAN` steps starting
/// `BEND_AHEAD` steps on.
const HAIRPIN_TURN: f64 = 1.75;
const BEND_AHEAD: i64 = 8;
const BEND_SPAN: i64 = 6;
/// A steep stretch: `GRADE_SPAN` steps climbing or falling at `STEEP` or more on average.
const GRADE_SPAN: i64 = 30;
const STEEP: f64 = 0.09;
/// Where the labels sit on the signs' faces (metres above the ground).
const PLACE_LABEL_HEIGHT: f32 = 1.9;
const GRADE_LABEL_HEIGHT: f32 = 1.84;
/// Power lines: about one in `LINE_SHARE` of the `LINE_STRETCH` stretches of country road
/// has one, on one side, its poles `LINE_POLE_SPACING` apart and `LINE_OFFSET` from the centre
/// line.
const LINE_STRETCH: f64 = 600.0;
const LINE_SHARE: f64 = 0.4;
const LINE_POLE_SPACING: f64 = 50.0;
const LINE_OFFSET: f64 = ROAD_HALF_WIDTH + 6.0;
/// The wires: where they are held on a pole (metres across from it, away from the road, and
/// up), how far they sag in the middle of a span, how thick they are drawn and in how many
/// pieces.
const WIRES: [(f64, f64); 3] = [(-0.7, 7.77), (0.7, 7.77), (0.0, 8.35)];
const WIRE_SAG: f64 = 0.9;
const WIRE_WIDTH: f64 = 0.03;
const WIRE_STEPS: u32 = 8;
/// The map's own way of the road ridden may lie this far off the drawn road: its points there
/// are the road, not a side street.
const ROAD_WAY_SLACK: f64 = 4.0;

/// Things standing by the road in one chunk.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Props {
    /// Instances by model name, as Godot `MultiMesh` buffers of 12 floats each: the transform
    /// relative to the chunk centre (row-major 3×4).
    pub models: BTreeMap<String, Vec<f32>>,
    /// Text on the signs: places' names and grades.
    pub labels: Vec<Label>,
    /// Power lines' wires, relative to the chunk centre.
    pub wires: MeshData,
}

/// Text on a sign's face.
#[derive(Debug, Clone, PartialEq)]
pub struct Label {
    /// What it says.
    pub text: String,
    /// Where it stands, relative to the chunk centre (row-major 3×4), its text facing +z.
    pub transform: [f32; 12],
}

/// What props are placed by: the chunk's ground, the land cover, the road ridden, where streets
/// join it, the streets' room and the buildings around.
#[derive(Clone, Copy)]
pub(crate) struct Roadside<'a> {
    pub(crate) heights: &'a HeightGrid,
    pub(crate) land: &'a LandIndex,
    pub(crate) road: &'a RoadIndex,
    pub(crate) mouths: &'a [Mouth],
    pub(crate) streets: &'a Clearance,
    pub(crate) buildings: &'a [Footprint],
    /// Places' names and where they are (metres east/north).
    pub(crate) places: &'a [(String, (f64, f64))],
    pub(crate) urban: &'a crate::urban::Urban,
}

/// Places props by their road point within `[origin, origin + size]` (metres east/north),
/// except benches, which belong to the chunk containing their sidewalk. Each decision uses
/// the road and ground near its own point, so neighbouring chunks agree about placement.
pub(crate) fn place(
    origin: (f64, f64),
    size: f64,
    roadside: &Roadside,
    chunk_origin: [f64; 3],
) -> Props {
    let mut props = Props::default();
    let (low, high) = (origin, (origin.0 + size, origin.1 + size));
    rails(&mut props, low, high, roadside, chunk_origin);
    posts(&mut props, low, high, roadside, chunk_origin);
    town(&mut props, low, high, roadside, chunk_origin);
    benches(&mut props, low, high, roadside, chunk_origin);
    fences(&mut props, low, high, roadside, chunk_origin);
    crosses(&mut props, low, high, roadside, chunk_origin);
    bales(&mut props, low, high, roadside, chunk_origin);
    signs(&mut props, low, high, roadside, chunk_origin);
    power_lines(&mut props, low, high, roadside, chunk_origin);
    props
}

/// Attach ground-mounted markers across chunk boundaries to the final terrain. Guardrails
/// are fitted to their owning carriageway instead: another hairpin can cut the bank below it.
#[allow(clippy::cast_possible_truncation)] // f32 instance buffers
pub(crate) fn ground_protection(chunks: &mut [crate::TerrainChunk]) {
    let terrain = crate::foundations::Ground(chunks);
    let mut corrections = Vec::new();
    for (chunk_index, chunk) in chunks.iter().enumerate() {
        for name in ["snow_pole", "delineator"] {
            let Some(buffer) = chunk.props.models.get(name) else {
                continue;
            };
            for (index, transform) in buffer.as_chunks::<12>().0.iter().enumerate() {
                if transform[3].abs().max(transform[11].abs()) <= crate::CHUNK_SIZE as f32 / 2.0 {
                    continue;
                }
                let east = f64::from(transform[3] + chunk.center[0]);
                let north = -f64::from(transform[11] + chunk.center[2]);
                if let Some(height) = terrain.at(east, north) {
                    corrections.push((chunk_index, name, index, height as f32 - chunk.center[1]));
                }
            }
        }
    }
    for (chunk, name, index, height) in corrections {
        let buffer = chunks[chunk]
            .props
            .models
            .get_mut(name)
            .expect("existing protection");
        buffer[index * 12 + 7] = height;
    }
}

/// A fixed model cannot be clipped halfway through a blade or plank. Remove spans crossing
/// actual pavement, including roads joining between their endpoints.
pub(crate) fn clear_fences(
    chunks: &mut [crate::TerrainChunk],
    pavement: &crate::pavement::Pavement,
) {
    for chunk in chunks {
        for (name, buffer) in &mut chunk.props.models {
            let half = match name.as_str() {
                "guard_rail" | "guard_rail_start" | "guard_rail_end" => RAIL_SPAN / 2.0,
                "fence" => FENCE_SPAN / 2.0,
                _ => continue,
            };
            *buffer = buffer
                .as_chunks::<12>()
                .0
                .iter()
                .filter(|t| {
                    let ends = [-half, half].map(|z| {
                        [0, 1, 2].map(|k| {
                            f64::from(t[k * 4 + 3])
                                + f64::from(chunk.center[k])
                                + f64::from(t[k * 4 + 2]) * z
                        })
                    });
                    if name.starts_with("guard_rail_") {
                        // Terminal blades flare 35 cm away from the road, beyond the post.
                        !pavement.crosses_width(ends[0], ends[1], 0.5)
                    } else {
                        !pavement.crosses(ends[0], ends[1])
                    }
                })
                .flatten()
                .copied()
                .collect();
        }
    }
}

/// Where a power line's pole stands at a station, if one does: along country roads in about
/// one in `LINE_SHARE` of the `LINE_STRETCH` stretches, on the side the stretch draws, every
/// `LINE_POLE_SPACING`, where there is room. Decided from the station alone, so the next pole's
/// place is known to the chunk drawing the wire to it.
fn pole_at(station: &Station, roadside: &Roadside) -> Option<(f64, [f64; 3])> {
    if station.surface != Surface::Ground || built_up(station, roadside) {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)] // stretch numbers along the route
    let stretch = (station.distance / LINE_STRETCH).floor() as i64;
    let seed = stretch.wrapping_mul(53) ^ 0x706f_6c65;
    if crate::hash(seed) >= LINE_SHARE {
        return None;
    }
    let side = if crate::hash(seed ^ 1) < 0.5 {
        1.0
    } else {
        -1.0
    };
    room(station, side, LINE_OFFSET, 0.5, roadside).map(|spot| (side, spot))
}

/// Power lines: wooden poles every `LINE_POLE_SPACING` and three wires sagging from each pole to
/// the next, built as a mesh (`Props::wires`) since no fixed model bends with the road.
fn power_lines(
    props: &mut Props,
    low: (f64, f64),
    high: (f64, f64),
    roadside: &Roadside,
    chunk_origin: [f64; 3],
) {
    let reach = LINE_POLE_SPACING * 1.5;
    let around = roadside.road.stations(
        LINE_POLE_SPACING,
        (low.0 - reach, low.1 - reach),
        (high.0 + reach, high.1 + reach),
    );
    #[allow(clippy::cast_possible_truncation)] // pole numbers along the route
    let by_step: BTreeMap<i64, &Station> = around
        .iter()
        .map(|s| ((s.distance / LINE_POLE_SPACING).round() as i64, s))
        .collect();
    for (&step, &station) in &by_step {
        let (east, north) = station.position;
        let inside = (low.0..high.0).contains(&east) && (low.1..high.1).contains(&north);
        if !inside {
            continue;
        }
        let Some((side, spot)) = pole_at(station, roadside) else {
            continue;
        };
        push(props, "power_pole", spot, station, side, 0.0, chunk_origin);
        let Some(next) = by_step.get(&(step + 1)) else {
            continue;
        };
        let Some((next_side, next_spot)) = pole_at(next, roadside) else {
            continue;
        };
        if (next_side - side).abs() > 0.5 {
            continue;
        }
        for &(across, up) in &WIRES {
            let from = attach(station, side, spot, across, up);
            let to = attach(next, side, next_spot, across, up);
            wire(&mut props.wires, from, to, chunk_origin);
        }
    }
}

/// Where a wire is held on a pole: `across` metres from it away from the road, `up` above its
/// foot (metres east, height, north).
fn attach(station: &Station, side: f64, spot: [f64; 3], across: f64, up: f64) -> [f64; 3] {
    let (de, dn) = station.direction;
    let (oe, on) = (dn * side, -de * side);
    [spot[0] + oe * across, spot[1] + up, spot[2] + on * across]
}

/// A wire hanging from `from` to `to`, sagging `WIRE_SAG` in its middle, as two thin crossed
/// ribbons so it shows from any side.
#[allow(clippy::cast_possible_truncation)] // geometry is stored as f32 for the GPU
fn wire(mesh: &mut MeshData, from: [f64; 3], to: [f64; 3], origin: [f64; 3]) {
    let point = |t: f64| -> [f64; 3] {
        let sag = WIRE_SAG * 4.0 * t * (1.0 - t);
        [
            from[0] + (to[0] - from[0]) * t,
            from[1] + (to[1] - from[1]) * t - sag,
            from[2] + (to[2] - from[2]) * t,
        ]
    };
    let (de, dn) = (to[0] - from[0], to[2] - from[2]);
    let length = de.hypot(dn).max(1e-6);
    // Across the wire, level, and straight up: the two ribbons' widths.
    let across = [
        -dn / length * WIRE_WIDTH / 2.0,
        0.0,
        de / length * WIRE_WIDTH / 2.0,
    ];
    let up = [0.0, WIRE_WIDTH / 2.0, 0.0];
    for (offset, normal) in [
        (across, [0.0, 1.0, 0.0]),
        (up, [-dn / length, 0.0, de / length]),
    ] {
        for k in 0..WIRE_STEPS {
            let (a, b) = (
                point(f64::from(k) / f64::from(WIRE_STEPS)),
                point(f64::from(k + 1) / f64::from(WIRE_STEPS)),
            );
            #[allow(clippy::cast_possible_truncation)] // a few thousand vertices a chunk
            let base = mesh.vertices.len() as u32;
            for p in [a, b] {
                for sign in [-1.0, 1.0] {
                    // Godot axes: x east, y up, z south.
                    mesh.vertices.push([
                        (p[0] + offset[0] * sign - origin[0]) as f32,
                        (p[1] + offset[1] * sign - origin[1]) as f32,
                        (-(p[2] + offset[2] * sign) - origin[2]) as f32,
                    ]);
                    mesh.normals
                        .push([normal[0] as f32, normal[1] as f32, -normal[2] as f32]);
                    mesh.uvs.push([0.0, 0.0]);
                }
            }
            mesh.indices
                .extend([base, base + 1, base + 3, base, base + 3, base + 2]);
        }
    }
}

/// Road signs on the right of travel: a place's name where the road enters it and its end
/// sign where it leaves; a bend sign before hairpins and a grade sign before steep stretches.
/// Each looks along the road from its own point (`SIGN_LOOK` either way), so it stands the
/// same whichever chunk places it.
fn signs(
    props: &mut Props,
    low: (f64, f64),
    high: (f64, f64),
    roadside: &Roadside,
    chunk_origin: [f64; 3],
) {
    let around = roadside.road.stations(
        SIGN_STEP,
        (low.0 - SIGN_LOOK, low.1 - SIGN_LOOK),
        (high.0 + SIGN_LOOK, high.1 + SIGN_LOOK),
    );
    #[allow(clippy::cast_possible_truncation)] // step numbers along the route
    let by_step: BTreeMap<i64, &Station> = around
        .iter()
        .map(|s| ((s.distance / SIGN_STEP).round() as i64, s))
        .collect();
    let at = |step: i64| by_step.get(&step).copied();
    let built = |step: i64| at(step).is_some_and(|s| built_up(s, roadside));
    // Town by majority over `TOWN_SMOOTH` steps either side: a gap between houses or a lone
    // farm does not end or start a village.
    let town = |step: i64| {
        let count = (-TOWN_SMOOTH..=TOWN_SMOOTH)
            .filter(|&k| built(step + k))
            .count();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a few steps
        let half = TOWN_SMOOTH as usize;
        count > half
    };
    for (&step, &station) in &by_step {
        let (east, north) = station.position;
        let inside = (low.0..high.0).contains(&east) && (low.1..high.1).contains(&north);
        if !inside || station.surface != Surface::Ground {
            continue;
        }
        // Into a town and out of it, steadily (`SIGN_STEADY` steps either side).
        let steady = SIGN_STEADY;
        let entering = (0..steady).all(|k| town(step + k)) && (1..=steady).all(|k| !town(step - k));
        let leaving = (1..=steady).all(|k| town(step - k)) && (0..steady).all(|k| !town(step + k));
        if (entering || leaving)
            && let Some(name) = place_near(roadside, east, north)
        {
            let model = if entering {
                "sign_place"
            } else {
                "sign_place_end"
            };
            if let Some(spot) = room(station, 1.0, SIGN_OFFSET, 0.4, roadside) {
                let transform = push(props, model, spot, station, 1.0, 0.0, chunk_origin);
                props
                    .labels
                    .push(label(&transform, name, PLACE_LABEL_HEIGHT));
            }
            continue;
        }
        if built_up(station, roadside) {
            continue;
        }
        // A hairpin ahead: the road turns more than `HAIRPIN_TURN` between `BEND_AHEAD` and
        // `BEND_AHEAD + BEND_SPAN` on; the sign stands where that first holds.
        let turn_ahead = |from: i64| -> Option<f64> {
            let start = at(from + BEND_AHEAD)?;
            let end = at(from + BEND_AHEAD + BEND_SPAN)?;
            let (a, b) = (start.direction, end.direction);
            let turn = (a.0 * b.1 - a.1 * b.0).atan2(a.0 * b.0 + a.1 * b.1);
            (turn.abs() > HAIRPIN_TURN).then_some(turn)
        };
        if let Some(turn) = turn_ahead(step)
            && turn_ahead(step - 1).is_none()
        {
            // Turning left (counter-clockwise) is a positive turn.
            let model = if turn > 0.0 {
                "sign_bend_left"
            } else {
                "sign_bend_right"
            };
            if let Some(spot) = room(station, 1.0, SIGN_OFFSET, 0.4, roadside) {
                push(props, model, spot, station, 1.0, 0.0, chunk_origin);
            }
            continue;
        }
        // A steep stretch ahead: the next `GRADE_SPAN` climb or fall at `STEEP` or more on
        // average.
        let grade_ahead = |from: i64| -> Option<f64> {
            let (start, end) = (at(from)?, at(from + GRADE_SPAN)?);
            let grade = (end.elevation - start.elevation) / (end.distance - start.distance);
            (grade.abs() >= STEEP).then_some(grade)
        };
        if let Some(grade) = grade_ahead(step)
            && grade_ahead(step - 1).is_none()
            && let Some(spot) = room(station, 1.0, SIGN_OFFSET, 0.4, roadside)
        {
            let transform = push(props, "sign_grade", spot, station, 1.0, 0.0, chunk_origin);
            let percent = format!("{:.0} %", grade.abs() * 100.0);
            props
                .labels
                .push(label(&transform, percent, GRADE_LABEL_HEIGHT));
        }
    }
}

/// The name of the place nearest a point, within `PLACE_REACH`.
fn place_near(roadside: &Roadside, east: f64, north: f64) -> Option<String> {
    roadside
        .places
        .iter()
        .map(|(name, (e, n))| (name, (e - east).hypot(n - north)))
        .filter(|(_, d)| *d < PLACE_REACH)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(name, _)| name.clone())
}

/// A label on a sign's face: the sign's transform, moved up to `height` and just in front of
/// its face (the model's +z in Godot axes, toward the traffic it faces).
fn label(transform: &[f32; 12], text: String, height: f32) -> Label {
    let mut placed = *transform;
    // Column 1 is up, column 2 the face's normal.
    for row in 0..3 {
        placed[row * 4 + 3] += transform[row * 4 + 1] * height + transform[row * 4 + 2] * 0.035;
    }
    Label {
        text,
        transform: placed,
    }
}

/// Whether the land at a point is pasture or field: mapped so, or open land the map leaves
/// unmarked, which in the country is mostly grazed or mown.
fn pasture(roadside: &Roadside, east: f64, north: f64) -> bool {
    matches!(
        roadside.land.cover_at(east, north),
        Some(LandCover::Meadow | LandCover::Farmland) | None
    )
}

/// Pasture fences span by span along stretches where the land beside the road is meadow or
/// field, outside towns and where no rail stands; whether a stretch is fenced is decided once
/// for it, so fences run on rather than flicker.
fn fences(
    props: &mut Props,
    low: (f64, f64),
    high: (f64, f64),
    roadside: &Roadside,
    chunk_origin: [f64; 3],
) {
    for station in roadside.road.stations(FENCE_SPAN, low, high) {
        if station.surface != Surface::Ground || built_up(&station, roadside) {
            continue;
        }
        #[allow(clippy::cast_possible_truncation)] // stretch numbers along the route
        let stretch = (station.distance / FENCE_STRETCH).floor() as i64;
        for side in [-1.0, 1.0] {
            #[allow(clippy::cast_possible_truncation)] // ±1
            let salt = side as i64;
            let fenced = crate::hash(stretch.wrapping_mul(31) ^ salt ^ 0x6665_6e63) < FENCE_SHARE;
            let (east, north) = out(&station, side, FENCE_OFFSET);
            if !fenced
                || !pasture(roadside, east, north)
                || falls_away(&station, side, roadside)
                || room(&station, side, FENCE_OFFSET, 0.2, roadside).is_none()
            {
                continue;
            }
            let at = beside(&station, side, FENCE_OFFSET, roadside);
            push(
                props,
                "fence",
                at,
                &station,
                side,
                station.grade,
                chunk_origin,
            );
        }
    }
}

/// A wayside cross just past about one in `CROSS_SHARE` of the lanes joining the road outside
/// towns, facing the road.
fn crosses(
    props: &mut Props,
    low: (f64, f64),
    high: (f64, f64),
    roadside: &Roadside,
    chunk_origin: [f64; 3],
) {
    for station in roadside.road.stations(TOWN_STEP, low, high) {
        if station.surface != Surface::Ground || built_up(&station, roadside) {
            continue;
        }
        for &(along, side, half) in roadside.mouths {
            let past = station.distance - (along + half + 3.0);
            #[allow(clippy::cast_possible_truncation)] // whole metres along the route
            let seed = along.round() as i64;
            if !(0.0..TOWN_STEP).contains(&past) || crate::hash(seed ^ 0x6372_6f73) >= CROSS_SHARE {
                continue;
            }
            if let Some(at) = room(&station, side, CROSS_OFFSET, 0.4, roadside) {
                push(
                    props,
                    "wayside_cross",
                    at,
                    &station,
                    side,
                    0.0,
                    chunk_origin,
                );
            }
        }
    }
}

/// Round hay bales, straw or wrapped, lying in fields near the road: one to three together at
/// a few spots of a grid, each turned its own way.
#[allow(clippy::cast_possible_truncation)] // grid cells and small counts
fn bales(
    props: &mut Props,
    low: (f64, f64),
    high: (f64, f64),
    roadside: &Roadside,
    chunk_origin: [f64; 3],
) {
    let first = (
        (low.0 / BALE_GRID).ceil() as i64,
        (low.1 / BALE_GRID).ceil() as i64,
    );
    let last = (
        (high.0 / BALE_GRID).ceil() as i64,
        (high.1 / BALE_GRID).ceil() as i64,
    );
    for i in first.0..last.0 {
        for j in first.1..last.1 {
            let seed = i.wrapping_mul(92_821) ^ j.wrapping_mul(68_917) ^ 0x6261_6c65;
            if crate::hash(seed) >= BALE_SHARE {
                continue;
            }
            #[allow(clippy::cast_precision_loss)] // grid cells
            let (east, north) = (i as f64 * BALE_GRID, j as f64 * BALE_GRID);
            let near = roadside
                .road
                .nearest(east, north, BALE_FAR)
                .map(|(d, _, _)| d);
            if !near.is_some_and(|d| d > BALE_NEAR)
                || roadside.land.cover_at(east, north) != Some(LandCover::Farmland)
            {
                continue;
            }
            #[allow(clippy::cast_sign_loss)] // hash is in [0, 1)
            let count = 1 + (crate::hash(seed ^ 1) * 3.0) as usize;
            let model = if crate::hash(seed ^ 2) < 0.4 {
                "hay_bale_wrapped"
            } else {
                "hay_bale"
            };
            for k in 0..count {
                #[allow(clippy::cast_possible_wrap)] // small counts
                let salt = seed ^ (k as i64 + 3).wrapping_mul(7919);
                let (e, n) = (
                    east + (crate::hash(salt) - 0.5) * 6.0,
                    north + (crate::hash(salt ^ 5) - 0.5) * 6.0,
                );
                let blocked = roadside.streets.blocked(e, n, 1.5)
                    || roadside
                        .buildings
                        .iter()
                        .any(|b| b.wall_distance(e, n) < 2.0);
                if blocked {
                    continue;
                }
                let yaw = crate::hash(salt ^ 9) * std::f64::consts::TAU;
                let ground = roadside.heights.at(e, n);
                let lying = Station {
                    appearance: crate::appearance::Appearance::default(),
                    position: (e, n),
                    elevation: ground,
                    distance: 0.0,
                    direction: (yaw.cos(), yaw.sin()),
                    grade: 0.0,
                    surface: Surface::Ground,
                };
                push(props, model, [e, ground, n], &lying, 1.0, 0.0, chunk_origin);
            }
        }
    }
}

/// In towns: street lamps staggered along both sides and a bus shelter every
/// `SHELTER_SPACING`; each only where it has room
/// (clear of buildings, side streets and where streets join).
fn town(
    props: &mut Props,
    low: (f64, f64),
    high: (f64, f64),
    roadside: &Roadside,
    chunk_origin: [f64; 3],
) {
    for station in roadside.road.stations(TOWN_STEP, low, high) {
        if station.surface != Surface::Ground || !built_up(&station, roadside) {
            continue;
        }
        let step = (station.distance / TOWN_STEP).round();
        // Lamps: on the right at whole multiples of the spacing, on the left half way between.
        let half = LAMP_SPACING / TOWN_STEP / 2.0;
        if step % half == 0.0 {
            let side = if (step / half) % 2.0 == 0.0 {
                1.0
            } else {
                -1.0
            };
            if let Some(at) = room(&station, side, LAMP_OFFSET, 0.4, roadside) {
                push(props, "street_lamp", at, &station, side, 0.0, chunk_origin);
            }
            continue;
        }
        if (station.distance / SHELTER_SPACING).fract().abs() < 1e-6
            && let Some(at) = room(&station, 1.0, SHELTER_OFFSET, 2.5, roadside)
        {
            push(props, "bus_shelter", at, &station, 1.0, 0.0, chunk_origin);
        }
    }
}

/// Seats belong to the chunk containing their sidewalk, rather than their road point: its
/// refined terrain heights are the ones the pavement is drawn on, even beside a chunk border.
fn benches(
    props: &mut Props,
    low: (f64, f64),
    high: (f64, f64),
    roadside: &Roadside,
    chunk_origin: [f64; 3],
) {
    let reach = BENCH_OFFSET + 23.0;
    let inside = |p: [f64; 3]| (low.0..high.0).contains(&p[0]) && (low.1..high.1).contains(&p[2]);
    let stations = roadside.road.stations(
        TOWN_STEP,
        (low.0 - reach, low.1 - reach),
        (high.0 + reach, high.1 + reach),
    );
    for anchor in &stations {
        if (anchor.distance - BENCH_PHASE).rem_euclid(BENCH_SPACING) >= 1e-6 {
            continue;
        }
        let preferred = if (anchor.distance / BENCH_SPACING).floor() % 2.0 == 0.0 {
            1.0
        } else {
            -1.0
        };
        // A little room along the block avoids losing a whole short city stretch to one
        // entrance. These phases also stay away from the lamps' 30 m stations.
        'candidate: for shift in [0.0, -10.0, 20.0] {
            let Some(station) = stations
                .iter()
                .find(|s| (s.distance - anchor.distance - shift).abs() < 1e-6)
            else {
                continue;
            };
            if station.surface != Surface::Ground || !built_up(station, roadside) {
                continue;
            }
            for side in [preferred, -preferred] {
                if let Some(seating) = bench_spots(station, side, roadside) {
                    if inside(seating.seat) && inside(seating.bin) && seating.level {
                        push(
                            props,
                            "bench",
                            seating.seat,
                            station,
                            side,
                            0.0,
                            chunk_origin,
                        );
                        push(props, "bin", seating.bin, station, side, 0.0, chunk_origin);
                    }
                    // The choice of side uses geometry alone, so neighbouring chunks make
                    // the same decision and cannot each place their own copy of a seat.
                    break 'candidate;
                }
            }
        }
    }
}

struct Seating {
    seat: [f64; 3],
    bin: [f64; 3],
    level: bool,
}

/// A bench and bin need a continuous, nearly level sidewalk outside entrances. Trying the
/// other side before dropping a candidate keeps short city stretches from losing all seats.
fn bench_spots(station: &Station, side: f64, roadside: &Roadside) -> Option<Seating> {
    // The sidewalk queries check the long footprint; this radius clears its narrow depth.
    let mut seat = room(station, side, BENCH_OFFSET, 0.3, roadside)?;
    let bin_station = Station {
        position: (
            station.position.0 + station.direction.0 * BIN_ALONG,
            station.position.1 + station.direction.1 * BIN_ALONG,
        ),
        ..*station
    };
    let mut bin = room(&bin_station, side, BENCH_OFFSET, 0.3, roadside)?;
    let lift_at = |point| roadside.urban.sidewalk_lift(point, roadside.heights);
    let (mut low, mut high) = (f64::MAX, f64::MIN);
    let owner = (
        (seat[0] / crate::CHUNK_SIZE).floor() * crate::CHUNK_SIZE,
        (seat[2] / crate::CHUNK_SIZE).floor() * crate::CHUNK_SIZE,
    );
    for along in [-0.95, 0.0, 0.95, BIN_ALONG, BIN_ALONG + 0.3] {
        for across in [-0.3, 0.32] {
            let (east, north) = out(station, side, BENCH_OFFSET + across);
            let point = (
                east + station.direction.0 * along,
                north + station.direction.1 * along,
            );
            let lift = lift_at(point)?;
            // A pair crossing a chunk edge tries another spot; the complete footprint then
            // samples one refined terrain grid. This decision is independent of the caller.
            if !(owner.0..owner.0 + crate::CHUNK_SIZE).contains(&point.0)
                || !(owner.1..owner.1 + crate::CHUNK_SIZE).contains(&point.1)
            {
                return None;
            }
            let height = roadside.heights.at(point.0, point.1) + lift;
            low = low.min(height);
            high = high.max(height);
        }
    }
    seat[1] += lift_at((seat[0], seat[2]))?;
    bin[1] += lift_at((bin[0], bin[2]))?;
    Some(Seating {
        seat,
        bin,
        level: high - low <= 0.12,
    })
}

/// The spot `offset` metres out on `side`, on the ground, if something of `reach` metres
/// fits there: off the road's edge where no street joins, clear of streets and buildings.
fn room(
    station: &Station,
    side: f64,
    offset: f64,
    reach: f64,
    roadside: &Roadside,
) -> Option<[f64; 3]> {
    if !open(station, side, roadside) {
        return None;
    }
    let (east, north) = out(station, side, offset);
    // The road ridden is one of the map's streets too: only the others count.
    let other_street = |e: f64, n: f64| roadside.road.edge_distance(e, n) >= ROAD_WAY_SLACK;
    let blocked = roadside
        .streets
        .blocked_by(east, north, reach + TOWN_CLEAR, other_street)
        || roadside
            .buildings
            .iter()
            .any(|b| b.wall_distance(east, north) < reach + TOWN_CLEAR);
    (!blocked).then(|| beside(station, side, offset, roadside))
}

/// Guard rails on the side where the ground falls away, span by span. A span stands where the
/// drop is there and next to it (so single dips get none and single gaps are bridged), and a
/// run starts and ends in a span sloping down into the ground.
fn rails(
    props: &mut Props,
    low: (f64, f64),
    high: (f64, f64),
    roadside: &Roadside,
    chunk_origin: [f64; 3],
) {
    // Two spans beyond the square, for the neighbours each decision looks at.
    let margin = 2.0 * RAIL_SPAN;
    let stations = roadside.road.stations(
        RAIL_SPAN,
        (low.0 - margin, low.1 - margin),
        (high.0 + margin, high.1 + margin),
    );
    for side in [-1.0, 1.0] {
        let drops: Vec<bool> = stations
            .iter()
            .map(|station| falls_away(station, side, roadside))
            .collect();
        // Neighbours are the stations a span apart; the road may leave and re-enter the area.
        let next_to = |i: usize, step: isize| -> Option<usize> {
            let j = i.checked_add_signed(step)?;
            let apart = stations.get(j)?.distance - stations[i].distance;
            #[allow(clippy::cast_precision_loss)] // one or two steps
            let wanted = RAIL_SPAN * step as f64;
            ((apart - wanted).abs() < 0.5).then_some(j)
        };
        let drop_at = |i: usize, step: isize| next_to(i, step).is_some_and(|j| drops[j]);
        let rail_at = |i: usize| {
            let here = drops[i] && (drop_at(i, -1) || drop_at(i, 1));
            let bridged = drop_at(i, -1) && drop_at(i, 1);
            (here || bridged) && open(&stations[i], side, roadside)
        };
        for (i, station) in stations.iter().enumerate() {
            let (east, north) = station.position;
            let inside = (low.0..high.0).contains(&east) && (low.1..high.1).contains(&north);
            if !inside {
                continue;
            }
            let rail = rail_at(i);
            let before = next_to(i, -1).is_some_and(rail_at);
            let after = next_to(i, 1).is_some_and(rail_at);
            // A run's ends slope down into the ground away from it: `guard_rail_end` down
            // ahead (the model's −z), `guard_rail_start` down behind. Along the road ridden that
            // is ahead on the right and behind on the left.
            let (ahead, behind) = if side > 0.0 {
                ("guard_rail_end", "guard_rail_start")
            } else {
                ("guard_rail_start", "guard_rail_end")
            };
            let model = match (rail, before, after) {
                (true, _, _) => Some("guard_rail"),
                (false, true, false) => Some(ahead),
                (false, false, true) => Some(behind),
                _ => None,
            };
            if let Some(name) = model
                && open(station, side, roadside)
            {
                rail_span(props, name, station, side, roadside, chunk_origin);
            }
        }
    }
}

/// Fit the four-metre model between shared points on the offset edge. Using a centre-line
/// tangent instead leaves gaps outside bends and overlapping blades on their inside.
fn rail_span(
    props: &mut Props,
    name: &str,
    station: &Station,
    side: f64,
    roadside: &Roadside,
    origin: [f64; 3],
) {
    let ends = [-0.5, 0.5].map(|step| {
        roadside
            .road
            .station_at(station.distance + step * RAIL_SPAN)
    });
    let [Some(a), Some(b)] = ends else {
        return;
    };
    if a.surface != Surface::Ground
        || b.surface != Surface::Ground
        || !open(&a, side, roadside)
        || !open(&b, side, roadside)
    {
        return;
    }
    // Buried post feet anchor in the road's closed edge/skirt. Sampling the cut bank can
    // put the entire blade below the asphalt when another arm of a hairpin runs lower.
    let anchor = |station: &Station| {
        let (east, north) = out(station, side, RAIL_OFFSET);
        [east, station.elevation - ROAD_SINK, north]
    };
    let (a, b) = (anchor(&a), anchor(&b));
    let (de, dn) = (b[0] - a[0], b[2] - a[2]);
    let length = de.hypot(dn);
    if length < 0.1 {
        return;
    }
    let middle = [0, 1, 2].map(|k| f64::midpoint(a[k], b[k]));
    let edge = Station {
        direction: (de / length, dn / length),
        ..*station
    };
    let transform = push(
        props,
        name,
        middle,
        &edge,
        side,
        (b[1] - a[1]) / length,
        origin,
    );
    // Scale only the model's longitudinal basis: the sheet reaches both endpoints and its
    // posts remain upright with their original width and height.
    #[allow(clippy::cast_possible_truncation)] // f32 instance buffers
    let scale = (length / RAIL_SPAN) as f32;
    let buffer = props.models.get_mut(name).expect("just inserted span");
    let start = buffer.len() - transform.len();
    for k in [2, 6, 10] {
        buffer[start + k] *= scale;
    }
}

/// Delineator posts every `POST_SPACING` along country roads, or snow poles every
/// `POLE_SPACING` above the snow line, on both sides; not in towns, on bridges or where a rail
/// stands (its blades carry the reflectors).
fn posts(
    props: &mut Props,
    low: (f64, f64),
    high: (f64, f64),
    roadside: &Roadside,
    chunk_origin: [f64; 3],
) {
    for station in roadside.road.stations(POLE_SPACING, low, high) {
        if station.surface != Surface::Ground || built_up(&station, roadside) {
            continue;
        }
        let snowy = station.elevation >= SNOW_LINE;
        let on_post_spacing = (station.distance / POST_SPACING).fract().abs() < 1e-6;
        if !snowy && !on_post_spacing {
            continue;
        }
        for side in [-1.0, 1.0] {
            if !open(&station, side, roadside) {
                continue;
            }
            let railed = falls_away(&station, side, roadside);
            let (name, offset) = match (snowy, railed) {
                // Behind the rail.
                (true, true) => ("snow_pole", RAIL_OFFSET + 0.5),
                (true, false) => ("snow_pole", POLE_OFFSET),
                (false, false) => ("delineator", POST_OFFSET),
                (false, true) => continue,
            };
            let at = beside(&station, side, offset, roadside);
            push(props, name, at, &station, side, 0.0, chunk_origin);
        }
    }
}

/// Whether the ground falls away beside the road on `side` (1 right of travel, −1 left): open
/// road on the ground, its edge well above the ground a few metres out.
fn falls_away(station: &Station, side: f64, roadside: &Roadside) -> bool {
    if station.surface != Surface::Ground {
        return false;
    }
    let (east, north) = out(station, side, DROP_PROBE);
    station.elevation - roadside.heights.at(east, north) >= RAIL_DROP
}

/// Whether the land at the road is built up: towns get lamps, not posts. Built-up land on the
/// map, or where it maps none, a few buildings close to the road.
fn built_up(station: &Station, roadside: &Roadside) -> bool {
    let (east, north) = station.position;
    let mapped = matches!(
        roadside.land.cover_at(east, north),
        Some(
            LandCover::Residential
                | LandCover::Industrial
                | LandCover::Commercial
                | LandCover::Public
        )
    );
    mapped
        || roadside
            .buildings
            .iter()
            .filter(|b| b.distance(east, north) < VILLAGE_REACH)
            .count()
            >= VILLAGE_BUILDINGS
}

/// Whether the road's edge on `side` is free here: no street joins it nearby.
fn open(station: &Station, side: f64, roadside: &Roadside) -> bool {
    !roadside.mouths.iter().any(|&(along, mouth_side, half)| {
        // Sides are ±1.
        (mouth_side - side).abs() < 1.0 && (along - station.distance).abs() < half + MOUTH_CLEAR
    })
}

/// The point `offset` metres from the centre line on `side`, on the ground there.
fn beside(station: &Station, side: f64, offset: f64, roadside: &Roadside) -> [f64; 3] {
    let (east, north) = out(station, side, offset);
    [east, roadside.heights.at(east, north), north]
}

/// The point `offset` metres from the centre line on `side`, in metres east/north.
fn out(station: &Station, side: f64, offset: f64) -> (f64, f64) {
    let (de, dn) = station.direction;
    let offset = offset - ROAD_HALF_WIDTH + station.appearance.width / 2.0;
    // Right of travel is the direction turned clockwise.
    let (re, rn) = (dn * side, -de * side);
    (
        station.position.0 + re * offset,
        station.position.1 + rn * offset,
    )
}

/// Adds an instance of `name` at `at`, its back to the road on `facing` (its +x away from the
/// road, its length along the road), sheared to follow `grade` so posts stay upright. Returns
/// its transform.
#[allow(clippy::cast_possible_truncation)] // geometry is stored as f32 for the GPU
fn push(
    props: &mut Props,
    name: &str,
    [east, height, north]: [f64; 3],
    station: &Station,
    facing: f64,
    grade: f64,
    origin: [f64; 3],
) -> [f32; 12] {
    let (de, dn) = station.direction;
    // Godot axes: x east, y up, z south. The model's +x points away from the road; its −z runs
    // along the road the way it is ridden on the right side and back on the left, rising with
    // the grade.
    let x = [dn * facing, 0.0, de * facing];
    let z = [-de * facing, -grade * facing, dn * facing];
    let (tx, ty, tz) = (east - origin[0], height - origin[1], -north - origin[2]);
    let transform = [
        x[0], 0.0, z[0], tx, //
        x[1], 1.0, z[1], ty, //
        x[2], 0.0, z[2], tz,
    ]
    .map(|v| v as f32);
    props
        .models
        .entry(name.to_owned())
        .or_default()
        .extend(transform);
    transform
}

#[cfg(test)]
mod tests {
    /// Every model the rules place, as `art/props` builds them.
    const PLACED: [&str; 19] = [
        "guard_rail",
        "guard_rail_end",
        "guard_rail_start",
        "delineator",
        "snow_pole",
        "street_lamp",
        "bench",
        "bin",
        "bus_shelter",
        "fence",
        "hay_bale",
        "hay_bale_wrapped",
        "wayside_cross",
        "sign_place",
        "sign_place_end",
        "sign_bend_left",
        "sign_bend_right",
        "sign_grade",
        "power_pole",
    ];

    #[test]
    fn every_placed_model_is_built_and_coloured() {
        let directory = concat!(env!("CARGO_MANIFEST_DIR"), "/../../app/assets/models/props");
        for name in PLACED {
            let file = format!("{directory}/{name}.glb");
            assert!(std::path::Path::new(&file).exists(), "{file}");
        }
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../../../app/assets/models/props/models.json"))
                .unwrap();
        for name in PLACED {
            assert!(
                manifest["models"].get(name).is_some(),
                "{name} in models.json"
            );
        }
        // The app colours each material from the palette; `srgb` panics on a missing colour.
        for material in [
            "galvanised",
            "post",
            "band",
            "reflector",
            "pole_red",
            "pole_white",
            "metal_dark",
            "wood",
            "lamp",
            "glass",
            "wood_weathered",
            "hay",
            "wrap",
            "stone",
            "sign_blue",
            "sign_white",
            "sign_red",
            "sign_black",
            "insulator",
            "wire",
        ] {
            crate::palette::srgb(&format!("props.{material}"), 1.0);
        }
    }
}
