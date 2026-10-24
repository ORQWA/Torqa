//! Inferred urban paving and building access (R105, R112). Geometry follows the drawn terrain;
//! mapped green spaces and infrastructure take precedence over inferred surroundings.

use std::cell::RefCell;
use std::collections::HashMap;

use orqwa_osm::LandCover;
use orqwa_routes::Surface;

use crate::buildings::{self, Plot, Point, Setting};
use crate::junctions::{Corner, FootwayCorner, FootwaySection};
use crate::landcover::LandIndex;
use crate::road::RoadIndex;
use crate::streets::{Clearance, Street};
use crate::{CHUNK_SIZE, HeightGrid, MeshData, On, ROAD_SINK, drape, palette};

const CELL: f64 = 50.0;
const WALK_WIDTH: f64 = 1.8;
const MIN_WALK_LENGTH: f64 = 12.0;
const CURB_HEIGHT: f64 = 0.14;
const CURB_WIDTH: f64 = 0.18;
const CURB_BEVEL: f64 = 0.025;
const GAP: f64 = 0.12;
const ACCESS_REACH: f64 = 35.0;
const DENSE_RADIUS: f64 = 50.0;
const DENSE_COVERAGE: f64 = 0.24;

/// Material codes in the urban mesh's vertex colour, separate from terrain's cover colours.
#[derive(Clone, Copy, PartialEq)]
enum Material {
    Paving,
    Concrete,
    Asphalt,
    Curb,
}

impl Material {
    fn color(self) -> [f32; 4] {
        let code = match self {
            Self::Paving => 0.0,
            Self::Concrete => 0.25,
            Self::Asphalt => 0.5,
            Self::Curb => 0.75,
        };
        [code, 0.0, 0.0, 1.0]
    }
}

struct Site {
    outline: Vec<Point>,
    cuts: Vec<[Point; 3]>,
    center: Point,
    area: f64,
    business: bool,
    town: bool,
}

#[derive(Clone, Copy)]
struct Way {
    a: Point,
    b: Point,
    half: f64,
    end_half: f64,
    lift: f64,
    elevations: Option<[f64; 2]>,
}

#[derive(Clone, Copy)]
struct Profile {
    a: Point,
    b: Point,
    elevations: [Option<f64>; 2],
    span: [f64; 2],
}

impl Way {
    fn half_at(self, point: Point) -> f64 {
        let along =
            distance(self.a, project(point, self.a, self.b)) / distance(self.a, self.b).max(0.01);
        self.half + (self.end_half - self.half) * along
    }

    fn profile(self) -> Profile {
        Profile {
            a: self.a,
            b: self.b,
            elevations: self.elevations.map_or([None; 2], |ends| ends.map(Some)),
            span: [0.0, 1.0],
        }
    }
}

struct Patch {
    polygon: Vec<Point>,
    material: Material,
    lift: f64,
    /// Raised sidewalks have solid sides; their first edge follows the road.
    curb: bool,
    wall: bool,
    /// Access paths rise gently to meet their street and the lowered sidewalk opening.
    ramp: Option<(Point, Point, f64)>,
    /// Walking surfaces follow their street, not a bank cut by another nearby road.
    profile: Option<Profile>,
}

#[derive(Clone, Copy)]
struct CurveSection {
    lifts: [f64; 2],
    parents: [Option<usize>; 2],
    span: [f64; 2],
}

struct Merge {
    masks: Vec<Vec<Point>>,
    patches: Vec<usize>,
}

type Grade = (Point, [f64; 3]);
type Grades = RefCell<HashMap<usize, Grade>>;

/// Immutable, spatially indexed urban surfaces, shared by all chunk builders.
#[derive(Default)]
pub(crate) struct Urban {
    pavement: crate::pavement::Pavement,
    sites: Vec<Site>,
    site_cells: HashMap<(i64, i64), Vec<usize>>,
    building_cells: HashMap<(i64, i64), Vec<usize>>,
    ways: Vec<Way>,
    way_cells: HashMap<(i64, i64), Vec<usize>>,
    covered: HashMap<(i64, i64), Vec<(Point, Point)>>,
    patches: Vec<Patch>,
    patch_cells: HashMap<(i64, i64), Vec<usize>>,
    chunk_cells: HashMap<(i64, i64), Vec<usize>>,
    curves: HashMap<usize, CurveSection>,
    zones: Vec<Vec<Point>>,
    zone_owners: Vec<Vec<usize>>,
    zone_cells: HashMap<(i64, i64), Vec<usize>>,
    junctions: Vec<Vec<Point>>,
    junction_cells: HashMap<(i64, i64), Vec<usize>>,
    shapes: Vec<Vec<Vec<Point>>>,
    merges: Vec<Merge>,
    merge_cells: HashMap<(i64, i64), Vec<usize>>,
    ends: Vec<[f64; 2]>,
}

// Kept outside the builder so its decisions can be queried identically by terrain and plants.
impl Urban {
    pub(crate) fn grade_points(&self) -> Vec<Point> {
        let mut needed = vec![false; self.patches.len()];
        for group in &self.merges {
            for &i in &group.patches {
                needed[i] = true;
            }
        }
        for (i, patch) in self.patches.iter().enumerate() {
            if patch.curb && cells(&patch.polygon, CHUNK_SIZE, 0.0).len() > 1 {
                needed[i] = true;
            }
        }
        for (&i, curve) in &self.curves {
            if needed[i] {
                for parent in curve.parents.into_iter().flatten() {
                    needed[parent] = true;
                }
            }
        }
        let mut points = Vec::new();
        for (i, patch) in self.patches.iter().enumerate() {
            if needed[i]
                && let Some(profile) = patch.profile
            {
                for (p, y) in [profile.a, profile.b].into_iter().zip(profile.elevations) {
                    if y.is_none() {
                        points.push(p);
                    }
                }
            }
        }
        points.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
        points.dedup_by(|a, b| a.0.to_bits() == b.0.to_bits() && a.1.to_bits() == b.1.to_bits());
        points
    }

    pub(crate) fn pin_grades(&mut self, samples: impl IntoIterator<Item = (Point, f64)>) {
        let samples: HashMap<_, _> = samples
            .into_iter()
            .map(|(p, y)| ((p.0.to_bits(), p.1.to_bits()), y))
            .collect();
        for patch in &mut self.patches {
            if let Some(profile) = &mut patch.profile {
                for (p, y) in [profile.a, profile.b]
                    .into_iter()
                    .zip(&mut profile.elevations)
                {
                    if y.is_none() {
                        *y = samples.get(&(p.0.to_bits(), p.1.to_bits())).copied();
                    }
                }
            }
        }
    }

    pub(crate) fn build<'a>(
        plots: impl Iterator<Item = &'a Plot<'a>>,
        streets: &[Street],
        corners: &[Corner],
        clearance: &Clearance,
        road: &RoadIndex,
        land: &LandIndex,
        pavement: crate::pavement::Pavement,
    ) -> Self {
        let mut plots: Vec<_> = plots.collect();
        plots.sort_by_key(|plot| plot.building.id);
        let mut urban = Self {
            pavement,
            ..Self::default()
        };
        for plot in plots {
            let outline = buildings::drawn_outline(&plot.footprint);
            let mut mask = outline.clone();
            if signed_area(&mask) < 0.0 {
                mask.reverse();
            }
            let cuts = buildings::triangulate(&mask)
                .into_iter()
                .map(|tri| tri.map(|i| mask[i as usize]))
                .collect();
            let center = centroid(&outline);
            let area = polygon_area(&outline);
            let index = urban.sites.len();
            urban
                .site_cells
                .entry(cell(center, CELL))
                .or_default()
                .push(index);
            for key in cells(&outline, CELL, 0.0) {
                urban.building_cells.entry(key).or_default().push(index);
            }
            urban.sites.push(Site {
                outline,
                cuts,
                center,
                area,
                business: plot.shop.is_some()
                    || plot.purpose.is_some()
                    || matches!(
                        plot.setting,
                        Setting::Commercial | Setting::Industrial | Setting::Public
                    ),
                town: matches!(plot.setting, Setting::Town),
            });
        }
        urban.collect_ways(streets, road);
        urban.walks(land);
        urban.corner_walks(corners, land, road);
        urban.bend_walks(land);
        urban.accesses(land);
        for corner in corners {
            let outline: Vec<_> = corner.outline().collect();
            for pair in outline[1..].windows(2) {
                let triangle = vec![outline[0], pair[0], pair[1]];
                let index = urban.junctions.len();
                for key in cells(&triangle, CELL, 0.0) {
                    urban.junction_cells.entry(key).or_default().push(index);
                }
                urban.junctions.push(triangle);
            }
        }
        urban.finish(land, clearance, road);
        urban
    }

    fn collect_ways(&mut self, streets: &[Street], road: &RoadIndex) {
        for street in streets
            .iter()
            .filter(|s| s.appearance.sidewalk && !s.on_bridge())
        {
            for pair in street.points().windows(2) {
                let direction = direction(pair[0], pair[1]);
                let middle = midpoint(pair[0], pair[1]);
                if road.owns_mapped(middle, direction, street.sources()) {
                    for key in cells(pair, CELL, 0.0) {
                        self.covered
                            .entry(key)
                            .or_default()
                            .push((pair[0], pair[1]));
                    }
                    continue;
                }
                self.add_way(Way {
                    a: pair[0],
                    b: pair[1],
                    half: street.half_width(),
                    end_half: street.half_width(),
                    lift: street.lift(),
                    elevations: None,
                });
            }
        }
        let stations = road.stations(3.0, (f64::MIN, f64::MIN), (f64::MAX, f64::MAX));
        for pair in stations.windows(2) {
            if pair.iter().any(|s| s.surface != Surface::Ground)
                || !pair[0].appearance.sidewalk
                || pair[1].distance - pair[0].distance > 3.1
            {
                continue;
            }
            self.add_way(Way {
                a: pair[0].position,
                b: pair[1].position,
                half: pair[0].appearance.width / 2.0,
                end_half: pair[1].appearance.width / 2.0,
                lift: ROAD_SINK,
                elevations: Some([pair[0].elevation - ROAD_SINK, pair[1].elevation - ROAD_SINK]),
            });
        }
    }

    fn add_way(&mut self, way: Way) {
        if distance(way.a, way.b) < 0.01 {
            return;
        }
        let index = self.ways.len();
        for key in cells(&[way.a, way.b], CELL, 0.0) {
            self.way_cells.entry(key).or_default().push(index);
        }
        self.ways.push(way);
    }

    fn nearest_way(&self, point: Point) -> Option<(Point, Way)> {
        Self::near(&self.way_cells, point, ACCESS_REACH)
            .map(|index| self.ways[index])
            .map(|way| (project(point, way.a, way.b), way))
            .filter(|(p, _)| distance(point, *p) < ACCESS_REACH)
            .min_by(|a, b| distance(point, a.0).total_cmp(&distance(point, b.0)))
    }

    fn near(
        index: &HashMap<(i64, i64), Vec<usize>>,
        point: Point,
        reach: f64,
    ) -> impl Iterator<Item = usize> + '_ {
        let keys = cells(&[point], CELL, reach);
        keys.into_iter()
            .flat_map(|key| index.get(&key).into_iter().flatten().copied())
    }

    fn dense(&self, point: Point) -> bool {
        let area: f64 = Self::near(&self.site_cells, point, DENSE_RADIUS)
            .map(|index| &self.sites[index])
            .filter(|site| distance(point, site.center) < DENSE_RADIUS)
            .map(|site| site.area)
            .sum();
        area > std::f64::consts::PI * DENSE_RADIUS.powi(2) * DENSE_COVERAGE
    }

    fn urban_at(&self, point: Point, land: &LandIndex) -> bool {
        !protected(point, land)
            && (matches!(
                land.cover_at(point.0, point.1),
                Some(
                    LandCover::Residential
                        | LandCover::Commercial
                        | LandCover::Industrial
                        | LandCover::Public
                )
            ) || self.dense(point))
    }

    fn walks(&mut self, land: &LandIndex) {
        for index in 0..self.ways.len() {
            let way = self.ways[index];
            if way.half < 2.5 || !self.urban_at(midpoint(way.a, way.b), land) {
                continue;
            }
            let start = self.walk_join(index, way.a);
            let end = self.walk_join(index, way.b);
            for sign in [-1.0, 1.0] {
                let offset = |p: Point, right: Point, width: f64| {
                    (p.0 + right.0 * width * sign, p.1 + right.1 * width * sign)
                };
                let inner = way.half + GAP;
                let end_inner = way.end_half + GAP;
                self.add_patch(Patch {
                    polygon: vec![
                        offset(way.a, start, inner),
                        offset(way.b, end, end_inner),
                        offset(way.b, end, end_inner + WALK_WIDTH),
                        offset(way.a, start, inner + WALK_WIDTH),
                    ],
                    material: Material::Concrete,
                    lift: way.lift + CURB_HEIGHT,
                    curb: true,
                    wall: false,
                    ramp: None,
                    profile: Some(way.profile()),
                });
            }
        }
    }

    fn corner_walks(&mut self, corners: &[Corner], land: &LandIndex, road: &RoadIndex) {
        for corner in corners {
            if let Some(footway) = corner.footway(GAP, WALK_WIDTH)
                && self.urban_at(centroid(&footway.zone), land)
            {
                let elevations = footway.bases.map(|(p, ridden)| {
                    ridden
                        .then(|| {
                            road.nearest(p.0, p.1, 12.0)
                                .map(|(_, height, _)| height - ROAD_SINK)
                        })
                        .flatten()
                });
                let profile = Profile {
                    a: footway.bases[0].0,
                    b: footway.bases[1].0,
                    elevations,
                    span: [0.0, 1.0],
                };
                self.add_curve(footway, profile, false);
            }
        }
    }

    fn add_curve(&mut self, mut footway: FootwayCorner, profile: Profile, replace: bool) {
        let parent = |point: Point| {
            Self::near(&self.patch_cells, point, 0.1)
                .filter(|&index| self.patches[index].curb && !self.curves.contains_key(&index))
                .map(|index| (polygon_distance(&self.patches[index].polygon, point), index))
                .filter(|&(distance, _)| distance < 0.1)
                .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)))
                .map(|(_, index)| index)
        };
        let parents = [
            footway
                .sections
                .first()
                .and_then(|s| parent(midpoint(s.polygon[0], s.polygon[3]))),
            footway
                .sections
                .last()
                .and_then(|s| parent(midpoint(s.polygon[1], s.polygon[2]))),
        ];
        // A fillet cannot justify a sidewalk by itself. Both walking approaches must
        // exist, including on service arms and at the edge of an urban area.
        if parents.iter().any(Option::is_none) {
            return;
        }
        if signed_area(&footway.zone) > 0.0 {
            footway.zone.reverse();
        }
        if replace {
            let zone = self.zones.len();
            self.zone_owners
                .push(self.replacement_owners(&footway.zone, parents));
            for key in cells(&footway.zone, CELL, 0.0) {
                self.zone_cells.entry(key).or_default().push(zone);
            }
            self.zones.push(footway.zone);
        }
        for FootwaySection {
            polygon,
            mut lifts,
            span,
        } in footway.sections
        {
            for lift in &mut lifts {
                *lift += CURB_HEIGHT;
            }
            if signed_area(&polygon) > 0.0 {
                lifts.reverse();
            }
            let curve_span = if signed_area(&polygon) > 0.0 {
                [span[1], span[0]]
            } else {
                span
            };
            self.curves.insert(
                self.patches.len(),
                CurveSection {
                    lifts,
                    parents,
                    span: curve_span,
                },
            );
            self.add_patch(Patch {
                polygon,
                material: Material::Concrete,
                lift: lifts[0].max(lifts[1]),
                curb: true,
                wall: false,
                ramp: None,
                profile: Some(Profile { span, ..profile }),
            });
        }
    }

    fn replacement_owners(&self, zone: &[Point], parents: [Option<usize>; 2]) -> Vec<usize> {
        let mut pending: Vec<_> = parents.into_iter().flatten().collect();
        let mut owners = Vec::new();
        while let Some(i) = pending.pop() {
            if owners.contains(&i) {
                continue;
            }
            owners.push(i);
            let p = &self.patches[i].polygon;
            for (a, b) in [(p[0], p[3]), (p[1], p[2])] {
                for other in Self::near(&self.patch_cells, midpoint(a, b), 0.001) {
                    if owners.contains(&other)
                        || !self.patches[other].curb
                        || self.curves.contains_key(&other)
                    {
                        continue;
                    }
                    let p = &self.patches[other].polygon;
                    let joins = [(p[0], p[3]), (p[1], p[2])].into_iter().any(|(c, d)| {
                        (distance(a, c) < 0.0001 && distance(b, d) < 0.0001)
                            || (distance(a, d) < 0.0001 && distance(b, c) < 0.0001)
                    });
                    if joins && overlaps(p, zone) {
                        pending.push(other);
                    }
                }
            }
        }
        owners.sort_unstable();
        owners
    }

    fn bend_walks(&mut self, land: &LandIndex) {
        for index in 0..self.ways.len() {
            let way = self.ways[index];
            if way.half < 2.5 {
                continue;
            }
            for point in [way.a, way.b] {
                let neighbours = self.walk_neighbours(index, point);
                let [other] = neighbours.as_slice() else {
                    continue;
                };
                if *other < index || !self.urban_at(point, land) {
                    continue;
                }
                let other = self.ways[*other];
                let at_end = |way: Way| {
                    way.elevations
                        .map(|ends| ends[usize::from(distance(way.b, point) < 1e-6)])
                };
                let profile = Profile {
                    a: point,
                    b: point,
                    elevations: [at_end(way), at_end(other)],
                    span: [0.0, 1.0],
                };
                let from = if distance(other.a, point) < 1e-6 {
                    other.b
                } else {
                    other.a
                };
                let to = if distance(way.a, point) < 1e-6 {
                    way.b
                } else {
                    way.a
                };
                let (before, after) = (direction(from, point), direction(point, to));
                let turn = (before.0 * after.1 - before.1 * after.0)
                    .atan2(before.0 * after.0 + before.1 * after.1);
                if !(0.12..=1.8).contains(&turn.abs()) {
                    continue;
                }
                let side = turn.signum();
                let start = (-before.0 * side).atan2(before.1 * side);
                let inner = way.half_at(point) + GAP;
                let radial = |angle: f64, radius: f64| {
                    (
                        point.0 + angle.cos() * radius,
                        point.1 + angle.sin() * radius,
                    )
                };
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a short arc
                let count = (turn.abs() / 0.12).ceil() as usize;
                #[allow(clippy::cast_precision_loss)] // a short arc
                let fraction = |i: usize| i as f64 / count as f64;
                let angle = |i: usize| start + turn * fraction(i);
                // Enclose the original outer miter, not the neighbouring street. At the
                // bisector the miter reaches R/cos(half_turn), while this triangle's far
                // edge reaches reach*cos(half_turn).
                let reach = (inner + WALK_WIDTH) / (turn / 2.0).cos().powi(2) + 0.02;
                let zone = vec![point, radial(start, reach), radial(start + turn, reach)];
                let sections = (0..count)
                    .map(|i| FootwaySection {
                        polygon: vec![
                            radial(angle(i), inner),
                            radial(angle(i + 1), inner),
                            radial(angle(i + 1), inner + WALK_WIDTH),
                            radial(angle(i), inner + WALK_WIDTH),
                        ],
                        lifts: [way.lift; 2],
                        span: [fraction(i), fraction(i + 1)],
                    })
                    .collect();
                self.add_curve(
                    FootwayCorner {
                        bases: [(point, false); 2],
                        zone,
                        sections,
                    },
                    profile,
                    true,
                );
            }
        }
    }

    fn finish(&mut self, land: &LandIndex, clearance: &Clearance, road: &RoadIndex) {
        for (index, patch) in self.patches.iter().enumerate() {
            let mut shapes = vec![if patch.curb {
                convex_hull(&patch.polygon)
            } else {
                patch.polygon.clone()
            }];
            if patch.curb {
                let mut zones: Vec<_> = cells(&patch.polygon, CELL, 0.0)
                    .into_iter()
                    .flat_map(|key| self.zone_cells.get(&key).into_iter().flatten().copied())
                    .collect();
                zones.sort_unstable();
                zones.dedup();
                for zone in zones {
                    if self.curves.contains_key(&index)
                        || self.zone_owners[zone].binary_search(&index).is_err()
                    {
                        continue;
                    }
                    shapes = subtract_all(shapes, &self.zones[zone]);
                }
                let mut ways: Vec<_> = cells(&patch.polygon, CELL, 4.0)
                    .into_iter()
                    .flat_map(|key| self.way_cells.get(&key).into_iter().flatten().copied())
                    .collect();
                ways.sort_unstable();
                ways.dedup();
                for way in ways {
                    let way = self.ways[way];
                    // Cut only the part occupying another carriageway, rather than
                    // discarding the whole slab and exposing square, disconnected ends.
                    shapes = subtract_all(shapes, &way_ribbon(way, 0.04));
                }
                // Rounded carriageway fans occupy more than the straight arm ribbons.
                // Their old inner sidewalk miters must not survive as detached rims.
                let mut junctions: Vec<_> = cells(&patch.polygon, CELL, 0.0)
                    .into_iter()
                    .flat_map(|key| self.junction_cells.get(&key).into_iter().flatten().copied())
                    .collect();
                junctions.sort_unstable();
                junctions.dedup();
                for junction in junctions {
                    shapes = subtract_all(shapes, &self.junctions[junction]);
                }
                let mut sites: Vec<_> = cells(&patch.polygon, CELL, 0.0)
                    .into_iter()
                    .flat_map(|key| self.building_cells.get(&key).into_iter().flatten().copied())
                    .collect();
                sites.sort_unstable();
                sites.dedup();
                for site in sites {
                    for mask in &self.sites[site].cuts {
                        // Keep the walking width that remains beside a protruding wall.
                        shapes = subtract_all(shapes, mask);
                    }
                }
            }
            shapes.retain(|polygon| self.clear_polygon(polygon, land, clearance, road));
            self.shapes.push(shapes);
        }
        self.ends = (0..self.patches.len()).map(|i| self.walk_ends(i)).collect();
        self.merge_walks();
    }

    fn merge_walks(&mut self) {
        let mut links: Vec<_> = (0..self.patches.len()).collect();
        let mut overlaps = Vec::new();
        let mut neighbours = Vec::with_capacity(self.patches.len());
        // A tight island can bring opposite footways over the same ground. Keep one top
        // there, with a common grade, rather than intersecting separate slabs.
        for index in 0..self.patches.len() {
            let patch = &self.patches[index];
            if !patch.curb {
                neighbours.push(Vec::new());
                continue;
            }
            let mut others: Vec<_> = cells(&patch.polygon, CELL, 0.0)
                .into_iter()
                .flat_map(|key| self.patch_cells.get(&key).into_iter().flatten().copied())
                .filter(|&other| other < index && self.patches[other].curb)
                .collect();
            others.sort_unstable();
            others.dedup();
            for &other in &others {
                let overlap = split_by_mask(
                    &convex_hull(&patch.polygon),
                    &convex_hull(&self.patches[other].polygon),
                )
                .1;
                if !self.shapes[index].is_empty()
                    && !self.shapes[other].is_empty()
                    && polygon_area(&overlap) > 0.00001
                {
                    let (a, b) = (root(&links, index), root(&links, other));
                    links[a.max(b)] = a.min(b);
                    overlaps.push((index, other));
                }
            }
            neighbours.push(others);
        }
        // Establish connectivity before subtracting any owner. Otherwise a piece already
        // claimed by an earlier one can hide a connection and split one island's grade.
        let mut involved = vec![false; self.patches.len()];
        for &(a, b) in &overlaps {
            involved[a] = true;
            involved[b] = true;
        }
        for (index, others) in neighbours.iter().enumerate() {
            if !involved[index] {
                continue;
            }
            for &other in others {
                if involved[other]
                    && self.patches[index].polygon.iter().any(|&p| {
                        polygon_distance(&convex_hull(&self.patches[other].polygon), p) < 1e-6
                    })
                {
                    let (a, b) = (root(&links, index), root(&links, other));
                    links[a.max(b)] = a.min(b);
                }
            }
        }
        for (index, others) in neighbours.into_iter().enumerate() {
            let mut shapes = std::mem::take(&mut self.shapes[index]);
            for other in others {
                for mask in &self.shapes[other] {
                    shapes = subtract_all(shapes, mask);
                }
            }
            self.shapes[index] = shapes;
        }
        self.retain_walk_runs();
        let mut groups = HashMap::new();
        for (a, b) in overlaps {
            let group = *groups.entry(root(&links, a)).or_insert_with(|| {
                self.merges.push(Merge {
                    masks: Vec::new(),
                    patches: Vec::new(),
                });
                self.merges.len() - 1
            });
            self.merges[group].patches.extend([a, b]);
        }
        for (index, group) in self.merges.iter_mut().enumerate() {
            group.patches.sort_unstable();
            group.patches.dedup();
            // The whole participating piece shares the grade, including its exposed rim.
            // Restricting the common plane to the intersection leaves steps beside it.
            group.masks = group
                .patches
                .iter()
                .map(|&i| convex_hull(&self.patches[i].polygon))
                .collect();
            for mask in &group.masks {
                for key in cells(mask, CELL, WALK_WIDTH) {
                    self.merge_cells.entry(key).or_default().push(index);
                }
            }
        }
        for groups in self.merge_cells.values_mut() {
            groups.sort_unstable();
            groups.dedup();
        }
    }

    fn retain_walk_runs(&mut self) {
        // Work on the surviving footprint, before chunking. A single source strip can
        // leave several disconnected remnants; many short strips can form one long walk.
        let pieces: Vec<_> = self
            .shapes
            .iter()
            .enumerate()
            .filter(|&(i, _)| self.patches[i].curb)
            .flat_map(|(i, shapes)| (0..shapes.len()).map(move |j| (i, j)))
            .collect();
        let mut links: Vec<_> = (0..pieces.len()).collect();
        let mut index: HashMap<_, Vec<usize>> = HashMap::new();
        for (i, &(patch, shape)) in pieces.iter().enumerate() {
            let polygon = &self.shapes[patch][shape];
            let keys = cells(polygon, CELL, 0.0001);
            let mut neighbours: Vec<_> = keys
                .iter()
                .flat_map(|key| index.get(key).into_iter().flatten().copied())
                .collect();
            neighbours.sort_unstable();
            neighbours.dedup();
            for other in neighbours {
                let (p, s) = pieces[other];
                // A shared corner alone is not a usable walking connection.
                if walking_connection(polygon, &self.shapes[p][s]) {
                    let (a, b) = (root(&links, i), root(&links, other));
                    links[a.max(b)] = a.min(b);
                }
            }
            for key in keys {
                index.entry(key).or_default().push(i);
            }
        }
        let mut spans: HashMap<_, Vec<(f64, f64)>> = HashMap::new();
        for (i, &(patch, shape)) in pieces.iter().enumerate() {
            let p = &self.patches[patch].polygon;
            let (a, b) = (midpoint(p[0], p[3]), midpoint(p[1], p[2]));
            let d = direction(a, b);
            let along =
                |p: Point| ((p.0 - a.0) * d.0 + (p.1 - a.1) * d.1).clamp(0.0, distance(a, b));
            let (mut low, mut high) = (f64::INFINITY, f64::NEG_INFINITY);
            for &p in &self.shapes[patch][shape] {
                low = low.min(along(p));
                high = high.max(along(p));
            }
            spans
                .entry((root(&links, i), patch))
                .or_default()
                .push((low, high));
        }
        let mut lengths = vec![0.0; pieces.len()];
        // Boolean clipping can partition a strip across its width. Count each surviving
        // longitudinal interval once, rather than counting its tessellation or perimeter.
        let mut spans: Vec<_> = spans.into_iter().collect();
        spans.sort_by_key(|(key, _)| *key);
        for ((group, _), mut intervals) in spans {
            intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut end = 0.0_f64;
            for (low, high) in intervals {
                lengths[group] += (high - low.max(end)).max(0.0);
                end = end.max(high);
            }
        }
        let mut keep: Vec<_> = self.shapes.iter().map(|s| vec![true; s.len()]).collect();
        for (i, &(patch, shape)) in pieces.iter().enumerate() {
            keep[patch][shape] = lengths[root(&links, i)] + 1e-6 >= MIN_WALK_LENGTH;
        }
        for (shapes, keep) in self.shapes.iter_mut().zip(keep) {
            let mut flags = keep.into_iter();
            shapes.retain(|_| flags.next().unwrap_or(false));
        }
    }

    fn walk_join(&self, index: usize, point: Point) -> Point {
        let way = self.ways[index];
        let forward = direction(way.a, way.b);
        let right = (forward.1, -forward.0);
        let neighbours = self.walk_neighbours(index, point);
        let [other] = neighbours.as_slice() else {
            return right;
        };
        let other = self.ways[*other];
        let away = if distance(other.a, point) < 1e-6 {
            direction(other.a, other.b)
        } else {
            direction(other.b, other.a)
        };
        let sign = if distance(way.a, point) < 1e-6 {
            -1.0
        } else {
            1.0
        };
        let next = (away.0 * sign, away.1 * sign);
        let divisor = 1.0 + forward.0 * next.0 + forward.1 * next.1;
        if divisor < 0.5 {
            return right;
        }
        ((right.0 + next.1) / divisor, (right.1 - next.0) / divisor)
    }

    fn walk_neighbours(&self, index: usize, point: Point) -> Vec<usize> {
        let way = self.ways[index];
        let mut neighbours: Vec<_> = Self::near(&self.way_cells, point, 0.001)
            .filter(|&other| {
                let candidate = self.ways[other];
                other != index
                    && (candidate.half_at(point) - way.half_at(point)).abs() < 1e-6
                    && (candidate.lift - way.lift).abs() < 1e-6
                    && (distance(candidate.a, point) < 1e-6 || distance(candidate.b, point) < 1e-6)
            })
            .collect();
        neighbours.sort_unstable();
        neighbours.dedup();
        neighbours
    }

    fn accesses(&mut self, land: &LandIndex) {
        for index in 0..self.sites.len() {
            let site = &self.sites[index];
            if !self.urban_at(site.center, land) || site.outline.len() < 3 {
                continue;
            }
            let Some((street, way)) = self.nearest_way(site.center) else {
                continue;
            };
            let edge = (0..site.outline.len())
                .map(|k| (site.outline[k], site.outline[(k + 1) % site.outline.len()]))
                .min_by(|a, b| {
                    distance(street, midpoint(a.0, a.1))
                        .total_cmp(&distance(street, midpoint(b.0, b.1)))
                })
                .expect("a building outline");
            let door = midpoint(edge.0, edge.1);
            let reach = distance(door, street);
            let half = way.half_at(street);
            if reach <= half + GAP || reach > ACCESS_REACH {
                continue;
            }
            let to_street = direction(door, street);
            let end = (
                street.0 - to_street.0 * (half + GAP),
                street.1 - to_street.1 * (half + GAP),
            );
            let business = site.business;
            let town = site.town;
            let area = site.area;
            let front = direction(edge.0, edge.1);
            let apron_reach = if business { 3.0 } else { 0.9 };
            let outer = |p: Point| {
                (
                    p.0 + to_street.0 * apron_reach,
                    p.1 + to_street.1 * apron_reach,
                )
            };
            let apron = vec![edge.0, edge.1, outer(edge.1), outer(edge.0)];
            self.add_patch(Patch {
                polygon: apron,
                material: Material::Paving,
                lift: 0.025,
                curb: false,
                wall: false,
                ramp: None,
                profile: None,
            });
            let half = if business { 1.7 } else { 0.75 };
            self.add_patch(Patch {
                polygon: ribbon(door, end, half),
                material: if business {
                    Material::Asphalt
                } else {
                    Material::Paving
                },
                lift: 0.025,
                curb: false,
                wall: false,
                ramp: Some((door, end, way.lift + 0.005)),
                profile: Some(way.profile()),
            });
            // Small front-garden walls leave a broad opening for the access, inferred from
            // the house and road rather than pretending cadastral boundaries are mapped.
            if town && !business && area < 300.0 && reach > half + WALK_WIDTH + 4.0 {
                let at = (
                    end.0 - to_street.0 * (WALK_WIDTH + 0.7),
                    end.1 - to_street.1 * (WALK_WIDTH + 0.7),
                );
                for sign in [-1.0, 1.0] {
                    let a = (at.0 + front.0 * sign * 1.5, at.1 + front.1 * sign * 1.5);
                    let b = (at.0 + front.0 * sign * 3.8, at.1 + front.1 * sign * 3.8);
                    self.add_patch(Patch {
                        polygon: ribbon(a, b, 0.12),
                        material: Material::Concrete,
                        lift: 0.65,
                        curb: false,
                        wall: true,
                        ramp: None,
                        profile: None,
                    });
                }
            }
        }
    }

    fn add_patch(&mut self, mut patch: Patch) {
        if signed_area(&patch.polygon) > 0.0 {
            // drape_polygon expects clockwise corners; preserve the curb's first edge.
            patch.polygon.reverse();
            if patch.curb {
                patch.polygon.rotate_left(2);
                if let Some(profile) = &mut patch.profile {
                    std::mem::swap(&mut profile.a, &mut profile.b);
                    profile.elevations.swap(0, 1);
                    profile.span = [1.0 - profile.span[1], 1.0 - profile.span[0]];
                }
            }
        }
        let index = self.patches.len();
        for key in cells(&patch.polygon, CELL, 0.0) {
            self.patch_cells.entry(key).or_default().push(index);
        }
        for key in cells(&patch.polygon, CHUNK_SIZE, 0.0) {
            self.chunk_cells.entry(key).or_default().push(index);
        }
        self.patches.push(patch);
    }

    /// Whether plants would stand in inferred paving. Mapped green land always wins.
    pub(crate) fn paved_at(&self, point: Point, land: &LandIndex) -> bool {
        if protected(point, land) {
            return false;
        }
        if matches!(
            land.cover_at(point.0, point.1),
            Some(LandCover::Commercial | LandCover::Industrial)
        ) {
            return true;
        }
        if self.dense(point)
            && matches!(
                land.cover_at(point.0, point.1),
                Some(LandCover::Residential) | None
            )
        {
            return true;
        }
        self.patch_cells
            .get(&cell(point, CELL))
            .into_iter()
            .flatten()
            .any(|&index| {
                self.shapes[index]
                    .iter()
                    .any(|shape| contains(shape, point))
            })
    }

    /// Terrain between dense blocks is paved, while suburban gardens retain their lawns.
    pub(crate) fn paint(&self, mesh: &mut MeshData, origin: [f64; 3], land: &LandIndex) {
        for (v, color) in mesh.vertices.iter().zip(&mut mesh.colors) {
            let point = (f64::from(v[0]) + origin[0], -f64::from(v[2]) - origin[2]);
            if !protected(point, land)
                && matches!(
                    land.cover_at(point.0, point.1),
                    Some(LandCover::Residential) | None
                )
                && self.dense(point)
            {
                *color = palette::srgb("ground.urban", 0.0);
            }
        }
    }

    /// Height above terrain of a clear sidewalk, excluding entrance paths through it.
    pub(crate) fn sidewalk_lift(&self, point: Point, heights: &HeightGrid) -> Option<f64> {
        self.patch_cells
            .get(&cell(point, CELL))?
            .iter()
            .copied()
            .filter(|&index| {
                let patch = &self.patches[index];
                patch.curb
                    && self.shapes[index]
                        .iter()
                        .any(|shape| contains(shape, point))
                    && !self.access_crosses(&patch.polygon, true)
            })
            .map(|index| {
                self.walk_height(index, point, heights, &Grades::default())
                    - heights.at(point.0, point.1)
            })
            .max_by(f64::total_cmp)
    }

    fn base_lifts(&self, index: usize) -> [f64; 2] {
        self.curves
            .get(&index)
            .map_or([self.patches[index].lift; 2], |curve| curve.lifts)
    }

    fn walk_base(&self, index: usize, point: Point, heights: &HeightGrid) -> f64 {
        let patch = &self.patches[index];
        let fallback = || walk_base(patch, point, heights);
        let Some(curve) = self.curves.get(&index) else {
            return fallback();
        };
        let [start, end] = curve.parents.map(|parent| {
            parent.map_or_else(fallback, |i| walk_base(&self.patches[i], point, heights))
        });
        let along = curve.span[0] + (curve.span[1] - curve.span[0]) * walk_fraction(patch, point);
        start + (end - start) * along
    }

    fn walk_ends(&self, index: usize) -> [f64; 2] {
        if let Some(&ends) = self.ends.get(index) {
            return ends;
        }
        let patch = &self.patches[index];
        let base = self.base_lifts(index);
        let end_lift = |point, lift| {
            Self::near(&self.patch_cells, point, 0.001)
                .filter(|&index| {
                    let other = &self.patches[index];
                    other.curb
                        && polygon_distance(&other.polygon, point) < 1e-6
                        && self.access_crosses(&other.polygon, false)
                })
                .map(|index| {
                    walk_lift(&self.patches[index], self.base_lifts(index), point) - CURB_HEIGHT
                })
                .fold(lift, f64::min)
        };
        // A shared cross-section has one height even if just one adjoining piece
        // crosses an entrance. Its neighbour becomes the approach ramp.
        [
            end_lift(midpoint(patch.polygon[0], patch.polygon[3]), base[0]),
            end_lift(midpoint(patch.polygon[1], patch.polygon[2]), base[1]),
        ]
    }

    fn walk_height(
        &self,
        index: usize,
        point: Point,
        heights: &HeightGrid,
        grades: &Grades,
    ) -> f64 {
        let raw = self.walk_base(index, point, heights)
            + walk_lift(&self.patches[index], self.walk_ends(index), point);
        let (mut total, mut weight) = (0.0, 0.0);
        for &group in self
            .merge_cells
            .get(&cell(point, CELL))
            .into_iter()
            .flatten()
        {
            let distance = self.merges[group]
                .masks
                .iter()
                .map(|mask| polygon_distance(mask, point))
                .fold(f64::MAX, f64::min);
            let share = (1.0 - distance / WALK_WIDTH).clamp(0.0, 1.0);
            let share = share * share * (3.0 - 2.0 * share);
            if share > 0.0 {
                let (centre, plane) = *grades
                    .borrow_mut()
                    .entry(group)
                    .or_insert_with(|| self.merge_grade(group, heights));
                total +=
                    (plane[0] + plane[1] * (point.0 - centre.0) + plane[2] * (point.1 - centre.1))
                        * share;
                weight += share;
            }
        }
        if weight > 0.0 {
            raw * (1.0 - weight.min(1.0)) + total / weight * weight.min(1.0)
        } else {
            raw
        }
    }

    fn merge_grade(&self, group: usize, heights: &HeightGrid) -> Grade {
        let samples: Vec<_> = self.merges[group]
            .patches
            .iter()
            .flat_map(|&i| {
                self.patches[i].polygon.iter().map(move |&p| {
                    (
                        p,
                        self.walk_base(i, p, heights)
                            + walk_lift(&self.patches[i], self.walk_ends(i), p),
                    )
                })
            })
            .collect();
        let centre = centroid(&samples.iter().map(|s| s.0).collect::<Vec<_>>());
        #[allow(clippy::cast_precision_loss)] // bounded local patch group
        let mean = samples.iter().map(|s| s.1).sum::<f64>() / samples.len() as f64;
        let (mut xx, mut zz, mut xz, mut xy, mut zy) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for (p, y) in samples {
            let (x, z) = (p.0 - centre.0, p.1 - centre.1);
            xx += x * x;
            zz += z * z;
            xz += x * z;
            xy += x * (y - mean);
            zy += z * (y - mean);
        }
        let det = xx * zz - xz * xz;
        let (dx, dz) = if det > 1e-9 {
            ((xy * zz - zy * xz) / det, (zy * xx - xy * xz) / det)
        } else if xx > zz {
            (xy / xx.max(1e-9), 0.0)
        } else {
            (0.0, zy / zz.max(1e-9))
        };
        (centre, [mean, dx, dz])
    }

    fn clear_polygon(
        &self,
        polygon: &[Point],
        land: &LandIndex,
        clearance: &Clearance,
        road: &RoadIndex,
    ) -> bool {
        let valid = |point: Point| {
            !protected(point, land)
                && !clearance.blocked_urban_by(point.0, point.1, 0.02, |east, north| {
                    // Padded vegetation-clearance samples swallow sidewalks beside
                    // their own streets. Check those centre-line segments exactly below.
                    road.edge_distance(east, north) >= 0.5 && !self.on_way((east, north), true)
                })
                && !self.on_way(point, false)
                && !self.pavement.contains(point.0, point.1)
                && !self.in_building(point)
        };
        // Furniture and mesh generation must agree about which sidewalk pieces exist.
        valid(centroid(polygon))
            && polygon
                .iter()
                .zip(polygon.iter().cycle().skip(1))
                .take(polygon.len())
                .flat_map(|(&a, &b)| drape::densify(&[a, b], 1.0))
                .all(valid)
    }

    fn on_way(&self, point: Point, center_only: bool) -> bool {
        if center_only
            && self.covered.get(&cell(point, CELL)).is_some_and(|lines| {
                lines
                    .iter()
                    .any(|&(a, b)| distance(point, project(point, a, b)) < 0.001)
            })
        {
            return true;
        }
        Self::near(&self.way_cells, point, 4.0).any(|index| {
            let way = self.ways[index];
            let reach = if center_only {
                0.001
            } else {
                way.half_at(point) + 0.02
            };
            distance(point, project(point, way.a, way.b)) < reach
        })
    }

    fn exposed_walk_edge(&self, a: Point, b: Point, current: &[Point]) -> Vec<(Point, Point)> {
        let mut spans = vec![(0.0_f64, 1.0_f64)];
        let mut neighbours: Vec<_> = cells(&[a, b], CELL, 0.01)
            .into_iter()
            .flat_map(|key| self.patch_cells.get(&key).into_iter().flatten().copied())
            .collect();
        neighbours.sort_unstable();
        neighbours.dedup();
        for index in neighbours {
            if !self.patches[index].curb {
                continue;
            }
            for shape in &self.shapes[index] {
                if std::ptr::eq(shape.as_slice(), current) {
                    continue;
                }
                if let Some((low, high)) = segment_inside(a, b, shape) {
                    spans = spans
                        .into_iter()
                        .flat_map(|(start, end)| {
                            [(start, end.min(low)), (start.max(high), end)]
                                .into_iter()
                                .filter(|&(a, b)| b - a > 1e-8)
                        })
                        .collect();
                }
            }
            if spans.is_empty() {
                break;
            }
        }
        let along = |t: f64| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        spans
            .into_iter()
            .map(|(a, b)| (along(a), along(b)))
            .filter(|&(a, b)| distance(a, b) > 1e-4)
            .collect()
    }

    pub(crate) fn mesh(&self, heights: &HeightGrid, origin: [f64; 3]) -> MeshData {
        let mut mesh = MeshData::default();
        let grades = Grades::default();
        for &index in self
            .chunk_cells
            .get(&cell(heights.origin, CHUNK_SIZE))
            .into_iter()
            .flatten()
        {
            let patch = &self.patches[index];
            if patch.wall
                && patch
                    .polygon
                    .iter()
                    .map(|p| heights.at(p.0, p.1))
                    .fold(f64::MIN, f64::max)
                    - patch
                        .polygon
                        .iter()
                        .map(|p| heights.at(p.0, p.1))
                        .fold(f64::MAX, f64::min)
                    > 0.4
            {
                continue;
            }
            for shape in &self.shapes[index] {
                for (polygon, material) in top_parts(patch, shape) {
                    let first_index = patch_top(
                        &mut mesh,
                        patch,
                        &polygon,
                        material,
                        (heights, origin),
                        &|point| self.walk_height(index, point, heights, &grades),
                    );
                    if patch.curb {
                        walk_sides(
                            &mut mesh,
                            first_index,
                            shape,
                            patch,
                            heights,
                            origin,
                            &|a, b| self.exposed_walk_edge(a, b, shape),
                        );
                    } else if patch.wall {
                        for k in 0..patch.polygon.len() {
                            vertical(
                                &mut mesh,
                                patch.polygon[k],
                                patch.polygon[(k + 1) % patch.polygon.len()],
                                0.0,
                                patch.lift,
                                heights,
                                origin,
                            );
                        }
                    }
                }
            }
        }
        mesh
    }

    fn in_building(&self, point: Point) -> bool {
        self.building_cells
            .get(&cell(point, CELL))
            .into_iter()
            .flatten()
            .any(|&index| {
                let outline = &self.sites[index].outline;
                contains(outline, point)
                    && outline
                        .iter()
                        .zip(outline.iter().cycle().skip(1))
                        .take(outline.len())
                        .all(|(&a, &b)| distance(point, project(point, a, b)) > 0.08)
            })
    }

    fn access_crosses(&self, polygon: &[Point], entrances_only: bool) -> bool {
        cells(polygon, CELL, 0.0)
            .into_iter()
            .flat_map(|key| self.patch_cells.get(&key).into_iter().flatten())
            .map(|&index| &self.patches[index])
            .any(|patch| {
                !patch.curb
                    && !patch.wall
                    && (!entrances_only || patch.ramp.is_some())
                    && overlaps(polygon, &patch.polygon)
            })
    }
}

fn patch_top(
    mesh: &mut MeshData,
    patch: &Patch,
    polygon: &[Point],
    material: Material,
    (heights, origin): (&HeightGrid, [f64; 3]),
    top_height: &impl Fn(Point) -> f64,
) -> usize {
    let lift = patch.lift;
    let first = mesh.vertices.len();
    let first_index = mesh.indices.len();
    drape::drape_polygon(mesh, polygon, (On::Ground, lift), heights, origin, &uv);
    if patch.curb {
        for vertex in &mut mesh.vertices[first..] {
            let point = (
                f64::from(vertex[0]) + origin[0],
                -f64::from(vertex[2]) - origin[2],
            );
            #[allow(clippy::cast_possible_truncation)]
            // a small offset in GPU geometry
            {
                let bevel = if material == Material::Curb {
                    0.02 * (1.0
                        - distance(point, project(point, patch.polygon[0], patch.polygon[1]))
                            / CURB_BEVEL)
                        .clamp(0.0, 1.0)
                } else {
                    0.0
                };
                vertex[1] = (top_height(point) - bevel - origin[1]) as f32;
            }
        }
    }
    if let Some((start, end, end_lift)) = patch.ramp {
        let end_base = patch.profile.map_or_else(
            || heights.at(end.0, end.1),
            |profile| {
                let along = distance(profile.a, project(end, profile.a, profile.b))
                    / distance(profile.a, profile.b).max(0.01);
                profile_height(profile, along, heights)
            },
        );
        let end_change = end_base + end_lift - heights.at(end.0, end.1) - lift;
        for vertex in &mut mesh.vertices[first..] {
            let point = (
                f64::from(vertex[0]) + origin[0],
                -f64::from(vertex[2]) - origin[2],
            );
            let along =
                distance(start, project(point, start, end)) / distance(start, end).max(0.01);
            #[allow(clippy::cast_possible_truncation)]
            // a small offset in GPU geometry
            {
                vertex[1] += (end_change * along.powi(4)) as f32;
            }
        }
    }
    mesh.colors.resize(mesh.vertices.len(), material.color());
    if material == Material::Curb {
        for (vertex, uv) in mesh.vertices[first..].iter().zip(&mut mesh.uvs[first..]) {
            let point = (
                f64::from(vertex[0]) + origin[0],
                -f64::from(vertex[2]) - origin[2],
            );
            *uv = curb_uv(patch, point);
        }
    }
    surface_normals(mesh, first, first_index);
    first_index
}

fn protected(point: Point, land: &LandIndex) -> bool {
    [
        LandCover::Forest,
        LandCover::Meadow,
        LandCover::Water,
        LandCover::Rock,
        LandCover::Farmland,
        LandCover::Orchard,
    ]
    .into_iter()
    .any(|cover| land.has(point.0, point.1, cover))
}

fn cell(p: Point, size: f64) -> (i64, i64) {
    #[allow(clippy::cast_possible_truncation)]
    {
        ((p.0 / size).floor() as i64, (p.1 / size).floor() as i64)
    }
}

fn cells(points: &[Point], size: f64, reach: f64) -> Vec<(i64, i64)> {
    let low = points.iter().fold((f64::MAX, f64::MAX), |m, p| {
        (m.0.min(p.0 - reach), m.1.min(p.1 - reach))
    });
    let high = points.iter().fold((f64::MIN, f64::MIN), |m, p| {
        (m.0.max(p.0 + reach), m.1.max(p.1 + reach))
    });
    let (a, b) = (cell(low, size), cell(high, size));
    (a.0..=b.0)
        .flat_map(|x| (a.1..=b.1).map(move |y| (x, y)))
        .collect()
}

fn root(links: &[usize], mut index: usize) -> usize {
    while links[index] != index {
        index = links[index];
    }
    index
}

fn distance(a: Point, b: Point) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}
fn midpoint(a: Point, b: Point) -> Point {
    (a.0.midpoint(b.0), a.1.midpoint(b.1))
}
fn direction(a: Point, b: Point) -> Point {
    let d = distance(a, b).max(1e-9);
    ((b.0 - a.0) / d, (b.1 - a.1) / d)
}
fn project(p: Point, a: Point, b: Point) -> Point {
    let forward = direction(a, b);
    let along = ((p.0 - a.0) * forward.0 + (p.1 - a.1) * forward.1).clamp(0.0, distance(a, b));
    (a.0 + forward.0 * along, a.1 + forward.1 * along)
}
fn centroid(points: &[Point]) -> Point {
    #[allow(clippy::cast_precision_loss)]
    let n = points.len().max(1) as f64;
    (
        points.iter().map(|p| p.0).sum::<f64>() / n,
        points.iter().map(|p| p.1).sum::<f64>() / n,
    )
}
fn signed_area(points: &[Point]) -> f64 {
    points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
        .map(|(a, b)| a.0 * b.1 - a.1 * b.0)
        .sum::<f64>()
        * 0.5
}
fn polygon_area(points: &[Point]) -> f64 {
    signed_area(points).abs()
}

fn convex_hull(points: &[Point]) -> Vec<Point> {
    // An inner offset wider than the remaining island folds across itself. Its bounded
    // hull is the walkable remainder; exact carriageway masks still remove all road area.
    let mut points = points.to_vec();
    points.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    points.dedup_by(|a, b| distance(*a, *b) < 1e-9);
    let half = |points: Vec<Point>| {
        let mut edge: Vec<Point> = Vec::new();
        for p in points {
            while edge.len() > 1 {
                let (a, b) = (edge[edge.len() - 2], edge[edge.len() - 1]);
                if (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0) > 1e-10 {
                    break;
                }
                edge.pop();
            }
            edge.push(p);
        }
        edge
    };
    let mut lower = half(points.clone());
    points.reverse();
    let mut upper = half(points);
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower.reverse();
    lower
}

fn contains(points: &[Point], p: Point) -> bool {
    let mut inside = false;
    for (a, b) in points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
    {
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
            inside = !inside;
        }
    }
    inside
}

fn polygon_distance(polygon: &[Point], point: Point) -> f64 {
    if contains(polygon, point) {
        return 0.0;
    }
    polygon
        .iter()
        .copied()
        .zip(polygon.iter().copied().cycle().skip(1))
        .take(polygon.len())
        .map(|(a, b)| distance(point, project(point, a, b)))
        .fold(f64::MAX, f64::min)
}

fn subtract_all(subjects: Vec<Vec<Point>>, mask: &[Point]) -> Vec<Vec<Point>> {
    subjects
        .into_iter()
        .flat_map(|polygon| subtract(&polygon, mask))
        .collect()
}

/// Disjoint convex pieces outside a convex mask. Each cut keeps its outside
/// piece and carries the inside remainder to the next edge of the mask.
fn subtract(subject: &[Point], mask: &[Point]) -> Vec<Vec<Point>> {
    split_by_mask(subject, mask).0
}

fn split_by_mask(subject: &[Point], mask: &[Point]) -> (Vec<Vec<Point>>, Vec<Point>) {
    let bounds = |points: &[Point]| {
        points
            .iter()
            .fold((f64::MAX, f64::MAX, f64::MIN, f64::MIN), |b, p| {
                (b.0.min(p.0), b.1.min(p.1), b.2.max(p.0), b.3.max(p.1))
            })
    };
    let (a, b) = (bounds(subject), bounds(mask));
    if a.2 <= b.0 || b.2 <= a.0 || a.3 <= b.1 || b.3 <= a.1 {
        return (vec![subject.to_vec()], Vec::new());
    }
    let mut mask = mask.to_vec();
    if signed_area(&mask) > 0.0 {
        mask.reverse();
    }
    let mut inside = subject.to_vec();
    let mut outside = Vec::new();
    for (&a, &b) in mask
        .iter()
        .zip(mask.iter().cycle().skip(1))
        .take(mask.len())
    {
        // Boolean cuts can repeat a vertex. A zero-length edge must not return the
        // entire inside remainder as another outside piece.
        if distance(a, b) < 1e-6 {
            continue;
        }
        if inside.is_empty() {
            break;
        }
        let side = |p: Point| (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0);
        let mut kept = Vec::new();
        let mut removed = Vec::new();
        for (&p, &q) in inside
            .iter()
            .zip(inside.iter().cycle().skip(1))
            .take(inside.len())
        {
            let (sp, sq) = (side(p), side(q));
            if sp <= 0.0 {
                kept.push(p);
            }
            if sp >= 0.0 {
                removed.push(p);
            }
            if (sp < 0.0 && sq > 0.0) || (sp > 0.0 && sq < 0.0) {
                let t = sp / (sp - sq);
                let cross = (p.0 + (q.0 - p.0) * t, p.1 + (q.1 - p.1) * t);
                kept.push(cross);
                removed.push(cross);
            }
        }
        if polygon_area(&removed) > 1e-6 {
            outside.push(removed);
        }
        inside = kept;
    }
    (outside, inside)
}

fn top_parts(patch: &Patch, shape: &[Point]) -> Vec<(Vec<Point>, Material)> {
    if !patch.curb {
        return vec![(shape.to_vec(), patch.material)];
    }
    let mask = |width: f64| {
        let t = width / WALK_WIDTH;
        let blend = |a: Point, b: Point| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        vec![
            patch.polygon[0],
            patch.polygon[1],
            blend(patch.polygon[1], patch.polygon[2]),
            blend(patch.polygon[0], patch.polygon[3]),
        ]
    };
    let (walk, curb) = split_by_mask(shape, &mask(CURB_WIDTH));
    let mut parts: Vec<_> = walk.into_iter().map(|p| (p, patch.material)).collect();
    if polygon_area(&curb) > 1e-6 {
        let (flat, bevel) = split_by_mask(&curb, &mask(CURB_BEVEL));
        parts.extend(flat.into_iter().map(|p| (p, Material::Curb)));
        if polygon_area(&bevel) > 1e-6 {
            parts.push((bevel, Material::Curb));
        }
    }
    parts
}

#[allow(clippy::cast_possible_truncation)] // curb texture coordinates in metres
fn curb_uv(patch: &Patch, point: Point) -> [f32; 2] {
    let (a, b) = (patch.polygon[0], patch.polygon[1]);
    let d = direction(a, b);
    let p = (point.0 - a.0, point.1 - a.1);
    [
        (p.0 * d.0 + p.1 * d.1) as f32,
        (p.0 * d.1 - p.1 * d.0).abs() as f32,
    ]
}

fn surface_normals(mesh: &mut MeshData, first: usize, first_index: usize) {
    // Clipping can leave slivers narrower than the GPU positions can represent. Their
    // quantised heights can reverse a face or its slope; they cannot form a visible tread.
    let kept: Vec<_> = mesh.indices[first_index..]
        .as_chunks::<3>()
        .0
        .iter()
        .filter(|face| {
            let [a, b, c] = face.map(|i| mesh.vertices[i as usize]);
            let area = (b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0]);
            let longest = [(a, b), (b, c), (c, a)]
                .into_iter()
                .map(|(p, q)| (p[0] - q[0]).hypot(p[2] - q[2]))
                .fold(0.0_f32, f32::max);
            area > longest * 0.0001
        })
        .flat_map(|face| *face)
        .collect();
    mesh.indices.truncate(first_index);
    mesh.indices.extend(kept);
    // Draped normals describe the terrain; entrance ramps and the curb's bevel
    // need the normal of the final raised surface instead.
    mesh.normals[first..].fill([0.0; 3]);
    for indices in mesh.indices[first_index..].as_chunks::<3>().0 {
        let [first, second, third] = indices.map(|i| mesh.vertices[i as usize]);
        let u = [
            second[0] - first[0],
            second[1] - first[1],
            second[2] - first[2],
        ];
        let v = [
            third[0] - first[0],
            third[1] - first[1],
            third[2] - first[2],
        ];
        let normal = [
            u[2] * v[1] - u[1] * v[2],
            u[0] * v[2] - u[2] * v[0],
            u[1] * v[0] - u[0] * v[1],
        ];
        for &index in indices {
            for (value, add) in mesh.normals[index as usize].iter_mut().zip(normal) {
                *value += add;
            }
        }
    }
    for normal in &mut mesh.normals[first..] {
        let length = normal.iter().map(|v| v * v).sum::<f32>().sqrt();
        if length > 1e-9 {
            for value in normal {
                *value /= length;
            }
        } else {
            // Clipping can leave unused vertices beside a discarded sliver.
            *normal = [0.0, 1.0, 0.0];
        }
    }
}

fn walk_fraction(patch: &Patch, point: Point) -> f64 {
    let from_edge = |a: Point, b: Point| {
        ((b.0 - a.0) * (point.1 - a.1) - (b.1 - a.1) * (point.0 - a.0)).abs()
            / distance(a, b).max(1e-9)
    };
    let start = from_edge(patch.polygon[0], patch.polygon[3]);
    let end = from_edge(patch.polygon[1], patch.polygon[2]);
    start / (start + end).max(1e-9)
}

fn walk_lift(patch: &Patch, ends: [f64; 2], point: Point) -> f64 {
    ends[0] + (ends[1] - ends[0]) * walk_fraction(patch, point)
}

fn walk_base(patch: &Patch, point: Point, heights: &HeightGrid) -> f64 {
    let profile = patch.profile.unwrap_or(Profile {
        a: patch.polygon[0],
        b: patch.polygon[1],
        elevations: [None; 2],
        span: [0.0, 1.0],
    });
    profile_height(profile, walk_fraction(patch, point), heights)
}

fn profile_height(profile: Profile, along: f64, heights: &HeightGrid) -> f64 {
    let [a, b] = [profile.a, profile.b].map(|p| heights.at(p.0, p.1));
    let [a, b] = [
        profile.elevations[0].unwrap_or(a),
        profile.elevations[1].unwrap_or(b),
    ];
    let along = profile.span[0] + (profile.span[1] - profile.span[0]) * along;
    a + (b - a) * along
}

#[allow(clippy::too_many_arguments)] // the existing top plus its exposed boundary
fn walk_sides(
    mesh: &mut MeshData,
    first_index: usize,
    polygon: &[Point],
    patch: &Patch,
    heights: &HeightGrid,
    origin: [f64; 3],
    exposed: &impl Fn(Point, Point) -> Vec<(Point, Point)>,
) {
    let mut sides = Vec::new();
    // Reuse the top's terrain cuts: a long straight side can otherwise bridge a
    // hollow or cut through the top on uneven ground. Chunk cuts get no end wall.
    for triangle in mesh.indices[first_index..].as_chunks::<3>().0 {
        for k in 0..3 {
            let (a, b) = (
                mesh.vertices[triangle[k] as usize],
                mesh.vertices[triangle[(k + 1) % 3] as usize],
            );
            let point = |v: [f32; 3]| (f64::from(v[0]) + origin[0], -f64::from(v[2]) - origin[2]);
            let (pa, pb) = (point(a), point(b));
            if polygon
                .iter()
                .copied()
                .zip(polygon.iter().copied().cycle().skip(1))
                .take(polygon.len())
                .any(|(start, end)| {
                    distance(pa, project(pa, start, end)) < 1e-4
                        && distance(pb, project(pb, start, end)) < 1e-4
                })
                && distance(pa, pb) > 1e-4
            {
                sides.push((
                    pa,
                    pb,
                    [
                        f64::from(a[1]) + origin[1] - heights.at(pa.0, pa.1),
                        f64::from(b[1]) + origin[1] - heights.at(pb.0, pb.1),
                    ],
                ));
            }
        }
    }
    for (a, b, top) in sides {
        let absolute = [top[0] + heights.at(a.0, a.1), top[1] + heights.at(b.0, b.1)];
        for (start, end) in exposed(a, b) {
            let lift = |p: Point| {
                let t = distance(a, p) / distance(a, b);
                absolute[0] + (absolute[1] - absolute[0]) * t - heights.at(p.0, p.1)
            };
            let (a, b, top) = (start, end, [lift(start), lift(end)]);
            let first = mesh.vertices.len();
            vertical_piece(mesh, a, b, -0.02, top, heights, origin);
            let curb = [a, b].into_iter().all(|point| {
                distance(point, project(point, patch.polygon[0], patch.polygon[1])) < 1e-4
            });
            if curb {
                for (i, point) in [a, a, b, b].into_iter().enumerate() {
                    mesh.uvs[first + i][0] = curb_uv(patch, point)[0];
                }
            } else {
                mesh.colors[first..].fill(Material::Concrete.color());
            }
        }
    }
}

fn walking_connection(first: &[Point], second: &[Point]) -> bool {
    [first, second]
        .into_iter()
        .zip([second, first])
        .any(|(a, b)| {
            a.iter()
                .copied()
                .zip(a.iter().copied().cycle().skip(1))
                .take(a.len())
                .any(|(start, end)| {
                    segment_inside(start, end, b)
                        .is_some_and(|(low, high)| (high - low) * distance(start, end) > 0.02)
                })
        })
}

fn segment_inside(a: Point, b: Point, polygon: &[Point]) -> Option<(f64, f64)> {
    let sign = signed_area(polygon).signum();
    let (mut low, mut high) = (0.0_f64, 1.0_f64);
    for (&p, &q) in polygon
        .iter()
        .zip(polygon.iter().cycle().skip(1))
        .take(polygon.len())
    {
        if distance(p, q) < 1e-6 {
            continue;
        }
        // Top edges have already been quantised for the GPU, unlike these footprint masks.
        let side = |v: Point| {
            sign * ((q.0 - p.0) * (v.1 - p.1) - (q.1 - p.1) * (v.0 - p.0)) + distance(p, q) * 0.0001
        };
        let (fa, fb) = (side(a), side(b));
        if fa < 0.0 && fb < 0.0 {
            return None;
        }
        if fa < 0.0 {
            low = low.max(fa / (fa - fb));
        }
        if fb < 0.0 {
            high = high.min(fa / (fa - fb));
        }
        if high <= low {
            return None;
        }
    }
    Some((low, high))
}

fn overlaps(first: &[Point], second: &[Point]) -> bool {
    let edges = |polygon: &[Point]| {
        polygon
            .iter()
            .copied()
            .zip(polygon.iter().copied().cycle().skip(1))
            .take(polygon.len())
            .collect::<Vec<_>>()
    };
    first.iter().any(|&p| contains(second, p))
        || second.iter().any(|&p| contains(first, p))
        || edges(first).into_iter().any(|(a, b)| {
            edges(second).into_iter().any(|(c, d)| {
                let cross = |p: Point, q: Point, r: Point| {
                    (q.0 - p.0) * (r.1 - p.1) - (q.1 - p.1) * (r.0 - p.0)
                };
                cross(a, b, c) * cross(a, b, d) < 0.0 && cross(c, d, a) * cross(c, d, b) < 0.0
            })
        })
}
fn way_ribbon(way: Way, margin: f64) -> Vec<Point> {
    let d = direction(way.a, way.b);
    let edge = |point: Point, half: f64, side: f64| {
        (
            point.0 + d.1 * (half + margin) * side,
            point.1 - d.0 * (half + margin) * side,
        )
    };
    vec![
        edge(way.a, way.half, 1.0),
        edge(way.b, way.end_half, 1.0),
        edge(way.b, way.end_half, -1.0),
        edge(way.a, way.half, -1.0),
    ]
}

fn ribbon(a: Point, b: Point, half: f64) -> Vec<Point> {
    let d = direction(a, b);
    let offset = (d.1 * half, -d.0 * half);
    vec![
        (a.0 - offset.0, a.1 - offset.1),
        (b.0 - offset.0, b.1 - offset.1),
        (b.0 + offset.0, b.1 + offset.1),
        (a.0 + offset.0, a.1 + offset.1),
    ]
}
#[allow(clippy::cast_possible_truncation)] // world metres to GPU data
fn uv(p: Point) -> [f32; 2] {
    [p.0 as f32, -p.1 as f32]
}

#[allow(clippy::too_many_arguments, clippy::cast_possible_truncation)] // draped curb geometry
fn vertical(
    mesh: &mut MeshData,
    a: Point,
    b: Point,
    low: f64,
    high: f64,
    heights: &HeightGrid,
    origin: [f64; 3],
) {
    let high_corner = (heights.origin.0 + CHUNK_SIZE, heights.origin.1 + CHUNK_SIZE);
    for piece in drape::pieces(&[a, b], heights.origin, high_corner) {
        let (a, b) = (piece[0].0, piece[piece.len() - 1].0);
        let middle = midpoint(a, b);
        if distance(a, b) < 1e-4
            || !(heights.origin.0..high_corner.0).contains(&middle.0)
            || !(heights.origin.1..high_corner.1).contains(&middle.1)
        {
            continue;
        }
        vertical_piece(mesh, a, b, low, [high; 2], heights, origin);
    }
}

#[allow(clippy::too_many_arguments, clippy::cast_possible_truncation)] // draped curb geometry
fn vertical_piece(
    mesh: &mut MeshData,
    a: Point,
    b: Point,
    low: f64,
    high: [f64; 2],
    heights: &HeightGrid,
    origin: [f64; 3],
) {
    let d = direction(a, b);
    let n = [-d.1 as f32, 0.0, -d.0 as f32];
    let base = u32::try_from(mesh.vertices.len()).expect("urban mesh fits u32");
    // A street profile can meet the bank partway through an edge. Keep the buried bottom
    // below that profile too, rather than turning the side inside out under the ground.
    for (point, lift, u) in [
        (a, low.min(high[0] - 0.02), 0.0),
        (a, high[0], 0.0),
        (b, high[1], distance(a, b)),
        (b, low.min(high[1] - 0.02), distance(a, b)),
    ] {
        mesh.vertices.push([
            (point.0 - origin[0]) as f32,
            (heights.at(point.0, point.1) + lift - origin[1]) as f32,
            (-point.1 - origin[2]) as f32,
        ]);
        mesh.normals.push(n);
        mesh.uvs.push([u as f32, lift as f32]);
        mesh.colors.push(Material::Curb.color());
    }
    // Godot's clockwise front faces point against the right-hand cross product.
    mesh.indices
        .extend([base, base + 2, base + 1, base, base + 3, base + 2]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(urban: &mut Urban, polygon: Vec<Point>, curb: bool) {
        urban.add_patch(Patch {
            polygon,
            material: Material::Concrete,
            lift: CURB_HEIGHT,
            curb,
            wall: false,
            ramp: None,
            profile: None,
        });
        urban
            .shapes
            .push(vec![urban.patches.last().unwrap().polygon.clone()]);
    }

    #[test]
    fn minimum_length_applies_to_connected_runs_not_individual_slabs_or_chunks() {
        for shift in [0.0, CHUNK_SIZE - 6.0] {
            let mut urban = Urban::default();
            for i in 0..4 {
                let start = shift + f64::from(i) * 3.0;
                walk(
                    &mut urban,
                    ribbon((0.0, start), (0.0, start + 3.0), WALK_WIDTH / 2.0),
                    true,
                );
            }
            // A nearly twelve metre run, a corner-touching pair and a nearby separate run
            // must not borrow length from the valid sidewalk.
            walk(
                &mut urban,
                ribbon((10.0, shift), (10.0, shift + 11.99), WALK_WIDTH / 2.0),
                true,
            );
            walk(
                &mut urban,
                ribbon((20.0, shift), (20.0, shift + 7.0), WALK_WIDTH / 2.0),
                true,
            );
            walk(
                &mut urban,
                ribbon(
                    (20.0 + WALK_WIDTH, shift + 7.0),
                    (20.0 + WALK_WIDTH, shift + 14.0),
                    WALK_WIDTH / 2.0,
                ),
                true,
            );
            // Building accesses and forecourts have their own eligibility rules.
            walk(
                &mut urban,
                ribbon((30.0, shift), (30.0, shift + 2.0), 0.6),
                false,
            );
            urban.retain_walk_runs();
            assert!(urban.shapes[..4].iter().all(|s| !s.is_empty()));
            assert!(urban.shapes[4..7].iter().all(Vec::is_empty));
            assert_eq!(urban.shapes[7].len(), 1, "building access must remain");
        }
    }

    #[test]
    fn clipping_cannot_qualify_a_short_remnant_with_its_missing_source_length() {
        let mut urban = Urban::default();
        walk(
            &mut urban,
            ribbon((0.0, 0.0), (0.0, 100.0), WALK_WIDTH / 2.0),
            true,
        );
        urban.shapes[0] = vec![
            ribbon((0.0, 0.0), (0.0, 4.0), WALK_WIDTH / 2.0),
            ribbon((0.0, 20.0), (0.0, 40.0), WALK_WIDTH / 2.0),
        ];
        urban.retain_walk_runs();
        assert_eq!(urban.shapes[0].len(), 1);
        assert!(urban.shapes[0][0].iter().all(|p| p.1 >= 20.0));
    }

    #[test]
    fn transverse_clipping_partitions_do_not_multiply_walking_length() {
        let mut urban = Urban::default();
        walk(
            &mut urban,
            ribbon((0.0, 0.0), (0.0, 8.0), WALK_WIDTH / 2.0),
            true,
        );
        urban.shapes[0] = vec![
            ribbon(
                (-WALK_WIDTH / 4.0, 0.0),
                (-WALK_WIDTH / 4.0, 8.0),
                WALK_WIDTH / 4.0,
            ),
            ribbon(
                (WALK_WIDTH / 4.0, 0.0),
                (WALK_WIDTH / 4.0, 8.0),
                WALK_WIDTH / 4.0,
            ),
        ];
        urban.retain_walk_runs();
        assert_eq!(urban.shapes[0].len(), 0, "short partitioned run survived");
    }

    #[test]
    fn curved_runs_are_measured_along_the_walk_not_across_the_bend() {
        for (angle, keep) in [(1.1, false), (1.3, true)] {
            let mut urban = Urban::default();
            for i in 0..20 {
                let (a, b) = (f64::from(i) * angle / 20.0, f64::from(i + 1) * angle / 20.0);
                let p = |a: f64, r: f64| (r * a.cos(), r * a.sin());
                walk(
                    &mut urban,
                    vec![p(a, 9.1), p(b, 9.1), p(b, 10.9), p(a, 10.9)],
                    true,
                );
            }
            urban.retain_walk_runs();
            assert!(urban.shapes.iter().all(|s| s.is_empty() != keep));
        }
    }

    #[test]
    fn repeated_cut_vertices_do_not_duplicate_or_hide_paving() {
        let subject = [(0.0, 0.0), (0.0, 4.0), (4.0, 4.0), (4.0, 0.0)];
        let mask = [
            (1.0, 1.0),
            (1.0, 3.0),
            (3.0, 3.0),
            (3.0, 3.0),
            (3.0 + 1e-14, 3.0),
            (3.0, 1.0),
        ];
        let (outside, inside) = split_by_mask(&subject, &mask);
        assert!((outside.iter().map(|p| polygon_area(p)).sum::<f64>() - 12.0).abs() < 1e-8);
        assert!((polygon_area(&inside) - 4.0).abs() < 1e-8);
        let (a, b) = segment_inside((0.0, 2.0), (4.0, 2.0), &mask).unwrap();
        assert!(
            (a - 0.25).abs() < 0.0001 && (b - 0.75).abs() < 0.0001,
            "repeated vertices must not invent a clipping plane"
        );
    }
}
