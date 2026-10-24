# Screenshots

Current app screens and generated scenery, captured from the application with a simulated
trainer and sample ride history. Open an image for its full size. The phone-remote browser
screens remain in the [remote guide](https://github.com/ORQWA/ORQWA/wiki/Devices-and-riding#remote).

## Riding and training

| Riding | Training |
|---|---|
| ![Chase camera](images/app/ride.jpg) | ![Heart-rate workout](images/workouts/heart-rate-hold.png) |
| ![First-person camera](images/app/ride-first-person.jpg) | ![Structured workout](images/workouts/structured-ride.png) |
| ![Overview camera](images/app/ride-overview.jpg) | ![Planned workouts](images/workouts/planned.png) |
| ![Video course](images/readme/video-course.jpg) | ![Workout editor](images/workouts/editor.png) |
| ![A moment of the ride: a climb record on its card](images/app/ride-moment.png) | |

Video footage: “Le petit Menglonnais” by [Fred Van Gestel](https://cycling.vangestel.online/indoor/videos/france/le-petit-menglonnais/),
[CC BY-NC-SA 4.0](https://creativecommons.org/licenses/by-nc-sa/4.0/). That screenshot has the
same licence; the footage is used only in documentation.

## Courses, setup and analysis

| Before riding | After riding |
|---|---|
| | ![Quick finish summary](images/app/summary-quick.png) |
| ![Course library](images/app/courses.png) | ![Ride summary](images/app/summary.png) |
| ![Course detail](images/app/course.png) | ![Ride history](images/app/history.png) |
| ![Connected devices](images/issues/devices-readings.png) | ![Power curve and profile](images/app/analysis.png) |
| ![Rider profile](images/app/profile.png) | ![Fitness and fatigue](images/app/analysis-fitness.png) |
| ![HUD editor](images/app/hud-editor.jpg) | ![Weekly training](images/app/analysis-weekly.png) |
| ![Graphics settings](images/app/settings-graphics.png) | ![Progress over time](images/app/analysis-progress.png) |

## Cities and woodland

The same woodland view on Low and Ultra graphics.
Change quality in [Settings](https://github.com/ORQWA/ORQWA/wiki/Settings#graphics-and-full-screen).

| Low | Ultra |
|---|---|
| ![Fewer nearby trees](images/views/forest-low.jpg) | ![Dense woods drawn farther](images/views/forest-ultra.jpg) |

| City streets | Woodland |
|---|---|
| ![City paving and sidewalks](images/views/city-street.jpg) | ![Wooded hairpin](images/views/hairpin-chase.jpg) |
| ![City overview](images/views/city-overview.jpg) | ![Forest floor and ferns](images/views/forest-floor.jpg) |
| ![City bench](images/views/city-benches.jpg) | ![Hairpin overview](images/views/hairpin-drone.jpg) |

![Grounded sidewalk edges and continuous access approaches](images/issues/sidewalks-grounded.jpg)

| Curved corners on Bielersee: Obedüre | Curbs from road height |
|---|---|
| ![Rounded sidewalk junction](images/issues/sidewalk-curves-overview.jpg) | ![Beveled curb-stone border](images/issues/sidewalk-curves-road.jpg) |

## Rocks and cliffs

| Cliff overview | Closer rock faces |
|---|---|
| ![Fractured cliff](images/issues/rock-cliff-overview.jpg) | ![Rock faces on a wooded slope](images/issues/rock-cliff-close.jpg) |

Repeat with `VIEWS="rock-cliff-overview rock-cliff-close"` in `scripts/render-views.sh`.
See the [boulder gallery](../art/README.md#vegetation).

## Standard route views

### Andermatt sidewalks

Generators 27–33 give sidewalk copies one owner using the local mapped section matched to
the ride. Coarse GPX bends no longer leave duplicate footways or phantom clearance cuts;
independent nearby streets and unridden parallel arms of the same map feature remain.
Walking tops follow their owning street's height and grade instead of draping down a bank
cut by another road. Closed sides reach terrain, and entrances and furniture share the
walking profile.
Road-width changes taper continuously. Partial building overlaps remove only the occupied
area, and rounding a bend no longer uses a replacement area large enough to cut the
sidewalk of the neighbouring street.
Curved joins share the adjoining straight walking surfaces' grades across the whole width;
matching footprints no longer conceal a height step at the tangent cross-section.
Overlapping walking pieces share one surface and grade. Continuous mapped streets keep
their own bend geometry rather than receiving another junction fan inside their loops;
narrow service arms cannot create isolated corner tiles without walking approaches.
Side walls close only the exposed walking perimeter; internal clipping seams no longer
extrude through the tread. Repeated boolean-cut vertices cannot duplicate paving.
Inferred raised sidewalks now require at least 12 m of connected walking length after
clipping. Short corner remnants disappear; small construction sections that join into a
long walk qualify together, including around bends and across terrain chunks.

These matching overview cameras use the full saved Alpenbrevet Gold inputs at
209.36 km to go (about 1.38 km from the start), near Andermatt. Nearby geometry alone is
sent to the renderer; the complete route determines terrain shaping and projection.
Buildings use their generated shells for context; vegetation is omitted.
Generic regression fixtures vary road width, loop direction and terrain-chunk placement;
they check coverage, walking height on both sides of internal joins and exposed side walls.
The fixed village, lakeside junction, city-bench and Oberalp hairpin/tunnel views below were
also regenerated and reviewed with the correction.
The village, junction, city-bench and Oberalp hairpin views were refreshed again for the
minimum-length rule; the long city walk and its bench/bin placement remain intact.

| Before | Continuous sidewalk at street height |
|---|---|
| ![Sidewalk strips draped down the Andermatt road bank](images/issues/andermatt-sidewalk-before.jpg) | ![Continuous walking surface follows the elevated street](images/issues/andermatt-sidewalk-after.jpg) |
| ![Fragmented walking edge down the bank](images/issues/andermatt-sidewalk-edge-before.jpg) | ![Level walking edge with its closed side reaching terrain](images/issues/andermatt-sidewalk-edge-after.jpg) |
| ![Step where the straight walking strip meets its curve](images/issues/andermatt-sidewalk-step-before.jpg) | ![Continuous grade across the straight and curved walking surfaces](images/issues/andermatt-sidewalk-step-after.jpg) |
| ![Stacked paving and internal channels in a turning island](images/issues/andermatt-sidewalk-island-before.jpg) | ![Joined turning-island paving without internal channels](images/issues/andermatt-sidewalk-island-after.jpg) |

![Junction and adjoining sidewalks after the continuity corrections](images/issues/andermatt-sidewalk-junction-after.jpg)

The following matching cameras use the same complete saved course at **116.93 km to go**
(93.81 km from the start). The short raised stubs beside the street are removed, while the
long sidewalk and separate building-front paving remain. Buildings use generated shells;
vegetation is omitted as in the Andermatt views above.

| Short sidewalk fragments | Minimum connected walking length |
|---|---|
| ![Short raised sidewalk stubs along a village street](images/issues/sidewalk-minimum-before.jpg) | ![Short stubs omitted and the continuous village sidewalk retained](images/issues/sidewalk-minimum-after.jpg) |

### Bridge ownership

The Alpenbrevet Lukmanier approach (approximately 34.8 km into the saved course) exposed a
second mapped bridge beneath the bridge ridden. Generator 13 gives the aligned bridge one
owner even when GPX smoothing shifts its centre line slightly; separate bridges remain.

| Duplicate mapped structure | One bridge |
|---|---|
| ![Overlapping decks and railings](images/issues/alpenbrevet-bridge-duplicate-before.jpg) | ![One deck and railing set](images/issues/alpenbrevet-bridge-duplicate-after.jpg) |

The separate tunnel-to-bridge case is at **174.17 km to go**, approximately 36.57 km into
the full course. Generator 15 owns the complete aligned bridge despite the slight difference
between its mapped and resampled tag boundaries. Focused geometry renders below use the same
camera above and beside the transition, with vegetation and buildings omitted.

| Extra approach deck | Continuous approach |
|---|---|
| ![Raised slab and duplicate railing at the tunnel exit](images/issues/alpenbrevet-bridge-transition-before.jpg) | ![Single road surface leading onto the bridge](images/issues/alpenbrevet-bridge-transition-after.jpg) |

### Railway approaches

Alpenbrevet Gold at 102.99 and 98.74 km to go (107.75 and 112.00 km into the full course).
Generator 17 keeps one clearance across the whole crossing instead of forcing adjacent
bridge and ground samples to incompatible heights. Independently elevated tracks also stop
copying the heights of nearby tracks at another level. These focused geometry comparisons
share cameras, saved course inputs and application materials, with vegetation/buildings omitted.

| Abrupt ramp | Gentle bridge approach |
|---|---|
| ![Railway rises abruptly above the road](images/issues/alpenbrevet-rail-ramp-before.jpg) | ![Railway approaches the overpass continuously](images/issues/alpenbrevet-rail-ramp-after.jpg) |
| ![Second crossing with conflicting height samples](images/issues/alpenbrevet-rail-second-before.jpg) | ![Second crossing with consistent clearance](images/issues/alpenbrevet-rail-second-after.jpg) |

### Support foundations

Generator 16 grounds piers and bearing walls on the completed terrain triangles across the
whole footing. The controlled Rust-generated regression contains a narrow DEM ridge absent
from the drawn grid; the old pier stopped 47.5 m above its visible ground. Cameras, terrain
and application materials are identical in the comparison.

| Floating pier | Foundation meets drawn ground |
|---|---|
| ![Pier ends above the terrain](images/issues/bridge-footing-before.jpg) | ![Pier reaches the drawn terrain](images/issues/bridge-footing-after.jpg) |

The full saved Alpenbrevet course was also checked at the Furka bridge near 24.37 km to go
(186.37 km from the start), including bearing walls at its ends. This focused geometry view
omits vegetation and buildings:

![Furka bridge foundations](images/issues/alpenbrevet-bridge-footing.jpg)

### Tunnel approaches

Alpenbrevet Gold's Lukmanier approach at 34.97 km and the reverse view at 36.38 km.
These focused renders use the full saved course inputs, generated terrain and road meshes,
and the app's materials; vegetation and buildings are omitted to expose the obstruction.
Generator 14 cuts approach paving, rounded ends and junctions to the portal opening as well
as cutting the terrain. Cameras and projection remain identical in each pair.

| Approach before | Approach after |
|---|---|
| ![Paving closes the Lukmanier mouth](images/issues/alpenbrevet-tunnel-before.jpg) | ![Open mouth with the road continuing inside](images/issues/alpenbrevet-tunnel-after.jpg) |
| ![Reverse view of the asphalt wall](images/issues/alpenbrevet-tunnel-exit-before.jpg) | ![Reverse view through the clear portal](images/issues/alpenbrevet-tunnel-exit-after.jpg) |

### Local traffic lanes

The rider follows the local traffic side and the drawn carriageway width (R120), including
on narrow cycleways. Ghosts stay toward the centre on the same side. The map and recorded
route retain their original centre line.

| Right-hand traffic: Oberalp | Left-hand traffic: narrow London road |
|---|---|
| ![Rider in the right-hand tunnel lane](images/traffic/right-tunnel.jpg) | ![Rider keeping left on a narrow road](images/traffic/left-narrow-road.jpg) |

[Tunnel before the fix](images/traffic/right-tunnel-before.jpg) ·
[London street](images/traffic/left-street.jpg) ·
[First person](images/traffic/left-first-person.jpg) ·
[Drone](images/traffic/left-drone.jpg)

Reproduce the London views in the dev container with
`GPX=core/fixtures/london-cycleway.gpx SHOTS="london-left-chase:120:0 london-left-first-person:120:1 london-left-drone:120:2 london-left-narrow:600:0" scripts/render-views.sh`.
The Swiss comparisons use `VIEWS="village-chase hairpin-chase oberalp-tunnel-chase oberalp-hairpins-drone"`.
The London review checks an offset of 1.5 m on the drawn 6 m carriageway at 120 m and
0.875 m on the drawn 3.5 m carriageway at 600 m; both offsets are left of travel.

### Guardrails beside lower hairpin arms

Generator 26 fits guardrail endpoint heights to their owning carriageway, retaining the
existing lateral clearance. Buried post feet meet the road edge/skirt; the lower bank no
longer dictates blade height. Curved spans and chunk joins share their fitted endpoints.

This matching close camera uses the saved Alpenbrevet Gold route at 30.64 km to go.
Generator 25 has already removed the duplicate asphalt in both images, making the remaining
low guardrail visible separately. After generator 26 the blade follows the upper wearing
surface and the lower road remains clear. Vegetation and buildings are omitted.

| Before | After |
|---|---|
| ![Guardrail below the upper hairpin's asphalt](images/issues/alpenbrevet-hairpin-guard-before.jpg) | ![Guardrail anchored beside its owning carriageway](images/issues/alpenbrevet-hairpin-guard-after.jpg) |

### Duplicate terrain-draped streets

Generator 25 gives a close, aligned mapped copy of the ridden carriageway the same pavement
owner, using the original map feature matched for road appearance. Small GPX/map offsets
no longer leave duplicate asphalt, edge paint or lane markings on the bank below it.
Shallow joining roads, crossings and independent parallel ways retain their pavement.

These identical cameras use the full saved Alpenbrevet Gold inputs at 30.64 km to go
(180.10 km from the start), looking at the upper switchback beside the rider. Buildings and
vegetation are omitted. Within 150 m of that position, the duplicate mapped paving falls
from 3,608 triangles to zero; this includes 118 asphalt faces steeper than 45 degrees.
The second pair isolates a two-metre map/GPX offset on a steep bank: the extra strip and
its painted edge disappear while the ridden road remains.

| Before | After |
|---|---|
| ![Duplicate asphalt on the Alpenbrevet switchback bank](images/issues/alpenbrevet-terrain-street-before.jpg) | ![Single ridden road with the terrain bank exposed](images/issues/alpenbrevet-terrain-street-after.jpg) |
| ![Offset mapped road adds another painted asphalt strip](images/issues/offset-terrain-street-before.jpg) | ![One carriageway with its original verge](images/issues/offset-terrain-street-after.jpg) |

### General road clearance for bridge and roadside fences

Generator 24 checks complete barrier footprints against the actual generated carriageways
and junction fills. Rails, posts, kerbs and parapets cannot cut a road even at a parallel
join or between clear span endpoints. Low slab edges also recede below joining pavement,
keeping their closed sides and undersides. Independent higher bridges retain protection.
Roadside guardrail and fence models use the same clearance rule.

The actual Alpenbrevet start/finish junction is shown around 0.33 km from the start
(210.41 km to go). Generator 23 had removed the duplicate return bridge, but a single fence
still followed the GPX across the joining carriageway. These matching close cameras show
the general clearance correction. Buildings and vegetation are omitted.
The second pair is an independent mapped bridge with a parallel road joining one side,
using no roundabout detection or course-specific conditions.

| Before | After |
|---|---|
| ![Fence dividing the joining carriageways](images/issues/alpenbrevet-pavement-fence-before.jpg) | ![Open carriageways with protection outside them](images/issues/alpenbrevet-pavement-fence-after.jpg) |
| ![Parallel joining road blocked by a mapped bridge fence](images/issues/bridge-parallel-join-before.jpg) | ![Parallel joining pavement open with the opposite bridge edge protected](images/issues/bridge-parallel-join-after.jpg) |

### Protection through curves

Generator 22 fits each guardrail span between shared road-edge endpoints rather than placing
straight models on centre-line tangents. Bridge sections and post directions follow the
wearing surface's curve. The controlled 12 m hairpin shows both the inside overlaps and
outside gaps; the actual Alpenbrevet start/finish loop checks the curved bridge beside its
roundabout, around 0.33 km from the start (210.41 km to go). Matched cameras use the real
materials and prop instances; buildings and vegetation are omitted.

| Before | After |
|---|---|
| ![Separated and overlapping guardrail blades](images/issues/guardrails-tight-curve-before.jpg) | ![Joined blades on both edges](images/issues/guardrails-tight-curve-after.jpg) |
| ![Start-loop bridge protection before](images/issues/alpenbrevet-start-loop-before.jpg) | ![Protection following the curved bridge](images/issues/alpenbrevet-start-loop-after.jpg) |

### Roadside protection beside a lower railway

Alpenbrevet Gold at 33.52 km to go (177.22 km from the start), using full saved inputs and
the actual guardrail and post instances. Generator 21 preserves the road's verge datum
beside the lower railway, so its protection meets the ground at carriageway height.
Buildings and vegetation are omitted; the cameras are identical.

| Before | After |
|---|---|
| ![Guardrails below the road](images/issues/alpenbrevet-low-guardrails-before.jpg) | ![Grounded protection at the road edge](images/issues/alpenbrevet-low-guardrails-after.jpg) |

### Tunnel interior and roof

Full saved Alpenbrevet Gold inputs around 175.72 km to go (35.02 km from the start), using
identical cameras before and after generator 20. The complete tube is now clear of terrain,
including thin strips at floor level, and the mapped approach no longer paints its roof.
These focused geometry views omit vegetation and buildings.

| View | Before | After |
|---|---|---|
| Inside | ![Rock blocks the tunnel interior](images/issues/alpenbrevet-tunnel-interior-before.jpg) | ![Clear tunnel interior](images/issues/alpenbrevet-tunnel-interior-after.jpg) |
| Approach | ![Paving above the tunnel mouth](images/issues/alpenbrevet-tunnel-roof-before.jpg) | ![Portal hill without duplicate paving](images/issues/alpenbrevet-tunnel-roof-after.jpg) |

### Duplicate bridge decks on ground-tagged course sections

Full saved Alpenbrevet Gold inputs, with identical cameras before and after generator 19.
The village bridge is near 32.05 km from the start; the rider's 178.51 km-to-go screenshot
looks back toward it. The other comparisons cover the 68.85 and 67.26 km-to-go reports.
These focused geometry views omit vegetation and buildings.

| Location | Before | After |
|---|---|---|
| Village | ![Skewed village bridge](images/issues/alpenbrevet-village-deck-before.jpg) | ![Single village road surface](images/issues/alpenbrevet-village-deck-after.jpg) |
| 68.85 km to go | ![Raised bridge across the road](images/issues/alpenbrevet-ground-deck-before.jpg) | ![Unobstructed ridden pavement](images/issues/alpenbrevet-ground-deck-after.jpg) |
| 67.26 km to go | ![Second raised bridge approach](images/issues/alpenbrevet-second-deck-before.jpg) | ![Continuous road at the second bridge](images/issues/alpenbrevet-second-deck-after.jpg) |

### Bridge junctions

Controlled offline world-generator fixture: a mapped bridge with terrain endpoints 1.5 m
above the ridden road. Its connected end now meets the road without a step or railing
across it; the approach bank follows the deck. Separate overpasses keep their protection.

| Before | After |
|---|---|
| ![Raised bridge end blocks the junction](images/issues/bridge-junction-before.jpg) | ![Bridge end meets the road with open protection](images/issues/bridge-junction-after.jpg) |

### Road profiles

Road hierarchy and paving (R119, [ADR 0031](adr/0031-road-appearance.md)) use the same
materials for ridden and surrounding ways. This controlled, offline scene uses the Rust
world generator; widths and lane counts are illustrative estimates, not a mapped location:

![Ridden cycleway, motorway carriageway and ramp, primary road, street, service road, track and path](images/roads/profiles.jpg)

In the dev container, reproduce it with
`cargo test --manifest-path core/Cargo.toml -p orqwa-world road_profiles_render_fixture -- --ignored`,
then `OUT=/workspaces/orqwa/screenshots/road-profiles.png xvfb-run -a godot --path app --resolution 1600x1000 -s res://tools/render_roads.gd`.

![Road hierarchy on a prepared real route](images/roads/road-types.jpg)

The sample course's shallow junctions near Lamboing retain their joining segments and open
the shoulder at the road edge:

![Joined roads near Lamboing](images/roads/lamboing-shortcut-after.jpg)

Reproduce with `COURSE=core/orqwa-gd/courses/twann-chasseral.tqc`
and `SHOTS="lamboing:4200:2:0,180,-20"` in `scripts/render-views.sh`. The course uses its
saved inputs offline. Matching before/after review images are in `docs/images/roads/`.

The Oberalp railway bridge (R122), seen from the same riding camera before and after the
proportion and material changes. The close views check open railings, supports and the soffit.

| Before: broad deck with solid walls | After: narrower deck and open railings |
|---|---|
| ![Previous Oberalp railway bridge](images/issues/oberalp-bridge-before.jpg) | ![Revised Oberalp railway bridge](images/views/oberalp-railway-chase.jpg) |

| Approach | From below |
|---|---|
| ![Railway bridge close up](images/views/oberalp-bridge-close.jpg) | ![Railway bridge underside](images/views/oberalp-bridge-under.jpg) |

![Railway bridge from the side](images/views/oberalp-bridge-side.jpg)

The non-ridden bridge regression scene checks the deck from underneath and above (R121).
It uses generated test geometry with the application's road, street and terrain materials.

| Before: sky visible through the deck | After: solid underside |
|---|---|
| ![Missing bridge underside](images/issues/bridge-underside-before.jpg) | ![Solid bridge underside](images/issues/bridge-underside-after.jpg) |

![Bridge deck from above](images/issues/bridge-deck-above.jpg)

The [river geometry correction](adr/0030-river-levels.md) is checked against the
Alpenbrevet Gold course near 104.57 km remaining and farther upstream:

![Alpenbrevet river surface following the valley](images/issues/alpenbrevet-river-after.jpg)

The fixture routes exercise villages, climbs, junctions, bridges, tunnels and weather. Their
fixed camera positions make it possible to compare future changes. These are the current
renders; historical before/after images remain with their original reviews.

| Villages and roads | Lake and junction views |
|---|---|
| ![Village](images/views/village-chase.jpg) | ![Lakeside road](images/views/lake-chase.jpg) |
| ![Climb](images/views/climb-chase.jpg) | ![Lake overview](images/views/lake-drone.jpg) |
| ![Evening village](images/views/village-evening.jpg) | ![Rain](images/views/lake-rain.jpg) |
| ![Morning climb](images/views/climb-morning-drone.jpg) | ![Morning lake](images/views/lake-morning.jpg) |
| ![Morning valley](images/views/valley-morning.jpg) | ![Lakeside village](images/views/lakeside-village.jpg) |
| ![Shops](images/views/shops-side.jpg) | ![Building block](images/views/block-chase.jpg) |
| ![Junction](images/views/junction-chase.jpg) | ![Junction overview](images/views/junction-drone.jpg) |
| ![Railway tunnel](images/views/railway-tunnel-drone.jpg) | |

| Bern | Oberalp |
|---|---|
| ![River](images/views/river-drone.jpg) | ![Road tunnel](images/views/oberalp-tunnel-chase.jpg) |
| ![Old town](images/views/oldtown-drone.jpg) | ![Railway](images/views/oberalp-railway-chase.jpg) |
| ![Bridge](images/views/bridge-chase.jpg) | ![Mountain hairpins](images/views/oberalp-hairpins-drone.jpg) |
| ![Roundabout](images/views/roundabout-drone.jpg) | ![Alpine lake](images/views/oberalp-lake-drone.jpg) |
| ![Bridge from the side](images/views/bridge-side.jpg) | ![Tunnel portal](images/views/oberalp-portal-drone.jpg) |
| | ![Mountain village](images/views/oberalp-village-chase.jpg) |

Every building, plant, roadside prop and rider also has a refreshed image in the
[model galleries](../art/README.md#gallery). See [Development](development.md#refreshing-screenshots)
for the capture and review workflow.
