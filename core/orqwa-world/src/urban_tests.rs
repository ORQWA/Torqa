//! Rider-visible urban surfaces and the green spaces and carriageways they must preserve.

use super::*;

fn city() -> MapData {
    MapData {
        areas: vec![Area {
            cover: LandCover::Residential,
            outer: vec![rectangle(0.0, 500.0, 180.0, 550.0)],
            inner: vec![],
        }],
        ..MapData::default()
    }
}

fn house(id: i64, east: f64, north: f64, half: f64) -> Building {
    Building {
        id,
        outline: square(east, north, half),
        height: None,
        levels: Some(2.0),
        color: None,
    }
}

fn urban_vertices(world: &World) -> impl Iterator<Item = ([f32; 3], [f32; 3], f32)> + '_ {
    world.chunks.iter().flat_map(|chunk| {
        chunk
            .urban
            .vertices
            .iter()
            .zip(&chunk.urban.normals)
            .zip(&chunk.urban.colors)
            .map(move |((v, n), c)| {
                (
                    [v[0] + chunk.center[0], v[1], v[2] + chunk.center[2]],
                    *n,
                    c[0],
                )
            })
    })
}

#[tokio::test]
async fn urban_eligibility_gaps_leave_no_short_sidewalks_but_keep_long_runs_across_chunks() {
    let mut map = MapData::default();
    map.areas = [
        rectangle(0.0, 200.0, 20.0, 4.0),
        rectangle(0.0, 480.0, 20.0, 30.0),
    ]
    .into_iter()
    .map(|outer| Area {
        cover: LandCover::Residential,
        outer: vec![outer],
        inner: vec![],
    })
    .collect();
    let world = world(&map).await;
    for side in [-1.0, 1.0] {
        assert!(
            height_on(&world, |c| &c.urban, side * 4.0, -200.0).is_none(),
            "isolated short sidewalk survived"
        );
        for north in [465.0, 479.9, 480.1, 495.0] {
            assert!(
                height_on(&world, |c| &c.urban, side * 4.0, -north).is_some(),
                "long sidewalk lost at chunk join"
            );
        }
    }
}

#[tokio::test]
async fn offset_map_copies_do_not_fragment_or_duplicate_the_ridden_sidewalks() {
    for offset in [1.5, 2.5, 8.5] {
        let mut map = city();
        let mut street = road_north();
        street.line = vec![at(offset, -20.0), at(offset, 480.0)];
        let mut continuation = street.clone();
        continuation.line = vec![at(offset, 480.0), at(offset, 1020.0)];
        map.roads = vec![street, continuation];
        let world = world(&map).await;
        for north in [200.0, 479.75, 480.25, 700.0] {
            for side in [-1.0, 1.0] {
                for across in [3.3, 4.0, 4.8] {
                    assert!(
                        height_on(&world, |c| &c.urban, across * side, -north).is_some(),
                        "walk cut by its mapped copy at offset {offset}, {across}, {north}"
                    );
                }
            }
            assert!(
                height_on(&world, |c| &c.urban, offset as f32 + 4.5, -north).is_none(),
                "second sidewalk on the bank at offset {offset}, {north}"
            );
        }
    }
}

#[tokio::test]
async fn an_independent_nearby_street_retains_its_sidewalks() {
    let mut map = city();
    let mut other = road_north();
    other.line = vec![at(16.0, 0.0), at(16.0, 1000.0)];
    map.roads = vec![road_north(), other];
    let world = world(&map).await;
    for east in [12.0, 20.0] {
        for north in [200.0, 479.75, 480.25, 700.0] {
            assert!(
                height_on(&world, |c| &c.urban, east, -north).is_some(),
                "independent street lost its sidewalk at {east}, {north}"
            );
        }
    }
}

#[tokio::test]
async fn an_unridden_parallel_arm_of_the_same_map_feature_keeps_its_sidewalk() {
    let mut map = city();
    let mut loop_road = road_north();
    loop_road.line = vec![at(0.0, 0.0), at(0.0, 1000.0), at(8.0, 1020.0), at(8.0, 0.0)];
    map.roads.push(loop_road);
    let world = world(&map).await;
    for north in [200.0, 479.75, 480.25, 700.0] {
        assert!(
            height_on(&world, |c| &c.urban, 12.0, -north).is_some(),
            "a shared map ID must not erase an unridden parallel arm"
        );
    }
}

#[tokio::test]
async fn sidewalks_stay_level_with_their_road_above_a_lower_hairpin_cutting() {
    let mut points = Vec::new();
    for i in 0..=100 {
        points.push((0.0, f64::from(i) * 10.0));
    }
    for k in 1..12 {
        let a = std::f64::consts::PI * f64::from(k) / 12.0;
        points.push((8.0 - 8.0 * a.cos(), 1000.0 + 8.0 * a.sin()));
    }
    for i in (0..=100).rev() {
        points.push((16.0, f64::from(i) * 10.0));
    }
    let mut xml = String::from("<gpx><trk><trkseg>");
    for (east, north) in points {
        let (lat, lon) = at(east, north);
        let elevation = 500.0 - east * 1.7 + north * 0.06;
        let _ = write!(
            xml,
            r#"<trkpt lat="{lat}" lon="{lon}"><ele>{elevation}</ele></trkpt>"#
        );
    }
    xml.push_str("</trkseg></trk></gpx>");
    let map = city();
    let route = Route::from_gpx_with::<Level>(&xml, None, &map)
        .await
        .unwrap();
    let world = generate(&route, &mut Level(440.0), &map, &mut |_, _| {}).await;
    for north in [200.0, 479.75, 480.25, 700.0] {
        for east in [-4.6, -3.5, 3.5, 4.6] {
            let top = height_on(&world, |c| &c.urban, east, -north).expect("continuous upper walk");
            assert!(
                (top - (500.14 + north * 0.06)).abs() < 0.03,
                "walk draped down the bank at {east}, {north}: {top}"
            );
        }
    }
    for c in &world.chunks {
        assert_faces_follow_normals(&c.urban);
    }
}

#[tokio::test]
async fn rounding_one_streets_bend_does_not_cut_a_nearby_streets_sidewalk() {
    let mut map = city();
    let mut upper = road_north();
    upper.line = vec![at(16.0, 0.0), at(16.0, 1000.0)];
    let mut lower = road_north();
    lower.line = vec![at(0.0, 0.0), at(0.0, 500.0), at(-150.0, 1000.0)];
    map.roads = vec![upper, lower];
    let mut xml = String::from("<gpx><trk><trkseg>");
    for i in 0..=100 {
        let (lat, lon) = at(16.0, f64::from(i) * 10.0);
        let _ = write!(
            xml,
            r#"<trkpt lat="{lat}" lon="{lon}"><ele>500</ele></trkpt>"#
        );
    }
    xml.push_str("</trkseg></trk></gpx>");
    let route = Route::from_gpx_with::<Level>(&xml, None, &map)
        .await
        .unwrap();
    let projection = orqwa_routes::LocalProjection::for_route(&route);
    let world = generate(&route, &mut Level(500.0), &map, &mut |_, _| {}).await;
    for i in 0..=60 {
        let north = 498.0 + i as f32 * 0.1;
        let (lat, lon) = at(12.0, f64::from(north));
        let (east, local_north) = projection.project(lat, lon);
        assert!(
            height_on(&world, |c| &c.urban, east as f32, -local_north as f32).is_some(),
            "another street's rounded corner cut this sidewalk at {north}"
        );
    }
}

#[tokio::test]
async fn sidewalks_keep_their_full_length_through_ridden_width_changes() {
    use orqwa_osm::RoadKind::{Local, Primary, Tertiary};
    for (from, to) in [(Tertiary, Primary), (Primary, Local), (Local, Primary)] {
        let mut map = city();
        let mut narrow = road_north();
        narrow.details.kind = from;
        narrow.line[1] = at(0.0, 500.0);
        let mut wider = road_north();
        wider.class = RoadClass::Major;
        wider.details.kind = to;
        wider.line = vec![at(0.0, 500.0), at(0.0, 1000.0)];
        map.roads = vec![narrow, wider];
        let world = world(&map).await;
        for i in 0..=400 {
            let north = 480.0 + i as f32 * 0.1;
            for side in [-1.0, 1.0] {
                assert!(
                    height_on(&world, |c| &c.urban, side * 4.1, -north).is_some(),
                    "sidewalk gap at width transition: {side}, {north}"
                );
            }
        }
    }
}

#[tokio::test]
async fn sidewalks_keep_a_clear_walking_strip_beside_partial_building_overlaps() {
    let mut map = city();
    map.roads.push(road_north());
    map.buildings.push(house(401, 7.5, 500.0, 3.0));
    let world = world(&map).await;
    for i in 0..=100 {
        let north = 495.0 + i as f32 * 0.1;
        assert!(
            height_on(&world, |c| &c.urban, 3.3, -north).is_some(),
            "a building at the outer edge must not remove the clear walking strip at {north}"
        );
    }
    assert!(
        height_on(&world, |c| &c.urban, 5.0, -498.0).is_none(),
        "building remains clear"
    );
}

#[tokio::test]
async fn sidewalk_clipping_respects_the_drawn_outline_of_an_irregular_building() {
    let mut map = city();
    map.roads.push(road_north());
    let mut building = house(402, 7.5, 500.0, 3.0);
    building.outline = [
        (4.5, 497.0),
        (10.0, 497.0),
        (10.0, 503.0),
        (7.0, 503.0),
        (7.0, 500.0),
        (4.5, 500.0),
        (4.5, 497.0),
    ]
    .into_iter()
    .map(|(e, n)| at(e, n))
    .collect();
    map.buildings.push(building);
    let world = world(&map).await;
    assert!(
        height_on(&world, |c| &c.urban, 3.3, -501.5).is_some(),
        "the strip outside the drawn building remains open"
    );
    assert!(
        height_on(&world, |c| &c.urban, 4.7, -501.5).is_none(),
        "the fitted model occupies the notch in its mapped footprint"
    );
}

#[tokio::test]
async fn sidewalks_have_raised_curbs_and_leave_roads_junctions_and_green_spaces_open() {
    let mut map = city();
    map.roads.push(road_north());
    for (cover, east, north) in [
        (LandCover::Meadow, 30.0, 300.0),
        (LandCover::Water, -30.0, 700.0),
    ] {
        map.areas.push(Area {
            cover,
            outer: vec![square(east, north, 25.0)],
            inner: vec![],
        });
    }
    map.roads.push(orqwa_osm::Road {
        class: orqwa_osm::RoadClass::Street,
        details: orqwa_osm::RoadDetails::default(),
        line: vec![at(-100.0, 500.0), at(100.0, 500.0)],
        structure: None,
    });
    let world = world(&map).await;
    for chunk in &world.chunks {
        assert_valid(&chunk.urban);
        assert_faces_follow_normals(&chunk.urban);
        for vertex in &chunk.urban.vertices {
            assert!(
                vertex[0].abs() <= CHUNK_SIZE as f32 / 2.0 + 0.01
                    && vertex[2].abs() <= CHUNK_SIZE as f32 / 2.0 + 0.01,
                "urban surface spills beyond its chunk"
            );
        }
    }
    let vertices: Vec<_> = urban_vertices(&world).collect();
    assert!(vertices.len() > 100, "no urban sidewalks");
    let mut curbs = 0;
    for ([x, y, z], normal, material) in vertices {
        assert!(
            x.abs() > 3.0 || !(0.0..=1000.0).contains(&-z),
            "paving on the ridden road"
        );
        assert!(
            x.abs() > 100.0 || (z + 500.0).abs() >= 2.75,
            "paving across a joining street"
        );
        assert!(
            (x - 30.0).abs() >= 25.0 || (z + 300.0).abs() >= 25.0,
            "paving in the park"
        );
        assert!(
            (x + 30.0).abs() >= 25.0 || (z + 700.0).abs() >= 25.0,
            "paving in water"
        );
        if material == 0.75 && x.abs() < 3.3 && normal[0].abs() > 0.9 {
            let ground = ground_at(&world, x, z).expect("terrain below curb");
            let rise = y - ground;
            assert!((-0.03..0.30).contains(&rise), "curb rise {rise}");
            assert!(normal[0] * x < 0.0, "curb should face the road");
            curbs += 1;
        }
    }
    assert!(curbs > 10, "no raised curb faces");
}

#[tokio::test]
async fn sidewalks_are_grounded_and_join_lowered_access_without_steps() {
    let mut map = city();
    map.buildings = vec![house(301, 25.0, 250.0, 6.0), house(302, 25.0, 480.0, 6.0)];
    let world = world(&map).await;
    let mut sides = 0;
    for chunk in &world.chunks {
        assert_valid(&chunk.urban);
        assert_faces_follow_normals(&chunk.urban);
        for triangle in chunk.urban.indices.as_chunks::<3>().0 {
            if chunk.urban.normals[triangle[0] as usize][1].abs() > 0.01 {
                continue;
            }
            let rises = [triangle[0], triangle[1], triangle[2]].map(|index| {
                let v = chunk.urban.vertices[index as usize];
                v[1] - ground_at(&world, v[0] + chunk.center[0], v[2] + chunk.center[2]).unwrap()
            });
            if rises.iter().copied().fold(f32::MIN, f32::max) > 0.4 {
                continue; // Garden walls have their own ground contact.
            }
            assert!(
                rises.iter().any(|rise| *rise <= 0.001),
                "floating sidewalk face {rises:?}"
            );
            sides += 1;
        }
    }
    assert!(sides > 100, "no solid sidewalk edges");
    for entrance in [250.0, 480.0] {
        let mut previous: Option<f32> = None;
        for step in -100..=100 {
            let z = -(entrance + step as f32 * 0.1);
            let y = height_on(&world, |chunk| &chunk.urban, 4.0, z).expect("continuous sidewalk");
            if let Some(before) = previous {
                assert!(
                    (y - before).abs() < 0.025,
                    "step beside entrance at {z}: {before} to {y}"
                );
            }
            previous = Some(y);
        }
    }
}

#[tokio::test]
async fn sidewalks_join_on_both_sides_of_a_bend() {
    let mut map = city();
    map.roads.push(orqwa_osm::Road {
        class: orqwa_osm::RoadClass::Street,
        details: orqwa_osm::RoadDetails::default(),
        line: vec![at(50.0, 150.0), at(50.0, 200.0), at(80.0, 250.0)],
        structure: None,
    });
    let world = world(&map).await;
    let forward = (30.0_f32 / 3400.0_f32.sqrt(), 50.0_f32 / 3400.0_f32.sqrt());
    let divisor = 1.0 + forward.1;
    for side in [-1.0, 1.0] {
        for width in [3.0, 3.5, 4.0, 4.2] {
            // Cross the joint inside the rounded walking strip, including the
            // wedge two independently offset rectangular slabs leave uncovered.
            let east = 50.0 + width * side;
            let north = 200.0 - forward.0 / divisor * width * side;
            for step in -10..=10 {
                let z = -(north + step as f32 * 0.01);
                assert!(
                    height_on(&world, |chunk| &chunk.urban, east, z).is_some(),
                    "sidewalk gap at {east}, {}",
                    -z
                );
            }
        }
    }
}

#[tokio::test]
async fn sidewalks_follow_rounded_junction_corners_without_square_gaps() {
    let mut map = city();
    map.roads = vec![
        road_north(),
        orqwa_osm::Road {
            class: orqwa_osm::RoadClass::Street,
            details: orqwa_osm::RoadDetails::default(),
            line: vec![at(-100.0, 500.0), at(0.0, 500.0)],
            structure: None,
        },
    ];
    let world = world(&map).await;
    for chunk in &world.chunks {
        assert_valid(&chunk.urban);
        assert_faces_follow_normals(&chunk.urban);
        assert!(
            chunk
                .urban
                .normals
                .iter()
                .all(|normal| { (normal.iter().map(|v| v * v).sum::<f32>() - 1.0).abs() < 0.001 }),
            "urban lighting normals must be unit vectors"
        );
    }
    // The footway follows the same tangent circle as the road's rounded corner,
    // on both sides of the side street, at its full walking width.
    let radius = (3.0_f32 + 2.75) * 0.6;
    for side in [-1.0, 1.0] {
        let center = (-3.0 - radius, 500.0 + side * (2.75 + radius));
        for step in 0..=32 {
            let angle = step as f32 / 32.0 * std::f32::consts::FRAC_PI_2;
            for across in [0.2, 0.9, 1.6] {
                let r = radius - 0.12 - across;
                let (east, north) = (
                    center.0 + r * angle.cos(),
                    center.1 - side * r * angle.sin(),
                );
                assert!(
                    height_on(&world, |chunk| &chunk.urban, east, -north).is_some(),
                    "gap around junction curb at {east}, {north}"
                );
                assert!(
                    street_at(&world, east, -north).is_none(),
                    "sidewalk in junction asphalt"
                );
            }
        }
    }
    for east in [-2.0, -1.0, 0.0] {
        assert!(
            height_on(&world, |chunk| &chunk.urban, east, -500.0).is_none(),
            "curb across the junction mouth"
        );
    }
}

#[tokio::test]
async fn acute_forks_have_a_rounded_sidewalk_nose_instead_of_cut_off_slabs() {
    let mut map = city();
    map.roads = vec![
        road_north(),
        orqwa_osm::Road {
            class: orqwa_osm::RoadClass::Street,
            details: orqwa_osm::RoadDetails::default(),
            line: vec![at(-40.0, 400.0), at(0.0, 500.0)],
            structure: None,
        },
    ];
    let world = world(&map).await;
    let angle = 0.4_f32.atan();
    let radius = (3.0 + 2.75) * 0.6;
    let apex_north = 500.0 - (2.75 + 3.0 * angle.cos()) / angle.sin();
    let center = (-3.0 - radius, apex_north - radius / (angle / 2.0).tan());
    for step in 1..32 {
        let arc = (std::f32::consts::PI - angle) * step as f32 / 32.0;
        let r = radius - 0.12 - 0.9;
        let (east, north) = (center.0 + r * arc.cos(), center.1 + r * arc.sin());
        assert!(
            height_on(&world, |chunk| &chunk.urban, east, -north).is_some(),
            "gap around fork nose at {east}, {north}"
        );
        assert!(
            street_at(&world, east, -north).is_none(),
            "footway overlaps fork asphalt"
        );
    }
}

#[tokio::test]
async fn narrow_side_roads_do_not_get_isolated_sidewalk_corner_tiles() {
    let mut map = city();
    let mut side = road_north();
    side.class = RoadClass::Service;
    side.details.kind = orqwa_osm::RoadKind::Service;
    side.line = vec![at(0.0, 500.0), at(80.0, 500.0)];
    map.roads = vec![road_north(), side];
    let world = world(&map).await;
    for e in 0..=30 {
        for n in 0..=30 {
            let (east, north) = (5.0 + e as f32 * 0.1, 502.0 + n as f32 * 0.1);
            assert!(
                height_on(&world, |c| &c.urban, east, -north).is_none(),
                "detached corner tile at {east}, {north}"
            );
        }
    }
    for n in [490.0, 510.0] {
        assert!(
            height_on(&world, |c| &c.urban, 4.0, -n).is_some(),
            "the main street retains its sidewalk"
        );
    }
}

#[tokio::test]
async fn rounded_junction_asphalt_trims_other_sidewalk_pieces() {
    let mut map = city();
    let street = |line: &[(f64, f64)]| orqwa_osm::Road {
        class: RoadClass::Street,
        details: orqwa_osm::RoadDetails::default(),
        line: line.iter().map(|&(e, n)| at(e, n)).collect(),
        structure: None,
    };
    map.roads = vec![
        street(&[
            (56.0, 200.0),
            (55.0, 204.0),
            (51.0, 206.0),
            (47.0, 204.0),
            (45.0, 200.0),
            (47.0, 196.0),
            (51.0, 194.0),
            (55.0, 196.0),
            (56.0, 200.0),
            (80.0, 200.0),
        ]),
        street(&[(51.0, 206.0), (51.0, 230.0)]),
    ];
    let world = world(&map).await;
    let mut paved = 0;
    for x in 0..51 {
        for n in 0..61 {
            let (east, north) = (45.0 + x as f32 * 0.2, 194.0 + n as f32 * 0.2);
            // Stay inside the asphalt rather than testing numerical shared curb edges.
            if [(0.0, 0.0), (0.1, 0.0), (-0.1, 0.0), (0.0, 0.1), (0.0, -0.1)]
                .into_iter()
                .all(|(e, n)| street_at(&world, east + e, -north + n).is_some())
            {
                paved += 1;
                assert!(
                    height_on(&world, |c| &c.urban, east, -north).is_none(),
                    "sidewalk rim across rounded asphalt at {east}, {north}"
                );
            }
        }
    }
    assert!(paved > 100, "the fixture must exercise the carriageway");
}

fn assert_continuous_walking_tread(world: &World, shift: f32) {
    // Disjoint tops can still have a step at their shared edge. Sample both sides
    // of every internal mesh edge rather than relying only on projected overlaps.
    let walking_height = |east, z| {
        let mut top: Option<f32> = None;
        for chunk in &world.chunks {
            let mesh = &chunk.urban;
            for face in mesh.indices.as_chunks::<3>().0 {
                // The narrow curb border has a deliberate bevel; test the tread.
                if face
                    .iter()
                    .any(|&i| mesh.colors[i as usize][0] > 0.5 || mesh.normals[i as usize][1] < 0.2)
                {
                    continue;
                }
                let triangle = face.map(|i| {
                    let p = mesh.vertices[i as usize];
                    [p[0] + chunk.center[0], p[1], p[2] + chunk.center[2]]
                });
                if let Some(y) = triangle_height(triangle, east, z) {
                    top = Some(top.map_or(y, |old| old.max(y)));
                }
            }
        }
        top
    };
    for chunk in &world.chunks {
        let mesh = &chunk.urban;
        for face in mesh.indices.as_chunks::<3>().0 {
            if face
                .iter()
                .all(|&i| mesh.normals[i as usize][1].abs() < 0.1)
            {
                let p = face.map(|i| {
                    let v = mesh.vertices[i as usize];
                    (v[0] + chunk.center[0], v[2] + chunk.center[2])
                });
                let center = (
                    (p[0].0 + p[1].0 + p[2].0) / 3.0,
                    (p[0].1 + p[1].1 + p[2].1) / 3.0,
                );
                if (48.0..52.0).contains(&center.0)
                    && (198.0 + shift..202.0 + shift).contains(&-center.1)
                {
                    let normal = mesh.normals[face[0] as usize];
                    assert!(
                        ![-1.0, 1.0].into_iter().all(|sign| walking_height(
                            center.0 + normal[0] * 0.002 * sign,
                            center.1 + normal[2] * 0.002 * sign
                        )
                        .is_some()),
                        "internal side wall through walking surface at {center:?}: {p:?}"
                    );
                }
            }
            if face.iter().any(|&i| mesh.normals[i as usize][1] < 0.2) {
                continue;
            }
            for edge in 0..3 {
                let [a, b] = [face[edge], face[(edge + 1) % 3]].map(|i| {
                    let p = mesh.vertices[i as usize];
                    (p[0] + chunk.center[0], p[2] + chunk.center[2])
                });
                let p = (a.0.midpoint(b.0), a.1.midpoint(b.1));
                if !(48.0..52.0).contains(&p.0) || !(198.0 + shift..202.0 + shift).contains(&-p.1) {
                    continue;
                }
                let length = (b.0 - a.0).hypot(b.1 - a.1).max(0.001);
                let offset = ((b.1 - a.1) * 0.005 / length, (a.0 - b.0) * 0.005 / length);
                if let (Some(a), Some(b)) = (
                    walking_height(p.0 + offset.0, p.1 + offset.1),
                    walking_height(p.0 - offset.0, p.1 - offset.1),
                ) {
                    assert!((a - b).abs() < 0.02, "walking step at {p:?}: {a}, {b}");
                }
            }
        }
    }
}

#[tokio::test]
async fn turning_islands_require_walking_length_and_retained_surfaces_do_not_stack() {
    for (reverse, shift, stretch) in [
        (false, 0.0, 1.0),
        (false, 0.0, 3.0),
        (true, 0.0, 3.0),
        (false, 280.0, 3.0),
    ] {
        let mut map = city();
        map.roads.push(orqwa_osm::Road {
            class: RoadClass::Major,
            details: orqwa_osm::RoadDetails::default(),
            line: [
                (56.0, 200.0),
                (55.0, 204.0),
                (51.0, 206.0),
                (47.0, 204.0),
                (45.0, 200.0),
                (47.0, 196.0),
                (51.0, 194.0),
                (55.0, 196.0),
                (56.0, 200.0),
                (80.0, 200.0),
            ]
            .into_iter()
            // Stretch the island to retain overlapping footways in the long cases;
            // its original compact form must lose the short isolated paving.
            .map(|(e, n)| at(e, 200.0 + (n - 200.0) * stretch + shift))
            .collect(),
            structure: None,
        });
        if reverse {
            map.roads[0].line.reverse();
        }
        let world = world(&map).await;
        let mut sampled = 0;
        for x in 0..21 {
            for n in 0..21 {
                let (east, north) = (48.0 + x as f32 * 0.2, 198.0 + shift as f32 + n as f32 * 0.2);
                let mut heights = Vec::new();
                for chunk in &world.chunks {
                    let mesh = &chunk.urban;
                    for face in mesh.indices.as_chunks::<3>().0 {
                        if face.iter().any(|&i| mesh.normals[i as usize][1] < 0.2) {
                            continue;
                        }
                        let tri = face.map(|i| {
                            let v = mesh.vertices[i as usize];
                            [v[0] + chunk.center[0], v[1], v[2] + chunk.center[2]]
                        });
                        if let Some(y) = triangle_height(tri, east, -north) {
                            heights.push(y);
                        }
                    }
                }
                if !heights.is_empty() {
                    sampled += 1;
                    let low = heights.iter().copied().fold(f32::INFINITY, f32::min);
                    let high = heights.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                    assert!(
                        high - low < 0.02,
                        "stacked paving at {east}, {north}: {heights:?}"
                    );
                }
            }
        }
        if stretch < 2.0 {
            assert_eq!(sampled, 0, "short isolated island paving must be omitted");
            continue;
        }
        assert!(
            sampled > 20,
            "the fixture must include walking surfaces inside the loop"
        );
        assert_continuous_walking_tread(&world, shift as f32);
        for i in 0..72 {
            let angle = i as f32 * std::f32::consts::TAU / 72.0;
            let (east, north) = (
                50.5 + 0.6 * angle.cos(),
                200.0 + shift as f32 + 0.6 * angle.sin(),
            );
            assert!(
                height_on(&world, |c| &c.urban, east, -north).is_some(),
                "one rounded corner cut another at {east}, {north}"
            );
        }
    }
}

#[tokio::test]
async fn curved_sidewalks_meet_straight_walks_without_height_steps() {
    let mut map = city();
    map.roads.push(orqwa_osm::Road {
        class: RoadClass::Street,
        details: orqwa_osm::RoadDetails::default(),
        line: vec![at(50.0, 150.0), at(50.0, 200.0), at(100.0, 200.0)],
        structure: None,
    });
    let world = world(&map).await;
    let radius = 2.75_f32 + 0.12 + 0.9;
    for ((a, b), description) in [
        (((50.0 - radius, 199.99), (50.0 - radius, 200.01)), "entry"),
        (((49.99, 200.0 + radius), (50.01, 200.0 + radius)), "exit"),
    ] {
        let height = |(east, north): (f32, f32)| {
            height_on(&world, |c| &c.urban, east, -north).expect("joined walking surface")
        };
        let step = (height(a) - height(b)).abs();
        assert!(step < 0.02, "{description} has a {step} m step");
    }
}

#[tokio::test]
async fn outer_sidewalk_bends_are_round_and_curbstones_have_their_own_beveled_border() {
    let mut map = city();
    map.roads.push(orqwa_osm::Road {
        class: orqwa_osm::RoadClass::Street,
        details: orqwa_osm::RoadDetails::default(),
        line: vec![at(50.0, 150.0), at(50.0, 200.0), at(100.0, 200.0)],
        structure: None,
    });
    let world = world(&map).await;
    for step in 1..32 {
        let angle = step as f32 / 32.0 * std::f32::consts::FRAC_PI_2;
        let radius = 2.75 + 0.12 + 0.9;
        let (east, north) = (50.0 - radius * angle.cos(), 200.0 + radius * angle.sin());
        assert!(
            height_on(&world, |chunk| &chunk.urban, east, -north).is_some(),
            "gap around outer bend at {east}, {north}"
        );
    }
    let mut curb_tops = 0;
    for ([x, _, _], normal, material) in urban_vertices(&world) {
        if material == 0.75 && normal[1] > 0.9 && x.abs() < 10.0 {
            assert!(
                (3.12 - 0.001..=3.30 + 0.001).contains(&x.abs()),
                "curbstone outside its narrow border: {x}"
            );
            curb_tops += 1;
        }
    }
    assert!(curb_tops > 100, "no separate curbstone tops");
    let walk = height_on(&world, |chunk| &chunk.urban, 4.0, -250.0).unwrap();
    let lip = height_on(&world, |chunk| &chunk.urban, 3.125, -250.0).unwrap();
    assert!(
        (0.01..0.025).contains(&(walk - lip)),
        "curb lip has no bevel"
    );
}

#[tokio::test]
async fn homes_and_businesses_have_access_and_forecourts_with_open_curbs() {
    let mut map = city();
    map.buildings = vec![house(105, 25.0, 250.0, 6.0), house(106, -25.0, 700.0, 8.0)];
    map.shops.push(at(-25.0, 700.0));
    let world = world(&map).await;
    for chunk in &world.chunks {
        assert_faces_follow_normals(&chunk.urban);
    }
    let paved = |x, z| height_on(&world, |chunk| &chunk.urban, x, z).is_some();
    assert!(paved(12.0, -250.0), "no path between the house and street");
    assert!(paved(18.5, -250.0), "no paved house frontage");
    assert!(paved(-10.0, -700.0), "no business access");
    assert!(paved(-16.0, -700.0), "no shop forecourt");
    assert!(!paved(25.0, -250.0), "paving inside the house");
    let opening =
        height_on(&world, |chunk| &chunk.urban, 4.0, -250.0).expect("access across sidewalk");
    let ground = ground_at(&world, 4.0, -250.0).unwrap();
    assert!(
        (opening - ground - ROAD_SINK as f32).abs() < 0.03,
        "curb blocks the house access"
    );
    assert!(
        urban_vertices(&world).any(|([x, y, z], _, material)| {
            material == 0.75
                && (x - 5.6).abs() < 0.5
                && (z + 250.0).abs() < 4.0
                && y - ground_at(&world, x, z).unwrap_or(y) > 0.6
        }),
        "no low front garden walls"
    );
    // A lawn remains beside the path, while the path itself has no grass poking through it.
    let grass: Vec<_> = world
        .chunks
        .iter()
        .flat_map(|chunk| {
            chunk
                .trees
                .grass
                .as_chunks::<12>()
                .0
                .iter()
                .map(move |t| (t[3] + chunk.center[0], t[11] + chunk.center[2]))
        })
        .collect();
    assert!(
        grass
            .iter()
            .any(|&(x, z)| (10.0..17.0).contains(&x) && (-275.0..-260.0).contains(&z)),
        "garden lost its lawn"
    );
    assert!(
        !grass
            .iter()
            .any(|&(x, z)| (6.0..18.0).contains(&x) && (z + 250.0).abs() < 0.65),
        "grass in the entrance path"
    );
}

#[tokio::test]
async fn dense_blocks_get_paving_but_parks_and_suburban_gardens_stay_green() {
    let mut map = city();
    for (row, north) in [200.0, 240.0, 280.0].into_iter().enumerate() {
        for (side, east) in [-27.0, 27.0].into_iter().enumerate() {
            map.buildings.push(Building {
                id: i64::try_from(row * 2 + side).unwrap(),
                outline: rectangle(east, north, 10.0, 14.0),
                height: None,
                levels: Some(4.0),
                color: None,
            });
        }
    }
    map.buildings.push(house(109, 25.0, 700.0, 6.0));
    map.areas.push(Area {
        cover: LandCover::Meadow,
        outer: vec![square(-8.0, 240.0, 8.0)],
        inner: vec![],
    });
    let world = world(&map).await;
    let urban_color = palette::srgb("ground.urban", 0.0);
    let park_color = landcover::color(Some(LandCover::Meadow));
    let town_color = landcover::color(Some(LandCover::Residential));
    let mut paving = 0;
    let mut park = 0;
    let mut garden = 0;
    for chunk in &world.chunks {
        for (v, color) in chunk.mesh.vertices.iter().zip(&chunk.mesh.colors) {
            let (x, z) = (v[0] + chunk.center[0], v[2] + chunk.center[2]);
            if (z + 240.0).abs() < 50.0 && *color == urban_color {
                paving += 1;
            }
            if (-15.0..-1.0).contains(&x) && (z + 240.0).abs() < 7.0 {
                assert_eq!(*color, park_color, "mapped park covered by dense city");
                park += 1;
            }
            if (20.0..50.0).contains(&x) && (z + 700.0).abs() < 20.0 && *color == town_color {
                garden += 1;
            }
        }
    }
    assert!(
        paving > 0 && park > 0 && garden > 0,
        "paving {paving}, park {park}, garden {garden}"
    );
}

#[tokio::test]
async fn countryside_and_structures_do_not_get_inferred_city_sidewalks() {
    let rural = world(&MapData::default()).await;
    assert_eq!(props_of(&rural, "bench"), [] as [[f64; 3]; 0]);
    assert!(
        rural
            .chunks
            .iter()
            .all(|chunk| chunk.urban.vertices.is_empty())
    );
    let structures = [Structure {
        kind: StructureKind::Bridge,
        line: vec![at(0.0, 300.0), at(0.0, 700.0)],
    }];
    let urban = generate(
        &route_north(&structures).await,
        &mut EastwardSlope,
        &city(),
        &mut |_, _| {},
    )
    .await;
    assert!(
        !urban_vertices(&urban)
            .any(|([x, _, z], _, _)| x.abs() < 6.0 && (-680.0..-320.0).contains(&z)),
        "sidewalk on a bridge"
    );
    assert!(
        !props_of(&urban, "bench")
            .iter()
            .any(|p| (320.0..680.0).contains(&p[2])),
        "bench on a bridge"
    );
}

#[tokio::test]
async fn city_benches_use_free_sidewalks_without_blocking_entrances_or_junctions() {
    let mut map = city();
    map.roads.push(road_north());
    map.buildings = vec![
        house(201, -25.0, 140.0, 6.0),
        house(202, -25.0, 260.0, 6.0),
        house(203, 25.0, 260.0, 6.0),
        Building {
            id: 204,
            outline: rectangle(12.0, 30.0, 5.0, 20.0),
            height: None,
            levels: Some(2.0),
            color: None,
        },
    ];
    map.shops.push(at(12.0, 30.0));
    map.areas.push(Area {
        cover: LandCover::Meadow,
        outer: vec![square(8.0, 500.0, 5.0)],
        inner: vec![],
    });
    map.roads.push(orqwa_osm::Road {
        class: orqwa_osm::RoadClass::Street,
        details: orqwa_osm::RoadDetails::default(),
        line: vec![at(-100.0, 620.0), at(100.0, 620.0)],
        structure: None,
    });
    let world = world(&map).await;
    let benches = props_of(&world, "bench");
    let bins = props_of(&world, "bin");
    assert!(benches.len() >= 4, "too little city seating: {benches:?}");
    assert_eq!(benches.len(), bins.len());
    assert!(
        benches
            .iter()
            .any(|p| p[0] > 0.0 && (p[2] - 20.0).abs() < 0.1),
        "a lowered shop frontage can hold a bench away from its entrance"
    );
    let mut along: Vec<_> = benches.iter().map(|p| p[2]).collect();
    along.sort_by(f64::total_cmp);
    assert!(
        along.windows(2).all(|pair| pair[1] - pair[0] > 89.0),
        "duplicate seating across chunks"
    );
    assert!(
        benches
            .iter()
            .any(|p| p[0] > 0.0 && (p[2] - 140.0).abs() < 0.1),
        "try the opposite sidewalk when an entrance blocks the preferred side"
    );
    assert!(
        benches
            .iter()
            .any(|p| p[0] < 0.0 && (p[2] - 500.0).abs() < 0.1),
        "try the opposite sidewalk beside a park"
    );
    assert!(
        benches
            .iter()
            .all(|p| (p[2] - 260.0).abs() > 3.0 && (p[2] - 620.0).abs() > 8.0),
        "leave both entrance paths and the junction clear: {benches:?}"
    );
    for [east, height, north] in benches.iter().chain(&bins) {
        let sidewalk = height_on(&world, |chunk| &chunk.urban, *east as f32, -*north as f32)
            .expect("furniture stands on a drawn sidewalk");
        assert!(
            (height - f64::from(sidewalk)).abs() < 0.01,
            "furniture at {east}, {height}, {north}, sidewalk {sidewalk}"
        );
    }
    for seat in &benches {
        assert!(
            bins.iter().any(|bin| {
                (bin[0] - seat[0]).abs() < 0.01 && (bin[2] - seat[2] - 1.4).abs() < 0.01
            }),
            "no bin beside {seat:?}"
        );
    }
}

#[tokio::test]
async fn steep_city_sidewalks_do_not_get_benches() {
    use std::fmt::Write as _;

    struct Uphill;
    impl ElevationModel for Uphill {
        fn elevation(
            &mut self,
            lat: f64,
            _lon: f64,
        ) -> impl std::future::Future<Output = Result<f64, String>> + Send {
            std::future::ready(Ok(500.0 + (lat - 46.0) * METERS_PER_DEGREE * 0.12))
        }
    }
    let mut xml = String::from("<gpx><trk><trkseg>");
    for i in 0..=100 {
        let north = f64::from(i) * 10.0;
        let (lat, lon) = at(0.0, north);
        let elevation = 500.0 + north * 0.12;
        let _ = write!(
            xml,
            r#"<trkpt lat="{lat}" lon="{lon}"><ele>{elevation}</ele></trkpt>"#
        );
    }
    xml.push_str("</trkseg></trk></gpx>");
    let route = Route::from_gpx(&xml, None).await.unwrap();
    let world = generate(&route, &mut Uphill, &city(), &mut |_, _| {}).await;
    assert!(
        world
            .chunks
            .iter()
            .any(|chunk| !chunk.urban.vertices.is_empty())
    );
    assert_eq!(props_of(&world, "bench"), [] as [[f64; 3]; 0]);
    assert_eq!(props_of(&world, "bin"), [] as [[f64; 3]; 0]);
}
