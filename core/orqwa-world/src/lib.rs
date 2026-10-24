//! 3D world geometry for ORQWA (R16): terrain chunks in a corridor around the route, coloured
//! by land cover, with buildings and trees from OpenStreetMap, rivers, and the road.
//!
//! Coordinates follow Godot: metres with x east, y up and z south (−z is north), relative to
//! the route start for the road and water, and to each chunk's centre for chunk geometry.
//! Triangles wind clockwise seen from their front, Godot's front-face order.

mod appearance;
mod buildings;
mod chains;
mod channels;
mod climate;
pub mod course_map;
mod drape;
mod foundations;
mod horizon;
mod junctions;
mod landcover;
mod minimap;
mod palette;
mod pavement;
mod props;
mod railways;
mod riding;
mod road;
mod roundabouts;
pub mod stored;
mod streets;
mod structures;
mod urban;
mod vegetation;
mod water;
mod water_levels;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};

use orqwa_osm::{LandCover, MapData, RoadClass};
use orqwa_routes::{ElevationModel, LocalProjection, Route, Surface};
use tracing::{info, warn};

pub use horizon::{HORIZON, Horizon, horizon};
use landcover::LandIndex;
pub use minimap::{FlatMap, MapFeature, minimap};
pub use props::{Label, Props};
pub use riding::RoadWidths;
use road::RoadIndex;
pub use vegetation::Trees;

/// What worlds look like. Raise it whenever a change makes the same course come out
/// differently: a world kept with an older one is still ridden, and its course offers to build
/// it again (R101). A change that also makes kept worlds unreadable raises
/// [`stored::FORMAT`] instead or as well.
pub const GENERATOR: u32 = 33;

/// Edge length of a terrain chunk.
pub(crate) const CHUNK_SIZE: f64 = 480.0;
/// Distance between terrain vertices.
const GRID: f64 = 16.0;
/// Terrain (and map data) is used up to this far from the route.
pub const CORRIDOR: f64 = 1500.0;
/// Half the road width.
const ROAD_HALF_WIDTH: f64 = 3.0;
/// Terrain cells near the road are split into this many pieces a side, so the ground can follow
/// the road's verge, cuttings and embankments closely.
const SUB: usize = 6;
/// The size of those pieces.
#[allow(clippy::cast_precision_loss)] // a small constant
const FINE: f64 = GRID / SUB as f64;
/// Ground up to this distance from the road centre is level just below the road: the road and
/// its verge. Wide enough that no triangle of the fine ground touching the road can rise above
/// it (half the road plus the diagonal of a fine piece, and a little).
pub(crate) const VERGE: f64 = ROAD_HALF_WIDTH + FINE * std::f64::consts::SQRT_2 + 0.2;
/// Beyond the verge the ground climbs to the hillside at most this steeply (a cutting)...
const CUT_SLOPE: f64 = 1.2;
/// ...or falls to the valley at most this steeply (an embankment).
const FILL_SLOPE: f64 = 0.6;
/// Cuttings and embankments reach at most this far from the road; over the last
/// `REACH_FADE` metres the shaped ground blends into the natural.
pub(crate) const LEVEL_REACH: f64 = 45.0;
const REACH_FADE: f64 = 12.0;
/// The hill heaped over a tunnel of the road ridden where the terrain has none (#135) lies this
/// far over the tube's arch...
const TUNNEL_HILL_COVER: f64 = 1.0;
/// ...and reaches this far beyond the tube either side, over the ring round its opening, before
/// banking down at `FILL_SLOPE`.
const TUNNEL_HILL_SHOULDER: f64 = 1.0;
/// Level ground sits this far below the road surface, so the two never flicker; the road's edge
/// bevels down to it (road.rs). Other streets lie between the two where they join the road.
pub(crate) const ROAD_SINK: f64 = 0.15;
/// A cell levelled across a street is split into fine pieces only where they would depart from
/// its plain triangles by more than this.
const PLAIN_TOLERANCE: f64 = 0.02;
/// Without terrain data, heights follow the road found within this distance.
const FALLBACK_RADIUS: f64 = 400.0;

/// Triangle geometry ready for a GPU mesh.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MeshData {
    /// Vertex positions.
    pub vertices: Vec<[f32; 3]>,
    /// Unit vertex normals.
    pub normals: Vec<[f32; 3]>,
    /// Texture coordinates; for the road `u` is 0–1 across and `v` the distance in metres.
    pub uvs: Vec<[f32; 2]>,
    /// Vertex colours or material metadata; empty when the mesh has none. Roads encode
    /// `[material / 8, width_metres / 32, marking_style / 4, (lanes + track_flag) / 8]`,
    /// with track flag 4 for a grassy centre (ADR 0031). Other meshes retain their own
    /// colour contracts, including alpha 1 marking water.
    pub colors: Vec<[f32; 4]>,
    /// Three vertex indices per triangle.
    pub indices: Vec<u32>,
}

impl MeshData {
    /// Adds another mesh's triangles. Missing colours default to white when either mesh
    /// carries colours, keeping the optional vertex array complete.
    ///
    /// # Panics
    /// If the combined mesh has more than `u32::MAX` vertices.
    pub fn append(&mut self, mut other: MeshData) {
        let offset = u32::try_from(self.vertices.len()).expect("mesh fits u32");
        if !self.colors.is_empty() || !other.colors.is_empty() {
            self.colors.resize(self.vertices.len(), [1.0; 4]);
            other.colors.resize(other.vertices.len(), [1.0; 4]);
        }
        self.vertices.extend(other.vertices);
        self.normals.extend(other.normals);
        self.uvs.extend(other.uvs);
        self.colors.extend(other.colors);
        self.indices
            .extend(other.indices.into_iter().map(|i| i + offset));
    }
}

/// The ways and water of the map around the route, ready for the chunks.
struct Ways {
    streets: Vec<streets::Street>,
    /// The railways, on their own lines (#85).
    network: railways::Network,
    streams: Vec<water::Stream>,
    pools: Vec<water::Pool>,
    /// The rounded corners where streets meet.
    corners: Vec<junctions::Corner>,
    /// Where plants keep off.
    clearance: streets::Clearance,
    /// Roundabouts' islands.
    islands: Vec<roundabouts::Island>,
    /// Where water cuts the ground.
    channels: channels::Channels,
    water_levels: std::sync::Arc<water_levels::Levels>,
    /// Where paved streets level the ground across them.
    levels: streets::Levels,
}

impl Ways {
    async fn new<M: ElevationModel>(
        map: &MapData,
        projection: &LocalProjection,
        road: &RoadIndex,
        model: &mut M,
    ) -> Self {
        let mut streets = streets::lines(map, projection, model).await;
        streets::connect(&mut streets, road);
        let network = railways::network(map, projection, road, model).await;
        let streams = water::streams(&map.waterways, projection, road);
        let pools = water::pools(&map.areas, projection, road);
        let water_levels =
            water_levels::Levels::new(&streams, &pools, projection, road, model).await;
        let corners = junctions::corners(&streets, road);
        Self {
            clearance: streets::Clearance::new(&streets, &corners, &network.railways, &pools),
            corners,
            islands: roundabouts::islands(map, projection),
            channels: channels::Channels::new(&streams, &pools, &streets),
            water_levels,
            levels: streets::Levels::new(&streets, &appearance::Index::new(map, projection)),
            streets,
            network,
            streams,
            pools,
        }
    }
}

/// A square piece of the world.
#[derive(Debug, Clone, PartialEq)]
pub struct TerrainChunk {
    /// Centre of the chunk at sea level; the chunk's geometry is relative to it.
    pub center: [f32; 3],
    /// Ground, coloured by land cover.
    pub mesh: MeshData,
    /// Buildings standing in the chunk that no model fits, built as shells.
    pub buildings: MeshData,
    /// Buildings drawn as Blender-made models up close, by cell.
    pub modelled: Vec<BuildingCell>,
    /// Paved ways around the route (not the road ridden), on this chunk's ground.
    pub streets: MeshData,
    /// Unpaved ways of the map, likewise.
    pub tracks: MeshData,
    /// Sidewalks, curbs, building access and forecourts, with urban material codes in colours.
    pub urban: MeshData,
    /// Lakes, ponds, rivers and streams of the map, likewise; they pass under streets and the
    /// road.
    pub water: MeshData,
    /// Trees standing in the chunk.
    pub trees: Trees,
    /// Guard rails, posts and poles by the road ridden (R89).
    pub props: Props,
}

/// Buildings drawn as Blender-made models (`app/assets/models/buildings`) up close, in one cell
/// of a chunk; each cell switches to the shells by its own distance.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BuildingCell {
    /// Instances by model name, as Godot `MultiMesh` buffers of 20 floats each: the transform
    /// relative to the chunk centre (row-major 3×4), the plaster colour (sRGB, alpha 1) as
    /// instance colour, and the roof colour (sRGB) with a variant in 0–1 as custom data.
    pub models: BTreeMap<String, Vec<f32>>,
    /// The same buildings as shells, for the distance.
    pub shells: MeshData,
}

/// The generated world.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct World {
    /// Terrain chunks around the route.
    pub chunks: Vec<TerrainChunk>,
    /// The road along the route.
    pub road: MeshData,
    /// Bridges and tunnels, of the road and the railways.
    pub structures: MeshData,
    /// The railways of the map near the route, in route coordinates (`u` across the bed of
    /// ballast, below 0 and above 1 on its shoulders, `v` metres along).
    pub railways: MeshData,
    /// The land beyond the corridor, from [`horizon`] (empty until it is made: it needs a coarse
    /// terrain model of its own).
    pub horizon: Horizon,
    /// Terrain samples that had no elevation data and followed the road instead.
    pub fallback_samples: usize,
}

/// Wearing surfaces are independent of terrain chunks. Structures follow the completed
/// terrain so their foundations can reach the ground actually drawn.
fn whole(shapers: &Shapers<'_>, ways: &Ways) -> World {
    let (road, rails) = (shapers.road, shapers.rails);
    World {
        road: road.mesh(ROAD_HALF_WIDTH, &streets::mouths(&ways.streets, road)),
        railways: rails.mesh(railways::BED_M / 2.0, &[]),
        ..World::default()
    }
}

/// Builds the world for `route`, sampling heights from `model` (e.g. the terrain tiles) and
/// placing `map` features. Where the model has no data, the terrain follows the road.
/// `progress` is called with (chunks done, chunks total).
#[allow(clippy::too_many_lines)] // one pass over the chunks, each step of it named in order
pub async fn generate<M: ElevationModel>(
    route: &Route,
    model: &mut M,
    map: &MapData,
    progress: &mut (dyn FnMut(usize, usize) + Send),
) -> World {
    let projection = LocalProjection::for_route(route);
    let mut road = RoadIndex::new(route, &projection);
    road.set_appearances(&appearance::Index::new(map, &projection));
    let cells = chunks_near_route(&road);
    let total = cells.len();
    // Announce the step before the slower preparation below.
    progress(0, total);
    let land = LandIndex::new(&map.areas, &projection);
    let climate = climate::Climate::at(route.points()[0].lat);
    let lone_lighthouses = buildings::lone_lighthouses(map, &projection);
    let buildings =
        buildings_by_chunk((map, &lone_lighthouses), &projection, &road, &land, climate);
    let mut ways = Ways::new(map, &projection, &road, model).await;
    let portals = structures::open_portals(
        &mut road,
        &mut ways.network.index,
        &ways.levels,
        &projection,
        model,
    )
    .await;
    let hollows = structures::hollows(&road, &ways.network.index, &portals);
    let shapers = Shapers {
        road: &road,
        rails: &ways.network.index,
        streets: &ways.levels,
        portals: &portals,
        hollows: &hollows,
        hills: true,
    };
    let below = structures::Below::new(&road, &ways.network.index, &ways.streets, &ways.islands);
    let mut world = whole(&shapers, &ways);
    let ridden_pavement = pavement::Pavement::new(&world, []);
    let mut urban = urban::Urban::build(
        buildings.values().flatten(),
        &ways.streets,
        &ways.corners,
        &ways.clearance,
        &road,
        &land,
        ridden_pavement,
    );
    // Walking joins need one datum even when their profile anchors lie in another chunk.
    let mut grades = Vec::new();
    for point in urban.grade_points() {
        if let Some(height) = drawn_ground(model, &projection, &shapers, point).await {
            grades.push((point, height));
        }
    }
    urban.pin_grades(grades);
    let mut chunk_barriers = Vec::new();
    let mouths = streets::mouths(&ways.streets, &road);
    let places: Vec<(String, (f64, f64))> = map
        .places
        .iter()
        .map(|p| {
            (
                p.name.clone(),
                projection.project(p.position.0, p.position.1),
            )
        })
        .collect();
    let corridor = Corridor {
        shapers: &shapers,
        land: &land,
        ways: &ways,
        below: &below,
        buildings: &buildings,
        urban: &urban,
        climate,
        mouths: &mouths,
        places: &places,
    };

    // Chunks go in batches: their elevations one after the other, as the model is not shared,
    // then the chunks themselves on all cores, where nearly all the time goes.
    let threads = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
    let cells: Vec<(i32, i32)> = cells.into_iter().collect();
    for batch in cells.chunks(threads * 4) {
        let mut jobs = Vec::with_capacity(batch.len());
        for &(cx, cn) in batch {
            let elevations = HeightGrid::elevations(cx, cn, &projection, model).await;
            jobs.push((cx, cn, elevations));
        }
        let chunks = in_parallel(&jobs, threads, |(cx, cn, elevations)| {
            corridor.chunk(*cx, *cn, elevations)
        });
        for (chunk, fallbacks, barriers) in chunks {
            world.chunks.push(chunk);
            chunk_barriers.push(barriers);
            world.fallback_samples += fallbacks;
        }
        progress(world.chunks.len(), total);
    }
    props::ground_protection(&mut world.chunks);
    let pavement = pavement::Pavement::new(&world, chunk_barriers.iter().map(|(_, n)| *n));
    for (chunk, ([paved, unpaved], _)) in world.chunks.iter_mut().zip(chunk_barriers) {
        pavement.clear(&mut chunk.streets, paved, chunk.center);
        pavement.clear(&mut chunk.tracks, unpaved, chunk.center);
    }
    props::clear_fences(&mut world.chunks, &pavement);
    let mut barriers = Vec::new();
    world.structures = structures::build_all(
        (&road, &ways.network.index),
        &below,
        &shapers,
        &foundations::Ground(&world.chunks),
        &projection,
        model,
        &mut barriers,
    )
    .await;
    pavement.clear(&mut world.structures, barriers, [0.0; 3]);
    if world.fallback_samples > 0 {
        warn!(
            samples = world.fallback_samples,
            "terrain data missing in places; terrain follows the road there"
        );
    }
    info!(
        chunks = world.chunks.len(),
        trees = world.chunks.iter().map(|c| c.trees.len()).sum::<usize>(),
        "world generated"
    );
    world
}

/// Buildings near the route, the map's and `extra` ones (lighthouses standing alone), grouped
/// by the chunk containing their first corner.
fn buildings_by_chunk<'a>(
    (map, extra): (&'a MapData, &'a [orqwa_osm::Building]),
    projection: &LocalProjection,
    road: &RoadIndex,
    land: &LandIndex,
    climate: climate::Climate,
) -> HashMap<(i32, i32), Vec<buildings::Plot<'a>>> {
    let project = |points: &[(f64, f64)]| -> Vec<(f64, f64)> {
        points
            .iter()
            .map(|&(lat, lon)| projection.project(lat, lon))
            .collect()
    };
    let streets = map
        .roads
        .iter()
        .filter(|r| {
            r.structure.is_none()
                && matches!(
                    r.class,
                    RoadClass::Major | RoadClass::Street | RoadClass::Service
                )
        })
        .flat_map(|r| drape::densify(&project(&r.line), 3.0))
        .chain(road.samples(3.0));
    let frontage = buildings::Frontage::new(streets);
    let mut plots = Vec::new();
    for building in map.buildings.iter().chain(extra) {
        let footprint = buildings::footprint(building, projection);
        let Some(&(east, north)) = footprint.first() else {
            continue;
        };
        // Roads stay clear (#100): buildings mapped across them (e.g. bad data) are left out,
        // those reaching into the road ridden by a corner or a wall as drawn (#138), and those
        // a street runs through.
        let mut outline = buildings::drawn_outline(&footprint);
        outline.push(outline[0]);
        let on_road = drape::densify(&outline, 2.0)
            .iter()
            .any(|&(e, n)| road.edge_distance(e, n) < 1.0);
        if on_road
            || frontage.runs_through(&footprint)
            || road.nearest(east, north, CORRIDOR).is_none()
        {
            continue;
        }
        let (e, n) = buildings::centroid(&footprint);
        let setting = if land.has(e, n, LandCover::Industrial) {
            buildings::Setting::Industrial
        } else if land.has(e, n, LandCover::Commercial) {
            buildings::Setting::Commercial
        } else if land.has(e, n, LandCover::Public) {
            buildings::Setting::Public
        } else if land.has(e, n, LandCover::Residential) {
            buildings::Setting::Town
        } else {
            buildings::Setting::Countryside
        };
        plots.push(buildings::Plot {
            building,
            footprint,
            setting,
            church: false,
            shop: None,
            purpose: None,
            landmark: None,
            climate,
        });
    }
    buildings::mark_churches(&mut plots, &project(&map.churches));
    for (points, landmark) in [
        (&map.castles, buildings::Landmark::Castle),
        (&map.lighthouses, buildings::Landmark::Lighthouse),
    ] {
        buildings::mark_landmarks(&mut plots, &project(points), landmark);
    }
    for (points, purpose) in [
        (&map.offices, buildings::Purpose::Office),
        (&map.hotels, buildings::Purpose::Hotel),
        (&map.public, buildings::Purpose::Public),
    ] {
        buildings::mark_purpose(&mut plots, &project(points), purpose);
    }
    buildings::mark_shops(&mut plots, &project(&map.shops), &frontage);

    let mut by_chunk: HashMap<_, Vec<_>> = HashMap::new();
    for plot in plots {
        let (east, north) = plot.footprint[0];
        by_chunk
            .entry(chunk_of(east, north))
            .or_default()
            .push(plot);
    }
    by_chunk
}

/// What every chunk is built from, shared by the chunks built at the same time.
struct Corridor<'a> {
    shapers: &'a Shapers<'a>,
    land: &'a LandIndex,
    ways: &'a Ways,
    below: &'a structures::Below<'a>,
    buildings: &'a HashMap<(i32, i32), Vec<buildings::Plot<'a>>>,
    urban: &'a urban::Urban,
    climate: climate::Climate,
    /// Where streets join the road ridden.
    mouths: &'a [road::Mouth],
    /// Places' names and where they are, for the signs.
    places: &'a [(String, (f64, f64))],
}

impl Corridor<'_> {
    /// Chunk (`cx`, `cn`) from its [`HeightGrid::elevations`], and how many of its terrain
    /// samples had no elevation data.
    fn chunk(
        &self,
        cx: i32,
        cn: i32,
        elevations: &[Option<f64>],
    ) -> (TerrainChunk, usize, ([Vec<pavement::Barrier>; 2], usize)) {
        let Ways {
            streets,
            streams,
            pools,
            ..
        } = self.ways;
        let (heights, fallbacks) = HeightGrid::build(
            cx,
            cn,
            elevations,
            self.shapers,
            (&self.ways.channels, self.land, &self.ways.water_levels),
        );
        let origin = [
            heights.origin.0 + CHUNK_SIZE / 2.0,
            0.0,
            -(heights.origin.1 + CHUNK_SIZE / 2.0),
        ];
        let mut chunk_buildings = buildings::ChunkBuildings::default();
        for plot in self.buildings.get(&(cx, cn)).into_iter().flatten() {
            buildings::add(&mut chunk_buildings, plot, &heights, origin);
        }
        #[allow(clippy::cast_possible_truncation)] // geometry is stored as f32 for the GPU
        let center = [origin[0] as f32, 0.0, origin[2] as f32];
        // Buildings of this chunk and its neighbours: plants near a border keep out of those
        // across it too.
        let footprints: Vec<vegetation::Footprint> = (-1..=1)
            .flat_map(|de| (-1..=1).map(move |dn| (cx + de, cn + dn)))
            .filter_map(|key| self.buildings.get(&key))
            .flatten()
            .map(|plot| vegetation::Footprint::around(&plot.footprint))
            .collect();
        let ground = vegetation::Ground {
            heights: &heights,
            land: self.land,
            road: self.shapers.road,
            streets: &self.ways.clearance,
            buildings: &footprints,
            climate: self.climate,
            urban: self.urban,
        };
        let mut trees = vegetation::place(heights.origin, CHUNK_SIZE, &ground, origin);
        let roadside = props::Roadside {
            heights: &heights,
            land: self.land,
            road: self.shapers.road,
            mouths: self.mouths,
            streets: &self.ways.clearance,
            buildings: &footprints,
            places: self.places,
            urban: self.urban,
        };
        let props = props::place(heights.origin, CHUNK_SIZE, &roadside, origin);
        vegetation::place_grass(&mut trees, heights.origin, CHUNK_SIZE, &ground, origin);
        let (paved, mut unpaved, barriers) = streets::meshes(
            (streets, &self.ways.corners),
            heights.origin,
            CHUNK_SIZE,
            &heights,
            self.below,
            origin,
        );
        let track_length = unpaved.indices.len();
        unpaved.append(water::shore_mesh(
            pools,
            heights.origin,
            CHUNK_SIZE,
            &heights,
            origin,
        ));
        let mut mesh = heights.mesh(self.land, origin, self.shapers, &self.ways.islands);
        self.urban.paint(&mut mesh, origin, self.land);
        let chunk = TerrainChunk {
            center,
            mesh,
            buildings: chunk_buildings.shells,
            modelled: chunk_buildings.cells.into_values().collect(),
            streets: paved,
            tracks: unpaved,
            urban: self.urban.mesh(&heights, origin),
            water: water::mesh(streams, pools, heights.origin, CHUNK_SIZE, &heights, origin),
            trees,
            props,
        };
        (chunk, fallbacks, (barriers, track_length))
    }
}

/// `work` done for each of `jobs` on up to `threads` threads, the results in the order of
/// `jobs`. A panic in `work` is passed on.
fn in_parallel<J: Sync, R: Send>(
    jobs: &[J],
    threads: usize,
    work: impl Fn(&J) -> R + Sync,
) -> Vec<R> {
    // Each thread takes the next job when done with one: chunks differ a lot in work (a town
    // against a field).
    let next = AtomicUsize::new(0);
    let mut results: Vec<Option<R>> = std::iter::repeat_with(|| None).take(jobs.len()).collect();
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads.clamp(1, jobs.len().max(1)))
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(job) = jobs.get(index) else {
                            return done;
                        };
                        done.push((index, work(job)));
                    }
                })
            })
            .collect();
        for worker in workers {
            for (index, result) in worker
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            {
                results[index] = Some(result);
            }
        }
    });
    results.into_iter().flatten().collect()
}

fn chunk_of(east: f64, north: f64) -> (i32, i32) {
    #[allow(clippy::cast_possible_truncation)] // world coordinates are far below 2^31 chunks
    (
        (east / CHUNK_SIZE).floor() as i32,
        (north / CHUNK_SIZE).floor() as i32,
    )
}

/// Chunk grid cells (east, north) within the corridor of any part of the route.
pub(crate) fn chunks_near_route(road: &RoadIndex) -> BTreeSet<(i32, i32)> {
    let mut cells = BTreeSet::new();
    let reach = CORRIDOR + CHUNK_SIZE / 2.0 * std::f64::consts::SQRT_2;
    // Sampling the road every half chunk is enough to touch every chunk in reach.
    for (east, north) in road.samples(CHUNK_SIZE / 2.0) {
        let (low, high) = (
            chunk_of(east - reach, north - reach),
            chunk_of(east + reach, north + reach),
        );
        for cx in low.0..=high.0 {
            for cn in low.1..=high.1 {
                let center_e = (f64::from(cx) + 0.5) * CHUNK_SIZE;
                let center_n = (f64::from(cn) + 0.5) * CHUNK_SIZE;
                if (center_e - east).hypot(center_n - north) <= reach {
                    cells.insert((cx, cn));
                }
            }
        }
    }
    cells
}

/// Terrain heights of one chunk: a vertex grid with a one-vertex border, and finer cells near
/// the road where the ground is shaped around it.
pub(crate) struct HeightGrid {
    /// South-west corner in metres east/north.
    origin: (f64, f64),
    /// Vertices per side, without the border.
    side: usize,
    /// Shaped heights of the grid's vertices, border included.
    heights: Vec<f64>,
    /// Natural heights of the grid's vertices, border included.
    natural: Vec<f64>,
    /// Cells split into `SUB` × `SUB` pieces: the shaped heights of their vertices, row by row
    /// from the south-west.
    fine: HashMap<(usize, usize), Vec<f64>>,
    /// Local downward correction beneath ground crossings (culverts), coarse and fine.
    water_drop: Vec<f64>,
    fine_water_drop: HashMap<(usize, usize), Vec<f64>>,
    water_levels: std::sync::Arc<water_levels::Levels>,
    /// Openings also cut paving laid on the hill, including approach caps and junctions.
    hollows: Vec<structures::TunnelCut>,
}

/// What a strip or area is laid on (`drape`): the ground, or the water's surface in its channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum On {
    Ground,
    Water,
}

impl HeightGrid {
    /// Vertices per side of a chunk's grid, without the border.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    const SIDE: usize = (CHUNK_SIZE / GRID) as usize + 1;

    /// Positions (east, north) of the vertices of chunk (`cx`, `cn`), border included, row by
    /// row from the south-west.
    fn vertices(cx: i32, cn: i32) -> impl Iterator<Item = (f64, f64)> {
        let bordered = Self::SIDE + 2;
        let origin = (f64::from(cx) * CHUNK_SIZE, f64::from(cn) * CHUNK_SIZE);
        (0..bordered).flat_map(move |j| {
            (0..bordered).map(move |i| {
                #[allow(clippy::cast_precision_loss)] // small grid indices
                (
                    origin.0 + (i as f64 - 1.0) * GRID,
                    origin.1 + (j as f64 - 1.0) * GRID,
                )
            })
        })
    }

    /// The model's elevations at the vertices of chunk (`cx`, `cn`), `None` where it has none.
    /// Kept apart from [`HeightGrid::build`]: the model is the one part chunks cannot share.
    async fn elevations<M: ElevationModel>(
        cx: i32,
        cn: i32,
        projection: &LocalProjection,
        model: &mut M,
    ) -> Vec<Option<f64>> {
        let mut elevations = Vec::with_capacity((Self::SIDE + 2).pow(2));
        for (east, north) in Self::vertices(cx, cn) {
            let (lat, lon) = projection.unproject(east, north);
            elevations.push(model.elevation(lat, lon).await.ok());
        }
        elevations
    }

    /// The grid of chunk (`cx`, `cn`) from its [`HeightGrid::elevations`], and how many
    /// vertices had none and follow the road instead.
    fn build(
        cx: i32,
        cn: i32,
        elevations: &[Option<f64>],
        shapers: &Shapers<'_>,
        water: (
            &channels::Channels,
            &LandIndex,
            &std::sync::Arc<water_levels::Levels>,
        ),
    ) -> (Self, usize) {
        let side = Self::SIDE;
        let bordered = side + 2;
        let origin = (f64::from(cx) * CHUNK_SIZE, f64::from(cn) * CHUNK_SIZE);
        let chunk_road_elevation = shapers
            .road
            .nearest(
                origin.0 + CHUNK_SIZE / 2.0,
                origin.1 + CHUNK_SIZE / 2.0,
                f64::INFINITY,
            )
            .map_or(0.0, |(_, elevation, _)| elevation);

        let mut natural = vec![0.0; bordered * bordered];
        let mut fallbacks = 0;
        for (k, ((east, north), elevation)) in Self::vertices(cx, cn).zip(elevations).enumerate() {
            let height = elevation.unwrap_or_else(|| {
                fallbacks += 1;
                shapers
                    .road
                    .nearest(east, north, FALLBACK_RADIUS)
                    .map_or(chunk_road_elevation, |(_, elevation, _)| elevation)
            });
            natural[k] = height;
        }
        let mut grid = Self {
            origin,
            side,
            heights: vec![0.0; bordered * bordered],
            natural,
            fine: HashMap::new(),
            water_drop: vec![0.0; bordered * bordered],
            fine_water_drop: HashMap::new(),
            water_levels: std::sync::Arc::clone(water.2),
            hollows: shapers
                .hollows
                .iter()
                .copied()
                .filter(|p| {
                    p.near(
                        origin.0 + CHUNK_SIZE / 2.0,
                        origin.1 + CHUNK_SIZE / 2.0,
                        CHUNK_SIZE,
                    )
                })
                .collect(),
        };
        // Streets level the ground across them from the natural ground at their centre lines:
        // all of it is known first.
        for (k, (east, north)) in Self::vertices(cx, cn).enumerate() {
            let shaped = grid.shaped_at(east, north, shapers);
            grid.heights[k] = shaped - carve_at(water, east, north, shaped, shapers);
            grid.water_drop[k] = crossing_drop(water, east, north, shaped, shapers);
        }
        let half_diagonal = GRID * std::f64::consts::FRAC_1_SQRT_2;
        for j in 0..side - 1 {
            for i in 0..side - 1 {
                let (east, north) = grid.position(i, j, 0.5, 0.5);
                let detail = if grid.needs_detail(i, j, shapers)
                    || water.0.near(east, north, half_diagonal)
                {
                    Some(grid.detail(i, j, shapers, water))
                } else if shapers
                    .streets
                    .nearest(east, north, half_diagonal)
                    .is_some()
                {
                    // Levelled across a street: split only where that changes the cell's
                    // ground, so cells on flat land stay plain. On the chunk's edge always, as
                    // the neighbouring chunk's cell along it may be split.
                    let edge = i == 0 || j == 0 || i == side - 2 || j == side - 2;
                    Some(grid.detail(i, j, shapers, water))
                        .filter(|(fine, _)| edge || grid.departs(i, j, fine))
                } else {
                    None
                };
                if let Some((fine, drops)) = detail {
                    grid.fine.insert((i, j), fine);
                    grid.fine_water_drop.insert((i, j), drops);
                }
            }
        }
        grid.close_edges();
        (grid, fallbacks)
    }

    /// Where a split cell meets a plain one in the chunk, its edge takes the plain cell's
    /// straight edge, so no gap opens between them: a plain cell beside a street departs a
    /// little from the levelled ground (`PLAIN_TOLERANCE`).
    fn close_edges(&mut self) {
        #[allow(clippy::cast_possible_wrap)] // small grid
        let cells = (self.side - 1) as isize;
        let split: Vec<(usize, usize)> = self.fine.keys().copied().collect();
        for (i, j) in split {
            #[allow(clippy::cast_possible_wrap)] // small grid indices
            let (ci, cj) = (i as isize, j as isize);
            let plain = |di: isize, dj: isize| {
                let (ni, nj) = (ci + di, cj + dj);
                (0..cells).contains(&ni)
                    && (0..cells).contains(&nj)
                    && usize::try_from(ni)
                        .ok()
                        .zip(usize::try_from(nj).ok())
                        .is_some_and(|cell| !self.fine.contains_key(&cell))
            };
            let (west, east, south, north) = (plain(-1, 0), plain(1, 0), plain(0, -1), plain(0, 1));
            let (sw, se, nw, ne) = (
                self.vertex(ci, cj),
                self.vertex(ci + 1, cj),
                self.vertex(ci, cj + 1),
                self.vertex(ci + 1, cj + 1),
            );
            let Some(fine) = self.fine.get_mut(&(i, j)) else {
                continue;
            };
            for k in 0..=SUB {
                #[allow(clippy::cast_precision_loss)] // a few pieces
                let share = k as f64 / SUB as f64;
                if south {
                    fine[k] = sw + (se - sw) * share;
                }
                if north {
                    fine[SUB * (SUB + 1) + k] = nw + (ne - nw) * share;
                }
                if west {
                    fine[k * (SUB + 1)] = sw + (nw - sw) * share;
                }
                if east {
                    fine[k * (SUB + 1) + SUB] = se + (ne - se) * share;
                }
            }
        }
    }

    /// The ground at a point before channels cut it: natural, levelled across the paved
    /// street nearest to it (#116), then shaped around the road ridden and the railways, which
    /// come first.
    fn shaped_at(&self, east: f64, north: f64, shapers: &Shapers<'_>) -> f64 {
        let natural = self.natural_at(east, north);
        if shapers.streets.is_empty() {
            return shape(natural, &shapers.near(east, north, LEVEL_REACH));
        }
        let levelled = shapers.streets.nearest(east, north, 0.0).map_or(
            natural,
            |(distance, half, (foot_east, foot_north))| {
                level_across(
                    natural,
                    self.natural_at(foot_east, foot_north),
                    distance - half,
                )
            },
        );
        shape(levelled, &shapers.near(east, north, LEVEL_REACH))
    }

    /// Whether the fine heights of cell (`i`, `j`) depart from its plain triangles by more than
    /// `PLAIN_TOLERANCE`.
    fn departs(&self, i: usize, j: usize, fine: &[f64]) -> bool {
        #[allow(clippy::cast_possible_wrap)] // small grid indices
        let (i, j) = (i as isize, j as isize);
        let (sw, se, nw, ne) = (
            self.vertex(i, j),
            self.vertex(i + 1, j),
            self.vertex(i, j + 1),
            self.vertex(i + 1, j + 1),
        );
        (0..=SUB).any(|b| {
            (0..=SUB).any(|a| {
                #[allow(clippy::cast_precision_loss)] // small counts
                let (u, v) = (a as f64 / SUB as f64, b as f64 / SUB as f64);
                let plain = on_triangles(sw, se, nw, ne, u, v);
                (fine[b * (SUB + 1) + a] - plain).abs() > PLAIN_TOLERANCE
            })
        })
    }

    /// Whether the ground anywhere in cell (`i`, `j`) is shaped around the road. Elsewhere it
    /// is natural, and the plain cell's flat triangles match the neighbours' edges exactly.
    fn needs_detail(&self, i: usize, j: usize, shapers: &Shapers<'_>) -> bool {
        let half_diagonal = GRID * std::f64::consts::FRAC_1_SQRT_2;
        let (east, north) = self.position(i, j, 0.5, 0.5);
        // At a tunnel portal the cutting ends: the ground steps up to the hill there.
        if shapers
            .portals
            .iter()
            .any(|p| p.near(east, north, half_diagonal + FINE))
        {
            return true;
        }
        let roads = shapers.near(east, north, LEVEL_REACH + half_diagonal);
        if roads.is_empty() {
            return false;
        }
        let nearest = roads.iter().map(|r| r.0).fold(f64::INFINITY, f64::min);
        let closest = (nearest - half_diagonal).max(0.0);
        if closest <= VERGE {
            return true;
        }
        // Natural ground within every road's cutting and embankment slopes is left as it is.
        let (low_road, high_road) = roads
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |a, r| {
                (a.0.min(r.1), a.1.max(r.1))
            });
        let corners = [(0, 0), (1, 0), (0, 1), (1, 1)].map(|(di, dj)| {
            #[allow(clippy::cast_possible_wrap)] // small grid indices
            self.natural_vertex(i as isize + di, j as isize + dj)
        });
        let (low, high) = corners
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |a, &h| {
                (a.0.min(h), a.1.max(h))
            });
        let room = closest - VERGE;
        high > low_road - ROAD_SINK + room * CUT_SLOPE
            || low < high_road - ROAD_SINK - room * FILL_SLOPE
    }

    /// The shaped bed and culvert corrections at a cell's fine vertices.
    fn detail(
        &self,
        i: usize,
        j: usize,
        shapers: &Shapers<'_>,
        water: (
            &channels::Channels,
            &LandIndex,
            &std::sync::Arc<water_levels::Levels>,
        ),
    ) -> (Vec<f64>, Vec<f64>) {
        let mut fine = Vec::with_capacity((SUB + 1) * (SUB + 1));
        let mut drops = Vec::with_capacity((SUB + 1) * (SUB + 1));
        for b in 0..=SUB {
            for a in 0..=SUB {
                #[allow(clippy::cast_precision_loss)] // small counts
                let (u, v) = (a as f64 / SUB as f64, b as f64 / SUB as f64);
                let (east, north) = self.position(i, j, u, v);
                let shaped = self.shaped_at(east, north, shapers);
                let cut = carve_at(water, east, north, shaped, shapers);
                fine.push(shaped - cut);
                drops.push(crossing_drop(water, east, north, shaped, shapers));
            }
        }
        (fine, drops)
    }

    /// Metres east/north of the point (`u`, `v`) (0–1) of cell (`i`, `j`).
    fn position(&self, i: usize, j: usize, u: f64, v: f64) -> (f64, f64) {
        #[allow(clippy::cast_precision_loss)] // small grid indices
        (
            self.origin.0 + (i as f64 + u) * GRID,
            self.origin.1 + (j as f64 + v) * GRID,
        )
    }

    /// Shaped height at a grid vertex; `i`/`j` may be −1 or `side` (the border).
    /// The ground's triangles (corners in metres east/north) overlapping the rectangle
    /// `low`–`high`, as the mesh draws them: cells, or their fine pieces near the road, split
    /// along the south-west to north-east diagonal, clockwise seen from above.
    pub(crate) fn triangles(&self, low: (f64, f64), high: (f64, f64)) -> Vec<[(f64, f64); 3]> {
        // Cells (or pieces) of `size` from `start` that `from`–`to` touches, of `count`.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped, small grid
        let touched = |from: f64, to: f64, start: f64, size: f64, count: usize| {
            let first = (((from - start) / size).floor().max(0.0) as usize).min(count - 1);
            let last = (((to - start) / size).floor().max(0.0) as usize).min(count - 1);
            first..=last
        };
        let cells = self.side - 1;
        let mut triangles = Vec::new();
        for j in touched(low.1, high.1, self.origin.1, GRID, cells) {
            for i in touched(low.0, high.0, self.origin.0, GRID, cells) {
                #[allow(clippy::cast_precision_loss)] // small grid
                let corner = (
                    self.origin.0 + i as f64 * GRID,
                    self.origin.1 + j as f64 * GRID,
                );
                let (size, count) = if self.fine.contains_key(&(i, j)) {
                    (FINE, SUB)
                } else {
                    (GRID, 1)
                };
                for row in touched(low.1, high.1, corner.1, size, count) {
                    for column in touched(low.0, high.0, corner.0, size, count) {
                        #[allow(clippy::cast_precision_loss)] // a few pieces
                        let sw = (
                            corner.0 + column as f64 * size,
                            corner.1 + row as f64 * size,
                        );
                        let (se, nw, ne) = (
                            (sw.0 + size, sw.1),
                            (sw.0, sw.1 + size),
                            (sw.0 + size, sw.1 + size),
                        );
                        triangles.push([sw, nw, ne]);
                        triangles.push([sw, ne, se]);
                    }
                }
            }
        }
        triangles
    }

    /// Draped ground surfaces share the terrain's tunnel openings. Cutting only the terrain
    /// leaves the approach's paving stretched across the mouth on the uncut height grid.
    pub(crate) fn cut_paving(&self, triangle: [[f64; 3]; 3]) -> Option<Vec<[[f64; 3]; 3]>> {
        if self.hollows.is_empty() {
            return None;
        }
        cut_out(&self.hollows.iter().collect::<Vec<_>>(), triangle)
    }

    /// Foundation height for a mapped bridge built in this chunk, over the whole footprint.
    pub(crate) fn foundation(
        &self,
        centre: (f64, f64),
        forward: (f64, f64),
        dimensions: (f64, f64),
    ) -> Option<f64> {
        let reach = dimensions.0.hypot(dimensions.1);
        let triangles = self
            .triangles(
                (centre.0 - reach, centre.1 - reach),
                (centre.0 + reach, centre.1 + reach),
            )
            .into_iter()
            .flat_map(|t| {
                let corners = t.map(|(e, n)| [e, n, self.at(e, n)]);
                self.cut_paving(corners).unwrap_or_else(|| vec![corners])
            });
        foundations::lowest(triangles, centre, forward, dimensions)
    }

    fn vertex(&self, i: isize, j: isize) -> f64 {
        self.heights[self.slot(i, j)]
    }

    fn natural_vertex(&self, i: isize, j: isize) -> f64 {
        self.natural[self.slot(i, j)]
    }

    fn slot(&self, i: isize, j: isize) -> usize {
        let bordered = self.side + 2;
        let clamp = |k: isize| usize::try_from(k + 1).unwrap_or(0).min(bordered - 1);
        clamp(j) * bordered + clamp(i)
    }

    /// Natural height anywhere in or around the chunk, between the grid's samples.
    fn natural_at(&self, east: f64, north: f64) -> f64 {
        let u = (east - self.origin.0) / GRID;
        let v = (north - self.origin.1) / GRID;
        let (i, j) = (u.floor(), v.floor());
        let (fu, fv) = (u - i, v - j);
        #[allow(clippy::cast_possible_truncation)] // clamped to the small grid by `slot`
        let (i, j) = (i as isize, j as isize);
        let bottom = self.natural_vertex(i, j) * (1.0 - fu) + self.natural_vertex(i + 1, j) * fu;
        let top =
            self.natural_vertex(i, j + 1) * (1.0 - fu) + self.natural_vertex(i + 1, j + 1) * fu;
        bottom * (1.0 - fv) + top * fv
    }

    /// The ground's height at any point of (or slightly around) the chunk, exactly as the mesh
    /// has it.
    /// The ground's height at a point, channels cut into it.
    pub(crate) fn at(&self, east: f64, north: f64) -> f64 {
        self.interpolate(east, north, &self.heights, &self.fine)
    }

    /// The height of what lies `on` the ground at a point: the ground, or the water's surface
    /// in its channel (the shared water datum, less the channel's drop).
    pub(crate) fn level(&self, on: On, east: f64, north: f64) -> f64 {
        match on {
            On::Ground => self.at(east, north),
            On::Water => {
                self.water_levels
                    .at(east, north)
                    .unwrap_or_else(|| self.natural_at(east, north))
                    + self.interpolate(east, north, &self.water_drop, &self.fine_water_drop)
                    - channels::DROP
            }
        }
    }

    /// A value given at the grid's vertices (`coarse`, and `fine` in detailed cells) at a point,
    /// on the triangles the mesh draws.
    fn interpolate(
        &self,
        east: f64,
        north: f64,
        coarse: &[f64],
        fine: &HashMap<(usize, usize), Vec<f64>>,
    ) -> f64 {
        let u = (east - self.origin.0) / GRID;
        let v = (north - self.origin.1) / GRID;
        // Points on the chunk's far edges belong to its last cells, not to the border.
        #[allow(clippy::cast_precision_loss)] // small grid
        let last = (self.side - 2) as f64;
        let cell = |w: f64| {
            let floor = w.floor();
            if (last + 1.0..=last + 1.0 + 1e-9).contains(&w) {
                last
            } else {
                floor
            }
        };
        let (i, j) = (cell(u), cell(v));
        let (fu, fv) = (u - i, v - j);
        #[allow(clippy::cast_possible_truncation)] // clamped to the small grid by `slot`
        let (i, j) = (i as isize, j as isize);
        if let (Ok(ci), Ok(cj)) = (usize::try_from(i), usize::try_from(j))
            && let Some(fine) = fine.get(&(ci, cj))
        {
            #[allow(clippy::cast_precision_loss)]
            let (su, sv) = (fu * SUB as f64, fv * SUB as f64);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let (column, row) = (
                (su.floor() as usize).min(SUB - 1),
                (sv.floor() as usize).min(SUB - 1),
            );
            #[allow(clippy::cast_precision_loss)]
            let (pu, pv) = (su - column as f64, sv - row as f64);
            let piece = |column: usize, row: usize| fine[row * (SUB + 1) + column];
            return on_triangles(
                piece(column, row),
                piece(column + 1, row),
                piece(column, row + 1),
                piece(column + 1, row + 1),
                pu,
                pv,
            );
        }
        let vertex = |i: isize, j: isize| coarse[self.slot(i, j)];
        on_triangles(
            vertex(i, j),
            vertex(i + 1, j),
            vertex(i, j + 1),
            vertex(i + 1, j + 1),
            fu,
            fv,
        )
    }

    /// Terrain normals include channel carving near water.
    fn normal_at(&self, east: f64, north: f64, shapers: &Shapers<'_>) -> [f32; 3] {
        // Channels change the bank's slope: lighting and triplanar rock must follow the
        // carved mesh, rather than treating a newly exposed face as level ground.
        let channel = self.water_levels.at(east, north).is_some();
        let border = (east - self.origin.0).abs() < 1e-6
            || (east - self.origin.0 - CHUNK_SIZE).abs() < 1e-6
            || (north - self.origin.1).abs() < 1e-6
            || (north - self.origin.1 - CHUNK_SIZE).abs() < 1e-6;
        // Both chunks know this same coarse border, while only one knows each fine cell.
        let plain = HashMap::new();
        let height = |e: f64, n: f64| {
            if channel && border {
                self.interpolate(e, n, &self.heights, &plain)
            } else if channel {
                self.at(e, n)
            } else {
                self.shaped_at(e, n, shapers)
            }
        };
        let step = 1.0;
        let slope_east = (height(east + step, north) - height(east - step, north)) / (2.0 * step);
        let slope_north = (height(east, north + step) - height(east, north - step)) / (2.0 * step);
        unit([-slope_east, 1.0, slope_north])
    }

    /// The ground mesh relative to `origin`, coloured by land cover, with the roundabouts'
    /// `islands` raised on it.
    fn mesh(
        &self,
        land: &LandIndex,
        origin: [f64; 3],
        shapers: &Shapers<'_>,
        islands: &[roundabouts::Island],
    ) -> MeshData {
        let mut mesh = self.plain_mesh(land, origin, shapers);
        mesh.append(roundabouts::mesh(
            islands,
            self.origin,
            CHUNK_SIZE,
            self,
            origin,
        ));
        mesh
    }

    /// The ground mesh relative to `origin`, coloured by land cover.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)] // f32 GPU data; small grid
    fn plain_mesh(&self, land: &LandIndex, origin: [f64; 3], shapers: &Shapers<'_>) -> MeshData {
        let side = self.side;
        let mut mesh = MeshData::default();
        let push = |mesh: &mut MeshData, east: f64, north: f64, height: f64| {
            mesh.vertices.push([
                (east - origin[0]) as f32,
                (height - origin[1]) as f32,
                (-north - origin[2]) as f32,
            ]);
            mesh.normals.push(self.normal_at(east, north, shapers));
            mesh.uvs.push([(east / GRID) as f32, (north / GRID) as f32]);
            mesh.colors
                .push(landcover::color(land.cover_at(east, north)));
            u32::try_from(mesh.vertices.len() - 1).expect("chunk fits u32")
        };
        // The plain grid's vertices, shared by its cells.
        let mut grid = Vec::with_capacity(side * side);
        for j in 0..side {
            for i in 0..side {
                let (east, north) = self.position(i, j, 0.0, 0.0);
                grid.push(push(
                    &mut mesh,
                    east,
                    north,
                    self.vertex(i as isize, j as isize),
                ));
            }
        }
        for j in 0..side - 1 {
            for i in 0..side - 1 {
                let (east, north) = self.position(i, j, 0.5, 0.5);
                let hollows: Vec<&structures::TunnelCut> = self
                    .hollows
                    .iter()
                    .filter(|p| p.near(east, north, GRID))
                    .collect();
                // A piece's two triangles from its corners (index, [east, north, height]), cut
                // to a tunnel's opening where they reach into one.
                let add = |mesh: &mut MeshData, corners: [(u32, [f64; 3]); 4]| {
                    let [sw, se, nw, ne] = corners;
                    for triangle in [[sw, nw, ne], [sw, ne, se]] {
                        match cut_out(&hollows, triangle.map(|c| c.1)) {
                            None => mesh.indices.extend(triangle.map(|c| c.0)),
                            Some(pieces) => {
                                for piece in pieces {
                                    for [east, north, height] in piece {
                                        let index = push(mesh, east, north, height);
                                        mesh.indices.push(index);
                                    }
                                }
                            }
                        }
                    }
                };
                if let Some(fine) = self.fine.get(&(i, j)) {
                    let mut corners = Vec::with_capacity((SUB + 1) * (SUB + 1));
                    for b in 0..=SUB {
                        for a in 0..=SUB {
                            #[allow(clippy::cast_precision_loss)]
                            let (east, north) =
                                self.position(i, j, a as f64 / SUB as f64, b as f64 / SUB as f64);
                            let height = fine[b * (SUB + 1) + a];
                            corners.push((
                                push(&mut mesh, east, north, height),
                                [east, north, height],
                            ));
                        }
                    }
                    for b in 0..SUB {
                        for a in 0..SUB {
                            let at = |a: usize, b: usize| corners[b * (SUB + 1) + a];
                            add(
                                &mut mesh,
                                [at(a, b), at(a + 1, b), at(a, b + 1), at(a + 1, b + 1)],
                            );
                        }
                    }
                } else {
                    let at = |i: usize, j: usize| {
                        let (east, north) = self.position(i, j, 0.0, 0.0);
                        (
                            grid[j * side + i],
                            [east, north, self.vertex(i as isize, j as isize)],
                        )
                    };
                    add(
                        &mut mesh,
                        [at(i, j), at(i + 1, j), at(i, j + 1), at(i + 1, j + 1)],
                    );
                }
            }
        }
        mesh
    }
}

/// The pieces of a ground triangle (corners east, north, height) outside the openings of the
/// tunnels at `portals`, where it reaches into one; `None` where it does not. Nothing closes an
/// opening, and the ground meets the tube's ring round it (#135).
fn cut_out(
    portals: &[&structures::TunnelCut],
    triangle: [[f64; 3]; 3],
) -> Option<Vec<[[f64; 3]; 3]>> {
    let mut pieces = vec![triangle];
    let mut touched = false;
    for portal in portals {
        let mut next = Vec::new();
        for piece in pieces {
            match portal.cut(piece) {
                None => next.push(piece),
                Some(cut) => {
                    touched = true;
                    next.extend(cut);
                }
            }
        }
        pieces = next;
    }
    touched.then_some(pieces)
}

/// Keep water below a ground crossing even when its road cuts below the natural DEM.
fn crossing_drop(
    (channels, _, levels): (
        &channels::Channels,
        &LandIndex,
        &std::sync::Arc<water_levels::Levels>,
    ),
    east: f64,
    north: f64,
    ground: f64,
    shapers: &Shapers<'_>,
) -> f64 {
    levels.at(east, north).map_or(0.0, |datum| {
        (ground - datum).min(0.0) * (1.0 - channels.open(east, north, shapers))
    })
}

/// How far channels cut the ground at a point (`channels::Channels::depth`).
fn carve_at(
    (channels, land, levels): (
        &channels::Channels,
        &LandIndex,
        &std::sync::Arc<water_levels::Levels>,
    ),
    east: f64,
    north: f64,
    ground: f64,
    shapers: &Shapers<'_>,
) -> f64 {
    let inside = land.cover_at(east, north) == Some(LandCover::Water);
    if !inside && !channels.near(east, north, 0.0) {
        return 0.0;
    }
    let datum = levels.at(east, north).unwrap_or(ground);
    channels.depth(east, north, inside, ground - datum, shapers)
}

/// Height at (`u`, `v`) (0–1) of a cell with corner heights south-west, south-east,
/// north-west and north-east, split into the two triangles the mesh draws (along the
/// south-west to north-east diagonal).
fn on_triangles(sw: f64, se: f64, nw: f64, ne: f64, u: f64, v: f64) -> f64 {
    if v >= u {
        sw + (ne - nw) * u + (nw - sw) * v
    } else {
        sw + (se - sw) * u + (ne - se) * v
    }
}

/// What shapes the ground: the road ridden and the railways (`railways`), each level across just
/// below it, with cuttings and embankments; and, giving way to them, the paved streets, level
/// across (#116); and the portals of their tunnels, where those cuttings end and the ground
/// keeps out of the openings (#135).
pub(crate) struct Shapers<'a> {
    pub(crate) road: &'a RoadIndex,
    pub(crate) rails: &'a RoadIndex,
    pub(crate) streets: &'a streets::Levels,
    pub(crate) portals: &'a [structures::Portal],
    pub(crate) hollows: &'a [structures::TunnelCut],
    /// Whether the road's tunnels get a hill heaped over them where the terrain has none
    /// (`shape`, #135): not while their portals are placed, which looks for the hill the
    /// terrain has.
    pub(crate) hills: bool,
}

impl Shapers<'_> {
    /// Every piece of road or railway within `reach` that shapes the ground, as
    /// [`RoadIndex::near`] gives them: the road's tunnels only with `hills`, a railway's never.
    pub(crate) fn near(&self, east: f64, north: f64, reach: f64) -> Vec<(f64, f64, Surface)> {
        let mut found: Vec<_> = self
            .road
            .near_shaped(east, north, reach)
            .into_iter()
            .filter(|r| self.hills || r.2 != Surface::Tunnel)
            .collect();
        // The ridden carriageway and its verge have one datum. A lower parallel railway
        // may cut its own bank, but must not drag the road's shoulder and protection down.
        let ground_road = found
            .iter()
            .filter(|r| r.2 == Surface::Ground)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .copied();
        found.extend(self.streets.bridges.near_shaped(east, north, reach));
        found.extend(
            self.rails
                .near(east, north, reach)
                .into_iter()
                .filter(|r| r.2 != Surface::Tunnel)
                .map(|mut rail| {
                    if let Some((distance, height, _)) = ground_road
                        && height > rail.1
                    {
                        // Include the protection's footing and the fine triangles
                        // touching it, beyond the margin needed by the asphalt alone.
                        let roadside = (distance - VERGE - 1.0).max(0.0);
                        let trackside =
                            (rail.0 - railways::BED_M / 2.0 - FINE * std::f64::consts::SQRT_2)
                                .max(0.0);
                        let share = if roadside + trackside > 1e-9 {
                            roadside / (roadside + trackside)
                        } else {
                            0.0
                        };
                        let blend = share * share * (3.0 - 2.0 * share);
                        let level = height + (rail.1 - height) * blend;
                        // Ease the railway cutting between the protected road verge and
                        // its own bed, rather than dropping it at a hard distance threshold.
                        let room = (rail.0 - VERGE).max(0.0);
                        rail.1 = rail.1.max(level - room * CUT_SLOPE);
                    }
                    rail
                }),
        );
        found
    }
}

/// The ground at a point with natural height `natural`, shaped around the pieces of road near
/// it (distance, road elevation, surface): level just below the road out to the verge, then
/// cut into the hillside or banked down to the valley at most as steeply as cuttings and
/// embankments are, natural again further away. Under bridges the ground is only lowered.
/// Over a tunnel of the road ridden the ground is only raised: a tunnel runs under a hill, and
/// where the terrain has none, as on a cliff the samples smooth away or under a gallery, the
/// ground is heaped over the tube and banked down beyond it, so no tube lies bare on the
/// hillside (#135). Where the road passes more than once (hairpins), the ground stays below
/// every pass.
pub(crate) fn shape(natural: f64, roads: &[(f64, f64, Surface)]) -> f64 {
    let nearest = roads
        .iter()
        .filter(|r| r.2 != Surface::Tunnel)
        .min_by(|a, b| a.0.total_cmp(&b.0));
    // Past a portal the line's pieces count as its tunnel's (`road`): the hill's ground there.
    let mut height = match nearest {
        Some(&(distance, elevation, surface)) => {
            let level = elevation - ROAD_SINK;
            let room = (distance - VERGE).max(0.0);
            let (floor, ceiling) = (level - room * FILL_SLOPE, level + room * CUT_SLOPE);
            let shaped = match surface {
                // The valley under a bridge stays open: only ground above the deck is cut away.
                Surface::Bridge => natural.min(ceiling),
                Surface::Ground | Surface::Tunnel => natural.clamp(floor, ceiling),
            };
            shaped + (natural - shaped) * fade(distance)
        }
        None => natural,
    };
    for &(distance, elevation, surface) in roads {
        if surface == Surface::Tunnel {
            let (half, crown) = structures::tube(false);
            let top = elevation + crown + TUNNEL_HILL_COVER;
            let hill = top - (distance - (half + TUNNEL_HILL_SHOULDER)).max(0.0) * FILL_SLOPE;
            height = height.max(hill + (natural - hill) * fade(distance));
        }
    }
    for &(distance, elevation, surface) in roads {
        if surface == Surface::Tunnel {
            continue;
        }
        let ceiling = elevation - ROAD_SINK + (distance - VERGE).max(0.0) * CUT_SLOPE;
        let allowed = ceiling + (natural - ceiling).max(0.0) * fade(distance);
        height = height.min(allowed);
    }
    height
}

/// The natural ground at a point as the chunks draw it: the terrain sampled on their `GRID`
/// lattice and interpolated between the samples, as [`HeightGrid::natural_at`] does. The
/// terrain tiles are finer than the lattice, so where the ground rises abruptly, as the hill
/// over a tunnel's portal does, the drawn hill lies lower than the tiles have it: whatever
/// stands against the drawn ground is placed by the drawn ground (#135). `None` where the model
/// has no data.
pub(crate) async fn drawn_natural<M: ElevationModel>(
    model: &mut M,
    projection: &LocalProjection,
    (east, north): (f64, f64),
) -> Option<f64> {
    let (u, v) = (east / GRID, north / GRID);
    let (i, j) = (u.floor(), v.floor());
    let (fu, fv) = (u - i, v - j);
    let mut corners = [0.0; 4];
    for (slot, (di, dj)) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)]
        .into_iter()
        .enumerate()
    {
        let (lat, lon) = projection.unproject((i + di) * GRID, (j + dj) * GRID);
        corners[slot] = model.elevation(lat, lon).await.ok()?;
    }
    let [sw, se, nw, ne] = corners;
    let bottom = sw * (1.0 - fu) + se * fu;
    let top = nw * (1.0 - fu) + ne * fu;
    Some(bottom * (1.0 - fv) + top * fv)
}

/// The ground at a point as the chunks draw it, before channels cut it: the drawn natural
/// ground levelled across the paved street nearest to it (#116) and shaped around the road
/// ridden and the railways, as [`HeightGrid::shaped_at`] does. `None` where the model has no
/// data.
pub(crate) async fn drawn_ground<M: ElevationModel>(
    model: &mut M,
    projection: &LocalProjection,
    shapers: &Shapers<'_>,
    (east, north): (f64, f64),
) -> Option<f64> {
    let natural = drawn_natural(model, projection, (east, north)).await?;
    let levelled = match shapers.streets.nearest(east, north, 0.0) {
        Some((distance, half, foot)) => level_across(
            natural,
            drawn_natural(model, projection, foot).await?,
            distance - half,
        ),
        None => natural,
    };
    Some(shape(levelled, &shapers.near(east, north, LEVEL_REACH)))
}

/// The ground at a point with natural height `natural`, `beyond` metres outside the edge of a
/// paved street whose centre line's nearest point lies on natural ground at `level` (#116):
/// level with it out to `LEVEL_VERGE_M`, so the street is level across rather than tilted with
/// the hillside, then cut into the hillside or banked down to the natural ground at most as
/// steeply as the road's cuttings and embankments, natural again at `LEVEL_REACH_M`.
fn level_across(natural: f64, level: f64, beyond: f64) -> f64 {
    let room = (beyond - streets::LEVEL_VERGE_M).max(0.0);
    let shaped = natural.clamp(level - room * FILL_SLOPE, level + room * CUT_SLOPE);
    let t = ((room - (streets::LEVEL_REACH_M - streets::LEVEL_FADE_M)) / streets::LEVEL_FADE_M)
        .clamp(0.0, 1.0);
    shaped + (natural - shaped) * t * t * (3.0 - 2.0 * t)
}

/// 0 where the ground is fully shaped around the road, rising to 1 at `LEVEL_REACH`.
fn fade(distance: f64) -> f64 {
    let t = ((distance - (LEVEL_REACH - REACH_FADE)) / REACH_FADE).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[allow(clippy::cast_possible_truncation)]
fn unit(v: [f64; 3]) -> [f32; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [
        (v[0] / length) as f32,
        (v[1] / length) as f32,
        (v[2] / length) as f32,
    ]
}

/// A deterministic pseudo-random number in `[0, 1)` from a seed (`SplitMix64`), so the world
/// looks the same every time a route is loaded.
pub(crate) fn hash(seed: i64) -> f64 {
    #[allow(clippy::cast_sign_loss)] // bit reinterpretation
    let mut z = (seed as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    #[allow(clippy::cast_precision_loss)] // 53 significant bits are plenty
    let value = (z >> 11) as f64 / (1u64 << 53) as f64;
    value
}

#[cfg(test)]
mod tests;
