//! The ORQWA application layer: devices, routes, the ride engine and storage behind a simple,
//! frame-driven API. Front ends (the Godot app, tests) call commands and [`App::update`] once per
//! frame; all asynchronous work runs on an internal runtime, so callers never block or await.

pub mod about;
pub mod ambient;
pub mod analysis;
pub mod failure;
use failure::{Failure, Kind};
pub mod hud;
mod import;
mod kept;
pub mod library;
pub mod media;
pub mod moments;
pub mod paths;
pub mod remote;
mod traffic;
pub mod updates;
pub mod uploads;
pub mod video;
pub mod view;

pub use import::{Imported, LoadStage, Progress, countries, course_map, import_gpx, import_route};
pub use kept::KeptWorld;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant, SystemTime};

use orqwa_devices::ble::{Bluetooth, ControllerModel, DeviceKind, DiscoveredDevice};
use orqwa_devices::fake::{self, FakeRider};
use orqwa_devices::{DeviceEvent, DeviceHandle};
use orqwa_domain::battery::Battery;
use orqwa_domain::files::UsedFiles;
use orqwa_domain::profile::{Drivetrain, Profile};
use orqwa_domain::recording::{RideSummary, Sample};
use orqwa_domain::shifting::{ButtonMap, Control, Navigation, Shift, ShiftInput};
use orqwa_domain::telemetry::{CadenceSource, Telemetry, TrainerControl};
use orqwa_domain::units::{Meters, MetersPerSecond, Percent, Watts};
use orqwa_physics::{DescentMode, RiderSetup, VirtualGears};
use orqwa_routes::{Climb, ElevationProfile, ElevationSource, LocalProjection, Route};
use orqwa_session::analysis::{
    analyse, effort, summarize, time_at, time_in_heart_rate_zones, time_in_power_zones,
};
use orqwa_session::estimate;
use orqwa_session::ghost::Ghost;
use orqwa_session::workout::Workout;
use orqwa_session::{Ride, RideConfig, RideState};
use orqwa_storage::computer;
pub use orqwa_storage::computer::{AUTO as GRAPHICS_AUTO, GRAPHICS_OPTIONS};
use orqwa_storage::course::{self, CourseCountry, Manifest};
use orqwa_storage::index::HistoryIndex;
use orqwa_storage::profiles::{self, DeviceRole, StoredProfile};
pub use orqwa_storage::profiles::{GraphicsQuality, OverlayWindow};
use orqwa_storage::rides::{self, ClimbTime, RideRecord};
use orqwa_terrain::{Terrain, TileSource};
pub use orqwa_video::Frame;
use orqwa_video::Video;
use orqwa_world::World;
use tracing::warn;
pub use video::SyncMark;

/// Credits for the data a course bundles, stored in course files (ODbL, CC BY) and listed on
/// the About tab.
const ATTRIBUTION: [&str; 3] = [
    "© OpenFreeMap © OpenMapTiles · Data © OpenStreetMap contributors (ODbL)",
    "Terrain: Mapterhorn (CC BY 4.0)",
    "Terrain: AWS Terrain Tiles",
];

/// Points of the thinned track and profile stored with a course for its card.
const PREVIEW_POINTS: usize = 200;

/// How long the reconnect at start scans for the devices used last.
const RECONNECT_SCAN: Duration = Duration::from_secs(6);

/// How long [`App::shutdown`] waits for devices to disconnect.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(3);

/// Errors from commands that fail immediately.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// A classified failure with a separate diagnostic for presentation.
    #[error("{0}")]
    Reported(#[from] failure::Failure),
    /// The async runtime could not be started.
    #[error("cannot start runtime: {0}")]
    Runtime(#[from] std::io::Error),
    /// A ride needs a loaded route.
    #[error("no route loaded")]
    NoRoute,
    /// A ride needs a trainer.
    #[error("no trainer connected")]
    NoTrainer,
    /// The device index does not refer to a discovered device of the right kind.
    #[error("unknown device")]
    UnknownDevice,
    /// Saving a course needs a route whose world has been built.
    #[error("the course is not ready yet")]
    CourseNotReady,
    /// No profile with that id.
    #[error("unknown profile")]
    UnknownProfile,
    /// The app always has a rider, so the only one cannot be deleted.
    #[error("the only rider cannot be deleted")]
    LastProfile,
    /// The chosen ghost cannot ride this route.
    #[error("{0}")]
    GhostUnavailable(String),
    /// No training platform with that id (R29).
    #[error("unknown platform")]
    UnknownPlatform,
    /// The rider has no account on that platform yet.
    #[error("connect {0} first")]
    NotConnected(&'static str),
}

/// Who to race against (R20).
#[derive(Debug, Clone, PartialEq)]
pub enum GhostChoice {
    /// Nobody.
    None,
    /// The rider's fastest earlier ride on this route.
    PersonalBest,
    /// A pacer holding constant power.
    Power(Watts),
    /// A pacer holding constant power per kilogram of the rider's body weight.
    WattsPerKg(f64),
    /// A recorded activity (GPX with times, or FIT) along this route.
    Activity(PathBuf),
}

/// Where the ghost is relative to the rider.
#[derive(Debug, Clone, PartialEq)]
pub struct GhostState {
    /// What the ghost is.
    pub name: String,
    /// Its distance from the route start.
    pub distance: Meters,
    /// Seconds the rider is behind it (negative: ahead); `None` once it is out of reach.
    pub gap: Option<f64>,
}

/// A device found by a scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Index to pass to the connect commands.
    pub index: usize,
    /// Advertised name.
    pub name: String,
    /// Whether it is a trainer, a power meter, a heart-rate sensor or a shifter.
    pub kind: DeviceKind,
    /// Which shifter it is, for a shifter: its buttons differ.
    pub model: Option<ControllerModel>,
    /// Signal strength in dBm, if known.
    pub rssi: Option<i16>,
    /// Whether this is the trainer or sensor used last (R41).
    pub remembered: bool,
}

/// A device the rider uses, and how it is doing now (R69, R76).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceStatus {
    /// Its name.
    pub name: String,
    /// Whether it is the trainer, the power meter, the heart-rate sensor or the shifter.
    pub kind: DeviceKind,
    /// Whether it is connected, on its way, or was not found.
    pub state: DeviceState,
    /// Why the last attempt to connect it failed, while its driver tries again (R80).
    pub failure: Option<String>,
}

/// How a device the rider uses is doing (R76).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    /// Linked and reporting.
    Connected,
    /// Chosen or remembered and being connected, or reconnected after a drop; the driver
    /// keeps trying until it is let go of.
    Connecting,
    /// Used last, but the scan did not find it: switched off, asleep or out of reach.
    Missing,
}

impl DeviceState {
    /// The state's name as the front end knows it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Connecting => "connecting",
            Self::Missing => "missing",
        }
    }
}

/// Key facts about a loaded route.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteSummary {
    /// Name from the file, or the file name.
    pub name: String,
    /// Length in metres.
    pub length: f64,
    /// Total climbing in metres.
    pub elevation_gain: f64,
    /// Steepest climbing gradient in percent.
    pub max_grade: f64,
    /// Where the elevations come from.
    pub elevation_source: ElevationSource,
}

/// A course in the library.
#[derive(Debug, Clone, PartialEq)]
pub struct CourseEntry {
    /// The course file.
    pub path: PathBuf,
    /// What the course file says about itself.
    pub manifest: Manifest,
}

/// A course as its estimated time depends on it: its file, and when and how long it was
/// prepared, which a course replaced under the same name changes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EstimateKey {
    path: PathBuf,
    created_unix_s: u64,
    length_bits: u64,
}

/// The gallery's estimated times by course, and the rider setup and power they are for.
#[derive(Debug, Default)]
struct Estimates {
    basis: Option<(RiderSetup, Watts)>,
    times: HashMap<EstimateKey, Option<Duration>>,
}

impl EstimateKey {
    fn of(course: &CourseEntry) -> Self {
        Self {
            path: course.path.clone(),
            created_unix_s: course.manifest.created_unix_s,
            length_bits: course.manifest.length_m.to_bits(),
        }
    }
}

/// A ride in the history.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    /// The FIT activity file.
    pub fit: PathBuf,
    /// Route and summary.
    pub record: RideRecord,
    /// Whether this is the rider's fastest time over the whole route.
    pub route_record: bool,
    /// Per entry of [`RideRecord::climbs`]: whether it is the rider's fastest time there.
    pub climb_records: Vec<bool>,
    /// The durations of [`RECORD_DURATIONS`](orqwa_domain::recording::RECORD_DURATIONS) over
    /// which this ride holds the rider's best power (R62).
    pub power_records: Vec<Duration>,
}

/// The rider's best times on a route (R27).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RouteRecords {
    /// Fastest time over the whole route.
    pub route: Option<Duration>,
    /// Fastest time per climb, in the order of [`Route::climbs`].
    pub climbs: Vec<Option<Duration>>,
    /// Whether the rider has ridden the route before, finished or not.
    pub ridden: bool,
}

/// The climb the rider is on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClimbProgress {
    /// Index in [`Route::climbs`].
    pub index: usize,
    /// The climb.
    pub climb: Climb,
    /// Distance ridden on it so far.
    pub ridden: Meters,
    /// Time on it so far.
    pub elapsed: Duration,
    /// The rider's best time on it.
    pub best: Option<Duration>,
}

/// Everything recorded during one ride, for its analysis.
#[derive(Debug, Clone, PartialEq)]
pub struct RideDetail {
    /// The 1 Hz samples.
    pub samples: Vec<Sample>,
    /// Time in each power zone of the active rider.
    pub power_zones: [Duration; 7],
    /// Time in each heart-rate zone of the active rider.
    pub heart_rate_zones: [Duration; 5],
}

/// How long a device's last reading counts as live (R80): a strap sends every second, a
/// trainer several times a second, so a longer silence means the device went quiet.
pub const READING_AGE: Duration = Duration::from_secs(5);

/// Something that happened since the last [`App::update`].
#[derive(Debug, Clone, PartialEq)]
pub enum AppEvent {
    /// A scan finished.
    DevicesFound(Vec<DeviceInfo>),
    /// Preparing a course advanced.
    LoadProgress {
        /// The current step.
        stage: LoadStage,
        /// Units done in this step.
        done: usize,
        /// Units in this step.
        total: usize,
    },
    /// A route was imported; its 3D world is being generated.
    RouteLoaded(RouteSummary),
    /// The 3D world for the loaded route is ready.
    WorldReady {
        /// Number of terrain chunks.
        chunks: usize,
        /// Terrain samples without elevation data (terrain follows the road there).
        fallback_samples: usize,
    },
    /// A device connected (also after a reconnect).
    Connected(String),
    /// Devices used last that a reconnect at start did not find (R41), by name: the rider
    /// should wake them and scan.
    RememberedMissing(Vec<String>),
    /// A device lost its connection; it reconnects automatically.
    Disconnected(String),
    /// An attempt to connect a device failed (R80); its driver tries again after a pause.
    ConnectionFailed {
        /// The device's name.
        name: String,
        /// Why: a timeout, the device out of reach, a Bluetooth error.
        reason: String,
    },
    /// The rider reached the finish.
    RideFinished,
    /// The ride was saved as a FIT file.
    RideSaved(PathBuf),
    /// A course was saved or imported into the library.
    CourseAdded(PathBuf),
    /// Courses in the library learned their countries (#230).
    CoursesChanged,
    /// Something worth telling the rider of happened on the ride (R124).
    Moment(moments::Moment),
    /// A shifter's button asked for a ride control (#139), during a ride.
    Control(Control),
    /// The remote asked for a step through the screens (#228), with or without a ride.
    Navigate(Navigation),
    /// A rider's account on a training platform is connected (R29).
    AccountConnected {
        /// The rider's profile id.
        rider: String,
        /// The platform's id.
        platform: String,
        /// Who is connected, as the platform knows them.
        label: String,
    },
    /// Connecting an account failed.
    AccountFailed {
        /// The rider's profile id.
        rider: String,
        /// The platform's id.
        platform: String,
        /// Why, in words for the rider.
        message: Failure,
    },
    /// A ride is on a platform (R29).
    UploadFinished {
        /// The ride's FIT file.
        fit: PathBuf,
        /// The platform's id.
        platform: String,
        /// The activity's page there.
        url: String,
    },
    /// A page to show the rider in their browser, e.g. a platform's login.
    OpenUrl(String),
    /// A newer ORQWA was released (R99).
    UpdateAvailable {
        /// Its version, e.g. `0.5.0`.
        version: String,
        /// Its release page, with what changed.
        page: String,
        /// Whether ORQWA can download and install it itself; else the rider gets the page.
        installable: bool,
    },
    /// The rider asked, and ORQWA is the newest version.
    UpToDate,
    /// The update's installer is downloaded, ready to install.
    UpdateDownloaded,
    /// Looking for, or downloading, an update failed.
    UpdateFailed(Failure),
    /// Uploading a ride failed.
    UploadFailed {
        /// The ride's FIT file.
        fit: PathBuf,
        /// The platform's id.
        platform: String,
        /// Why, in words for the rider.
        message: Failure,
        /// The platform no longer accepts the account: the rider has to connect it again.
        reconnect: bool,
    },
    /// The workouts planned for `day` on `platform` were read (R81): [`App::workouts`]
    /// lists them now.
    PlannedWorkouts {
        /// The platform's id.
        platform: String,
        /// The day asked for, `YYYY-MM-DD`.
        day: String,
    },
    /// Reading the workouts planned on `platform` failed.
    PlannedWorkoutsFailed {
        /// The platform's id.
        platform: String,
        /// Why, in words for the rider.
        message: Failure,
    },
    /// A background operation failed.
    Error(Failure),
}

/// Which trainer to connect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrainerChoice {
    /// The simulated trainer.
    Fake(FakeRider),
    /// A device from the last scan, by [`DeviceInfo::index`].
    Discovered(usize),
}

enum JobResult {
    Scan(Result<(Bluetooth, Vec<DiscoveredDevice>), String>),
    // Loading results carry the load they belong to; those of an abandoned load are dropped.
    Route(u64, Result<Box<Imported>, Failure>),
    Video(u64, Result<Box<(Imported, video::VideoSource)>, Failure>),
    /// A world, the version of the generator that built it, and the fingerprint to keep it
    /// under once built for a library course (R101).
    World(u64, Arc<World>, u32, Option<String>),
    Progress(u64, LoadStage, usize, usize),
    CourseAdded(Result<PathBuf, Failure>),
    /// The loaded route was saved as this course.
    CourseSaved(Result<PathBuf, Failure>),
    /// The course at the first path has its own copy of its video at the second (#165).
    VideoKept(PathBuf, PathBuf),
    Upload(Box<uploads::Done>),
    Update(updates::Done),
    /// The countries of the course at the path, prepared at the given time, were found
    /// (#230); `None` once the lookup is over.
    Countries(Option<(PathBuf, u64, Vec<CourseCountry>)>),
    Failed(Failure),
}

/// Forwards load progress to the frame loop, at most once per percent per stage.
struct Reporter {
    tx: mpsc::Sender<JobResult>,
    load: u64,
    last: Option<(LoadStage, usize)>,
}

impl Reporter {
    fn report(&mut self, stage: LoadStage, done: usize, total: usize) {
        let percent = done * 100 / total.max(1);
        if self.last != Some((stage, percent)) {
            self.last = Some((stage, percent));
            let _ = self
                .tx
                .send(JobResult::Progress(self.load, stage, done, total));
        }
    }
}

/// Feeds `ride` the shifts `input` asks for, passes on its other controls, and reports when it
/// connects or drops, `was_connected` telling how it was at the last update.
fn drive_shift_input(
    input: &mut dyn ShiftInput,
    ride: &mut Option<ActiveRide>,
    was_connected: &mut bool,
    events: &mut Vec<AppEvent>,
) {
    for shift in input.poll() {
        if let Some(active) = ride {
            active.shift(shift);
        }
    }
    // Taken either way, so that presses between rides do not act in the next one.
    for control in input.controls() {
        if ride.is_some() {
            events.push(AppEvent::Control(control));
        }
    }
    // Screens are gone through between rides as well: that is how one is started (#228).
    events.extend(input.navigation().into_iter().map(AppEvent::Navigate));
    let connected = input.connected();
    if connected != *was_connected {
        *was_connected = connected;
        let name = input.name().to_owned();
        events.push(if connected {
            AppEvent::Connected(name)
        } else {
            AppEvent::Disconnected(name)
        });
    }
}

struct ActiveRide {
    ride: Ride,
    /// The name it goes into the history by, for a workout without a course; rides on a course
    /// are named after it.
    name: Option<String>,
    /// Wall-clock start, set when the trainer first connects.
    started: Option<SystemTime>,
    finished: bool,
    /// Best times before this ride, to compare against.
    records: RouteRecords,
    /// The next climb whose top has not been reached.
    next_climb: usize,
    milestones: moments::Milestones,
    peaks: moments::PowerPeaks,
    steps: moments::StepsHeld,
    /// What to tell of once the ride starts: that it is the first on its course.
    greeting: Option<moments::Moment>,
    /// Summary of the samples so far and how many it covers, refreshed once per new sample
    /// rather than every frame.
    summary: (usize, RideSummary),
    /// Estimated time to the finish (R66).
    time_to_go: estimate::TimeToGo,
    ghost: Option<Ghost>,
    /// Plays the video of a video course along the ride.
    player: Option<video::VideoPlayer>,
    /// And its sound (R26), if it has any.
    sound: Option<video::SoundPlayer>,
    ambient: ambient::Sound,
    ambient_settings: ambient::Settings,
    /// Whether the video shows: a ride along a blank screen must say why.
    video_watch: VideoWatch,
    /// Ride time per real time, with the fake trainer (#53).
    time_scale: f64,
    /// Paused by the rider: the clock, the road and the trainer wait.
    paused: bool,
    /// Sped up or jumped: its times are not real, so it counts towards no records.
    simulated: bool,
}

impl ActiveRide {
    fn shift(&mut self, shift: Shift) {
        let before = self.ride.state().gear;
        self.ride.shift(shift);
        let after = self.ride.state().gear;
        if before.map(|gear| gear.number) != after.map(|gear| gear.number)
            && self.started.is_some()
            && !self.paused
            && !self.finished
            && self.player.is_none()
            && self.ride.route().is_some()
            && self.ambient_settings.bike > 0.0
        {
            self.ambient.shift();
        }
    }
}

/// The name the rider gave the course being imported.
#[derive(Debug)]
struct Naming {
    name: String,
    replace: bool,
}

/// How a ride is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    /// In the generated 3D world.
    World,
    /// Along the course's video.
    Video,
}

/// When the 3D world of a loaded route is built.
enum WorldPlan {
    /// Right after the route.
    Now,
    /// Once asked for (riding a course), with the route's map data when it is loaded.
    OnRequest(Option<Box<orqwa_osm::MapData>>),
}

/// Frames of the video handed out during a ride, and whether a problem was reported.
#[derive(Debug)]
struct VideoWatch {
    since: std::time::Instant,
    frames: usize,
    reported: bool,
}

/// The longest step a ride advances by at once, also when sped up.
const MAX_RIDE_STEP: Duration = Duration::from_millis(50);
/// How far a simulated ride can be sped up (#53).
const MAX_TIME_SCALE: f64 = 20.0;

/// How long a video course may show no picture before the rider is told.
const NO_PICTURE_AFTER: Duration = Duration::from_secs(5);

/// The application state.
pub struct App {
    runtime: tokio::runtime::Runtime,
    jobs_tx: mpsc::Sender<JobResult>,
    jobs_rx: mpsc::Receiver<JobResult>,
    bluetooth: Option<Bluetooth>,
    discovered: Vec<DiscoveredDevice>,
    trainer: Option<DeviceHandle>,
    sensor: Option<DeviceHandle>,
    /// Identifiers of the connected Bluetooth trainer and sensor, to avoid reconnecting them.
    trainer_id: Option<String>,
    /// When the current trainer last reported itself connected; `None` while it is not. A
    /// ride started later must not wait for a report that has already come.
    trainer_connected: Option<SystemTime>,
    sensor_id: Option<String>,
    /// When the heart-rate sensor last reported itself connected; `None` while it is not.
    sensor_connected: Option<SystemTime>,
    /// A power meter, whose power and cadence count over the trainer's (R71).
    power_meter: Option<DeviceHandle>,
    power_meter_id: Option<String>,
    power_meter_connected: Option<SystemTime>,
    /// A cadence sensor (R72).
    cadence_sensor: Option<DeviceHandle>,
    cadence_sensor_id: Option<String>,
    cadence_sensor_connected: Option<SystemTime>,
    /// What each device reported last and when (R80), so the devices screen can show a live
    /// reading beside a connected device, also between rides.
    readings: HashMap<DeviceKind, (Telemetry, Instant)>,
    /// What each connected device told of its batteries (R97); the shifter keeps its own.
    batteries: HashMap<DeviceKind, Battery>,
    /// Why the last attempt to connect each device failed, while its driver tries again
    /// (R80); cleared once it connects or is let go.
    failures: HashMap<DeviceKind, String>,
    /// The running scan was started to reconnect the remembered devices.
    reconnecting: bool,
    /// The loaded route came from a course file (and is in the library already).
    from_course: bool,
    /// Counts loads; results of earlier ones are stale.
    load: u64,
    /// The job of the current load, to cancel it (#219).
    loading: Option<tokio::task::AbortHandle>,
    /// When to build the loaded route's world.
    world_plan: WorldPlan,
    /// The course file of the loaded route, once it is one.
    course: Option<PathBuf>,
    /// The route key of the course being written to the library, until it is there.
    saving: Option<String>,
    /// How the rider named the course being imported.
    naming: Option<Naming>,
    /// The loaded course's video, for video courses (R17).
    video: Option<video::VideoCourse>,
    /// How the next ride is shown.
    view: View,
    /// A video open for preview frames, e.g. while aligning it to a route.
    preview: Option<(PathBuf, Video)>,
    route: Option<Route>,
    /// The map of the land around the loaded route, for its course's picture (#203) and the
    /// minimap (R70); `None` without map data.
    course_map: Option<Box<orqwa_world::course_map::CourseMap>>,
    /// The detailed map of the loaded route's corridor, for the minimap's close view.
    minimap: orqwa_world::FlatMap,
    /// The countries the loaded route passes, if looked up (#230).
    countries: Option<Vec<CourseCountry>>,
    /// Whether the courses' countries are being looked up, and how many were found so far.
    filling_countries: Option<usize>,
    /// The gallery's estimated times (R67) by course, for the rider setup and power they
    /// were worked out for: a search refreshes the gallery on every key, too often to ride
    /// every course again.
    estimates: Estimates,
    world: Option<Arc<World>>,
    road_widths: orqwa_world::RoadWidths,
    driving_left: Vec<Option<bool>>,
    offline: bool,
    /// Name and GPX of the loaded route, for saving it as a course.
    loaded: Option<(String, String)>,
    /// Cached files the loaded route and its world were built from.
    used: UsedFiles,
    ride: Option<ActiveRide>,
    /// Inputs that shift the virtual gears (R7): the keyboard, …
    shift_inputs: Vec<Box<dyn ShiftInput>>,
    /// Whether each of them was connected at the last update, to report changes.
    shift_inputs_connected: Vec<bool>,
    /// The keyboard's keys, pressed through [`App::shift`].
    keys: orqwa_devices::shift::KeyboardKeys,
    /// The phone remote while it is on (#228): a shift input beside them.
    remote: Option<remote::RunningRemote>,
    /// What its page says, in the rider's language.
    remote_texts: remote::RemoteTexts,
    /// Identifier of the connected controller, to avoid reconnecting it.
    controller_id: Option<String>,
    /// What each shifter model's buttons do (#139, R84), by button number (a Di2's channels, a
    /// Click's + and −).
    buttons: Arc<orqwa_devices::shift::Assignments>,
    profile: StoredProfile,
    /// The training platforms rides can be uploaded to and the uploads under way (R29).
    uploads: uploads::UploadState,
    updates: updates::UpdateState,
    data_dir: PathBuf,
    cache_dir: PathBuf,
    /// This computer's own settings (R78), next to the cache: never synced with the data.
    computer: PathBuf,
    /// The version of the generator that built the loaded world: an older one when it was
    /// kept from before (R101).
    world_generator: u32,
    /// The rides' metadata read once and kept (R79); `None` if the cache cannot hold it, when
    /// the history reads every ride's file as before.
    index: Mutex<Option<HistoryIndex>>,
}

impl App {
    /// Creates the application with rides saved under `data_dir` and downloads cached under
    /// `cache_dir` (see [`paths`] for platform defaults), where this computer's own settings
    /// are kept too (R78).
    ///
    /// # Errors
    /// [`AppError::Runtime`] if the async runtime cannot start.
    pub fn new(data_dir: PathBuf, cache_dir: PathBuf) -> Result<Self, AppError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let (jobs_tx, jobs_rx) = mpsc::channel();
        let (keys, keyboard) = orqwa_devices::shift::keyboard();
        let computer = cache_dir.join(computer::FILE);
        let profile = initial_profile(&data_dir);
        // Settings from before R78 go to the riders and this computer, once (#257).
        if let Err(error) = profiles::carry_over(&data_dir, &computer) {
            warn!(%error, "cannot carry the settings over");
        }
        let index = HistoryIndex::open(&cache_dir.join(orqwa_storage::index::FILE))
            .inspect_err(|error| warn!(%error, "the history reads every ride's file"))
            .ok();
        Ok(Self {
            runtime,
            jobs_tx,
            jobs_rx,
            bluetooth: None,
            discovered: Vec::new(),
            trainer: None,
            sensor: None,
            trainer_id: None,
            trainer_connected: None,
            sensor_id: None,
            sensor_connected: None,
            power_meter: None,
            power_meter_id: None,
            power_meter_connected: None,
            cadence_sensor: None,
            cadence_sensor_id: None,
            cadence_sensor_connected: None,
            readings: HashMap::new(),
            batteries: HashMap::new(),
            failures: HashMap::new(),
            reconnecting: false,
            from_course: false,
            load: 0,
            loading: None,
            world_plan: WorldPlan::Now,
            course: None,
            saving: None,
            naming: None,
            video: None,
            view: View::World,
            preview: None,
            route: None,
            course_map: None,
            minimap: orqwa_world::FlatMap::default(),
            countries: None,
            filling_countries: None,
            estimates: Estimates::default(),
            world: None,
            road_widths: orqwa_world::RoadWidths::default(),
            driving_left: Vec::new(),
            offline: false,
            loaded: None,
            used: UsedFiles::default(),
            ride: None,
            shift_inputs: vec![Box::new(keyboard)],
            shift_inputs_connected: vec![true],
            keys,
            remote: None,
            remote_texts: remote::RemoteTexts::default(),
            controller_id: None,
            buttons: Arc::new(orqwa_devices::shift::Assignments::new(|model| {
                profiles::button_map(&data_dir, &profile.id, model)
            })),
            profile,
            uploads: uploads::UploadState::new(uploads::platforms()),
            updates: updates::UpdateState::default(),
            data_dir,
            cache_dir,
            computer,
            world_generator: orqwa_world::GENERATOR,
            index: Mutex::new(index),
        })
    }

    /// All rider profiles, by name.
    #[must_use]
    pub fn profiles(&self) -> Vec<StoredProfile> {
        let listed = profiles::list(&self.data_dir);
        if listed.is_empty() {
            // Not saved yet, e.g. a read-only data directory: still offer the rider in use.
            vec![self.profile.clone()]
        } else {
            listed
        }
    }

    /// The rider riding now.
    #[must_use]
    pub fn profile(&self) -> &StoredProfile {
        &self.profile
    }

    /// Switches to another rider, remembered for the next start. The devices in use that are
    /// not the rider's are let go, and the shifter's buttons do what the rider gave them (R82);
    /// [`App::reconnect_remembered`] then connects the rider's own.
    ///
    /// # Errors
    /// [`AppError::UnknownProfile`] if there is no such profile.
    pub fn select_profile(&mut self, id: &str) -> Result<(), AppError> {
        let profile = profiles::load(&self.data_dir, id).map_err(|_| AppError::UnknownProfile)?;
        self.profile = StoredProfile {
            id: id.to_owned(),
            profile,
        };
        if let Err(error) = profiles::set_active(&self.data_dir, id) {
            warn!(%error, "cannot remember the active profile");
        }
        self.take_riders_devices();
        Ok(())
    }

    /// Saves a profile (a new one if `id` is `None`) and makes it the active one; returns its id.
    /// A new rider starts with the devices of the rider before (R82), so nothing connected is
    /// let go. The ride under way follows the riding rider's changes at once (R98): their
    /// weight, drivetrain and FTP.
    ///
    /// # Errors
    /// [`AppError::Reported`] if it cannot be written; nothing changes then.
    pub fn save_profile(&mut self, id: Option<&str>, profile: Profile) -> Result<String, AppError> {
        let new = id.is_none();
        let id = id.map_or_else(
            || profiles::new_id(&self.data_dir, &profile.name),
            ToOwned::to_owned,
        );
        profiles::save(&self.data_dir, &id, &profile)
            .map_err(|e| AppError::Reported(Failure::new(Kind::SaveProfile, e)))?;
        if new && let Err(error) = profiles::copy_devices(&self.data_dir, &self.profile.id, &id) {
            warn!(%error, "the new rider starts without devices");
        }
        if let Err(error) = profiles::set_active(&self.data_dir, &id) {
            warn!(%error, "cannot remember the active profile");
        }
        let switched = id != self.profile.id;
        self.profile = StoredProfile {
            id: id.clone(),
            profile,
        };
        if switched {
            self.take_riders_devices();
        } else {
            let (setup, profile) = (self.rider_setup(), &self.profile.profile);
            if let Some(active) = &mut self.ride {
                active
                    .ride
                    .change_rider(setup, virtual_gears(profile.drivetrain), profile.ftp);
            }
        }
        Ok(id)
    }

    /// Deletes a rider with their rides, withdrawing ORQWA's access to their accounts on
    /// training platforms (R29); deleting the active one switches to the first rider left.
    ///
    /// # Errors
    /// [`AppError::LastProfile`] for the only rider, [`AppError::UnknownProfile`] if there is no
    /// such rider, [`AppError::Reported`] if it cannot be removed.
    pub fn delete_profile(&mut self, id: &str) -> Result<(), AppError> {
        let listed = profiles::list(&self.data_dir);
        if !listed.iter().any(|p| p.id == id) {
            return Err(AppError::UnknownProfile);
        }
        let Some(next) = listed.into_iter().find(|p| p.id != id) else {
            return Err(AppError::LastProfile);
        };
        // While the credentials are still there.
        self.revoke_accounts(id);
        profiles::delete(&self.data_dir, id)
            .map_err(|e| AppError::Reported(Failure::new(Kind::DeleteProfile, e)))?;
        if self.profile.id == id {
            self.select_profile(&next.id)?;
        }
        Ok(())
    }

    /// Scans for trainers and heart-rate sensors; reports [`AppEvent::DevicesFound`].
    pub fn scan(&mut self, duration: Duration) {
        let tx = self.jobs_tx.clone();
        let existing = self.bluetooth.clone();
        self.runtime.spawn(async move {
            let result = async {
                let bluetooth = match existing {
                    Some(bluetooth) => bluetooth,
                    None => Bluetooth::new().await?,
                };
                let devices = bluetooth.scan(duration).await?;
                Ok((bluetooth, devices))
            }
            .await
            .map_err(|e: orqwa_devices::DeviceError| e.to_string());
            let _ = tx.send(JobResult::Scan(result));
        });
    }

    /// Imports a GPX route with terrain-corrected elevations; reports [`AppEvent::RouteLoaded`].
    pub fn load_route(&mut self, path: PathBuf, offline: bool) {
        let used = self.start_loading(offline);
        let load = self.load;
        let tx = self.jobs_tx.clone();
        let cache = self.cache_dir.clone();
        self.spawn_load(load, async move {
            let mut reporter = Reporter {
                tx: tx.clone(),
                load,
                last: None,
            };
            let mut report = |stage, done, total| reporter.report(stage, done, total);
            let result = match import_route(&path, &cache, offline, &used, &mut report).await {
                Ok(mut imported) => {
                    imported.course_map =
                        course_map(&imported.route, (&cache, offline), &used, &mut report).await;
                    imported.countries =
                        countries(&route_points(&imported.route), &cache, offline, &mut report)
                            .await;
                    Ok(Box::new(imported))
                }
                Err(message) => Err(message),
            };
            let _ = tx.send(JobResult::Route(load, result));
        });
    }

    /// Opens a course file: its data goes back into the cache and its route is loaded offline;
    /// reports [`AppEvent::RouteLoaded`] like [`App::load_route`]. Its 3D world is built only
    /// when asked for with [`App::build_world`].
    pub fn open_course(&mut self, path: PathBuf) {
        let used = self.start_loading(true);
        let load = self.load;
        self.naming = None;
        self.from_course = true;
        self.world_plan = WorldPlan::OnRequest(None);
        self.course = Some(path.clone());
        let tx = self.jobs_tx.clone();
        let cache = self.cache_dir.clone();
        self.spawn_load(load, async move {
            let mut reporter = Reporter {
                tx: tx.clone(),
                load,
                last: None,
            };
            reporter.report(LoadStage::Route, 0, 1);
            let unpack_from = path.clone();
            let unpack_to = cache.clone();
            let unpacked = tokio::task::spawn_blocking(move || {
                let outdated = course::preview_outdated(&unpack_from);
                course::unpack(&unpack_from, &unpack_to).map(|unpacked| (unpacked, outdated))
            })
            .await
            .map_err(|e| Failure::new(Kind::PreparationStopped, e))
            .and_then(|r| r.map_err(|e| Failure::new(Kind::OpenFile, e).arg(path.display())));
            let (unpacked, outdated_preview) = match unpacked {
                Ok(unpacked) => unpacked,
                Err(message) => {
                    let _ = tx.send(JobResult::Route(load, Err(message)));
                    return;
                }
            };
            // A video course needs its video, referenced rather than stored (R35).
            let mut video = None;
            if let Some(reference) = &unpacked.manifest.video {
                let Some(file) = reference.locate(&path) else {
                    let message = Failure::new(
                        Kind::MissingVideo,
                        format!(
                            "video {} not found — put it next to the course file",
                            reference.file_name
                        ),
                    )
                    .arg(&reference.file_name);
                    let _ = tx.send(JobResult::Route(load, Err(message)));
                    return;
                };
                video = Some(video::VideoSource {
                    video: file,
                    name: unpacked.manifest.name.clone(),
                    gpx: unpacked.gpx.clone(),
                    offset: Duration::from_secs_f64(reference.offset_s.max(0.0)),
                    marks: video_marks(reference),
                    located: reference.located,
                });
            }
            let mut report = |stage, done, total| reporter.report(stage, done, total);
            let mut imported = import_gpx(
                unpacked.gpx,
                &unpacked.manifest.name,
                (&cache, true),
                video.is_some(),
                &used,
                &mut report,
            )
            .await
            .map(|mut imported| {
                // Older shared courses may know their country without having bundled the
                // border tiles. A single known country gives a useful offline fallback.
                if let Some(countries) = &unpacked.manifest.countries
                    && let [country] = countries.as_slice()
                {
                    let left = traffic::keeps_left(Some(&country.code));
                    for side in &mut imported.driving_left {
                        side.get_or_insert(left);
                    }
                }
                imported.name = unpacked.manifest.name;
                imported
            });
            // The map of the course's land, from the tiles its file brought along (R70). A
            // course from before #236 lacks them, one from before #203 has a picture of its
            // corridor only: theirs comes online, for the minimap and a new picture.
            if let Ok(imported) = &mut imported
                && video.as_ref().is_none_or(|v| v.located)
            {
                let mut map = None;
                if !outdated_preview {
                    map = course_map(&imported.route, (&cache, true), &used, &mut report).await;
                }
                if map.is_none() {
                    map = course_map(&imported.route, (&cache, false), &used, &mut report).await;
                }
                imported.course_map = map;
            }
            let _ = tx.send(match (imported, video) {
                (Ok(imported), Some(source)) => {
                    JobResult::Video(load, Ok(Box::new((imported, source))))
                }
                (Ok(imported), None) => JobResult::Route(load, Ok(Box::new(imported))),
                (Err(message), _) => JobResult::Route(load, Err(message)),
            });
        });
    }

    /// Prepares a video course (R17) from a GoPro video with GPS or an Incyclist route video
    /// (its `.xml` control file): the route as usual, without a 3D world, paired with the
    /// video; reports [`AppEvent::RouteLoaded`] and [`AppEvent::WorldReady`] like
    /// [`App::load_route`], and adds it to the library. `offline` as for [`App::load_route`].
    pub fn load_video(&mut self, path: PathBuf, offline: bool) {
        self.load_video_from(offline, move || video::source(&path));
    }

    fn load_video_from(
        &mut self,
        offline: bool,
        source: impl FnOnce() -> Result<video::VideoSource, Failure> + Send + 'static,
    ) {
        let used = self.start_loading(offline);
        let load = self.load;
        let tx = self.jobs_tx.clone();
        let cache = self.cache_dir.clone();
        self.spawn_load(load, async move {
            let mut reporter = Reporter {
                tx: tx.clone(),
                load,
                last: None,
            };
            reporter.report(LoadStage::Route, 0, 1);
            let source = tokio::task::spawn_blocking(source)
                .await
                .map_err(|e| Failure::new(Kind::ReadVideo, e))
                .and_then(|r| r);
            let mut report = |stage, done, total| reporter.report(stage, done, total);
            let result = match source {
                Ok(source) => match import_gpx(
                    source.gpx.clone(),
                    &source.name,
                    // A course without a place keeps its own elevations: terrain data from
                    // wherever its drawn line lies would only spoil them.
                    (&cache, offline || !source.located),
                    true,
                    &used,
                    &mut report,
                )
                .await
                {
                    Ok(mut imported) => {
                        // Nor does it get a map or countries: there is no land around a drawn
                        // line.
                        if source.located {
                            imported.course_map =
                                course_map(&imported.route, (&cache, offline), &used, &mut report)
                                    .await;
                            let points = route_points(&imported.route);
                            imported.countries =
                                countries(&points, &cache, offline, &mut report).await;
                        } else {
                            imported.countries = Some(Vec::new());
                        }
                        Ok(Box::new((imported, source)))
                    }
                    Err(message) => Err(message),
                },
                Err(message) => Err(message),
            };
            let _ = tx.send(JobResult::Video(load, result));
        });
    }

    /// The loaded video course's video, if it is one.
    #[must_use]
    pub fn video(&self) -> Option<&video::VideoCourse> {
        self.video.as_ref()
    }

    /// Adds a video to the loaded course (a GPX course from the library), placed on its route
    /// by hand: `marks` say where route positions sit in the video, from the route's start to
    /// its end; the video follows the rider's distance evenly between neighbouring marks. The course becomes a video course; its file refers
    /// to the video.
    ///
    /// # Errors
    /// [`AppError::Reported`] if no course is loaded, it has a video already, the video does
    /// not fit the marks, or the course file cannot be updated.
    pub fn add_video(&mut self, video: &Path, marks: &[SyncMark]) -> Result<(), AppError> {
        let (Some(route), Some(path), Some((name, gpx))) =
            (&self.route, &self.course, &self.loaded)
        else {
            return Err(AppError::Reported(Failure::new(
                Kind::OpenCourse,
                "open a course first",
            )));
        };
        if self.video.is_some() {
            return Err(AppError::Reported(Failure::new(
                Kind::AlreadyVideo,
                "this course has a video already",
            )));
        }
        let source = video::VideoSource {
            video: video.to_owned(),
            name: name.clone(),
            gpx: gpx.clone(),
            offset: Duration::ZERO,
            marks: marks.to_vec(),
            located: true,
        };
        let added = video::VideoCourse::new(route, &source).map_err(AppError::Reported)?;
        let reference = video_reference(&added, route);
        let (size, course) = (reference.size, path.clone());
        update_manifest(&course, |manifest| manifest.video = Some(reference))?;
        self.video = Some(added);
        self.view = View::Video;
        // The course's own copy of the video, beside it, follows in the background: a copy
        // across disks takes a while (#165). The course refers to the original until then;
        // the course file is changed from the frame loop only, like every other change to it.
        let video = video.to_owned();
        let tx = self.jobs_tx.clone();
        self.runtime.spawn_blocking(move || {
            let _ = tx.send(match course::keep_video(&video, &course, size) {
                Ok(kept) => JobResult::VideoKept(course, kept),
                Err(e) => JobResult::Failed(Failure::new(Kind::SaveVideo, e)),
            });
        });
        Ok(())
    }

    /// Takes the video off the loaded course, which is ridden in 3D only from then on; the
    /// route stays as it is.
    ///
    /// # Errors
    /// [`AppError::Reported`] if the loaded course has no video or the course file cannot be
    /// updated.
    pub fn remove_video(&mut self) -> Result<(), AppError> {
        let (Some(video), Some(path)) = (&self.video, &self.course) else {
            return Err(AppError::Reported(Failure::new(
                Kind::NoVideo,
                "this course has no video",
            )));
        };
        if !video.located {
            return Err(AppError::Reported(Failure::new(
                Kind::VideoRequired,
                "this course is known only along its video and keeps it",
            )));
        }
        update_manifest(path, |manifest| manifest.video = None)?;
        self.video = None;
        self.view = View::World;
        Ok(())
    }

    /// How the next ride on a video course is shown: along its video, or in 3D (#44).
    /// Courses without a video are always ridden in 3D, courses without a place (Tacx RLV)
    /// always along their video.
    pub fn ride_along_video(&mut self, along: bool) {
        let unlocated = self.video.as_ref().is_some_and(|v| !v.located);
        self.view = if (along || unlocated) && self.video.is_some() {
            View::Video
        } else {
            View::World
        };
    }

    /// Whether the current ride plays the course's video.
    #[must_use]
    pub fn riding_along_video(&self) -> bool {
        self.ride.as_ref().is_some_and(|ride| ride.player.is_some())
    }

    /// Replaces the marks of the loaded video course's video (see [`App::add_video`]), for
    /// videos placed on the route by hand; the course file keeps the new marks.
    ///
    /// # Errors
    /// [`AppError::Reported`] if the loaded course is not aligned by hand, the marks do not fit
    /// the video, or the course file cannot be updated.
    pub fn align_video(&mut self, marks: &[SyncMark]) -> Result<(), AppError> {
        let (Some(route), Some(current), Some((name, gpx))) =
            (&self.route, &self.video, &self.loaded)
        else {
            return Err(AppError::Reported(Failure::new(
                Kind::NoVideoCourse,
                "no video course loaded",
            )));
        };
        if !current.aligned_by_hand() {
            return Err(AppError::Reported(Failure::new(
                Kind::VideoAuto,
                "this video follows its GPS; there is nothing to align",
            )));
        }
        let source = video::VideoSource {
            video: current.video.clone(),
            name: name.clone(),
            gpx: gpx.clone(),
            offset: Duration::ZERO,
            marks: marks.to_vec(),
            located: current.located,
        };
        let aligned = video::VideoCourse::new(route, &source).map_err(AppError::Reported)?;
        if let Some(path) = &self.course {
            let reference = video_reference(&aligned, route);
            update_manifest(path, |manifest| manifest.video = Some(reference))?;
        }
        self.video = Some(aligned);
        Ok(())
    }

    /// The frame of the video at `path` shown at `time`, e.g. to see where a route starts in
    /// it. The video stays open for the next call.
    ///
    /// # Errors
    /// [`AppError::Reported`] if the video cannot be opened or decoded.
    pub fn video_preview(&mut self, path: &Path, time: Duration) -> Result<Frame, AppError> {
        let opened = match &mut self.preview {
            Some((open, video)) if open == path => video,
            preview => {
                let video = Video::open(path)
                    .map_err(|e| AppError::Reported(Failure::new(Kind::PlayVideo, e)))?;
                &mut preview.insert((path.to_owned(), video)).1
            }
        };
        opened
            .frame_at(time)
            .cloned()
            .map_err(|e| AppError::Reported(Failure::new(Kind::PlayVideo, e)))
    }

    /// Done with [`App::video_preview`]: closes the video.
    pub fn close_video_preview(&mut self) {
        self.preview = None;
    }

    /// The moment of the video to show now, during a ride on a video course.
    #[must_use]
    pub fn video_time(&self) -> Option<Duration> {
        let video = self.video.as_ref()?;
        Some(video.time_at(self.ride_state()?.distance))
    }

    /// The next video frame for the ride on a video course, once the moment of
    /// [`App::video_time`] has reached the one before: it lies just after that moment, decoded
    /// ahead, so the view can blend towards it from the frame before.
    pub fn video_frame(&mut self) -> Option<orqwa_video::Frame> {
        let now = self.video_time()?;
        let active = self.ride.as_mut()?;
        let frame = active.player.as_ref()?.frame(now)?;
        active.video_watch.frames += 1;
        Some(frame)
    }

    /// Reports once if the ride's video cannot be decoded or shows no picture.
    fn watch_video(&mut self, events: &mut Vec<AppEvent>) {
        let Some(active) = &mut self.ride else { return };
        let Some(player) = &active.player else { return };
        if active.video_watch.reported {
            return;
        }
        let problem = player
            .error()
            .map(|error| format!("cannot play the video: {error}"))
            .or_else(|| {
                (active.video_watch.frames == 0
                    && active.video_watch.since.elapsed() > NO_PICTURE_AFTER)
                    .then(|| "the video shows no picture yet — see the log for why".to_owned())
            });
        if let Some(message) = problem {
            warn!(%message, "video course");
            active.video_watch.reported = true;
            events.push(AppEvent::Error(Failure::new(Kind::PlayVideo, message)));
        }
    }

    /// The next stereo samples of the video's sound during a ride on a video course, at most
    /// `max`; at [`App::video_sound_rate`] samples per second. Empty without sound.
    pub fn video_sound(&mut self, max: usize) -> Vec<orqwa_video::audio::Stereo> {
        self.ride
            .as_ref()
            .and_then(|active| active.sound.as_ref())
            .map_or_else(Vec::new, |sound| sound.pull(max))
    }

    /// Samples per second of [`App::video_sound`]; `None` while riding without the video's
    /// sound.
    #[must_use]
    pub fn video_sound_rate(&self) -> Option<u32> {
        Some(self.ride.as_ref()?.sound.as_ref()?.rate())
    }

    /// Plays the video's sound during this ride or not (R26, a ride option).
    pub fn set_video_sound(&mut self, on: bool) {
        if let Some(sound) = self.ride.as_ref().and_then(|a| a.sound.as_ref()) {
            sound.set_on(on);
        }
    }

    /// Change wind and bicycle volume and weather for this ride (R114).
    pub fn set_ambient_sound(&mut self, settings: ambient::Settings) {
        if let Some(active) = self.ride.as_mut() {
            active.ambient_settings = settings;
        }
    }

    /// Fade in a newly opened output queue, including when resuming a paused ride.
    pub fn restart_ambient_sound(&mut self) {
        if let Some(active) = self.ride.as_mut() {
            active.ambient.restart();
        }
    }

    /// Whether ambient playback may run: only an active, unpaused 3D course ride.
    #[must_use]
    pub fn ambient_sound_active(&self) -> bool {
        self.view != View::Video
            && self.ride.as_ref().is_some_and(|a| {
                !a.paused && !a.finished && a.started.is_some() && a.ride.state().position.is_some()
            })
    }

    /// Next stereo frames at [`ambient::RATE`]; empty when ambient playback is inactive.
    /// Pulling is bounded to a quarter second to keep accidental requests cheap.
    pub fn ambient_sound(&mut self, frames: usize) -> Vec<[f32; 2]> {
        if !self.ambient_sound_active() {
            return Vec::new();
        }
        let Some(active) = self.ride.as_mut() else {
            return Vec::new();
        };
        let state = active.ride.state();
        active.ambient.render(
            frames.min(ambient::RATE as usize / 4),
            ambient::Motion {
                speed: state.speed,
                cadence: state.telemetry.cadence,
                power: state.telemetry.power,
            },
            active.ambient_settings,
        )
    }

    /// Saves the loaded route with everything needed to ride it offline as a course in the
    /// library; reports [`AppEvent::CourseAdded`].
    ///
    /// # Errors
    /// [`AppError::CourseNotReady`] until the route's world has been built.
    pub fn save_course(&mut self) -> Result<(), AppError> {
        // A GPX course is complete with the data of its world; a video course rides along
        // its video and builds the world on demand.
        let (Some(route), Some((name, gpx))) = (&self.route, &self.loaded) else {
            return Err(AppError::CourseNotReady);
        };
        if self.world.is_none() && self.video.is_none() {
            return Err(AppError::CourseNotReady);
        }
        let naming = self.naming.take();
        let name = naming
            .as_ref()
            .map_or_else(|| name.clone(), |n| n.name.clone());
        let manifest = Manifest {
            format: course::FORMAT_VERSION,
            generator: format!("ORQWA {}", orqwa_domain::version()),
            name: name.clone(),
            length_m: route.length().0,
            elevation_gain_m: route.elevation_gain().0,
            max_grade_percent: route.max_grade().0,
            created_unix_s: SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            attribution: ATTRIBUTION.map(ToOwned::to_owned).to_vec(),
            route_key: Some(route.key()),
            track: thin(&view::track(route, PREVIEW_POINTS)),
            profile: thin(&view::elevation_profile(route, PREVIEW_POINTS)),
            video: self.video.as_ref().map(|v| video_reference(v, route)),
            countries: self.countries.clone(),
        };
        // Preparing the same course again must not fill the library with copies; one still
        // being written reports itself when done. A course named on import is the rider's
        // choice: saved as asked. A video course is not the GPX course of its route (#40).
        if naming.is_none() {
            if manifest.route_key.is_some() && manifest.route_key == self.saving {
                return Ok(());
            }
            let video_file = |m: &Manifest| m.video.as_ref().map(|v| v.file_name.clone());
            if let Some(existing) = self.courses().into_iter().find(|c| {
                c.manifest.route_key == manifest.route_key
                    && video_file(&c.manifest) == video_file(&manifest)
            }) {
                let _ = self.jobs_tx.send(JobResult::CourseSaved(Ok(existing.path)));
                return Ok(());
            }
        }
        let replacing = self.import_target(naming.as_ref());
        let gpx = gpx.clone();
        if let Some((loaded, _)) = &mut self.loaded {
            loaded.clone_from(&name);
        }
        self.saving.clone_from(&manifest.route_key);
        let data = self.used.paths();
        let cache = self.cache_dir.clone();
        let library = self.courses_dir();
        let tx = self.jobs_tx.clone();
        // The world just built is that of the new course (R101).
        let keep = self
            .world
            .clone()
            .filter(|_| self.keeps_worlds())
            .map(|world| (world, self.world_generator, self.keeper()));
        self.runtime.spawn_blocking(move || {
            let result = std::fs::create_dir_all(&library)
                .map_err(course::CourseError::from)
                .and_then(|()| {
                    let path =
                        replacing.unwrap_or_else(|| unique_course_path(&library, &manifest.name));
                    // The course's own copy of its video, beside it: the course is ridden long
                    // after the file it was imported from is moved or gone (#165).
                    let mut manifest = manifest;
                    if let Some(video) = &mut manifest.video {
                        let kept = course::keep_video(Path::new(&video.path), &path, video.size)?;
                        video.file_name = kept
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        video.path = kept.display().to_string();
                    }
                    // Written aside first: a replaced course stays whole if writing fails.
                    let partial = path.with_extension("part");
                    course::write(&partial, &manifest, &gpx, &cache, &data)?;
                    std::fs::rename(&partial, &path)?;
                    Ok(path)
                })
                .map_err(|e| Failure::new(Kind::SaveCourse, e));
            let saved = result.as_ref().ok().cloned();
            let _ = tx.send(JobResult::CourseSaved(result));
            if let (Some(course), Some((world, generator, keeper))) = (saved, keep) {
                keeper.keep(&course, &world, generator, None);
            }
        });
        Ok(())
    }

    /// Builds the 3D world of a course opened with [`App::open_course`], now or as soon as its
    /// route is loaded; reports [`AppEvent::WorldReady`]. True if the world is ready already.
    pub fn build_world(&mut self) -> bool {
        if self.world.is_some() {
            return true;
        }
        // A video course's file holds the data of its route only, as no world was built when
        // it was saved: the world fetches the rest when online.
        if self.video.is_some() {
            self.offline = false;
        }
        if let WorldPlan::OnRequest(map) = std::mem::replace(&mut self.world_plan, WorldPlan::Now)
            && let (Some(route), Some(map)) = (self.route.clone(), map)
        {
            self.generate_world(route, *map);
        }
        false
    }

    /// The course file of the loaded route: the opened course, or the one a GPX import was
    /// saved as.
    #[must_use]
    pub fn loaded_course(&self) -> Option<&Path> {
        self.course.as_deref()
    }

    /// Renames the course at `path`; blank names are refused.
    ///
    /// # Errors
    /// [`AppError::Reported`] if the name is blank or the file cannot be rewritten.
    pub fn rename_course(&mut self, path: &Path, name: &str) -> Result<(), AppError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(AppError::Reported(Failure::new(
                Kind::CourseName,
                "a course needs a name",
            )));
        }
        update_manifest(path, |manifest| name.clone_into(&mut manifest.name))?;
        if self.course.as_deref() == Some(path)
            && let Some((loaded, _)) = &mut self.loaded
        {
            name.clone_into(loaded);
        }
        Ok(())
    }

    /// Deletes the course at `path` from the library; rides on it stay in the history.
    ///
    /// # Errors
    /// [`AppError::Reported`] if the file cannot be deleted.
    pub fn delete_course(&mut self, path: &Path) -> Result<(), AppError> {
        // The course's own copy of its video, beside it, goes with it unless another course
        // rides along the same file (#165); a video elsewhere is the rider's and stays.
        let video = course::read_manifest(path)
            .ok()
            .and_then(|manifest| manifest.video)
            .and_then(|reference| reference.locate(path))
            .filter(|video| video.parent() == path.parent());
        std::fs::remove_file(path).map_err(|e| {
            AppError::Reported(Failure::new(Kind::DeleteFile, e).arg(path.display()))
        })?;
        if self.course.as_deref() == Some(path) {
            self.course = None;
        }
        // Its kept world goes too (R101).
        self.tidy_kept_worlds();
        if let Some(video) = video {
            let shared = self.courses().into_iter().any(|c| {
                c.manifest
                    .video
                    .as_ref()
                    .and_then(|reference| reference.locate(&c.path))
                    .is_some_and(|other| other == video)
            });
            if !shared && let Err(e) = std::fs::remove_file(&video) {
                warn!(%e, video = %video.display(), "cannot remove the course's video");
            }
        }
        Ok(())
    }

    /// Names the course the next import adds to the library ([`App::load_route`],
    /// [`App::load_video`] or [`App::import_course`]): saved as `name`, replacing a course of
    /// that name with `replace`, else next to it (#40).
    pub fn name_next_import(&mut self, name: &str, replace: bool) {
        self.naming = Some(Naming {
            name: name.trim().to_owned(),
            replace,
        });
    }

    /// The library's course named `name` (ignoring case and surrounding spaces), if any.
    #[must_use]
    pub fn course_named(&self, name: &str) -> Option<PathBuf> {
        let name = name.trim();
        self.courses()
            .into_iter()
            .find(|c| c.manifest.name.trim().eq_ignore_ascii_case(name))
            .map(|c| c.path)
    }

    /// Where the course from the next import goes: next to others, or over the one of that name.
    fn import_target(&self, naming: Option<&Naming>) -> Option<PathBuf> {
        naming
            .filter(|n| n.replace)
            .and_then(|n| self.course_named(&n.name))
    }

    /// Copies a course file into the library; reports [`AppEvent::CourseAdded`].
    pub fn import_course(&mut self, path: PathBuf) {
        let library = self.courses_dir();
        let tx = self.jobs_tx.clone();
        let naming = self.naming.take();
        let replacing = self.import_target(naming.as_ref());
        self.runtime.spawn_blocking(move || {
            let result = course::read_manifest(&path)
                .and_then(|mut manifest| {
                    std::fs::create_dir_all(&library)?;
                    if let Some(naming) = &naming {
                        manifest.name.clone_from(&naming.name);
                    }
                    let target =
                        replacing.unwrap_or_else(|| unique_course_path(&library, &manifest.name));
                    // Not `.part`: renaming rewrites the copy through a `.part` file itself.
                    let partial = target.with_extension("import");
                    std::fs::copy(&path, &partial)?;
                    if naming.is_some() {
                        course::rewrite_manifest(&partial, &manifest)?;
                    }
                    std::fs::rename(&partial, &target)?;
                    Ok(target)
                })
                .map_err(|e| Failure::new(Kind::ImportFile, e).arg(path.display()));
            let _ = tx.send(JobResult::CourseAdded(result));
        });
    }

    /// The courses in the library, by name. Unreadable files are skipped.
    #[must_use]
    pub fn courses(&self) -> Vec<CourseEntry> {
        let Ok(entries) = std::fs::read_dir(self.courses_dir()) else {
            return Vec::new();
        };
        let mut courses: Vec<CourseEntry> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|e| e == course::EXTENSION))
            .filter_map(|path| match course::read_manifest(&path) {
                Ok(manifest) => Some(CourseEntry { path, manifest }),
                Err(error) => {
                    warn!(path = %path.display(), %error, "skipping course");
                    None
                }
            })
            .collect();
        courses.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
        courses
    }

    /// The course library as the Courses tab shows it (R67): what `query` searches, chooses,
    /// sorts and groups, each course with the time the active rider is estimated to take from
    /// the profile its file keeps (R66, coasting the descents as the course page does until
    /// asked otherwise) and when they last rode it.
    #[must_use]
    pub fn course_library(&mut self, query: &library::CourseQuery) -> library::Gallery {
        let ridden = self.last_ridden();
        let basis = (
            self.rider_setup(),
            estimate::planned_power(self.profile.profile.ftp),
        );
        if self.estimates.basis != Some(basis) {
            self.estimates = Estimates {
                basis: Some(basis),
                times: HashMap::new(),
            };
        }
        let (setup, power) = basis;
        let courses: Vec<CourseEntry> = self.courses();
        let courses = courses
            .into_iter()
            .map(|entry| {
                let estimated = *self
                    .estimates
                    .times
                    .entry(EstimateKey::of(&entry))
                    .or_insert_with(|| {
                        let profile = entry
                            .manifest
                            .profile
                            .iter()
                            .map(|&[d, e]| (Meters(f64::from(d)), Meters(f64::from(e))));
                        ElevationProfile::new(profile).and_then(|profile| {
                            estimate::time_to_finish(
                                &profile,
                                &setup,
                                DescentMode::Coast,
                                Meters(0.0),
                                MetersPerSecond(0.0),
                                power,
                            )
                        })
                    });
                let last_ridden = entry
                    .manifest
                    .route_key
                    .as_ref()
                    .and_then(|key| ridden.get(key).copied());
                library::LibraryCourse {
                    entry,
                    estimated,
                    last_ridden,
                }
            })
            .collect();
        library::gallery(courses, query, self.profile.profile.units)
    }

    /// How the Courses tab was left (R67): its order and groups, the defaults for names this
    /// version does not know.
    #[must_use]
    pub fn courses_view(&self) -> library::View {
        let stored = profiles::courses_view(&self.data_dir);
        library::View {
            sort: library::SortKey::from_name(&stored.sort).unwrap_or_default(),
            descending: stored.descending,
            group: library::GroupKey::from_name(&stored.group).unwrap_or_default(),
        }
    }

    /// Remembers how the Courses tab orders and groups the library.
    ///
    /// # Errors
    /// [`AppError::Reported`] if it cannot be saved.
    pub fn set_courses_view(&mut self, view: library::View) -> Result<(), AppError> {
        let stored = profiles::CoursesView {
            sort: view.sort.name().to_owned(),
            descending: view.descending,
            group: view.group.name().to_owned(),
        };
        profiles::set_courses_view(&self.data_dir, &stored)
            .map_err(|e| AppError::Reported(Failure::new(Kind::SaveSetting, e)))
    }

    /// When the active rider last started a ride, by the key of its route.
    fn last_ridden(&self) -> HashMap<String, SystemTime> {
        let mut last: HashMap<String, SystemTime> = HashMap::new();
        for (_, record) in self.ride_records() {
            if let Some(key) = record.route_key {
                last.entry(key)
                    .and_modify(|start| *start = (*start).max(record.start))
                    .or_insert(record.start);
            }
        }
        last
    }

    /// Looks up in the background the countries of the courses that do not know them yet
    /// (#230): those prepared before ORQWA knew countries, offline, or while the map tiles were
    /// out of reach. Each course's file learns its countries as they are found, and
    /// [`AppEvent::CoursesChanged`] follows once all were looked up. `offline` uses cached map
    /// tiles only. Does nothing while a lookup is under way.
    pub fn fill_in_countries(&mut self, offline: bool) {
        if self.filling_countries.is_some() {
            return;
        }
        let missing: Vec<(PathBuf, u64, bool)> = self
            .courses()
            .into_iter()
            .filter(|c| c.manifest.countries.is_none())
            .map(|c| {
                let located = c.manifest.video.as_ref().is_none_or(|v| v.located);
                (c.path, c.manifest.created_unix_s, located)
            })
            .collect();
        if missing.is_empty() {
            return;
        }
        self.filling_countries = Some(0);
        let tx = self.jobs_tx.clone();
        let cache = self.cache_dir.clone();
        self.runtime.spawn(async move {
            for (path, created, located) in missing {
                let found = if located {
                    let file = path.clone();
                    let points = tokio::task::spawn_blocking(move || {
                        let gpx = course::route_gpx(&file).ok()?;
                        orqwa_routes::track_points(&gpx).ok()
                    })
                    .await
                    .ok()
                    .flatten();
                    match points {
                        Some(points) => {
                            countries(&points, &cache, offline, &mut |_, _, _| {}).await
                        }
                        None => None,
                    }
                } else {
                    // Known only by distance and slope: no land, no country.
                    Some(Vec::new())
                };
                if let Some(found) = found {
                    let _ = tx.send(JobResult::Countries(Some((path, created, found))));
                }
            }
            let _ = tx.send(JobResult::Countries(None));
        });
    }

    /// The map picture of the course at `path` (#192), the PNG the app drew when the course
    /// was prepared; `None` without one.
    #[must_use]
    pub fn course_preview(path: &Path) -> Option<Vec<u8>> {
        course::preview(path)
    }

    /// Whether the course at `path` is to get a new map picture: it has none, or one of its
    /// route's corridor only, from before #203.
    #[must_use]
    pub fn course_preview_outdated(path: &Path) -> bool {
        course::preview_outdated(path)
    }

    /// The map to draw the picture of the course at `path` from (#203), if it is the loaded
    /// course; `None` too when no map data could be had.
    #[must_use]
    pub fn course_map(&self, path: &Path) -> Option<&orqwa_world::course_map::CourseMap> {
        (self.course.as_deref() == Some(path))
            .then_some(self.course_map.as_deref())
            .flatten()
    }

    /// The loaded route's maps for the minimap (R70): the land around the whole route, if any
    /// map data could be had, and the route's corridor in detail. `None` without a route.
    #[must_use]
    pub fn minimap(
        &self,
    ) -> Option<(
        Option<&orqwa_world::course_map::CourseMap>,
        &orqwa_world::FlatMap,
    )> {
        self.route
            .as_ref()
            .map(|_| (self.course_map.as_deref(), &self.minimap))
    }

    /// Keeps `png`, the map picture the app drew, with the course at `path` (#192).
    ///
    /// # Errors
    /// [`AppError::Reported`] if the course file cannot be rewritten.
    pub fn set_course_preview(&mut self, path: &Path, png: &[u8]) -> Result<(), AppError> {
        course::set_preview(path, png)
            .map_err(|e| AppError::Reported(Failure::new(Kind::UpdateCourse, e)))
    }

    /// A name for the course imported from `path`, for the rider to confirm: a GPX file's
    /// route name, an Incyclist video's title, a course file's name, else the file name.
    #[must_use]
    pub fn suggested_course_name(path: &Path) -> String {
        let extension = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let read = || std::fs::read_to_string(path).ok();
        let named = match extension.as_str() {
            "gpx" => read().and_then(|xml| orqwa_routes::route_name(&xml)),
            "xml" => read()
                .and_then(|xml| orqwa_video::incyclist::parse(&xml).ok())
                .map(|route| route.title),
            "tqc" => course::read_manifest(path).ok().map(|m| m.name),
            "rlv" => video::tacx_course_name(path),
            _ => None,
        };
        named.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| {
            path.file_stem()
                .map_or_else(|| "Course".to_owned(), |s| s.to_string_lossy().into_owned())
        })
    }

    /// The course library: `courses/` in the data directory (R34).
    #[must_use]
    pub fn courses_dir(&self) -> PathBuf {
        self.data_dir.join("courses")
    }

    /// Starts the library with the sample `course`, saved as `file_name`, if there is no library
    /// yet (R95): a new rider has a course to ride at once, offline. An existing library stays
    /// as it is, so a deleted sample is not brought back. True if the sample was added.
    ///
    /// # Errors
    /// An I/O error writing the library.
    pub fn add_sample_course(&self, file_name: &str, course: &[u8]) -> std::io::Result<bool> {
        let library = self.courses_dir();
        if library.exists() {
            return Ok(false);
        }
        // The library appears with its course: a first start cut short leaves no empty
        // library behind, which would keep the sample out for good.
        let partial = self.data_dir.join("courses.part");
        if partial.exists() {
            std::fs::remove_dir_all(&partial)?;
        }
        std::fs::create_dir_all(&partial)?;
        std::fs::write(partial.join(file_name), course)?;
        std::fs::rename(&partial, &library)?;
        Ok(true)
    }

    /// Stops preparing the route, video or world being loaded (#219): its results are dropped,
    /// and what it downloaded stays in the cache, so trying again is quick.
    pub fn cancel_loading(&mut self) {
        if self.loading.as_ref().is_some_and(|job| !job.is_finished()) {
            let _ = self.start_loading(self.offline);
        }
    }

    /// Runs the job of load `load` (the current one), in place of the current load's job; a
    /// job that panics fails the load instead of leaving the rider waiting (#219).
    fn spawn_load(&mut self, load: u64, job: impl Future<Output = ()> + Send + 'static) {
        let task = self.runtime.spawn(job);
        self.loading = Some(task.abort_handle());
        let tx = self.jobs_tx.clone();
        self.runtime.spawn(async move {
            let Err(error) = task.await else {
                return;
            };
            // A cancelled load is a stale one: its report is dropped like its results.
            let message = if error.is_panic() {
                let panic = error.into_panic();
                let reason = panic
                    .downcast_ref::<&str>()
                    .map(|reason| (*reason).to_owned())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_default();
                Failure::new(
                    Kind::PreparationStopped,
                    format!("preparing the course stopped unexpectedly: {reason}"),
                )
            } else {
                Failure::new(
                    Kind::PreparationStopped,
                    "preparing the course was cancelled",
                )
            };
            let _ = tx.send(JobResult::Route(load, Err(message)));
        });
    }

    /// Forgets the current route, stops its job and starts recording the files a new one uses.
    fn start_loading(&mut self, offline: bool) -> UsedFiles {
        if let Some(job) = self.loading.take() {
            job.abort();
        }
        self.load += 1;
        self.offline = offline;
        self.world_plan = WorldPlan::Now;
        self.course = None;
        // Opening a course sets this; a GPX import goes into the library when ready.
        self.from_course = false;
        self.video = None;
        self.view = View::World;
        self.world = None;
        self.road_widths = orqwa_world::RoadWidths::default();
        self.driving_left.clear();
        self.route = None;
        self.course_map = None;
        self.minimap = orqwa_world::FlatMap::default();
        self.countries = None;
        self.loaded = None;
        self.used = UsedFiles::default();
        self.used.clone()
    }

    /// Generates the 3D world for `route` in the background; reports [`AppEvent::WorldReady`].
    fn generate_world(&mut self, route: Route, map: orqwa_osm::MapData) {
        let tx = self.jobs_tx.clone();
        let load = self.load;
        let terrain = |sources| {
            let terrain =
                Terrain::new(sources, self.cache_dir.join("terrain")).recording(self.used.clone());
            if self.offline {
                terrain.offline()
            } else {
                terrain
            }
        };
        let mut detailed = terrain(TileSource::defaults());
        // The land beyond the corridor needs only coarse tiles, a few for the whole view.
        let mut distant = terrain(TileSource::distant());
        // A library course's world may be kept, and is kept once built (R101).
        let kept = self
            .course
            .clone()
            .filter(|_| self.keeps_worlds())
            .map(|course| (course, self.keeper()));
        self.spawn_load(load, async move {
            let mut reporter = Reporter {
                tx: tx.clone(),
                load,
                last: None,
            };
            let mut keep_as = None;
            if let Some((course, keeper)) = kept {
                reporter.report(LoadStage::World, 0, 1);
                let found = tokio::task::spawn_blocking(move || keeper.find(&course)).await;
                match found {
                    Ok(kept::Found::World(world, generator)) => {
                        let _ = tx.send(JobResult::World(load, Arc::from(world), generator, None));
                        return;
                    }
                    Ok(kept::Found::Missing(fingerprint)) => keep_as = Some(fingerprint),
                    Ok(kept::Found::Unknown) | Err(_) => {}
                }
            }
            let mut world =
                orqwa_world::generate(&route, &mut detailed, &map, &mut |done, total| {
                    reporter.report(LoadStage::World, done, total);
                })
                .await;
            world.horizon = orqwa_world::horizon(&route, &mut distant).await;
            let generator = orqwa_world::GENERATOR;
            let _ = tx.send(JobResult::World(load, Arc::new(world), generator, keep_as));
        });
    }

    /// How detailed the 3D world is drawn on this computer (R43, R78).
    #[must_use]
    pub fn graphics_quality(&self) -> profiles::GraphicsQuality {
        computer::graphics_quality(&self.computer)
    }

    /// Chooses how detailed the 3D world is drawn on this computer (R43); another computer
    /// sharing the data keeps its own choice (R78).
    ///
    /// # Errors
    /// [`AppError::Reported`] if the choice cannot be saved.
    pub fn set_graphics_quality(
        &mut self,
        quality: profiles::GraphicsQuality,
    ) -> Result<(), AppError> {
        computer::set_graphics_quality(&self.computer, quality)
            .map_err(|e| AppError::Reported(Failure::new(Kind::SaveSetting, e)))
    }

    /// The graphics options set apart from the preset on this computer (R90), by name.
    #[must_use]
    pub fn graphics_options(&self) -> std::collections::BTreeMap<String, String> {
        computer::graphics_options(&self.computer)
    }

    /// Sets a graphics option on this computer (R90); `"auto"` follows the preset again.
    /// Returns false for an unknown option or value.
    ///
    /// # Errors
    /// [`AppError::Reported`] if the choice cannot be saved.
    pub fn set_graphics_option(&mut self, name: &str, value: &str) -> Result<bool, AppError> {
        computer::set_graphics_option(&self.computer, name, value)
            .map_err(|e| AppError::Reported(Failure::new(Kind::SaveSetting, e)))
    }

    /// Where the rider's overlay was last on screen (R55, R78); `None` before they first used it.
    #[must_use]
    pub fn overlay_window(&self) -> Option<profiles::OverlayWindow> {
        profiles::overlay_window(&self.data_dir, &self.profile.id)
    }

    /// Remembers where the rider's overlay is on screen.
    ///
    /// # Errors
    /// [`AppError::Reported`] if it cannot be saved.
    pub fn set_overlay_window(&mut self, window: profiles::OverlayWindow) -> Result<(), AppError> {
        profiles::set_overlay_window(&self.data_dir, &self.profile.id, window)
            .map_err(|e| AppError::Reported(Failure::new(Kind::SaveSetting, e)))
    }

    /// Whether the ride view's control bar is folded away to its corner (#189).
    #[must_use]
    pub fn ride_bar_folded(&self) -> bool {
        profiles::ride_bar_folded(&self.data_dir)
    }

    /// Remembers whether the ride view's control bar is folded away.
    ///
    /// # Errors
    /// [`AppError::Reported`] if it cannot be saved.
    pub fn set_ride_bar_folded(&mut self, folded: bool) -> Result<(), AppError> {
        profiles::set_ride_bar_folded(&self.data_dir, folded)
            .map_err(|e| AppError::Reported(Failure::new(Kind::SaveSetting, e)))
    }

    /// Whether the app plays its own sounds, such as the chime of a ride's moments (R125).
    #[must_use]
    pub fn sounds(&self) -> bool {
        profiles::sounds(&self.data_dir)
    }

    /// Turns the app's own sounds on or off.
    ///
    /// # Errors
    /// [`AppError::Reported`] if it cannot be saved.
    pub fn set_sounds(&mut self, on: bool) -> Result<(), AppError> {
        profiles::set_sounds(&self.data_dir, on)
            .map_err(|e| AppError::Reported(Failure::new(Kind::SaveSetting, e)))
    }

    /// Whether the app fills the screen on this computer (#218, R78).
    #[must_use]
    pub fn fullscreen(&self) -> bool {
        computer::fullscreen(&self.computer)
    }

    /// Remembers whether the app fills the screen on this computer, to start that way next time.
    ///
    /// # Errors
    /// [`AppError::Reported`] if it cannot be saved.
    pub fn set_fullscreen(&mut self, fullscreen: bool) -> Result<(), AppError> {
        computer::set_fullscreen(&self.computer, fullscreen)
            .map_err(|e| AppError::Reported(Failure::new(Kind::SaveSetting, e)))
    }

    /// Whether rides are simulated: with the fake trainer, which can be sped up and jumped
    /// along the route (#53).
    #[must_use]
    pub fn simulating(&self) -> bool {
        self.trainer.is_some() && self.trainer_id.is_none()
    }

    /// Speeds the simulated ride up (or back down to 1): ride time per real time, from 1 to
    /// 20; returns the speed in effect. Without a simulated ride, 1.
    pub fn set_time_scale(&mut self, scale: f64) -> f64 {
        let simulating = self.simulating();
        let Some(active) = self.ride.as_mut().filter(|_| simulating) else {
            return 1.0;
        };
        active.time_scale = if scale.is_finite() {
            scale.clamp(1.0, MAX_TIME_SCALE)
        } else {
            1.0
        };
        if active.time_scale > 1.0 {
            active.simulated = true;
        }
        active.time_scale
    }

    /// Moves the simulated ride's rider to `distance` along the route; false without one.
    pub fn jump_to(&mut self, distance: Meters) -> bool {
        let simulating = self.simulating();
        let Some(active) = self.ride.as_mut().filter(|_| simulating) else {
            return false;
        };
        active.ride.jump_to(distance);
        active.simulated = true;
        // Climbs passed by jumping are not timed; the next one counts from where it starts.
        let distance = active.ride.state().distance;
        active.milestones.skip_to(distance);
        let climbs = active.ride.route().map_or(&[][..], Route::climbs);
        active.next_climb = climbs
            .iter()
            .position(|c| c.start.0 >= distance.0)
            .unwrap_or(climbs.len());
        true
    }

    /// Moves the simulated ride's rider to the route's point nearest to `x`/`y` (metres east
    /// and north of the start, as on the map); false without a simulated ride.
    pub fn jump_near(&mut self, x: f64, y: f64) -> bool {
        let Some(route) = self.ride.as_ref().and_then(|a| a.ride.route()) else {
            return false;
        };
        let projection = LocalProjection::for_route(route);
        let nearest = route
            .points()
            .iter()
            .map(|p| {
                let (px, py) = projection.project(p.lat, p.lon);
                ((px - x).hypot(py - y), p.distance)
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, distance)| distance);
        nearest.is_some_and(|distance| self.jump_to(distance))
    }

    /// Whether the trainer is connected now.
    #[must_use]
    pub fn trainer_connected(&self) -> bool {
        self.trainer_connected.is_some()
    }

    /// Connects the trainer, replacing any previous one.
    ///
    /// # Errors
    /// [`AppError::UnknownDevice`] if the index is not a trainer from the last scan.
    pub fn connect_trainer(&mut self, choice: TrainerChoice) -> Result<(), AppError> {
        let _runtime = self.runtime.enter();
        self.forget_reports(DeviceKind::Trainer);
        let handle = match choice {
            TrainerChoice::Fake(mut rider) => {
                self.trainer_id = None;
                // A real strap's heart rate is the one to use.
                if self.sensor.is_some() {
                    rider.heart = None;
                }
                fake::spawn(rider, Duration::from_millis(250))
            }
            TrainerChoice::Discovered(index) => {
                let device = self.discovered_device(index, DeviceKind::Trainer)?;
                let id = device.id();
                // Already connected (e.g. reconnected at start): keep the link.
                if self.trainer.is_some() && self.trainer_id.as_deref() == Some(id.as_str()) {
                    return Ok(());
                }
                self.remember(DeviceRole::Trainer, &device);
                self.trainer_id = Some(id);
                self.bluetooth()?.connect(device)
            }
        };
        self.trainer = Some(handle);
        self.trainer_connected = None;
        Ok(())
    }

    /// Reconnects the trainer, power meter, heart-rate sensor and shifter used last (R41):
    /// scans in the background and connects them when found, reporting
    /// [`AppEvent::DevicesFound`] (with the remembered devices marked) and
    /// [`AppEvent::RememberedMissing`] for those not found. Returns false, without touching
    /// Bluetooth, if no device is remembered.
    pub fn reconnect_remembered(&mut self) -> bool {
        let remembered = self.remembered();
        if remembered.trainer.is_none()
            && remembered.heart_rate.is_none()
            && remembered.power_meter.is_none()
            && remembered.cadence_sensor.is_none()
            && remembered.controller.is_none()
        {
            return false;
        }
        self.reconnecting = true;
        self.scan(RECONNECT_SCAN);
        true
    }

    /// The trainer, heart-rate sensor, power meter and shifter in use or remembered (R41), and
    /// how each is doing now (R69, R76): connected, being connected, or not found.
    #[must_use]
    pub fn devices(&self) -> Vec<DeviceStatus> {
        let remembered = self.remembered();
        let status = |kind: DeviceKind,
                      live: Option<(&str, bool)>,
                      remembered: Option<profiles::RememberedDevice>| {
            live.map(|(name, connected)| {
                let state = if connected {
                    DeviceState::Connected
                } else {
                    DeviceState::Connecting
                };
                (name.to_owned(), state)
            })
            .or_else(|| remembered.map(|r| (r.name, DeviceState::Missing)))
            .map(|(name, state)| DeviceStatus {
                name,
                kind,
                state,
                failure: self.failures.get(&kind).cloned(),
            })
        };
        let controller = self
            .controller_id
            .as_ref()
            .and_then(|_| self.shift_inputs.get(1))
            .map(|input| (input.name(), input.connected()));
        [
            status(
                DeviceKind::Trainer,
                self.trainer
                    .as_ref()
                    .map(|t| (t.name(), self.trainer_connected.is_some())),
                remembered.trainer,
            ),
            status(
                DeviceKind::HeartRateSensor,
                self.sensor
                    .as_ref()
                    .map(|s| (s.name(), self.sensor_connected.is_some())),
                remembered.heart_rate,
            ),
            status(
                DeviceKind::PowerMeter,
                self.power_meter
                    .as_ref()
                    .map(|m| (m.name(), self.power_meter_connected.is_some())),
                remembered.power_meter,
            ),
            status(
                DeviceKind::CadenceSensor,
                self.cadence_sensor
                    .as_ref()
                    .map(|c| (c.name(), self.cadence_sensor_connected.is_some())),
                remembered.cadence_sensor,
            ),
            status(DeviceKind::Controller, controller, remembered.controller),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// What the device of `kind` reported last, if within [`READING_AGE`] (R80): the trainer's
    /// and the power meter's power and cadence, the cadence sensor's cadence, the strap's heart
    /// rate; also between rides, for the devices screen. The shifter reports nothing.
    #[must_use]
    pub fn device_reading(&self, kind: DeviceKind) -> Option<Telemetry> {
        self.readings
            .get(&kind)
            .filter(|(_, at)| at.elapsed() <= READING_AGE)
            .map(|(telemetry, _)| *telemetry)
    }

    /// How full the batteries of the device of `kind` are, if it told while connected (R97):
    /// one level, or a pair's left and right; also between rides, for the devices screen.
    #[must_use]
    pub fn device_battery(&self, kind: DeviceKind) -> Option<Battery> {
        let battery = if kind == DeviceKind::Controller {
            self.controller_id
                .as_ref()
                .and_then(|_| self.shift_inputs.get(1))
                .map(|input| input.battery())
        } else {
            self.batteries.get(&kind).copied()
        };
        battery.filter(Battery::is_known)
    }

    /// A device chosen anew or let go: its last reading, battery and failure are not its
    /// successor's.
    fn forget_reports(&mut self, kind: DeviceKind) {
        self.readings.remove(&kind);
        self.batteries.remove(&kind);
        self.failures.remove(&kind);
    }

    /// Connects again the Bluetooth devices that are not connected now, e.g. during a ride
    /// (R69): drops their links, scans and connects the remembered ones as at start (R41),
    /// reporting [`AppEvent::RememberedMissing`] for those not found. Connected devices keep
    /// their link. Returns false if no device is remembered.
    pub fn reconnect_devices(&mut self) -> bool {
        if self.trainer_id.is_some() && self.trainer_connected.is_none() {
            self.trainer = None;
            self.trainer_id = None;
        }
        if self.sensor_connected.is_none() {
            self.sensor = None;
            self.sensor_id = None;
        }
        if self.power_meter_connected.is_none() {
            self.power_meter = None;
            self.power_meter_id = None;
        }
        if self.cadence_sensor_connected.is_none() {
            self.cadence_sensor = None;
            self.cadence_sensor_id = None;
        }
        if self.controller_id.is_some() && !self.shift_inputs.get(1).is_some_and(|i| i.connected())
        {
            self.shift_inputs.truncate(1);
            self.shift_inputs_connected.truncate(1);
            self.controller_id = None;
        }
        self.reconnect_remembered()
    }

    /// The devices the rider connected last (R82).
    fn remembered(&self) -> profiles::RememberedDevices {
        profiles::remembered_devices(&self.data_dir, &self.profile.id)
    }

    fn remember(&self, role: DeviceRole, device: &DiscoveredDevice) {
        let remembered = profiles::RememberedDevice {
            id: device.id(),
            name: device.name.clone(),
        };
        if let Err(error) =
            profiles::remember_device(&self.data_dir, &self.profile.id, role, remembered)
        {
            warn!(%error, "cannot remember the device");
        }
    }

    /// No longer reconnects the rider's device in `role`.
    fn forget(&self, role: DeviceRole) -> Result<(), AppError> {
        profiles::forget_device(&self.data_dir, &self.profile.id, role)
            .map_err(|e| AppError::Reported(Failure::new(Kind::SaveSetting, e)))
    }

    /// The system's identifier and the name of the Bluetooth device linked in `role`, if any.
    fn linked(&self, role: DeviceRole) -> Option<(&str, &str)> {
        match role {
            DeviceRole::Trainer => self
                .trainer_id
                .as_deref()
                .zip(self.trainer.as_ref().map(DeviceHandle::name)),
            DeviceRole::HeartRate => self
                .sensor_id
                .as_deref()
                .zip(self.sensor.as_ref().map(DeviceHandle::name)),
            DeviceRole::PowerMeter => self
                .power_meter_id
                .as_deref()
                .zip(self.power_meter.as_ref().map(DeviceHandle::name)),
            DeviceRole::CadenceSensor => self
                .cadence_sensor_id
                .as_deref()
                .zip(self.cadence_sensor.as_ref().map(DeviceHandle::name)),
            DeviceRole::Controller => self
                .controller_id
                .as_deref()
                .zip(self.shift_inputs.get(1).map(|i| i.name())),
        }
    }

    /// Drops the link to the device in `role`, and what it reported (R80); a ride under way
    /// stops counting on it.
    fn let_go(&mut self, role: DeviceRole) {
        match role {
            DeviceRole::Trainer => {
                self.trainer = None;
                self.trainer_id = None;
                self.trainer_connected = None;
                self.forget_reports(DeviceKind::Trainer);
            }
            DeviceRole::HeartRate => {
                self.sensor = None;
                self.sensor_id = None;
                self.sensor_connected = None;
                self.forget_reports(DeviceKind::HeartRateSensor);
                if let Some(active) = &mut self.ride {
                    active.ride.on_heart_rate_lost();
                }
            }
            DeviceRole::PowerMeter => {
                self.power_meter = None;
                self.power_meter_id = None;
                self.power_meter_connected = None;
                self.forget_reports(DeviceKind::PowerMeter);
                if let Some(active) = &mut self.ride {
                    active.ride.on_power_meter_lost();
                }
            }
            DeviceRole::CadenceSensor => {
                self.cadence_sensor = None;
                self.cadence_sensor_id = None;
                self.cadence_sensor_connected = None;
                self.forget_reports(DeviceKind::CadenceSensor);
                if let Some(active) = &mut self.ride {
                    active.ride.on_cadence_sensor_lost();
                }
            }
            DeviceRole::Controller => {
                self.shift_inputs.truncate(1);
                self.shift_inputs_connected.truncate(1);
                self.controller_id = None;
                self.forget_reports(DeviceKind::Controller);
            }
        }
    }

    /// The rider's devices replace those of the rider before (R82): the shifters' buttons do
    /// what this rider gave them, and a device linked now stays only if it is theirs. The fake
    /// trainer is nobody's and stays.
    fn take_riders_devices(&mut self) {
        self.buttons
            .set_all(|model| profiles::button_map(&self.data_dir, &self.profile.id, model));
        let theirs = self.remembered();
        for role in DeviceRole::ALL {
            let others = self
                .linked(role)
                .is_some_and(|(id, name)| !theirs.get(role).is_some_and(|d| d.is(id, name)));
            if others {
                self.let_go(role);
            }
        }
    }

    /// Connects the remembered devices found by a reconnect scan; returns the names of those
    /// not found.
    fn connect_remembered(&mut self) -> Vec<String> {
        let remembered = self.remembered();
        let mut missing = Vec::new();
        for (wanted, kind) in [
            (remembered.trainer, DeviceKind::Trainer),
            (remembered.heart_rate, DeviceKind::HeartRateSensor),
            (remembered.power_meter, DeviceKind::PowerMeter),
            (remembered.cadence_sensor, DeviceKind::CadenceSensor),
            (remembered.controller, DeviceKind::Controller),
        ] {
            let Some(wanted) = wanted else { continue };
            let found = self
                .discovered
                .iter()
                .position(|d| d.kind == kind && d.id() == wanted.id)
                .or_else(|| {
                    self.discovered
                        .iter()
                        .position(|d| d.kind == kind && d.name == wanted.name)
                });
            let connected = match (found, kind) {
                (Some(index), DeviceKind::Trainer) => {
                    self.connect_trainer(TrainerChoice::Discovered(index))
                }
                (Some(index), DeviceKind::HeartRateSensor) => self.connect_heart_rate(index),
                (Some(index), DeviceKind::PowerMeter) => self.connect_power_meter(index),
                (Some(index), DeviceKind::CadenceSensor) => self.connect_cadence_sensor(index),
                (Some(index), DeviceKind::Controller) => self.connect_controller(index),
                (None, _) => Err(AppError::UnknownDevice),
            };
            if connected.is_err() {
                missing.push(wanted.name);
            }
        }
        missing
    }

    /// Whether a scanned device is one of the remembered ones.
    fn is_remembered(&self, device: &DiscoveredDevice) -> bool {
        let remembered = self.remembered();
        let wanted = match device.kind {
            DeviceKind::Trainer => remembered.trainer,
            DeviceKind::HeartRateSensor => remembered.heart_rate,
            DeviceKind::PowerMeter => remembered.power_meter,
            DeviceKind::CadenceSensor => remembered.cadence_sensor,
            DeviceKind::Controller => remembered.controller,
        };
        wanted.is_some_and(|w| w.is(&device.id(), &device.name))
    }

    /// Connects a heart-rate sensor, replacing any previous one.
    ///
    /// # Errors
    /// [`AppError::UnknownDevice`] if the index is not a heart-rate sensor from the last scan.
    pub fn connect_heart_rate(&mut self, index: usize) -> Result<(), AppError> {
        let device = self.discovered_device(index, DeviceKind::HeartRateSensor)?;
        let id = device.id();
        if self.sensor.is_some() && self.sensor_id.as_deref() == Some(id.as_str()) {
            return Ok(());
        }
        let _runtime = self.runtime.enter();
        self.remember(DeviceRole::HeartRate, &device);
        self.forget_reports(DeviceKind::HeartRateSensor);
        self.sensor_id = Some(id);
        self.sensor = Some(self.bluetooth()?.connect(device));
        self.sensor_connected = None;
        Ok(())
    }

    /// Lets the heart-rate sensor go and no longer reconnects it (R76): the rider chose none.
    ///
    /// # Errors
    /// [`AppError::Reported`] if the choice cannot be saved.
    pub fn disconnect_heart_rate(&mut self) -> Result<(), AppError> {
        self.let_go(DeviceRole::HeartRate);
        self.forget(DeviceRole::HeartRate)
    }

    /// Lets the power meter go and no longer reconnects it (R76): the trainer measures again.
    ///
    /// # Errors
    /// [`AppError::Reported`] if the choice cannot be saved.
    pub fn disconnect_power_meter(&mut self) -> Result<(), AppError> {
        self.let_go(DeviceRole::PowerMeter);
        self.forget(DeviceRole::PowerMeter)
    }

    /// Lets the shifter go and no longer reconnects it (R76); the keyboard keeps shifting.
    ///
    /// # Errors
    /// [`AppError::Reported`] if the choice cannot be saved.
    pub fn disconnect_controller(&mut self) -> Result<(), AppError> {
        self.let_go(DeviceRole::Controller);
        self.forget(DeviceRole::Controller)
    }

    /// Connects a power meter, replacing any previous one: its power, and its cadence if it
    /// reports one, count over the trainer's (R71).
    ///
    /// # Errors
    /// [`AppError::UnknownDevice`] if the index is not a power meter from the last scan.
    pub fn connect_power_meter(&mut self, index: usize) -> Result<(), AppError> {
        let device = self.discovered_device(index, DeviceKind::PowerMeter)?;
        let id = device.id();
        if self.power_meter.is_some() && self.power_meter_id.as_deref() == Some(id.as_str()) {
            return Ok(());
        }
        let _runtime = self.runtime.enter();
        self.remember(DeviceRole::PowerMeter, &device);
        self.forget_reports(DeviceKind::PowerMeter);
        self.power_meter_id = Some(id);
        self.power_meter = Some(self.bluetooth()?.connect(device));
        self.power_meter_connected = None;
        if let Some(active) = &mut self.ride {
            // The meter replaced may have left its values.
            active.ride.on_power_meter_lost();
        }
        Ok(())
    }

    /// Connects a cadence sensor, replacing any previous one; its cadence counts first (R72).
    ///
    /// # Errors
    /// [`AppError::UnknownDevice`] if the index is not a cadence sensor from the last scan,
    /// [`AppError::Reported`] if the choice cannot be saved.
    pub fn connect_cadence_sensor(&mut self, index: usize) -> Result<(), AppError> {
        let device = self.discovered_device(index, DeviceKind::CadenceSensor)?;
        profiles::set_cadence_source(&self.data_dir, &self.profile.id, CadenceSource::Automatic)
            .map_err(|e| AppError::Reported(Failure::new(Kind::SaveSetting, e)))?;
        let id = device.id();
        if self.cadence_sensor.is_some() && self.cadence_sensor_id.as_deref() == Some(id.as_str()) {
            return Ok(());
        }
        let _runtime = self.runtime.enter();
        self.remember(DeviceRole::CadenceSensor, &device);
        self.forget_reports(DeviceKind::CadenceSensor);
        self.cadence_sensor_id = Some(id);
        self.cadence_sensor = Some(self.bluetooth()?.connect(device));
        self.cadence_sensor_connected = None;
        if let Some(active) = &mut self.ride {
            active.ride.on_cadence_sensor_lost();
        }
        Ok(())
    }

    /// Which device's cadence counts for the rider (R72, R82).
    #[must_use]
    pub fn cadence_source(&self) -> CadenceSource {
        profiles::cadence_source(&self.data_dir, &self.profile.id)
    }

    /// Takes the cadence from `source` from the next ride on, without a cadence sensor: the
    /// one connected is let go and no longer reconnected (R72).
    ///
    /// # Errors
    /// [`AppError::Reported`] if the choice cannot be saved.
    pub fn set_cadence_source(&mut self, source: CadenceSource) -> Result<(), AppError> {
        self.let_go(DeviceRole::CadenceSensor);
        self.forget(DeviceRole::CadenceSensor)?;
        profiles::set_cadence_source(&self.data_dir, &self.profile.id, source)
            .map_err(|e| AppError::Reported(Failure::new(Kind::SaveSetting, e)))
    }

    /// Connects a shifter (a Shimano Di2 or a Zwift Click) whose buttons shift or do what else
    /// the rider gave them (R7, R75, #139), replacing any previous one; the keyboard keeps
    /// shifting too.
    ///
    /// # Errors
    /// [`AppError::UnknownDevice`] if the index is not a controller from the last scan.
    pub fn connect_controller(&mut self, index: usize) -> Result<(), AppError> {
        let device = self.discovered_device(index, DeviceKind::Controller)?;
        let id = device.id();
        if self.controller_id.as_deref() == Some(id.as_str()) {
            return Ok(());
        }
        let _runtime = self.runtime.enter();
        self.remember(DeviceRole::Controller, &device);
        self.forget_reports(DeviceKind::Controller);
        // A scan finds every shifter with its model: the Di2 fallback is never taken.
        let model = device.model.unwrap_or(ControllerModel::Di2);
        let handle = self.bluetooth()?.connect(device);
        self.controller_id = Some(id);
        let buttons = Arc::clone(&self.buttons);
        self.add_shift_input(Box::new(orqwa_devices::shift::Controller::new(
            handle, model, buttons,
        )));
        Ok(())
    }

    /// What the buttons of a shifter `model` do (#139, R84): a Di2's D-Fly channels 1–4, a
    /// Zwift Click's + (button 1) and − (button 2), which has no double press, or a Click v2's
    /// ten.
    #[must_use]
    pub fn button_map(&self, model: ControllerModel) -> ButtonMap {
        self.buttons.get(model)
    }

    /// Gives the buttons of a shifter `model` what `map` says, at once and for the rider's next
    /// time; the other models' stay as they are.
    ///
    /// # Errors
    /// [`AppError::Reported`] if the choice cannot be saved.
    pub fn set_button_map(
        &mut self,
        model: ControllerModel,
        map: ButtonMap,
    ) -> Result<(), AppError> {
        self.buttons.set(model, map);
        profiles::set_button_map(&self.data_dir, &self.profile.id, model, &map)
            .map_err(|e| AppError::Reported(Failure::new(Kind::SaveSetting, e)))
    }

    /// Shifts from `input` from now on, instead of the shifter connected before (R7).
    pub fn add_shift_input(&mut self, input: Box<dyn ShiftInput>) {
        // The keyboard stays first; one controller at a time after it.
        self.shift_inputs.truncate(1);
        self.shift_inputs_connected.truncate(1);
        self.shift_inputs_connected.push(false);
        self.shift_inputs.push(input);
    }

    /// Starts riding the loaded route as the active rider, whose profile sets the mass. The
    /// clock starts once the trainer is connected.
    ///
    /// # Errors
    /// [`AppError::NoRoute`] or [`AppError::NoTrainer`] if either is missing.
    pub fn start_ride(
        &mut self,
        difficulty: Percent,
        descent: DescentMode,
        ghost: &GhostChoice,
    ) -> Result<(), AppError> {
        let route = self.route.clone().ok_or(AppError::NoRoute)?;
        if self.trainer.is_none() {
            return Err(AppError::NoTrainer);
        }
        let ghost = self.ghost_for(&route, descent, ghost)?;
        let along = self.video.as_ref().filter(|_| self.view == View::Video);
        let player = along
            .map(|v| video::VideoPlayer::open(&v.video))
            .transpose()
            .map_err(|e| AppError::Reported(Failure::new(Kind::PlayVideo, e)))?;
        // A ride without the video's sound is still a ride.
        let sound = along.and_then(|v| {
            video::SoundPlayer::open(&v.video)
                .inspect_err(|error| warn!(%error, "no sound for the video"))
                .ok()
                .flatten()
        });
        let records = self.records_for(&route);
        let ride = Ride::new(route, self.ride_config(difficulty, descent));
        self.begin(ride, None, records, ghost, (player, sound));
        Ok(())
    }

    /// Starts `workout` as the active rider (R56, R58): on the loaded course in its 3D world
    /// if `on_course` (the gradient then sets only the speed), otherwise on its own on a flat
    /// road, going into the history as `name`. The clock starts once the trainer is connected.
    ///
    /// # Errors
    /// [`AppError::NoTrainer`] without a trainer, [`AppError::NoRoute`] if `on_course` but no
    /// course is loaded.
    pub fn start_workout(
        &mut self,
        workout: Workout,
        on_course: bool,
        descent: DescentMode,
        name: &str,
    ) -> Result<(), AppError> {
        if self.trainer.is_none() {
            return Err(AppError::NoTrainer);
        }
        let config = self.ride_config(Percent(100.0), descent);
        if on_course {
            let route = self.route.clone().ok_or(AppError::NoRoute)?;
            self.view = View::World;
            let records = self.records_for(&route);
            let ride = Ride::new(route, config).with_workout(workout);
            self.begin(ride, None, records, None, (None, None));
        } else {
            let ride = Ride::workout(workout, config);
            let records = RouteRecords::default();
            self.begin(ride, Some(name.to_owned()), records, None, (None, None));
        }
        Ok(())
    }

    /// The workouts to choose from (R21): those planned for the day on the rider's platforms
    /// (R81), then the built-in ones, then the library's files.
    #[must_use]
    pub fn workouts(&self) -> Vec<orqwa_workouts::Entry> {
        self.planned_workouts()
            .into_iter()
            .map(|(_, entry)| entry)
            .chain(orqwa_workouts::library(&orqwa_workouts::library_dir(
                &self.data_dir,
            )))
            .collect()
    }

    /// Adds a workout file (ZWO, ERG, MRC or FIT) to the library; returns its id.
    ///
    /// # Errors
    /// [`AppError::Reported`] if it is no workout ORQWA understands or cannot be copied.
    pub fn import_workout(&mut self, file: &Path) -> Result<String, AppError> {
        orqwa_workouts::import(&orqwa_workouts::library_dir(&self.data_dir), file)
            .map(|path| path.display().to_string())
            .map_err(|e| AppError::Reported(Failure::new(Kind::ImportWorkout, e)))
    }

    /// Saves a workout made in the editor into the library (over `replace` if that is the
    /// library's ZWO file being edited); returns its id.
    ///
    /// # Errors
    /// [`AppError::Reported`] if it has no steps or cannot be written.
    pub fn save_workout(
        &mut self,
        plan: &orqwa_domain::workout::Plan,
        replace: Option<&str>,
    ) -> Result<String, AppError> {
        let dir = orqwa_workouts::library_dir(&self.data_dir);
        orqwa_workouts::save(&dir, plan, self.profile.profile.ftp, replace)
            .map_err(|e| AppError::Reported(Failure::new(Kind::SaveWorkout, e)))
    }

    /// Deletes a workout file from the library.
    ///
    /// # Errors
    /// [`AppError::Reported`] for built-ins and files that cannot be deleted.
    pub fn delete_workout(&mut self, id: &str) -> Result<(), AppError> {
        orqwa_workouts::delete(&orqwa_workouts::library_dir(&self.data_dir), id)
            .map_err(|e| AppError::Reported(Failure::new(Kind::DeleteWorkout, e)))
    }

    /// A structured workout by its id (see [`App::workouts`]), for the active rider's FTP.
    ///
    /// # Errors
    /// [`AppError::Reported`] if it cannot be read.
    pub fn structured_workout(&self, id: &str) -> Result<Workout, AppError> {
        let planned = self
            .planned_workouts()
            .into_iter()
            .find(|(_, entry)| entry.id == id)
            .map(|(_, entry)| entry.plan);
        let plan = match planned {
            Some(plan) => plan,
            None => orqwa_workouts::load(id)
                .map_err(|e| AppError::Reported(Failure::new(Kind::LoadWorkout, e)))?,
        };
        Ok(Workout::Structured {
            plan: Arc::new(plan),
            ftp: self.profile.profile.ftp,
        })
    }

    /// Makes `ftp` the active rider's FTP, e.g. from an FTP test (R22).
    ///
    /// # Errors
    /// [`AppError::Reported`] if the profile cannot be saved.
    pub fn set_ftp(&mut self, ftp: Watts) -> Result<(), AppError> {
        let mut profile = self.profile.profile.clone();
        profile.ftp = ftp;
        let id = self.profile.id.clone();
        self.save_profile(Some(&id), profile).map(|_| ())
    }

    /// Changes the workout of the current workout, e.g. its target power or heart rate.
    pub fn change_workout(&mut self, workout: Workout) {
        if let Some(active) = &mut self.ride {
            active.ride.change_workout(workout);
        }
    }

    /// The active rider on their bike.
    fn rider_setup(&self) -> RiderSetup {
        RiderSetup {
            mass: self.profile.profile.system_mass(),
            ..RiderSetup::default()
        }
    }

    fn ride_config(&self, difficulty: Percent, descent: DescentMode) -> RideConfig {
        RideConfig {
            setup: self.rider_setup(),
            difficulty,
            descent,
            gears: virtual_gears(self.profile.profile.drivetrain),
            cadence: self.cadence_source(),
        }
    }

    /// A shift key was pressed (R7); the ride shifts on the next update.
    pub fn shift(&mut self, shift: Shift) {
        self.keys.press(shift);
    }

    fn begin(
        &mut self,
        ride: Ride,
        name: Option<String>,
        records: RouteRecords,
        ghost: Option<Ghost>,
        (player, sound): (Option<video::VideoPlayer>, Option<video::SoundPlayer>),
    ) {
        let milestones =
            moments::Milestones::new(ride.route().map(Route::length), self.profile.profile.units);
        let peaks = moments::PowerPeaks::new(analysis::record_powers(
            self.ride_records().iter().map(|(_, record)| record),
        ));
        let greeting = self
            .loaded
            .as_ref()
            .filter(|_| ride.route().is_some() && !records.ridden)
            .map(|(course, _)| moments::Moment::FirstRide {
                course: course.clone(),
            });
        self.ride = Some(ActiveRide {
            ride,
            name,
            // With the trainer connected already (e.g. while the world was built), the ride
            // starts now; otherwise when it connects.
            started: self.trainer_connected.map(|_| SystemTime::now()),
            finished: false,
            records,
            next_climb: 0,
            milestones,
            peaks,
            steps: moments::StepsHeld::new(self.profile.profile.ftp),
            greeting,
            summary: (0, RideSummary::default()),
            time_to_go: estimate::TimeToGo::default(),
            ghost,
            player,
            sound,
            ambient: ambient::Sound::new(
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map_or(259, |d| d.as_secs() ^ u64::from(d.subsec_nanos())),
            ),
            ambient_settings: ambient::Settings::default(),
            video_watch: VideoWatch {
                since: std::time::Instant::now(),
                frames: 0,
                reported: false,
            },
            time_scale: 1.0,
            paused: false,
            simulated: false,
        });
    }

    /// Pauses or resumes the ride: paused, the clock, the road and the samples wait, and the
    /// trainer is freed of its resistance; resumed, it gets its gradient or power back on the
    /// next tick. Returns whether the ride is paused afterwards.
    pub fn set_paused(&mut self, paused: bool) -> bool {
        let Some(active) = &mut self.ride else {
            return false;
        };
        if active.finished || active.paused == paused {
            return active.paused;
        }
        active.paused = paused;
        if paused {
            if let Some(trainer) = &self.trainer
                && let Err(error) = trainer.try_control(TrainerControl::Resistance(Percent(0.0)))
            {
                warn!(%error, "cannot free the trainer");
            }
        } else {
            active.ride.resend_control();
        }
        paused
    }

    /// Whether the ride is paused.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.ride.as_ref().is_some_and(|active| active.paused)
    }

    /// Changes trainer difficulty and descent mode of the current ride (R48).
    pub fn adjust_ride(&mut self, difficulty: Percent, descent: DescentMode) {
        if let Some(active) = &mut self.ride {
            active.ride.adjust(difficulty, descent);
        }
    }

    /// Makes the current ride's trainer difficulty `steps` steps harder (easier if negative),
    /// as a shifter's button asks (#139); the new difficulty, `None` when not riding.
    pub fn step_difficulty(&mut self, steps: i32) -> Option<Percent> {
        Some(self.ride.as_mut()?.ride.step_difficulty(steps))
    }

    /// Makes the workout's powers `steps` steps harder (easier if negative), as a shifter's
    /// button asks (#139); the share of the planned powers now asked for, `None` when not in
    /// a workout that can change them.
    pub fn step_intensity(&mut self, steps: i32) -> Option<Percent> {
        self.ride.as_mut()?.ride.step_intensity(steps)
    }

    /// Goes on with the next step of a structured workout (#139); whether there was one.
    pub fn skip_step(&mut self) -> bool {
        self.ride
            .as_mut()
            .is_some_and(|active| active.ride.skip_step())
    }

    /// Ends the ride without saving anything (R49).
    pub fn abort_ride(&mut self) {
        self.ride = None;
    }

    /// Ends the ride and saves it; reports [`AppEvent::RideSaved`] on the next update.
    pub fn finish_ride(&mut self) -> Vec<AppEvent> {
        let Some(active) = self.ride.take() else {
            return Vec::new();
        };
        let Some(start) = active.started else {
            return Vec::new();
        };
        if active.ride.samples().is_empty() {
            return Vec::new();
        }
        let path = profiles::rides_dir(&self.data_dir, &self.profile.id)
            .join(paths::activity_file_name(start));
        let samples = active.ride.samples();
        let saved = orqwa_storage::encode_fit(start, samples)
            .map_err(|e| Failure::new(Kind::SaveFile, e).arg(path.display()))
            .and_then(|fit| {
                std::fs::create_dir_all(path.parent().unwrap_or(&self.data_dir))
                    .and_then(|()| std::fs::write(&path, fit))
                    .map_err(|e| Failure::new(Kind::SaveFile, e).arg(path.display()))
            });
        if saved.is_ok() {
            let route = active.ride.route();
            let finished = samples
                .last()
                .zip(route)
                .is_some_and(|(s, route)| s.distance.0 >= route.length().0 - 1.0);
            // A simulated ride's times are not real: like a ride known only from its FIT file,
            // it counts towards no records; neither does a workout without a route.
            let real = !active.simulated;
            let record = RideRecord {
                route: active
                    .name
                    .clone()
                    .or_else(|| self.loaded.as_ref().map(|(name, _)| name.clone()))
                    .unwrap_or_else(|| "Ride".to_owned()),
                start,
                summary: summarize(samples, self.profile.profile.ftp),
                route_key: route.filter(|_| real).map(Route::key),
                route_time: route
                    .filter(|_| finished && real)
                    .and_then(|route| effort(samples, Meters(0.0), route.length()))
                    .map(|e| e.elapsed),
                climbs: route
                    .map_or(&[][..], Route::climbs)
                    .iter()
                    .filter(|_| real)
                    .filter_map(|c| {
                        effort(samples, c.start, c.end).map(|e| ClimbTime {
                            start: c.start,
                            end: c.end,
                            elapsed: e.elapsed,
                            avg_power: e.avg_power,
                        })
                    })
                    .collect(),
                name: None,
                ftp_estimate: active.ride.ftp_estimate(),
                rider_mass: Some(self.profile.profile.rider_mass),
                simulated: active.simulated,
                analysis: Some(analyse(samples)),
                uploads: std::collections::BTreeMap::new(),
            };
            // The FIT file is what counts; the history rebuilds missing metadata from it.
            if let Err(error) = rides::save(&path, &record) {
                warn!(%error, "cannot save ride metadata");
            }
        }
        vec![match saved {
            Ok(()) => AppEvent::RideSaved(path),
            Err(message) => AppEvent::Error(message),
        }]
    }

    /// The active rider's rides with their metadata, newest first: from the history index
    /// where it knows them as their files are now (R79), else read from the files. Rides
    /// without metadata, e.g. FIT files copied in by hand, are analysed once and get it
    /// written; so do rides without the data the analysis across rides needs (R62), e.g. saved
    /// by an older ORQWA.
    fn ride_records(&self) -> Vec<(PathBuf, RideRecord)> {
        let mut load = |fit: &Path| {
            let record = rides::load(fit)
                .or_else(|_| self.rebuild_metadata(fit))
                .map(|record| with_analysis(fit, record));
            match record {
                Ok(record) => Some(record),
                Err(error) => {
                    warn!(path = %fit.display(), %error, "skipping ride");
                    None
                }
            }
        };
        let dir = self.rides_dir();
        let mut index = self.index.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(index) = index.as_mut() {
            match index.records(&dir, &mut load) {
                Ok(records) => return records,
                Err(error) => warn!(%error, "reading every ride's file instead"),
            }
        }
        rides::fit_files(&dir)
            .into_iter()
            .filter_map(|fit| load(&fit).map(|record| (fit, record)))
            .collect()
    }

    /// The active rider's rides, newest first (R31), with the records each set.
    #[must_use]
    pub fn history(&self) -> Vec<HistoryEntry> {
        let records = self.ride_records();
        let all: Vec<&RideRecord> = records.iter().map(|(_, r)| r).collect();
        let power_bests = analysis::record_powers(all.iter().copied());
        records
            .iter()
            .map(|(fit, record)| {
                let same_route: Vec<&RideRecord> = all
                    .iter()
                    .copied()
                    .filter(|other| {
                        other.route_key.is_some() && other.route_key == record.route_key
                    })
                    .collect();
                let best_route = same_route.iter().filter_map(|r| r.route_time).min();
                HistoryEntry {
                    fit: fit.clone(),
                    route_record: record.route_time.is_some() && record.route_time == best_route,
                    climb_records: record
                        .climbs
                        .iter()
                        .map(|climb| Some(climb.elapsed) == best_climb_time(&same_route, climb))
                        .collect(),
                    power_records: analysis::power_records(record, &power_bests),
                    record: record.clone(),
                }
            })
            .collect()
    }

    /// What the summary of the active rider's ride saved as `fit` says of it beyond its
    /// figures and records (R124): its first ride on the course, or how far into their week it
    /// is, local time `utc_offset_s` ahead of UTC.
    #[must_use]
    pub fn recap(&self, fit: &Path, utc_offset_s: i64) -> Option<moments::Moment> {
        let records = self.ride_records();
        let (_, record) = records.iter().find(|(path, _)| path == fit)?;
        let others: Vec<&RideRecord> = records
            .iter()
            .filter(|(path, _)| path != fit)
            .map(|(_, other)| other)
            .collect();
        moments::recap(
            record,
            &others,
            self.profile.profile.rides_per_week,
            utc_offset_s,
        )
    }

    /// The active rider's best times on `route`, from earlier rides on the same course.
    #[must_use]
    pub fn records_for(&self, route: &Route) -> RouteRecords {
        let key = route.key();
        let history = self.history();
        let same_route: Vec<&RideRecord> = history
            .iter()
            .map(|entry| &entry.record)
            .filter(|record| record.route_key.as_deref() == Some(key.as_str()))
            .collect();
        RouteRecords {
            ridden: !same_route.is_empty(),
            route: same_route.iter().filter_map(|r| r.route_time).min(),
            climbs: route
                .climbs()
                .iter()
                .map(|c| {
                    let probe = ClimbTime {
                        start: c.start,
                        end: c.end,
                        elapsed: Duration::ZERO,
                        avg_power: None,
                    };
                    best_climb_time(&same_route, &probe)
                })
                .collect(),
        }
    }

    /// The active rider's power curves (R62): the best efforts among the rides of the last
    /// `period` (all rides for `None`) and among those of `comparison`; see
    /// [`analysis::power_curves`].
    #[must_use]
    pub fn power_curves(
        &self,
        period: Option<Duration>,
        comparison: analysis::Comparison,
        per_kg: bool,
    ) -> analysis::PowerCurves {
        analysis::power_curves(
            &self.history(),
            period,
            comparison,
            SystemTime::now(),
            self.profile.profile.rider_mass,
            per_kg,
        )
    }

    /// The active rider's power profile (R63): the best of the rides of the last `period` (all
    /// rides for `None`) against their best of all; see [`analysis::power_profile`].
    #[must_use]
    pub fn power_profile(
        &self,
        period: Option<Duration>,
        per_kg: bool,
    ) -> Vec<analysis::ProfileAxis> {
        analysis::power_profile(
            &self.history(),
            period,
            SystemTime::now(),
            self.profile.profile.rider_mass,
            per_kg,
        )
    }

    /// The active rider's fitness, fatigue and form for each local day of the last `period`
    /// (since their first ride for `None`), local time `utc_offset_s` ahead of UTC (R64).
    #[must_use]
    pub fn fitness(&self, period: Option<Duration>, utc_offset_s: i64) -> Vec<analysis::DayLoad> {
        let history = self.history();
        let (first, last) =
            analysis::period_days(&history, period, SystemTime::now(), utc_offset_s);
        analysis::day_loads(&history, first, last, utc_offset_s)
    }

    /// The active rider's weeks of the last `period` (since their first ride for `None`), with
    /// the time in their zones today (R64); local time `utc_offset_s` ahead of UTC.
    #[must_use]
    pub fn weekly(&self, period: Option<Duration>, utc_offset_s: i64) -> Vec<analysis::WeekTotals> {
        let history = self.history();
        let (first, last) =
            analysis::period_days(&history, period, SystemTime::now(), utc_offset_s);
        analysis::weekly(&history, &self.profile.profile, first, last, utc_offset_s)
    }

    /// The active rider's progress (R65) over the last `period` (since their first ride for
    /// `None`), week by week or month by month; local time `utc_offset_s` ahead of UTC.
    #[must_use]
    pub fn progress(
        &self,
        period: Option<Duration>,
        utc_offset_s: i64,
        per_kg: bool,
    ) -> analysis::Progress {
        let history = self.history();
        let (first, last) =
            analysis::period_days(&history, period, SystemTime::now(), utc_offset_s);
        analysis::progress(
            &history,
            first,
            last,
            utc_offset_s,
            self.profile.profile.rider_mass,
            per_kg,
        )
    }

    /// The FTP the active rider's best efforts of the last 90 days show (R65), offered and
    /// never applied by itself; `None` without an effort of 20 minutes.
    #[must_use]
    pub fn ftp_estimate(&self) -> Option<orqwa_session::ftp_estimate::FtpEstimate> {
        let curves = analysis::power_curves(
            &self.history(),
            Some(analysis::FTP_ESTIMATE_PERIOD),
            analysis::Comparison::AllTime,
            SystemTime::now(),
            self.profile.profile.rider_mass,
            false,
        );
        let efforts: Vec<(Duration, Watts)> = curves
            .period
            .iter()
            .map(|effort| (effort.duration, effort.power))
            .collect();
        orqwa_session::ftp_estimate::estimate_ftp(&efforts)
    }

    /// The active rider's HUD metrics, in order (R23); the first is shown large.
    #[must_use]
    pub fn hud_layout(&self) -> Vec<String> {
        self.hud_layout_of(&self.profile.id)
    }

    /// Any rider's HUD metrics, in order: the default layout for one who never chose any.
    #[must_use]
    pub fn hud_layout_of(&self, id: &str) -> Vec<String> {
        hud::sanitize(&profiles::load_hud(&self.data_dir, id).unwrap_or_default())
    }

    /// Saves the active rider's HUD metrics; unknown or repeated ones are dropped. Returns the
    /// layout as saved.
    ///
    /// # Errors
    /// [`AppError::Reported`] if it cannot be written.
    pub fn set_hud_layout(&mut self, layout: &[String]) -> Result<Vec<String>, AppError> {
        let layout = hud::sanitize(layout);
        profiles::save_hud(&self.data_dir, &self.profile.id, &layout)
            .map_err(|e| AppError::Reported(Failure::new(Kind::SaveHud, e)))?;
        Ok(layout)
    }

    /// Live values of all HUD metrics while riding (see [`hud::values`]).
    #[must_use]
    pub fn hud_values(&self) -> Vec<(&'static str, Option<f64>)> {
        let Some(active) = &self.ride else {
            return Vec::new();
        };
        let state = active.ride.state();
        hud::values(
            &state,
            active.ride.samples(),
            &active.summary.1,
            active.ride.route(),
            &self.profile.profile,
            active.time_to_go.at(state.elapsed),
        )
    }

    /// Tells the music app to play/pause or skip (R26), in the background; failures are
    /// reported as [`AppEvent::Error`].
    pub fn control_music(&mut self, command: media::MediaCommand) {
        let tx = self.jobs_tx.clone();
        self.runtime.spawn_blocking(move || {
            if let Err(message) = media::send(command) {
                let _ = tx.send(JobResult::Failed(message));
            }
        });
    }

    /// How long the active rider would take for the loaded route at the planned share of their
    /// FTP, with their weight and `descent` (R66); `None` without a route, or if that power
    /// would not get them to the finish.
    #[must_use]
    pub fn estimated_time(&self, descent: DescentMode) -> Option<Duration> {
        estimate::time_to_finish(
            self.route()?,
            &self.rider_setup(),
            descent,
            Meters(0.0),
            MetersPerSecond(0.0),
            estimate::planned_power(self.profile.profile.ftp),
        )
    }

    /// The ghost of the current ride, if any.
    #[must_use]
    pub fn ghost_state(&self) -> Option<GhostState> {
        let active = self.ride.as_ref()?;
        let ghost = active.ghost.as_ref()?;
        let state = active.ride.state();
        Some(GhostState {
            name: ghost.name.clone(),
            distance: ghost.distance_at(state.elapsed),
            gap: ghost.gap(state.distance, state.elapsed),
        })
    }

    fn ghost_for(
        &self,
        route: &Route,
        descent: DescentMode,
        choice: &GhostChoice,
    ) -> Result<Option<Ghost>, AppError> {
        let profile = &self.profile.profile;
        let setup = self.rider_setup();
        let unavailable = |why: String| AppError::GhostUnavailable(why);
        let ghost = match choice {
            GhostChoice::None => return Ok(None),
            GhostChoice::PersonalBest => {
                let key = route.key();
                let best = self
                    .history()
                    .into_iter()
                    .filter(|h| h.record.route_key.as_deref() == Some(key.as_str()))
                    .filter_map(|h| h.record.route_time.map(|t| (t, h.fit)))
                    .min_by_key(|(t, _)| *t)
                    .ok_or_else(|| unavailable("no finished ride on this route yet".to_owned()))?;
                let (_, samples) = read_fit(&best.1).map_err(unavailable)?;
                Ghost::from_samples("Your best", &samples)
            }
            GhostChoice::Power(watts) => Some(Ghost::pacer(
                &format!("Pacer {:.0} W", watts.0),
                route,
                &setup,
                descent,
                *watts,
            )),
            GhostChoice::WattsPerKg(ratio) => Some(Ghost::pacer(
                &format!("Pacer {ratio:.1} W/kg"),
                route,
                &setup,
                descent,
                Watts(ratio * profile.rider_mass.0),
            )),
            GhostChoice::Activity(path) => {
                let points = activity_points(path).map_err(AppError::Reported)?;
                let name = path
                    .file_stem()
                    .map_or_else(|| "Ghost".to_owned(), |s| s.to_string_lossy().into_owned());
                Some(Ghost::from_activity(&name, route, &points).ok_or_else(|| {
                    AppError::Reported(
                        Failure::new(
                            Kind::GhostRoute,
                            format!("{} does not follow this route", path.display()),
                        )
                        .arg(path.display()),
                    )
                })?)
            }
        };
        Ok(ghost)
    }

    /// The climb the rider is on now, if any.
    #[must_use]
    pub fn current_climb(&self) -> Option<ClimbProgress> {
        let active = self.ride.as_ref()?;
        let state = active.ride.state();
        let (index, climb) = active
            .ride
            .route()?
            .climbs()
            .iter()
            .enumerate()
            .find(|(_, c)| c.start.0 <= state.distance.0 && state.distance.0 < c.end.0)?;
        let started = time_at(active.ride.samples(), climb.start).unwrap_or(state.elapsed);
        Some(ClimbProgress {
            index,
            climb: *climb,
            ridden: Meters(state.distance.0 - climb.start.0),
            elapsed: state.elapsed.saturating_sub(started),
            best: active.records.climbs.get(index).copied().flatten(),
        })
    }

    /// The recorded data of one ride, with zones of the active rider.
    ///
    /// # Errors
    /// [`AppError::Reported`] if the FIT file cannot be read.
    pub fn ride_detail(&self, fit: &Path) -> Result<RideDetail, AppError> {
        let (_, samples) =
            read_fit(fit).map_err(|e| AppError::Reported(Failure::new(Kind::LoadRide, e)))?;
        let profile = &self.profile.profile;
        Ok(RideDetail {
            power_zones: time_in_power_zones(&samples, profile),
            heart_rate_zones: time_in_heart_rate_zones(&samples, profile),
            samples,
        })
    }

    /// Names a ride (R50); an empty name goes back to the route and date. Only the metadata
    /// changes, so file names stay stable for syncing.
    ///
    /// # Errors
    /// [`AppError::Reported`] if the metadata cannot be read or written.
    pub fn rename_ride(&self, fit: &Path, name: &str) -> Result<(), AppError> {
        let storage = |e: rides::RideError| AppError::Reported(Failure::new(Kind::RenameRide, e));
        let mut record = rides::load(fit)
            .or_else(|_| self.rebuild_metadata(fit))
            .map_err(|e| AppError::Reported(Failure::new(Kind::RenameRide, e)))?;
        let name = name.trim();
        record.name = (!name.is_empty()).then(|| name.to_owned());
        rides::save(fit, &record).map_err(storage)
    }

    /// Deletes a ride: its FIT file and metadata.
    ///
    /// # Errors
    /// [`AppError::Reported`] if the FIT file cannot be removed.
    pub fn delete_ride(&self, fit: &Path) -> Result<(), AppError> {
        std::fs::remove_file(fit).map_err(|e| {
            AppError::Reported(Failure::new(Kind::DeleteFile, e).arg(fit.display()))
        })?;
        let _ = std::fs::remove_file(rides::metadata_path(fit));
        Ok(())
    }

    /// Saves a copy of a ride's FIT file at `to`, a place the rider chose outside the data
    /// directory (R28).
    ///
    /// # Errors
    /// [`AppError::Reported`] if the FIT file cannot be read or the copy not written.
    pub fn export_ride(&self, fit: &Path, to: &Path) -> Result<(), AppError> {
        rides::export(fit, to)
            .map_err(|e| AppError::Reported(Failure::new(Kind::ExportFile, e).arg(to.display())))
    }

    /// The file name to suggest when exporting the ride titled `title` (R50).
    #[must_use]
    pub fn ride_export_file_name(title: &str) -> String {
        rides::export_file_name(title)
    }

    fn rides_dir(&self) -> PathBuf {
        profiles::rides_dir(&self.data_dir, &self.profile.id)
    }

    fn rebuild_metadata(&self, fit: &Path) -> Result<RideRecord, String> {
        let (start, samples) = read_fit(fit)?;
        let record = RideRecord {
            route: fit
                .file_stem()
                .map_or_else(|| "Ride".to_owned(), |s| s.to_string_lossy().into_owned()),
            start,
            summary: summarize(&samples, self.profile.profile.ftp),
            // The route is unknown, so this ride counts towards no records.
            route_key: None,
            route_time: None,
            climbs: Vec::new(),
            name: None,
            ftp_estimate: None,
            rider_mass: None,
            simulated: false,
            analysis: Some(analyse(&samples)),
            uploads: std::collections::BTreeMap::new(),
        };
        if let Err(error) = rides::save(fit, &record) {
            warn!(%error, "cannot save rebuilt ride metadata");
        }
        Ok(record)
    }

    /// Feeds the ride the shifts asked for, passes on the other controls, and reports the
    /// shifter's connection and why it fails to connect (R80).
    fn poll_shift_inputs(&mut self, events: &mut Vec<AppEvent>) {
        for (input, was_connected) in self
            .shift_inputs
            .iter_mut()
            .zip(&mut self.shift_inputs_connected)
        {
            drive_shift_input(input.as_mut(), &mut self.ride, was_connected, events);
            if *was_connected {
                self.failures.remove(&DeviceKind::Controller);
            } else if let Some(reason) = input.failure()
                && self
                    .failures
                    .get(&DeviceKind::Controller)
                    .map(String::as_str)
                    != Some(reason)
            {
                self.failures
                    .insert(DeviceKind::Controller, reason.to_owned());
                events.push(AppEvent::ConnectionFailed {
                    name: input.name().to_owned(),
                    reason: reason.to_owned(),
                });
            }
        }
    }

    /// Processes everything that happened since the last call and advances the ride by `dt`.
    pub fn update(&mut self, dt: Duration) -> Vec<AppEvent> {
        let mut events = Vec::new();
        self.poll_jobs(&mut events);
        self.poll_trainer(&mut events);
        self.poll_sensors(&mut events);
        self.poll_shift_inputs(&mut events);
        self.poll_phone_remote(&mut events);

        if let Some(active) = &mut self.ride
            && active.started.is_some()
            && !active.finished
            && !active.paused
        {
            // Sped up, the ride advances in small steps all the same, so its physics and
            // one-second samples stay as exact as at real speed.
            let mut left = dt.mul_f64(active.time_scale);
            let mut control = None;
            while !left.is_zero() {
                let step = left.min(MAX_RIDE_STEP);
                left -= step;
                control = active.ride.tick(step).or(control);
            }
            if let Some(control) = control
                && let Some(trainer) = &self.trainer
                && let Err(error) = trainer.try_control(control)
            {
                warn!(%error, "cannot control trainer");
            }
            let samples = active.ride.samples();
            if samples.len() != active.summary.0 {
                let ftp = self.profile.profile.ftp;
                let summary = summarize(samples, ftp);
                active.time_to_go.update(summary.elapsed, || {
                    let power = estimate::expected_power(ftp, summary.avg_power, summary.elapsed);
                    active.ride.time_to_finish(power)
                });
                active.summary = (samples.len(), summary);
                let workout = active.ride.state().workout;
                events.extend(
                    active
                        .steps
                        .follow(samples, workout.as_ref())
                        .map(AppEvent::Moment),
                );
                // A simulated ride's powers are not real either.
                if !active.simulated {
                    events.extend(
                        active
                            .peaks
                            .follow(samples)
                            .into_iter()
                            .map(AppEvent::Moment),
                    );
                }
            }
            events.extend(active.greeting.take().map(AppEvent::Moment));
            // Samples arrive once a second, so a climb is timed once a sample lies past its top.
            let climbs = active.ride.route().map_or(&[][..], Route::climbs);
            while let Some(climb) = climbs.get(active.next_climb)
                && let Some(done) = effort(active.ride.samples(), climb.start, climb.end)
            {
                events.push(AppEvent::Moment(moments::climb_topped(
                    active.next_climb,
                    done.elapsed,
                    climb.average_grade,
                    active
                        .records
                        .climbs
                        .get(active.next_climb)
                        .copied()
                        .flatten(),
                    !active.simulated,
                )));
                active.next_climb += 1;
            }
            // The finish is the summary's to tell (R113).
            if active.ride.is_finished() {
                active.finished = true;
                events.push(AppEvent::RideFinished);
            } else {
                let distance = active.ride.state().distance;
                events.extend(
                    active
                        .milestones
                        .reach(distance)
                        .into_iter()
                        .map(AppEvent::Moment),
                );
            }
        }
        if let (Some(active), Some(video)) = (&self.ride, &self.video) {
            let time = video.time_at(active.ride.state().distance);
            if let Some(player) = &active.player {
                player.show(time);
            }
            if let Some(sound) = &active.sound {
                sound.follow(time);
            }
        }
        self.watch_video(&mut events);
        events
    }

    /// The current ride's samples so far, one per second, e.g. for a live chart.
    #[must_use]
    pub fn ride_samples(&self) -> &[Sample] {
        self.ride
            .as_ref()
            .map_or(&[], |active| active.ride.samples())
    }

    /// The current ride's state, if riding.
    #[must_use]
    pub fn ride_state(&self) -> Option<RideState> {
        self.ride.as_ref().map(|active| active.ride.state())
    }

    /// The loaded route.
    #[must_use]
    pub fn route(&self) -> Option<&Route> {
        self.route.as_ref()
    }

    /// The 3D world of the loaded route, once generated.
    #[must_use]
    pub fn world(&self) -> Option<&World> {
        self.world.as_deref()
    }

    /// Avatar position in metres east/north, on the local traffic side of the drawn road.
    /// Ghosts ride nearer the centre on the same side. The route, GPS recording, minimap and
    /// simulation keep their original centre-line positions.
    #[must_use]
    pub fn rider_position(&self, distance: Meters, ghost: bool) -> Option<(f64, f64)> {
        Some(traffic::position(
            self.route.as_ref()?,
            &self.driving_left,
            &self.road_widths,
            distance,
            ghost,
        ))
    }

    /// Disconnects all devices, waiting at most a few seconds.
    pub fn shutdown(&mut self) {
        self.remote = None;
        let trainer = self.trainer.take();
        let sensor = self.sensor.take();
        let power_meter = self.power_meter.take();
        let cadence_sensor = self.cadence_sensor.take();
        self.runtime.block_on(async {
            for device in [trainer, sensor, power_meter, cadence_sensor]
                .into_iter()
                .flatten()
            {
                device.close(CLOSE_TIMEOUT).await;
            }
        });
    }

    fn bluetooth(&self) -> Result<&Bluetooth, AppError> {
        self.bluetooth.as_ref().ok_or(AppError::UnknownDevice)
    }

    fn discovered_device(
        &self,
        index: usize,
        kind: DeviceKind,
    ) -> Result<DiscoveredDevice, AppError> {
        self.discovered
            .get(index)
            .filter(|d| d.kind == kind)
            .cloned()
            .ok_or(AppError::UnknownDevice)
    }

    fn poll_jobs(&mut self, events: &mut Vec<AppEvent>) {
        while let Ok(result) = self.jobs_rx.try_recv() {
            match result {
                JobResult::Scan(Ok((bluetooth, devices))) => {
                    self.scanned(bluetooth, devices, events);
                }
                JobResult::Route(load, _)
                | JobResult::Video(load, _)
                | JobResult::World(load, ..)
                | JobResult::Progress(load, ..)
                    if load != self.load => {}
                JobResult::Route(_, Ok(imported)) => {
                    let map = imported.map.clone();
                    self.route_loaded(*imported, events);
                    if let WorldPlan::OnRequest(waiting) = &mut self.world_plan {
                        *waiting = Some(Box::new(map));
                    } else if let Some(route) = self.route.clone() {
                        self.generate_world(route, map);
                    }
                }
                JobResult::Video(_, Ok(loaded)) => {
                    let (imported, source) = *loaded;
                    match video::VideoCourse::new(&imported.route, &source) {
                        Ok(course) => {
                            self.video = Some(course);
                            self.view = View::Video;
                            let map = imported.map.clone();
                            self.route_loaded(imported, events);
                            // Ridden along the video, or in 3D once asked for (#44).
                            self.world_plan = WorldPlan::OnRequest(Some(Box::new(map)));
                            if !self.from_course
                                && let Err(error) = self.save_course()
                            {
                                warn!(%error, "cannot add the video course to the library");
                            }
                        }
                        Err(message) => events.push(AppEvent::Error(message)),
                    }
                }
                JobResult::Progress(_, stage, done, total) => {
                    events.push(AppEvent::LoadProgress { stage, done, total });
                }
                JobResult::World(_, world, generator, keep_as) => {
                    events.push(AppEvent::WorldReady {
                        chunks: world.chunks.len(),
                        fallback_samples: world.fallback_samples,
                    });
                    self.world_generator = generator;
                    if let (Some(fingerprint), Some(course)) = (keep_as, &self.course)
                        && self.keeps_worlds()
                    {
                        self.keep_world(course.clone(), Arc::clone(&world), Some(fingerprint));
                    }
                    self.road_widths = orqwa_world::RoadWidths::new(&world.road);
                    self.world = Some(world);
                    // A prepared GPX goes into the course library (R39).
                    if !self.from_course
                        && let Err(error) = self.save_course()
                    {
                        warn!(%error, "cannot add the course to the library");
                    }
                }
                JobResult::CourseAdded(Ok(path)) => events.push(AppEvent::CourseAdded(path)),
                JobResult::CourseSaved(Ok(path)) => {
                    self.saving = None;
                    self.course = Some(path.clone());
                    events.push(AppEvent::CourseAdded(path));
                }
                JobResult::CourseSaved(Err(message)) => {
                    self.saving = None;
                    events.push(AppEvent::Error(message));
                }
                JobResult::VideoKept(course, kept) => {
                    if let Err(e) = refer_to_kept_video(&course, &kept) {
                        events.push(AppEvent::Error(Failure::new(Kind::SaveVideo, e)));
                    }
                }
                JobResult::Upload(done) => events.push(self.upload_work_done(*done)),
                JobResult::Update(done) => events.extend(self.update_done(done)),
                JobResult::Countries(found) => self.countries_found(found, events),
                JobResult::Scan(Err(message)) => self.scan_failed(message, events),
                JobResult::Route(_, Err(message))
                | JobResult::Video(_, Err(message))
                | JobResult::CourseAdded(Err(message))
                | JobResult::Failed(message) => {
                    events.push(AppEvent::Error(message));
                }
            }
        }
    }

    /// Takes the devices a scan found, and connects the remembered ones if it was to
    /// reconnect them.
    fn scanned(
        &mut self,
        bluetooth: Bluetooth,
        devices: Vec<DiscoveredDevice>,
        events: &mut Vec<AppEvent>,
    ) {
        let infos = devices
            .iter()
            .enumerate()
            .map(|(index, d)| DeviceInfo {
                index,
                name: d.name.clone(),
                kind: d.kind,
                model: d.model,
                rssi: d.rssi,
                remembered: self.is_remembered(d),
            })
            .collect();
        self.bluetooth = Some(bluetooth);
        self.discovered = devices;
        events.push(AppEvent::DevicesFound(infos));
        if std::mem::take(&mut self.reconnecting) {
            let missing = self.connect_remembered();
            if !missing.is_empty() {
                events.push(AppEvent::RememberedMissing(missing));
            }
        }
    }

    fn scan_failed(&mut self, message: String, events: &mut Vec<AppEvent>) {
        if !std::mem::take(&mut self.reconnecting) {
            events.push(AppEvent::Error(Failure::new(Kind::Scan, message)));
            return;
        }
        warn!(%message, "cannot reconnect the devices used last");
        // During a ride the rider asked for it (R69); at start they see it when scanning.
        if self.ride.is_some() {
            events.push(AppEvent::Error(Failure::new(Kind::Scan, message)));
        }
    }

    /// Keeps the countries found for a course in its file, and reports the courses changed
    /// once the lookup is over.
    fn countries_found(
        &mut self,
        found: Option<(PathBuf, u64, Vec<CourseCountry>)>,
        events: &mut Vec<AppEvent>,
    ) {
        let Some((path, created, countries)) = found else {
            if self.filling_countries.take().is_some_and(|kept| kept > 0) {
                events.push(AppEvent::CoursesChanged);
            }
            return;
        };
        if keep_countries(&path, created, countries)
            && let Some(kept) = &mut self.filling_countries
        {
            *kept += 1;
        }
    }

    /// Takes a loaded route as the current one and reports it.
    fn route_loaded(&mut self, imported: Imported, events: &mut Vec<AppEvent>) {
        let Imported {
            route,
            name,
            gpx,
            course_map,
            minimap,
            countries,
            driving_left,
            ..
        } = imported;
        self.loaded = Some((name.clone(), gpx));
        self.course_map = course_map;
        self.minimap = minimap;
        self.countries = countries;
        self.driving_left = driving_left;
        events.push(AppEvent::RouteLoaded(RouteSummary {
            name,
            length: route.length().0,
            elevation_gain: route.elevation_gain().0,
            max_grade: route.max_grade().0,
            elevation_source: route.elevation_source(),
        }));
        self.route = Some(route);
    }

    fn poll_trainer(&mut self, events: &mut Vec<AppEvent>) {
        let Some(trainer) = &mut self.trainer else {
            return;
        };
        loop {
            match trainer.try_next_event() {
                Ok(Some(DeviceEvent::Connected)) => {
                    self.trainer_connected = Some(SystemTime::now());
                    if let Some(active) = &mut self.ride {
                        active.started.get_or_insert_with(SystemTime::now);
                        // A trainer connected anew (see `reconnect_devices`) knows no gradient.
                        if !active.paused {
                            active.ride.resend_control();
                        }
                    }
                    self.failures.remove(&DeviceKind::Trainer);
                    events.push(AppEvent::Connected(trainer.name().to_owned()));
                }
                Ok(Some(DeviceEvent::Disconnected)) => {
                    if let Some(active) = &mut self.ride {
                        active.ride.on_trainer_lost();
                    }
                    self.readings.remove(&DeviceKind::Trainer);
                    self.batteries.remove(&DeviceKind::Trainer);
                    // Each failed attempt to reconnect reports again; the rider hears it once.
                    if self.trainer_connected.take().is_some() {
                        events.push(AppEvent::Disconnected(trainer.name().to_owned()));
                    }
                }
                Ok(Some(DeviceEvent::Failed(reason))) => {
                    self.failures.insert(DeviceKind::Trainer, reason.clone());
                    events.push(AppEvent::ConnectionFailed {
                        name: trainer.name().to_owned(),
                        reason,
                    });
                }
                Ok(Some(DeviceEvent::Telemetry(telemetry))) => {
                    if let Some(active) = &mut self.ride {
                        active.ride.on_telemetry(&telemetry);
                    }
                    self.readings
                        .insert(DeviceKind::Trainer, (telemetry, Instant::now()));
                }
                Ok(Some(DeviceEvent::Battery(battery))) => {
                    self.batteries.insert(DeviceKind::Trainer, battery);
                }
                Ok(Some(DeviceEvent::Buttons(_))) => {}
                Ok(None) => break,
                Err(error) => {
                    events.push(AppEvent::Error(Failure::new(Kind::Trainer, error)));
                    self.trainer = None;
                    self.trainer_connected = None;
                    self.readings.remove(&DeviceKind::Trainer);
                    self.batteries.remove(&DeviceKind::Trainer);
                    break;
                }
            }
        }
    }

    /// Feeds the ride what the heart-rate strap, power meter and cadence sensor reported.
    fn poll_sensors(&mut self, events: &mut Vec<AppEvent>) {
        let sensors: [SensorSlot<'_>; 3] = [
            (
                &mut self.sensor,
                &mut self.sensor_connected,
                (DeviceKind::HeartRateSensor, Kind::HeartRate),
                (Ride::on_telemetry, Ride::on_heart_rate_lost),
            ),
            (
                &mut self.power_meter,
                &mut self.power_meter_connected,
                (DeviceKind::PowerMeter, Kind::PowerMeter),
                (Ride::on_power_meter, Ride::on_power_meter_lost),
            ),
            (
                &mut self.cadence_sensor,
                &mut self.cadence_sensor_connected,
                (DeviceKind::CadenceSensor, Kind::Cadence),
                (Ride::on_cadence_sensor, Ride::on_cadence_sensor_lost),
            ),
        ];
        for (slot, connected, what, feed) in sensors {
            let ride = self.ride.as_mut().map(|active| &mut active.ride);
            let mut reports = Reports {
                readings: &mut self.readings,
                batteries: &mut self.batteries,
                failures: &mut self.failures,
            };
            poll_sensor(slot, connected, what, ride, feed, &mut reports, events);
        }
    }
}

/// Where the sensors' last readings, batteries and failures are kept (R80, R97).
struct Reports<'a> {
    readings: &'a mut HashMap<DeviceKind, (Telemetry, Instant)>,
    batteries: &'a mut HashMap<DeviceKind, Battery>,
    failures: &'a mut HashMap<DeviceKind, String>,
}

/// How a ride takes a sensor's measurements, and its loss.
type SensorFeed = (fn(&mut Ride, &Telemetry), fn(&mut Ride));

/// A sensor's handle and when it connected, what it is for the failures and the readings,
/// and how a ride takes what it reports.
type SensorSlot<'a> = (
    &'a mut Option<DeviceHandle>,
    &'a mut Option<SystemTime>,
    (DeviceKind, Kind),
    SensorFeed,
);

/// Reads a sensor's events into the ride, if one is under way. A sensor whose driver stopped
/// is let go.
fn poll_sensor(
    slot: &mut Option<DeviceHandle>,
    connected: &mut Option<SystemTime>,
    (kind, what): (DeviceKind, Kind),
    mut ride: Option<&mut Ride>,
    (on_reading, on_lost): SensorFeed,
    reports: &mut Reports<'_>,
    events: &mut Vec<AppEvent>,
) {
    let Some(sensor) = slot else {
        return;
    };
    loop {
        match sensor.try_next_event() {
            Ok(Some(DeviceEvent::Connected)) => {
                *connected = Some(SystemTime::now());
                reports.failures.remove(&kind);
                events.push(AppEvent::Connected(sensor.name().to_owned()));
            }
            Ok(Some(DeviceEvent::Disconnected)) => {
                if let Some(ride) = ride.as_deref_mut() {
                    on_lost(ride);
                }
                reports.readings.remove(&kind);
                reports.batteries.remove(&kind);
                // Each failed attempt to reconnect reports again; the rider hears it once.
                if connected.take().is_some() {
                    events.push(AppEvent::Disconnected(sensor.name().to_owned()));
                }
            }
            Ok(Some(DeviceEvent::Failed(reason))) => {
                reports.failures.insert(kind, reason.clone());
                events.push(AppEvent::ConnectionFailed {
                    name: sensor.name().to_owned(),
                    reason,
                });
            }
            Ok(Some(DeviceEvent::Telemetry(telemetry))) => {
                if let Some(ride) = ride.as_deref_mut() {
                    on_reading(ride, &telemetry);
                }
                reports.readings.insert(kind, (telemetry, Instant::now()));
            }
            Ok(Some(DeviceEvent::Battery(battery))) => {
                reports.batteries.insert(kind, battery);
            }
            Ok(Some(DeviceEvent::Buttons(_))) => {}
            Ok(None) => break,
            Err(error) => {
                events.push(AppEvent::Error(Failure::new(what, error)));
                if let Some(ride) = ride.as_deref_mut() {
                    on_lost(ride);
                }
                *slot = None;
                *connected = None;
                reports.readings.remove(&kind);
                reports.batteries.remove(&kind);
                break;
            }
        }
    }
}

/// Single-precision points for course previews, which need no more precision.
#[allow(clippy::cast_possible_truncation)] // metres in a course fit f32 easily
fn thin(points: &[(f64, f64)]) -> Vec<[f32; 2]> {
    points.iter().map(|&(a, b)| [a as f32, b as f32]).collect()
}

/// The fastest time on `climb` among `records`.
fn best_climb_time(records: &[&RideRecord], climb: &ClimbTime) -> Option<Duration> {
    records
        .iter()
        .flat_map(|r| r.climbs.iter())
        .filter(|other| other.same_climb(climb))
        .map(|other| other.elapsed)
        .min()
}

/// The timed positions of a recorded activity: a GPX file with times, or a FIT file.
fn activity_points(path: &Path) -> Result<Vec<orqwa_routes::TimedPoint>, Failure> {
    let is_fit = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("fit"));
    if is_fit {
        let (start, samples) = read_fit(path).map_err(|e| Failure::new(Kind::LoadGhost, e))?;
        let start = start
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        return Ok(samples
            .iter()
            .filter_map(|s| {
                let location = s.location?;
                Some(orqwa_routes::TimedPoint {
                    lat: location.lat,
                    lon: location.lon,
                    time: start + s.elapsed.as_secs_f64(),
                })
            })
            .collect());
    }
    let xml = std::fs::read_to_string(path)
        .map_err(|e| Failure::new(Kind::ReadFile, e).arg(path.display()))?;
    let points = orqwa_routes::timed_points(&xml).map_err(|e| Failure::new(Kind::LoadGhost, e))?;
    if points.is_empty() {
        return Err(Failure::new(
            Kind::GhostTimes,
            format!("{} has no times to race against", path.display()),
        )
        .arg(path.display()));
    }
    Ok(points)
}

/// `record` with what the analysis across rides keeps of it (R62): computed from the FIT file
/// and saved with the rest if it has none yet. Every other field stays as it was.
fn with_analysis(fit: &Path, mut record: RideRecord) -> RideRecord {
    if record.analysis.is_some() {
        return record;
    }
    match read_fit(fit) {
        Ok((_, samples)) => {
            record.analysis = Some(analyse(&samples));
            if let Err(error) = rides::save(fit, &record) {
                warn!(%error, "cannot save the ride's analysis");
            }
        }
        Err(error) => warn!(%error, "cannot analyse ride"),
    }
    record
}

fn read_fit(fit: &Path) -> Result<(SystemTime, Vec<Sample>), String> {
    let bytes = std::fs::read(fit).map_err(|e| format!("cannot read {}: {e}", fit.display()))?;
    orqwa_storage::decode_fit(&bytes).map_err(|e| format!("{}: {e}", fit.display()))
}

/// The profile chosen last, else the first one, else a new default profile (saved, so it shows
/// up in the data directory to be edited).
fn initial_profile(data_dir: &Path) -> StoredProfile {
    let listed = profiles::list(data_dir);
    if let Some(found) = profiles::active(data_dir)
        .and_then(|id| listed.iter().find(|p| p.id == id).cloned())
        .or_else(|| listed.into_iter().next())
    {
        return found;
    }
    let profile = Profile::default();
    let id = profiles::new_id(data_dir, &profile.name);
    if let Err(error) = profiles::save(data_dir, &id, &profile) {
        warn!(%error, "cannot save the default profile");
    }
    StoredProfile { id, profile }
}

/// A free file name in `library` for a course called `name`.
/// How a course file refers to its video.
/// How the course file refers to its video. Its marks are kept along the track as recorded,
/// which the course is loaded along as a video course: placed on a route ridden in 3D, they
/// move on by its turns in place taken out.
fn video_reference(video: &video::VideoCourse, route: &Route) -> course::VideoReference {
    course::VideoReference {
        path: video.video.display().to_string(),
        file_name: video
            .video
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        size: std::fs::metadata(&video.video).map_or(0, |m| m.len()),
        offset_s: video.offset.as_secs_f64(),
        end_s: video.marks.last().map(|m| m.time.as_secs_f64()),
        marks: video
            .marks
            .iter()
            .map(|m| [route.recorded_distance(m.distance).0, m.time.as_secs_f64()])
            .collect(),
        located: video.located,
    }
}

/// The marks of a video course from its file, older files with a start and end only included.
fn video_marks(reference: &course::VideoReference) -> Vec<SyncMark> {
    let mark = |distance: f64, time: f64| SyncMark {
        distance: Meters(distance),
        time: Duration::try_from_secs_f64(time.max(0.0)).unwrap_or_default(),
    };
    if !reference.marks.is_empty() {
        return reference.marks.iter().map(|&[d, t]| mark(d, t)).collect();
    }
    // The route's end is placed when the course is paired with its route.
    reference.end_s.map_or_else(Vec::new, |end| {
        vec![mark(0.0, reference.offset_s), mark(0.0, end)]
    })
}

/// Changes the manifest of the course file at `path`.
/// Points the course at `path` to its own copy of its video at `kept` (#165); a course whose
/// video was taken off in the meantime stays as it is.
fn refer_to_kept_video(path: &Path, kept: &Path) -> Result<(), AppError> {
    update_manifest(path, |manifest| {
        if let Some(video) = &mut manifest.video {
            video.file_name = kept
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            video.path = kept.display().to_string();
        }
    })
}

/// The positions of `route` as (latitude, longitude).
fn route_points(route: &Route) -> Vec<(f64, f64)> {
    route.points().iter().map(|p| (p.lat, p.lon)).collect()
}

/// Gives the course at `path` the `countries` found for it while it was the one prepared at
/// `created`, unless it knows them already; true if it was given them. A course replaced or
/// deleted meanwhile is left alone.
fn keep_countries(path: &Path, created: u64, countries: Vec<CourseCountry>) -> bool {
    let Ok(mut manifest) = course::read_manifest(path) else {
        return false;
    };
    if manifest.created_unix_s != created || manifest.countries.is_some() {
        return false;
    }
    manifest.countries = Some(countries);
    course::rewrite_manifest(path, &manifest)
        .inspect_err(|error| warn!(path = %path.display(), %error, "cannot keep the countries"))
        .is_ok()
}

fn update_manifest(path: &Path, change: impl FnOnce(&mut Manifest)) -> Result<(), AppError> {
    let storage = |e: course::CourseError| AppError::Reported(Failure::new(Kind::UpdateCourse, e));
    let mut manifest = course::read_manifest(path).map_err(storage)?;
    change(&mut manifest);
    course::rewrite_manifest(path, &manifest).map_err(storage)
}

fn unique_course_path(library: &Path, name: &str) -> PathBuf {
    let slug = orqwa_storage::slug(name, "course");
    let mut path = library.join(format!("{slug}.{}", course::EXTENSION));
    let mut n = 1;
    while path.exists() {
        n += 1;
        path = library.join(format!("{slug}-{n}.{}", course::EXTENSION));
    }
    path
}

/// The virtual gears a drivetrain shifts (R9): none with a cassette.
fn virtual_gears(drivetrain: Drivetrain) -> Option<VirtualGears> {
    match drivetrain {
        Drivetrain::SingleCog { chainring, cog } => Some(VirtualGears::new(chainring, cog)),
        Drivetrain::Cassette => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use orqwa_domain::units::{Kilograms, MetersPerSecond, Rpm, Watts};
    use orqwa_session::workout::RampTest;

    use crate::moments::Moment;

    use super::*;

    pub(crate) fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("orqwa-app-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A flat 400 m route with elevations, so no terrain download is needed.
    pub(crate) fn write_route(dir: &std::path::Path) -> PathBuf {
        let mut xml = String::from("<gpx><trk><name>Test loop</name><trkseg>");
        for i in 0..=40 {
            let lat = 46.0 + f64::from(i) * 10.0 / 111_195.0;
            let _ = write!(xml, r#"<trkpt lat="{lat}" lon="7"><ele>500</ele></trkpt>"#);
        }
        xml.push_str("</trkseg></trk></gpx>");
        let path = dir.join("route.gpx");
        std::fs::write(&path, xml).unwrap();
        path
    }

    #[test]
    fn older_offline_courses_use_their_single_known_country_for_traffic() {
        let dir = temp_dir("traffic-country-fallback");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        app.load_route(write_route(&dir), true);
        run_until(&mut app, |event| matches!(event, AppEvent::CourseAdded(_)));
        let path = app.course.clone().unwrap();
        let mut manifest = course::read_manifest(&path).unwrap();
        let gb = CourseCountry {
            code: "GB".into(),
            names: std::collections::BTreeMap::new(),
        };
        for (countries, expected_side) in [
            (vec![gb.clone()], -1.0),
            (
                vec![
                    gb,
                    CourseCountry {
                        code: "FR".into(),
                        names: std::collections::BTreeMap::new(),
                    },
                ],
                1.0,
            ),
        ] {
            manifest.countries = Some(countries);
            course::rewrite_manifest(&path, &manifest).unwrap();
            app.open_course(path.clone());
            run_until(&mut app, |event| matches!(event, AppEvent::RouteLoaded(_)));
            let route = app.route().unwrap();
            let at = route.position(Meters(100.0));
            let centre = LocalProjection::for_route(route).project(at.lat, at.lon);
            let rider = app.rider_position(Meters(100.0), false).unwrap();
            let right =
                (rider.0 - centre.0) * at.heading.cos() - (rider.1 - centre.1) * at.heading.sin();
            assert!(right * expected_side > 0.0);
        }
        app.shutdown();
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Calls `update` like a 60 fps frame loop until `done` returns true (or 120 s pass).
    pub(crate) fn run_until(
        app: &mut App,
        mut done: impl FnMut(&AppEvent) -> bool,
    ) -> Vec<AppEvent> {
        // Generous wall-clock limit: an unoptimized world build under a parallel test run takes
        // well over 30 s on some machines; the limit only has to catch a hang.
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        let mut seen = Vec::new();
        while std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(16));
            for event in app.update(Duration::from_millis(16)) {
                let stop = done(&event);
                seen.push(event);
                if stop {
                    return seen;
                }
            }
        }
        panic!("timed out; events so far: {seen:?}");
    }

    #[tokio::test]
    async fn courses_ridden_in_3d_lose_their_turns_in_place_videos_keep_them() {
        // North 300 m, 30 m back and north again to 500 m: steps back and forth (#101).
        let dir = temp_dir("turns");
        let mut xml = String::from("<gpx><trk><trkseg>");
        let norths = (0..=30).chain((27..30).rev()).chain(28..=50);
        for north in norths {
            let lat = 46.0 + f64::from(north) * 10.0 / 111_195.0;
            let _ = write!(xml, r#"<trkpt lat="{lat}" lon="7"><ele>500</ele></trkpt>"#);
        }
        xml.push_str("</trkseg></trk></gpx>");
        let length = async |video| {
            import_gpx(
                xml.clone(),
                "zig-zag",
                (&dir, true),
                video,
                &UsedFiles::default(),
                &mut |_, _, _| {},
            )
            .await
            .unwrap()
            .route
            .length()
            .0
        };

        let (ridden, recorded) = (length(false).await, length(true).await);
        assert!((ridden - 500.0).abs() < 10.0, "ridden in 3D: {ridden} m");
        assert!(
            (recorded - 560.0).abs() < 10.0,
            "along a video: {recorded} m"
        );
    }

    #[test]
    fn loads_a_route_in_the_background() {
        let dir = temp_dir("load");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();

        app.load_route(write_route(&dir), true);
        let events = run_until(&mut app, |e| matches!(e, AppEvent::RouteLoaded(_)));

        let Some(AppEvent::RouteLoaded(summary)) = events.last() else {
            unreachable!()
        };
        assert_eq!(summary.name, "Test loop");
        assert!((summary.length - 400.0).abs() < 1.0);
        assert!(app.route().is_some());
    }

    #[test]
    fn a_load_that_hangs_can_be_cancelled_and_the_next_one_goes_through() {
        let dir = temp_dir("cancel");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        let _ = app.start_loading(true);
        app.spawn_load(app.load, std::future::pending());

        app.cancel_loading();

        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        while std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(16));
            let events = app.update(Duration::from_millis(16));
            assert!(
                events.is_empty(),
                "nothing from a cancelled load: {events:?}"
            );
        }
        app.load_route(write_route(&dir), true);
        run_until(&mut app, |e| matches!(e, AppEvent::RouteLoaded(_)));
    }

    #[test]
    fn a_load_whose_job_panics_is_reported_instead_of_loading_forever() {
        let dir = temp_dir("panic");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        let _ = app.start_loading(true);

        app.spawn_load(app.load, async { panic!("a broken tile") });

        let events = run_until(&mut app, |e| matches!(e, AppEvent::Error(_)));
        let Some(AppEvent::Error(message)) = events.last() else {
            unreachable!()
        };
        assert!(message.contains("a broken tile"), "{message}");
    }

    /// The reference route (#167) imported as a rider would, from the map and terrain tiles in
    /// the cache: run `orqwa-cli route core/fixtures/oberalp.gpx` once online, then
    /// `cargo test -p orqwa-app -- --ignored reference_route`.
    #[test]
    #[ignore = "needs the Oberalp's map and terrain tiles in the cache"]
    fn the_reference_route_follows_the_map_at_road_grades() {
        let dir = temp_dir("reference-route");
        let mut app = App::new(dir.join("data"), paths::cache_dir()).unwrap();
        let fixture = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../fixtures/oberalp.gpx"
        ));
        app.load_route(fixture, true);
        run_until(&mut app, |e| matches!(e, AppEvent::RouteLoaded(_)));
        let route = app.route().expect("the route");

        assert_eq!(route.elevation_source(), ElevationSource::Terrain);
        assert!(
            (33_000.0..33_900.0).contains(&route.length().0),
            "{}",
            route.length().0
        );
        // The planner drew the road through the Oberalpsee and left the tunnels' insides out:
        // on the map's roads the route climbs like a pass road, never like a cliff (#166).
        assert!(
            route.max_grade().0 < 15.0,
            "steepest {} %",
            route.max_grade().0
        );
        let gain = route.elevation_gain().0;
        assert!((550.0..900.0).contains(&gain), "gain {gain}");
        app.shutdown();
    }

    /// The reference route's roadside (R92), from the cached tiles as above: a pass road has
    /// guard rails where it runs along drops, snow poles up high and delineator posts below,
    /// none of them standing in the villages.
    #[test]
    #[ignore = "needs the Oberalp's map and terrain tiles in the cache"]
    fn the_reference_route_has_rails_poles_and_posts() {
        let dir = temp_dir("reference-props");
        let mut app = App::new(dir.join("data"), paths::cache_dir()).unwrap();
        let fixture = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../fixtures/oberalp.gpx"
        ));
        app.load_route(fixture, true);
        run_until(&mut app, |e| matches!(e, AppEvent::WorldReady { .. }));
        let world = app.world().expect("the world");
        let mut counts = std::collections::BTreeMap::<&str, usize>::new();
        for chunk in &world.chunks {
            for (name, buffer) in &chunk.props.models {
                *counts.entry(name.as_str()).or_default() += buffer.len() / 12;
            }
        }
        eprintln!("props on the Oberalp: {counts:?}");
        let count = |name: &str| counts.get(name).copied().unwrap_or(0);
        assert!(count("guard_rail") > 100, "{counts:?}");
        assert!(count("snow_pole") > 100, "{counts:?}");
        assert!(count("delineator") > 20, "{counts:?}");
        app.shutdown();
    }

    #[test]
    fn a_paused_ride_waits_and_goes_on_where_it_was() {
        let dir = temp_dir("pause");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        app.load_route(write_route(&dir), true);
        run_until(&mut app, |e| matches!(e, AppEvent::RouteLoaded(_)));
        app.connect_trainer(TrainerChoice::Fake(FakeRider {
            power: Watts(250.0),
            cadence: Rpm(90.0),
            heart: None,
        }))
        .unwrap();
        run_until(&mut app, |e| matches!(e, AppEvent::Connected(_)));
        app.start_ride(Percent(50.0), DescentMode::Coast, &GhostChoice::None)
            .unwrap();
        let ride = |app: &mut App, frames: usize| {
            for _ in 0..frames {
                std::thread::sleep(Duration::from_millis(16));
                app.update(Duration::from_millis(16));
            }
            app.ride_state().unwrap()
        };
        let before = ride(&mut app, 60);
        assert!(before.distance.0 > 1.0, "{before:?}");
        assert!(app.ambient_sound_active());
        assert_eq!(app.ambient_sound(2_400).len(), 2_400);

        // Paused, nothing moves and the clock stands...
        assert!(app.set_paused(true));
        assert!(app.is_paused());
        assert!(!app.ambient_sound_active());
        assert_eq!(app.ambient_sound(2_400), [] as [[f32; 2]; 0]);
        let paused = ride(&mut app, 30);
        assert_eq!(paused.distance, before.distance);
        assert_eq!(paused.elapsed, before.elapsed);
        // ...resumed, the ride goes on from there.
        assert!(!app.set_paused(false));
        assert!(!app.is_paused());
        let after = ride(&mut app, 30);
        assert!(after.distance.0 > paused.distance.0, "{after:?}");
        assert!(after.elapsed > paused.elapsed);
        assert_eq!(app.ambient_sound(2_400).len(), 2_400);
        app.finish_ride();
        assert!(!app.ambient_sound_active());
        assert_eq!(app.ambient_sound(2_400), [] as [[f32; 2]; 0]);
        app.shutdown();
    }

    #[test]
    fn a_ride_starts_with_a_trainer_connected_before_it() {
        // As on the course page: the trainer connects, the world is built, then the ride
        // starts — the trainer reported itself long before.
        let dir = temp_dir("connected-first");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        app.load_route(write_route(&dir), true);
        run_until(&mut app, |e| matches!(e, AppEvent::RouteLoaded(_)));
        app.connect_trainer(TrainerChoice::Fake(FakeRider {
            power: Watts(250.0),
            cadence: Rpm(90.0),
            heart: None,
        }))
        .unwrap();
        run_until(&mut app, |e| matches!(e, AppEvent::Connected(_)));

        app.start_ride(Percent(50.0), DescentMode::Coast, &GhostChoice::None)
            .unwrap();
        for _ in 0..120 {
            std::thread::sleep(Duration::from_millis(16));
            app.update(Duration::from_millis(16));
        }

        let state = app.ride_state().unwrap();
        assert!(state.distance.0 > 1.0, "rider should be moving: {state:?}");
        assert!(app.trainer_connected());
        app.shutdown();
    }

    #[test]
    fn a_workout_on_its_own_rides_a_flat_road_and_lands_in_the_history_by_its_name() {
        let dir = temp_dir("workout");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        app.connect_trainer(TrainerChoice::Fake(FakeRider {
            power: Watts(150.0),
            cadence: Rpm(90.0),
            heart: None,
        }))
        .unwrap();
        run_until(&mut app, |e| matches!(e, AppEvent::Connected(_)));

        let workout = Workout::ConstantPower(Watts(220.0));
        app.start_workout(workout, false, DescentMode::Coast, "Constant power 220 W")
            .unwrap();
        for _ in 0..150 {
            std::thread::sleep(Duration::from_millis(16));
            app.update(Duration::from_millis(16));
        }

        let state = app.ride_state().unwrap();
        assert!(state.distance.0 > 1.0, "{state:?}");
        assert_eq!(state.position, None);
        assert_eq!(
            state.telemetry.power,
            Some(Watts(220.0)),
            "ERG holds the target"
        );
        app.change_workout(Workout::ConstantPower(Watts(240.0)));
        assert_eq!(
            app.ride_state()
                .unwrap()
                .workout
                .and_then(|w| w.target_power),
            Some(Watts(240.0))
        );
        let saved = app.finish_ride();
        assert!(
            matches!(saved.as_slice(), [AppEvent::RideSaved(_)]),
            "{saved:?}"
        );
        let history = app.history();
        assert_eq!(history[0].record.route, "Constant power 220 W");
        assert_eq!(
            history[0].record.route_key, None,
            "no records without a course"
        );
        app.shutdown();
    }

    #[test]
    fn a_workout_on_a_course_rides_along_it() {
        let dir = temp_dir("workout-course");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        assert!(matches!(
            app.start_workout(
                Workout::ConstantPower(Watts(200.0)),
                true,
                DescentMode::Coast,
                "x"
            ),
            Err(AppError::NoTrainer)
        ));
        app.connect_trainer(TrainerChoice::Fake(FakeRider {
            power: Watts(150.0),
            cadence: Rpm(90.0),
            heart: None,
        }))
        .unwrap();
        assert!(matches!(
            app.start_workout(
                Workout::ConstantPower(Watts(200.0)),
                true,
                DescentMode::Coast,
                "x"
            ),
            Err(AppError::NoRoute)
        ));
        app.load_route(write_route(&dir), true);
        run_until(&mut app, |e| matches!(e, AppEvent::RouteLoaded(_)));

        app.start_workout(
            Workout::ConstantPower(Watts(200.0)),
            true,
            DescentMode::Coast,
            "x",
        )
        .unwrap();
        for _ in 0..150 {
            std::thread::sleep(Duration::from_millis(16));
            app.update(Duration::from_millis(16));
        }

        let state = app.ride_state().unwrap();
        assert!(state.position.is_some() && state.remaining.is_some());
        assert!(state.workout.is_some());
        assert!(!app.riding_along_video());
        app.finish_ride();
        assert_eq!(
            app.history()[0].record.route,
            "Test loop",
            "named after the course"
        );
        app.shutdown();
    }

    #[test]
    fn workouts_come_built_in_and_from_imported_files() {
        let dir = temp_dir("workout-library");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        let file = dir.join("Tempo.erg");
        std::fs::write(
            &file,
            "[COURSE HEADER]\nMINUTES WATTS\n[END COURSE HEADER]\n\
             [COURSE DATA]\n0\t180\n20\t180\n[END COURSE DATA]\n",
        )
        .unwrap();
        let builtin = app.workouts().len();

        let id = app.import_workout(&file).unwrap();

        let library = app.workouts();
        assert_eq!(library.len(), builtin + 1);
        assert_eq!(library.last().unwrap().id, id);
        let Workout::Structured { plan, ftp } = app.structured_workout(&id).unwrap() else {
            panic!("a structured workout");
        };
        assert_eq!(
            (plan.name.as_str(), ftp),
            ("Tempo", app.profile().profile.ftp)
        );
        assert!(matches!(
            app.import_workout(&dir.join("missing.zwo")),
            Err(AppError::Reported(Failure {
                kind: Kind::ImportWorkout,
                ..
            }))
        ));
    }

    #[test]
    fn an_ftp_test_ends_when_the_rider_gives_way_and_its_ftp_can_be_kept() {
        let dir = temp_dir("ftp-test");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        app.connect_trainer(TrainerChoice::Fake(FakeRider {
            power: Watts(150.0),
            cadence: Rpm(90.0),
            heart: None,
        }))
        .unwrap();
        run_until(&mut app, |e| matches!(e, AppEvent::Connected(_)));
        let test = RampTest::for_ftp(app.profile().profile.ftp);
        app.start_workout(
            Workout::RampTest(test),
            false,
            DescentMode::Coast,
            "FTP test",
        )
        .unwrap();
        // Through the warm-up and a few steps at once; the fake rider holds every step.
        let active = app.ride.as_mut().unwrap();
        for _ in 0..(10 * 60 * 20) {
            active
                .ride
                .on_telemetry(&orqwa_domain::telemetry::Telemetry {
                    power: Some(Watts(200.0)),
                    cadence: Some(Rpm(90.0)),
                    ..Default::default()
                });
            active.ride.tick(Duration::from_millis(50));
        }
        // Then the legs give way.
        for _ in 0..(15 * 20) {
            active
                .ride
                .on_telemetry(&orqwa_domain::telemetry::Telemetry {
                    cadence: Some(Rpm(20.0)),
                    ..Default::default()
                });
            active.ride.tick(Duration::from_millis(50));
        }
        assert!(active.ride.is_finished(), "over once the rider gives way");

        let saved = app.finish_ride();

        assert!(
            matches!(saved.as_slice(), [AppEvent::RideSaved(_)]),
            "{saved:?}"
        );
        let test = &app.history()[0].record;
        assert_eq!(
            test.ftp_estimate,
            Some(Watts(150.0)),
            "75 % of the best minute at 200 W"
        );
        app.set_ftp(Watts(150.0)).unwrap();
        assert_eq!(app.profile().profile.ftp, Watts(150.0));
        app.shutdown();
    }

    #[test]
    fn deleting_the_active_rider_switches_to_one_left_but_the_last_one_stays() {
        let dir = temp_dir("delete-rider");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        let first = app.profile().id.clone();
        let anna = app
            .save_profile(
                None,
                Profile {
                    name: "Anna".to_owned(),
                    ..Profile::default()
                },
            )
            .unwrap();
        assert_eq!(app.profile().id, anna);

        app.delete_profile(&anna).unwrap();

        assert_eq!(app.profile().id, first);
        assert_eq!(profiles::active(&dir.join("data")), Some(first.clone()));
        assert!(matches!(
            app.delete_profile(&anna),
            Err(AppError::UnknownProfile)
        ));
        assert!(matches!(
            app.delete_profile(&first),
            Err(AppError::LastProfile)
        ));
        assert_eq!(app.profiles().len(), 1);
        app.shutdown();
    }

    #[test]
    fn a_single_cog_rides_virtual_gears_shifted_from_the_keyboard() {
        let dir = temp_dir("gears");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        let mut profile = app.profile().profile.clone();
        profile.drivetrain = Drivetrain::SingleCog {
            chainring: 50,
            cog: 14,
        };
        let id = app.profile().id.clone();
        app.save_profile(Some(&id), profile).unwrap();
        app.load_route(write_route(&dir), true);
        run_until(&mut app, |e| matches!(e, AppEvent::RouteLoaded(_)));
        app.connect_trainer(TrainerChoice::Fake(FakeRider {
            power: Watts(200.0),
            cadence: Rpm(90.0),
            heart: None,
        }))
        .unwrap();
        app.start_ride(Percent(50.0), DescentMode::Coast, &GhostChoice::None)
            .unwrap();
        let before = app.ride_state().unwrap().gear.unwrap().number;

        app.shift(Shift::Up);
        app.update(Duration::from_millis(16));

        assert_eq!(app.ride_state().unwrap().gear.unwrap().number, before + 1);
        app.shutdown();
    }

    #[test]
    fn only_successful_live_virtual_shifts_make_a_sound() {
        let dir = temp_dir("shift-sound");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        let mut profile = app.profile().profile.clone();
        profile.drivetrain = Drivetrain::SingleCog {
            chainring: 50,
            cog: 14,
        };
        let id = app.profile().id.clone();
        app.save_profile(Some(&id), profile.clone()).unwrap();
        app.load_route(write_route(&dir), true);
        run_until(&mut app, |e| matches!(e, AppEvent::RouteLoaded(_)));
        app.connect_trainer(TrainerChoice::Fake(FakeRider {
            power: Watts(200.0),
            cadence: Rpm(90.0),
            heart: None,
        }))
        .unwrap();
        run_until(&mut app, |e| matches!(e, AppEvent::Connected(_)));
        app.start_ride(Percent(50.0), DescentMode::Coast, &GhostChoice::None)
            .unwrap();

        // Compare with the same soundscape without a shift, keeping natural noise out of
        // the assertion. Drive the shared input path so keyboard, BLE and phone all count.
        let hear = |app: &mut App, shifts: &[Shift]| {
            app.ride.as_mut().unwrap().ambient = ambient::Sound::new(259);
            for &shift in shifts {
                app.shift(shift);
            }
            app.update(Duration::ZERO);
            let active = app.ride.as_mut().unwrap();
            let state = active.ride.state();
            let motion = ambient::Motion {
                speed: state.speed,
                cadence: state.telemetry.cadence,
                power: state.telemetry.power,
            };
            let heard = active
                .ambient
                .render(12_000, motion, active.ambient_settings);
            let plain = ambient::Sound::new(259).render(12_000, motion, active.ambient_settings);
            heard != plain
        };
        assert!(!hear(&mut app, &[]), "starting a ride is silent");
        assert!(
            hear(&mut app, &[Shift::Up]),
            "an accepted harder gear clicks"
        );
        assert!(
            hear(&mut app, &[Shift::Down]),
            "an accepted easier gear clicks"
        );
        assert!(
            hear(&mut app, &[Shift::Up, Shift::Down]),
            "two changes in one poll are not lost"
        );
        assert!(hear(&mut app, &[Shift::To(1)]));
        assert!(
            !hear(&mut app, &[Shift::Down]),
            "the easiest gear cannot shift down"
        );
        assert!(
            !hear(&mut app, &[Shift::To(1)]),
            "selecting the current gear is silent"
        );
        assert!(hear(&mut app, &[Shift::To(24)]));
        assert!(
            !hear(&mut app, &[Shift::Up]),
            "the hardest gear cannot shift up"
        );
        app.set_paused(true);
        assert!(
            !hear(&mut app, &[Shift::Down]),
            "paused shifts must not be queued"
        );
        app.set_paused(false);
        assert!(
            !hear(&mut app, &[]),
            "resume does not replay paused requests"
        );
        app.set_ambient_sound(ambient::Settings {
            bike: 0.0,
            ..ambient::Settings::default()
        });
        assert!(
            !hear(&mut app, &[Shift::Down]),
            "muting bicycle sound includes shifts"
        );
        app.set_ambient_sound(ambient::Settings::default());
        profile.drivetrain = Drivetrain::Cassette;
        app.save_profile(Some(&id), profile).unwrap();
        assert!(
            !hear(&mut app, &[Shift::Up]),
            "cassette button presses do not invent a shift"
        );
        app.shutdown();
    }

    #[test]
    fn the_ride_follows_the_profile_changed_during_it_and_the_profile_keeps_it() {
        let dir = temp_dir("ride-profile");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        let change = |app: &mut App, drivetrain: Drivetrain| {
            let mut profile = app.profile().profile.clone();
            profile.drivetrain = drivetrain;
            let id = app.profile().id.clone();
            app.save_profile(Some(&id), profile).unwrap();
        };
        change(&mut app, Drivetrain::Cassette);
        app.load_route(write_route(&dir), true);
        run_until(&mut app, |e| matches!(e, AppEvent::RouteLoaded(_)));
        app.connect_trainer(TrainerChoice::Fake(FakeRider {
            power: Watts(200.0),
            cadence: Rpm(90.0),
            heart: None,
        }))
        .unwrap();
        app.start_ride(Percent(50.0), DescentMode::Coast, &GhostChoice::None)
            .unwrap();
        assert_eq!(app.ride_state().unwrap().gear, None, "a cassette");

        let single_cog = Drivetrain::SingleCog {
            chainring: 50,
            cog: 14,
        };
        change(&mut app, single_cog);
        let before = app.ride_state().unwrap().gear.unwrap().number;
        app.shift(Shift::Up);
        app.update(Duration::from_millis(16));

        assert_eq!(app.ride_state().unwrap().gear.unwrap().number, before + 1);
        let id = app.profile().id.clone();
        app.shutdown();
        let mut reopened = App::new(dir.join("data"), dir.join("cache")).unwrap();
        assert_eq!(reopened.profile().id, id);
        assert_eq!(
            reopened.profile().profile.drivetrain,
            single_cog,
            "the profile keeps it"
        );
        reopened.shutdown();
    }

    /// A shifter that shifts and asks for controls as told, and is connected or not.
    struct Shifter {
        shifts: Vec<Shift>,
        controls: Vec<Control>,
        connected: bool,
    }

    impl ShiftInput for Shifter {
        fn name(&self) -> &'static str {
            "RDR9250"
        }

        fn poll(&mut self) -> Vec<Shift> {
            std::mem::take(&mut self.shifts)
        }

        fn controls(&mut self) -> Vec<Control> {
            std::mem::take(&mut self.controls)
        }

        fn connected(&self) -> bool {
            self.connected
        }
    }

    #[test]
    fn a_shifters_shifts_reach_the_ride_its_controls_the_front_end_and_its_connection_is_told() {
        let dir = temp_dir("shifter");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        let mut profile = app.profile().profile.clone();
        profile.drivetrain = Drivetrain::SingleCog {
            chainring: 50,
            cog: 14,
        };
        let id = app.profile().id.clone();
        app.save_profile(Some(&id), profile).unwrap();
        app.load_route(write_route(&dir), true);
        run_until(&mut app, |e| matches!(e, AppEvent::RouteLoaded(_)));
        app.connect_trainer(TrainerChoice::Fake(FakeRider {
            power: Watts(200.0),
            cadence: Rpm(90.0),
            heart: None,
        }))
        .unwrap();
        app.start_ride(Percent(50.0), DescentMode::Coast, &GhostChoice::None)
            .unwrap();
        let before = app.ride_state().unwrap().gear.unwrap().number;

        app.add_shift_input(Box::new(Shifter {
            shifts: vec![Shift::Up, Shift::Up, Shift::Down, Shift::Up],
            controls: vec![Control::NextCamera, Control::PlayPause],
            connected: true,
        }));
        let events = app.update(Duration::from_millis(16));

        assert!(
            events.contains(&AppEvent::Connected("RDR9250".to_owned())),
            "{events:?}"
        );
        let controls: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, AppEvent::Control(_)))
            .collect();
        assert_eq!(
            controls,
            [
                &AppEvent::Control(Control::NextCamera),
                &AppEvent::Control(Control::PlayPause)
            ]
        );
        assert_eq!(app.ride_state().unwrap().gear.unwrap().number, before + 2);
        app.shutdown();
    }

    #[test]
    fn the_shifters_buttons_are_assigned_for_next_time() {
        let dir = temp_dir("buttons");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        assert_eq!(app.button_map(ControllerModel::Di2), ButtonMap::default());
        let mut map = ButtonMap::shifting(2, 1);
        map.assign(
            3,
            orqwa_domain::shifting::Press::Short,
            Some(orqwa_domain::shifting::ButtonAction::Control(
                Control::Overlay,
            )),
        );

        app.set_button_map(ControllerModel::Di2, map).unwrap();
        assert_eq!(
            app.button_map(ControllerModel::ZwiftClick),
            ButtonMap::default(),
            "a Di2's channels leave a Click's buttons alone"
        );
        app.shutdown();

        let app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        assert_eq!(app.button_map(ControllerModel::Di2), map);
        assert_eq!(
            app.button_map(ControllerModel::ZwiftClick),
            ButtonMap::default()
        );
    }

    #[test]
    fn simulated_rides_speed_up_and_jump_but_set_no_records() {
        let dir = temp_dir("simulation");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        app.load_route(write_climb_route(&dir), true);
        run_until(&mut app, |e| matches!(e, AppEvent::RouteLoaded(_)));
        assert!(!app.simulating());
        assert_eq!(app.set_time_scale(10.0), 1.0, "nothing to speed up");
        app.connect_trainer(TrainerChoice::Fake(FakeRider {
            power: Watts(300.0),
            cadence: Rpm(90.0),
            heart: None,
        }))
        .unwrap();
        assert!(app.simulating());
        app.start_ride(Percent(50.0), DescentMode::Coast, &GhostChoice::None)
            .unwrap();
        run_until(&mut app, |e| matches!(e, AppEvent::Connected(_)));
        assert_eq!(app.set_time_scale(100.0), 20.0);
        assert_eq!(app.set_time_scale(10.0), 10.0);

        // One real second at 10×: ten seconds of riding, sampled every ride second.
        for _ in 0..60 {
            std::thread::sleep(Duration::from_millis(16));
            app.update(Duration::from_millis(16));
        }
        let state = app.ride_state().unwrap();
        assert!(state.elapsed > Duration::from_secs(8), "{state:?}");
        let samples = app.ride.as_ref().unwrap().ride.samples().len();
        let seconds = usize::try_from(state.elapsed.as_secs()).unwrap();
        assert!(
            samples.abs_diff(seconds) <= 1,
            "{samples} samples in {seconds} s"
        );

        // Jumps: by distance, and to the route's point nearest a spot on the map.
        assert!(app.jump_to(Meters(150.0)));
        assert!((app.ride_state().unwrap().distance.0 - 150.0).abs() < 1.0);
        let route = app.route().unwrap().clone();
        let target = route.position(Meters(80.0));
        let (x, y) = LocalProjection::for_route(&route).project(target.lat, target.lon);
        assert!(app.jump_near(x + 3.0, y - 2.0));
        assert!((app.ride_state().unwrap().distance.0 - 80.0).abs() < 12.0);

        let saved = app.finish_ride();
        let Some(AppEvent::RideSaved(fit)) = saved.first() else {
            panic!("{saved:?}")
        };
        let record = &app.history()[0].record;
        assert_eq!(app.history()[0].fit, *fit);
        assert_eq!(record.route_key, None, "a simulated ride sets no records");
        assert_eq!(record.climbs.len(), 0);
        assert!(record.simulated);
        assert_eq!(app.history()[0].power_records, [], "nor power records");
        app.shutdown();
    }

    #[test]
    fn rides_with_the_fake_trainer_and_saves_a_fit_file() {
        let dir = temp_dir("ride");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        app.load_route(write_route(&dir), true);
        run_until(&mut app, |e| matches!(e, AppEvent::RouteLoaded(_)));
        let planned = app.estimated_time(DescentMode::Coast).unwrap();

        app.connect_trainer(TrainerChoice::Fake(FakeRider {
            power: Watts(250.0),
            cadence: Rpm(90.0),
            heart: None,
        }))
        .unwrap();
        app.start_ride(Percent(50.0), DescentMode::Coast, &GhostChoice::None)
            .unwrap();
        run_until(&mut app, |e| matches!(e, AppEvent::Connected(_)));
        // Ride three seconds of frames.
        for _ in 0..180 {
            std::thread::sleep(Duration::from_millis(16));
            app.update(Duration::from_millis(16));
        }
        let state = app.ride_state().unwrap();
        assert!(state.distance.0 > 1.0, "rider should be moving: {state:?}");
        assert_eq!(state.telemetry.power, Some(Watts(250.0)));
        let values = app.hud_values();
        assert_eq!(values.len(), hud::METRICS.len());
        let value = |id: &str| values.iter().find(|(m, _)| *m == id).and_then(|(_, v)| *v);
        assert_eq!(value("power_3s"), Some(250.0));
        assert_eq!(value("heart_rate"), None);
        assert!(value("distance").is_some_and(|km| km > 0.0));
        // Under way, less of the route is left than was planned.
        let to_go = value("time_remaining").unwrap();
        assert!(
            to_go > 0.0 && to_go < planned.as_secs_f64(),
            "{to_go} of {planned:?}"
        );

        app.adjust_ride(Percent(80.0), DescentMode::Flat);
        let events = app.finish_ride();
        let Some(AppEvent::RideSaved(path)) = events.first() else {
            panic!("not saved: {events:?}");
        };
        assert!(path.starts_with(dir.join("data/profiles/rider/rides")));
        assert!(std::fs::metadata(path).unwrap().len() > 100);

        let history = app.history();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].fit, *path);
        assert_eq!(history[0].record.route, "Test loop");
        assert!(history[0].record.summary.distance.0 > 1.0);
        assert_eq!(history[0].record.rider_mass, Some(Kilograms(75.0)));
        let curve = &history[0].record.analysis.as_ref().unwrap().power_curve;
        assert_eq!(curve.at(Duration::from_secs(1)), Some(Watts(250.0)));
        let detail = app.ride_detail(path).unwrap();
        assert!(detail.samples.len() > 1);
        assert!(detail.power_zones.iter().sum::<Duration>() > Duration::ZERO);

        // Without metadata, e.g. a FIT file copied in by hand, the history rebuilds it.
        std::fs::remove_file(rides::metadata_path(path)).unwrap();
        assert_eq!(app.history().len(), 1);
        assert!(rides::metadata_path(path).exists());

        app.rename_ride(path, "  Lunch spin ").unwrap();
        assert_eq!(app.history()[0].record.name.as_deref(), Some("Lunch spin"));
        app.rename_ride(path, "").unwrap();
        assert_eq!(app.history()[0].record.name, None);

        app.delete_ride(path).unwrap();
        assert_eq!(app.history().len(), 0);
        assert!(
            dir.join("cache").join(orqwa_storage::index::FILE).exists(),
            "the history was served from the index in the cache (R79)"
        );
        app.shutdown();
    }

    #[test]
    fn rides_saved_before_the_analysis_get_it_once_and_keep_everything_else() {
        let dir = temp_dir("analysis-backfill");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        let rides_dir = dir.join("data/profiles/rider/rides");
        std::fs::create_dir_all(&rides_dir).unwrap();
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000);
        // Ten minutes: five at 300 W, five at 200 W.
        let samples: Vec<Sample> = (0..600_u32)
            .map(|s| Sample {
                elapsed: Duration::from_secs(u64::from(s)),
                location: None,
                distance: Meters(8.0 * f64::from(s)),
                speed: MetersPerSecond(8.0),
                power: Some(Watts(if s < 300 { 300.0 } else { 200.0 })),
                cadence: None,
                heart_rate: None,
            })
            .collect();
        let fit = rides_dir.join(paths::activity_file_name(start));
        std::fs::write(&fit, orqwa_storage::encode_fit(start, &samples).unwrap()).unwrap();
        // As an older ORQWA saved it: named, on a known course, without the analysis.
        let saved = RideRecord {
            route: "Gurten".to_owned(),
            start,
            summary: summarize(&samples, Watts(250.0)),
            route_key: Some("0123456789abcdef".to_owned()),
            route_time: Some(Duration::from_mins(10)),
            climbs: Vec::new(),
            name: Some("Before the update".to_owned()),
            ftp_estimate: None,
            rider_mass: None,
            simulated: false,
            analysis: None,
            uploads: std::collections::BTreeMap::new(),
        };
        rides::save(&fit, &saved).unwrap();

        let history = app.history();

        let analysed = &history[0].record;
        let curve = &analysed.analysis.as_ref().unwrap().power_curve;
        assert_eq!(curve.at(Duration::from_mins(5)), Some(Watts(300.0)));
        assert_eq!(curve.at(Duration::from_mins(10)), Some(Watts(250.0)));
        assert_eq!(
            RideRecord {
                analysis: None,
                ..analysed.clone()
            },
            saved,
            "nothing else changes"
        );
        assert_eq!(
            rides::load(&fit).unwrap(),
            *analysed,
            "saved: computed once"
        );
        // The rider's only ride holds every record it is long enough for.
        assert_eq!(
            history[0].power_records,
            [
                Duration::from_secs(5),
                Duration::from_mins(1),
                Duration::from_mins(5)
            ]
        );
        app.shutdown();
    }

    #[test]
    fn a_saved_course_rides_on_another_machine() {
        let dir = temp_dir("course");
        let mut prepared = App::new(dir.join("a/data"), dir.join("a/cache")).unwrap();
        prepared.load_route(write_route(&dir), true);
        assert!(matches!(
            prepared.save_course(),
            Err(AppError::CourseNotReady)
        ));
        run_until(&mut prepared, |e| matches!(e, AppEvent::WorldReady { .. }));

        prepared.save_course().unwrap();
        let events = run_until(&mut prepared, |e| matches!(e, AppEvent::CourseAdded(_)));
        let Some(AppEvent::CourseAdded(file)) = events.last() else {
            unreachable!()
        };
        assert_eq!(*file, dir.join("a/data/courses/test-loop.tqc"));
        let listed = prepared.courses();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].manifest.name, "Test loop");
        assert!((listed[0].manifest.length_m - 400.0).abs() < 1.0);
        // Prepared once, listed once — the import added it, saving again found it — and with a
        // preview for its card.
        assert!(listed[0].manifest.track.len() > 2);
        assert!(listed[0].manifest.profile.len() > 2);

        let mut other = App::new(dir.join("b/data"), dir.join("b/cache")).unwrap();
        other.import_course(file.clone());
        let events = run_until(&mut other, |e| matches!(e, AppEvent::CourseAdded(_)));
        let Some(AppEvent::CourseAdded(imported)) = events.last() else {
            unreachable!()
        };
        assert!(imported.starts_with(dir.join("b/data/courses")));
        other.open_course(imported.clone());
        let events = run_until(&mut other, |e| matches!(e, AppEvent::RouteLoaded(_)));

        let Some(AppEvent::RouteLoaded(summary)) = events.last() else {
            unreachable!()
        };
        assert!(summary.name == "Test loop" && (summary.length - 400.0).abs() < 1.0);
        assert_eq!(other.loaded_course(), Some(imported.as_path()));
        // Looking at a course does not build its world; riding it does.
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(16));
            other.update(Duration::from_millis(16));
        }
        assert!(other.world().is_none());
        assert!(!other.build_world());
        run_until(&mut other, |e| matches!(e, AppEvent::WorldReady { .. }));
        assert!(other.world().is_some());
        assert!(other.build_world());
    }

    #[test]
    fn opening_another_course_drops_the_one_still_loading() {
        let dir = temp_dir("switch");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        app.load_route(write_route(&dir), true);
        let events = run_until(&mut app, |e| matches!(e, AppEvent::CourseAdded(_)));
        let Some(AppEvent::CourseAdded(course)) = events.last() else {
            unreachable!()
        };
        assert_eq!(app.loaded_course(), Some(course.as_path()));

        // Back and forth quickly, as when leaving and re-entering a course page.
        app.open_course(course.clone());
        app.build_world();
        app.open_course(course.clone());
        let events = run_until(&mut app, |e| matches!(e, AppEvent::RouteLoaded(_)));
        for _ in 0..60 {
            std::thread::sleep(Duration::from_millis(16));
            app.update(Duration::from_millis(16));
        }

        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, AppEvent::RouteLoaded(_)))
                .count(),
            1,
            "{events:?}"
        );
        assert!(app.world().is_none(), "the abandoned load built its world");
    }

    #[test]
    fn courses_can_be_renamed_and_deleted() {
        let dir = temp_dir("rename-course");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        app.load_route(write_route(&dir), true);
        let events = run_until(&mut app, |e| matches!(e, AppEvent::CourseAdded(_)));
        let Some(AppEvent::CourseAdded(course)) = events.last() else {
            unreachable!()
        };

        app.rename_course(course, "  Evening loop ").unwrap();
        assert!(app.rename_course(course, "  ").is_err());
        assert_eq!(app.courses()[0].manifest.name, "Evening loop");
        app.open_course(course.clone());
        let events = run_until(&mut app, |e| matches!(e, AppEvent::RouteLoaded(_)));
        assert!(
            matches!(events.last(), Some(AppEvent::RouteLoaded(s)) if s.name == "Evening loop")
        );

        app.delete_course(course).unwrap();
        assert_eq!(app.courses().len(), 0);
        assert_eq!(app.loaded_course(), None);
    }

    fn sample_course(dir: &Path) -> Vec<u8> {
        let manifest = Manifest {
            format: course::FORMAT_VERSION,
            generator: "ORQWA test".to_owned(),
            name: "Sample climb".to_owned(),
            length_m: 20_000.0,
            elevation_gain_m: 1_150.0,
            max_grade_percent: 12.0,
            created_unix_s: 1_790_000_000,
            attribution: vec!["Map data: © OpenStreetMap contributors (ODbL)".to_owned()],
            route_key: Some("0123456789abcdef".to_owned()),
            track: Vec::new(),
            profile: Vec::new(),
            video: None,
            countries: None,
        };
        let path = dir.join("sample.tqc");
        course::write(&path, &manifest, "<gpx/>", dir, &[]).unwrap();
        std::fs::read(path).unwrap()
    }

    #[test]
    fn a_first_start_has_the_sample_course_and_later_ones_leave_the_library_alone() {
        let dir = temp_dir("sample-course");
        let sample = sample_course(&dir);
        // A first start cut short while writing the library.
        std::fs::create_dir_all(dir.join("data/courses.part")).unwrap();
        std::fs::write(dir.join("data/courses.part/sample-climb.tqc"), b"half").unwrap();
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();

        assert!(app.add_sample_course("sample-climb.tqc", &sample).unwrap());
        let courses = app.courses();
        assert_eq!(courses.len(), 1);
        assert_eq!(courses[0].manifest.name, "Sample climb");
        assert!(!dir.join("data/courses.part").exists());

        assert!(!app.add_sample_course("sample-climb.tqc", &sample).unwrap());
        assert_eq!(app.courses().len(), 1);
        app.delete_course(&courses[0].path).unwrap();
        assert!(!app.add_sample_course("sample-climb.tqc", &sample).unwrap());
        assert_eq!(app.courses().len(), 0, "the deleted sample came back");
    }

    #[test]
    fn riders_with_a_library_get_no_sample_course() {
        let dir = temp_dir("library-before-sample");
        std::fs::create_dir_all(dir.join("data/courses")).unwrap();
        let app = App::new(dir.join("data"), dir.join("cache")).unwrap();

        assert!(
            !app.add_sample_course("sample-climb.tqc", &sample_course(&dir))
                .unwrap()
        );
        assert_eq!(app.courses().len(), 0);
    }

    #[test]
    fn the_course_library_knows_the_riders_estimates_and_last_rides() {
        let dir = temp_dir("library");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        app.load_route(write_route(&dir), true);
        run_until(&mut app, |e| matches!(e, AppEvent::CourseAdded(_)));
        app.load_route(write_climb_route(&dir), true);
        run_until(&mut app, |e| matches!(e, AppEvent::CourseAdded(_)));
        ride_to_the_finish(&mut app, &GhostChoice::None);

        let query = library::CourseQuery {
            view: library::View {
                sort: library::SortKey::Ridden,
                descending: true,
                group: library::GroupKey::None,
            },
            ..library::CourseQuery::default()
        };
        let gallery = app.course_library(&query);

        let courses = &gallery.groups[0].courses;
        let names: Vec<&str> = courses
            .iter()
            .map(|c| c.entry.manifest.name.as_str())
            .collect();
        assert_eq!(names, ["Hill", "Test loop"]);
        assert!(courses[0].last_ridden.is_some());
        assert_eq!(courses[1].last_ridden, None);
        // 1.2 km with a climb takes longer than 400 m on the flat.
        assert!(courses[1].estimated.is_some());
        assert!(courses[0].estimated > courses[1].estimated);

        // A stronger rider is quicker: the estimates follow the rider's figures.
        let before = courses[0].estimated.unwrap();
        let id = app.profile().id.clone();
        let stronger = Profile {
            ftp: Watts(app.profile().profile.ftp.0 * 1.5),
            ..app.profile().profile.clone()
        };
        app.save_profile(Some(&id), stronger).unwrap();
        let after = app.course_library(&query).groups[0].courses[0]
            .estimated
            .unwrap();
        assert!(
            after < before,
            "{after:?} with more power, {before:?} before"
        );
    }

    /// 400 m due north through Bern, far from any border, with elevations.
    fn write_bern_route(dir: &std::path::Path) -> PathBuf {
        let mut xml = String::from("<gpx><trk><name>Bern</name><trkseg>");
        for i in 0..=40 {
            let lat = 46.948 + f64::from(i) * 10.0 / 111_195.0;
            let _ = write!(
                xml,
                r#"<trkpt lat="{lat}" lon="7.447"><ele>540</ele></trkpt>"#
            );
        }
        xml.push_str("</trkseg></trk></gpx>");
        let path = dir.join("bern.gpx");
        std::fs::write(&path, xml).unwrap();
        path
    }

    #[test]
    fn courses_learn_their_countries_once_the_map_tiles_are_at_hand() {
        let dir = temp_dir("countries");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        // Prepared offline without the tiles: its countries are not known yet.
        app.load_route(write_bern_route(&dir), true);
        let events = run_until(&mut app, |e| matches!(e, AppEvent::CourseAdded(_)));
        let Some(AppEvent::CourseAdded(path)) = events.last().cloned() else {
            unreachable!()
        };
        assert_eq!(course::read_manifest(&path).unwrap().countries, None);

        // The tile with Switzerland's borders and label arrives in the cache.
        let tile = orqwa_osm::tile_path(&dir.join("cache/osm"), 6, (33, 22));
        std::fs::create_dir_all(tile.parent().unwrap()).unwrap();
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/countries/6/33/22.pbf"),
            &tile,
        )
        .unwrap();
        app.rename_course(&path, "Bärn").unwrap();
        app.fill_in_countries(true);
        run_until(&mut app, |e| matches!(e, AppEvent::CoursesChanged));

        let manifest = course::read_manifest(&path).unwrap();
        let countries = manifest.countries.unwrap();
        assert_eq!(countries.len(), 1);
        assert_eq!(countries[0].code, "CH");
        assert_eq!(countries[0].names["de"], "Schweiz");
        assert_eq!(manifest.name, "Bärn", "the rename made meanwhile is kept");
        // Nothing left to look up: no lookup, no event.
        app.fill_in_countries(true);
        assert!(app.filling_countries.is_none());

        // A course prepared with the tiles at hand knows its countries at once.
        app.name_next_import("Bern again", false);
        app.load_route(write_bern_route(&dir), true);
        let events = run_until(&mut app, |e| matches!(e, AppEvent::CourseAdded(_)));
        let Some(AppEvent::CourseAdded(again)) = events.last().cloned() else {
            unreachable!()
        };
        let known = course::read_manifest(&again).unwrap().countries.unwrap();
        assert_eq!(known[0].code, "CH");
    }

    #[test]
    fn the_overlay_place_is_the_riders_and_the_graphics_the_computers() {
        let dir = temp_dir("per-rider-per-computer");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        let anna = app.profile().id.clone();
        let window = OverlayWindow {
            x: 100,
            y: 50,
            width: 400,
            height: 300,
            scale: Some(1.5),
        };
        app.set_overlay_window(window).unwrap();
        app.set_graphics_quality(profiles::GraphicsQuality::Ultra)
            .unwrap();
        app.set_fullscreen(true).unwrap();

        let ben = app
            .save_profile(
                None,
                Profile {
                    name: "Ben".to_owned(),
                    ..Profile::default()
                },
            )
            .unwrap();

        assert_eq!(
            app.overlay_window(),
            None,
            "Ben has no place of his own yet"
        );
        assert_eq!(app.graphics_quality(), profiles::GraphicsQuality::Ultra);
        assert!(app.fullscreen(), "the computer's, whoever rides");
        app.select_profile(&anna).unwrap();
        assert_eq!(app.overlay_window(), Some(window));
        assert!(
            dir.join("cache").join(computer::FILE).exists(),
            "kept next to the cache, outside the synced data"
        );
        assert!(
            !dir.join("data")
                .join("profiles")
                .join(&ben)
                .join("overlay.toml")
                .exists()
        );
    }

    #[test]
    fn the_courses_view_is_remembered_and_unknown_choices_fall_back() {
        let dir = temp_dir("courses-view");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        assert_eq!(app.courses_view(), library::View::default());
        let view = library::View {
            sort: library::SortKey::Added,
            descending: true,
            group: library::GroupKey::Terrain,
        };

        app.set_courses_view(view).unwrap();
        assert_eq!(app.courses_view(), view);

        // A choice only a newer ORQWA knows.
        let newer = profiles::CoursesView {
            sort: "colour".to_owned(),
            descending: true,
            group: "terrain".to_owned(),
        };
        profiles::set_courses_view(&dir.join("data"), &newer).unwrap();
        assert_eq!(app.courses_view().sort, library::SortKey::Name);
        assert_eq!(app.courses_view().group, library::GroupKey::Terrain);
    }

    /// A copy of a generated test video in `dir`, so each test owns its files.
    fn video_in(dir: &Path, generated: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::rename(generated, &path)
            .or_else(|_| std::fs::copy(generated, &path).map(|_| ()))
            .unwrap();
        path
    }

    fn loaded_video(app: &mut App, path: PathBuf) -> Vec<AppEvent> {
        app.load_video(path, true);
        run_until(app, |e| {
            matches!(e, AppEvent::CourseAdded(_) | AppEvent::Error(_))
        })
    }

    #[test]
    fn a_gopro_video_with_gps_becomes_a_course_that_follows_the_rider() {
        let dir = temp_dir("gopro");
        let video = video_in(&dir, &orqwa_video::testing::gopro_video("app"), "Ride.MOV");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();

        let events = loaded_video(&mut app, video.clone());

        assert!(
            events
                .iter()
                .any(|e| matches!(e, AppEvent::RouteLoaded(s) if s.name == "Ride")),
            "{events:?}"
        );
        // Ridden along the video unless the rider asks for 3D: no world built for it.
        assert!(app.world().is_none());
        let course = app.video().unwrap();
        assert_eq!(course.video, video);
        // 10 m every quarter second: the video is 2.5 s in after 100 m.
        let at = |m: f64| course.time_at(Meters(m)).as_secs_f64();
        assert!(at(0.0) < 0.05, "{}", at(0.0));
        assert!((at(100.0) - 2.5).abs() < 0.1, "{}", at(100.0));
        assert!(at(10_000.0) <= course.duration.as_secs_f64());
        let listed = app.courses();
        assert_eq!(listed.len(), 1);
        let reference = listed[0].manifest.video.as_ref().unwrap();
        assert_eq!(reference.file_name, "Ride.MOV");
    }

    #[test]
    fn the_video_plays_as_far_as_the_rider_has_come() {
        let dir = temp_dir("video-ride");
        let video = video_in(&dir, &orqwa_video::testing::gopro_video("ride"), "Ride.MOV");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        loaded_video(&mut app, video);
        app.connect_trainer(TrainerChoice::Fake(FakeRider {
            power: Watts(300.0),
            cadence: Rpm(90.0),
            heart: None,
        }))
        .unwrap();
        app.start_ride(Percent(50.0), DescentMode::Coast, &GhostChoice::None)
            .unwrap();

        let mut frames: Vec<(Duration, Duration)> = Vec::new();
        for _ in 0..240 {
            std::thread::sleep(Duration::from_millis(16));
            app.update(Duration::from_millis(16));
            let shown = app.video_time().unwrap();
            if let Some(frame) = app.video_frame() {
                assert_eq!((frame.width, frame.height), (64, 48));
                frames.push((shown, frame.time));
            }
        }

        let distance = app.ride_state().unwrap().distance;
        assert!(distance.0 > 5.0, "rider should be moving: {distance:?}");
        assert!(frames.len() >= 2, "{frames:?}");
        assert!(frames.windows(2).all(|w| w[1].1 > w[0].1), "{frames:?}");
        // Each frame is the one coming up next, decoded ahead, to blend towards.
        assert!(
            frames[1..].iter().all(|(shown, frame)| frame > shown),
            "{frames:?}"
        );
        app.abort_ride();
        assert!(app.video_frame().is_none());
    }

    #[test]
    fn video_courses_find_their_video_on_another_machine() {
        let dir = temp_dir("video-course");
        let video = video_in(
            &dir,
            &orqwa_video::testing::gopro_video("moved"),
            "Gurten.mov",
        );
        let mut prepared = App::new(dir.join("a/data"), dir.join("a/cache")).unwrap();
        let events = loaded_video(&mut prepared, video.clone());
        let Some(AppEvent::CourseAdded(file)) = events.last() else {
            panic!("{events:?}")
        };

        // Another machine: the course elsewhere, the video neither where it was recorded nor
        // in the first machine's library.
        let shared = dir.join("shared");
        let aside = dir.join("aside");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::create_dir_all(&aside).unwrap();
        std::fs::copy(file, shared.join("gurten.tqc")).unwrap();
        std::fs::rename(&video, aside.join("Gurten.mov")).unwrap();
        std::fs::remove_file(file.parent().unwrap().join("Gurten.mov")).unwrap();
        let mut other = App::new(dir.join("b/data"), dir.join("b/cache")).unwrap();
        other.open_course(shared.join("gurten.tqc"));
        let events = run_until(&mut other, |e| matches!(e, AppEvent::Error(_)));
        assert!(
            matches!(events.last(), Some(AppEvent::Error(m)) if m.contains("Gurten.mov")),
            "{events:?}"
        );

        std::fs::rename(aside.join("Gurten.mov"), shared.join("Gurten.mov")).unwrap();
        other.open_course(shared.join("gurten.tqc"));
        run_until(&mut other, |e| matches!(e, AppEvent::RouteLoaded(_)));
        assert_eq!(other.video().unwrap().video, shared.join("Gurten.mov"));
        // Opened, not prepared: nothing is added to that machine's library.
        assert_eq!(other.courses().len(), 0);
    }

    #[test]
    fn a_video_course_outlives_the_file_it_was_imported_from() {
        // #165: the video moved or deleted after the import, the course still rides along it.
        let dir = temp_dir("video-kept");
        let video = video_in(&dir, &orqwa_video::testing::gopro_video("kept"), "Ride.MOV");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        let events = loaded_video(&mut app, video.clone());
        let Some(AppEvent::CourseAdded(file)) = events.last() else {
            panic!("{events:?}")
        };
        let kept = file.parent().unwrap().join("Ride.MOV");
        assert!(kept.is_file(), "no copy beside {}", file.display());
        std::fs::remove_file(&video).unwrap();

        let mut later = App::new(dir.join("data"), dir.join("cache")).unwrap();
        later.open_course(file.clone());
        run_until(&mut later, |e| {
            matches!(e, AppEvent::RouteLoaded(_) | AppEvent::Error(_))
        });

        assert_eq!(later.video().map(|v| v.video.clone()), Some(kept.clone()));

        // The copy goes with the course, once no course rides along it any more.
        let twin = file.with_file_name("twin.tqc");
        std::fs::copy(file, &twin).unwrap();
        later.delete_course(file).unwrap();
        assert!(kept.is_file(), "deleted under another course");
        later.delete_course(&twin).unwrap();
        assert!(!kept.exists(), "left behind");
    }

    #[test]
    fn an_incyclist_route_video_starts_at_its_start_frame() {
        let dir = temp_dir("incyclist");
        video_in(
            &dir,
            &orqwa_video::testing::test_video("incyclist", 64, 48),
            "climb.mp4",
        );
        // 200 m north, 10 m every 0.1 s, from frame 11 at 10 fps: 1 s into the video.
        let mut gpx = String::from("<gpx><trk><trkseg>");
        for i in 0..=20 {
            let lat = 46.0 + f64::from(i) * 10.0 / 111_195.0;
            let _ = write!(
                gpx,
                r#"<trkpt lat="{lat}" lon="7"><ele>500</ele><time>2024-06-01T08:00:{:06.3}Z</time></trkpt>"#,
                f64::from(i) / 10.0
            );
        }
        gpx.push_str("</trkseg></trk></gpx>");
        std::fs::write(dir.join("climb.gpx"), gpx).unwrap();
        std::fs::write(
            dir.join("climb.xml"),
            "<gpx-import><title>Col &amp; Climb</title><video-file-path>climb.mp4</video-file-path>\
             <gpx-file-path>climb.gpx</gpx-file-path><framerate>10</framerate>\
             <start-frame>11</start-frame></gpx-import>",
        )
        .unwrap();
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();

        let events = loaded_video(&mut app, dir.join("climb.xml"));

        assert!(
            events
                .iter()
                .any(|e| matches!(e, AppEvent::RouteLoaded(s) if s.name == "Col & Climb")),
            "{events:?}"
        );
        let course = app.video().unwrap();
        assert_eq!(course.offset, Duration::from_secs(1));
        assert!((course.time_at(Meters(0.0)).as_secs_f64() - 1.0).abs() < 0.05);
        assert!((course.time_at(Meters(40.0)).as_secs_f64() - 1.4).abs() < 0.1);
    }

    /// Sync marks from `(distance m, video s)` pairs.
    fn marks(pairs: &[(f64, f64)]) -> Vec<SyncMark> {
        pairs
            .iter()
            .map(|&(d, t)| SyncMark {
                distance: Meters(d),
                time: Duration::from_secs_f64(t),
            })
            .collect()
    }

    /// 200 m north, 10 m per point, with timestamps of another day's recording.
    fn write_timed_route(dir: &Path) -> PathBuf {
        let mut gpx = String::from("<gpx><trk><trkseg>");
        for i in 0..=20 {
            let lat = 46.0 + f64::from(i) * 10.0 / 111_195.0;
            let _ = write!(
                gpx,
                r#"<trkpt lat="{lat}" lon="7"><ele>500</ele><time>2024-06-01T08:{i:02}:00Z</time></trkpt>"#
            );
        }
        gpx.push_str("</trkseg></trk></gpx>");
        let path = dir.join("commute.gpx");
        std::fs::write(&path, gpx).unwrap();
        path
    }

    #[test]
    fn a_video_without_gps_is_added_to_a_gpx_course_by_its_start_and_end() {
        let dir = temp_dir("aligned");
        let video = video_in(
            &dir,
            &orqwa_video::testing::test_video("aligned", 64, 48),
            "Commute.mp4",
        );
        let probe = video::probe(&video).unwrap();
        assert!(!probe.has_gps);
        assert!((probe.duration.as_secs_f64() - 4.0).abs() < 0.15);
        let mut app = App::new(dir.join("a/data"), dir.join("a/cache")).unwrap();
        // Only a course can take a video.
        assert!(
            app.add_video(&video, &marks(&[(0.0, 1.0), (1.0, 3.0)]))
                .is_err()
        );
        // The GPX course, as imported on the Courses tab and opened on its page.
        app.load_route(write_timed_route(&dir), true);
        let events = run_until(&mut app, |e| matches!(e, AppEvent::CourseAdded(_)));
        let Some(AppEvent::CourseAdded(file)) = events.last() else {
            unreachable!()
        };
        app.open_course(file.clone());
        run_until(&mut app, |e| matches!(e, AppEvent::RouteLoaded(_)));

        // Start and end only; the end mark is put at the route's end whatever its distance.
        app.add_video(&video, &marks(&[(0.0, 1.0), (0.0, 3.0)]))
            .unwrap();

        let length = app.route().unwrap().length().0;
        let at = |app: &App, m: f64| app.video().unwrap().time_at(Meters(m)).as_secs_f64();
        // Evenly between the marks — the GPX's own minutes apart are ignored.
        assert!((at(&app, 0.0) - 1.0).abs() < 0.01);
        assert!((at(&app, length / 2.0) - 2.0).abs() < 0.01);
        assert!((at(&app, length) - 3.0).abs() < 0.01);
        assert!(app.video().unwrap().aligned_by_hand());
        assert!(
            app.add_video(&video, &marks(&[(0.0, 0.0), (0.0, 1.0)]))
                .is_err()
        );

        // Moved later, with a point in between where the footage slows down: a quarter of the
        // route takes two seconds of video, the rest one.
        let quarter = length / 4.0;
        app.align_video(&marks(&[(0.0, 0.5), (quarter, 2.5), (length, 3.5)]))
            .unwrap();
        assert!((at(&app, 0.0) - 0.5).abs() < 0.01);
        assert!((at(&app, quarter / 2.0) - 1.5).abs() < 0.01);
        assert!((at(&app, quarter) - 2.5).abs() < 0.01);
        assert!((at(&app, quarter + (length - quarter) / 2.0) - 3.0).abs() < 0.01);
        // Points must follow each other on the route and in the video, within it.
        for wrong in [
            vec![(0.0, 3.0), (quarter, 2.5), (length, 3.5)],
            vec![
                (0.0, 0.5),
                (quarter, 2.0),
                (quarter / 2.0, 2.5),
                (length, 3.5),
            ],
            vec![(0.0, 0.5), (length + 10.0, 2.5), (length, 3.5)],
            vec![(0.0, 1.0), (quarter, 2.0), (length, 60.0)],
        ] {
            assert!(app.align_video(&marks(&wrong)).is_err(), "{wrong:?}");
        }
        let reference = course::read_manifest(file).unwrap().video.unwrap();
        assert!((reference.offset_s - 0.5).abs() < 1e-9);
        assert_eq!(reference.end_s, Some(3.5));
        assert_eq!(reference.marks.len(), 3);

        let mut other = App::new(dir.join("b/data"), dir.join("b/cache")).unwrap();
        other.open_course(file.clone());
        run_until(&mut other, |e| matches!(e, AppEvent::RouteLoaded(_)));
        assert!((at(&other, 0.0) - 0.5).abs() < 0.01);
        assert!((at(&other, quarter) - 2.5).abs() < 0.01);
        assert!((at(&other, length) - 3.5).abs() < 0.01);

        // Course files with only a start and an end (before points in between) still open.
        update_manifest(file, |m| {
            let video = m.video.as_mut().unwrap();
            video.marks.clear();
            video.end_s = Some(3.0);
        })
        .unwrap();
        other.open_course(file.clone());
        run_until(&mut other, |e| matches!(e, AppEvent::RouteLoaded(_)));
        assert!((at(&other, length / 2.0) - 1.75).abs() < 0.01);

        // Removed again: a 3D course, as before.
        app.remove_video().unwrap();
        assert!(app.video().is_none());
        assert!(course::read_manifest(file).unwrap().video.is_none());
        assert!(!app.build_world());
        run_until(&mut app, |e| matches!(e, AppEvent::WorldReady { .. }));
    }

    #[test]
    fn the_videos_sound_plays_along_at_its_pitch_and_can_be_switched_off() {
        let dir = temp_dir("sound");
        let video = video_in(&dir, &orqwa_video::testing::sound_video("app"), "Ride.mov");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        app.load_route(write_route(&dir), true);
        let events = run_until(&mut app, |e| matches!(e, AppEvent::CourseAdded(_)));
        let Some(AppEvent::CourseAdded(file)) = events.last() else {
            unreachable!()
        };
        app.open_course(file.clone());
        run_until(&mut app, |e| matches!(e, AppEvent::RouteLoaded(_)));
        // Most of the video on the first 40 m: at 6 m/s it plays at about half its speed.
        app.add_video(&video, &marks(&[(0.0, 0.0), (40.0, 3.6), (400.0, 3.95)]))
            .unwrap();
        app.connect_trainer(TrainerChoice::Fake(FakeRider {
            power: Watts(400.0),
            cadence: Rpm(90.0),
            heart: None,
        }))
        .unwrap();
        app.start_ride(Percent(0.0), DescentMode::Coast, &GhostChoice::None)
            .unwrap();
        assert_eq!(app.video_sound_rate(), Some(48_000));

        let ride = |app: &mut App, frames: usize| {
            let mut heard = Vec::new();
            for _ in 0..frames {
                std::thread::sleep(Duration::from_millis(16));
                app.update(Duration::from_millis(16));
                heard.extend(app.video_sound(4_096));
            }
            heard
        };
        let heard = ride(&mut app, 240);

        // Sound keeps up with real time once it runs (≈ 48 000 samples a second).
        assert!(heard.len() > 48_000 * 3, "{} samples", heard.len());
        let last: Vec<f32> = heard[heard.len() - 9_600..].iter().map(|s| s[0]).collect();
        let loudness = last.iter().map(|v| v.abs()).fold(0.0_f32, f32::max);
        assert!(loudness > 0.05, "silent while riding: {loudness}");
        #[allow(clippy::cast_precision_loss)]
        let hertz = last
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count() as f32
            / 2.0
            / 0.2;
        assert!((hertz - 440.0).abs() < 30.0, "pitch {hertz} Hz");

        app.set_video_sound(false);
        let heard = ride(&mut app, 60);
        let tail = &heard[heard.len().saturating_sub(4_800)..];
        assert!(
            tail.iter().flatten().all(|v| v.abs() < 1e-4),
            "still audible"
        );
        app.abort_ride();
        assert_eq!(app.video_sound_rate(), None);
    }

    #[test]
    fn a_video_showing_no_picture_is_reported_once() {
        let dir = temp_dir("no-picture");
        let video = video_in(
            &dir,
            &orqwa_video::testing::gopro_video("no-picture"),
            "Ride.MOV",
        );
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        loaded_video(&mut app, video);
        app.connect_trainer(TrainerChoice::Fake(FakeRider {
            power: Watts(250.0),
            cadence: Rpm(90.0),
            heart: None,
        }))
        .unwrap();
        let ride = |app: &mut App, take_frames: bool| {
            app.start_ride(Percent(50.0), DescentMode::Coast, &GhostChoice::None)
                .unwrap();
            let mut problems = Vec::new();
            for _ in 0..360 {
                std::thread::sleep(Duration::from_millis(16));
                for event in app.update(Duration::from_millis(16)) {
                    if let AppEvent::Error(message) = event {
                        problems.push(message);
                    }
                }
                if take_frames {
                    let _ = app.video_frame();
                }
            }
            app.abort_ride();
            problems
        };

        // Shown as it should: nothing to report.
        assert_eq!(ride(&mut app, true), Vec::<Failure>::new());
        // Never shown (as on a front end that cannot display it): said once, not every frame.
        let problems = ride(&mut app, false);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("no picture"), "{problems:?}");
    }

    #[test]
    fn a_video_course_is_ridden_along_its_video_or_in_3d() {
        let dir = temp_dir("video-or-3d");
        let video = video_in(
            &dir,
            &orqwa_video::testing::gopro_video("or-3d"),
            "Ride.MOV",
        );
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        loaded_video(&mut app, video);
        app.connect_trainer(TrainerChoice::Fake(FakeRider {
            power: Watts(250.0),
            cadence: Rpm(90.0),
            heart: None,
        }))
        .unwrap();
        let start = |app: &mut App| {
            app.start_ride(Percent(50.0), DescentMode::Coast, &GhostChoice::None)
                .unwrap();
        };

        // In 3D: the route's world is built, the video stays off.
        app.ride_along_video(false);
        assert!(!app.build_world());
        run_until(&mut app, |e| matches!(e, AppEvent::WorldReady { .. }));
        start(&mut app);
        assert!(!app.riding_along_video());
        assert!(app.video_frame().is_none());
        app.abort_ride();

        // Along the video, as before.
        app.ride_along_video(true);
        start(&mut app);
        assert!(app.riding_along_video());
        assert!(!app.ambient_sound_active());
        assert_eq!(app.ambient_sound(2_400), [] as [[f32; 2]; 0]);
        app.abort_ride();

        // Without its video, a course is ridden in 3D only.
        app.remove_video().unwrap();
        app.ride_along_video(true);
        start(&mut app);
        assert!(!app.riding_along_video());
    }

    #[test]
    fn a_tacx_real_life_video_rides_along_its_video_by_distance_and_slope() {
        use orqwa_video::testing::{pgmf_bytes, rlv_bytes, test_video};

        let dir = temp_dir("rlv");
        video_in(&dir, &test_video("rlv", 64, 48), "stelvio.mp4");
        std::fs::write(
            dir.join("Stelvio.rlv"),
            rlv_bytes(r"C:\Tacx\Videos\STELVIO.MP4"),
        )
        .unwrap();
        std::fs::write(dir.join("Stelvio.PGMF"), pgmf_bytes("Passo dello Stelvio")).unwrap();
        assert_eq!(
            App::suggested_course_name(&dir.join("Stelvio.rlv")),
            "Stelvio"
        );
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();

        app.load_video(dir.join("Stelvio.rlv"), false);
        let file = added(&mut app);

        let route = app.route().unwrap();
        assert!(
            (route.length().0 - 30.0).abs() < 0.5,
            "{}",
            route.length().0
        );
        // The elevations are the course's own (from 500 m, up then down), not terrain data's
        // from wherever its drawn line happens to lie.
        assert_eq!(route.elevation_source(), ElevationSource::File);
        assert!(route.elevation_gain().0 > 0.3);
        let course = app.video().unwrap();
        assert!(!course.located);
        let at = |m: f64| course.time_at(Meters(m)).as_secs_f64();
        // 1 m per frame for 20 m, then 0.5 m: 2 s at 20 m, 3 s at 25 m.
        assert!((at(20.0) - 2.0).abs() < 0.05, "{}", at(20.0));
        assert!((at(25.0) - 3.0).abs() < 0.05, "{}", at(25.0));
        // The PGMF's 17-character name field cut "Passo dello Stelvio" short: the RLV's
        // file name is used instead.
        assert_eq!(course::read_manifest(&file).unwrap().name, "Stelvio");

        // Only along its video: it has no place for a 3D world, and keeps its video.
        app.ride_along_video(false);
        app.connect_trainer(TrainerChoice::Fake(FakeRider {
            power: Watts(200.0),
            cadence: Rpm(90.0),
            heart: None,
        }))
        .unwrap();
        app.start_ride(Percent(50.0), DescentMode::Coast, &GhostChoice::None)
            .unwrap();
        assert!(app.riding_along_video());
        app.abort_ride();
        assert!(app.remove_video().is_err());

        // Opened again from its file: still a course without a place.
        app.open_course(file);
        run_until(&mut app, |e| matches!(e, AppEvent::RouteLoaded(_)));
        assert!(!app.video().unwrap().located);
    }

    #[test]
    fn videos_with_gps_follow_it_and_cannot_be_moved() {
        let dir = temp_dir("gps-align");
        let video = video_in(
            &dir,
            &orqwa_video::testing::gopro_video("align"),
            "Ride.MOV",
        );
        assert!(video::probe(&video).unwrap().has_gps);
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        loaded_video(&mut app, video);

        assert!(!app.video().unwrap().aligned_by_hand());
        assert!(app.align_video(&marks(&[(0.0, 0.0), (0.0, 2.0)])).is_err());
    }

    #[test]
    fn previews_show_the_frame_at_a_moment() {
        let dir = temp_dir("preview");
        let video = video_in(
            &dir,
            &orqwa_video::testing::test_video("preview", 64, 48),
            "a.mp4",
        );
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();

        let frame = app
            .video_preview(&video, Duration::from_millis(2050))
            .unwrap();
        assert_eq!(frame.time, Duration::from_secs(2));
        let earlier = app
            .video_preview(&video, Duration::from_millis(500))
            .unwrap();
        assert_eq!(earlier.time, Duration::from_millis(500));
        assert!(
            app.video_preview(&dir.join("missing.mp4"), Duration::ZERO)
                .is_err()
        );
    }

    #[test]
    fn videos_without_gps_are_refused_with_a_reason() {
        let dir = temp_dir("nogps");
        let video = video_in(
            &dir,
            &orqwa_video::testing::test_video("app-nogps", 64, 48),
            "a.mp4",
        );
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();

        let events = loaded_video(&mut app, video);

        assert!(
            matches!(events.last(), Some(AppEvent::Error(m)) if m.contains("no GPS")),
            "{events:?}"
        );
        assert!(app.video().is_none());
        assert_eq!(app.courses().len(), 0);
    }

    /// An Incyclist route video in `dir` for `video` (copied there), on a 200 m GPX.
    fn write_incyclist(dir: &Path, video: &Path) {
        std::fs::copy(video, dir.join("climb.mp4")).unwrap();
        let mut gpx = String::from("<gpx><trk><trkseg>");
        for i in 0..=20 {
            let lat = 46.0 + f64::from(i) * 10.0 / 111_195.0;
            let _ = write!(
                gpx,
                r#"<trkpt lat="{lat}" lon="7"><ele>500</ele><time>2024-06-01T08:00:{:06.3}Z</time></trkpt>"#,
                f64::from(i) / 10.0
            );
        }
        gpx.push_str("</trkseg></trk></gpx>");
        std::fs::write(dir.join("climb.gpx"), gpx).unwrap();
        std::fs::write(
            dir.join("climb.xml"),
            "<gpx-import><title>Col &amp; Climb</title><video-file-path>climb.mp4</video-file-path>\
             <gpx-file-path>climb.gpx</gpx-file-path><framerate>10</framerate>\
             <start-frame>1</start-frame></gpx-import>",
        )
        .unwrap();
    }

    fn added(app: &mut App) -> PathBuf {
        let events = run_until(app, |e| {
            matches!(e, AppEvent::CourseAdded(_) | AppEvent::Error(_))
        });
        match events.last() {
            Some(AppEvent::CourseAdded(path)) => path.clone(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_route_video_of_an_imported_gpx_is_a_course_of_its_own() {
        let dir = temp_dir("same-gpx");
        write_incyclist(&dir, &orqwa_video::testing::test_video("same-gpx", 64, 48));
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();

        app.load_route(dir.join("climb.gpx"), true);
        let gpx_course = added(&mut app);
        app.load_video(dir.join("climb.xml"), true);
        let video_course = added(&mut app);

        assert_ne!(gpx_course, video_course);
        let courses = app.courses();
        assert_eq!(courses.len(), 2);
        assert_eq!(
            courses
                .iter()
                .filter(|c| c.manifest.video.is_some())
                .count(),
            1
        );
    }

    #[test]
    fn imported_courses_get_the_name_given_and_replace_only_when_asked() {
        let dir = temp_dir("naming");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        let route = write_route(&dir);

        app.name_next_import("  Evening loop ", false);
        let first = added_after(&mut app, |app| app.load_route(route.clone(), true));
        assert_eq!(course::read_manifest(&first).unwrap().name, "Evening loop");
        assert_eq!(app.course_named("evening LOOP"), Some(first.clone()));
        // Same name, kept next to it.
        app.name_next_import("Evening loop", false);
        let second = added_after(&mut app, |app| app.load_route(route.clone(), true));
        assert_ne!(first, second);
        assert_eq!(app.courses().len(), 2);
        // Replacing: the course of that name is the new one, nothing added.
        app.name_next_import("Evening loop", true);
        write_incyclist(&dir, &orqwa_video::testing::test_video("naming", 64, 48));
        let replaced = added_after(&mut app, |app| app.load_video(dir.join("climb.xml"), true));
        assert_eq!(app.courses().len(), 2);
        assert!(replaced == first || replaced == second);
        assert!(course::read_manifest(&replaced).unwrap().video.is_some());
        // Course files too.
        app.name_next_import("Shared", false);
        let copied = added_after(&mut app, |app| app.import_course(replaced.clone()));
        assert_eq!(course::read_manifest(&copied).unwrap().name, "Shared");
        assert_eq!(app.courses().len(), 3);
    }

    fn added_after(app: &mut App, import: impl FnOnce(&mut App)) -> PathBuf {
        import(app);
        added(app)
    }

    #[test]
    fn imports_suggest_the_name_the_files_give() {
        let dir = temp_dir("suggest");
        write_incyclist(&dir, &orqwa_video::testing::test_video("suggest", 64, 48));
        let named = dir.join("named.gpx");
        std::fs::write(&named, r#"<gpx><trk><name>Gurten</name><trkseg><trkpt lat="46" lon="7"/></trkseg></trk></gpx>"#).unwrap();

        assert_eq!(App::suggested_course_name(&named), "Gurten");
        assert_eq!(App::suggested_course_name(&dir.join("climb.gpx")), "climb");
        assert_eq!(
            App::suggested_course_name(&dir.join("climb.xml")),
            "Col & Climb"
        );
        assert_eq!(App::suggested_course_name(&dir.join("Ride.MOV")), "Ride");
    }

    #[test]
    fn saving_a_course_twice_keeps_both() {
        let library = temp_dir("unique");
        std::fs::write(library.join("lake-biel.tqc"), b"").unwrap();

        assert_eq!(
            unique_course_path(&library, "Lake Biel"),
            library.join("lake-biel-2.tqc")
        );
        assert_eq!(
            unique_course_path(&library, "Gurten / Bern!"),
            library.join("gurten-bern.tqc")
        );
        assert_eq!(
            unique_course_path(&library, "??"),
            library.join("course.tqc")
        );
    }

    #[test]
    fn generates_the_world_after_loading_a_route() {
        let dir = temp_dir("world");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();

        app.load_route(write_route(&dir), true);
        let events = run_until(&mut app, |e| matches!(e, AppEvent::WorldReady { .. }));

        assert!(matches!(events.last(), Some(AppEvent::WorldReady { chunks, .. }) if *chunks > 0));
        assert!(app.world().is_some_and(|w| !w.road.vertices.is_empty()));
    }

    #[test]
    fn reports_progress_through_all_stages() {
        let dir = temp_dir("progress");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();

        app.load_route(write_route(&dir), true);
        let events = run_until(&mut app, |e| matches!(e, AppEvent::WorldReady { .. }));

        for stage in [
            LoadStage::Route,
            LoadStage::Map,
            LoadStage::Elevation,
            LoadStage::World,
        ] {
            let finished = events.iter().any(|e| {
                matches!(e, AppEvent::LoadProgress { stage: s, done, total } if *s == stage && done == total)
            });
            assert!(finished, "{stage:?} not completed: {events:?}");
        }
    }

    /// 300 m flat, 600 m at 6 %, 300 m flat, due north.
    fn write_climb_route(dir: &std::path::Path) -> PathBuf {
        let mut xml = String::from("<gpx><trk><name>Hill</name><trkseg>");
        for i in 0..=120 {
            let lat = 46.0 + f64::from(i) * 10.0 / 111_195.0;
            let ele = 500.0 + f64::from((i - 30).clamp(0, 60)) * 0.6;
            let _ = write!(
                xml,
                r#"<trkpt lat="{lat}" lon="7"><ele>{ele}</ele></trkpt>"#
            );
        }
        xml.push_str("</trkseg></trk></gpx>");
        let path = dir.join("hill.gpx");
        std::fs::write(&path, xml).unwrap();
        path
    }

    /// Rides the loaded route with the fake trainer in fast-forward and saves it.
    fn ride_to_the_finish(app: &mut App, ghost: &GhostChoice) -> Vec<AppEvent> {
        app.connect_trainer(TrainerChoice::Fake(FakeRider {
            power: Watts(400.0),
            cadence: Rpm(90.0),
            heart: None,
        }))
        .unwrap();
        app.start_ride(Percent(50.0), DescentMode::Coast, ghost)
            .unwrap();
        // Whole updates: the ride starts in the one the trainer connects in.
        let mut events = Vec::new();
        while !events.iter().any(|e| matches!(e, AppEvent::Connected(_))) {
            std::thread::sleep(Duration::from_millis(16));
            events.extend(app.update(Duration::from_millis(16)));
        }
        for _ in 0..2000 {
            std::thread::sleep(Duration::from_millis(2));
            events.extend(app.update(Duration::from_millis(500)));
            if events.iter().any(|e| matches!(e, AppEvent::RideFinished)) {
                app.finish_ride();
                return events;
            }
        }
        panic!("did not finish: {events:?}");
    }

    #[test]
    fn climbs_and_routes_are_timed_against_the_riders_records() {
        let dir = temp_dir("records");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        app.load_route(write_climb_route(&dir), true);
        run_until(&mut app, |e| matches!(e, AppEvent::RouteLoaded(_)));
        assert_eq!(app.route().unwrap().climbs().len(), 1);

        // Nothing to race on a first ride.
        assert!(matches!(
            app.ghost_for(
                app.route().unwrap(),
                DescentMode::Coast,
                &GhostChoice::PersonalBest
            ),
            Err(AppError::GhostUnavailable(_))
        ));
        let first = ride_to_the_finish(&mut app, &GhostChoice::None);
        let climb = first.iter().find_map(|e| match e {
            AppEvent::Moment(Moment::ClimbTopped {
                index: 0, elapsed, ..
            }) => Some(*elapsed),
            _ => None,
        });
        assert!(climb.is_some_and(|t| t > Duration::ZERO), "{first:?}");
        assert!(
            first
                .iter()
                .any(|e| matches!(e, AppEvent::Moment(Moment::FirstRide { .. }))),
            "the first ride on the course is told (R124): {first:?}"
        );

        let records = app.records_for(app.route().unwrap());
        assert_eq!(records.climbs, [climb]);
        assert!(records.route.is_some());

        // Riding it again compares with the first ride.
        std::thread::sleep(Duration::from_millis(1100)); // a new FIT file name
        let second = ride_to_the_finish(&mut app, &GhostChoice::PersonalBest);
        assert!(app.records_for(app.route().unwrap()).ridden);
        assert!(
            !second
                .iter()
                .any(|e| matches!(e, AppEvent::Moment(Moment::FirstRide { .. }))),
            "{second:?}"
        );
        // At the same power, the climb takes about as long; a record only if it is faster.
        assert!(second.iter().any(|e| match e {
            AppEvent::Moment(Moment::ClimbTopped { elapsed, .. }) => Some(*elapsed) >= climb,
            AppEvent::Moment(Moment::ClimbRecord { elapsed, gain, .. }) =>
                climb == Some(*elapsed + *gain),
            _ => false,
        }));
        let history = app.history();
        assert_eq!(history.len(), 2);
        // Racing the first ride at the same power: the gap stays about zero.
        app.start_ride(
            Percent(50.0),
            DescentMode::Coast,
            &GhostChoice::PersonalBest,
        )
        .unwrap();
        let ghost = app.ghost_state().unwrap();
        assert_eq!(ghost.name, "Your best");
        assert_eq!(ghost.distance, Meters(0.0));
        app.finish_ride();
        app.start_ride(
            Percent(50.0),
            DescentMode::Coast,
            &GhostChoice::WattsPerKg(3.0),
        )
        .unwrap();
        assert_eq!(app.ghost_state().unwrap().name, "Pacer 3.0 W/kg");
        app.finish_ride();
        // Each record belongs to a ride; equal times would count for both.
        assert!(history.iter().any(|h| h.route_record));
        assert!(history.iter().any(|h| h.climb_records == [true]));
        app.shutdown();
    }

    #[test]
    fn riders_keep_their_own_hud_layout() {
        let dir = temp_dir("hud");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        assert_eq!(app.hud_layout(), hud::DEFAULT_LAYOUT);

        let saved = app
            .set_hud_layout(&["power_3s".to_owned(), "nonsense".to_owned()])
            .unwrap();

        assert_eq!(saved, ["power_3s"]);
        assert_eq!(app.hud_layout(), ["power_3s"]);
        let first = app.profile().id.clone();
        let second = app.save_profile(None, Profile::default()).unwrap();
        assert_eq!(app.hud_layout(), hud::DEFAULT_LAYOUT);
        // The Profile tab shows every rider's layout, not only the active one's.
        assert_eq!(app.hud_layout_of(&first), ["power_3s"]);
        assert_eq!(app.hud_layout_of(&second), hud::DEFAULT_LAYOUT);
    }

    #[test]
    fn riders_have_their_own_profiles_and_rides() {
        let dir = temp_dir("profiles");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        assert_eq!(app.profile().id, "rider");

        let anna = Profile {
            name: "Anna".to_owned(),
            ftp: Watts(280.0),
            ..Profile::default()
        };
        let id = app.save_profile(None, anna.clone()).unwrap();

        assert_eq!(app.profile().profile, anna);
        let names: Vec<String> = app.profiles().into_iter().map(|p| p.profile.name).collect();
        assert_eq!(names, ["Anna", "Rider"]);
        // The choice survives a restart.
        let restarted = App::new(dir.join("data"), dir.join("cache")).unwrap();
        assert_eq!(restarted.profile().id, id);
        assert!(matches!(
            app.select_profile("nobody"),
            Err(AppError::UnknownProfile)
        ));
    }

    #[test]
    fn aborted_rides_leave_no_trace() {
        let dir = temp_dir("abort");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        app.load_route(write_route(&dir), true);
        run_until(&mut app, |e| matches!(e, AppEvent::RouteLoaded(_)));
        app.connect_trainer(TrainerChoice::Fake(FakeRider {
            power: Watts(250.0),
            cadence: Rpm(90.0),
            heart: None,
        }))
        .unwrap();
        app.start_ride(Percent(50.0), DescentMode::Coast, &GhostChoice::None)
            .unwrap();
        run_until(&mut app, |e| matches!(e, AppEvent::Connected(_)));
        for _ in 0..60 {
            std::thread::sleep(Duration::from_millis(16));
            app.update(Duration::from_millis(50));
        }

        app.abort_ride();

        assert!(app.ride_state().is_none());
        assert_eq!(app.finish_ride(), []);
        assert_eq!(app.history().len(), 0);
        app.shutdown();
    }

    #[test]
    fn without_remembered_devices_nothing_is_scanned() {
        let dir = temp_dir("reconnect");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();

        // No Bluetooth is touched (the container has none): nothing to reconnect.
        assert!(!app.reconnect_remembered());
        assert_eq!(app.update(Duration::ZERO), []);
    }

    #[test]
    fn the_devices_show_whether_they_are_connected_and_a_reconnect_keeps_the_connected() {
        let dir = temp_dir("device-status");
        let data = dir.join("data");
        let mut app = App::new(data.clone(), dir.join("cache")).unwrap();
        let strap = profiles::RememberedDevice {
            id: "strap".to_owned(),
            name: "Polar H9".to_owned(),
        };
        let rider = app.profile().id.clone();
        profiles::remember_device(&data, &rider, DeviceRole::HeartRate, strap).unwrap();
        let status = |app: &App| {
            app.devices()
                .into_iter()
                .map(|d| (d.kind, d.state))
                .collect::<Vec<_>>()
        };
        app.connect_trainer(TrainerChoice::Fake(FakeRider {
            power: Watts(200.0),
            cadence: Rpm(90.0),
            heart: None,
        }))
        .unwrap();
        // Chosen, so on its way (R76), until the driver says it is connected.
        assert_eq!(
            status(&app)[0],
            (DeviceKind::Trainer, DeviceState::Connecting)
        );
        run_until(&mut app, |e| matches!(e, AppEvent::Connected(_)));
        // What it reports is at hand between rides too, for the devices screen (R80).
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.device_reading(DeviceKind::Trainer).is_none() {
            assert!(
                Instant::now() < deadline,
                "no reading from the fake trainer"
            );
            std::thread::sleep(Duration::from_millis(16));
            app.update(Duration::from_millis(16));
        }
        let reading = app.device_reading(DeviceKind::Trainer).unwrap();
        assert_eq!(reading.power, Some(Watts(200.0)));
        assert_eq!(reading.cadence, Some(Rpm(90.0)));
        assert!(app.device_reading(DeviceKind::HeartRateSensor).is_none());

        // The strap used last was not found: the rider can ask to reconnect it (R69).
        assert_eq!(
            status(&app),
            [
                (DeviceKind::Trainer, DeviceState::Connected),
                (DeviceKind::HeartRateSensor, DeviceState::Missing)
            ]
        );
        assert_eq!(app.devices()[1].name, "Polar H9");
        // A reconnect scans for the strap (no Bluetooth in the container) and leaves the
        // connected trainer alone.
        assert!(app.reconnect_devices());
        app.update(Duration::ZERO);
        assert!(app.trainer_connected());
        assert_eq!(
            status(&app)[0],
            (DeviceKind::Trainer, DeviceState::Connected)
        );
        // Choosing no strap forgets it: nothing to reconnect next time (R76).
        app.disconnect_heart_rate().unwrap();
        assert_eq!(
            status(&app),
            [(DeviceKind::Trainer, DeviceState::Connected)]
        );
        assert!(
            profiles::remembered_devices(&data, &rider)
                .heart_rate
                .is_none()
        );
        app.shutdown();
    }

    /// Links a Bluetooth device in `role` as the system's `id`, without Bluetooth: a fake
    /// driver stands in for it.
    fn link(app: &mut App, role: DeviceRole, id: &str) {
        let handle = {
            let _runtime = app.runtime.enter();
            let rider = FakeRider {
                power: Watts(0.0),
                cadence: Rpm(0.0),
                heart: None,
            };
            fake::spawn(rider, Duration::from_secs(1))
        };
        let id = Some(id.to_owned());
        match role {
            DeviceRole::Trainer => (app.trainer, app.trainer_id) = (Some(handle), id),
            DeviceRole::HeartRate => (app.sensor, app.sensor_id) = (Some(handle), id),
            DeviceRole::PowerMeter => (app.power_meter, app.power_meter_id) = (Some(handle), id),
            DeviceRole::CadenceSensor => {
                (app.cadence_sensor, app.cadence_sensor_id) = (Some(handle), id);
            }
            DeviceRole::Controller => unreachable!("a shifter is linked as a shift input"),
        }
    }

    fn linked_id(app: &App, role: DeviceRole) -> Option<String> {
        app.linked(role).map(|(id, _)| id.to_owned())
    }

    #[test]
    fn riders_switch_to_their_own_devices_and_a_shared_one_stays_connected() {
        let dir = temp_dir("rider-devices");
        let data = dir.join("data");
        let mut app = App::new(data.clone(), dir.join("cache")).unwrap();
        let anna = app.profile().id.clone();
        for (role, id, name) in [
            (DeviceRole::Trainer, "kickr", "KICKR CORE"),
            (DeviceRole::HeartRate, "anna-strap", "Polar H9 A"),
        ] {
            let device = profiles::RememberedDevice {
                id: id.to_owned(),
                name: name.to_owned(),
            };
            profiles::remember_device(&data, &anna, role, device).unwrap();
            link(&mut app, role, id);
        }

        // A new rider starts with the devices in use: adding Ben lets nothing go.
        let ben = Profile {
            name: "Ben".to_owned(),
            ..Profile::default()
        };
        let ben = app.save_profile(None, ben).unwrap();
        assert_eq!(
            linked_id(&app, DeviceRole::Trainer).as_deref(),
            Some("kickr")
        );
        assert_eq!(
            linked_id(&app, DeviceRole::HeartRate).as_deref(),
            Some("anna-strap")
        );
        // Ben rides without Anna's strap: it is let go and forgotten for him alone.
        app.disconnect_heart_rate().unwrap();
        assert!(
            profiles::remembered_devices(&data, &anna)
                .heart_rate
                .is_some()
        );

        app.select_profile(&anna).unwrap();
        assert!(app.reconnect_remembered(), "Anna's strap is looked for");
        assert_eq!(
            app.devices()
                .iter()
                .map(|d| (d.name.as_str(), d.state))
                .collect::<Vec<_>>(),
            [
                ("Fake trainer", DeviceState::Connecting),
                ("Polar H9 A", DeviceState::Missing)
            ],
            "the trainer they share stays linked"
        );
        link(&mut app, DeviceRole::HeartRate, "anna-strap");

        app.select_profile(&ben).unwrap();
        assert_eq!(
            linked_id(&app, DeviceRole::Trainer).as_deref(),
            Some("kickr")
        );
        assert_eq!(
            linked_id(&app, DeviceRole::HeartRate),
            None,
            "Anna's strap is not Ben's"
        );
        app.shutdown();
    }

    #[test]
    fn the_fake_trainer_is_nobodys_and_stays_when_riders_switch() {
        let dir = temp_dir("rider-fake");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        let anna = app.profile().id.clone();
        app.connect_trainer(TrainerChoice::Fake(FakeRider {
            power: Watts(200.0),
            cadence: Rpm(90.0),
            heart: None,
        }))
        .unwrap();

        let ben = Profile {
            name: "Ben".to_owned(),
            ..Profile::default()
        };
        app.save_profile(None, ben).unwrap();
        app.select_profile(&anna).unwrap();

        assert!(app.trainer.is_some());
        app.shutdown();
    }

    #[test]
    fn each_rider_has_their_own_buttons_and_cadence_source() {
        let dir = temp_dir("rider-buttons");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        let anna = app.profile().id.clone();
        let ben = Profile {
            name: "Ben".to_owned(),
            ..Profile::default()
        };
        let ben = app.save_profile(None, ben).unwrap();
        let swapped = ButtonMap::shifting(2, 1);

        app.set_button_map(ControllerModel::ZwiftClick, swapped)
            .unwrap();
        app.set_cadence_source(CadenceSource::PowerMeter).unwrap();
        app.select_profile(&anna).unwrap();

        assert_eq!(
            app.button_map(ControllerModel::ZwiftClick),
            ButtonMap::default()
        );
        assert_eq!(app.cadence_source(), CadenceSource::Automatic);
        app.select_profile(&ben).unwrap();
        assert_eq!(
            app.button_map(ControllerModel::ZwiftClick),
            swapped,
            "the buttons shift as Ben set them"
        );
        assert_eq!(app.cadence_source(), CadenceSource::PowerMeter);
        app.shutdown();
    }

    #[test]
    fn a_failed_attempt_to_connect_says_why_beside_the_device_until_it_is_let_go() {
        let dir = temp_dir("device-failure");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        let period = Duration::from_millis(20);
        {
            let _runtime = app.runtime.enter();
            app.sensor = Some(fake::spawn_failing(
                "Polar H9",
                "connecting timed out",
                period,
            ));
            let buttons = Arc::clone(&app.buttons);
            app.controller_id = Some("click".to_owned());
            app.add_shift_input(Box::new(orqwa_devices::shift::Controller::new(
                fake::spawn_failing("Zwift Click", "device not found", period),
                ControllerModel::ZwiftClick,
                buttons,
            )));
        }
        let failed = |name: &str, reason: &str| AppEvent::ConnectionFailed {
            name: name.to_owned(),
            reason: reason.to_owned(),
        };
        let mut events = run_until(
            &mut app,
            |e| matches!(e, AppEvent::ConnectionFailed { name, .. } if name == "Polar H9"),
        );
        events.extend(run_until(
            &mut app,
            |e| matches!(e, AppEvent::ConnectionFailed { name, .. } if name == "Zwift Click"),
        ));
        assert!(events.contains(&failed("Polar H9", "connecting timed out")));
        assert!(events.contains(&failed("Zwift Click", "device not found")));
        // A link never made is not lost: the rider hears of the failure, not of a drop.
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AppEvent::Disconnected(_)))
        );

        // The devices show the reason while their drivers keep trying (R80).
        let devices = app.devices();
        let reasons = devices
            .iter()
            .map(|d| (d.kind, d.state, d.failure.as_deref()))
            .collect::<Vec<_>>();
        assert_eq!(
            reasons,
            [
                (
                    DeviceKind::HeartRateSensor,
                    DeviceState::Connecting,
                    Some("connecting timed out")
                ),
                (
                    DeviceKind::Controller,
                    DeviceState::Connecting,
                    Some("device not found")
                ),
            ]
        );
        assert!(app.device_reading(DeviceKind::HeartRateSensor).is_none());
        // The shifter's reason is told once, not with every attempt.
        let again = run_until(
            &mut app,
            |e| matches!(e, AppEvent::ConnectionFailed { name, .. } if name == "Polar H9"),
        );
        assert!(!again.contains(&failed("Zwift Click", "device not found")));

        // Let go, the devices take their reasons with them.
        app.disconnect_heart_rate().unwrap();
        app.disconnect_controller().unwrap();
        assert_eq!(app.devices(), []);
        app.shutdown();
    }

    #[test]
    fn a_connected_devices_battery_is_kept_until_it_is_let_go() {
        let dir = temp_dir("device-battery");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        let strap = Battery {
            level: Some(64),
            ..Battery::default()
        };
        let pods = Battery {
            left: Some(80),
            right: Some(75),
            level: None,
        };
        {
            let _runtime = app.runtime.enter();
            app.sensor = Some(fake::spawn_with_battery("Polar H9", strap));
            let buttons = Arc::clone(&app.buttons);
            app.controller_id = Some("click-v2".to_owned());
            app.add_shift_input(Box::new(orqwa_devices::shift::Controller::new(
                fake::spawn_with_battery("Zwift Click v2", pods),
                ControllerModel::ZwiftClickV2,
                buttons,
            )));
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while app.device_battery(DeviceKind::HeartRateSensor).is_none()
            || app.device_battery(DeviceKind::Controller).is_none()
        {
            assert!(std::time::Instant::now() < deadline, "no battery told");
            std::thread::sleep(Duration::from_millis(16));
            app.update(Duration::from_millis(16));
        }

        // Also between rides, for the devices screen (R97).
        assert_eq!(app.device_battery(DeviceKind::HeartRateSensor), Some(strap));
        assert_eq!(app.device_battery(DeviceKind::Controller), Some(pods));
        assert_eq!(app.device_battery(DeviceKind::Trainer), None);

        app.disconnect_heart_rate().unwrap();
        app.disconnect_controller().unwrap();
        assert_eq!(app.device_battery(DeviceKind::HeartRateSensor), None);
        assert_eq!(app.device_battery(DeviceKind::Controller), None);
        app.shutdown();
    }

    #[test]
    fn ride_needs_route_and_trainer() {
        let dir = temp_dir("needs");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();

        assert!(matches!(
            app.start_ride(Percent(50.0), DescentMode::Coast, &GhostChoice::None),
            Err(AppError::NoRoute)
        ));
        assert!(matches!(
            app.connect_heart_rate(0),
            Err(AppError::UnknownDevice)
        ));
    }
}
