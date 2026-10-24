# ADR 0025 — Kept worlds: a course's 3D world kept on the computer that built it

- Status: accepted
- Date: 2026-10-20

## Context

**Ride** on a course page waits while the course's 3D world is built (#237): format 1 course
files hold the inputs only, and every ride builds the world from them (ADR 0007). Measured on an
M5 Pro (18 cores, in the dev container, optimised build), the world alone:

| Course | Length | Chunks | Built in | Stored | Deflated | Read back |
|---|---|---|---|---|---|---|
| Twann – Chasseral | 20 km | 273 | 2.2 s | 238 MB | 103 MB | 0.05 s |
| Bielersee | 56 km | 793 | 10 s | 1.3 GB | 529 MB | 0.26 s |
| Weissenstein – Balmberg | 103 km | 1343 | 18–24 s | 2.7 GB | 1.1 GB | 0.46 s |
| Alpenbrevet Gold | 211 km | 2663 | 25–34 s | 2.8 GB | 1.2 GB | 0.60 s |

A built world is 10–40 times its course file, far beyond the "few hundred MB for long routes"
ADR 0007 expected, and slower computers build for longer. Options considered:

- **Build while the course's page is open** (ADR 0007's amendment of 2026-10-15): small, but
  hides only as much of the wait as the rider spends on the page, and builds for every course
  merely looked at.
- **Ride at once and stream the world in along the route**: no wait at all, on every computer,
  but `orqwa_world::generate` is one job today, and the parts built whole (roads, portals,
  bridges) would still come first.
- **Store the world in the course file** (format 2): shared files ten times larger, and a
  shared world goes stale with each new generator.
- **Keep the world on the computer, opt-in**: no wait from the second ride on (the first, for a
  course prepared from GPX); costs disk only for riders who ask for it.

## Decision

A setting of the computer (`keep_worlds` in `computer.toml`, R78), **off until turned on**,
keeps the 3D world of a library course once it is built (R101):

- **Where.** `<cache>/worlds/<fingerprint>.world`: in the cache directory, never in the course
  file nor the data directory, which may sync (R30). Course files and their format stay as they
  are.
- **Across updates.** A kept world stays; two numbers at the head of every world tell what
  became of it. Releases mostly leave the generator alone (four of the five before this one
  did), so dropping every world with each release would make riders wait for nothing.
  - `orqwa_world::stored::FORMAT`: what a kept world needs to be **read and drawn**. It is
    raised when the stored layout changes (a test reads a sample stored in
    `core/orqwa-world/tests/data/stored-world-<FORMAT>.bin` and fails until it is) or what the
    app makes of the data (a model renamed or removed, another instance buffer layout). A world
    of another format is not read: **Ride** builds and keeps it again. As a guard for a change
    nobody raised the format for, the app leaves out models it no longer has.
  - `orqwa_world::GENERATOR`: what worlds **look like**. It is raised when a change makes the
    same course come out differently. A world of an older generator is ridden as it is, and its
    course page offers **Regenerate world**, which builds and keeps it with the current one. A
    change nobody raised it for only goes unoffered.
  Both are raised by hand, by the change that needs it (CLAUDE.md). A developer changing the
  generator turns the setting off while working on it, or rides worlds kept before the change.
- **Which course.** The fingerprint hashes the course file's track and data entries by the
  checksums and sizes in its zip directory, nothing unpacked; the manifest and the map picture
  are left out, so renaming a course or finding its countries keeps its world.
- **Format.** One zip per world: `whole` (road, structures, railways, horizon) and
  `chunks/<n>`, each deflated at level 1 through the `zip` crate the course files use, so no
  new dependency. Inside, `orqwa_world::stored` writes the types field after field,
  little-endian, after a magic, `FORMAT` and `GENERATOR`. Level 1 shrinks a world 2.4 times;
  reading inflates the chunks on all cores.
- **When.** Kept after a GPX is prepared into a course (the world is built for it anyway),
  after a library course's world is built for a ride, and at once when the setting is turned on
  with a course loaded. Writing runs in the background, about 8 s for a 100 km course.
- **Read.** **Ride** looks for the course's world before building it; a missing, damaged or
  unreadable one is built (and kept) again, which the rider sees only as the usual wait.
- **Gone.** Deleting a course, or its disappearing from the library (also deleted on another
  computer), deletes its world when the worlds are tidied, at start and after keeping one, as
  are worlds this ORQWA cannot read; turning the setting off deletes all.

## Consequences

Road appearance (R119, [ADR 0031](0031-road-appearance.md)) raises the generator to 9 and
stored format to 3: road/street vertex colours now carry width, surface and marking data.
Older kept worlds rebuild from their saved course inputs, with no new network requests.

- A ride on a kept course starts within a second or two instead of after tens of seconds.
- Riders who keep worlds give about 10 MB per kilometre of route; the settings show the total.
  There is no cap: the rider chooses which courses stay in the library.
- Updates cost no waits unless they change the format; when they change the look, the rider
  chooses which courses to build again and when. Nothing is built in the background for courses
  not ridden.
- Kept worlds are only as current as the two numbers: a format change nobody raised `FORMAT`
  for may misdraw a kept world (models it names that are gone are left out), and
  **Regenerate world** is offered only once `GENERATOR` is raised.
- The first ride of a course imported as a file still waits, as does every ride for riders who
  keep no worlds; streaming the world in remains possible later (#237).
- The world in memory is as large as the stored one before deflating (the app holds 2.7 GB for
  a 100 km course), unchanged by this decision.

## Amendment — 2026-10-21

[Urban surfaces (ADR 0027)](0027-urban-surfaces.md) add a mesh to each chunk and new terrain
cover codes, raising the stored format to 2 and generator to 5 (including city benches).
Format-1 worlds rebuild when next ridden; the course-file format is unchanged.

## Amendment — 2026-10-22

[River levels (ADR 0030)](0030-river-levels.md) raise the generator to 8. The stored format
stays at 2; older kept worlds remain rideable and offer regeneration for the corrected rivers.

## Amendment — 2026-10-23

Solid undersides for non-ridden road bridges (R121) raise the generator to 11. The added
vertices and triangles use the existing mesh layout, so stored format stays at 3. Older kept
worlds remain rideable and offer regeneration to close the decks seen from below.

The bridge restyle (R122) raises the generator to 12 for narrower railway decks, open
railings, capped columns and bearing walls. Stored format remains 3. Wearing surfaces retain
the existing road-profile metadata;
new bridge details carry sRGB tints with zero alpha, outside the positive road-style flag
range. No vertex field or model name changes. Two-sided road/rail surface rendering
and textured structure materials apply to older worlds immediately; regeneration adds
the new geometry.

Generator 13 removes duplicate mapped decks under ridden bridges whose GPX and map centre
lines differ slightly. The ownership change leaves ground-junction coverage and separate
crossing or parallel bridges intact; stored format remains 3.

Generator 14 cuts draped ground paving to tunnel openings too, including approach end caps
and junctions, and suppresses aligned mapped roads over the ridden tunnel. Previously only
the terrain mesh was hollowed out, leaving road-textured ramps across some portals. Stored
format remains 3; regenerated worlds receive the corrected geometry.

Generator 15 extends bridge ownership across the complete aligned mapped feature. This removes
short duplicate slabs at tunnel-to-bridge transitions where map nodes and resampled structure
boundaries differ. A bridge leaving the ridden corridor retains its independent sections.
Stored format stays at 3.

Generator 16 grounds bridge supports against the completed terrain mesh rather than raw DEM
samples or pre-carving helper heights. Structures are built after chunks; pier and bearing
foundations use the lowest triangle intersection across their full footprint, including
channels and steep banks. Mapped-road piers use the same footprint calculation in their chunk.
Stored format remains 3: only generated geometry and build order change.

Generator 17 gives each continuous railway/road crossing one overpass, underpass or level
clearance, including adjoining samples whose structure tags differ. Parallel-track coupling
also rejects vertically separated lines. This removes conflicting elevation pins and the
resulting railway ramps; stored format remains 3.

Generator 18 joins mapped bridge endpoints to the ridden ground road when their height
difference is within 3 m. Kerbs, parapets and steel railings leave openings at ground-street
connections within 0.5 m of deck level. Exact short street segments define these openings
and support clearance, preserving the opposite edge and grade-separated crossings. Slab
sides and undersides remain closed. Connected profiles also lower their approach banks before
chunk generation, preventing the adjusted pavement from being buried. Stored format remains 3.

Generator 19 assigns a complete aligned mapped bridge to the ridden pavement even when the
route importer retained Ground tags rather than a short or DEM-unconfirmed bridge. The mapped
feature no longer adds a raw-DEM slab above that pavement. A tunnel alone cannot claim a
surface bridge above it; crossing, parallel and off-route bridges retain their geometry.
Stored format remains 3.

Generator 20 clips surrounding terrain and draped paving along the complete tunnel volume,
using the same road and bundled railway sections as its visible tube. Mouth cuts still
extend beyond the ring. Aligned mapped ground approaches overlapping a ridden tunnel belong
to that route's pavement, preventing duplicate approach ribbons from painting its roof.
Interior cuts extend below the tube floor. Compressed railway underpasses retain their road
embankment cover, and distinct surface crossings above remain. Stored format remains 3.

Generator 21 gives the ridden ground road priority over railway terrain shaping inside its
width-adjusted verge. A nearby lower track can no longer pull down the shoulder and the
protection placed on it. The bank blends between the protected verge and the separate railway
bed rather than dropping at a hard boundary. Railway shaping still applies under bridges;
track elevations are unchanged. Protection crossing its station's chunk boundary uses the
neighbour's final terrain for its footing height instead of the owner's coarse border samples.
Drop detection probes beyond the shaped shoulder to keep continuous cliff protection.
Stored format remains 3.

Generator 22 fits guardrail blades between shared endpoints on the width-adjusted road edge,
using the drawn ribbon's interpolated cross-section rather than a centre-line tangent.
Longitudinal scaling closes outside gaps and inside overlaps without tilting posts. Endpoint
heights also agree across terrain chunk boundaries. Stored format remains 3.
Bridge cross-sections use the same averaged tangents as the pavement, including at their
ends, and bridge post directions interpolate along the curve. This keeps protection outside
the carriageway and the slab beneath the wearing surface on the start/finish loop.

Generator 23 excludes bridge segments already owned by an earlier traversal of the course
from structure generation, as pavement generation already does. Slightly different outbound
and return GPX curves no longer create two decks and crossing sets of fences at a shared
roundabout approach. It also treats each mapped roundabout's filled outer road boundary as a junction area
for protection. Ground/bridge boundary mismatches at the same level share the earlier
pavement owner; short differing ends within the pavement overlap margin share the complete
earlier bridge, rather than producing fence stubs.
Bridge tags persisting through a GPX turn no longer create a fence
inside that area, even when the mapped ring runs parallel to the local GPX tangent. Kerbs,
parapets and steel rails share the exclusion. Outside protection and overpasses more than
3 m above the junction remain; underground service loops do not suppress surface protection.
Stored format remains 3.

Generator 24 checks generated barrier footprints against the completed carriageway triangles,
including mapped streets, tracks and junction fans. Exact polygon clipping checks the whole
span, thickness and interpolated elevation, so clear endpoints or parallel headings cannot
hide a road crossing. Ephemeral triangle ranges identify steel rails/posts and concrete
kerbs/parapets separately from slab sides, undersides and supports. Conflicting pieces are
removed and meshes compacted; roadside guardrail and fence instances use the same index.
Exposed slab sides, shoulders and end edges recede below same-level joining pavement
instead of leaving a low concrete ridge or removing the closed side of the slab.
The mapped deck's narrow kerb is excluded from its own carriageway, and shore gravel is not
a road. Independent levels retain their protection. No temporary metadata enters kept worlds;
stored format remains 3.

Generator 25 retains the matched map feature alongside each ridden road segment's appearance
during generation. A close, aligned copy of that same feature belongs to the ridden pavement
despite small GPX/map offsets; it no longer adds asphalt and markings to the shaped bank.
Source identity distinguishes that copy from a shallow joining street or a wider highway
beside a narrow ridden cycleway. Original map feature IDs survive street-chain assembly,
and rounded caps and shoulder openings use the same ownership test. These IDs are temporary;
stored format remains 3.

Generator 26 anchors guardrail span endpoints to their owning carriageway's elevation minus
the 15 cm shoulder sink. Buried post feet meet the existing road edge/skirt instead of moving
the blade down to a bank cut by a lower hairpin arm. Lateral clearance, upright posts and
shared curved endpoints remain; their fitted heights no longer depend on terrain chunks.
Final-ground corrections still apply to ground-mounted snow poles and delineators.
Terrain shaping and the lower carriageway are unchanged. Stored format remains 3.

Generator 27 matches routes to roundabouts the way traffic goes round them (R126), so a
planner's file that skips the ring no longer takes the short way against the traffic. The
ridden road of a kept world follows the old route there; regeneration redraws it. Stored
format remains 3.

## Amendment — 2026-10-24: one world at a time

The settings' Courses tab lists each kept world by its course's name with its size in MB or
GB (powers of 1000), their total in a last row, and a bin that deletes that world alone (R136,
#355); before, a rider could free room only by deleting a course or every kept world at once.
It still sets no cap. A world is found by its course's fingerprint, so a course in the library
twice shows its one world once, under both names, and deleting it deletes it for both. The
course stays; its next ride builds and keeps the world again, so deleting one asks nothing. A
world deleted while it is still being written is kept all the same once written.
