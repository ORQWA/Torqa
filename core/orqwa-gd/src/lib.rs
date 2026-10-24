//! Godot GDExtension bindings exposing the ORQWA core to the presentation layer.
//!
//! This crate only translates between Godot types and [`orqwa_app`]; it holds no logic.

// This crate is the FFI boundary: gdext's entry point is an `unsafe impl`, and the
// `#[gdextension]` macro drops item-level attributes, so the allow must be crate-wide.
#![allow(unsafe_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use godot::classes::image::Format as ImageFormat;
use godot::classes::{Engine, INode, Image, Node, TranslationServer};
use godot::prelude::*;
use orqwa_app::analysis::{BestEffort, Comparison};
use orqwa_app::failure::{Failure, Kind};
use orqwa_app::hud::MetricKind;
use orqwa_app::media::MediaCommand;
use orqwa_app::moments::Moment;
use orqwa_app::{App, AppEvent, GhostChoice, TrainerChoice, paths};
use orqwa_app::{library, view};
use orqwa_devices::ble::{ControllerModel, DeviceKind};
use orqwa_devices::fake::{FakeHeart, FakeRider};
use orqwa_domain::profile::{Avatar, Drivetrain, MapStyle, Profile, UnitSystem, Voice};
use orqwa_domain::shifting::{BUTTONS, ButtonAction, Control, Press, Shift};
use orqwa_domain::telemetry::CadenceSource;
use orqwa_domain::units::{
    BeatsPerMinute, Kilograms, Meters, MetersPerSecond, Percent, Rpm, Watts,
};
use orqwa_domain::workout::{Cue, Intensity, Plan, Step, Target};
use orqwa_physics::{DescentMode, lean_angle};
use orqwa_routes::{ElevationSource, LocalProjection};
use orqwa_session::estimate;
use orqwa_session::workout::{EffortTest, HeartRateHold, RampTest, Workout};
use orqwa_world::MapFeature;

mod log;

struct OrqwaExtension;

#[gdextension]
unsafe impl ExtensionLibrary for OrqwaExtension {}

/// Static information about the ORQWA core.
#[derive(GodotClass)]
#[class(base = RefCounted, init)]
pub struct OrqwaCore;

#[godot_api]
impl OrqwaCore {
    /// Version of the ORQWA core.
    #[func]
    fn version() -> GString {
        GString::from(orqwa_domain::version())
    }

    /// The commit the core is built from, abbreviated; empty when the build could not tell
    /// (R83).
    #[func]
    fn commit() -> GString {
        GString::from(COMMIT)
    }

    /// The branch the core is built from; empty when the build could not tell, as for a
    /// release built from its tag (R130).
    #[func]
    fn branch() -> GString {
        GString::from(BRANCH)
    }

    /// The branch and commit together, as the log and bug reports name the build (R130).
    #[func]
    fn build() -> GString {
        GString::from(&build_name(BRANCH, COMMIT))
    }

    /// Where ORQWA's source code is.
    #[func]
    fn source_url() -> GString {
        GString::from(orqwa_app::about::SOURCE)
    }

    /// The licence notices of ORQWA and of everything built into the app, in the order to list
    /// them (R83): `[{part, text}]`, `part` one of `orqwa`, `godot`, `ffmpeg`, `crates` and
    /// `assets`.
    #[func]
    fn notices() -> VarArray {
        let mut array = VarArray::new();
        for notice in orqwa_app::about::notices(&engine_credits()) {
            array.push(&vdict! {
                "part" => notice.part.name(),
                "text" => notice.text.as_str(),
            });
        }
        array
    }
}

/// The commit the build script found (see `build.rs`).
const COMMIT: &str = env!("ORQWA_COMMIT");

/// The branch the build script found (see `build.rs`).
const BRANCH: &str = env!("ORQWA_BRANCH");

/// `main@90b74ef`; the commit alone without a branch, and nothing without a commit, as a
/// branch alone does not tell which build it is.
fn build_name(branch: &str, commit: &str) -> String {
    match (branch, commit) {
        (_, "") => String::new(),
        ("", commit) => commit.to_owned(),
        (branch, commit) => format!("{branch}@{commit}"),
    }
}

/// The course a new library starts with (R95), with its terrain and map so that it rides
/// offline. Here rather than in the core, which the command-line tool shares: only the app
/// ships it. See `courses/README.md`.
const SAMPLE_COURSE: &[u8] = include_bytes!("../courses/twann-chasseral.tqc");
const SAMPLE_COURSE_FILE: &str = "twann-chasseral.tqc";

/// What the running engine reports of its licence and of the components it is built from.
fn engine_credits() -> orqwa_app::about::EngineCredits {
    let engine = Engine::singleton();
    let text = |value: Option<Variant>| value.map(|v| v.to_string()).unwrap_or_default();
    let mut components = Vec::new();
    for component in engine.get_copyright_info().iter_shared() {
        let name = text(component.get("name"));
        let parts = component
            .get("parts")
            .and_then(|v| v.try_to::<VarArray>().ok());
        for part in parts.iter().flat_map(VarArray::iter_shared) {
            let Ok(part) = part.try_to::<VarDictionary>() else {
                continue;
            };
            let copyright = part
                .get("copyright")
                .and_then(|v| v.try_to::<VarArray>().ok());
            components.push(orqwa_app::about::Component {
                name: name.clone(),
                copyright: copyright
                    .iter()
                    .flat_map(VarArray::iter_shared)
                    .map(|holder| holder.to_string())
                    .collect(),
                licence: text(part.get("license")),
            });
        }
    }
    orqwa_app::about::EngineCredits {
        licence: engine.get_license_text().to_string(),
        components,
        licences: engine
            .get_license_info()
            .iter_shared()
            .map(|(name, licence)| (name.to_string(), licence.to_string()))
            .collect(),
    }
}

/// The ORQWA application as a node: add it to the scene tree, call its commands and listen to
/// its signals. It advances rides in `_process`.
#[derive(GodotClass)]
#[class(base = Node)]
pub struct OrqwaApp {
    base: Base<Node>,
    app: Option<App>,
    last_failure: VarDictionary,
    startup_failure: Option<Failure>,
}

#[godot_api]
impl INode for OrqwaApp {
    fn init(base: Base<Node>) -> Self {
        // The editor instantiates nodes too; it must not start runtimes or touch Bluetooth.
        let mut startup_failure = None;
        let app = if Engine::singleton().is_editor_hint() {
            None
        } else {
            // Set once per process; a second node keeps the first one's.
            let _ = tracing::subscriber::set_global_default(log::StderrLog);
            App::new(paths::data_dir(), paths::cache_dir())
                .inspect(|app| {
                    if let Err(error) = app.add_sample_course(SAMPLE_COURSE_FILE, SAMPLE_COURSE) {
                        tracing::warn!(%error, "cannot add the sample course");
                    }
                })
                .inspect_err(|error| {
                    godot_error!("ORQWA: {error}");
                    startup_failure = Some(Failure::new(Kind::Runtime, error));
                })
                .ok()
        };
        Self {
            base,
            app,
            last_failure: VarDictionary::new(),
            startup_failure,
        }
    }

    fn ready(&mut self) {
        // Courses from before ORQWA knew countries, or prepared offline, learn them (#230).
        if let Some(app) = self.app.as_mut() {
            app.fill_in_countries(false);
            // Courses deleted on another computer, or an update, may leave kept worlds no
            // course needs (R101).
            app.tidy_kept_worlds();
        }
    }

    fn process(&mut self, delta: f64) {
        if let Some(failure) = self.startup_failure.take() {
            self.report_failure(&failure);
        }
        let Some(app) = self.app.as_mut() else {
            return;
        };
        let events = app.update(Duration::from_secs_f64(delta.max(0.0)));
        // A course imported from a file, or prepared offline, may not know its countries yet.
        if events.iter().any(|e| matches!(e, AppEvent::CourseAdded(_))) {
            app.fill_in_countries(false);
        }
        for event in events {
            self.emit(event);
        }
    }

    fn exit_tree(&mut self) {
        if let Some(app) = self.app.as_mut() {
            app.shutdown();
        }
    }
}

#[godot_api]
impl OrqwaApp {
    /// A scan finished: an array of dictionaries `{index, name, kind ("trainer",
    /// "power_meter", "cadence_sensor", "heart_rate" or "controller"), model (a controller's
    /// "di2" or "zwift_click", else ""), rssi, remembered}`.
    #[signal]
    fn devices_found(devices: VarArray);

    /// Preparing a course advanced: what is being done, the unit counted, done and total.
    #[signal]
    fn loading_progress(step: GString, unit: GString, done: i64, total: i64);

    /// A route was imported: `{name, length_m, elevation_gain_m, max_grade, elevation_source}`.
    #[signal]
    fn route_loaded(route: VarDictionary);

    /// The 3D world for the loaded route is ready: `{chunks, fallback_samples}`.
    #[signal]
    fn world_ready(world: VarDictionary);

    /// A device connected (also after reconnecting).
    #[signal]
    fn device_connected(name: GString);

    /// A device lost its connection and is reconnecting.
    #[signal]
    fn device_disconnected(name: GString);

    /// An attempt to connect `name` failed for `reason` (R80); its driver tries again.
    #[signal]
    fn device_failed(name: GString, reason: GString);

    /// The rider reached the finish.
    #[signal]
    fn ride_finished();

    /// Something worth telling the rider of happened on the ride (R124), as `moment_dict`
    /// describes it.
    #[signal]
    fn moment(info: VarDictionary);

    /// The ride was saved as a FIT file.
    #[signal]
    fn ride_saved(path: GString);

    /// A course was saved or imported into the library.
    #[signal]
    fn course_added(path: GString);

    /// Courses in the library learned their countries (#230): the gallery is to be shown
    /// again.
    #[signal]
    fn courses_changed();

    /// Devices used last that the reconnect at start did not find, by name.
    #[signal]
    fn remembered_missing(names: PackedStringArray);

    /// The active rider changed: another one selected, one saved or created, or the active
    /// one deleted.
    #[signal]
    fn profile_changed();

    /// Something went wrong.
    #[signal]
    fn failed(message: GString);

    /// Structured failure history: message key, arguments and original diagnostics.
    #[signal]
    fn failure_reported(failure: VarDictionary);

    /// Most recently reported failure, for inline expandable diagnostic controls.
    #[func]
    fn last_failure(&self) -> VarDictionary {
        self.last_failure.clone()
    }

    /// Rider `rider`'s account on `platform` is connected, as `label` (R29).
    #[signal]
    fn account_connected(rider: GString, platform: GString, label: GString);

    /// Connecting rider `rider`'s account on `platform` failed, for the reason in `message`.
    #[signal]
    fn account_failed(rider: GString, platform: GString, message: GString);

    /// A page to show in the rider's browser, e.g. a platform's login while connecting.
    #[signal]
    fn open_url(url: GString);

    /// ORQWA `version` was released, its notes at `page` (R99); `installable` when
    /// `download_update` and `install_update` can put it in place, else the rider gets the page.
    #[signal]
    fn update_available(version: GString, page: GString, installable: bool);

    /// The rider asked, and ORQWA is the newest version.
    #[signal]
    fn up_to_date();

    /// The update is downloaded: `install_update` installs it.
    #[signal]
    fn update_downloaded();

    /// Looking for or downloading an update failed, for the reason in `message`.
    #[signal]
    fn update_failed(message: GString);

    /// The ride in `path` is on `platform`, its page at `url` (R29).
    #[signal]
    fn upload_finished(path: GString, platform: GString, url: GString);

    /// Uploading the ride in `path` to `platform` failed, for the reason in `message`;
    /// `reconnect` if the platform no longer accepts the account.
    #[signal]
    fn upload_failed(path: GString, platform: GString, message: GString, reconnect: bool);

    /// The workouts planned for `day` on `platform` were read (R81): `workouts()` lists them.
    #[signal]
    fn planned_workouts(platform: GString, day: GString);

    /// Reading the workouts planned on `platform` failed, for the reason in `message`.
    #[signal]
    fn planned_workouts_failed(platform: GString, message: GString);

    /// Asks the platforms holding the rider's plan for the workouts of `day` (`YYYY-MM-DD`,
    /// this computer's date) in the background (R81): `planned_workouts` or
    /// `planned_workouts_failed` follows per platform. False, and nothing asked, without an
    /// account on such a platform.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn fetch_planned_workouts(&mut self, day: GString) -> bool {
        self.app
            .as_mut()
            .is_some_and(|app| app.fetch_planned_workouts(&day.to_string()))
    }

    /// A shifter's button asked for a ride control (#139), to be carried out as its key is:
    /// `next_camera`, `overlay`, the music's `play_pause`, `next` and `previous`, `pause`,
    /// `difficulty_up` and `_down`, the workout's `intensity_up` and `_down`, `skip_step`, or
    /// `finish_ride`.
    #[signal]
    fn control_requested(control: GString);

    /// The remote asked for a step through the screens (#228), with or without a ride: `up`,
    /// `down`, `left`, `right`, `ok` (press what has the focus) or `back`.
    #[signal]
    fn navigation_requested(step: GString);

    /// Reconnects the devices used last, in the background (emits
    /// `devices_found`, `device_connected` and, for those not found, `remembered_missing`).
    /// False if none is remembered.
    #[func]
    fn reconnect_remembered(&mut self) -> bool {
        self.app.as_mut().is_some_and(App::reconnect_remembered)
    }

    /// Reconnects the devices not connected now, e.g. during a ride (R69), as
    /// `reconnect_remembered` does at start; connected devices keep their link. False if none
    /// is remembered.
    #[func]
    fn reconnect_devices(&mut self) -> bool {
        self.app.as_mut().is_some_and(App::reconnect_devices)
    }

    /// The trainer, sensors and shifter in use or used last, as
    /// `[{name, kind, state, connected, failure}]` with `kind` as in `devices_found`, `state`
    /// `connected`, `connecting` (chosen or remembered, on its way or reconnecting) or
    /// `missing` (used last, not found by the scan) (R69, R76), and `failure` why the last
    /// attempt to connect failed while the driver tries again, else "" (R80).
    #[func]
    fn devices(&self) -> VarArray {
        let mut array = VarArray::new();
        for device in self.app.as_ref().map(App::devices).unwrap_or_default() {
            array.push(&vdict! {
                "name" => device.name.as_str(),
                "kind" => kind_name(device.kind),
                "state" => device.state.name(),
                "connected" => device.state == orqwa_app::DeviceState::Connected,
                "failure" => device.failure.as_deref().unwrap_or(""),
            });
        }
        array
    }

    /// What the device of `kind` (as in `devices_found`) reported in the last seconds, also
    /// between rides (R80): `{power, cadence, heart_rate, speed_kmh}`, each null when the
    /// device does not report it, and how full its batteries are (R97): `battery` in percent
    /// for most devices, `battery_left` and `battery_right` for a pair such as the Zwift Click
    /// v2's pods, null while untold. Empty when nothing came or the kind is unknown.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn device_reading(&self, kind: GString) -> VarDictionary {
        let (Some(kind), Some(app)) = (kind_of(&kind.to_string()), self.app.as_ref()) else {
            return VarDictionary::new();
        };
        let (reading, battery) = match (app.device_reading(kind), app.device_battery(kind)) {
            (None, None) => return VarDictionary::new(),
            (reading, battery) => (reading.unwrap_or_default(), battery.unwrap_or_default()),
        };
        let optional = |value: Option<f64>| value.map_or_else(Variant::nil, |v| v.to_variant());
        let percent = |level: Option<u8>| level.map_or_else(Variant::nil, |l| l.to_variant());
        vdict! {
            "power" => &optional(reading.power.map(|p| p.0)),
            "cadence" => &optional(reading.cadence.map(|c| c.0)),
            "heart_rate" => &optional(reading.heart_rate.map(|h| h.0)),
            "speed_kmh" => &optional(reading.speed.map(MetersPerSecond::as_kilometers_per_hour)),
            "battery" => &percent(battery.level),
            "battery_left" => &percent(battery.left),
            "battery_right" => &percent(battery.right),
        }
    }

    /// Scans for trainers, power meters, cadence and heart-rate sensors and shifters.
    #[func]
    fn scan(&mut self, seconds: f64) {
        if let Some(app) = self.app.as_mut() {
            app.scan(Duration::from_secs_f64(seconds.max(1.0)));
        }
    }

    /// Imports a GPX file; `offline` uses cached terrain only.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn load_route(&mut self, path: GString, offline: bool) {
        if let Some(app) = self.app.as_mut() {
            app.load_route(PathBuf::from(path.to_string()), offline);
        }
    }

    /// Prepares a video course (R17) from a GoPro video with GPS or an Incyclist route video's
    /// `.xml` file; emits `route_loaded`, `world_ready` and `course_added` like `load_route`.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn load_video(&mut self, path: GString, offline: bool) {
        if let Some(app) = self.app.as_mut() {
            app.load_video(PathBuf::from(path.to_string()), offline);
        }
    }

    /// Stops preparing the route, video or world being loaded (#219); nothing more is emitted
    /// for it.
    #[func]
    fn cancel_loading(&mut self) {
        if let Some(app) = self.app.as_mut() {
            app.cancel_loading();
        }
    }

    /// File extensions `load_video` accepts, for file dialogs: videos, Incyclist's `xml` and
    /// Tacx's `rlv`.
    #[func]
    fn video_extensions() -> PackedStringArray {
        orqwa_app::video::VIDEO_EXTENSIONS
            .iter()
            .chain(&["xml", "rlv"])
            .map(|e| GString::from(*e))
            .collect()
    }

    /// The loaded video course: `{path, duration_s, offset_s, aligned_by_hand, located, marks}`;
    /// `located` is false for courses without a place (Tacx RLV), ridden along the video only;
    /// `marks` (x metres along the route, y seconds into the video) are those of a video
    /// placed on the route by hand, from the route's start to its end. Empty for other courses.
    #[func]
    fn video(&self) -> VarDictionary {
        let Some(video) = self.app.as_ref().and_then(App::video) else {
            return VarDictionary::new();
        };
        let path = video.video.display().to_string();
        vdict! {
            "path" => path.as_str(),
            "duration_s" => video.duration.as_secs_f64(),
            "offset_s" => video.offset.as_secs_f64(),
            "aligned_by_hand" => video.aligned_by_hand(),
            "located" => video.located,
            "marks" => &video
                .marks
                .iter()
                .map(|m| vector2(m.distance.0, m.time.as_secs_f64()))
                .collect::<PackedVector2Array>(),
        }
    }

    /// A video about to be imported: `{duration_s, has_gps}`; empty (and `failed`) if it
    /// cannot be read. Without GPS it is added to a GPX course with `add_video`.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn video_probe(&mut self, path: GString) -> VarDictionary {
        match orqwa_app::video::probe(&PathBuf::from(path.to_string())) {
            Ok(probe) => vdict! {
                "duration_s" => probe.duration.as_secs_f64(),
                "has_gps" => probe.has_gps,
            },
            Err(message) => {
                self.report_failure(&message);
                VarDictionary::new()
            }
        }
    }

    /// Adds a video (e.g. one without GPS) to the loaded GPX course, placed by `marks` (x
    /// metres along the route, y seconds into the video, from the route's start to its end).
    /// The course becomes a video course; emits `failed` if that is not possible.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn add_video(&mut self, video: GString, marks: PackedVector2Array) -> bool {
        let video = PathBuf::from(video.to_string());
        let marks = sync_marks(&marks);
        self.command(|app| app.add_video(&video, &marks))
    }

    /// Takes the video off the loaded course, which is ridden in 3D only from then on.
    #[func]
    fn remove_video(&mut self) -> bool {
        self.command(App::remove_video)
    }

    /// How the next ride on a video course is shown: along its video, or in 3D (then call
    /// `build_world` first). Courses without a video are always ridden in 3D.
    #[func]
    fn ride_along_video(&mut self, along: bool) {
        if let Some(app) = self.app.as_mut() {
            app.ride_along_video(along);
        }
    }

    /// Whether the current ride plays the course's video.
    #[func]
    fn riding_along_video(&self) -> bool {
        self.app.as_ref().is_some_and(App::riding_along_video)
    }

    /// Replaces the marks of the loaded video course (see `add_video`); emits `failed` if they
    /// do not fit.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn align_video(&mut self, marks: PackedVector2Array) -> bool {
        let marks = sync_marks(&marks);
        self.command(|app| app.align_video(&marks))
    }

    /// The frame of the video at `path` shown `time_s` seconds in, e.g. to align it; `null`
    /// if it cannot be decoded. Call `close_video_preview` when done.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn video_preview(&mut self, path: GString, time_s: f64) -> Option<Gd<Image>> {
        let frame = self
            .app
            .as_mut()?
            .video_preview(&PathBuf::from(path.to_string()), seconds(time_s))
            .ok()?;
        image(frame)
    }

    /// Closes the video opened by `video_preview`.
    #[func]
    fn close_video_preview(&mut self) {
        if let Some(app) = self.app.as_mut() {
            app.close_video_preview();
        }
    }

    /// The moment of the video to show now, in seconds; -1 when not riding a video course.
    #[func]
    fn video_time(&self) -> f64 {
        self.app
            .as_ref()
            .and_then(App::video_time)
            .map_or(-1.0, |t| t.as_secs_f64())
    }

    /// The next video frame during a ride on a video course: `{time_s, image}` (an RGBA8
    /// `Image`); empty while the one before still lies ahead of `video_time()`. It is the frame
    /// following `video_time()`, decoded ahead, to blend towards from the previous one.
    #[func]
    fn video_frame(&mut self) -> VarDictionary {
        let Some(frame) = self.app.as_mut().and_then(App::video_frame) else {
            return VarDictionary::new();
        };
        let time_s = frame.time.as_secs_f64();
        let Some(image) = image(frame) else {
            return VarDictionary::new();
        };
        vdict! {
            "time_s" => time_s,
            "image" => &image,
        }
    }

    /// The next samples of the video's sound during a ride on a video course, at most `max`,
    /// as stereo frames (x left, y right) for an `AudioStreamGenerator` at
    /// `video_sound_rate()`; empty without sound.
    #[func]
    fn video_sound(&mut self, max: i64) -> PackedVector2Array {
        let max = usize::try_from(max).unwrap_or(0);
        self.app
            .as_mut()
            .map(|app| app.video_sound(max))
            .unwrap_or_default()
            .into_iter()
            .map(|[left, right]| Vector2::new(left, right))
            .collect()
    }

    /// Samples per second of `video_sound`; 0 while riding without the video's sound.
    #[func]
    fn video_sound_rate(&self) -> i64 {
        self.app
            .as_ref()
            .and_then(App::video_sound_rate)
            .map_or(0, i64::from)
    }

    /// Plays the video's sound during this ride or not (R26).
    #[func]
    fn set_video_sound(&mut self, on: bool) {
        if let Some(app) = self.app.as_mut() {
            app.set_video_sound(on);
        }
    }

    /// Samples per second of the ambient stream (R114).
    #[func]
    fn ambient_sound_rate() -> i64 {
        i64::from(orqwa_app::ambient::RATE)
    }

    /// Whether the 3D course ride's ambient stream can play now.
    #[func]
    fn ambient_sound_active(&self) -> bool {
        self.app.as_ref().is_some_and(App::ambient_sound_active)
    }

    /// Fade in after opening a new ambient output queue.
    #[func]
    fn restart_ambient_sound(&mut self) {
        if let Some(app) = self.app.as_mut() {
            app.restart_ambient_sound();
        }
    }

    /// Next stereo ambient frames (x left, y right); empty outside an active 3D ride.
    #[func]
    fn ambient_sound(&mut self, frames: i64) -> PackedVector2Array {
        self.app
            .as_mut()
            .map(|app| app.ambient_sound(usize::try_from(frames).unwrap_or(0)))
            .unwrap_or_default()
            .into_iter()
            .map(|[left, right]| Vector2::new(left, right))
            .collect()
    }

    /// Wind and bicycle volumes in percent; weather names match the world presets.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // Godot passes strings by value
    fn set_ambient_sound(&mut self, wind: f64, bike: f64, weather: GString) {
        use orqwa_app::ambient::{Settings, Weather};
        let weather = match weather.to_string().as_str() {
            "Cloudy" => Weather::Cloudy,
            "Hazy" => Weather::Hazy,
            "Rain" => Weather::Rain,
            _ => Weather::Clear,
        };
        if let Some(app) = self.app.as_mut() {
            app.set_ambient_sound(Settings {
                wind,
                bike,
                weather,
            });
        }
    }

    /// Opens a course file from the library or elsewhere; emits `route_loaded` like
    /// `load_route`. Its 3D world is built by `build_world`.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn open_course(&mut self, path: GString) {
        if let Some(app) = self.app.as_mut() {
            app.open_course(PathBuf::from(path.to_string()));
        }
    }

    /// Builds the opened course's 3D world, for riding it (emits `world_ready`); true if it is
    /// ready already.
    #[func]
    fn build_world(&mut self) -> bool {
        self.app.as_mut().is_some_and(App::build_world)
    }

    /// Builds the loaded course's world again with this ORQWA, in place of the one kept from an
    /// older one (R101); emits `world_ready` like `build_world`. False if no library course is
    /// loaded.
    #[func]
    fn regenerate_world(&mut self) -> bool {
        self.app.as_mut().is_some_and(App::regenerate_world)
    }

    /// Whether the course at `path` has a world kept that an older ORQWA built, which
    /// `regenerate_world` builds again (R101).
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn kept_world_outdated(&self, path: GString) -> bool {
        self.app
            .as_ref()
            .is_some_and(|app| app.kept_world_outdated(Path::new(&path.to_string())))
    }

    /// The course file of the loaded route; empty if it is not one (yet).
    #[func]
    fn loaded_course(&self) -> GString {
        self.app
            .as_ref()
            .and_then(App::loaded_course)
            .map_or_else(GString::new, |p| {
                GString::from(p.display().to_string().as_str())
            })
    }

    /// Whether a route is loaded (its figures, climbs and records are available).
    #[func]
    fn has_route(&self) -> bool {
        self.app.as_ref().is_some_and(|app| app.route().is_some())
    }

    /// Renames a course in the library; emits `failed` on errors (e.g. a blank name).
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn rename_course(&mut self, path: GString, name: GString) -> bool {
        let path = PathBuf::from(path.to_string());
        let name = name.to_string();
        self.command(|app| app.rename_course(&path, &name))
    }

    /// Deletes a course from the library; rides on it stay in the history.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn delete_course(&mut self, path: GString) -> bool {
        let path = PathBuf::from(path.to_string());
        self.command(|app| app.delete_course(&path))
    }

    /// Saves the loaded route as a course in the library (emits `course_added` or `failed`).
    /// False until the world is ready.
    #[func]
    fn save_course(&mut self) -> bool {
        self.command(App::save_course)
    }

    /// Names the course the next import adds (`load_route`, `load_video`, `import_course`):
    /// `name`, replacing the course of that name with `replace`, else kept next to it.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn name_next_import(&mut self, name: GString, replace: bool) {
        if let Some(app) = self.app.as_mut() {
            app.name_next_import(&name.to_string(), replace);
        }
    }

    /// Whether the library has a course named `name` (ignoring case).
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn course_exists(&self, name: GString) -> bool {
        self.app
            .as_ref()
            .is_some_and(|app| app.course_named(&name.to_string()).is_some())
    }

    /// A name for the course imported from `path`, for the rider to confirm.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn suggested_course_name(path: GString) -> GString {
        GString::from(App::suggested_course_name(&PathBuf::from(path.to_string())).as_str())
    }

    /// Copies a course file into the library (emits `course_added` or `failed`).
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn import_course(&mut self, path: GString) {
        if let Some(app) = self.app.as_mut() {
            app.import_course(PathBuf::from(path.to_string()));
        }
    }

    /// The courses in the library: `[{path, name, length_m, elevation_gain_m, max_grade,
    /// created_unix_s, track, profile, video, located}]`; `track` (metres east/north of the
    /// start) and `profile` (distance, elevation) are thinned for cards and empty for older
    /// courses; `video` is the video's file name for video courses, else empty; `located` is
    /// false for courses known only by distance and slope (Tacx RLV); `countries` are those
    /// the route passes as `[{code, name}]`, named in the interface language (#230), empty
    /// where not known.
    #[func]
    fn courses(&self) -> VarArray {
        let locale = locale();
        let mut array = VarArray::new();
        for course in self.app.as_ref().map(App::courses).unwrap_or_default() {
            array.push(&course_dict(&course, &locale).to_variant());
        }
        array
    }

    /// The library as the Courses tab shows it (R67), for `query` `{search, sort, descending,
    /// group, kinds, terrains, lengths, countries}`: the names of `course_library_choices()`,
    /// the chips as `PackedStringArray`s (empty for all), countries by code or `none` for
    /// those not known. Returns `{total, groups: [{group, label, courses}], countries}`: how
    /// many courses the library holds; the groups shown, `group` the name of their kind,
    /// terrain or length band, or a country's code or `none` (empty when not grouped) and
    /// `label` a country's name; and the countries the library's courses pass as `[{code,
    /// name}]` (#230). Countries are named and searched in the interface language. Courses are
    /// as in `courses()`, with `estimated_s` at the planned share of FTP (R66, from the card's
    /// profile; `null` out of reach), `last_ridden_unix_s` (`null` if never) and `terrain`.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn course_library(&mut self, query: VarDictionary) -> VarDictionary {
        let text = |key: &str| {
            query
                .get(key)
                .and_then(|v| v.try_to::<GString>().ok())
                .map(|s| s.to_string())
                .unwrap_or_default()
        };
        let chips = |key: &str| -> Vec<String> {
            query
                .get(key)
                .and_then(|v| v.try_to::<PackedStringArray>().ok())
                .map(|names| names.as_slice().iter().map(ToString::to_string).collect())
                .unwrap_or_default()
        };
        let query = library::CourseQuery {
            view: library::View {
                sort: library::SortKey::from_name(&text("sort")).unwrap_or_default(),
                descending: query
                    .get("descending")
                    .and_then(|v| v.try_to::<bool>().ok())
                    .unwrap_or(false),
                group: library::GroupKey::from_name(&text("group")).unwrap_or_default(),
            },
            search: text("search"),
            kinds: chips("kinds")
                .iter()
                .filter_map(|n| library::CourseKind::from_name(n))
                .collect(),
            terrains: chips("terrains")
                .iter()
                .filter_map(|n| library::Terrain::from_name(n))
                .collect(),
            lengths: chips("lengths")
                .iter()
                .filter_map(|n| library::LengthBand::from_name(n))
                .collect(),
            countries: chips("countries"),
            locale: locale(),
        };
        let gallery = self
            .app
            .as_mut()
            .map(|app| app.course_library(&query))
            .unwrap_or_default();
        let mut groups = VarArray::new();
        for group in &gallery.groups {
            let mut courses = VarArray::new();
            for course in &group.courses {
                let mut dict = course_dict(&course.entry, &query.locale);
                let estimated = course
                    .estimated
                    .map_or_else(Variant::nil, |t| t.as_secs_f64().to_variant());
                let ridden = course
                    .last_ridden
                    .map_or_else(Variant::nil, |t| unix_seconds(t).to_variant());
                dict.set("estimated_s", &estimated);
                dict.set("last_ridden_unix_s", &ridden);
                dict.set("terrain", course.terrain().name());
                courses.push(&dict.to_variant());
            }
            let label = if group.label == group.group.name() {
                // A country the map named only by its code.
                country_name_or_godots(&group.label)
            } else {
                group.label.clone()
            };
            groups.push(
                &vdict! {
                    "group" => group.group.name(),
                    "label" => label.as_str(),
                    "courses" => &courses,
                }
                .to_variant(),
            );
        }
        let mut countries = VarArray::new();
        for choice in &gallery.countries {
            let name = if choice.name == choice.code {
                country_name_or_godots(&choice.code)
            } else {
                choice.name.clone()
            };
            countries.push(
                &vdict! {
                    "code" => choice.code.as_str(),
                    "name" => name.as_str(),
                }
                .to_variant(),
            );
        }
        vdict! {
            "total" => i64::try_from(gallery.total).unwrap_or(i64::MAX),
            "groups" => &groups,
            "countries" => &countries,
        }
    }

    /// What the Courses tab offers (R67), in the order to list it: `sorts` as `[{name,
    /// descending}]` with the direction each starts in, `groups`, `kinds` and `terrains` as
    /// names, and `lengths` as `[{name, from_m, to_m}]` with edges in the active rider's units
    /// (`to_m` 0 for the last, open band).
    #[func]
    fn course_library_choices(&self) -> VarDictionary {
        let mut sorts = VarArray::new();
        for &sort in library::SortKey::ALL {
            sorts.push(
                &vdict! {
                    "name" => sort.name(),
                    "descending" => sort.natural_descending(),
                }
                .to_variant(),
            );
        }
        let units = self
            .app
            .as_ref()
            .map_or(UnitSystem::Metric, |app| app.profile().profile.units);
        let edges = library::LengthBand::edges(units);
        let mut lengths = VarArray::new();
        for (index, &band) in library::LengthBand::ALL.iter().enumerate() {
            let from = index.checked_sub(1).map_or(0.0, |i| edges[i].0);
            let to = edges.get(index).map_or(0.0, |edge| edge.0);
            lengths.push(
                &vdict! {
                    "name" => band.name(),
                    "from_m" => from,
                    "to_m" => to,
                }
                .to_variant(),
            );
        }
        vdict! {
            "sorts" => &sorts,
            "groups" => &name_array(library::GroupKey::ALL.iter().map(|g| g.name())),
            "kinds" => &name_array(library::CourseKind::ALL.iter().map(|k| k.name())),
            "terrains" => &name_array(library::Terrain::ALL.iter().map(|t| t.name())),
            "lengths" => &lengths,
        }
    }

    /// How the Courses tab was left (R67): `{sort, descending, group}`.
    #[func]
    fn courses_view(&self) -> VarDictionary {
        let view = self.app.as_ref().map(App::courses_view).unwrap_or_default();
        vdict! {
            "sort" => view.sort.name(),
            "descending" => view.descending,
            "group" => view.group.name(),
        }
    }

    /// Remembers how the Courses tab orders and groups the library; names it does not know
    /// are taken as the defaults.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn set_courses_view(&mut self, sort: GString, descending: bool, group: GString) {
        let view = library::View {
            sort: library::SortKey::from_name(&sort.to_string()).unwrap_or_default(),
            descending,
            group: library::GroupKey::from_name(&group.to_string()).unwrap_or_default(),
        };
        self.command(|app| app.set_courses_view(view));
    }

    /// The active rider's rides, newest first: `[{path, route, name, start_unix_s, elapsed_s,
    /// distance_m, elevation_gain_m, avg_speed_kmh, avg_power, max_power, normalized_power,
    /// intensity_factor, training_stress, work_kj, avg_cadence, avg_heart_rate,
    /// max_heart_rate, route_time_s, route_record, ftp_estimate_w, climbs, power_curve,
    /// power_records, uploads}]`; values the ride did not record are `null`, and
    /// `ftp_estimate_w` for all but FTP tests (R22). `power_curve` holds `(duration s, W)`
    /// points of the ride's best power, `power_records` the durations in seconds over which it
    /// is the rider's best (R62). `uploads` has the page of the ride's activity by platform id
    /// (R29).
    #[func]
    fn history(&self) -> VarArray {
        let optional = |value: Option<f64>| value.map_or_else(Variant::nil, |v| v.to_variant());
        let mut array = VarArray::new();
        for entry in self.app.as_ref().map(App::history).unwrap_or_default() {
            let s = &entry.record.summary;
            let path = entry.fit.display().to_string();
            let start = unix_seconds(entry.record.start);
            array.push(
                &vdict! {
                    "path" => path.as_str(),
                    "route" => entry.record.route.as_str(),
                    "name" => entry.record.name.as_deref().unwrap_or_default(),
                    "start_unix_s" => start,
                    "elapsed_s" => s.elapsed.as_secs_f64(),
                    "distance_m" => s.distance.0,
                    "elevation_gain_m" => s.elevation_gain.0,
                    "avg_speed_kmh" => s.avg_speed.as_kilometers_per_hour(),
                    "avg_power" => &optional(s.avg_power.map(|v| v.0)),
                    "max_power" => &optional(s.max_power.map(|v| v.0)),
                    "normalized_power" => &optional(s.normalized_power.map(|v| v.0)),
                    "intensity_factor" => &optional(s.intensity_factor),
                    "training_stress" => &optional(s.training_stress),
                    "work_kj" => &optional(s.work.map(|w| w.0 / 1000.0)),
                    "avg_cadence" => &optional(s.avg_cadence.map(|v| v.0)),
                    "avg_heart_rate" => &optional(s.avg_heart_rate.map(|v| v.0)),
                    "max_heart_rate" => &optional(s.max_heart_rate.map(|v| v.0)),
                    "route_time_s" => &optional(entry.record.route_time.map(|t| t.as_secs_f64())),
                    "route_record" => entry.route_record,
                    "ftp_estimate_w" => &optional(entry.record.ftp_estimate.map(|w| w.0)),
                    "climbs" => &climb_times(&entry),
                    "power_curve" => &entry
                        .record
                        .analysis
                        .as_ref()
                        .map(|a| {
                            a.power_curve
                                .0
                                .iter()
                                .map(|(d, p)| vector2(d.as_secs_f64(), p.0))
                                .collect::<PackedVector2Array>()
                        })
                        .unwrap_or_default(),
                    "power_records" => &entry
                        .power_records
                        .iter()
                        .map(Duration::as_secs_f64)
                        .collect::<PackedFloat64Array>(),
                    "uploads" => &uploads_dict(&entry),
                }
                .to_variant(),
            );
        }
        array
    }

    /// What the summary of the ride saved at `path` says of it beyond its figures and records
    /// (R124): its first ride on the course, or how far into the rider's week it is, as the
    /// `moment` signal describes it; empty for neither. Local time is `utc_offset_minutes`
    /// ahead of UTC.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn ride_recap(&self, path: GString, utc_offset_minutes: i64) -> VarDictionary {
        self.app
            .as_ref()
            .and_then(|app| app.recap(Path::new(&path.to_string()), utc_offset_minutes * 60))
            .map_or_else(VarDictionary::new, |moment| moment_dict(&moment))
    }

    /// The active rider's power curves (R62): `{period, compare}`, each the best efforts per
    /// duration as `[{duration_s, power, watts_per_kg, start_unix_s, path}]`, shortest first.
    /// `period` holds those of the rides of the last `period_days` (all rides for 0), `compare`
    /// those of all rides or, with `previous`, of the period before. With `per_kg`, the best
    /// effort is the one with the most power per kilogram.
    #[func]
    fn power_curves(&self, period_days: i64, previous: bool, per_kg: bool) -> VarDictionary {
        let Some(app) = self.app.as_ref() else {
            return VarDictionary::new();
        };
        let period = period_of(period_days);
        let comparison = if previous {
            Comparison::PreviousPeriod
        } else {
            Comparison::AllTime
        };
        let curves = app.power_curves(period, comparison, per_kg);
        let efforts = |efforts: &[BestEffort]| {
            let mut array = VarArray::new();
            for effort in efforts {
                let path = effort.fit.display().to_string();
                array.push(
                    &vdict! {
                        "duration_s" => effort.duration.as_secs_f64(),
                        "power" => effort.power.0,
                        "watts_per_kg" => effort.watts_per_kg,
                        "start_unix_s" => unix_seconds(effort.start),
                        "path" => path.as_str(),
                    }
                    .to_variant(),
                );
            }
            array
        };
        vdict! {
            "period" => &efforts(&curves.period),
            "compare" => &efforts(&curves.compare),
        }
    }

    /// The active rider's power profile (R63): per duration of 5 s, 1, 5, 20 and 60 min,
    /// `{duration_s, period, best, share}` with the best of the rides of the last `period_days`
    /// (all rides for 0) and of all rides, in W or with `per_kg` in W/kg, and the first as a
    /// share of the second (0–1); values without rides are `null`.
    #[func]
    fn power_profile(&self, period_days: i64, per_kg: bool) -> VarArray {
        let Some(app) = self.app.as_ref() else {
            return VarArray::new();
        };
        let optional = |value: Option<f64>| value.map_or_else(Variant::nil, |v| v.to_variant());
        let value = |effort: &Option<BestEffort>| {
            optional(
                effort
                    .as_ref()
                    .map(|e| if per_kg { e.watts_per_kg } else { e.power.0 }),
            )
        };
        let mut array = VarArray::new();
        for axis in app.power_profile(period_of(period_days), per_kg) {
            array.push(
                &vdict! {
                    "duration_s" => axis.duration.as_secs_f64(),
                    "period" => &value(&axis.period),
                    "best" => &value(&axis.best),
                    "share" => &optional(axis.share),
                }
                .to_variant(),
            );
        }
        array
    }

    /// The active rider's fitness, fatigue and form (R64) for each day of the last
    /// `period_days` (since the first ride for 0), local time `utc_offset_minutes` ahead of
    /// UTC: `{day_unix_s, stress, fitness, fatigue, form}`, an array each with one entry a day.
    #[func]
    fn fitness(&self, period_days: i64, utc_offset_minutes: i64) -> VarDictionary {
        let Some(app) = self.app.as_ref() else {
            return VarDictionary::new();
        };
        let offset = utc_offset_minutes * 60;
        let days = app.fitness(period_of(period_days), offset);
        let series = |value: fn(&orqwa_session::fitness::Load) -> f64| {
            days.iter()
                .map(|d| value(&d.load))
                .collect::<PackedFloat64Array>()
        };
        vdict! {
            "day_unix_s" => &days
                .iter()
                .map(|d| orqwa_session::fitness::day_start(d.day, offset))
                .collect::<PackedInt64Array>(),
            "stress" => &series(|l| l.stress),
            "fitness" => &series(|l| l.fitness),
            "fatigue" => &series(|l| l.fatigue),
            "form" => &series(|l| l.form),
        }
    }

    /// The active rider's weeks (R64) of the last `period_days` (since the first ride for 0),
    /// local time `utc_offset_minutes` ahead of UTC: `[{start_unix_s, rides, time_s,
    /// distance_m, climbing_m, stress, power_zones, heart_rate_zones}]`, the zones as seconds
    /// in each of the rider's zones today.
    #[func]
    fn weekly(&self, period_days: i64, utc_offset_minutes: i64) -> VarArray {
        let Some(app) = self.app.as_ref() else {
            return VarArray::new();
        };
        let offset = utc_offset_minutes * 60;
        let seconds = |zones: &[Duration]| {
            zones
                .iter()
                .map(Duration::as_secs_f64)
                .collect::<PackedFloat64Array>()
        };
        let mut array = VarArray::new();
        for week in app.weekly(period_of(period_days), offset) {
            array.push(
                &vdict! {
                    "start_unix_s" => orqwa_session::fitness::day_start(week.start, offset),
                    "rides" => i64::from(week.rides),
                    "time_s" => week.time.as_secs_f64(),
                    "distance_m" => week.distance.0,
                    "climbing_m" => week.climbing.0,
                    "stress" => week.stress,
                    "power_zones" => &seconds(&week.power_zones),
                    "heart_rate_zones" => &seconds(&week.heart_rate_zones),
                }
                .to_variant(),
            );
        }
        array
    }

    /// The active rider's progress (R65) over the last `period_days` (since the first ride for
    /// 0), local time `utc_offset_minutes` ahead of UTC: `{monthly, start_unix_s, one_minute,
    /// five_minutes, twenty_minutes, ftp, test}`, one entry per week or month (months for
    /// periods over 13 weeks) in each array, in W or with `per_kg` in W/kg, -1 where a week or
    /// month has none.
    #[func]
    fn progress(&self, period_days: i64, utc_offset_minutes: i64, per_kg: bool) -> VarDictionary {
        let Some(app) = self.app.as_ref() else {
            return VarDictionary::new();
        };
        let offset = utc_offset_minutes * 60;
        let progress = app.progress(period_of(period_days), offset, per_kg);
        let series = |value: &dyn Fn(&orqwa_app::analysis::ProgressPoint) -> Option<f64>| {
            progress
                .points
                .iter()
                .map(|point| value(point).unwrap_or(-1.0))
                .collect::<PackedFloat64Array>()
        };
        vdict! {
            "monthly" => progress.monthly,
            "start_unix_s" => &progress
                .points
                .iter()
                .map(|point| orqwa_session::fitness::day_start(point.start, offset))
                .collect::<PackedInt64Array>(),
            "one_minute" => &series(&|point| point.bests[0]),
            "five_minutes" => &series(&|point| point.bests[1]),
            "twenty_minutes" => &series(&|point| point.bests[2]),
            "ftp" => &series(&|point| point.ftp),
            "test" => &series(&|point| point.test),
        }
    }

    /// The FTP the active rider's best efforts of the last 90 days show (R65): `{ftp_w,
    /// method, w_prime_kj, max_power_w}`, `method` `critical_power` or `twenty_minutes` (then
    /// without W′ and maximal power, `null`); empty without an effort of 20 minutes.
    #[func]
    fn ftp_estimate(&self) -> VarDictionary {
        use orqwa_session::ftp_estimate::Method;
        let Some(estimate) = self.app.as_ref().and_then(App::ftp_estimate) else {
            return VarDictionary::new();
        };
        let optional = |value: Option<f64>| value.map_or_else(Variant::nil, |v| v.to_variant());
        vdict! {
            "ftp_w" => estimate.ftp.0,
            "method" => match estimate.method {
                Method::CriticalPower => "critical_power",
                Method::TwentyMinutes => "twenty_minutes",
            },
            "w_prime_kj" => &optional(estimate.w_prime.map(|w| w.0 / 1000.0)),
            "max_power_w" => &optional(estimate.max_power.map(|p| p.0)),
        }
    }

    /// Charts of one ride as `(elapsed_s, value)` points — `power`, `heart_rate`, `cadence`,
    /// `speed_kmh`, `elevation_m` — and seconds per zone of the active rider in `power_zones`
    /// (7) and `heart_rate_zones` (5). Empty (and emits `failed`) if the file cannot be read.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn ride_detail(&mut self, path: GString, max_points: i64) -> VarDictionary {
        let Some(app) = self.app.as_ref() else {
            return VarDictionary::new();
        };
        let detail = match app.ride_detail(std::path::Path::new(&path.to_string())) {
            Ok(detail) => detail,
            Err(error) => {
                self.report_failure(&Failure::from(error));
                return VarDictionary::new();
            }
        };
        let max_points = usize::try_from(max_points).unwrap_or(500);
        let series = |value: fn(&orqwa_domain::recording::Sample) -> Option<f64>| {
            view::ride_series(&detail.samples, max_points, value)
                .into_iter()
                .map(|(a, b)| vector2(a, b))
                .collect::<PackedVector2Array>()
        };
        let seconds = |zones: &[Duration]| {
            zones
                .iter()
                .map(Duration::as_secs_f64)
                .collect::<PackedFloat64Array>()
        };
        vdict! {
            "power" => &series(|s| s.power.map(|p| p.0)),
            "heart_rate" => &series(|s| s.heart_rate.map(|h| h.0)),
            "cadence" => &series(|s| s.cadence.map(|c| c.0)),
            "speed_kmh" => &series(|s| Some(s.speed.as_kilometers_per_hour())),
            "elevation_m" => &series(|s| s.location.map(|l| l.elevation.0)),
            "power_zones" => &seconds(&detail.power_zones),
            "heart_rate_zones" => &seconds(&detail.heart_rate_zones),
        }
    }

    /// Names a ride (empty: back to route and date); emits `failed` on errors.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn rename_ride(&mut self, path: GString, name: GString) -> bool {
        let path = PathBuf::from(path.to_string());
        let name = name.to_string();
        self.command(|app| app.rename_ride(&path, &name))
    }

    /// Deletes a ride from the history.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn delete_ride(&mut self, path: GString) -> bool {
        let path = PathBuf::from(path.to_string());
        self.command(|app| app.delete_ride(&path))
    }

    /// Saves a copy of a ride's FIT file at `to`; emits `failed` on errors.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn export_ride(&mut self, path: GString, to: GString) -> bool {
        let path = PathBuf::from(path.to_string());
        let to = PathBuf::from(to.to_string());
        self.command(|app| app.export_ride(&path, &to))
    }

    /// Every platform rides can be uploaded to, with rider `rider`'s account on it (R29):
    /// `[{platform, name, fields, help, help_url, label, auto_upload}]` in the order offered,
    /// `fields` as `[{key, label, secret}]` to ask for when connecting (labels and help in
    /// English, to translate), `label` who is connected, empty if nobody, `auto_upload` whether
    /// kept rides go up without asking (R60).
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn accounts(&self, rider: GString) -> VarArray {
        let mut array = VarArray::new();
        let Some(app) = self.app.as_ref() else {
            return array;
        };
        for account in app.accounts(&rider.to_string()) {
            let mut fields = VarArray::new();
            for field in account.fields {
                fields.push(
                    &vdict! {
                        "key" => field.key,
                        "label" => field.label,
                        "secret" => field.secret,
                    }
                    .to_variant(),
                );
            }
            array.push(
                &vdict! {
                    "platform" => account.platform,
                    "name" => account.name,
                    "fields" => &fields,
                    "help" => account.help,
                    "help_url" => account.help_url,
                    "label" => account.label.as_deref().unwrap_or_default(),
                    "auto_upload" => account.auto_upload,
                }
                .to_variant(),
            );
        }
        array
    }

    /// Connects rider `rider`'s account on `platform` with the `entered` fields (by key), in
    /// the background: emits `account_connected` or `account_failed`. Platforms the rider logs
    /// in to emit `open_url` with their login first; the page the browser then comes back to
    /// says `connected_text` or `failed_text`.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn connect_account(
        &mut self,
        rider: GString,
        platform: GString,
        entered: VarDictionary,
        connected_text: GString,
        failed_text: GString,
    ) -> bool {
        let entered = entered
            .iter_shared()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        let pages = orqwa_app::uploads::LoginPages {
            connected: connected_text.to_string(),
            failed: failed_text.to_string(),
        };
        self.command(|app| {
            app.connect_account(&rider.to_string(), &platform.to_string(), entered, pages)
        })
    }

    /// Gives up on the account being connected, e.g. a login no longer to wait for.
    #[func]
    fn cancel_connect(&mut self) {
        if let Some(app) = self.app.as_mut() {
            app.cancel_connect();
        }
    }

    /// Forgets rider `rider`'s account on `platform`.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn disconnect_account(&mut self, rider: GString, platform: GString) -> bool {
        self.command(|app| app.disconnect_account(&rider.to_string(), &platform.to_string()))
    }

    /// Whether rides rider `rider` keeps go up to `platform` without asking (R60); emits
    /// `failed` if they have no account there.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn set_auto_upload(&mut self, rider: GString, platform: GString, on: bool) -> bool {
        self.command(|app| app.set_auto_upload(&rider.to_string(), &platform.to_string(), on))
    }

    /// The rider keeps the ride in `path` (Done on its summary): it goes up as `title` to their
    /// accounts set to upload automatically (R60), in the background like `upload_ride`.
    /// Returns those platforms.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn keep_ride(&mut self, path: GString, title: GString) -> PackedStringArray {
        let path = PathBuf::from(path.to_string());
        self.app
            .as_mut()
            .map(|app| app.keep_ride(&path, &title.to_string()))
            .unwrap_or_default()
            .into_iter()
            .map(GString::from)
            .collect()
    }

    /// The platforms the ride in `path` is on its way to.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn uploading(&self, path: GString) -> PackedStringArray {
        let path = PathBuf::from(path.to_string());
        self.app
            .as_ref()
            .map(|app| app.uploading(&path))
            .unwrap_or_default()
            .into_iter()
            .map(GString::from)
            .collect()
    }

    /// Uploads the active rider's ride in `path` to `platform` as `title`, in the background:
    /// emits `upload_finished` or `upload_failed` (and `failed` if it cannot start).
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn upload_ride(&mut self, path: GString, platform: GString, title: GString) -> bool {
        let path = PathBuf::from(path.to_string());
        self.command(|app| app.upload_ride(&path, &platform.to_string(), &title.to_string()))
    }

    /// The file name to suggest when exporting the ride titled `title`.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn ride_export_file_name(title: GString) -> GString {
        GString::from(App::ride_export_file_name(&title.to_string()).as_str())
    }

    /// The graphics presets, lightest first: `low`, `medium`, `high`, `ultra` (R43).
    #[func]
    fn graphics_qualities() -> PackedStringArray {
        orqwa_app::GraphicsQuality::ALL
            .iter()
            .map(|q| GString::from(q.name()))
            .collect()
    }

    /// The graphics preset chosen on this computer.
    #[func]
    fn graphics_quality(&self) -> GString {
        let quality = self
            .app
            .as_ref()
            .map(App::graphics_quality)
            .unwrap_or_default();
        GString::from(quality.name())
    }

    /// Chooses the graphics preset for this computer; false for unknown names.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn set_graphics_quality(&mut self, name: GString) -> bool {
        let Some(quality) = orqwa_app::GraphicsQuality::from_name(&name.to_string()) else {
            return false;
        };
        self.command(|app| app.set_graphics_quality(quality))
    }

    /// The graphics options a rider may set apart from the preset (R90): option name → the
    /// values it takes, in order (without "auto", which every option also takes).
    #[func]
    fn graphics_option_values() -> VarDictionary {
        let mut options = VarDictionary::new();
        for (name, values) in orqwa_app::GRAPHICS_OPTIONS {
            let list: PackedStringArray = values.iter().map(|v| GString::from(*v)).collect();
            options.set(*name, &list);
        }
        options
    }

    /// Every graphics option's value on this computer (R90): "auto" for those following the
    /// preset.
    #[func]
    fn graphics_options(&self) -> VarDictionary {
        let set = self
            .app
            .as_ref()
            .map(App::graphics_options)
            .unwrap_or_default();
        let mut options = VarDictionary::new();
        for (name, _) in orqwa_app::GRAPHICS_OPTIONS {
            let value = set
                .get(*name)
                .map_or(orqwa_app::GRAPHICS_AUTO, String::as_str);
            options.set(*name, &GString::from(value));
        }
        options
    }

    /// Sets a graphics option on this computer (R90), "auto" to follow the preset; false for
    /// unknown options or values, or when it cannot be saved.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn set_graphics_option(&mut self, name: GString, value: GString) -> bool {
        let (name, value) = (name.to_string(), value.to_string());
        let mut known = false;
        let saved = self.command(|app| {
            known = app.set_graphics_option(&name, &value)?;
            Ok(())
        });
        saved && known
    }

    /// Where the overlay was last on screen (R55), in screen pixels; empty before its first use.
    #[func]
    fn overlay_window(&self) -> Rect2i {
        self.app
            .as_ref()
            .and_then(App::overlay_window)
            .map_or(Rect2i::default(), |w| {
                Rect2i::new(
                    Vector2i::new(w.x, w.y),
                    Vector2i::new(
                        i32::try_from(w.width).unwrap_or(i32::MAX),
                        i32::try_from(w.height).unwrap_or(i32::MAX),
                    ),
                )
            })
    }

    /// How large the overlay draws its figures (#124), 1 being one interface unit per point;
    /// 0 before it was first chosen.
    #[func]
    fn overlay_scale(&self) -> f64 {
        self.app
            .as_ref()
            .and_then(App::overlay_window)
            .and_then(|w| w.scale)
            .unwrap_or(0.0)
    }

    /// Remembers where the overlay is on screen and how large it draws (`scale`, as in
    /// `overlay_scale()`); ignores empty rectangles.
    #[func]
    fn set_overlay_window(&mut self, rect: Rect2i, scale: f64) {
        let (Ok(width), Ok(height)) = (u32::try_from(rect.size.x), u32::try_from(rect.size.y))
        else {
            return;
        };
        if width == 0 || height == 0 {
            return;
        }
        let window = orqwa_app::OverlayWindow {
            x: rect.position.x,
            y: rect.position.y,
            width,
            height,
            scale: (scale > 0.0).then_some(scale),
        };
        self.command(|app| app.set_overlay_window(window));
    }

    /// The map picture of the course at `path` (#192): a PNG, empty without one.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    #[allow(clippy::unused_self)] // a #[func] is called on the node
    fn course_preview(&self, path: GString) -> PackedByteArray {
        App::course_preview(Path::new(&path.to_string()))
            .map(|png| PackedByteArray::from(png.as_slice()))
            .unwrap_or_default()
    }

    /// Whether the course at `path` is to get a new map picture (#203): it has none, or one of
    /// its route's corridor only.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    #[allow(clippy::unused_self)] // a #[func] is called on the node
    fn course_preview_outdated(&self, path: GString) -> bool {
        App::course_preview_outdated(Path::new(&path.to_string()))
    }

    /// The map to draw the picture of the course at `path` from (#203), when it is the loaded
    /// course and its map came with it; empty otherwise. `{size, frame, background, vertices,
    /// colors, lines, labels}`: the picture's `size` in pixels; the `frame` of land it shows
    /// and everything on it in metres east/north of the route start (as in `track()`); woods,
    /// towns and lakes as coloured triangles (`vertices`, `colors`); `lines` as
    /// `[{points, width, color}]` in drawing order, `width` in pixels; `labels` as
    /// `[{name, latin, kind, position}]`, most important first: `name` in the local language,
    /// `latin` in Latin letters (empty if unknown), `kind` one of `"city"`, `"town"`,
    /// `"village"`.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn course_map(&self, path: GString) -> VarDictionary {
        use orqwa_world::course_map::{PICTURE, PlaceKind};
        let path = PathBuf::from(path.to_string());
        let Some(map) = self.app.as_ref().and_then(|app| app.course_map(&path)) else {
            return VarDictionary::new();
        };
        // The cards' pictures are dark whatever the rider's minimap.
        let style = MapStyle::Dark;
        let mut labels = VarArray::new();
        for label in &map.labels {
            let kind = match label.kind {
                PlaceKind::City => "city",
                PlaceKind::Town => "town",
                PlaceKind::Village => "village",
            };
            labels.push(
                &vdict! {
                    "name" => label.name.as_str(),
                    "latin" => label.latin.as_str(),
                    "kind" => kind,
                    "position" => Vector2::new(label.position[0], label.position[1]),
                }
                .to_variant(),
            );
        }
        let frame = map.frame;
        // Drawing precision.
        #[allow(clippy::cast_possible_truncation)]
        let frame = Rect2::new(
            Vector2::new(frame.west as f32, frame.south as f32),
            Vector2::new(frame.width() as f32, frame.height() as f32),
        );
        let size = Vector2i::new(
            i32::try_from(PICTURE[0]).unwrap_or(i32::MAX),
            i32::try_from(PICTURE[1]).unwrap_or(i32::MAX),
        );
        let (vertices, colors) = flat_map_arrays(&map.areas, style);
        vdict! {
            "size" => size,
            "frame" => frame,
            "background" => map_color(MapFeature::Land, style),
            "vertices" => &vertices,
            "colors" => &colors,
            "lines" => &map_lines(&map.lines, style),
            "labels" => &labels,
        }
    }

    /// Keeps `png`, the map picture the app drew, with the course at `path` (#192).
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn set_course_preview(&mut self, path: GString, png: PackedByteArray) {
        let bytes = png.to_vec();
        let path = PathBuf::from(path.to_string());
        self.command(|app| app.set_course_preview(&path, &bytes));
    }

    /// Whether the ride view's control bar is folded away to its corner (#189).
    #[func]
    fn ride_bar_folded(&self) -> bool {
        self.app.as_ref().is_some_and(App::ride_bar_folded)
    }

    /// Remembers whether the ride view's control bar is folded away.
    #[func]
    fn set_ride_bar_folded(&mut self, folded: bool) {
        self.command(|app| app.set_ride_bar_folded(folded));
    }

    /// Whether the app plays its own sounds, such as the chime of a ride's moments (R125).
    #[func]
    fn sounds(&self) -> bool {
        self.app.as_ref().is_none_or(App::sounds)
    }

    /// Turns the app's own sounds on or off.
    #[func]
    fn set_sounds(&mut self, on: bool) {
        self.command(|app| app.set_sounds(on));
    }

    /// Whether the app fills the screen (#218).
    #[func]
    fn fullscreen(&self) -> bool {
        self.app.as_ref().is_some_and(App::fullscreen)
    }

    /// Remembers whether the app fills the screen, to start that way next time.
    #[func]
    fn set_fullscreen(&mut self, fullscreen: bool) {
        self.command(|app| app.set_fullscreen(fullscreen));
    }

    /// Whether this computer looks for a newer ORQWA at start-up (R99).
    #[func]
    fn checks_for_updates(&self) -> bool {
        self.app.as_ref().is_some_and(App::checks_for_updates)
    }

    /// Turns looking for a newer ORQWA at start-up on or off for this computer.
    #[func]
    fn set_checks_for_updates(&mut self, on: bool) {
        self.command(|app| app.set_checks_for_updates(on));
    }

    /// Whether this computer keeps the courses' worlds once built, so that riding one again
    /// starts at once (R101).
    #[func]
    fn keeps_worlds(&self) -> bool {
        self.app.as_ref().is_some_and(App::keeps_worlds)
    }

    /// Turns keeping the courses' worlds on (the world loaded is kept at once) or off (every
    /// kept world is deleted) for this computer.
    #[func]
    fn set_keeps_worlds(&mut self, on: bool) {
        self.command(|app| app.set_keeps_worlds(on));
    }

    /// How many bytes the kept worlds take on this computer.
    #[func]
    fn kept_worlds_size(&self) -> i64 {
        self.app.as_ref().map_or(0, |app| {
            i64::try_from(app.kept_worlds_size()).unwrap_or(i64::MAX)
        })
    }

    /// The worlds this computer keeps (R136), by course name: `[{path, names, size}]`, `path`
    /// one of the world's courses, to delete it by with `delete_kept_world`, `names` those of
    /// the courses it is the world of (one, unless a course is in the library twice) and
    /// `size` the bytes it takes. Empty while worlds are not kept.
    #[func]
    fn kept_worlds(&self) -> VarArray {
        let mut array = VarArray::new();
        for world in self.app.as_ref().map(App::kept_worlds).unwrap_or_default() {
            let Some(first) = world.courses.first() else {
                continue;
            };
            let names: PackedStringArray = world
                .courses
                .iter()
                .map(|course| GString::from(course.manifest.name.as_str()))
                .collect();
            let dict = vdict! {
                "path" => first.path.display().to_string().as_str(),
                "names" => &names,
                "size" => i64::try_from(world.size).unwrap_or(i64::MAX),
            };
            array.push(&dict.to_variant());
        }
        array
    }

    /// Deletes the kept world of the course at `path` (R136): the course stays, and its next
    /// ride builds the world again. Emits `failed` on errors.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn delete_kept_world(&mut self, path: GString) -> bool {
        let path = PathBuf::from(path.to_string());
        self.command(|app| app.delete_kept_world(&path))
    }

    /// Looks for a newer ORQWA in the background (R99): at start-up (`asked` false) only if
    /// this computer does, telling only a newer version; when the rider `asked`, also
    /// `up_to_date` or `update_failed`. Never during a ride. Whether it looks.
    #[func]
    fn check_for_updates(&mut self, asked: bool) -> bool {
        self.app
            .as_mut()
            .is_some_and(|app| app.check_for_updates(asked))
    }

    /// Downloads the newer release's installer (emits `update_downloaded` or
    /// `update_failed`); whether it does.
    #[func]
    fn download_update(&mut self) -> bool {
        self.app.as_mut().is_some_and(App::download_update)
    }

    /// Starts installing the downloaded update; on success the app must quit at once, so the
    /// installer can replace it. Reports a failure and returns false otherwise.
    #[func]
    fn install_update(&mut self) -> bool {
        self.command(App::install_update)
    }

    /// The downloaded update's path, "" before one is: on Android, ORQWA's Android plugin
    /// installs it (ADR 0024).
    #[func]
    fn downloaded_update(&self) -> GString {
        self.app
            .as_ref()
            .and_then(App::downloaded_update)
            .map(|path| GString::from(path.to_string_lossy().as_ref()))
            .unwrap_or_default()
    }

    /// How the phone remote (#228) is set and doing: `{on, port, address ("" while off or
    /// without a network), connected}`.
    #[func]
    fn phone_remote(&self) -> VarDictionary {
        let Some(app) = self.app.as_ref() else {
            return VarDictionary::new();
        };
        let remote = app.phone_remote();
        vdict! {
            "on" => remote.on,
            "port" => i64::from(remote.port),
            "address" => remote.address.unwrap_or_default(),
            "connected" => remote.connected,
        }
    }

    /// The QR code of the phone remote's address: `{size, modules}`, `modules` a byte per
    /// module row by row, 0 dark and 255 light, without the quiet zone; empty without one.
    #[func]
    fn phone_remote_code(&self) -> VarDictionary {
        let Some(code) = self.app.as_ref().and_then(App::phone_remote_code) else {
            return VarDictionary::new();
        };
        let modules: PackedByteArray = code
            .dark
            .iter()
            .map(|&dark| if dark { 0 } else { 255 })
            .collect();
        vdict! {
            "size" => i64::try_from(code.size).unwrap_or_default(),
            "modules" => &modules,
        }
    }

    /// The name the phone remote's connection is reported by.
    #[func]
    fn phone_remote_name() -> GString {
        GString::from(orqwa_app::remote::REMOTE_NAME)
    }

    /// The keys of the texts the phone remote's page says, for [`Self::set_phone_remote_texts`].
    #[func]
    fn phone_remote_words() -> PackedStringArray {
        orqwa_app::remote::remote_words()
            .map(GString::from)
            .collect()
    }

    /// The phone remote's page says `words` (text by key, translated) in `language`.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn set_phone_remote_texts(&mut self, language: GString, words: VarDictionary) {
        let Some(app) = self.app.as_mut() else {
            return;
        };
        app.set_phone_remote_texts(orqwa_app::remote::RemoteTexts {
            language: language.to_string(),
            words: words
                .iter_shared()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
        });
    }

    /// Starts the phone remote if the rider left it on, as at start.
    #[func]
    fn resume_phone_remote(&mut self) -> bool {
        self.command(App::resume_phone_remote)
    }

    /// Turns the phone remote on or off, for the next time too.
    #[func]
    fn set_phone_remote(&mut self, on: bool) -> bool {
        self.command(|app| app.set_phone_remote(on))
    }

    /// Moves the phone remote to `port` (1024–65535).
    #[func]
    fn set_phone_remote_port(&mut self, port: i64) -> bool {
        let Ok(port) = u16::try_from(port.clamp(1024, 65535)) else {
            return false;
        };
        self.command(|app| app.set_phone_remote_port(port))
    }

    /// Gives the phone remote a new secret address: phones paired before can no longer press.
    #[func]
    fn renew_phone_remote(&mut self) -> bool {
        self.command(App::renew_phone_remote)
    }

    /// Whether rides are simulated (fake trainer): they can be sped up and jumped (#53).
    #[func]
    fn simulating(&self) -> bool {
        self.app.as_ref().is_some_and(App::simulating)
    }

    /// Speeds the simulated ride up, 1 to 20 times; returns the speed in effect.
    #[func]
    fn set_time_scale(&mut self, scale: f64) -> f64 {
        self.app
            .as_mut()
            .map_or(1.0, |app| app.set_time_scale(scale))
    }

    /// Moves the simulated ride's rider to `distance_m` along the route.
    #[func]
    fn jump_to_distance(&mut self, distance_m: f64) -> bool {
        self.app
            .as_mut()
            .is_some_and(|app| app.jump_to(Meters(distance_m)))
    }

    /// Moves the simulated ride's rider to the route's point nearest `x`/`y` (metres east and
    /// north of the start, as on the map).
    #[func]
    fn jump_near(&mut self, x: f64, y: f64) -> bool {
        self.app.as_mut().is_some_and(|app| app.jump_near(x, y))
    }

    /// Whether the trainer is connected now (rides start at once then).
    #[func]
    fn trainer_connected(&self) -> bool {
        self.app.as_ref().is_some_and(App::trainer_connected)
    }

    /// Connects the simulated trainer, with a simulated heart rate.
    #[func]
    fn connect_fake_trainer(&mut self, power: f64, cadence: f64) -> bool {
        let choice = TrainerChoice::Fake(FakeRider {
            power: Watts(power),
            cadence: Rpm(cadence),
            heart: Some(FakeHeart::default()),
        });
        self.command(|app| app.connect_trainer(choice))
    }

    /// Connects a scanned trainer by its index.
    #[func]
    fn connect_trainer(&mut self, index: i64) -> bool {
        let Ok(index) = usize::try_from(index) else {
            return false;
        };
        self.command(|app| app.connect_trainer(TrainerChoice::Discovered(index)))
    }

    /// Connects a scanned heart-rate sensor by its index.
    #[func]
    fn connect_heart_rate(&mut self, index: i64) -> bool {
        let Ok(index) = usize::try_from(index) else {
            return false;
        };
        self.command(|app| app.connect_heart_rate(index))
    }

    /// Lets the heart-rate sensor go and forgets it (R76).
    #[func]
    fn disconnect_heart_rate(&mut self) -> bool {
        self.command(App::disconnect_heart_rate)
    }

    /// Lets the power meter go and forgets it (R76): the trainer measures again.
    #[func]
    fn disconnect_power_meter(&mut self) -> bool {
        self.command(App::disconnect_power_meter)
    }

    /// Lets the shifter go and forgets it (R76); the keyboard keeps shifting.
    #[func]
    fn disconnect_controller(&mut self) -> bool {
        self.command(App::disconnect_controller)
    }

    /// Connects a scanned power meter by its index; its power counts over the trainer's (R71).
    #[func]
    fn connect_power_meter(&mut self, index: i64) -> bool {
        let Ok(index) = usize::try_from(index) else {
            return false;
        };
        self.command(|app| app.connect_power_meter(index))
    }

    /// Connects a scanned cadence sensor by its index; its cadence counts first (R72).
    #[func]
    fn connect_cadence_sensor(&mut self, index: i64) -> bool {
        let Ok(index) = usize::try_from(index) else {
            return false;
        };
        self.command(|app| app.connect_cadence_sensor(index))
    }

    /// Which device's cadence counts (R72): `automatic`, `trainer` or `power_meter`.
    #[func]
    fn cadence_source(&self) -> GString {
        let source = self
            .app
            .as_ref()
            .map(App::cadence_source)
            .unwrap_or_default();
        GString::from(source.name())
    }

    /// Takes the cadence from `name` (as `cadence_source` returns it) from the next ride on,
    /// letting go of any cadence sensor. False for an unknown name.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn set_cadence_source(&mut self, name: GString) -> bool {
        let Some(source) = CadenceSource::from_name(&name.to_string()) else {
            return false;
        };
        self.command(|app| app.set_cadence_source(source))
    }

    /// Starts riding the loaded route as the active rider, against `ghost`: `{kind}` with
    /// `kind` one of `none`, `best` (own best on the route), `power` (`{watts}`), `wkg`
    /// (`{watts_per_kg}`) or `activity` (`{path}` of a GPX or FIT file). Emits `failed` and
    /// returns false if it cannot start, e.g. no best time yet.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn start_ride(&mut self, difficulty: f64, flat_descents: bool, ghost: VarDictionary) -> bool {
        let descent = if flat_descents {
            DescentMode::Flat
        } else {
            DescentMode::Coast
        };
        let number = |key: &str| {
            ghost
                .get(key)
                .and_then(|v| v.try_to::<f64>().ok())
                .unwrap_or(0.0)
        };
        let kind = ghost
            .get("kind")
            .and_then(|v| v.try_to::<GString>().ok())
            .map(|k| k.to_string())
            .unwrap_or_default();
        let choice = match kind.as_str() {
            "best" => GhostChoice::PersonalBest,
            "power" => GhostChoice::Power(Watts(number("watts"))),
            "wkg" => GhostChoice::WattsPerKg(number("watts_per_kg")),
            "activity" => GhostChoice::Activity(PathBuf::from(
                ghost
                    .get("path")
                    .and_then(|v| v.try_to::<GString>().ok())
                    .map(|p| p.to_string())
                    .unwrap_or_default(),
            )),
            _ => GhostChoice::None,
        };
        self.command(|app| app.start_ride(Percent(difficulty), descent, &choice))
    }

    /// Starts a workout (R56, R21) as the active rider: `{kind, power_w, zone, bpm, min_w,
    /// max_w, id, test, name}` with `kind` one of `power` (hold `power_w`), `zone` (hold the
    /// middle of heart-rate zone `zone`, 1–5), `bpm` (hold `bpm`) — the heart-rate ones between
    /// `min_w` and `max_w` —, `plan` (the structured workout `id` from `workouts()`) or
    /// `ftp_test` (R22: `test` `ramp`, `twenty_minutes` or `two_by_eight`). On the loaded
    /// course in 3D if `on_course` (R58), else on its own, going into the history as `name`.
    /// Emits `failed` and returns false if it cannot start.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn start_workout(
        &mut self,
        workout: VarDictionary,
        on_course: bool,
        flat_descents: bool,
    ) -> bool {
        let Some(parsed) = self.workout_of(&workout) else {
            return false;
        };
        let name = workout
            .get("name")
            .and_then(|v| v.try_to::<GString>().ok())
            .map(|n| n.to_string())
            .unwrap_or_default();
        let descent = if flat_descents {
            DescentMode::Flat
        } else {
            DescentMode::Coast
        };
        self.command(|app| app.start_workout(parsed, on_course, descent, &name))
    }

    /// Changes the current workout, given as for `start_workout()`.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn change_workout(&mut self, workout: VarDictionary) {
        if let Some(parsed) = self.workout_of(&workout)
            && let Some(app) = self.app.as_mut()
        {
            app.change_workout(parsed);
        }
    }

    /// The structured workouts to choose from (R21): `[{id, name, description, duration_s,
    /// builtin, steps}]`, the built-in ones first. `steps` are `[{duration_s, from_w, to_w,
    /// from_pct, to_pct, cadence, message, all_out}]`: power in watts for the active rider's FTP
    /// and in percent of it (`null` in free steps), cadence `null` if none, and the first
    /// message of the step ("" if none) — what the workout editor works with. `all_out` is
    /// false here, true for the free steps of `ftp_test_steps()`.
    #[func]
    fn workouts(&self) -> VarArray {
        let Some(app) = self.app.as_ref() else {
            return VarArray::new();
        };
        let ftp = app.profile().profile.ftp;
        let planned = app.planned_workouts();
        let mut array = VarArray::new();
        for entry in app.workouts() {
            let steps = plan_steps(&entry.plan, ftp, false);
            // The platform a planned workout comes from (R81), "" for the library's.
            let platform = planned
                .iter()
                .find(|(_, p)| p.id == entry.id)
                .map_or("", |(name, _)| name);
            array.push(
                &vdict! {
                    "id" => entry.id.as_str(),
                    "name" => entry.plan.name.as_str(),
                    "description" => entry.plan.description.as_str(),
                    "duration_s" => entry.plan.duration().as_secs_f64(),
                    "builtin" => entry.id.starts_with(orqwa_workouts::BUILTIN),
                    "planned" => platform,
                    "steps" => &steps,
                }
                .to_variant(),
            );
        }
        array
    }

    /// An FTP test (R22) as the active rider would ride it, for a preview: `test` as in
    /// `start_workout()`, its steps as in `workouts()`. The ramp test's go up to 150 % of the
    /// rider's FTP: it goes on until they give way.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn ftp_test_steps(&self, test: GString) -> VarArray {
        let Some(app) = self.app.as_ref() else {
            return VarArray::new();
        };
        let ftp = app.profile().profile.ftp;
        match ftp_test(&test.to_string(), ftp) {
            Workout::EffortTest(test) => plan_steps(&test.plan, test.ftp, true),
            Workout::RampTest(test) => {
                let step = |seconds: f64, watts: f64| {
                    vdict! { "duration_s" => seconds, "from_w" => watts, "to_w" => watts }
                        .to_variant()
                };
                let mut steps = VarArray::new();
                steps.push(&step(test.warm_up.as_secs_f64(), test.warm_up_power.0));
                let mut watts = test.start.0;
                while watts <= ftp.0 * 1.5 {
                    steps.push(&step(test.step_duration.as_secs_f64(), watts));
                    watts += test.step.0;
                }
                steps
            }
            _ => VarArray::new(),
        }
    }

    /// Makes `watts` the active rider's FTP, e.g. from an FTP test; false (and `failed`) if
    /// it cannot be saved.
    #[func]
    fn use_ftp(&mut self, watts: f64) -> bool {
        self.command(|app| app.set_ftp(Watts(watts)))
    }

    /// Saves a workout from the editor: `{name, description, steps: [{duration_s, from_pct,
    /// to_pct, free, cadence, message}]}` (cadence 0 for none, message "" for none), over
    /// `replace` if that is the library's ZWO file being edited. Returns its id, or "" (and
    /// emits `failed`).
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn save_workout(&mut self, workout: VarDictionary, replace: GString) -> GString {
        let plan = plan_from(&workout);
        let replace = replace.to_string();
        let Some(app) = self.app.as_mut() else {
            return GString::new();
        };
        match app.save_workout(&plan, Some(replace.as_str()).filter(|r| !r.is_empty())) {
            Ok(id) => GString::from(id.as_str()),
            Err(error) => {
                self.report_failure(&Failure::from(error));
                GString::new()
            }
        }
    }

    /// Deletes a workout file from the library; false (and `failed`) for built-ins.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn delete_workout(&mut self, id: GString) -> bool {
        let id = id.to_string();
        self.command(|app| app.delete_workout(&id))
    }

    /// The extensions of workout files, for file dialogs.
    #[func]
    fn workout_extensions() -> PackedStringArray {
        orqwa_workouts::extensions()
            .into_iter()
            .map(GString::from)
            .collect()
    }

    /// Adds a workout file to the library; returns its id, or "" (and emits `failed`).
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn import_workout(&mut self, path: GString) -> GString {
        let Some(app) = self.app.as_mut() else {
            return GString::new();
        };
        match app.import_workout(std::path::Path::new(&path.to_string())) {
            Ok(id) => GString::from(id.as_str()),
            Err(error) => {
                self.report_failure(&Failure::from(error));
                GString::new()
            }
        }
    }

    /// The workout a `start_workout()` dictionary describes; `None` (and `failed`) if its
    /// structured workout cannot be read.
    fn workout_of(&mut self, workout: &VarDictionary) -> Option<Workout> {
        let app = self.app.as_ref()?;
        let text = |key: &str| {
            workout
                .get(key)
                .and_then(|v| v.try_to::<GString>().ok())
                .map(|k| k.to_string())
                .unwrap_or_default()
        };
        if text("kind") != "plan" {
            return Some(workout_from(workout, &app.profile().profile));
        }
        match app.structured_workout(&text("id")) {
            Ok(parsed) => Some(parsed),
            Err(error) => {
                self.report_failure(&Failure::from(error));
                None
            }
        }
    }

    /// The active rider's heart-rate zones 1–5 as `(lowest, highest)` bpm.
    #[func]
    fn heart_rate_zones(&self) -> PackedVector2Array {
        let Some(app) = self.app.as_ref() else {
            return PackedVector2Array::new();
        };
        let profile = &app.profile().profile;
        (1..=5)
            .map(|zone| {
                let (low, high) = profile.heart_rate_zone_range(zone);
                vector2(low.0, high.0)
            })
            .collect()
    }

    /// The current ride so far as `(elapsed_s, value)` points, for a live chart: `power` and
    /// `heart_rate`, at most `max_points` each.
    #[func]
    fn ride_chart(&self, max_points: i64) -> VarDictionary {
        let samples = self.app.as_ref().map_or(&[][..], App::ride_samples);
        let max_points = usize::try_from(max_points).unwrap_or(200);
        let series = |value: fn(&orqwa_domain::recording::Sample) -> Option<f64>| {
            view::ride_series(samples, max_points, value)
                .into_iter()
                .map(|(a, b)| vector2(a, b))
                .collect::<PackedVector2Array>()
        };
        vdict! {
            "power" => &series(|s| s.power.map(|p| p.0)),
            "heart_rate" => &series(|s| s.heart_rate.map(|h| h.0)),
        }
    }

    /// Controls the rider's music app: `play_pause`, `next` or `previous` (emits `failed` if
    /// no player reacts).
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn control_music(&mut self, command: GString) {
        let command = match command.to_string().as_str() {
            "next" => MediaCommand::Next,
            "previous" => MediaCommand::Previous,
            _ => MediaCommand::PlayPause,
        };
        if let Some(app) = self.app.as_mut() {
            app.control_music(command);
        }
    }

    /// Connects a scanned shifter (a Shimano Di2 or a Zwift Click) by its index: its buttons shift or do what
    /// else the rider gave them (R7, #139).
    #[func]
    fn connect_controller(&mut self, index: i64) -> bool {
        let Ok(index) = usize::try_from(index) else {
            return false;
        };
        self.command(|app| app.connect_controller(index))
    }

    /// The names of the actions a shifter's buttons can be given (#139), in the order to
    /// offer them.
    #[func]
    fn button_actions() -> PackedStringArray {
        ButtonAction::ALL
            .into_iter()
            .map(|action| GString::from(action.name()))
            .collect()
    }

    /// What the buttons of the shifter `model` (`"di2"`, `"zwift_click"`, `"zwift_click_v2"`
    /// or the phone remote's `"phone_remote"`) do (#139, R84, #228): for each button 1–10 (a
    /// Di2's D-Fly channels 1–4; a Zwift Click's + is 1 and its − 2, with no double press; the
    /// phone's presses only), the action names of a press, a hold and a double press, "" where
    /// it does nothing. Empty for an unknown model.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn button_map(&self, model: GString) -> VarArray {
        let Some(model) = ControllerModel::from_name(&model.to_string()) else {
            return VarArray::new();
        };
        let map = self
            .app
            .as_ref()
            .map_or_else(Default::default, |app| app.button_map(model));
        (1..=BUTTONS)
            .map(|channel| {
                Press::ALL
                    .into_iter()
                    .map(|press| map.action(channel, press).map_or("", ButtonAction::name))
                    .map(GString::from)
                    .collect::<PackedStringArray>()
                    .to_variant()
            })
            .collect()
    }

    /// Gives `press` (0 a press, 1 a hold, 2 a double press) of button `channel` (1–10) of
    /// the shifter `model` the action called `action`, or nothing with ""; the other models'
    /// buttons stay as they are (R84). False for an unknown model, press or action.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn assign_button(&mut self, model: GString, channel: i64, press: i64, action: GString) -> bool {
        let Some(model) = ControllerModel::from_name(&model.to_string()) else {
            return false;
        };
        let action = action.to_string();
        let assigned = if action.is_empty() {
            None
        } else {
            let Some(found) = ButtonAction::from_name(&action) else {
                return false;
            };
            Some(found)
        };
        let (Ok(channel), Some(&press)) = (
            u8::try_from(channel),
            usize::try_from(press).ok().and_then(|p| Press::ALL.get(p)),
        ) else {
            return false;
        };
        if !(1..=BUTTONS).contains(&channel) {
            return false;
        }
        self.command(|app| {
            let mut map = app.button_map(model);
            map.assign(channel, press, assigned);
            app.set_button_map(model, map)
        })
    }

    /// Shifts the virtual gears (R9): `direction` 1 up (harder), -1 down; nothing without them.
    #[func]
    fn shift(&mut self, direction: i64) {
        if let Some(app) = self.app.as_mut() {
            app.shift(if direction > 0 {
                Shift::Up
            } else {
                Shift::Down
            });
        }
    }

    /// Pauses or resumes the ride: the clock, the road and the trainer wait. Returns whether
    /// the ride is paused afterwards.
    #[func]
    fn set_paused(&mut self, paused: bool) -> bool {
        self.app.as_mut().is_some_and(|app| app.set_paused(paused))
    }

    /// Whether the ride is paused.
    #[func]
    fn is_paused(&self) -> bool {
        self.app.as_ref().is_some_and(App::is_paused)
    }

    /// Changes difficulty and descent mode of the current ride.
    #[func]
    fn adjust_ride(&mut self, difficulty: f64, flat_descents: bool) {
        let descent = if flat_descents {
            DescentMode::Flat
        } else {
            DescentMode::Coast
        };
        if let Some(app) = self.app.as_mut() {
            app.adjust_ride(Percent(difficulty), descent);
        }
    }

    /// Makes the ride's trainer difficulty `steps` steps of 10 % harder (easier if negative),
    /// within 0–100 %. Returns the new difficulty in percent, or -1 when not riding.
    #[func]
    fn step_difficulty(&mut self, steps: i64) -> f64 {
        let steps = i32::try_from(steps).unwrap_or_default();
        self.app
            .as_mut()
            .and_then(|app| app.step_difficulty(steps))
            .map_or(-1.0, |difficulty| difficulty.0)
    }

    /// Makes the workout's powers `steps` steps of 5 % harder (easier if negative), within
    /// 50–150 % of the plan. Returns the new share in percent, or -1 when not in a constant
    /// power or structured workout.
    #[func]
    fn step_intensity(&mut self, steps: i64) -> f64 {
        let steps = i32::try_from(steps).unwrap_or_default();
        self.app
            .as_mut()
            .and_then(|app| app.step_intensity(steps))
            .map_or(-1.0, |intensity| intensity.0)
    }

    /// Goes on with the next step of a structured workout; false if there is none to skip.
    #[func]
    fn skip_step(&mut self) -> bool {
        self.app.as_mut().is_some_and(App::skip_step)
    }

    /// Ends the current ride without saving it.
    #[func]
    fn abort_ride(&mut self) {
        if let Some(app) = self.app.as_mut() {
            app.abort_ride();
        }
    }

    /// Whether the active rider has finished the loaded route before, so `best` can be raced.
    #[func]
    fn has_personal_best(&self) -> bool {
        self.app
            .as_ref()
            .and_then(|app| app.route().map(|route| app.records_for(route)))
            .is_some_and(|records| records.route.is_some())
    }

    /// All riders: `[{id, name}]`, by name.
    #[func]
    fn profiles(&self) -> VarArray {
        let mut array = VarArray::new();
        for stored in self.app.as_ref().map(App::profiles).unwrap_or_default() {
            array.push(&profile_dict(&stored.id, &stored.profile).to_variant());
        }
        array
    }

    /// The active rider: `{id, name, rider_mass_kg, bike_mass_kg, ftp_w, max_heart_rate_bpm,
    /// units, language, avatar}` with `units` either `"metric"` or `"imperial"`, `language` a
    /// locale code, empty for the system language, and `avatar` either `"female"` or `"male"`.
    #[func]
    fn profile(&self) -> VarDictionary {
        self.app
            .as_ref()
            .map(App::profile)
            .map_or_else(VarDictionary::new, |stored| {
                profile_dict(&stored.id, &stored.profile)
            })
    }

    /// How far a rider at `speed_kmh` leans into a bend of `curvature` (1 / radius, positive to
    /// the right, as in `ride_state()`): radians, positive to the right (R46).
    #[func]
    fn lean_angle(speed_kmh: f64, curvature: f64) -> f64 {
        lean_angle(
            MetersPerSecond::from_kilometers_per_hour(speed_kmh),
            curvature,
        )
    }

    /// Switches to another rider (emits `profile_changed`): the devices in use that are not
    /// theirs are let go, and their shifter's buttons and cadence source apply (R82);
    /// `reconnect_remembered` then connects their own.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn select_profile(&mut self, id: GString) -> bool {
        let selected = self.command(|app| app.select_profile(&id.to_string()));
        if selected {
            self.signals().profile_changed().emit();
        }
        selected
    }

    /// Deletes a rider with their rides (emits `failed` for the only rider); deleting the
    /// active one switches to another, as `select_profile` does.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn delete_profile(&mut self, id: GString) -> bool {
        let id = id.to_string();
        let active = self.app.as_ref().is_some_and(|app| app.profile().id == id);
        let deleted = self.command(|app| app.delete_profile(&id));
        if deleted && active {
            self.signals().profile_changed().emit();
        }
        deleted
    }

    /// Saves a rider (a new one if `id` is empty) from a dictionary shaped like `profile()`
    /// and makes it active; returns its id, or an empty string on failure (emits `failed`).
    /// Saved during a ride, the ride follows the rider's weight, drivetrain and FTP (R98).
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn save_profile(&mut self, id: GString, data: VarDictionary) -> GString {
        let number = |key: &str, default: f64| {
            data.get(key)
                .and_then(|v| v.try_to::<f64>().ok())
                .unwrap_or(default)
        };
        let defaults = Profile::default();
        let profile = Profile {
            name: data
                .get("name")
                .and_then(|v| v.try_to::<GString>().ok())
                .map_or(defaults.name, |n| n.to_string()),
            rider_mass: Kilograms(number("rider_mass_kg", defaults.rider_mass.0)),
            bike_mass: Kilograms(number("bike_mass_kg", defaults.bike_mass.0)),
            ftp: Watts(number("ftp_w", defaults.ftp.0)),
            max_heart_rate: BeatsPerMinute(number("max_heart_rate_bpm", defaults.max_heart_rate.0)),
            power_zones: Profile::sane_power_zones(&shares(&data, "power_zones_pct")),
            heart_rate_zones: Profile::sane_heart_rate_zones(&shares(
                &data,
                "heart_rate_zones_pct",
            )),
            default_difficulty: Percent(
                number("default_difficulty_pct", defaults.default_difficulty.0).clamp(0.0, 100.0),
            ),
            language: data
                .get("language")
                .and_then(|v| v.try_to::<GString>().ok())
                .map(|l| l.to_string())
                .unwrap_or_default(),
            units: if data
                .get("units")
                .and_then(|v| v.try_to::<GString>().ok())
                .is_some_and(|u| u == "imperial")
            {
                UnitSystem::Imperial
            } else {
                UnitSystem::Metric
            },
            avatar: if data
                .get("avatar")
                .and_then(|v| v.try_to::<GString>().ok())
                .is_some_and(|a| a == "male")
            {
                Avatar::Male
            } else {
                Avatar::Female
            },
            map_style: if data
                .get("map_style")
                .and_then(|v| v.try_to::<GString>().ok())
                .is_some_and(|m| m == "dark")
            {
                MapStyle::Dark
            } else {
                MapStyle::Coloured
            },
            voice: voice_of(&data),
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped
            rides_per_week: Some(number("rides_per_week", 0.0).round().clamp(0.0, 14.0) as u8)
                .filter(|n| *n > 0),
            // A rider saved without one, such as a new one, starts on the default (R98).
            drivetrain: match data
                .get("drivetrain")
                .and_then(|v| v.try_to::<GString>().ok())
                .map(|d| d.to_string())
                .as_deref()
            {
                Some("cassette") => Drivetrain::Cassette,
                Some("single_cog") => {
                    let teeth = |key: &str, default: f64| {
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        // clamped
                        let teeth = number(key, default).round().clamp(1.0, 99.0) as u8;
                        teeth
                    };
                    Drivetrain::SingleCog {
                        chainring: teeth("chainring", 50.0),
                        cog: teeth("cog", 14.0),
                    }
                }
                _ => Drivetrain::default(),
            },
        };
        let id = id.to_string();
        let id = (!id.is_empty()).then_some(id);
        let Some(app) = self.app.as_mut() else {
            return GString::new();
        };
        match app.save_profile(id.as_deref(), profile) {
            Ok(id) => {
                self.signals().profile_changed().emit();
                GString::from(id.as_str())
            }
            Err(error) => {
                self.report_failure(&Failure::from(error));
                GString::new()
            }
        }
    }

    /// Ends the ride and saves it (emits `ride_saved` or `failed`).
    #[func]
    fn finish_ride(&mut self) {
        let Some(app) = self.app.as_mut() else {
            return;
        };
        for event in app.finish_ride() {
            self.emit(event);
        }
    }

    /// The ride state: `{elapsed_s, distance_m, speed_kmh, power, cadence, heart_rate,
    /// watts_per_kg, power_zone, heart_rate_zone, metrics, workout}`, and on a route also
    /// `{remaining_m, grade, elevation_m, x, y, rider_x, rider_y, heading, curvature, ghost, climb}`; sensor
    /// values and what derives from them are `null` when unknown. Empty when not riding.
    /// `workout` is `{target_power_w, target_heart_rate, intensity_pct, progress}` in a
    /// workout, else `null`: the power `null` in a free step, the heart rate `null` but in a
    /// heart-rate hold, the intensity the share of the planned powers asked for, and
    /// `progress` of a structured workout `{step, steps, elapsed_s, step_left_s, left_s,
    /// cadence, next: {power_w, duration_s, all_out}, cue, all_out}` (`elapsed_s` the time into
    /// the workout, `next` `null` in the last step, `all_out` in the free steps of an FTP
    /// test), else `null`. `x`/`y` are metres east/north of the route start, as in `track()`;
    /// `heading` is the direction of travel in radians clockwise from north, `curvature` how
    /// sharply the road bends there (1 / radius, positive to the right).
    /// `rider_x`/`rider_y` place the avatar in the local traffic lane; `x`/`y` stay on the
    /// route centre line for the map. The ghost has both pairs too.
    #[func]
    fn ride_state(&self) -> VarDictionary {
        let Some(app) = self.app.as_ref() else {
            return VarDictionary::new();
        };
        let Some(state) = app.ride_state() else {
            return VarDictionary::new();
        };
        let optional = |value: Option<f64>| value.map_or_else(Variant::nil, |v| v.to_variant());
        let t = state.telemetry;
        let rider = &app.profile().profile;
        let zone =
            |value: Option<u8>| value.map_or_else(Variant::nil, |z| i64::from(z).to_variant());
        let mut dict = vdict! {
            "elapsed_s" => state.elapsed.as_secs_f64(),
            "paused" => app.is_paused(),
            "distance_m" => state.distance.0,
            "speed_kmh" => state.speed.as_kilometers_per_hour(),
            "power" => &optional(t.power.map(|p| p.0)),
            "cadence" => &optional(t.cadence.map(|c| c.0)),
            "heart_rate" => &optional(t.heart_rate.map(|h| h.0)),
            "watts_per_kg" => &optional(t.power.map(|p| rider.watts_per_kg(p))),
            "power_zone" => &zone(t.power.map(|p| rider.power_zone(p))),
            "heart_rate_zone" => &zone(t.heart_rate.map(|h| rider.heart_rate_zone(h))),
            "metrics" => &hud_values(app),
            "gear" => &state.gear.map_or_else(Variant::nil, |g| {
                vdict! {
                    "number" => i64::try_from(g.number).unwrap_or(0),
                    "of" => i64::try_from(g.of).unwrap_or(0),
                    "ratio" => g.ratio,
                }
                .to_variant()
            }),
            "workout" => &state.workout.as_ref().map_or_else(Variant::nil, workout_state),
        };
        // A ride without a route (a workout on its own) has no place: the loaded route, if
        // any, is not the one ridden.
        let (Some(position), Some(route)) = (state.position, app.route()) else {
            return dict;
        };
        let (x, y) = LocalProjection::for_route(route).project(position.lat, position.lon);
        let (rider_x, rider_y) = app.rider_position(state.distance, false).unwrap_or((x, y));
        dict.extend_dictionary(
            &vdict! {
                "remaining_m" => state.remaining.map_or(0.0, |r| r.0),
                "grade" => position.grade.0,
                "elevation_m" => position.elevation.0,
                "x" => x,
                "y" => y,
                "rider_x" => rider_x,
                "rider_y" => rider_y,
                "heading" => position.heading,
                "curvature" => position.curvature,
                "ghost" => &app.ghost_state().map_or_else(Variant::nil, |g| {
                    let at = route.position(g.distance);
                    let (gx, gy) = LocalProjection::for_route(route).project(at.lat, at.lon);
                    let (ghost_x, ghost_y) = app.rider_position(g.distance, true).unwrap_or((gx, gy));
                    vdict! {
                        "name" => g.name.as_str(),
                        "distance_m" => g.distance.0,
                        "x" => gx,
                        "y" => gy,
                        "rider_x" => ghost_x,
                        "rider_y" => ghost_y,
                        "elevation_m" => at.elevation.0,
                        "heading" => at.heading,
                        "curvature" => at.curvature,
                        "grade" => at.grade.0,
                        "gap_s" => &optional(g.gap),
                    }
                    .to_variant()
                }),
                "climb" => &app.current_climb().map_or_else(Variant::nil, |c| {
                    let count = i64::try_from(route.climbs().len()).unwrap_or(0);
                    vdict! {
                        "index" => i64::try_from(c.index).unwrap_or(0),
                        "count" => count,
                        "category" => c.climb.category.label(),
                        "length_m" => c.climb.length().0,
                        "ridden_m" => c.ridden.0,
                        "grade" => c.climb.average_grade.0,
                        "elapsed_s" => c.elapsed.as_secs_f64(),
                        "best_s" => &optional(c.best.map(|b| b.as_secs_f64())),
                    }
                    .to_variant()
                }),
            },
            true,
        );
        dict
    }

    /// Every metric the HUD can show: `[{id, caption, unit, decimals, kind, sample}]`, with `kind` one of
    /// `number`, `speed`, `distance`, `elevation`, `duration`, `grade`, `zone`. Values come in
    /// `ride_state()["metrics"]` in km/h, km, m and s.
    #[func]
    fn hud_metrics() -> VarArray {
        let mut array = VarArray::new();
        for metric in orqwa_app::hud::METRICS {
            let kind = match metric.kind {
                MetricKind::Number => "number",
                MetricKind::Speed => "speed",
                MetricKind::Distance => "distance",
                MetricKind::Elevation => "elevation",
                MetricKind::Duration => "duration",
                MetricKind::Grade => "grade",
                MetricKind::Zone => "zone",
            };
            array.push(
                &vdict! {
                    "id" => metric.id,
                    "caption" => metric.caption,
                    "unit" => metric.unit,
                    "decimals" => i64::from(metric.decimals),
                    "kind" => kind,
                    "sample" => metric.sample,
                }
                .to_variant(),
            );
        }
        array
    }

    /// The HUD metric ids a new rider starts with.
    #[func]
    fn hud_default_layout() -> PackedStringArray {
        orqwa_app::hud::DEFAULT_LAYOUT
            .iter()
            .map(|&id| GString::from(id))
            .collect()
    }

    /// Most metrics a HUD layout may hold.
    #[func]
    fn hud_max_metrics() -> i64 {
        i64::try_from(orqwa_app::hud::MAX_METRICS).unwrap_or(i64::MAX)
    }

    /// The active rider's HUD metric ids, in order; the first is shown large.
    #[func]
    fn hud_layout(&self) -> PackedStringArray {
        self.app
            .as_ref()
            .map(App::hud_layout)
            .unwrap_or_default()
            .iter()
            .map(|id| GString::from(id.as_str()))
            .collect()
    }

    /// A rider's HUD metric ids, in order, whether they are the active rider or not.
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn hud_layout_of(&self, id: GString) -> PackedStringArray {
        self.app
            .as_ref()
            .map(|app| app.hud_layout_of(&id.to_string()))
            .unwrap_or_default()
            .iter()
            .map(|id| GString::from(id.as_str()))
            .collect()
    }

    /// Saves the active rider's HUD metrics and returns them as saved (unknown ids dropped).
    #[func]
    #[allow(clippy::needless_pass_by_value)] // #[func] parameters are passed by value from Godot
    fn set_hud_layout(&mut self, layout: PackedStringArray) -> PackedStringArray {
        let ids: Vec<String> = layout.as_slice().iter().map(ToString::to_string).collect();
        let Some(app) = self.app.as_mut() else {
            return PackedStringArray::new();
        };
        match app.set_hud_layout(&ids) {
            Ok(saved) => saved.iter().map(|id| GString::from(id.as_str())).collect(),
            Err(error) => {
                self.report_failure(&Failure::from(error));
                PackedStringArray::new()
            }
        }
    }

    /// The loaded route's climbs with the active rider's best times: `{route_best_s,
    /// climbs: [{start_m, end_m, length_m, gain_m, grade, category, best_s}]}`; times are `null`
    /// without an earlier ride.
    #[func]
    fn climbs(&self) -> VarDictionary {
        let Some((app, route)) = self.app.as_ref().and_then(|a| a.route().map(|r| (a, r))) else {
            return VarDictionary::new();
        };
        let optional = |value: Option<Duration>| {
            value.map_or_else(Variant::nil, |v| v.as_secs_f64().to_variant())
        };
        let records = app.records_for(route);
        let mut climbs = VarArray::new();
        for (index, climb) in route.climbs().iter().enumerate() {
            climbs.push(
                &vdict! {
                    "start_m" => climb.start.0,
                    "end_m" => climb.end.0,
                    "length_m" => climb.length().0,
                    "gain_m" => climb.gain.0,
                    "grade" => climb.average_grade.0,
                    "category" => climb.category.label(),
                    "best_s" => &optional(records.climbs.get(index).copied().flatten()),
                }
                .to_variant(),
            );
        }
        vdict! {
            "route_best_s" => &optional(records.route),
            "climbs" => &climbs,
        }
    }

    /// How long the active rider would take for the loaded route (R66), with descents ridden
    /// flat or not: `{time_s, ftp_share_pct}`, at that share of their FTP; `time_s` is `null`
    /// without a route, or where that power would not get them to the finish.
    #[func]
    fn estimated_time(&self, flat_descents: bool) -> VarDictionary {
        let descent = if flat_descents {
            DescentMode::Flat
        } else {
            DescentMode::Coast
        };
        let time = self
            .app
            .as_ref()
            .and_then(|app| app.estimated_time(descent));
        vdict! {
            "time_s" => &time.map_or_else(Variant::nil, |t| t.as_secs_f64().to_variant()),
            "ftp_share_pct" => estimate::FTP_SHARE * 100.0,
        }
    }

    /// Number of terrain chunks in the generated world (0 until `world_ready`).
    #[func]
    fn world_chunk_count(&self) -> i64 {
        self.app
            .as_ref()
            .and_then(App::world)
            .map_or(0, |world| i64::try_from(world.chunks.len()).unwrap_or(0))
    }

    /// The land beyond the corridor: `{ground, water}` as mesh arrays in route coordinates,
    /// `ground` with land-cover colours like the chunks' terrain, `water` its lakes.
    #[func]
    fn horizon_meshes(&self) -> VarDictionary {
        self.app
            .as_ref()
            .and_then(App::world)
            .map_or_else(VarDictionary::new, |world| {
                vdict! {
                    "ground" => &mesh_arrays(&world.horizon.ground),
                    "water" => &mesh_arrays(&world.horizon.water),
                }
            })
    }

    /// World chunk `index`: `{center, terrain, buildings, modelled, streets, tracks, urban, water,
    /// plants, props, labels, wires, grass, flowers}`. `terrain`, `buildings`, `streets`, `tracks` and `water`
    /// (lakes, rivers and streams) are mesh arrays (`{vertices, normals, uvs, colors, indices}`);
    /// `urban` is another mesh array, with paving/concrete/asphalt/curb codes in its colours.
    /// `plants` maps vegetation model names (trees, bushes, ferns, rocks) to `MultiMesh` buffers
    /// (transform and colour), `props` maps roadside model names (guard rails, posts, poles) to
    /// transform buffers, `labels` lists the signs' text (`{text, transform}`), `wires` are the
    /// power lines' wires as mesh arrays, and `grass` and `flowers` are transform buffers.
    /// `modelled` lists cells of buildings drawn as models up close: `{models, shells}`, with
    /// `models` mapping model names to `MultiMesh` buffers (transform, colour, custom data)
    /// and `shells` the mesh arrays to draw in the distance instead. Geometry is relative to
    /// `center`, in Godot coordinates (x east, y up, −z north, metres from the route start).
    #[func]
    fn world_chunk(&self, index: i64) -> VarDictionary {
        self.world_chunk_with_tree_density(index, 1.0)
    }

    /// Like `world_chunk`, with only the requested share of trees uploaded (R108).
    /// The world itself stays complete, so changing quality needs no regeneration.
    #[func]
    fn world_chunk_with_tree_density(&self, index: i64, tree_density: f64) -> VarDictionary {
        let chunk = self
            .app
            .as_ref()
            .and_then(App::world)
            .zip(usize::try_from(index).ok())
            .and_then(|(world, index)| world.chunks.get(index));
        let Some(chunk) = chunk else {
            return VarDictionary::new();
        };
        let [x, y, z] = chunk.center;
        let mut plants = VarDictionary::new();
        for (name, buffer) in chunk.trees.models_for_density(tree_density) {
            if !buffer.is_empty() {
                plants.set(name, &PackedFloat32Array::from(buffer.as_ref()));
            }
        }
        let mut props = VarDictionary::new();
        for (name, buffer) in &chunk.props.models {
            props.set(name.as_str(), &PackedFloat32Array::from(buffer.as_slice()));
        }
        let labels: VarArray = chunk
            .props
            .labels
            .iter()
            .map(|label| {
                vdict! {
                    "text" => &GString::from(label.text.as_str()),
                    "transform" => &PackedFloat32Array::from(label.transform.as_slice()),
                }
                .to_variant()
            })
            .collect();
        let grass = PackedFloat32Array::from(chunk.trees.grass.as_slice());
        let flowers = PackedFloat32Array::from(chunk.trees.flowers.as_slice());
        let modelled: VarArray = chunk
            .modelled
            .iter()
            .map(|cell| {
                let mut models = VarDictionary::new();
                for (name, buffer) in &cell.models {
                    models.set(name.as_str(), &PackedFloat32Array::from(buffer.as_slice()));
                }
                vdict! { "models" => &models, "shells" => &mesh_arrays(&cell.shells) }.to_variant()
            })
            .collect();
        vdict! {
            "center" => Vector3::new(x, y, z),
            "terrain" => &mesh_arrays(&chunk.mesh),
            "buildings" => &mesh_arrays(&chunk.buildings),
            "modelled" => &modelled,
            "streets" => &mesh_arrays(&chunk.streets),
            "tracks" => &mesh_arrays(&chunk.tracks),
            "urban" => &mesh_arrays(&chunk.urban),
            "water" => &mesh_arrays(&chunk.water),
            "plants" => &plants,
            "props" => &props,
            "labels" => &labels,
            "wires" => &mesh_arrays(&chunk.props.wires),
            "grass" => &grass,
            "flowers" => &flowers,
        }
    }

    /// Bridges and tunnels as mesh arrays (vertex-coloured), in route coordinates.
    #[func]
    fn structures_mesh(&self) -> VarDictionary {
        self.app
            .as_ref()
            .and_then(App::world)
            .map_or_else(VarDictionary::new, |world| mesh_arrays(&world.structures))
    }

    /// The minimap (R70) in the rider's map style: `{background, land, lines, corridor}`, all
    /// in metres east/north of the route start (as in `track()`). `land` is the map of the
    /// land around the whole route as coloured triangles `{vertices, colors}`, with its rivers
    /// and roads as `lines` (`[{points, width, color}]` in drawing order, `width` in pixels)
    /// over it; `corridor` the route's corridor in detail, drawn over both. Empty without a
    /// route.
    #[func]
    fn minimap_mesh(&self) -> VarDictionary {
        let Some(app) = self.app.as_ref() else {
            return VarDictionary::new();
        };
        let Some((land, corridor)) = app.minimap() else {
            return VarDictionary::new();
        };
        let style = app.profile().profile.map_style;
        let mesh = |flat: &orqwa_world::FlatMap| {
            let (vertices, colors) = flat_map_arrays(flat, style);
            vdict! { "vertices" => &vertices, "colors" => &colors }
        };
        let empty = orqwa_world::FlatMap::default();
        vdict! {
            "background" => map_color(MapFeature::Land, style),
            "land" => &mesh(land.map_or(&empty, |l| &l.areas)),
            "lines" => &map_lines(land.map_or(&[][..], |l| &l.lines[..]), style),
            "corridor" => &mesh(corridor),
        }
    }

    /// The railways near the route as mesh arrays, in route coordinates (`u` across the bed of
    /// ballast, `v` metres along).
    #[func]
    fn railways_mesh(&self) -> VarDictionary {
        self.app
            .as_ref()
            .and_then(App::world)
            .map_or_else(VarDictionary::new, |world| mesh_arrays(&world.railways))
    }

    /// The road as mesh arrays `{vertices, normals, uvs, indices}`; `uv.y` is the distance
    /// along the route in metres.
    #[func]
    fn road_mesh(&self) -> VarDictionary {
        self.app
            .as_ref()
            .and_then(App::world)
            .map_or_else(VarDictionary::new, |world| mesh_arrays(&world.road))
    }

    /// `(distance m, elevation m)` points of the loaded route, at most `max_points`.
    #[func]
    fn elevation_profile(&self, max_points: i64) -> PackedVector2Array {
        self.points(max_points, view::elevation_profile)
    }

    /// The loaded route in metres east/north of its start, at most `max_points`.
    #[func]
    fn track(&self, max_points: i64) -> PackedVector2Array {
        self.points(max_points, view::track)
    }
}

impl OrqwaApp {
    fn record_failure(&mut self, failure: &Failure) -> GString {
        tracing::warn!(kind = ?failure.kind, details = %failure.details, "application operation failed");
        let message = failure.kind.message();
        let translated = TranslationServer::singleton()
            .translate(message)
            .to_string();
        let summary = format_failure(&translated, &failure.args);
        let args = failure
            .args
            .iter()
            .map(|arg| GString::from(arg.as_str()))
            .collect::<PackedStringArray>();
        self.last_failure = vdict! {
            "message" => message,
            "args" => &args,
            "details" => failure.details.as_str(),
        };
        let payload = self.last_failure.clone();
        self.signals().failure_reported().emit(&payload);
        GString::from(summary.as_str())
    }

    fn report_failure(&mut self, failure: &Failure) {
        let summary = self.record_failure(failure);
        self.signals().failed().emit(&summary);
    }

    fn command(&mut self, run: impl FnOnce(&mut App) -> Result<(), orqwa_app::AppError>) -> bool {
        let Some(app) = self.app.as_mut() else {
            return false;
        };
        match run(app) {
            Ok(()) => true,
            Err(error) => {
                self.report_failure(&Failure::from(error));
                false
            }
        }
    }

    fn points(
        &self,
        max_points: i64,
        pick: fn(&orqwa_routes::Route, usize) -> Vec<(f64, f64)>,
    ) -> PackedVector2Array {
        let Some(route) = self.app.as_ref().and_then(App::route) else {
            return PackedVector2Array::new();
        };
        let max_points = usize::try_from(max_points).unwrap_or(2);
        pick(route, max_points)
            .into_iter()
            .map(|(a, b)| vector2(a, b))
            .collect()
    }

    /// Signals what was found out about updates (R99).
    fn emit_update(&mut self, event: AppEvent) {
        match event {
            AppEvent::UpdateAvailable {
                version,
                page,
                installable,
            } => {
                self.signals().update_available().emit(
                    &GString::from(version.as_str()),
                    &GString::from(page.as_str()),
                    installable,
                );
            }
            AppEvent::UpToDate => self.signals().up_to_date().emit(),
            AppEvent::UpdateDownloaded => self.signals().update_downloaded().emit(),
            AppEvent::UpdateFailed(failure) => {
                let summary = self.record_failure(&failure);
                self.signals().update_failed().emit(&summary);
            }
            _ => {}
        }
    }

    /// Signals the outcome of upload work (R29).
    fn emit_upload(&mut self, event: AppEvent) {
        match event {
            AppEvent::AccountConnected {
                rider,
                platform,
                label,
            } => {
                self.signals().account_connected().emit(
                    &GString::from(rider.as_str()),
                    &GString::from(platform.as_str()),
                    &GString::from(label.as_str()),
                );
            }
            AppEvent::AccountFailed {
                rider,
                platform,
                message,
            } => {
                let summary = self.record_failure(&message);
                self.signals().account_failed().emit(
                    &GString::from(rider.as_str()),
                    &GString::from(platform.as_str()),
                    &summary,
                );
            }
            AppEvent::OpenUrl(url) => {
                self.signals().open_url().emit(&GString::from(url.as_str()));
            }
            AppEvent::UploadFinished { fit, platform, url } => {
                let path = fit.display().to_string();
                self.signals().upload_finished().emit(
                    &GString::from(path.as_str()),
                    &GString::from(platform.as_str()),
                    &GString::from(url.as_str()),
                );
            }
            AppEvent::UploadFailed {
                fit,
                platform,
                message,
                reconnect,
            } => {
                let path = fit.display().to_string();
                let summary = self.record_failure(&message);
                self.signals().upload_failed().emit(
                    &GString::from(path.as_str()),
                    &GString::from(platform.as_str()),
                    &summary,
                    reconnect,
                );
            }
            AppEvent::PlannedWorkouts { platform, day } => {
                self.signals().planned_workouts().emit(
                    &GString::from(platform.as_str()),
                    &GString::from(day.as_str()),
                );
            }
            AppEvent::PlannedWorkoutsFailed { platform, message } => {
                let summary = self.record_failure(&message);
                self.signals()
                    .planned_workouts_failed()
                    .emit(&GString::from(platform.as_str()), &summary);
            }
            _ => {}
        }
    }

    // One arm per event, the bigger groups already handed to helpers.
    #[allow(clippy::too_many_lines)]
    fn emit(&mut self, event: AppEvent) {
        match event {
            AppEvent::DevicesFound(devices) => {
                self.signals().devices_found().emit(&device_array(devices));
            }
            AppEvent::RouteLoaded(route) => self.signals().route_loaded().emit(&route_dict(&route)),
            AppEvent::LoadProgress { stage, done, total } => {
                self.signals().loading_progress().emit(
                    &GString::from(stage.label()),
                    &GString::from(stage.unit()),
                    i64::try_from(done).unwrap_or(i64::MAX),
                    i64::try_from(total).unwrap_or(i64::MAX),
                );
            }
            AppEvent::WorldReady {
                chunks,
                fallback_samples,
            } => {
                let info = vdict! {
                    "chunks" => i64::try_from(chunks).unwrap_or(0),
                    "fallback_samples" => i64::try_from(fallback_samples).unwrap_or(0),
                };
                self.signals().world_ready().emit(&info);
            }
            AppEvent::Connected(name) => {
                self.signals()
                    .device_connected()
                    .emit(&GString::from(name.as_str()));
            }
            AppEvent::Disconnected(name) => {
                self.signals()
                    .device_disconnected()
                    .emit(&GString::from(name.as_str()));
            }
            AppEvent::ConnectionFailed { name, reason } => {
                self.signals().device_failed().emit(
                    &GString::from(name.as_str()),
                    &GString::from(reason.as_str()),
                );
            }
            AppEvent::RideFinished => self.signals().ride_finished().emit(),
            AppEvent::RideSaved(path) => {
                let path = path.display().to_string();
                self.signals()
                    .ride_saved()
                    .emit(&GString::from(path.as_str()));
            }
            event @ (AppEvent::AccountConnected { .. }
            | AppEvent::AccountFailed { .. }
            | AppEvent::OpenUrl(_)
            | AppEvent::UploadFinished { .. }
            | AppEvent::UploadFailed { .. }
            | AppEvent::PlannedWorkouts { .. }
            | AppEvent::PlannedWorkoutsFailed { .. }) => self.emit_upload(event),
            event @ (AppEvent::UpdateAvailable { .. }
            | AppEvent::UpToDate
            | AppEvent::UpdateDownloaded
            | AppEvent::UpdateFailed(_)) => self.emit_update(event),
            AppEvent::CourseAdded(path) => {
                let path = path.display().to_string();
                self.signals()
                    .course_added()
                    .emit(&GString::from(path.as_str()));
            }
            AppEvent::CoursesChanged => self.signals().courses_changed().emit(),
            AppEvent::Moment(moment) => self.signals().moment().emit(&moment_dict(&moment)),
            AppEvent::RememberedMissing(names) => {
                let names: PackedStringArray =
                    names.iter().map(|n| GString::from(n.as_str())).collect();
                self.signals().remembered_missing().emit(&names);
            }
            AppEvent::Control(control) => {
                self.signals()
                    .control_requested()
                    .emit(&GString::from(control_name(control)));
            }
            AppEvent::Navigate(step) => {
                self.signals()
                    .navigation_requested()
                    .emit(&GString::from(step.name()));
            }
            AppEvent::Error(message) => {
                self.report_failure(&message);
            }
        }
    }
}

/// How the front end calls `control`: the music's as `control_music` does.
fn control_name(control: Control) -> &'static str {
    match control {
        Control::NextCamera => "next_camera",
        Control::Overlay => "overlay",
        Control::PlayPause => "play_pause",
        Control::NextTrack => "next",
        Control::PreviousTrack => "previous",
        Control::Pause => "pause",
        Control::DifficultyUp => "difficulty_up",
        Control::DifficultyDown => "difficulty_down",
        Control::IntensityUp => "intensity_up",
        Control::IntensityDown => "intensity_down",
        Control::SkipStep => "skip_step",
        Control::FinishRide => "finish_ride",
    }
}

/// Live HUD values as `{id: value or null}`.
fn hud_values(app: &App) -> VarDictionary {
    let mut values = VarDictionary::new();
    for (id, value) in app.hud_values() {
        values.set(id, &value.map_or_else(Variant::nil, |v| v.to_variant()));
    }
    values
}

/// Zone bounds as percentages, for the rider settings.
fn percent(bounds: &[f64]) -> PackedFloat64Array {
    bounds.iter().map(|b| b * 100.0).collect()
}

/// Zone bounds from `data[key]` in percent, as shares; empty where missing.
fn shares(data: &VarDictionary, key: &str) -> Vec<f64> {
    data.get(key)
        .and_then(|v| v.try_to::<PackedFloat64Array>().ok())
        .map(|a| a.as_slice().iter().map(|p| p / 100.0).collect())
        .unwrap_or_default()
}

/// Preview points as Godot vectors.
/// A workout's state for `ride_state()`.
fn workout_state(w: &orqwa_session::workout::WorkoutState) -> Variant {
    let optional = |value: Option<f64>| value.map_or_else(Variant::nil, |v| v.to_variant());
    vdict! {
        "target_power_w" => &optional(w.target_power.map(|p| p.0)),
        "target_heart_rate" => &optional(w.target_heart_rate.map(|h| h.0)),
        "intensity_pct" => w.intensity.0,
        "progress" => &w.progress.as_ref().map_or_else(Variant::nil, |p| {
            vdict! {
                "step" => i64::try_from(p.step).unwrap_or(0),
                "steps" => i64::try_from(p.steps).unwrap_or(0),
                "elapsed_s" => p.elapsed.as_secs_f64(),
                "step_left_s" => p.step_left.as_secs_f64(),
                "left_s" => p.left.as_secs_f64(),
                "cadence" => &optional(p.cadence.map(|c| c.0)),
                "next" => &p.next.map_or_else(Variant::nil, |n| {
                    vdict! {
                        "power_w" => &optional(n.power.map(|w| w.0)),
                        "duration_s" => n.duration.as_secs_f64(),
                        "all_out" => n.all_out,
                    }
                    .to_variant()
                }),
                "cue" => p.cue.as_deref().unwrap_or_default(),
                "all_out" => p.all_out,
            }
            .to_variant()
        }),
    }
    .to_variant()
}

/// A workout from the editor, as `save_workout()` takes it.
fn plan_from(workout: &VarDictionary) -> Plan {
    let text = |dict: &VarDictionary, key: &str| {
        dict.get(key)
            .and_then(|v| v.try_to::<GString>().ok())
            .map(|t| t.to_string())
            .unwrap_or_default()
    };
    let number = |dict: &VarDictionary, key: &str| {
        dict.get(key)
            .and_then(|v| {
                v.try_to::<f64>()
                    .ok()
                    .or_else(|| v.try_to::<i32>().ok().map(f64::from))
            })
            .unwrap_or(0.0)
    };
    let mut plan = Plan {
        name: text(workout, "name").trim().to_owned(),
        description: text(workout, "description").trim().to_owned(),
        ..Plan::default()
    };
    let steps = workout
        .get("steps")
        .and_then(|v| v.try_to::<VarArray>().ok())
        .unwrap_or_default();
    for step in steps.iter_shared() {
        let Ok(step) = step.try_to::<VarDictionary>() else {
            continue;
        };
        let free = step
            .get("free")
            .and_then(|v| v.try_to::<bool>().ok())
            .unwrap_or(false);
        let target = if free {
            Target::Free
        } else {
            Target::Power {
                from: Intensity::Ftp(number(&step, "from_pct") / 100.0),
                to: Intensity::Ftp(number(&step, "to_pct") / 100.0),
            }
        };
        let message = text(&step, "message").trim().to_owned();
        if !message.is_empty() {
            plan.cues.push(Cue {
                at: plan.duration(),
                text: message,
            });
        }
        let cadence = number(&step, "cadence");
        plan.steps.push(Step {
            duration: Duration::from_secs_f64(number(&step, "duration_s").max(0.0)),
            target,
            cadence: (cadence > 0.0).then_some(Rpm(cadence)),
        });
    }
    plan
}

/// A workout as `start_workout()` takes it; heart-rate holds use `profile`'s zones and figures.
fn workout_from(workout: &VarDictionary, profile: &Profile) -> Workout {
    // GDScript hands whole numbers (e.g. the zone) over as ints.
    let number = |key: &str| {
        workout
            .get(key)
            .and_then(|v| {
                v.try_to::<f64>()
                    .ok()
                    .or_else(|| v.try_to::<i32>().ok().map(f64::from))
            })
            .unwrap_or(0.0)
    };
    let kind = workout
        .get("kind")
        .and_then(|v| v.try_to::<GString>().ok())
        .map(|k| k.to_string())
        .unwrap_or_default();
    let (min, max) = (Watts(number("min_w")), Watts(number("max_w")));
    match kind.as_str() {
        "zone" => {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // zones are 1–5
            let zone = number("zone").clamp(1.0, 5.0) as u8;
            Workout::HeartRate(HeartRateHold::zone(profile, zone, min, max))
        }
        "ftp_test" => {
            let test = workout
                .get("test")
                .and_then(|v| v.try_to::<GString>().ok())
                .map(|t| t.to_string())
                .unwrap_or_default();
            ftp_test(&test, profile.ftp)
        }
        "bpm" => Workout::HeartRate(HeartRateHold::bpm(
            profile,
            BeatsPerMinute(number("bpm")),
            min,
            max,
        )),
        _ => Workout::ConstantPower(Watts(number("power_w"))),
    }
}

/// The FTP test (R22) called `test` for a rider with `ftp`: `twenty_minutes` or
/// `two_by_eight` (#125), else the ramp test.
fn ftp_test(test: &str, ftp: Watts) -> Workout {
    match test {
        "twenty_minutes" => Workout::EffortTest(EffortTest::twenty_minutes(ftp)),
        "two_by_eight" => Workout::EffortTest(EffortTest::two_by_eight(ftp)),
        _ => Workout::RampTest(RampTest::for_ftp(ftp)),
    }
}

/// A plan's steps for the front end, as `workouts()` lists them, for a rider with `ftp`; in an
/// FTP test (`test`) the free steps are marked `all_out`.
fn plan_steps(plan: &Plan, ftp: Watts, test: bool) -> VarArray {
    let mut steps = VarArray::new();
    let mut start = Duration::ZERO;
    for step in &plan.steps {
        let percent = |watts: f64| (watts / ftp.0.max(1.0) * 100.0).to_variant();
        let (from, to, from_pct, to_pct) = match step.target {
            Target::Power { from, to } => {
                let (a, b) = (from.watts(ftp).0, to.watts(ftp).0);
                (a.to_variant(), b.to_variant(), percent(a), percent(b))
            }
            Target::Free => (
                Variant::nil(),
                Variant::nil(),
                Variant::nil(),
                Variant::nil(),
            ),
        };
        let end = start + step.duration;
        let message = plan
            .cues
            .iter()
            .find(|cue| cue.at >= start && cue.at < end)
            .map_or("", |cue| cue.text.as_str());
        steps.push(
            &vdict! {
                "duration_s" => step.duration.as_secs_f64(),
                "from_w" => &from,
                "to_w" => &to,
                "from_pct" => &from_pct,
                "to_pct" => &to_pct,
                "cadence" => &step.cadence.map_or_else(Variant::nil, |c| c.0.to_variant()),
                "message" => message,
                "all_out" => test && step.target == Target::Free,
            }
            .to_variant(),
        );
        start = end;
    }
    steps
}

/// Names of choices for the front end.
fn name_array(names: impl Iterator<Item = &'static str>) -> PackedStringArray {
    names.map(GString::from).collect()
}

/// A loaded route for `route_loaded`.
fn route_dict(route: &orqwa_app::RouteSummary) -> VarDictionary {
    let source = match route.elevation_source {
        ElevationSource::Terrain => "terrain",
        ElevationSource::File => "file",
    };
    vdict! {
        "name" => route.name.as_str(),
        "length_m" => route.length,
        "elevation_gain_m" => route.elevation_gain,
        "max_grade" => route.max_grade,
        "elevation_source" => source,
    }
}

/// The interface language, e.g. `de` or `en_US`.
fn locale() -> String {
    TranslationServer::singleton().get_locale().to_string()
}

/// A country named only by its `code`, by Godot's name for it (in English); the code itself
/// if Godot knows none.
fn country_name_or_godots(code: &str) -> String {
    let name = TranslationServer::singleton().get_country_name(code);
    if name.is_empty() {
        code.to_owned()
    } else {
        name.to_string()
    }
}

/// A course for the front end, as `OrqwaApp.courses()` lists it, its countries named in
/// `locale`.
fn course_dict(course: &orqwa_app::CourseEntry, locale: &str) -> VarDictionary {
    let path = course.path.display().to_string();
    let mut countries = VarArray::new();
    for country in course.manifest.countries.iter().flatten() {
        let name = library::country_name(country, locale);
        let name = if name == country.code {
            country_name_or_godots(name)
        } else {
            name.to_owned()
        };
        countries.push(
            &vdict! {
                "code" => country.code.as_str(),
                "name" => name.as_str(),
            }
            .to_variant(),
        );
    }
    let video = course
        .manifest
        .video
        .as_ref()
        .map_or("", |v| v.file_name.as_str());
    vdict! {
        "path" => path.as_str(),
        "name" => course.manifest.name.as_str(),
        "length_m" => course.manifest.length_m,
        "elevation_gain_m" => course.manifest.elevation_gain_m,
        "max_grade" => course.manifest.max_grade_percent,
        "created_unix_s" => i64::try_from(course.manifest.created_unix_s).unwrap_or(0),
        "track" => &points(&course.manifest.track),
        "profile" => &points(&course.manifest.profile),
        "video" => video,
        // Tacx RLV courses are known only by distance and slope: no 3D world.
        "located" => course.manifest.video.as_ref().is_none_or(|v| v.located),
        "countries" => &countries,
    }
}

fn points(points: &[[f32; 2]]) -> PackedVector2Array {
    points.iter().map(|&[x, y]| Vector2::new(x, y)).collect()
}

/// A device kind as GDScript knows it.
fn kind_name(kind: DeviceKind) -> &'static str {
    match kind {
        DeviceKind::Trainer => "trainer",
        DeviceKind::HeartRateSensor => "heart_rate",
        DeviceKind::PowerMeter => "power_meter",
        DeviceKind::CadenceSensor => "cadence_sensor",
        DeviceKind::Controller => "controller",
    }
}

/// The device kind GDScript names, if any.
fn kind_of(name: &str) -> Option<DeviceKind> {
    [
        DeviceKind::Trainer,
        DeviceKind::HeartRateSensor,
        DeviceKind::PowerMeter,
        DeviceKind::CadenceSensor,
        DeviceKind::Controller,
    ]
    .into_iter()
    .find(|&kind| kind_name(kind) == name)
}

/// Scanned devices as `[{index, name, kind, model, rssi, remembered}]` for `devices_found`.
fn device_array(devices: Vec<orqwa_app::DeviceInfo>) -> VarArray {
    let mut array = VarArray::new();
    for device in devices {
        let kind = kind_name(device.kind);
        let model = device.model.map_or("", ControllerModel::name);
        let rssi = device
            .rssi
            .map_or_else(Variant::nil, |rssi| i64::from(rssi).to_variant());
        let index = i64::try_from(device.index).unwrap_or(-1);
        array.push(
            &vdict! {
                "index" => index,
                "name" => device.name.as_str(),
                "kind" => kind,
                "model" => model,
                "rssi" => &rssi,
                "remembered" => device.remembered,
            }
            .to_variant(),
        );
    }
    array
}

/// A history entry's climb times: `[{start_m, end_m, length_m, time_s, avg_power, record}]`.
fn climb_times(entry: &orqwa_app::HistoryEntry) -> VarArray {
    let mut array = VarArray::new();
    for (climb, record) in entry.record.climbs.iter().zip(&entry.climb_records) {
        let power = climb
            .avg_power
            .map_or_else(Variant::nil, |p| p.0.to_variant());
        array.push(
            &vdict! {
                "start_m" => climb.start.0,
                "end_m" => climb.end.0,
                "length_m" => climb.end.0 - climb.start.0,
                "time_s" => climb.elapsed.as_secs_f64(),
                "avg_power" => &power,
                "record" => *record,
            }
            .to_variant(),
        );
    }
    array
}

/// The page of a ride's activity, by platform id.
fn uploads_dict(entry: &orqwa_app::HistoryEntry) -> VarDictionary {
    let mut dict = VarDictionary::new();
    for (platform, upload) in &entry.record.uploads {
        dict.set(platform.as_str(), upload.url.as_str());
    }
    dict
}

/// Converts mesh data to the arrays Godot's `ArrayMesh` takes; `colors` is empty when the
/// mesh has none.
fn mesh_arrays(mesh: &orqwa_world::MeshData) -> VarDictionary {
    let vertices: PackedVector3Array = mesh
        .vertices
        .iter()
        .map(|&[x, y, z]| Vector3::new(x, y, z))
        .collect();
    let normals: PackedVector3Array = mesh
        .normals
        .iter()
        .map(|&[x, y, z]| Vector3::new(x, y, z))
        .collect();
    let uvs: PackedVector2Array = mesh.uvs.iter().map(|&[u, v]| Vector2::new(u, v)).collect();
    let colors: PackedColorArray = mesh
        .colors
        .iter()
        .map(|&[r, g, b, a]| Color::from_rgba(r, g, b, a))
        .collect();
    let indices: PackedInt32Array = mesh
        .indices
        .iter()
        .map(|&i| i32::try_from(i).unwrap_or(0))
        .collect();
    vdict! {
        "vertices" => &vertices,
        "normals" => &normals,
        "uvs" => &uvs,
        "colors" => &colors,
        "indices" => &indices,
    }
}

/// The last `days` before now as the analysis takes a period (R62); all time for 0 or less.
fn period_of(days: i64) -> Option<Duration> {
    u64::try_from(days)
        .ok()
        .filter(|&days| days > 0)
        .map(|days| Duration::from_hours(days * 24))
}

/// Seconds since 1970 of `time`, as the history gives start times.
fn unix_seconds(time: std::time::SystemTime) -> i64 {
    time.duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

// Godot vectors are f32; metre precision is ample for drawing.
#[allow(clippy::cast_possible_truncation)]
fn vector2(x: f64, y: f64) -> Vector2 {
    Vector2::new(x as f32, y as f32)
}

/// A decoded video frame as a Godot RGBA8 image.
fn image(frame: orqwa_app::Frame) -> Option<Gd<Image>> {
    let width = i32::try_from(frame.width).ok()?;
    let height = i32::try_from(frame.height).ok()?;
    let data = PackedByteArray::from(frame.rgba);
    Image::create_from_data(width, height, false, ImageFormat::RGBA8, &data)
}

/// Seconds from Godot as a duration; negative or invalid values count as zero.
fn seconds(value: f64) -> Duration {
    Duration::try_from_secs_f64(value.max(0.0)).unwrap_or_default()
}

/// Sync marks from Godot: x metres along the route, y seconds into the video.
fn sync_marks(marks: &PackedVector2Array) -> Vec<orqwa_app::SyncMark> {
    marks
        .as_slice()
        .iter()
        .map(|m| orqwa_app::SyncMark {
            distance: Meters(f64::from(m.x)),
            time: seconds(f64::from(m.y)),
        })
        .collect()
}

/// A flat map's triangles and their colours in `style`, for an `ArrayMesh`.
fn flat_map_arrays(
    flat: &orqwa_world::FlatMap,
    style: MapStyle,
) -> (PackedVector2Array, PackedColorArray) {
    let vertices = flat
        .vertices
        .iter()
        .map(|&[x, y]| Vector2::new(x, y))
        .collect();
    let colors = flat.features.iter().map(|&f| map_color(f, style)).collect();
    (vertices, colors)
}

/// A course map's rivers and roads as `[{points, width, color}]`, coloured in `style`.
fn map_lines(lines: &[orqwa_world::course_map::Line], style: MapStyle) -> VarArray {
    let mut array = VarArray::new();
    for line in lines {
        let points: PackedVector2Array = line
            .points
            .iter()
            .map(|&[x, y]| Vector2::new(x, y))
            .collect();
        array.push(
            &vdict! {
                "points" => &points,
                "width" => line.width,
                "color" => map_color(line.feature, style),
            }
            .to_variant(),
        );
    }
    array
}

fn map_color(feature: MapFeature, style: MapStyle) -> Color {
    let [r, g, b, a] = feature.color(style);
    Color::from_rgba(r, g, b, a)
}

/// A rider as `profile()` describes them: `{id, name, rider_mass_kg, bike_mass_kg, ftp_w,
/// max_heart_rate_bpm, units, language, avatar, drivetrain, chainring, cog, power_zones_pct,
/// heart_rate_zones_pct, default_difficulty_pct, map_style, voice, rides_per_week}` with `map_style`
/// either `"coloured"` or `"dark"`, `voice` (R124) `"casual"`, `"plain"` or `"off"` and
/// `rides_per_week` 0 for none.
fn profile_dict(id: &str, p: &Profile) -> VarDictionary {
    vdict! {
        "id" => id,
        "name" => p.name.as_str(),
        "rider_mass_kg" => p.rider_mass.0,
        "bike_mass_kg" => p.bike_mass.0,
        "ftp_w" => p.ftp.0,
        "max_heart_rate_bpm" => p.max_heart_rate.0,
        "units" => match p.units {
            UnitSystem::Metric => "metric",
            UnitSystem::Imperial => "imperial",
        },
        "language" => p.language.as_str(),
        "avatar" => match p.avatar {
            Avatar::Female => "female",
            Avatar::Male => "male",
        },
        "drivetrain" => match p.drivetrain {
            Drivetrain::Cassette => "cassette",
            Drivetrain::SingleCog { .. } => "single_cog",
        },
        "chainring" => match p.drivetrain {
            Drivetrain::SingleCog { chainring, .. } => i64::from(chainring),
            Drivetrain::Cassette => 50,
        },
        "cog" => match p.drivetrain {
            Drivetrain::SingleCog { cog, .. } => i64::from(cog),
            Drivetrain::Cassette => 14,
        },
        "power_zones_pct" => &percent(&p.power_zones),
        "heart_rate_zones_pct" => &percent(&p.heart_rate_zones),
        "default_difficulty_pct" => p.default_difficulty.0,
        "map_style" => match p.map_style {
            MapStyle::Coloured => "coloured",
            MapStyle::Dark => "dark",
        },
        "rides_per_week" => i64::from(p.rides_per_week.unwrap_or(0)),
        "voice" => match p.voice {
            Voice::Casual => "casual",
            Voice::Plain => "plain",
            Voice::Off => "off",
        },
    }
}

/// The voice a dictionary shaped like `profile()` names (R124), the casual one by default.
fn voice_of(data: &VarDictionary) -> Voice {
    match data
        .get("voice")
        .and_then(|v| v.try_to::<GString>().ok())
        .map(|v| v.to_string())
        .as_deref()
    {
        Some("plain") => Voice::Plain,
        Some("off") => Voice::Off,
        _ => Voice::Casual,
    }
}

/// A moment of a ride (R124) as `{kind, …}`: `climb_topped {index, elapsed_s, grade_pct}`,
/// `climb_record {index, elapsed_s, gain_s}`, `halfway {distance_m}`, `one_to_go`,
/// `distance {distance_m}`, `power_peak {duration_s, power_w}`, `step_held {index, power_w}`,
/// `first_ride {course}`, `streak {rides}` or `week_plan {rides, planned}`; `index` counts climbs from 0, as `climbs()` does.
fn moment_dict(moment: &Moment) -> VarDictionary {
    let index = |i: usize| i64::try_from(i).unwrap_or(0);
    match *moment {
        Moment::StepHeld { index: i, power } => vdict! {
            "kind" => "step_held",
            "index" => index(i),
            "power_w" => power.0,
        },
        Moment::Streak { rides } => vdict! {
            "kind" => "streak",
            "rides" => i64::from(rides),
        },
        Moment::WeekPlan { ride, planned } => vdict! {
            "kind" => "week_plan",
            "rides" => i64::from(ride),
            "planned" => i64::from(planned),
        },
        Moment::ClimbTopped {
            index: i,
            elapsed,
            grade,
        } => vdict! {
            "kind" => "climb_topped",
            "index" => index(i),
            "elapsed_s" => elapsed.as_secs_f64(),
            "grade_pct" => grade.0,
        },
        Moment::ClimbRecord {
            index: i,
            elapsed,
            gain,
        } => vdict! {
            "kind" => "climb_record",
            "index" => index(i),
            "elapsed_s" => elapsed.as_secs_f64(),
            "gain_s" => gain.as_secs_f64(),
        },
        Moment::Halfway { distance } => vdict! {
            "kind" => "halfway",
            "distance_m" => distance.0,
        },
        Moment::OneToGo => vdict! { "kind" => "one_to_go" },
        Moment::Distance(distance) => vdict! {
            "kind" => "distance",
            "distance_m" => distance.0,
        },
        Moment::PowerPeak { duration, power } => vdict! {
            "kind" => "power_peak",
            "duration_s" => duration.as_secs_f64(),
            "power_w" => power.0,
        },
        Moment::FirstRide { ref course } => vdict! {
            "kind" => "first_ride",
            "course" => course.as_str(),
        },
    }
}

/// Substitute only template slots: a filename containing `%s` must remain untouched.
fn format_failure(template: &str, args: &[String]) -> String {
    let mut parts = template.split("%s");
    let mut result = parts.next().unwrap_or_default().to_owned();
    for (index, part) in parts.enumerate() {
        result.push_str(args.get(index).map_or("%s", String::as_str));
        result.push_str(part);
    }
    result
}

#[cfg(test)]
mod build_tests {
    use super::{BRANCH, COMMIT, build_name};

    #[test]
    fn the_commit_is_abbreviated_or_unknown() {
        assert!(
            COMMIT.is_empty()
                || (COMMIT.len() == 7 && COMMIT.chars().all(|c| c.is_ascii_hexdigit())),
            "{COMMIT}"
        );
    }

    #[test]
    fn the_branch_is_named_or_unknown() {
        assert!(
            BRANCH != "HEAD" && !BRANCH.chars().any(char::is_whitespace),
            "{BRANCH}"
        );
    }

    #[test]
    fn the_build_is_named_by_branch_and_commit() {
        assert_eq!(build_name("main", "90b74ef"), "main@90b74ef");
        assert_eq!(
            build_name("fix/sidewalk-continuity", "ff2ee14"),
            "fix/sidewalk-continuity@ff2ee14"
        );
        assert_eq!(
            build_name("", "90b74ef"),
            "90b74ef",
            "a release from its tag"
        );
        assert_eq!(build_name("main", ""), "", "a source archive");
        assert_eq!(build_name("", ""), "");
    }
}

#[cfg(test)]
mod failure_tests {
    use super::format_failure;

    #[test]
    fn substitution_never_interprets_a_filename_as_another_placeholder() {
        assert_eq!(
            format_failure("Could not open %s.", &["/Räder/100%/%s.fit".to_owned()]),
            "Could not open /Räder/100%/%s.fit."
        );
    }
}

#[cfg(test)]
mod sample_course_tests {
    use std::time::{Duration, Instant};

    use orqwa_app::{App, AppEvent};
    use orqwa_routes::ElevationSource;

    use super::{SAMPLE_COURSE, SAMPLE_COURSE_FILE};

    /// Runs `app` until `done` accepts an event; panics on an error or after `limit`.
    fn run_until(app: &mut App, limit: Duration, done: impl Fn(&AppEvent) -> bool) -> AppEvent {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(16));
            for event in app.update(Duration::from_millis(16)) {
                assert!(!matches!(event, AppEvent::Error(_)), "{event:?}");
                if done(&event) {
                    return event;
                }
            }
        }
        panic!("timed out");
    }

    #[test]
    fn the_sample_course_rides_on_its_own_data() {
        let dir = std::env::temp_dir().join(format!("orqwa-gd-sample-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // An empty cache: the terrain and map must come from the course file.
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();

        assert!(
            app.add_sample_course(SAMPLE_COURSE_FILE, SAMPLE_COURSE)
                .unwrap()
        );
        let courses = app.courses();
        assert_eq!(courses.len(), 1);
        assert_eq!(courses[0].manifest.name, "Twann – Chasseral");

        app.open_course(courses[0].path.clone());
        let limit = Duration::from_secs(300);
        let loaded = run_until(&mut app, limit, |e| matches!(e, AppEvent::RouteLoaded(_)));
        let AppEvent::RouteLoaded(summary) = loaded else {
            unreachable!()
        };
        assert_eq!(summary.elevation_source, ElevationSource::Terrain);
        app.build_world();
        let ready = |e: &AppEvent| matches!(e, AppEvent::WorldReady { .. });
        let AppEvent::WorldReady {
            fallback_samples, ..
        } = run_until(&mut app, limit, ready)
        else {
            unreachable!()
        };
        assert_eq!(fallback_samples, 0, "terrain missing from the course");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Android loads ORQWA's library from Java first (ADR 0024: the APK's plugin does, before Godot
/// opens it as an extension), on a thread with the app's class loader: the one moment Bluetooth
/// can find btleplug's Java classes.
#[cfg(target_os = "android")]
#[allow(non_snake_case)] // the name Android looks for
#[unsafe(no_mangle)]
pub extern "system" fn JNI_OnLoad(
    vm: *mut orqwa_devices::android::sys::JavaVM,
    _reserved: *mut std::ffi::c_void,
) -> orqwa_devices::android::sys::jint {
    // SAFETY: Android passes its Java VM to JNI_OnLoad.
    match unsafe { orqwa_devices::android::init(vm) } {
        Ok(()) => orqwa_devices::android::log("Bluetooth ready"),
        Err(error) => orqwa_devices::android::log(&format!("Bluetooth not ready: {error}")),
    }
    orqwa_devices::android::sys::JNI_VERSION_1_6
}

/// Called once by the APK's plugin with the app's context (ADR 0024): HTTPS checks certificates
/// through Android, which needs it before the first download (map tiles, uploads, updates).
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_orqwa_android_OrqwaAndroid_initTls<'caller>(
    mut env: orqwa_devices::android::jni::EnvUnowned<'caller>,
    _class: orqwa_devices::android::jni::objects::JClass<'caller>,
    context: orqwa_devices::android::jni::objects::JObject<'caller>,
) {
    use orqwa_devices::android::jni::Outcome;
    let outcome = env
        .with_env(|env| rustls_platform_verifier::android::init_with_env(env, context))
        .into_outcome();
    orqwa_devices::android::log(&match outcome {
        Outcome::Ok(()) => "HTTPS ready".to_owned(),
        Outcome::Err(error) => format!("HTTPS not ready: {error}"),
        Outcome::Panic(_) => "HTTPS not ready: the set-up panicked".to_owned(),
    });
}
