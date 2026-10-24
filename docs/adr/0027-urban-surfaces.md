# ADR 0027 — Urban ground, sidewalks and building access

- Status: accepted
- Date: 2026-10-21
- Requirements: R105, R106, R112, R131–R135, R89, R101

## Context

Residential land used the meadow shader and commercial land used gravel, so buildings stood
in grass even in dense city centres. Streets lacked sidewalks and buildings had no paths or
forecourts connecting them to their streets. The existing map contains land use, buildings
and roads, but not every entrance or property boundary. The result must work from a prepared
course offline, without another data provider, and leave parks and suburban gardens green.

## Decision

- Generate surfaces in Rust (`orqwa-world::urban`) from the same map inputs. Index
  buildings, streets and paving spatially; sort buildings by map ID for deterministic
  geometry across runs and chunk boundaries.
- Preserve mapped meadows/parks, woods, water, rock, farmland and orchards. Meadows take
  precedence over residential land. Residential areas with building footprints covering
  over 24% of a 50 m neighbourhood get paving; suburban lawns retain grass. Commercial
  land gets mostly asphalt and industrial yards asphalt with gravel.
- Eligible urban roads have 1.8 m sidewalks and 14 cm curbs. Short pieces leave junctions
  clear; bridges, tunnels, tracks and rural stretches get no inferred urban sidewalks.
  Building access lowers the sidewalk and interrupts its curb.
- Infer a street-facing paved frontage and an access path for buildings near a paved
  street. Businesses get broader forecourts and asphalt access. Small homes can have low
  front-garden walls with a broad opening. These represent plausible surroundings, not
  surveyed entrances or cadastral boundaries.
- Cut surfaces to terrain triangles and chunk edges, avoiding flicker. Keep them out of
  buildings, carriageways, railways, water and mapped green spaces. Vegetation uses the
  same paving decisions, keeping grass out of hard surfaces.
- Bake own concrete and staggered-paver textures (albedo, normal, roughness) with
  `art/textures/build.py`, sharing asphalt with roads. The urban shader reads material
  codes from mesh colours; terrain has separate dense-city and commercial cover codes.
  Wet surfaces darken and show puddles with the existing weather settings.
- Godot stays presentation only: `UrbanSurfaces` supplies shared materials and draws the
  mesh; `WorldMeshes` converts the core's mesh arrays.
- City bench candidates recur every 120 m, alternating sidewalks and trying the other side
  or a nearby spot on the block when blocked. Reuse the wooden bench and bin models at the
  sidewalk's height. Require a continuous, nearly level surface for both; leave entrances,
  junctions and the inner walking
  strip open. A complete pair uses the terrain chunk containing its feet, avoiding coarse
  height samples across a boundary. No extra benches on rural stretches, bridges or tunnels.

## Kept worlds

Generator **7**, stored format **2**. Each chunk gains an `urban` mesh and terrain colour
codes gain urban/commercial kinds. Older kept worlds rebuild on the next ride, as required
for an incompatible layout (ADR 0025); course files and their format are unchanged. The new
stored-world fixture records the layout. The bench placement raises the generator from 4
to 5 without changing the stored format; a format-2 world offers regeneration.

### Sidewalk grounding amendment (2026-10-22)

Generator 6 shares offset-edge intersections between adjoining sidewalk strips at bends.
All sidewalk perimeter faces extend slightly below the terrain, following the same terrain
triangle cuts as their tops, so curbs, outer edges and exposed ends have no open underside.
Shared cross-sections use the lower adjoining height at building access; neighbouring
pieces slope into the opening instead of leaving a vertical step between slabs. Bench
height queries use the same profile. Paved street edges use exact segment distances in
urban clearance checks; the padded point samples used to exclude vegetation must not remove
the sidewalk beside its own street. Junctions and protected land remain clear. The stored
format stays at 2; existing kept worlds offer regeneration.

### Rounded footways and curbstones amendment (2026-10-22)

Generator 7 uses the same tangent circles for a junction's asphalt and its sidewalks.
Replace the straight strips between the tangent cross-sections with a curved walking band;
round the outside of mapped street bends too. Convex polygon subtraction trims overlaps
against other carriageways and replacement regions, retaining the clear part of a strip
instead of discarding it. Vegetation's padded junction clearance is separate from these
exact road boundaries. Cache the resulting footprints once for meshes, benches and plants.
Shallow forks keep their own road arm: matching the ridden road checks the line beyond the
meeting point, rather than treating every near-parallel approach as the same road. Allow
tangent points farther along acute forks so their triangular islands get a rounded nose.

The road-facing 18 cm border is a separate surface with a 2 cm bevel over its first 2.5 cm.
Its UVs follow the curb in metres, with 75 cm stone joints in the urban shader; paved walking
surfaces keep their existing material and scale. Normals follow the final beveled and ramped
surfaces. Corner heights interpolate between their streets, including the ridden road's
height, and solid sides still follow the terrain's cuts. Stored format 2 is unchanged.

### Shared sidewalk owner amendment (2026-10-24)

Generator 27 keeps the original mapped feature selected for each ridden road segment's
appearance and its projected position on the matched local map section. A mapped copy of
that section shares the ridden sidewalk and pavement owner, including when several tile
features form one street chain. Ownership is indexed at the projected map position, so a
coarse GPX bend can differ by more than the carriageway width without leaving raw map strips.
The previous one-metre distance
test could alternate between mapped and ridden strips, letting them cut each other into
pieces and leaving duplicate paving on the bank. Identity distinguishes these copies from
independent streets, shallow joining arms and unridden parallel arms of the same map feature.
Superseded map centre lines no longer supply phantom clearance cuts through the new walk.
Temporary source IDs and projected positions are discarded before
storage; stored format remains 3.

### Street-profile footways amendment (2026-10-24)

Generator 27 gives sidewalk patches their owning street's longitudinal profile. Ridden
street endpoints use the drawn road's elevations; mapped streets sample their centre line,
and junction curves sample their road-facing edge. Walking cross-sections stay level rather
than following a bank cut by another nearby road. Reversing a left-side patch reverses its
height profile too, so both sides agree on grades and shared cross-sections.

Solid sides still extend into the terrain, including where part of a corner is buried;
their bottoms stay below their tops rather than creating inverted faces. Lowered access
and entrance ramps share the street datum. Bench and bin queries return the same drawn
walking height. No stored fields change; stored format remains 3.

### Width transitions and partial obstructions amendment (2026-10-24)

Generator 28 carries both endpoint widths through each sidewalk section. The walking band,
shared joins, access offsets and street masks taper together. Clearance uses the existing
drawn carriageway triangles, indexed before urban generation, rather than the conservative
terrain-shaping envelope that can extend wider than a changing road.

Partial building overlaps trim only their occupied polygon area. The same drawn outlines
used by building models and shells are triangulated once, retaining the remaining
walking strip and closing its exposed edges. Submillimetre clipping slivers that cannot
retain reliable GPU winding or slope are discarded before computing surface normals.
This adds no stored fields; stored format remains 3.

### Neighbouring bend clearance amendment (2026-10-24)

Generator 29 bounds each bend's replacement triangle by the original outer miter. For
outer walking radius R and half-turn h, the miter reaches R/cos(h), while the triangle's
far edge reaches reach*cos(h). Use reach = R/cos²(h), with a 2 cm clipping allowance.
The former three-radius triangle could reach a separate hairpin arm, remove its sidewalk
and leave the replacement arc beside the lower street. The smaller geometric bound keeps
that unrelated walking surface continuous without changing the rounded corner itself.
A regression reproduces the gap with two streets 16 m apart. Stored format remains 3.

### Shared corner grades amendment (2026-10-24)

Generator 30 evaluates a curved sidewalk against the height fields of the straight walking
strips at its tangent cross-sections. Every arc section uses the same two parents and its
fraction along the whole corner. The blend matches the complete walking width at either
end, including a sloping miter; sampling the inner edge's ground or just one centre height
can leave a step even when the footprints touch. Ridden and mapped profile endpoints share
their original grade, with mapped endpoint samples interpolated along each short section.
Junction geometry retains street-centre anchors for corners without a surviving parent strip.
Furniture queries use the same field. Profiles and references are temporary; stored format
remains 3. Review from street height as well as above: an intact footprint alone cannot
validate a walking join.

### Continuous islands and connected corners amendment (2026-10-24)

Generator 31 removes overlap between inferred walking footprints before meshing. Offsets
that fold across themselves in a tight inner bend are resolved to their bounded
convex footprint before carriageway clipping, rather than passing a self-crossing polygon
to convex subtraction and triangulation. This also fills the intended walking remainder
where the available island is narrower than a walking strip.

Connected overlap regions use one fitted grade from their adjoining walking profiles over
the complete participating pieces, easing back to neighbouring profiles over one sidewalk
width. Restricting that grade to the intersection leaves a step at the surviving rim.
Every top inside the shared region lies in the same plane;
blending separate tessellated tops left cracks and stacked slabs. Deterministic ownership
keeps a single surface, and exposed edges remain closed. Grades and shared cross-section
lifts are cached during generation, and furniture height queries use the same grade.
Connectivity is established from the original footprints before ownership subtraction;
touching overlap groups share one component. Trimming first can hide connections and give
different planes to adjoining pieces of the same island.
Profiles used by overlaps or crossing chunk boundaries pin their missing endpoint heights
once against the global drawn-ground sampler, before chunks are built. Their straight and
curved pieces therefore retain the same datum even when an anchor falls outside one chunk's
refined height grid. Explicit ridden-road heights remain authoritative.

A curved corner requires walking strips at both tangent approaches. A paved junction alone
cannot create detached sidewalk tiles beside narrow service roads or beyond an urban walk.
Junction arcs join the existing walking footprint; their broad construction wedges do not
delete it. Only outer-bend rounding replaces the old miter, following the owning strips
through shared cross-sections and trimming their connected straight pieces. Neither can
erase a separate curved band or the opposite side of a turning loop. Actual curve overlaps
are clipped to a single graded surface instead.
Every sidewalk is clipped against the carriageway's actual junction fans, including
fans belonging to another nearby corner. Straight arm ribbons alone omit that rounded
asphalt and can leave an obsolete sidewalk rim across it inside a small turning loop.
Two arms of the same continuous mapped street do not get a junction fan: that street
already draws its own bends. Treating the inside of a loop as a separate intersection
adds a second corner through its existing sidewalk. Independent joining streets retain
their fillets.
Regressions include different road widths, reversed loop geometry, a terrain-chunk boundary
and a narrow joining arm. Stored format stays 3.

### Exposed sidewalk sides amendment (2026-10-24)

Generator 32 closes only exposed edges of the combined walking footprint. Side segments
covered by another walking piece are clipped away before extrusion; internal boolean cuts
and material partitions do not become walls through the tread. Partial exposed segments
retain their interpolated absolute top height and still meet the drawn terrain.
Repeated mask vertices and submicrometre edges do not define clipping half-planes: they
could duplicate an outside polygon or leave an internal wall. Position comparisons allow
0.1 mm for the already quantised GPU top edges. Stored format remains 3.

### Minimum connected walking length amendment (2026-10-24)

Generator 33 requires 12 m of connected inferred sidewalk after road, junction and building
clipping and overlap ownership. This removes short isolated remnants without rejecting the
small sections used to construct a long walk. Connectivity uses the surviving convex pieces
and shared walking edges, not the original unclipped road, a shared vertex or proximity.
It is resolved for the whole world before terrain chunking, including curved approaches.

Length follows each strip's longitudinal direction between its walking cross-sections.
Surviving intervals are combined per source strip and component, so partitions across the
width do not multiply the length. Arc sections accumulate along the bend. Each disconnected
remnant qualifies independently; missing parts of its source street add no length.
Building paths, forecourts and garden walls retain their own rules. No course names or
coordinates enter the decision. The temporary component index adds no stored fields;
stored format remains 3.

## Validation and limits

Tests cover raised curbs, clear junctions/carriageways, building access and lowered curbs,
garden walls, parks in dense districts, suburban lawns, vegetation exclusion, and rural/bridge
exclusions. Regression tests cover solid ground contact, continuous access approaches across
chunk boundaries and both sides of sidewalk bends. Bench tests cover regular seating, paired bins, sidewalk height and trying the
opposite side while keeping access clear. Review images compare the same street and overview
on the Bern fixture, with MSAA and bloom disabled in both software-rendered views to avoid
lavapipe artifacts. Native M1 performance remains a manual check (R43).
Additional regressions check a full-width walk around both corners of a T-junction, a rounded
outer bend and the narrow beveled curb border. Review views include Bielersee: Obedüre from
above and road height, using a temporary copy of the course without changing the rider's file.

Only geometry supplied by the current tiles is authoritative. Inferred access and low walls
can differ from the real property. Pedestrian-only spaces, further street furniture and
neighbourhood-specific styles remain separate work.
