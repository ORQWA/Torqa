//! The courses' worlds this computer keeps (R101, ADR 0025): once a library course's world is
//! built it is kept in the cache, and **Ride** reads it instead of building it again. Off until
//! the rider turns it on, as a world takes about 10 MB per kilometre of route. A kept world
//! stays across updates; one an older generator built is ridden until the rider has it built
//! again ([`App::regenerate_world`]).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use orqwa_storage::computer;
use orqwa_storage::course;
use orqwa_storage::worlds::KeptWorlds;
use orqwa_world::{GENERATOR, World};
use tracing::{info, warn};

use crate::failure::{Failure, Kind};
use crate::{App, AppError, CourseEntry};

/// A world this computer keeps, as the settings list it to delete (R136).
#[derive(Debug, Clone, PartialEq)]
pub struct KeptWorld {
    /// The library's courses it is the world of, by name: one, unless the same course is in
    /// the library twice.
    pub courses: Vec<CourseEntry>,
    /// How many bytes it takes on disk.
    pub size: u64,
}

/// What the cache holds of a course's world.
pub(crate) enum Found {
    /// Its world, kept, and the version of the generator that built it.
    World(Box<World>, u32),
    /// None to ride: one built is kept under this fingerprint.
    Missing(String),
    /// Nothing that can be told: the course file cannot be read.
    Unknown,
}

/// Where kept worlds are: the cache, the setting to check once one is written (it may have
/// been turned off meanwhile) and the library whose courses' worlds stay.
pub(crate) struct Keeper {
    kept: KeptWorlds,
    computer: PathBuf,
    library: PathBuf,
}

impl Keeper {
    /// The kept world of the library course at `course`, if there is one. One damaged or of
    /// another format is built again.
    pub(crate) fn find(&self, course: &Path) -> Found {
        let fingerprint = match course::fingerprint(course) {
            Ok(fingerprint) => fingerprint,
            Err(error) => {
                warn!(%error, course = %course.display(), "cannot tell the course's kept world");
                return Found::Unknown;
            }
        };
        match self.kept.read(&fingerprint) {
            Ok(Some((world, generator))) => {
                info!(
                    chunks = world.chunks.len(),
                    generator, "riding the kept world"
                );
                Found::World(Box::new(world), generator)
            }
            Ok(None) => Found::Missing(fingerprint),
            Err(error) => {
                warn!(%error, "the kept world cannot be read; building it again");
                Found::Missing(fingerprint)
            }
        }
    }

    /// Keeps `world`, built by the generator of version `generator`, as that of the course at
    /// `course`, found by `fingerprint` (that of a world just built) or else by the file, and
    /// drops the worlds no course of the library has any more. Blocks for seconds: the world is
    /// deflated.
    pub(crate) fn keep(
        &self,
        course: &Path,
        world: &World,
        generator: u32,
        fingerprint: Option<String>,
    ) {
        let fingerprint = match fingerprint {
            Some(fingerprint) => fingerprint,
            None => match course::fingerprint(course) {
                // Turned on again while it is still kept: writing it again only costs time.
                Ok(fingerprint) if self.kept.generator(&fingerprint) == Some(generator) => {
                    return self.tidy();
                }
                Ok(fingerprint) => fingerprint,
                Err(error) => {
                    warn!(%error, course = %course.display(), "cannot keep the course's world");
                    return;
                }
            },
        };
        if let Err(error) = self.kept.write(&fingerprint, world, generator) {
            warn!(%error, "cannot keep the course's world");
        }
        self.tidy();
    }

    /// Drops the worlds of courses gone from the library and those this ORQWA cannot read, or
    /// all of them once keeping them is turned off.
    pub(crate) fn tidy(&self) {
        let tidied = if computer::keep_worlds(&self.computer) {
            self.kept.keep_only(&library_fingerprints(&self.library))
        } else {
            self.kept.clear()
        };
        if let Err(error) = tidied {
            warn!(%error, "cannot tidy the kept worlds");
        }
    }
}

/// The fingerprints of the courses in `library`.
fn library_fingerprints(library: &Path) -> HashSet<String> {
    std::fs::read_dir(library)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == course::EXTENSION))
        .filter_map(|path| course::fingerprint(&path).ok())
        .collect()
}

impl App {
    /// Whether this computer keeps the courses' worlds once built (R101); no until turned on.
    #[must_use]
    pub fn keeps_worlds(&self) -> bool {
        computer::keep_worlds(&self.computer)
    }

    /// Turns keeping the courses' worlds on or off for this computer. On, the world of the
    /// course loaded is kept at once; off, every kept world is deleted.
    ///
    /// # Errors
    /// [`AppError::Reported`] if it cannot be saved.
    pub fn set_keeps_worlds(&mut self, on: bool) -> Result<(), AppError> {
        computer::set_keep_worlds(&self.computer, on)
            .map_err(|e| AppError::Reported(Failure::new(Kind::SaveSetting, e)))?;
        match (on, &self.world, &self.course) {
            (true, Some(world), Some(course)) => {
                self.keep_world(course.clone(), Arc::clone(world), None);
            }
            _ => self.tidy_kept_worlds(),
        }
        Ok(())
    }

    /// How many bytes the kept worlds take on this computer.
    #[must_use]
    pub fn kept_worlds_size(&self) -> u64 {
        self.keeper().kept.size()
    }

    /// The worlds this computer keeps of the library's courses, by the name of their first
    /// course (R136); none while it keeps none, as turning it off deletes them all.
    #[must_use]
    pub fn kept_worlds(&self) -> Vec<KeptWorld> {
        if !self.keeps_worlds() {
            return Vec::new();
        }
        let kept = self.keeper().kept;
        let mut worlds: Vec<(String, KeptWorld)> = Vec::new();
        for entry in self.courses() {
            let Ok(fingerprint) = course::fingerprint(&entry.path) else {
                continue;
            };
            if let Some((_, world)) = worlds.iter_mut().find(|(f, _)| *f == fingerprint) {
                world.courses.push(entry);
            } else if let Some(size) = kept.size_of(&fingerprint) {
                let courses = vec![entry];
                worlds.push((fingerprint, KeptWorld { courses, size }));
            }
        }
        worlds.into_iter().map(|(_, world)| world).collect()
    }

    /// Deletes the kept world of the course at `course` (R136), and so of every course it is
    /// the world of: the course stays, and its next ride builds and keeps it again.
    ///
    /// # Errors
    /// [`AppError::Reported`] if the world cannot be deleted.
    pub fn delete_kept_world(&self, course: &Path) -> Result<(), AppError> {
        let fingerprint = course::fingerprint(course).map_err(|e| {
            AppError::Reported(Failure::new(Kind::OpenFile, e).arg(course.display()))
        })?;
        let kept = self.keeper().kept;
        kept.remove(&fingerprint).map_err(|e| {
            let file = kept.file(&fingerprint);
            AppError::Reported(Failure::new(Kind::DeleteFile, e).arg(file.display()))
        })
    }

    /// Whether the course at `course` has a world kept that an older generator built, which
    /// [`App::regenerate_world`] builds again. Courses with a video are ridden along it and
    /// never asked.
    #[must_use]
    pub fn kept_world_outdated(&self, course: &Path) -> bool {
        self.keeps_worlds()
            && course::read_manifest(course).is_ok_and(|manifest| manifest.video.is_none())
            && course::fingerprint(course).is_ok_and(|fingerprint| {
                self.keeper()
                    .kept
                    .generator(&fingerprint)
                    .is_some_and(|generator| generator < GENERATOR)
            })
    }

    /// Builds the world of the course loaded again with this ORQWA's generator, in place of the
    /// one kept (R101): the kept one is deleted, and the course opened anew and its world built
    /// and kept as for a ride, reported by [`AppEvent::WorldReady`](crate::AppEvent::WorldReady).
    /// False if no library course is loaded.
    pub fn regenerate_world(&mut self) -> bool {
        let Some(course) = self.course.clone() else {
            return false;
        };
        if let Ok(fingerprint) = course::fingerprint(&course)
            && let Err(error) = self.keeper().kept.remove(&fingerprint)
        {
            warn!(%error, "cannot delete the kept world");
        }
        self.open_course(course);
        self.build_world();
        true
    }

    /// Drops, in the background, the kept worlds of courses gone from the library (also on
    /// another computer sharing it) and those this ORQWA cannot read, or all once keeping them
    /// is off.
    pub fn tidy_kept_worlds(&self) {
        let keeper = self.keeper();
        self.runtime.spawn_blocking(move || keeper.tidy());
    }

    pub(crate) fn keeper(&self) -> Keeper {
        Keeper {
            kept: KeptWorlds::new(&self.cache_dir),
            computer: self.computer.clone(),
            library: self.courses_dir(),
        }
    }

    /// Keeps the loaded `world` as that of the library course at `course`, in the background.
    pub(crate) fn keep_world(
        &self,
        course: PathBuf,
        world: Arc<World>,
        fingerprint: Option<String>,
    ) {
        let keeper = self.keeper();
        let generator = self.world_generator;
        self.runtime
            .spawn_blocking(move || keeper.keep(&course, &world, generator, fingerprint));
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::AppEvent;
    use crate::tests::{run_until, temp_dir, write_route};

    /// Updates `app` until `done` holds, as the kept worlds are written in the background.
    fn wait_for(app: &mut App, mut done: impl FnMut(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !done(app) {
            assert!(Instant::now() < deadline, "waited in vain");
            std::thread::sleep(Duration::from_millis(16));
            app.update(Duration::from_millis(16));
        }
    }

    /// Prepares the test route as a course of `app`'s library.
    fn prepare(app: &mut App, dir: &Path) -> PathBuf {
        prepare_gpx(app, write_route(dir))
    }

    /// Prepares the route of the GPX file at `gpx` as a course of `app`'s library.
    fn prepare_gpx(app: &mut App, gpx: PathBuf) -> PathBuf {
        app.load_route(gpx, true);
        let events = run_until(app, |e| matches!(e, AppEvent::CourseAdded(_)));
        let Some(AppEvent::CourseAdded(course)) = events.last() else {
            unreachable!()
        };
        course.clone()
    }

    /// Opens `course` and builds its world for a ride; how many fallback samples it has, which
    /// tells a world marked in its kept file apart from one built again.
    fn ride(app: &mut App, course: &Path) -> usize {
        app.open_course(course.to_owned());
        app.build_world();
        let events = run_until(app, |e| {
            matches!(e, AppEvent::WorldReady { .. } | AppEvent::Error(_))
        });
        match events.last() {
            Some(AppEvent::WorldReady {
                fallback_samples, ..
            }) => *fallback_samples,
            other => panic!("no world: {other:?}"),
        }
    }

    const MARK: usize = 424_242;

    /// Marks the kept world of the course with `fingerprint`, as built by the generator of
    /// version `generator`.
    fn mark(keeper: &Keeper, fingerprint: &str, generator: u32) {
        let (mut world, _) = keeper.kept.read(fingerprint).unwrap().expect("kept");
        world.fallback_samples = MARK;
        keeper.kept.write(fingerprint, &world, generator).unwrap();
    }

    #[test]
    fn a_course_ridden_again_rides_its_kept_world() {
        let dir = temp_dir("kept-ride");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        assert!(!app.keeps_worlds(), "off until turned on (R101)");
        app.set_keeps_worlds(true).unwrap();
        // Prepared from its GPX: the world built for it is kept for the course.
        let course = prepare(&mut app, &dir);
        let keeper = app.keeper();
        let fingerprint = course::fingerprint(&course).unwrap();
        wait_for(&mut app, |_| keeper.kept.has(&fingerprint));
        mark(&keeper, &fingerprint, GENERATOR);

        assert_eq!(ride(&mut app, &course), MARK, "ridden from the kept world");
        assert!(!app.kept_world_outdated(&course), "built by this generator");
        // Renamed, it is still the same world.
        app.rename_course(&course, "Evening loop").unwrap();
        assert_eq!(ride(&mut app, &course), MARK);

        // Turned off, the kept worlds go and the course is built again.
        app.set_keeps_worlds(false).unwrap();
        wait_for(&mut app, |app| app.kept_worlds_size() == 0);
        assert_ne!(ride(&mut app, &course), MARK);
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(16));
            app.update(Duration::from_millis(16));
        }
        assert_eq!(app.kept_worlds_size(), 0, "and none is kept");
        app.shutdown();
    }

    #[test]
    fn turned_on_the_world_loaded_is_kept_until_its_course_is_deleted() {
        let dir = temp_dir("kept-delete");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        let course = prepare(&mut app, &dir);
        let keeper = app.keeper();
        let fingerprint = course::fingerprint(&course).unwrap();
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(16));
            app.update(Duration::from_millis(16));
        }
        assert_eq!(app.kept_worlds_size(), 0, "nothing kept while off");

        app.set_keeps_worlds(true).unwrap();
        wait_for(&mut app, |_| keeper.kept.has(&fingerprint));
        assert!(app.kept_worlds_size() > 0);

        app.delete_course(&course).unwrap();
        wait_for(&mut app, |_| !keeper.kept.has(&fingerprint));
        app.shutdown();
    }

    #[test]
    fn an_older_generators_world_is_ridden_until_the_rider_has_it_built_again() {
        let dir = temp_dir("kept-older");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        app.set_keeps_worlds(true).unwrap();
        let course = prepare(&mut app, &dir);
        let keeper = app.keeper();
        let fingerprint = course::fingerprint(&course).unwrap();
        wait_for(&mut app, |_| keeper.kept.has(&fingerprint));
        assert!(!app.kept_world_outdated(&course));
        // Kept before an update that changed how worlds look.
        mark(&keeper, &fingerprint, GENERATOR - 1);
        assert!(
            app.kept_world_outdated(&course),
            "the course offers to build it again"
        );

        assert_eq!(
            ride(&mut app, &course),
            MARK,
            "still ridden at once, as it is"
        );
        // Turned off and on again, the world loaded is kept as the older generator built it.
        app.set_keeps_worlds(false).unwrap();
        wait_for(&mut app, |app| app.kept_worlds_size() == 0);
        app.set_keeps_worlds(true).unwrap();
        wait_for(&mut app, |_| keeper.kept.has(&fingerprint));
        assert!(app.kept_world_outdated(&course));

        assert!(app.regenerate_world());
        let events = run_until(&mut app, |e| {
            matches!(e, AppEvent::WorldReady { .. } | AppEvent::Error(_))
        });
        assert!(
            matches!(events.last(), Some(AppEvent::WorldReady { fallback_samples, .. }) if *fallback_samples != MARK),
            "built anew: {events:?}"
        );
        wait_for(&mut app, |_| {
            keeper.kept.generator(&fingerprint) == Some(GENERATOR)
        });
        assert!(!app.kept_world_outdated(&course));
        assert_ne!(
            ride(&mut app, &course),
            MARK,
            "and kept in place of the older"
        );
        app.shutdown();
    }

    #[test]
    fn the_settings_list_each_kept_world_with_its_room_to_delete_it() {
        let dir = temp_dir("kept-list");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        app.set_keeps_worlds(true).unwrap();
        assert_eq!(app.kept_worlds(), Vec::new(), "none kept yet");
        let lakeside = prepare(&mut app, &dir);
        app.rename_course(&lakeside, "Lakeside").unwrap();
        // Another route, a kilometre east.
        let east = std::fs::read_to_string(write_route(&dir))
            .unwrap()
            .replace(r#"lon="7""#, r#"lon="7.013""#);
        let gpx = dir.join("east.gpx");
        std::fs::write(&gpx, east).unwrap();
        let hills = prepare_gpx(&mut app, gpx);
        app.rename_course(&hills, "Hills").unwrap();
        // The same course twice in the library has one world.
        let again = lakeside.with_file_name("again.tqc");
        std::fs::copy(&lakeside, &again).unwrap();
        app.rename_course(&again, "Lakeside again").unwrap();
        let keeper = app.keeper();
        let [lakeside_world, hills_world] =
            [&lakeside, &hills].map(|c| course::fingerprint(c).unwrap());
        assert_ne!(lakeside_world, hills_world);
        wait_for(&mut app, |_| {
            keeper.kept.has(&lakeside_world) && keeper.kept.has(&hills_world)
        });

        let names = |app: &App| -> Vec<Vec<String>> {
            app.kept_worlds()
                .iter()
                .map(|world| {
                    let courses = world.courses.iter();
                    courses.map(|c| c.manifest.name.clone()).collect()
                })
                .collect()
        };
        assert_eq!(
            names(&app),
            [vec!["Hills"], vec!["Lakeside", "Lakeside again"]],
            "by name, a world shared by two courses once"
        );
        let worlds = app.kept_worlds();
        assert_eq!(Some(worlds[0].size), keeper.kept.size_of(&hills_world));
        assert_eq!(Some(worlds[1].size), keeper.kept.size_of(&lakeside_world));
        assert_eq!(
            worlds.iter().map(|world| world.size).sum::<u64>(),
            app.kept_worlds_size(),
            "all the room the kept worlds take is listed"
        );

        app.delete_kept_world(&again).unwrap();
        assert!(
            !keeper.kept.has(&lakeside_world),
            "deleted for both its courses"
        );
        assert_eq!(names(&app), [vec!["Hills"]], "the other world stays");
        assert!(lakeside.exists() && again.exists(), "and the courses do");
        app.delete_kept_world(&again)
            .expect("a list shown before it went may delete it again");
        // Its next ride builds and keeps it again.
        ride(&mut app, &lakeside);
        wait_for(&mut app, |_| keeper.kept.has(&lakeside_world));
        assert_eq!(names(&app).len(), 2);

        // Turned off, they are all being deleted.
        app.set_keeps_worlds(false).unwrap();
        assert_eq!(app.kept_worlds(), Vec::new());
        app.shutdown();
    }

    #[test]
    fn a_damaged_kept_world_is_built_again() {
        let dir = temp_dir("kept-damaged");
        let mut app = App::new(dir.join("data"), dir.join("cache")).unwrap();
        app.set_keeps_worlds(true).unwrap();
        let course = prepare(&mut app, &dir);
        let keeper = app.keeper();
        let fingerprint = course::fingerprint(&course).unwrap();
        wait_for(&mut app, |_| keeper.kept.has(&fingerprint));
        mark(&keeper, &fingerprint, GENERATOR);

        // No error to the rider: the world is built and kept again.
        let file = dir.join(format!("cache/worlds/{fingerprint}.world"));
        std::fs::write(&file, b"damaged").unwrap();
        assert!(!app.kept_world_outdated(&course), "nothing to offer");
        assert_ne!(ride(&mut app, &course), MARK);
        wait_for(&mut app, |_| {
            keeper.kept.generator(&fingerprint) == Some(GENERATOR)
        });
        app.shutdown();
    }
}
