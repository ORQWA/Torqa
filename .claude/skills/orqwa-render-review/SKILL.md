---
name: orqwa-render-review
description: How to see ORQWA's 3D world and models without a GPU — render screenshots in the dev container with software Vulkan (lavapipe), at a chosen place on a route and from a chosen camera, then judge and share them. Use whenever a change affects what the world, roads, terrain, buildings, vegetation, riders, sky or lighting look like, before declaring it done.
---

# Rendering ORQWA for review

On a Windows PC with a GPU, render natively instead: `scripts/dev.sh scripts/build-gdext.sh
release windows` cross-builds the extension into `app/bin/` (ADR 0021), and the portable Godot
editor on the host runs the same scripts (`app/tools/render_views.gd` with `ROUTE`, `SHOTS`,
`OUT_DIR`) in seconds — a long route in under a minute. Rebuild the DLL after any Rust change;
only the editor runs on the host.

Godot renders in the dev container with **lavapipe** (software Vulkan) on a virtual display.
It is correct but slow: a town scene can take minutes per frame burst, so take few, chosen
shots, run them in the background, and read the PNGs with the Read tool. Output goes to
`screenshots/` (gitignored).

## Ready-made

For road hierarchy and paving (R119), generate the offline comparison meshes with
`cargo test --manifest-path core/Cargo.toml -p orqwa-world road_profiles_render_fixture -- --ignored`,
then render `app/tools/render_roads.gd` with `OUT` set to an absolute PNG path. It shares
`RoadMaterials` and the real Rust generator with the app, rather than redrawing profiles.
Review motorway/ramp shoulders and dashes, unmarked local ways, narrow ridden cycleways and
grass only in unpaved tracks. Pair the controlled scene with fixed real-route views, including
Oberalp and rain; compare against current main using the same cameras and software settings.

- `scripts/dev.sh scripts/render-views.sh` — the **standard views** (`app/tools/render_views.gd`):
  fixed shots on the fixture routes (village, climb, hairpin, lake, rain, morning, evening,
  shops, junctions, river, old town, roundabout, bridges and the Oberalp reference route) into
  `screenshots/views/`. Render them before a visual change (`OUT_DIR=…/views-before`, pass it inside
  `sh -c '…'`) and after, and compare pairs; `VIEWS="village-chase lake-drone"` limits it.
  For a prepared course, set `COURSE=core/orqwa-gd/courses/twann-chasseral.tqc` and
  `SHOTS="shortcut:4200:2:0,180,-20"` (adjust the distance/camera as needed). This opens its saved
  inputs and builds offline, rather than preparing a GPX with fresh provider requests.

- `scripts/dev.sh scripts/screenshots.sh` — the standard app screenshots (`app/tests/
  screenshots.gd`): start page tabs, course page, a ride in each camera, the settings, summary,
  history, analysis, workouts and the overlay. The rider gets 16 weeks of made-up rides first
  (`orqwa-cli sample-rides`), so the history and the Analysis tab have something to show. The
  wiki guides show some of them (assets in `docs/images/app/`, `workouts/`, `overlay/`); copy the new ones
  there when a change alters a screen.
- `scripts/dev.sh scripts/render-models.sh` — every Blender model as the world draws it:
  buildings (three variants and a close-up each) and plants (three variants) into
  `screenshots/models/<group>/`, the clouds together; `GROUPS="vegetation clouds"` or
  `MODELS="house_gable_2_m chalet_2_m"` limits it (pass them inside `sh -c '…'` so they reach
  the container). `scripts/render-riders.sh` does the same for the riders.
- With `GALLERY=docs/images/models`, both also write the small JPEGs of the gallery in
  `art/README.md`; refresh it whenever a model changes.

## A view of a place on a route

Copy a template from `templates/` into `app/tests/` (they are scratch scripts: **never commit
them**; delete them and their `.uid` files afterwards), then run a small runner script with
`scripts/dev.sh sh <runner>`:

- `templates/tmp_road.gd` — loads `core/fixtures/$ROUTE.gpx`, starts a simulated ride, jumps to
  `$DIST` metres, uses camera `$CAM` (0 chase, 1 first person, 2 drone), saves `$OUT`.
- `templates/tmp_buildings.gd` — the same, then a free camera `$HEIGHT` up, `$BACK` behind,
  `$SIDE` aside, looking `$AHEAD` and `$LOOK_SIDE`; with `NEAR_MODEL=any` it aims at the model
  instance nearest the rider from eye height instead (good for street-level detail).

Runner pattern (one `godot` per shot; each loads the route and builds the world):

```sh
set -e
cd /workspaces/orqwa
scripts/build-gdext.sh debug >/dev/null
godot --headless --path app --import >/dev/null 2>&1 || true
shot() {
  name=$1; shift
  env "$@" OUT=$PWD/screenshots/q/$name.png timeout 1500 \
    xvfb-run -a -s "-screen 0 1600x900x24" godot --path app --rendering-driver vulkan \
    --resolution 1280x720 -s res://tests/tmp_road.gd 2>&1 | grep -E "ERROR|SCRIPT" | head -5
  echo "done $name"
}
shot hairpin ROUTE=gurtenstrasse DIST=1240 CAM=2
```

Fixtures: `gurtenstrasse` (Wabern village, a hillside climb with a hairpin), `kirchenfeldbruecke`
(Bern, a bridge, blocks and halls), `bielersee` (lakeside road, vineyards, rock cuttings), and
`oberalp`, the reference route (#167): 33.6 km of a planner's file from Andermatt over the
Oberalp pass to Disentis, with hairpins, three road tunnels, the rack railway alongside, a lake
the planner drew the road through, the summit tunnel and two villages. Render its views after
any change to routes, roads, tunnels or railways; `orqwa-cli route core/fixtures/oberalp.gpx`
prints its figures, and `cargo test -p orqwa-app -- --ignored reference_route` checks them
against the cached tiles. Map and terrain tiles are cached in the container's volumes after
the first run.

For rock changes (R115), render `rock-cliff-overview` and `rock-cliff-close` (the cameras
find a broad steep face on the Bern fixture), plus `rock_block` and `rock_pair` in the model
gallery. Grain alone is insufficient at distance. Refresh the linked wiki images too.

## Pitfalls

For sidewalks, use the full saved Alpenbrevet Gold inputs at 209.36 km to go (about 1.38 km
from the start) in Andermatt. Inspect the elevated bend and the nearby village junction in
one overview, then move closer to the walking edge. Offset GPX/map copies must not make
duplicate strips or trim each other into pieces. Retain independent streets' walks, include
tile-piece and terrain-chunk joins, and check curbs and lowered access from road height.
Include a rounded bend beside a separate street: its corner replacement must not cut the
neighbour's sidewalk, including when the two streets have different heights.
Inspect the full width of tangent joins at street height on sloping ground. Straight and
curved strips must share a grade, including lowered access; intact footprints from above
do not prove that the walking surface is continuous.
Include a small turning island on sloping ground and an urban junction with a narrow
service arm. Walking surfaces must not stack inside the island, and corners must not
leave isolated tiles where no eligible sidewalk approaches exist.
Inferred raised sidewalk runs need 12 m of surviving connected length. Check small corner
remnants and the Alpenbrevet village at 116.93 km to go alongside long village strips.
Short construction sections and curves must qualify together across chunk boundaries;
clipped-away street length, width partitions and point contacts cannot qualify a fragment.
Use an elongated island for overlap/grade checks; a short isolated island is omitted.
Check the entire island perimeter at close range: a surviving raised rim with a channel
inside it is still incorrect. Junction construction wedges must not excavate an existing
walking run; rounded carriageway fans must remain clear of paving.
Inspect internal clipping seams from road height too. Shared footway pieces must not
extrude side walls through their joined tread; retain closed sides only on the exposed
perimeter. If a dark triangle remains, inspect its mesh and repeat without shadows before
attributing it to lighting.
Include widening and narrowing streets and a wall touching only the outer part of a walk.
The usable strip must remain continuous and keep clear of the actual asphalt, while clipped
outer faces reach terrain. Inspect the same sloping, partially clipped corner for reversed
faces or seams; the wider terrain-shaping envelope is not the walking boundary.

For roadside guardrail placement, include a tight hairpin from above with both edges visible.
Four-metre models placed on centre-line tangents can overlap inside a curve and leave gaps
outside it. Check shared blade endpoints, vertical alignment across terrain chunk boundaries,
and that posts remain upright. Include Alpenbrevet's start/finish loop as well as its mountain
switchbacks; a straight-road view cannot validate curve placement.
On curved bridges, check that the slab stays below the asphalt and that railings and their
posts remain outside the wearing surface, especially beside the start/finish roundabout.
At roundabouts, inspect the whole junction from above and from its entry: bridge fences must
not follow the GPX through its interior or cross themselves there. Aligning rail pieces is
insufficient. Confirm that genuine outside-edge protection and higher overpasses remain.
Check barriers against all visible carriageways, including parallel joining arms and rounded
junction fills. Both endpoints being clear is insufficient: inspect the middle of each span
and its thickness too. Review steel rails/posts, concrete kerbs/parapets and roadside fence
models, retaining bridge undersides and protection at different elevations.

For bridge geometry (R121), include a camera below a non-ridden road bridge: top and side
views cannot reveal a missing underside. Check its downward-facing deck across chunk joins,
and retain a view from above to check the road surface. Motorway/local-road decks and
unpaved track/path decks share this geometry; ridden-road and railway structures use another
builder.

For the bridge restyle (R122), compare `oberalp-railway-chase` with `oberalp-bridge-close`,
`oberalp-bridge-under` and `oberalp-bridge-side`, plus Bern's `bridge-side`. Include a tunnel
view when changing the shared structure material. Check low kerbs, open metal railings,
plausible deck widths, capped supports and closed ends without stretching the texture on
vertical faces. `surface_smoke.gd` uses software Vulkan to verify the four ribbon materials
stay opaque above and below; shader flags alone are insufficient evidence.

Bridge ownership needs a slightly offset GPX/map centre line in regression tests: full
cross-section coverage can leave a second mapped deck below the ridden bridge. Preserve
independent parallel and crossing bridges. For Alpenbrevet, the Lukmanier approach has
bridges and tunnels around 34.7–36.8 km from the original course start; use saved course
inputs and check the bridge from the side as well as the riding camera.

Keep the full route when reproducing terrain-grid faults: cropping changes the projection
origin and grid alignment. Alpenbrevet's portal approaches around 35.0 km and 39.2 km exposed
paving stretched across the already-cut terrain openings. Check both mouths; an open terrain
mesh alone does not prove the street ribbons, rounded caps and junctions leave them clear.

At 174.17 km to go on Alpenbrevet Gold (about 36.57 km from its 210.74 km start-to-finish
length), check the tunnel-to-bridge transition from the side. Map and resampled structure tags
can change a metre apart: point-wise suppression can leave a short raised deck and railing
across the approach. Use remaining course distance when the rider has jumped during review.

For floating supports, compare feet with the final terrain triangles, including carved
channels and steep banks. A raw DEM sample can land on a ridge absent from the drawn grid;
checking only the centre or four corners can leave a downhill edge or channel exposed. Include
both ridden/railway supports and mapped-road piers. Keep the same side camera in comparisons.

For railway transitions, check Alpenbrevet around 102.99 and 98.74 km to go (107.75 and
112.00 km from the start). Ground and bridge tag samples around one crossing must keep one
clearance, with gentle approaches in both tracks. Nearby tracks at different vertical levels
must not share elevation pins. Retain true level crossings and road/rail underpass clearance.

At bridge junctions, check the end height and the kerb and railing opening from the ridden
road. Compare a connection at deck level with an overpass at a different height; only the
former opens. Check both edges of a ridden bridge with a side street: the opposite edge must
stay protected, and the slab underside must remain closed through the opening.

Also review Alpenbrevet's short mapped bridges near 32.05, 141.89 and 143.45 km from the start
(the 178.51, 68.85 and 67.26 km-to-go screenshots can look ahead or behind the rider). Ground
route tags must not leave a second raw-DEM deck over the ridden pavement. Preserve separate
bridges above a tunnel and bridges that leave the ridden corridor.

For tunnel interiors, inspect the full saved Alpenbrevet route around 175.72 km to go
(35.02 km from the start) from inside and outside. Look beyond the first six metres: a
nearby lower way can cut terrain back into the tube farther inside. Check roof paving from
above the mouth as well. Retain the hillside outside the shell and distinct roads crossing
above; clipping must follow the complete curved road or bundled railway tube.

At Alpenbrevet's 33.52 km-to-go protection report (177.22 km from the start), render the
actual guardrail instances from the valley side. A lower parallel railway must not pull
down the ridden shoulder, posts or draped paving. Check that the rails meet their ground
at carriageway height and that the separate railway keeps its original lower elevation.
Include a road on a chunk boundary: its protection can stand in a neighbour and must use
that neighbour's refined ground rather than coarse border extrapolation from its owner.

At 30.64 km to go (180.10 km from the start), inspect the switchbacks from the valley side.
Asphalt with lane dashes below the ridden road is duplicate terrain-draped paving, not a
shadow. Compare mapped street triangles as well as the image, using the saved full course.
Offset GPX/map centre lines must share a pavement owner without removing shallow side-road
arms, crossings, independent parallel cycleways or wider highways beside a ridden cycleway.
Also compare guardrails with the upper switchback's asphalt height. A neighbouring lower
road can cut the bank below that edge even without a railway: the blade must follow its
own carriageway and buried post feet must meet the road edge/skirt. Check both arms remain
clear, curved span endpoints still meet, and chunk boundaries do not change the datum.

For a complete documentation refresh, follow
[`docs/development.md#refreshing-screenshots`](../../../docs/development.md#refreshing-screenshots):
isolate sample data, capture device/error/planned-workout fixtures and licensed local video,
review contact sheets, then export to stable wiki image paths. `SCREENSHOT_DIR` chooses the app
capture folder, and `OUT_DIR` also works for model and rider runners. The app and route wrappers
set `ORQWA_RENDER_SOFTWARE=1` for FXAA without MSAA or glow under lavapipe. Standard route views
wait for all chunks while hidden, pause the rider and include city streets, benches and ferns.

- A script error makes Godot wait forever for `world_ready`: always use `timeout`, and grep the
  output for `SCRIPT ERROR` / `SHADER ERROR` — a scene that fails to compile renders nothing.
- Warnings are errors in this project: untyped `Variant` arguments fail to parse; give values a
  typed local first.
- Free-camera shots can end up inside a hillside; prefer `NEAR_MODEL` or the drone camera.
- Very dense scenes are slow under lavapipe; that says little about a real GPU. For frame rate,
  ask the user to check on their M1.

## Sharing results

Look at every image before reporting. For a PR, convert a few to JPEG on the host
(`sips -s format jpeg -s formatOptions 78 in.png --out docs/images/<topic>/name.jpg`, about
150 KB each), commit them, and reference them in the PR body by the commit that adds them:
`https://raw.githubusercontent.com/ORQWA/ORQWA/<commit>/docs/images/<topic>/name.jpg`. A
commit link keeps working after the branch is deleted and after docs refreshes remove images no
doc uses (#79); a branch link does not.
