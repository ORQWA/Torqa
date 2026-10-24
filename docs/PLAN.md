# ORQWA — Architecture & Plan

See [REQUIREMENTS.md](REQUIREMENTS.md) for requirement IDs and [adr/](adr/) for decisions.

## Architecture

**Godot 4** (presentation, statically typed GDScript) + **Rust core** via GDExtension
([godot-rust/gdext](https://github.com/godot-rust/gdext)). All logic lives in Rust; Godot only
renders and presents. Rationale in [ADR 0001](adr/0001-stack-godot-rust.md).

```
core/                      Rust workspace (tokio + tracing)
  orqwa-domain/            newtype units (SI internally), profiles, plugin traits
  orqwa-physics/           speed integration, grade scaling, descent modes, virtual gears
  orqwa-devices/           btleplug FTMS + Tacx FE-C over BLE + sensors/shifters, fake trainer
  orqwa-routes/            GPX import, snapping to the map's roads, smoothing, DEM correction,
                           climb detection
  orqwa-terrain/           elevation tiles (Mapterhorn, AWS fallback), disk cache, height lookup
  orqwa-osm/               OpenStreetMap features from OpenFreeMap vector tiles, tile cache
  orqwa-world/             terrain chunks, land cover, road mesh, streets, railways, bridges and
                           tunnels, water, buildings, vegetation, the minimap
  orqwa-session/           ride loop (10–20 Hz), metrics (NP/TSS/zones, power curve), ghosts,
                           workouts (ERG, heart-rate hold, structured), recording
  orqwa-workouts/          workout files (ZWO, ERG/MRC, FIT), built-in workouts, the library
  orqwa-video/             FFmpeg decoding, GoPro GPMF GPS, Incyclist and Tacx RLV (ADR 0010)
  orqwa-storage/           FIT files, ride summaries, profiles and accounts, course files (.tqc)
  orqwa-upload/            uploads to training platforms (intervals.icu, Strava) behind
                           ActivityUploader, the OAuth login in the browser
  orqwa-remote/            the remote for a phone or tablet: a page on the local network,
                           a ShiftInput
                           (ADR 0019)
  orqwa-update/            a newer release on GitHub, its Windows installer downloaded and
                           started over ORQWA (ADR 0023)
  orqwa-app/               application layer: commands + update() per frame for front ends,
                           the analysis across a rider's rides
  orqwa-gd/                gdext bindings (OrqwaApp node), the sample course (R95)
  orqwa-cli/               headless: scan devices, ride with fake trainer, export FIT, sample
                           rides
app/                       Godot 4 project: scenes/ (screens, 3D world, video view), ui/ (shared
                           controls and theme), shaders/, translations/, tests/ (smoke tests)
android/                   the Android APK's image, Godot plugin and export additions (ADR 0024)
art/                       Blender Python scripts for the 3D models: buildings, vegetation,
                           clouds, riders and bikes; third-party bases in art/sources/ (ADR 0009)
docs/                      requirements, plan, ADRs, development guides and shared images
wiki/                      GitHub wiki submodule: application usage guides, published after merge
scripts/                   container helpers: checks, builds (GDExtension, FFmpeg), renders and
                           screenshots, the art container, i18n
.devcontainer/             development container (all tooling lives here)
```

### Key design points

- **Plugin traits**: `RouteImporter`, `TrainerDriver`, `SensorDriver`, `ShiftInput`,
  `ActivityUploader`, `WorkoutParser`. `ShiftInput` (keyboard, Shimano Di2, Zwift Click, the remote),
  `WorkoutParser` (ZWO, ERG/MRC, FIT) and `ActivityUploader` (intervals.icu, Strava;
  ADR 0013) exist, as does `ElevationModel`; the others
  are not introduced yet: today there is one implementation of each (or none), behind concrete
  types, and each trait comes with its second implementation. `TrainerDriver` now encodes
  FTMS and Tacx controls behind one boundary in `orqwa-devices`; Bluetooth owns transport,
  notifications and reconnection ([ADR 0028](adr/0028-tacx-bluetooth.md)).
  Ride modes are separate Godot views: the 3D world and the video view.
- **FTMS**: service `0x1826`, Indoor Bike Data `0x2AD2`, Control Point `0x2AD9`
  (Request Control `0x00`, Set Target Resistance `0x04`, Set Target Power `0x05`,
  Set Indoor Bike Simulation `0x11`). Heart rate: `0x180D` / `0x2A37`.
  Grade updates throttled to ~1–2 Hz.
- **Tacx FE-C over BLE**: the NEO 2 Smart's UART service `6e40fec1…`, notifications
  `…fec2…`, controls `…fec3…`; ANT-framed FE-C pages over Bluetooth. Simulation alternates
  wind and track pages at four writes per second; ERG and resistance repeat the latest target.
  No ANT+ transport. FTMS is preferred when both services exist. Confirmed on a NEO 2 Smart (R110).
- **Physics**: `P·η = v·(m·g·(Crr·cosθ + sinθ) + ½·ρ·CdA·v_rel²) + m·v·dv/dt`, integrated per
  tick. The trainer receives `grade × difficulty` (descents per R15).
- **Elevation**: Mapterhorn terrain tiles with AWS fallback, cached on disk
  ([ADR 0005](adr/0005-elevation-data.md)).
- **Storage**: files are the source of truth ([ADR 0002](adr/0002-sync-safe-storage.md)):
  per rider `profiles/<rider>/profile.toml`, `hud.toml` and `rides/` (FIT + JSON summary, with
  what the analysis across rides keeps of each ride, [ADR 0014](adr/0014-ride-analysis-data.md));
  `courses/*.tqc` shared by all riders. The SQLite index of ADR 0002 (`history.sqlite` in the
  cache directory, R79) spares the history and the analysis reading every JSON summary; the
  files stay the truth and the index follows them. Config TOML, data JSON
  ([ADR 0004](adr/0004-config-formats.md)). See the rider guide to
  [syncing between devices](https://github.com/ORQWA/ORQWA/wiki/Syncing-between-devices),
  including direct Nextcloud folder sync; computer settings and caches stay local.
- **Course files** (`.tqc`, [ADR 0007](adr/0007-course-files.md)): manifest, GPX, the
  terrain and map tiles used and a picture of its map, so a course is rebuilt offline
  anywhere; video courses refer to their video instead of embedding it.
- **3D world generation**: a 1.5 km corridor around the route, coarse land beyond it out to
  12 km; DEM → chunked heightmap meshes, finer near the road; the road shaped into the ground
  along the route snapped to the map; streets, railways, bridges and tunnels from the map;
  buildings as Blender models fitted to the mapped outlines (simple shells further out and for
  odd outlines); MultiMesh vegetation by land cover; water in channels. Built when a ride
  starts, terrain chunks on all cores.
- **3D assets**: rider, bike, building and vegetation models generated by headless Blender
  scripts (riders from Blender Studio's CC0 Human Base Meshes), exported to `.glb`; scripts and
  models committed, CC0/CC-BY(-SA) only ([ADR 0009](adr/0009-asset-pipeline.md),
  [ADR 0011](adr/0011-stylized-look.md)).
- **Video**: FFmpeg 9.0.2 (LGPL, built from a pinned release) in the Rust core
  ([ADR 0010](adr/0010-video-decoding.md)); software decoding on a background thread, a few
  frames ahead of the ride; the rider's distance sets the video time (matched like ghosts,
  onto the route's line and evened out over a few seconds); Godot blends the previous and
  next frame in a shader. GPS from GoPro GPMF; Incyclist route videos (GPX + `.xml`); any other video added to a
  GPX course by its start and end marks.
- **Development environment**: everything runs in Docker / devcontainer. Docker on macOS has no
  Bluetooth or Metal, so macOS builds come from GitHub Actions macOS runners and only the
  portable Godot editor and built ORQWA app run natively.

## Roadmap

### Phase 0 — Foundation
- [x] Requirements, plan, ADRs, CLAUDE.md, README, LICENSE
- [x] `.devcontainer/` (Dockerfile + devcontainer.json) with Rust, cargo-deny, gdtoolkit, headless Godot
- [x] Rust workspace skeleton + lint config (clippy pedantic, missing_docs, cargo-deny)
- [x] Godot project + gdext hello-world
- [x] GitHub Actions: container job (fmt, clippy, test, deny) + macOS runner job (GDExtension + app export)
- [x] Tagged releases (#273): after checks and all native builds, publish the app/CLI
  archives for macOS arm64, Windows x86_64 and Linux x86_64/arm64 on GitHub Releases;
  preserve ZIP uploads as archives when downloading them for publication
- [x] Release versions: the version stays in `core/Cargo.toml` and the macOS and Windows
  presets, checked to agree on every change and, for a release, to be the tag's (`v<version>`),
  after v0.2.0 shipped saying 0.1.0 (ADR 0018 amendment); `make release-version VERSION=<version>`
  sets it in all of them and in `core/Cargo.lock`
- [x] Windows installer and updates (R99, ADR 0023):
  - [x] `ORQWA-windows-x86_64.msi` (WiX v7) on every release: Program Files, Start menu,
    uninstall, upgrade in place; installed and uninstalled once in CI, installed by hand
  - [x] Update check at start-up in released builds (Settings → About turns it off and checks
    now), offered on the start page or after the ride; Windows downloads and installs the new
    MSI and restarts, elsewhere the release page opens; the real GitHub check tried by hand,
    the download and install wait for the first release carrying the MSI
- **Exit:** macOS CI artifact runs on M1 and calls into Rust

### Phase 1 — Devices
- [x] FTMS driver (SIM / ERG / resistance), heart-rate sensor
- [x] Tacx FE-C over Bluetooth driver (R110, ADR 0028): NEO 2 Smart discovery, power/cadence/
  speed, SIM / ERG / resistance, latest control restored after reconnection; protocol tests
- [x] NEO 2 Smart discovery, readings, control modes and reconnection confirmed on hardware
- [x] Fake trainer
- [x] `orqwa-cli scan` / `orqwa-cli ride`
- [x] macOS CLI artifact from CI
- [x] Power meter (R71, [ADR 0016](adr/0016-power-meters.md)): Cycling Power Service, cadence
  from crank revolutions; its power and cadence win over the trainer's, each falling back to
  the trainer when the meter drops; chosen, remembered and reconnected like the other devices
- [x] Cadence source (R72): automatic, trainer, power meter or a Bluetooth cadence sensor
  (Cycling Speed and Cadence Service), the others standing in while the chosen one is silent
- [ ] ~~ERG power matching (R71's later step): the trainer's ERG target nudged until the power
  meter shows the workout's power~~ — dropped 2026-10-15: the trainer holds ERG by its own measurement
- **Exit:** CLI controls the KICKR Core 2 grade and reads power/cadence/HR ✅ (verified on hardware 2026-10-02)

### Phase 2 — Route, physics, FIT
- [x] GPX import, smoothing, online DEM correction
- [x] Physics model, difficulty, descent modes
- [x] FIT export
- **Exit:** headless fake-trainer ride over a GPX produces a valid FIT ✅ (2026-10-02, validated with fitdecode)

### Phase 3 — 3D world (MVP)
Built in rideable steps:
- [x] Step 1 — ride view in the app: route picker, device scan, settings, HUD, elevation
  profile, track minimap; `orqwa-app` application layer behind the `OrqwaApp` Godot node
- [x] Step 2 — 3D terrain from cached elevation tiles, road mesh along the route, sky, cameras
- [x] Step 3a — OpenStreetMap: land cover, forests with trees, buildings, rivers; bridges and
  tunnels in the elevation profile ([ADR 0008](adr/0008-vector-tiles.md))
- [x] Step 3b — bridge decks and tunnel tubes, weather/time-of-day presets, animated avatar,
  minimap drawn from the OSM data (offline, no tile server)
- [x] Step 3c — detailed terrain (Mapterhorn zoom 15, swissALTI3D in Switzerland), true
  first-person camera. Aerial imagery (SWISSIMAGE) was tried and dropped: draped photos looked
  worse than the land-cover shading
- [ ] ~~Step 3d — real 3D buildings in Switzerland (swissBUILDINGS3D)~~ — dropped 2026-10-15: buildings
  come from OpenStreetMap, in the stylized look (ADR 0011)
- **Exit:** ride a real GPX on the KICKR in 3D at 60 fps and save a FIT

### Phase 3c — Course files (R32–R35, [ADR 0007](adr/0007-course-files.md))
- [x] `.tqc` format 1: manifest, GPX and the downloaded inputs, with format and generator
  versions (ADR 0007 amendment)
- [x] Course library in the data directory; list, import, save-as-course in the app
- [x] An import can be cancelled from beside its loading bar, and a job that dies reports an
  error instead of loading forever (#219)
- [x] Rebuild outdated worlds from stored inputs (format 1 always rebuilds offline)
- [x] A new library starts with a sample course, Twann – Chasseral, shipped in the
  GDExtension and ridden offline (R95, ADR 0007 amendment 2026-10-19)
- Format 2 (a stored pre-built world) dropped (ADR 0007 amendment 2026-10-15)
- [x] **Keep courses ready to ride**: an opt-in setting keeps a course's built world in the
  cache, so riding it again starts at once; kept worlds stay across updates, and a course
  offers to regenerate one an older generator built (R101, ADR 0025, #237)
- **Exit:** prepare a course online, ride it offline on another machine from the `.tqc` file ✅
  (2026-10-03, Lake Biel 7 km: 6.2 MB, opened offline with an empty cache)

### Phase 4 — Rider & history
- [x] Multiple rider profiles (TOML), power and heart-rate zones, W/kg, metric/imperial display
- [x] Zones shown and adjustable per rider (#173); a rider's default trainer difficulty (#175);
  riders deleted with their rides, after asking
- [x] Customizable HUD (R23); an ascent-to-go figure (#174); power in % of FTP and heart
  rate in % of the maximum; % of FTP also under a large power figure, beside W/kg and zone
- [x] Estimated time to the finish (R66, #231): on the course page at 75 % of FTP, as a
  time-to-go figure on the HUD from the rider's average power so far
- [x] Simulation with the fake trainer: speed-up, jumps on map/profile, free camera (#53)
- [x] HUD layout per rider in the profile settings: drag and drop from the widget list
  directly into the HUD preview and within it (R51, R54)
- [x] History & analysis: FIT + JSON summary per ride, NP / IF / TSS / kJ, charts, time in
  zones
- [x] The history index of ADR 0002 (R79, #262): SQLite in the cache directory, built from
  the ride files and following them, so hundreds of rides cost one read
- [x] Climbs (auto-detected, categorised) and personal records per route and climb
- [x] Ghosts & pacers: own best, power / W/kg pacers, recorded GPX/FIT activities (R20)
- [x] Audio (R26, R114): music app control, original video sound (Phase 5), and continuous
  ambient riding sound (#259, ADR 0029): CC0 wind and bicycle textures, irregular gusts and
  speed-dependent airflow, chain movement following cadence and effort, a separately
  recorded coasting freewheel following road speed; independent volume sliders before and
  during a 3D course ride, paused/finished playback flushed; listening preview available.
  - [x] Recorded gear changes (R116): one-shot on each accepted virtual shift through
    keyboard, BLE or remote, sharing bicycle volume; limit/no-change presses stay quiet,
    rapid shifts overlap with bounded voices and paused requests are never replayed.
  - [x] Chain transfers and settling (R117): a quiet lever action followed by one of three
    recorded transfers, following cadence and load and blending into the steady chain;
    coasting defers the chain until pedaling resumes, and stopping pedals suspends it.
  - [x] Music helpers (#260): macOS compiles only the running player's command, so Apple
    Music works without Spotify installed; Windows sends media-key down/up events through
    `SendInput`. Native CI checks cover missing macOS players and the Windows input ABI.
  - [x] Real-player checks (#260): the rider reported working music control on macOS with
    Music/Spotify open, on Windows, and on Ubuntu with VLC after installing `playerctl`
    and loading media. The wiki explains Linux setup and the missing-helper, no-player
    and no-media errors.
- [x] i18n: all texts translatable, German translation, language per rider (R24)
- [x] German reviewed text by text (#261): contextual translations and classified core errors
  with localized explanations, unchanged diagnostic details and session error history (R74)

### Phase 5 — Video mode (R17, [ADR 0010](adr/0010-video-decoding.md))
- [x] Video decoding in the core: frame for any moment, forward/backward/seek, scaled to 1080p
- [x] GoPro GPMF GPS (`GPS5`, `GPS9`) with video times; GPX from the footage for the usual import
- [x] Distance↔video-time sync; video courses (`.tqc` referencing the video)
- [x] Import Incyclist route videos (`.xml` + GPX + video, e.g. the free Van Gestel library)
- [x] Video ride view: playback following the rider's speed, frame blending, HUD on top
- [x] Videos without GPS added to a GPX course ("Add video…" in the course page's Edit course
  dialog): route start and end marks in the video with frame previews; movable or removable later
- [x] More sync points between start and end (stops, speed changes in the footage)
- [ ] Hardware decoding (VideoToolbox); transcode above 1080p on import; Insta360 GPS
- [x] Video courses ridden along the video or in 3D, chosen when riding (#44); videos can be
  removed from any course
- [x] Tacx RLV import (`.rlv` + `.pgmf` + video, #42): ridden along the video by distance
  and slope, no 3D
- [x] Original video sound (R26): pitch-keeping time stretch (WSOLA) at the rider's speed,
  fades out when slow, switchable per ride
- [x] Ride options that apply to video courses only (no camera, time of day, weather)
- [x] A video course keeps its own copy of the video beside it, so it outlives the imported
  file (#165); AV1 decoded fast enough for real time on x86_64 (rav1d's assembly)
- [x] Smooth playback: the footage's GPS matched onto the route's line rather than its points
  10 m apart (1 Hz GPS gave each 10 m one, two or three seconds of video) and evened out over
  5 s; frames decoded ahead of the ride, so the blend never waits for one (AV1 on Apple
  silicon decodes slowly)
- **Exit:** ride a GoPro recording of a real climb on the KICKR, video in step with the effort

### ~~Phase 6 — Street imagery~~
- [ ] ~~Google Street View (user key) + Mapillary with crossfades~~ — dropped 2026-10-15

### Phase 7 — Extras
- [x] Virtual gears + shift inputs (R59, R75: keyboard, Shimano Di2 and the Zwift Click):
  - [x] Virtual gears on a single cog (24 gears, the road scaled for the trainer, ADR 0003)
    and the keyboard (↑ / ↓) behind `ShiftInput`; the drivetrain per rider
  - [x] Shimano Di2 over Bluetooth: buttons assigned to D-Fly channels shift up and down
    (channels chosen under Devices), remembered and reconnected like the trainer
  - [x] Di2 buttons assignable (#139): each channel's press, hold and double press shift one
    or two gears, or work the camera, the overlay or the music
  - [x] More ride controls on Di2 buttons (R66): pause/resume, trainer difficulty ±10 %,
    workout intensity ±5 % and skipping a structured workout's step; also on the keyboard
    (H / E, I / L, N)
  - [x] The Zwift Click over Bluetooth (R75, #228): the original unit's + and − as buttons
    1 and 2 of the same assignments (press and hold; no double press), its plain protocol
    checked on a unit; the Play and Ride stay out
  - [x] The Zwift Click v2 (R77): its two pods as one shifter, + and − as buttons 1 and 2;
    what any shifter's buttons do set in a dialog behind a pen beside its row (#271)
    and the other eight as 3–10, kept awake and the left pod's Zwift lock told in the log;
    written from the community's drivers and checked on a pair (R96): the left pod relays the
    right pod's buttons, which count once
  - [x] Virtual gears by default (R98): a new rider starts on a single cog (50 × 14), so a
    connected Click shifts from the first ride; the whole profile also changes during a ride
    (ride settings → Profile), kept in the profile and followed by the ride at once: weight,
    drivetrain, FTP of a structured workout, zones, units, map, language; pausing opens the
    ride settings
  - [x] The remote (R86, #228, ADR 0019): a page on the local network (`orqwa-remote`)
    that a phone's or tablet's browser opens from a QR code, its ten buttons (two big) doing what the
    rider gave them behind a pen like a shifter's, showing the gear and the pause under the
    ORQWA logo; off until turned on under Devices, a secret token in its address, a new
    address to lock out those paired before; **not yet tried on a real phone or tablet**
  - [x] A TV remote between rides (R87): a pad of arrows, OK and Back goes through ORQWA's
    screens to start a course or workout, a held Finish & save ends a ride, and a focus ring
    shows where keyboard and remote are (Escape goes back too)
  - [x] The remote keeps the screen on (R88): an unseen silent clip, muted for Chrome and
    mixed into the rider's music on an iPhone, as plain http gives no wake lock; works in
    Firefox on Android, **not yet tried in Chrome or on an iPhone or iPad**
- [x] ERG workouts (ZWO/ERG/MRC/FIT + editor), FTP test:
  - [x] Structured workouts (R21): ZWO, ERG/MRC and FIT workout files behind `WorkoutParser`
    (`orqwa-workouts`), a library in `workouts/` with five built-in workouts, ridden on their
    own or on a course; steps, ramps, free steps, cadence targets and messages
  - [x] FTP test (R22): a ramp test from the rider's FTP, over when the cadence gives way;
    75 % of the best minute, kept with the ride and taken into the profile from the summary;
    also the 20-minute (95 %) and 2 × 8-minute (90 %) tests, all out in slope mode (#125)
  - [x] Workout editor: steps (steady, ramp, free) with duration, % of FTP, cadence and a
    message, intervals added at once, a live chart; saved as ZWO in the library
- [x] Uploaders (R29), ANT+ FE-C:
  - [x] intervals.icu behind `ActivityUploader` (`orqwa-upload`, ADR 0013): the rider's API
    key under Accounts on their card, uploads from the summary and the history, where each
    ride went kept with it
  - [x] Upload rides automatically, per account (R60): on the summary's Done; uploads take
    turns, each with the account's newest credentials, and a ride goes to a platform once
  - [x] Strava, through each rider's own Strava API application: its Client ID and Secret,
    then the login in the browser back to a loopback address; tokens renewed in the upload's
    turn, access revoked on disconnecting or deleting the rider; rides along a route go up as
    virtual rides with their map, workouts without one as trainer rides (#354)
  - [x] Today's planned workouts from intervals.icu on the Workouts tab (R81, #187): read
    through the same account when the tab opens, chosen with a click, nothing stored
  - [ ] ~~ANT+ FE-C~~ — dropped 2026-10-15: Bluetooth only; Tacx FE-C over BLE added by R110
- [x] Windows and Linux builds from CI (Linux x86_64 and arm64 on Ubuntu 22.04, ADR 0012)
- [x] Logo (orca on a bike, `docs/brand/`), app icon and boot splash
- [x] Windows `.exe` with the app icon, ORQWA's name and version, as on macOS, rather than the
  Godot template's (taskbar, Explorer, Task Manager); CI fails a build that keeps them
- [x] Moments (R124, #185, [ADR 0032](adr/0032-ride-moments.md)): what happened on the ride,
  in the rider's voice
  - [x] One channel for everything said mid-ride (`AppEvent::Moment`, `moments.rs`): climbs
    topped and their records, halfway, 1 km (1 mi) to go, every 25 km (25 mi), the first
    ride on a course; a card at the top centre, the line in the overlay's bar; the **Voice**
    per rider (Casual, Plain, Off) in the profile, the lines in `MomentLines`
  - [x] Power peaks against the rider's best 5 s, 1, 5, 20 and 60 min, from their history at
    the start, each told once the effort tops out
  - [x] One line on both summaries, worked out from the history (`App::recap`): the first
    ride on the course, else the rides this week against the rider's optional plan (profile:
    rides planned per week); figures and records stay with their own rows. A hard workout step
    held is told live
  - [x] A chime under a new **Sound** settings tab (R125, on by default, made at start-up from
    two bell notes rather than a recording) and a burst of particles on the card

### Phase 8 — Start page & course gallery (R36–R42, R48–R54)
- [x] Start page with top tabs (Courses, History, Profile, Devices); ride view separated
- [x] Path card: route in `#2EB0FF` on black (elevation profile on the course page only); since
  #192 on the course's own map, drawn when the course is prepared and kept in the `.tqc`; since
  #203 the land around the whole route, black and white, with the names of its towns
- [x] The ride's minimap on the course's map of the land around the whole route, its tiles
  kept in the `.tqc`, with the corridor in detail over it for the close view, on video
  courses too; colored or dark as each rider chooses (R70, #236)
- [ ] ~~Auto-rendered course screenshots at build time, cover selection, stored in the `.tqc`
  (ADR 0007 format bump); frames from the video for video courses~~ — dropped 2026-10-15: the course's
  map (path card) is its picture
- [x] Courses gallery (cards with stats and small map) and course detail page with ride options;
  courses renamed (the pencil's Edit course dialog, with their video) and deleted (the bin)
  there, 3D world built only when riding; the cards share the window's full width, their maps
  growing with them
- [x] The gallery searched by name, sorted (name, length, climbing, estimated time, date added,
  last ridden), filtered by chips (type, terrain, length bands) and grouped (R67, #230); each
  card with the rider's estimated time from the profile its file keeps
- [x] The countries a course passes, from the OpenFreeMap tiles' borders and country labels,
  to group, filter and search by and listed on the cards; older courses learn theirs in the
  background (R68, #230)
- [x] The gallery's search, filters, sort and group in one row with its title and Import; the
  filter chips in a panel under a Filters button, the chips turned on listed under the toolbar
  (R85)
- [x] Ride summary screen after the ride (name, keep or discard), then back home (R42)
  - [x] Automatic quick summary dialog over the finished ride, shared with Finish & save:
    essential figures, power PR badges (R62), rename, Done, confirmed discard and View details
    (R113); no redundant finish flag in the bottom controls.
- [x] Auto-reconnect last-used devices (R41)
- [x] Devices that drop come back by themselves, also after a pause: the driver looks the
  device up again (macOS forgets it at once) instead of retrying its stale link; the in-ride
  settings' Devices tab shows each device and reconnects those that are not (R69)
- [x] Devices connect the moment they are chosen, and every row says how its device is doing
  (R76): searching, connecting, connected, reconnecting, not found or failed, in colour; a
  search per row finds that kind of device alone and connects the nearest, a scan for all is
  the fallback; the devices used last are shown by name while they reconnect at start, and
  choosing none lets a device go
- [x] Beside a connected device its live reading (power, cadence, heart rate), also between
  rides; beside a failed one why the attempt failed, while the driver keeps trying (R80)
- [x] Beside a connected device how full its battery is (R97): Bluetooth's Battery Service, and
  Zwift's battery message for the Click and each Click v2 pod; printed by the CLI too
- [x] A scan's *Found n device(s)* is a link to a panel listing what it found, each device
  under its row with its signal (R107)
- [x] In-ride settings dialog (camera, difficulty, descents, time of day, weather, HUD) from
  one button or key S; finish & save or abort without saving after a confirmation (R48–R49)
- [x] The same ride options on the course detail page (R48, with the start page)
- [x] Ride names: default course + date, set on the summary, rename in history (R50)
- [x] Export a ride's FIT file from the history or summary to a place the rider picks (R28)
- [x] The ride chart draws cadence too; clicking a legend key hides or shows its line (R73)
- [x] Pause a ride and go on with it (button, P or space; also in the overlay)
- [x] Full screen (R25, #218): F11 or Alt+Enter on any page, or Settings → Graphics;
  the choice is kept and ORQWA starts that way
- [x] The overlay's place is each rider's, graphics quality and full screen this computer's
  (R78, #257): kept next to the cache, out of the synced data folder, carried over once
- [x] The devices are each rider's (R82): remembered devices, cadence source and shifter
  buttons in `profiles/<rider>/devices.toml`, carried over once from `settings.toml`;
  switching riders lets go of the other rider's devices and reconnects the new one's, a new
  rider starts with the devices in use, a scan picks a strap only if used last, and a device
  used last that a scan misses is no longer forgotten
- [x] Settings behind a cog at the top right of the start page (R83): Graphics (quality, full
  screen) moved from the devices tab, now Devices; About with the version, the commit
  (`build.rs`) and the licence notices of ORQWA, Godot, FFmpeg, every Rust crate (cargo-about,
  checked to be current) and others' models and data (ADR 0018)
- [x] About names the branch with the commit (R130): a pull request's own branch and last
  commit, not the merge CI builds
- [x] Each shifter model has its own button assignments (R84): a Di2's channels no longer
  change a Click's + and −; the assignments kept for every shifter before stay each model's
- [x] UI design system (sleek minimal) applied to all screens and dialogs, polished course
  summaries; no text bloat on course load, dialogs that reflow when resized (R52–R53):
  - [x] The ride view's controls as icons with tooltips in a bar that folds away (#189)
  - [x] The summary's rename and delete as icon buttons (#190); buttons that delete are red
  - [x] The Profile tab and dialog: the rider as a heading, cards unfolding to every setting
    and the zones (#191, #194)
  - [x] The course page's title line like a ride's: a pencil for Edit course (name, video), a
    bin to delete it
  - [x] A spinner above Ride while the course page loads its route; a course that failed to
    open tries again when reopened
  - [x] The interface at the screen's scale rather than stretched to the window (R94): a
    smaller window shows less, not smaller text; the layouts reflow down to 1024 × 640
    (`ReflowGrid`, a wrapping gallery toolbar, scrolling pages) and dialogs fit the window
  - [x] Pages that scroll keep their scroll bar's room: the gallery no longer reflows without
    end at some widths, which crashed the app when macOS zoomed its window
- **Exit:** start the app, pick a course from the gallery, ride it, see the summary, land back home

### Phase 9 — Graphics (R43–R47, R89, [ADR 0009](adr/0009-asset-pipeline.md), [ADR 0011](adr/0011-stylized-look.md), [ADR 0020](adr/0020-realistic-look.md))
From 2026-10-05 the target was a **stylized, faceted, pastel** look (ADR 0011) instead of realism;
the realistic look's textures and patterns were replaced step by step. Since 2026-10-18 the
target is **as realistic as possible** (ADR 0020, R89): smooth shading, real proportions,
physically based materials and light, restyled group by group.

- [x] Route on the real road: GPX snapped to the OpenStreetMap roads and paths it rides
  (bends between sparse points restored, GPS wander removed); record keys unchanged — roads
  chosen for the whole track (no hops onto side streets, sidewalks or parallel roads), turns
  through junctions, corners rounded; bridges and tunnels only from the roads ridden and where
  the terrain bears them out
- [x] Natural road in its landscape: a smooth curve with its verge, cuttings into hillsides
  and embankments down to valleys (finer ground near the road), never covered by the ground
- [x] Other streets from the map drawn in 3D: asphalt streets, gravel tracks and paths, draped
  on the ground and joined to the road ridden, bridges as decks; trees and grass kept off them;
  ways for pedestrians only (footways, sidewalks, steps) left out
- [x] Building types: churches, chalets, farmhouses, apartment blocks, industrial halls
  — kinds inferred from the map (places of worship, land use, size, elevation), Blender-made
  models placed where they fit (chalets facing the valley, choirs east), shells in the
  distance and for odd outlines
- [x] Castles and lighthouses (#137): from the map's points of interest (castles by class,
  lighthouses by name, as OpenMapTiles has no class for them), Blender-made castles (keep,
  crenellations, corner towers) and lighthouses (banded tower, gallery, lantern), lighthouses
  without an outline standing on their own; a castle of any other outline built from it:
  windowed walls behind merlons, towers at its convex corners, a keep under a hipped roof
  over the largest rectangle inside
- [x] Tropical worlds (#136): palms, banana plants and tropical shrubs, and houses built for
  the heat (flat roofs with water tanks, low red-tiled hipped roofs) within 27° of the
  equator below 1,200 m
- [x] Quality presets Low–Ultra in settings, frame-time budget per preset (60 fps; a lower
  preset is suggested when a ride stays below)
- [x] Trees follow graphics quality (R108): Low/Medium/High/Ultra upload about
  12.5/25/50/100 % of the trees and draw them to 600/1,200/2,200/3,240 m. Stable subsets
  keep existing trees as quality rises; View distance overrides their range. Kept worlds
  work without regeneration; a changed density refreshes uploaded chunks for the next ride.
- [x] Separate Tree density detail (R109): Auto or Low/Medium/High/Ultra independently of
  View distance and the other effects, kept per computer and reset with the other details.
- [x] Authored tree LODs (R43, R108): retain nearby detail and forest population, simplify
  distant canopies and their shadows, with screen-size selection and bounded spatial batches;
  verify native FPS and Low's rendering cost without changing kept worlds.
- [x] Graphics details below the preset (R90): shadows, ambient occlusion, reflections, bloom,
  view distance, grass, anti-aliasing, render scale, frame rate limit and V-Sync, each Auto or
  its own value, kept per computer
- [x] Default frame rate capped at 60 fps, including Auto at every quality preset (R111),
  from startup; explicit frame rate limits and Unlimited remain available
- [x] Asset pipeline: Blender 5.2 LTS in an x86-64 art container (`scripts/art.sh`), scripted
  models, an in-game preview renderer (`scripts/render-models.sh`), agent skills for the look,
  the pipeline and render review (`.claude/skills`)
- [x] Stylized look, step by step (ADR 0011):
  - [x] A build log while a course is prepared (R93): each step with its count and time, ticked
    when done, and how long the whole took
  - [x] Foundation: one palette for core and shaders (`app/assets/palette.json`); standard
    views rendered before and after every visual change (`scripts/render-views.sh`)
  - [x] Ground, water, roads and sky: faceted ground coloured by land cover (rock on steep
    facets), faceted water, clean roads and tracks, warm soft light balanced for linear tone
    mapping, pastel gradient sky with crisp clouds, pastel evening and rain; photo textures
    retired
  - [x] Vegetation: faceted conifers, broadleaf trees, bushes and rocks made in Blender
    (`art/vegetation`), coloured per plant from the palette and swaying in the wind; forests as
    before, now with bushes under the trees near the road, a few solitary trees and bushes in
    meadows and gardens, rocks on scree and steep slopes, nothing in buildings; grass and
    flowers stay procedural
  - [x] Buildings restyled: flat palette colours (walls, roofs, shutters, flowers per building;
    mapped colours turned into their nearest palette colour), faceted, no drawn patterns;
    chunkier models without gutters, downpipes, rafters or glazing bars (20–50 % fewer faces);
    shells with clean window rectangles only
  - [x] Rider & bike: faceted female and male riders from CC0 stylized bodies (Blender
    Studio's Human Base Meshes) with natural proportions, scripted kit, helmet and sunglasses,
    each seated on a scripted bike sized to their legs and reach (`art/riders`); cadence-driven
    pedalling (legs by inverse kinematics, ankling, a little sway); the rider chosen per
    profile
  - [x] Polish (#103): a band of gravel along the shores of lakes and rivers; low-poly clouds
    made in Blender, lit by the sun and the weather, more, bigger and lower as it clouds over;
    faceted raindrops falling straight round the camera, wet roads and streets with polygonal
    puddles where they are level; pastel haze thicker in low sun, fog lying in the route's low
    ground (thick on mornings); presets without SSIL, SDFGI or volumetric fog, their budget
    given to grass, models and shadows further out, raindrops per preset
- [x] Feedback round after the riders (issues #74–#79), in this order:
  - [x] Nicer riders (#76): hair over the scalp (to the nape for her, short for him) and a
    longer ponytail; helmets shaped from the head they sit on, drawn out at the back, with vent
    slots; shoulder caps smaller and turning with the arms; sunglasses with two lenses following
    the face, a bridge and arms; in bends the rider follows the road's own curve (the one it is
    drawn along), turns with it and leans in by speed and radius
  - [x] Free camera (#78): Shift + arrows look around, arrows alone move (the Shift speed boost
    is gone; the mouse wheel sets the speed); Shift + mouse works again: the app's root control
    took every mouse move over the 3D view, now it lets them through
  - [x] Nicer roads (#74):
    - [x] Other streets never cut by the ground: draped along the ground's own triangles, so
      no grass shows through the asphalt and no street sinks into a slope
    - [x] Junctions joined cleanly where streets meet the road ridden: no kerb walls across the
      mouths (the road's edge is road there), roundabouts as rings round a raised island with a
      kerb; streets meet each other and the road ridden with rounded kerbs in the corners, end
      round and bend round, so no jagged polygons where they overlap
    - [x] The road's edge as a low bevel, not a step that reads as a wall (15 cm instead of
      25 cm, sloping gentler than 45°)
    - [x] Kinks of the snapped route smoothed (S-bends where the track jumps between roads):
      service roads, cycle paths and side streets right beside a bigger road are taken only
      where the track clearly follows them, so the route no longer hops on and off them
    - [x] Water under roads, never over them: streams and rivers laid on the ground just
      below every street and the road, so they pass under them; culverts left out
  - [x] Nicer environment (#75):
    - [x] Water lies in the land, never floating above it or buried: lakes, rivers and
      streams are laid on the ground (whose height the terrain model measures at the water's
      surface), so lakes lie level and rivers slope with their course (a shore band remains
      for Polish)
    - [x] Water in channels (#93): streams, rivers and lakes lie half a metre below the land
      beside them, the ground carved into natural banks and a bed; under bridges the channel
      runs on, where roads, streets and railways cross on the ground it stops short of them
      (a culvert)
    - [x] Railways: main lines, narrow gauge, funiculars and light rail, each on a smooth line
      of its own (railway grades, bridges and tunnels straight between their ends, tunnels
      where the hill rises far above the track, viaducts where the ground falls far below it,
      clear of the road ridden where they cross), shaping the ground like the road and
      carrying its kind of bridges and tunnels; a bed of ballast with sleepers and rails;
      plants keep off them
    - [x] A wider view in the mountains: coarse land (240 m facets, from zoom-10 terrain tiles)
      beyond the 1.5 km corridor out to 12 km, coloured by its shape (meadow, forest, lakes;
      rock and snow by the shader), fading into the haze, so the edge of the map never shows
    - [x] Buildings in the reference style of #75 (cornices, colour bands, parapets, balconies,
      awnings, rooftop details) and more kinds and colours; whole windows only on shells
      (none cut at corners and edges): blocks in coloured, light or modern (pale yellow, pale
      blue, sage, brick) plaster with contrasting trim (a ground storey of its own, string
      course, cornice), stacked balconies and rooftop units; shops (mapped shops, cafés,
      restaurants) with a glazed front and an awning onto their street; offices (commercial
      land, mapped offices) with bands of glass, hotels (mapped hotels and guest houses) with
      balconies in every column, public buildings (schools, hospitals, town halls, libraries,
      post offices, police and fire stations, and buildings on school and hospital grounds),
      classic or modern — Blender models where they fit, shells otherwise
    - [x] Bridges and viaducts: short, low bridges of the road ridden are stone arch bridges
      (arches, the walls over them, vaults), longer and higher ones viaducts on capped
      columns; bridges of other streets and railways stand on piers too
    - [x] Solid undersides on non-ridden road bridges (R121), including highways, tracks and
      paths: downward-facing deck surfaces follow bends, slopes and chunk boundaries.
      Generator 11 offers regeneration of kept worlds; stored format remains 3.
    - [x] Realistic bridge structures (R122): narrower single-track decks, low kerbs and open
      steel railings, capped columns, end bearing walls and closed slab ends; textured,
      scanned cast concrete and weathered masonry. Roads, streets, paths and railway beds also draw
      their exposed backs, including in kept worlds. Generator 12 adds the bridge geometry.
    - [x] One owner for a ridden bridge whose mapped centre line is slightly offset;
      independent crossing and parallel bridges stay visible (generator 13).
    - [x] Open Lukmanier tunnel mouths through draped approach paving, end caps and
      junctions as well as terrain; preserve roads above tunnels (generator 14).
    - [x] Complete bridge ownership across mismatched tunnel/bridge tag boundaries:
      remove the extra approach slab at Alpenbrevet 174.17 km to go (generator 15).
    - [x] Ground pier and bearing foundations across their complete footprint on the final
      shaped and carved terrain, including steep banks and channels (generator 16).
    - [x] Alpenbrevet railway ramps after Lukmanier before Airolo: consistent crossing
      clearances and independent elevations for vertically separated tracks (generator 17).
    - [x] Bridge ends and protection crossing junctions: mapped bridge ends meet a nearby
      ridden ground road at its height; protection opens only at same-level connections
      (generator 18). Grade-separated crossings and the opposite edge stay protected.
    - [x] Follow-up Alpenbrevet skewed village bridge at 178.51 km to go and raised decks at
      68.85 and 67.26 km to go: one surface owner even when the ridden course has a Ground
      tag on the mapped bridge (generator 19).
    - [x] Guardrails on tight curves: shared endpoints on both road edges and across chunk
      boundaries; bridge barriers follow the pavement's cross-sections on Alpenbrevet's
      start/finish loop (generator 22).
    - [x] Duplicate streets draped down terrain: matched map features share the ridden
      pavement owner despite GPX offsets; verify Alpenbrevet at 30.64 km to go, keeping
      shallow joining arms, crossings and independent parallel ways (generator 25, R124).
    - [x] Guardrails beside lower hairpin arms: keep blade heights at the owning road,
      with post feet anchored to its edge and curved/chunk joins retained (generator 26, R125).
    - [x] Roundabout bridge fences: one structure owner for outbound and return curves,
      including mismatched tag boundaries; leave the at-grade junction interior open while
      retaining outside-edge and grade-separated protection (generator 23).
    - [x] General barrier clearance: bridge railings, kerbs and parapets and roadside fences
      must not cut any generated carriageway, including parallel joins and crossings between
      span endpoints; preserve independent elevations and slab closure (generator 24).
    - [x] Terrain inside tunnels and pavement on their roofs around 175.72 km to go:
      clip surrounding terrain and paving along the complete tube, and give aligned
      mapped ground approaches one surface owner (generator 20).
    - [x] Guardrails below the road around 33.52 km to go: preserve the ridden road's
      verge datum beside a lower parallel railway, keeping protection grounded at road
      level and the railway at its separate elevation (generator 21).
    - [x] Tunnel portals (#135): a tunnel's tube starts where the ground as drawn rises over
      it and keeps it covered, not in the open in front of the hill; a stone ring round the
      opening ends the cutting leading to it, no ground closes the opening, and the hill over
      the tunnel stays as it is (road ridden and railways)
  - [x] Fixes from a ride round the Bielersee (#98–#101):
    - [x] Roads stay clear of buildings (#100): none reaches into the road ridden by a corner
      or a wall, and none stands across a paved street on the ground
    - [x] Railway bridges give way to roads (#98): no pier or wall on a road, street or
      track below; bridges over them are viaducts, piers moved off the ways
    - [x] Parallel tracks under one bridge or in one tunnel (#99), at one height
    - [x] The same road ridden twice is one road at one height (#101), drawn once, and sharp
      turns are round, not pointed
    - [x] Turns in place taken out for riding in 3D (#101): a track running a few dozen
      metres into a side road or past a junction and straight back rides on; longer turns back
      and turns at roundabouts stay; video courses keep their track
  - [x] Stale docs (#79, #104): the README screenshot and the building gallery in art/README
    in the stylized look, review images only PRs used removed (PRs now link theirs by commit),
    the realism risk below, MPFB2 and textures in ADR 0009, feature docs describing the old
    look, the CLI doc (bridges and tunnels, `route --world`)
- [x] Planners' files (#166, #172) and the reference route (#167): where a file leaves the roads
  (a straight line through a tunnel or gallery, across a lake, a fix lost in a tunnel) the
  route follows the map's roads between; the terrain model checked against the file's own
  elevations and spikes cut; the Oberalp route as a fixture with its standard views and checks
- [x] Roundabouts ridden the way traffic goes round them (R126): counter-clockwise where it
  keeps right, clockwise where it keeps left, also where a planner's file skips the ring;
  GPS wander a few metres back is no extra lap, turning loops stay two-way (generator 27)
- [x] Streets and railways as one line across tile borders, paved streets level across
  (#116, #117); nothing of a railway tunnel or a house in the road ridden (#138)
- [ ] Realistic look, step by step (ADR 0020, R89):
  - [x] Decision and tools: ADR 0020, the `orqwa-look` and `orqwa-art-pipeline` skills
    rewritten, a baker of tileable PBR textures from procedural Blender materials
    (`art/textures`, WebP), Cycles previews from the build scripts (`--preview DIR`,
    `art/preview.py`); taken over from the author's earlier game project
  - [x] Ground and roads: smooth terrain textured by land cover (grass varying in large
    patches and drier on slopes and high up, golden fields, mossy forest floor, gravel, rock
    from three sides on steep ground, snow), covers blended over a grid cell; asphalt with
    grain, cracks, wheel tracks and worn markings, a ragged gravel strip and a grass verge;
    streets and tracks textured; puddles as irregular patches; finer grass tufts
  - [x] Road hierarchy and paving (R119, ADR 0031): shared appearance for ridden and surrounding
    roads; motorway carriageways and ramps, main roads, local streets and cycleways retain
    distinct estimated widths and markings. Explicit paving overrides class defaults;
    geometry and roadside placement follow widths. Shallow branches retain their joining
    segments and nearby parallel paths keep their visible surface. Existing course tiles only,
    no added downloads or ride-start requests. Generator 10 / stored format 3.
  - [x] Vegetation: spruces of drooping needle sprays, broadleaf trees with limbs and crowns
    of leaf-cluster cards (`art/vegetation/foliage.py`), lit through and fluttering; barked
    trunks; rounded rocks with a new rock texture (no paving cells)
  - [x] Natural rocks (R115): asymmetric boulders with weathered fracture planes, a CC0
    rock scan and shared cliff/boulder shading with broad mineral patches and irregular
    interrupted joints visible from a distance. Kept worlds update without regeneration.
  - [x] Denser mapped forests (R103): 4 m candidate spacing within 300 m of the road,
    10 m equivalent spacing farther away; wooded slopes keep their trees, clearings and
    other land retain their cover. Generator 2 offers regeneration of kept worlds.
  - [x] Woodland ferns (R104): spreading and upright Blender models with paired leaflets,
    patchy undergrowth within 60 m of the road using the existing shrub budget and the
    short draw distance; generator 3 offers regeneration of kept worlds.
  - [x] Urban surfaces (R105, ADR 0027): dense-city paving, commercial/industrial asphalt,
    sidewalks and curbs, inferred building paths, forecourts and low garden walls; parks
    and suburban lawns preserved, grass kept out of paving. Generator 7 / stored format 2.
  - [x] Grounded sidewalks: shared joins around bends, solid edges down to the terrain,
    and continuous approaches to lowered building access; no floating slabs or open seams.
  - [x] Rounded sidewalk corners and kerbs: follow the carriageway's junction arcs, round
    outer bends, trim intersecting surfaces instead of removing whole slabs, and draw a
    separate 18 cm curb-stone border with joints and a beveled lip (R105/R112).
  - [x] Andermatt sidewalk continuity: one sidewalk owner for offset GPX/map lines,
    walking surfaces anchored to their street, and continuous clear widths beside obstacles;
    bounded corner replacement preserves neighbouring walks. Compared the full saved
    Alpenbrevet route at 209.36 km to go from above and beside the street; connected islands
    share a grade and only exposed sides close to terrain. Generic width, direction and
    chunk-boundary regressions plus village, city and Oberalp renders pass (R131–R135).
  - [x] Minimum inferred sidewalk length: require 12 m of connected walking surface
    after clipping, across straight/curved sections and chunk boundaries. Omit detached
    corner and island remnants; preserve long runs and separate building access (R136).
  - [x] City benches (R106): regular candidates on alternate sidewalks, paired bins, placement
    at sidewalk height, clear entrances and junctions; no seats on unsuitable slopes.
  - [x] Buildings: textured plaster, clay tiles, slate, boards, masonry and gravel roofs
    tinted per building, walls darker at their foot, reflective glass; textured shells
  - [x] Light and sky: physically based atmosphere with sunlit clouds (the faceted cloud
    meshes removed), AgX tone mapping, sun and sky light rebalanced, natural palette; dark
    rippled water with reflections (screen-space on High and Ultra); rain as faint streaks
  - [x] River surfaces (R118, ADR 0030): shared longitudinal water levels independent of
    valley sides and road embankments, level cross-sections and beds shaped to the same
    datum, wider transitions on tall banks and rock on exposed channel sides; generator 8
    offers regeneration for older kept worlds.
  - [x] Riders and bikes: detailed road bikes and cycling kit; realistic bodies (Human Base
    Meshes) skinned to a skeleton, posed on the bike in Blender, the app turning the leg bones
    to the pedals
  - [x] Rider and bike motion (R91, `RiderMotion`): pitch and lean as damped springs, standing
    on the pedals on steep climbs and in sprints with the bike rocking under the rider, sway
    with effort, cranks level when coasting
  - [x] Local traffic lanes (R120): rider and ghosts keep right or follow left-hand traffic
    at the route's location. Placement follows the drawn carriageway width, including narrow
    paths and kept worlds; country-border tiles are bundled for offline riding. Recording,
    physics, video synchronization and minimap coordinates stay on the original route.
  - [ ] Props by the road (R92), group by group:
    - [x] Roadside safety: guard rails where the ground falls away (start and end spans, gaps
      at junctions), delineator posts on country roads, snow poles above 1,300 m
      (`art/props`, `core/orqwa-world/src/props.rs`)
    - [x] Towns: street lamps staggered every 60 m, benches with bins, bus shelters every
      400 m, where they have room (clear of walls, side streets and junctions); villages the
      map leaves unmarked found by their buildings
    - [ ] Bollards, fountains and the like
    - [x] Road signs: places' names where the road enters and leaves them (text on the sign,
      from the map), bend warnings before hairpins, grade warnings before steep stretches
    - [ ] Signposts at junctions
    - [x] Countryside: pasture fences in stretches beside pasture and fields, round hay bales
      in fields, wayside crosses where lanes join outside towns
    - [x] Power lines: wooden poles every 50 m along stretches of country road, three wires
      sagging from pole to pole as geometry
    - [ ] Field gates
  - [ ] More geometry on buildings where it shows (eaves, window depth); frame rate on an M1
    at Medium checked after the restyle
- **Exit:** a long hilly course at 60 fps on M1 base at Medium ✅ (#111, 2026-10-08); screenshots
  reviewed against the references (R44)

### Phase 10 — Workout modes & overlay (R55–R56)
- [x] Constant-power workout (ERG target) and heart-rate hold (zone or bpm, min/max power, gentle
  ramping) in the core, tested with the fake trainer and a simulated heart rate; rides without
  a route (flat road, FIT as indoor cycling without positions); `orqwa-cli ride --power` /
  `--hr-zone` / `--hr-target`
- [x] Workouts in the app (R58): a Workouts tab after Courses; a workout on its own (HUD, its
  targets and a live chart of power and heart rate) or on a course in 3D; changed during the
  ride from the settings dialog; in the history by its name
- [x] Overlay window: transparent, borderless, always on top, movable and resizable, shows the
  rider's HUD; entered from a button during any ride or workout, or by starting a workout as
  an overlay (R57); recorded as a ride
- [ ] ~~Overlay over full-screen apps on macOS~~ — dropped (R57): it would need native window
  code and a new dependency; the overlay stays on top of normal windows
- [ ] Overlay on Windows and Linux (X11; Wayland where the compositor supports it)
- **Exit:** stream a video in a (non-full-screen) window on the Mac while the overlay HUD holds
  Zone 3 on the KICKR and the session lands in the history as a FIT

### Phase 11 — Rider analysis (R61–R65, #217)
The rider's power across their rides on an Analysis tab, from what each ride keeps beside its
summary ([ADR 0014](adr/0014-ride-analysis-data.md)). One step per analysis:
- [x] Power curve (R62): the best power per duration among a period's rides against all rides or
  the period before, in W or W/kg; the ride's curve on its summary and ★ for its power records;
  rides saved before analysed once from their FIT files; `orqwa-cli sample-rides` for
  screenshots
- [x] Power profile (R63): a radar of the best 5 s / 1 / 5 / 20 / 60 min of the period against
  the rider's best of all time (the "power spider" of #217), beside the power curve
- [x] Fitness, fatigue and form (R64, CTL / ATL / TSB) from the rides' TSS, by local day;
  weekly bars of time, distance, climbing, TSS, rides and time in power and heart-rate zones,
  from power and heart-rate histograms each ride keeps (analysis version 2)
- [x] Progress (R65): the best 1 / 5 / 20 min week by week or month by month, the FTP over time
  and the FTP tests; an FTP estimated from the last 90 days' best efforts (Morton's
  three-parameter critical power, else 95 % of 20 min) offered, never applied
- **Exit:** a season of real rides shows where the rider got stronger, and how fit and fresh
  they are, without leaving ORQWA

### Phase 12 — Android tablets (R100, [ADR 0024](adr/0024-android.md))
- [x] A — trial: a separate Android image, the core for `aarch64-linux-android`, a Godot
  Android plugin that readies btleplug through `JNI_OnLoad`, the project staged so `app/` stays
  untouched; on a Galaxy Tab S7 (Android 16) the KICKR's power and the Click's shifting work;
  the core's log in Android's log; picked files copied from `content://` addresses
- [x] B — an `android` CI job beside the others, building the APK on every pull request (its
  own image cached in GHCR like the dev image); on releases only with D
- [x] C — fitting Android, tried on the Tab S7 on 2026-10-20:
  - [x] profiles, rides and courses in the app's `files` folder, not the cache Android may clear
  - [x] saving to places picked in Android's save dialog (ride export), as opening already does
  - [x] Low graphics by default (the mobile renderer has no SSAO or screen-space reflections
    anyway), smooth on the Tab S7
  - [x] HTTPS through Android's certificate checks (rustls-platform-verifier's Java part from its
    own archive, set up with the app's context): a course imported on the tablet fetched its tiles
  - [x] the overlay (R55) as picture-in-picture: the ride's HUD in a small window over other
    apps, scaled to it, the world not drawn behind it. Music control says it is not supported;
    the update check offers the release page (until E)
  - [x] touch: Godot turns taps into clicks, enough to ride
- [x] D — a release-signed APK as a release asset (tags only, the key from the repository
  secrets; version code from ORQWA's version), the wiki (installing, permissions, updates,
  picture-in-picture); the internet list is unchanged
- [x] E — updates installed from ORQWA (R103): the release's APK downloaded by the core and handed
  to Android's installer by the plugin, without a prompt from Android 12 where Android allows it;
  tried on the Tab S7 on 2026-10-22. Where Advanced Protection forbids installing apps, the
  dialog explains how to turn it off
- **Exit:** a real ride on the tablet: course imported there, KICKR and Click, saved and uploaded

### The name ORQWA (R102, [ADR 0026](adr/0026-the-name-orqwa.md))
"Torqa" is taken by another registered app; the project becomes ORQWA (orca, torque and watt).
- [x] Code identifiers: crates `orqwa-*`, `OrqwaApp` and the extension `orqwa_gd`, `ORQWA_*`
  variables, the Android plugin, the dev image and volumes, the skills
- [x] The app named ORQWA: the wordmark (`scripts/brand/wordmark.py`), Torqa's folders moved to
  ORQWA's on the first start, installer, `org.orqwa.bike`, release files, docs, wiki and
  screenshots
- [x] Welcome and naming story on the wiki homepage, with the "orqward" joke, the supplied
  cycling-orca illustration, sidebar navigation and a link from the README
- [x] Refresh the app, video-course, route, model and rider screenshots after the city and
  woodland changes; a screenshot gallery linked from the README and wiki navigation
- [x] Shorten the usage wiki around setup and rider tasks; remove scenery and implementation
  descriptions, keeping the naming story and screenshot gallery
- [ ] Drop the release asset `Torqa-windows-x86_64.msi` (`scripts/release/publish.sh`) once
  riders have moved past Torqa 0.5.0
- [x] Release files named with their version (`ORQWA-0.5.2-windows-x86_64.msi`, #322); the
  updater takes the versioned installer first
- [ ] Drop the plain-named installers (`ORQWA-windows-x86_64.msi`, `ORQWA-android-arm64.apk`) from
  releases once riders have moved past the versions that look only for them

## Risks

- The Shimano Di2 button protocol is reverse-engineered (community findings, no code reused)
  and may change → isolated behind `ShiftInput`.
- The remote listens on the local network over plain http → off until turned on, a
  secret token in every address, renewable; the token can be read by anyone who can watch the
  network's traffic, acceptable for shifting gears (ADR 0019).
- Garmin / TrainingPeaks / Komoot upload APIs need partner approval (Garmin's developer
  program takes no new applications in 2026) → not offered, FIT export instead (ADR 0013).
  Strava requires a client secret → each rider brings their own Strava API application.
- Insta360 GPS extraction is less documented than GoPro GPMF.
- Detail on an M1 integrated GPU (towns place thousands of buildings, forests thousands of
  trees) → triangle and texture budgets, compressed textures with mipmaps, mesh LODs from
  Godot's import, quality presets (R43); art direction reviewed on rendered previews.
- Realism with few assets shows every gap (why ADR 0011 left it) → each group restyled as a
  whole to one level of quality before the next (ADR 0020), textures baked from scripted
  procedural materials or CC0 scans, reviewed in the standard views.
- Wayland has no portable always-on-top → the overlay may not stay on top there. (Over
  full-screen apps it would need native macOS window settings Godot doesn't expose; dropped, R57.)
- Free terrain imagery is limited → the ground is coloured by OSM land cover and slope, no
  imagery needed.

## Verification

- **Unit tests**: FTMS packet encode/decode, physics vs reference values (e.g. 250 W, 75 kg, 0 %
  → ~36–37 km/h), GPX fixtures, smoothing, climb detection, NP/TSS.
- **Integration**: `orqwa-cli ride --fake --route core/fixtures/<route>.gpx --time-scale 50` → FIT
  validated with the Garmin FIT SDK / `fitdecode`, plus a test upload to intervals.icu. In the
  app, `scripts/check.sh` runs the GDExtension smoke tests (`app/tests/`): a fake-trainer ride
  over a course and the UI components.
- **Hardware**: ride on the KICKR Core 2 — resistance follows grade/difficulty, HR strap reads,
  reconnects after dropout.
- **Performance**: Godot profiler ≥ 60 fps on M1 at the Medium preset on a long hilly route; world-generation time
  measured and cached.
