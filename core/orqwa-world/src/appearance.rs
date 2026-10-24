//! Road appearance from the course's saved map. Widths and lane counts are visual estimates:
//! the current tile schema preserves hierarchy and paving, not surveyed widths or lanes.

use std::collections::HashMap;

use orqwa_osm::{MapData, Road, RoadClass, RoadDetails, RoadKind, RoadSurface, StructureKind};
use orqwa_routes::{LocalProjection, Surface};

use crate::drape;

/// Material and cross-section shared by the ridden road and the surrounding streets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Appearance {
    pub(crate) width: f64,
    /// Asphalt, gravel, dirt, concrete, paving, cobbles, grass (shader contract).
    material: u8,
    /// No paint, main-road centre/edges, same-direction lane dividers, edges only.
    markings: u8,
    lanes: u8,
    pub(crate) sidewalk: bool,
    pub(crate) grass_middle: bool,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            width: 6.0,
            material: 0,
            markings: 1,
            lanes: 2,
            sidewalk: true,
            grass_middle: false,
        }
    }
}

impl Appearance {
    pub(crate) fn of(road: &Road) -> Self {
        Self::from_tags(road.class, road.details)
    }

    fn from_tags(class: RoadClass, tags: RoadDetails) -> Self {
        let (mut width, mut markings, mut lanes, sidewalk): (f64, u8, u8, bool) = match tags.kind {
            RoadKind::Motorway => (10.5, 2, 2, false),
            RoadKind::Trunk if tags.oneway => (8.5, 2, 2, false),
            RoadKind::Trunk => (8.0, 1, 2, false),
            RoadKind::Primary => (7.0, 1, 2, true),
            RoadKind::Secondary => (6.5, 1, 2, true),
            RoadKind::Tertiary => (6.0, 1, 2, true),
            RoadKind::Local => (5.5, 0, 2, true),
            RoadKind::Service => (3.5, 0, 1, true),
            RoadKind::Track => (2.8, 0, 1, false),
            RoadKind::Cycleway => (2.5, 0, 1, false),
            RoadKind::Path => (1.6, 0, 1, false),
            RoadKind::Unknown => match class {
                RoadClass::Major => (7.0, 1, 2, true),
                RoadClass::Street => (5.5, 0, 2, true),
                RoadClass::Service => (3.5, 0, 1, true),
                RoadClass::Track => (2.8, 0, 1, false),
                RoadClass::Path => (1.6, 0, 1, false),
            },
        };
        if tags.ramp {
            width = 4.5;
            markings = 3;
            lanes = 1;
        } else if tags.oneway && !matches!(tags.kind, RoadKind::Motorway | RoadKind::Trunk) {
            width = if matches!(class, RoadClass::Major) {
                4.0
            } else {
                width.min(3.5)
            };
            markings = if markings == 0 { 0 } else { 3 };
            lanes = 1;
        }
        let material = match tags.surface {
            RoadSurface::Paved | RoadSurface::Asphalt => 0,
            RoadSurface::Unpaved | RoadSurface::Gravel => 1,
            RoadSurface::Dirt => 2,
            RoadSurface::Concrete => 3,
            RoadSurface::Paving => 4,
            RoadSurface::Cobbles => 5,
            RoadSurface::Grass => 6,
            RoadSurface::Unknown => u8::from(
                matches!(class, RoadClass::Track | RoadClass::Path)
                    && tags.kind != RoadKind::Cycleway,
            ),
        };
        if material != 0 && material != 3 {
            markings = 0;
        }
        Self {
            width,
            material,
            markings,
            lanes,
            sidewalk: sidewalk && matches!(material, 0 | 3 | 4 | 5) && !tags.ramp,
            grass_middle: class == RoadClass::Track && material == 1,
        }
    }

    pub(crate) fn paved(self) -> bool {
        matches!(self.material, 0 | 3 | 4 | 5)
    }

    /// Normalised mesh metadata; all four components are data, never colour or opacity.
    #[allow(clippy::cast_possible_truncation)] // bounded widths for GPU attributes
    pub(crate) fn color(self) -> [f32; 4] {
        [
            f32::from(self.material) / 8.0,
            self.width as f32 / 32.0,
            f32::from(self.markings) / 4.0,
            f32::from(self.lanes + if self.grass_middle { 4 } else { 0 }) / 8.0,
        ]
    }

    pub(crate) fn plain_color(mut self) -> [f32; 4] {
        self.markings = 0;
        self.color()
    }
}

struct Piece {
    source: usize,
    a: (f64, f64),
    b: (f64, f64),
    structure: Option<StructureKind>,
    appearance: Appearance,
}

#[derive(Clone, Copy)]
struct Matched {
    appearance: Appearance,
    source: usize,
    point: (f64, f64),
}

/// Short pieces indexed once, so matching a long route never scans the entire road network.
pub(crate) struct Index {
    pieces: Vec<Piece>,
    cells: HashMap<(i64, i64), Vec<usize>>,
}

impl Index {
    pub(crate) fn new(map: &MapData, projection: &LocalProjection) -> Self {
        let mut index = Self {
            pieces: Vec::new(),
            cells: HashMap::new(),
        };
        for (source, road) in map.roads.iter().enumerate() {
            let line: Vec<_> = road
                .line
                .iter()
                .map(|&(lat, lon)| projection.project(lat, lon))
                .collect();
            for pair in drape::densify(&line, 20.0).windows(2) {
                let i = index.pieces.len();
                index.pieces.push(Piece {
                    source,
                    a: pair[0],
                    b: pair[1],
                    structure: road.structure,
                    appearance: Appearance::of(road),
                });
                let (a, b) = (cell(pair[0]), cell(pair[1]));
                index.cells.entry(a).or_default().push(i);
                if a != b {
                    index.cells.entry(b).or_default().push(i);
                }
            }
        }
        index
    }

    pub(crate) fn at(
        &self,
        point: (f64, f64),
        direction: (f64, f64),
        surface: Surface,
    ) -> (Appearance, Option<usize>) {
        self.matched(point, direction, surface).map_or_else(
            || (Appearance::default(), None),
            |m| (m.appearance, Some(m.source)),
        )
    }

    /// Position on the matched mapped section, rather than just its whole feature ID.
    pub(crate) fn point_at(
        &self,
        point: (f64, f64),
        direction: (f64, f64),
        surface: Surface,
    ) -> Option<(f64, f64)> {
        self.matched(point, direction, surface).map(|m| m.point)
    }

    fn matched(
        &self,
        point: (f64, f64),
        direction: (f64, f64),
        surface: Surface,
    ) -> Option<Matched> {
        let (cx, cy) = cell(point);
        let mut best: Option<(f64, Matched)> = None;
        for x in cx - 1..=cx + 1 {
            for y in cy - 1..=cy + 1 {
                for &i in self.cells.get(&(x, y)).into_iter().flatten() {
                    let piece = &self.pieces[i];
                    let structure = match surface {
                        Surface::Ground => None,
                        Surface::Bridge => Some(StructureKind::Bridge),
                        Surface::Tunnel => Some(StructureKind::Tunnel),
                    };
                    if piece.structure != structure {
                        continue;
                    }
                    let (dx, dy) = (piece.b.0 - piece.a.0, piece.b.1 - piece.a.1);
                    let length = dx.hypot(dy);
                    if length < 0.01 {
                        continue;
                    }
                    let alignment = ((dx * direction.0 + dy * direction.1) / length).abs();
                    if alignment < 0.85 {
                        continue;
                    }
                    let t = (((point.0 - piece.a.0) * dx + (point.1 - piece.a.1) * dy)
                        / (length * length))
                        .clamp(0.0, 1.0);
                    let distance =
                        (point.0 - piece.a.0 - t * dx).hypot(point.1 - piece.a.1 - t * dy);
                    if distance > 12.0 {
                        continue;
                    }
                    let score = distance + 20.0 * (1.0 - alignment);
                    if best.is_none_or(|b| score < b.0) {
                        best = Some((
                            score,
                            Matched {
                                appearance: piece.appearance,
                                source: piece.source,
                                point: (piece.a.0 + t * dx, piece.a.1 + t * dy),
                            },
                        ));
                    }
                }
            }
        }
        best.map(|b| b.1)
    }
}

fn cell((east, north): (f64, f64)) -> (i64, i64) {
    #[allow(clippy::cast_possible_truncation)] // local coordinates
    (
        (east / 100.0).floor() as i64,
        (north / 100.0).floor() as i64,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highways_main_roads_and_local_access_have_distinct_cross_sections() {
        let look = |kind, ramp, oneway| {
            Appearance::from_tags(
                RoadClass::Major,
                RoadDetails {
                    kind,
                    ramp,
                    oneway,
                    ..RoadDetails::default()
                },
            )
        };
        let highway = look(RoadKind::Motorway, false, true);
        let main = look(RoadKind::Primary, false, false);
        let ramp = look(RoadKind::Motorway, true, true);
        let local = look(RoadKind::Local, false, false);
        assert!(highway.width > main.width && main.width > local.width);
        assert!(ramp.width < highway.width);
        assert_eq!(highway.markings, 2, "same-direction lane dividers");
        assert_eq!(ramp.lanes, 1);
        assert_eq!(
            local.markings, 0,
            "no invented centre line on residential streets"
        );
        assert!(!highway.sidewalk && !ramp.sidewalk);
    }

    #[test]
    fn mapped_surface_overrides_class_and_only_unpaved_tracks_get_centre_grass() {
        let look = |class, kind, surface| {
            Appearance::from_tags(
                class,
                RoadDetails {
                    kind,
                    surface,
                    ..RoadDetails::default()
                },
            )
        };
        let paved_track = look(RoadClass::Track, RoadKind::Track, RoadSurface::Paved);
        let unpaved_street = look(RoadClass::Street, RoadKind::Local, RoadSurface::Unpaved);
        let cycleway = look(RoadClass::Path, RoadKind::Cycleway, RoadSurface::Paved);
        let trail = look(RoadClass::Path, RoadKind::Path, RoadSurface::Unpaved);
        assert!(paved_track.paved() && !paved_track.grass_middle);
        assert!(!unpaved_street.paved() && !unpaved_street.sidewalk);
        assert!(cycleway.paved() && cycleway.width < 3.0 && cycleway.markings == 0);
        assert!(!trail.grass_middle && !cycleway.grass_middle);
        assert!(look(RoadClass::Track, RoadKind::Track, RoadSurface::Unpaved).grass_middle);
    }
}
