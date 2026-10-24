//! The 3D worlds a computer keeps of its courses (R101, ADR 0025): built once, then read on
//! **Ride** in a second or two instead of being built for half a minute.
//!
//! Layout, in the cache directory, so nothing of it syncs with the data:
//!
//! ```text
//! worlds/<fingerprint>.world   one zip per course: `whole`, the parts built whole, and
//!                              `chunks/<n>` for each chunk, deflated
//! ```
//!
//! A course's world is found by the [fingerprint](crate::course::fingerprint) of what it is
//! built from. It stays kept across updates of ORQWA: one of another
//! [format](orqwa_world::stored::FORMAT) cannot be read and is built again, one of an older
//! [generator](orqwa_world::GENERATOR) is ridden as it is until the rider has it built again.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use orqwa_world::{TerrainChunk, World, stored};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// The directory of kept worlds, in the cache directory.
pub const DIR: &str = "worlds";
const EXTENSION: &str = "world";
const WHOLE: &str = "whole";
const CHUNKS: &str = "chunks/";
/// The quickest deflate: a third of the size, at a fraction of the time the next levels take.
const LEVEL: i64 = 1;
/// A half-written world older than this was left by a write that never finished.
const ABANDONED: Duration = Duration::from_secs(600);
/// Numbers the writes, so two of the same world at once do not write into one file.
static WRITES: AtomicU64 = AtomicU64::new(0);

/// The worlds this computer keeps.
#[derive(Debug, Clone)]
pub struct KeptWorlds {
    root: PathBuf,
}

impl KeptWorlds {
    /// The worlds kept under `cache_dir`.
    #[must_use]
    pub fn new(cache_dir: &Path) -> Self {
        Self {
            root: cache_dir.join(DIR),
        }
    }

    /// Where the world of the course with `fingerprint` is kept, or would be.
    #[must_use]
    pub fn file(&self, fingerprint: &str) -> PathBuf {
        self.root.join(format!("{fingerprint}.{EXTENSION}"))
    }

    /// Whether the world of the course with `fingerprint` is kept.
    #[must_use]
    pub fn has(&self, fingerprint: &str) -> bool {
        self.file(fingerprint).is_file()
    }

    /// How many bytes the kept world of the course with `fingerprint` takes on disk; `None` if
    /// none is kept.
    #[must_use]
    pub fn size_of(&self, fingerprint: &str) -> Option<u64> {
        std::fs::metadata(self.file(fingerprint))
            .ok()
            .filter(std::fs::Metadata::is_file)
            .map(|m| m.len())
    }

    /// The version of the [generator](orqwa_world::GENERATOR) that built the kept world of the
    /// course with `fingerprint`; `None` if none is kept that this ORQWA can read.
    #[must_use]
    pub fn generator(&self, fingerprint: &str) -> Option<u32> {
        let file = File::open(self.file(fingerprint)).ok()?;
        let mut zip = ZipArchive::new(BufReader::new(file)).ok()?;
        let whole = zip.by_name(WHOLE).ok()?;
        stored::read_generator(&mut BufReader::new(whole)).ok()
    }

    /// The kept world of the course with `fingerprint` and the version of the generator that
    /// built it; `None` if there is none. Its chunks are read on all cores.
    ///
    /// # Errors
    /// On I/O errors, and if the file is damaged or of another
    /// [format](orqwa_world::stored::FORMAT).
    pub fn read(&self, fingerprint: &str) -> io::Result<Option<(World, u32)>> {
        let path = self.file(fingerprint);
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut zip = ZipArchive::new(BufReader::new(file))?;
        let (mut world, generator) = stored::read_whole(&mut BufReader::new(zip.by_name(WHOLE)?))?;
        let count = zip.len() - 1;
        let threads = std::thread::available_parallelism()
            .map_or(1, NonZeroUsize::get)
            .min(count)
            .max(1);
        let read = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..threads)
                .map(|first| {
                    let path = &path;
                    scope.spawn(move || -> io::Result<Vec<TerrainChunk>> {
                        let mut zip = ZipArchive::new(BufReader::new(File::open(path)?))?;
                        (first..count)
                            .step_by(threads)
                            .map(|index| {
                                let entry = zip.by_name(&format!("{CHUNKS}{index}"))?;
                                stored::read_chunk(&mut BufReader::new(entry))
                            })
                            .collect()
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| {
                    worker
                        .join()
                        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
                })
                .collect::<io::Result<Vec<_>>>()
        })?;
        // Back in their order: worker `w` read chunks w, w + threads, …
        let mut workers: Vec<_> = read.into_iter().map(Vec::into_iter).collect();
        for index in 0..count {
            let chunk = workers[index % threads]
                .next()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "chunk missing"))?;
            world.chunks.push(chunk);
        }
        Ok(Some((world, generator)))
    }

    /// Keeps `world`, built by the [generator](orqwa_world::GENERATOR) of version
    /// `generator`, as that of the course with `fingerprint`, in place of one kept before. The
    /// file appears whole or not at all.
    ///
    /// # Errors
    /// On I/O errors.
    pub fn write(&self, fingerprint: &str, world: &World, generator: u32) -> io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let path = self.file(fingerprint);
        let write = WRITES.fetch_add(1, Ordering::Relaxed);
        let partial =
            path.with_extension(format!("{EXTENSION}.{}-{write}.part", std::process::id()));
        let written =
            write_to(&partial, world, generator).and_then(|()| std::fs::rename(&partial, &path));
        if written.is_err() {
            let _ = std::fs::remove_file(&partial);
        }
        written
    }

    /// Drops the kept world of the course with `fingerprint`, if there is one.
    ///
    /// # Errors
    /// On I/O errors.
    pub fn remove(&self, fingerprint: &str) -> io::Result<()> {
        remove(&self.file(fingerprint))
    }

    /// Drops the worlds of courses not among `fingerprints`, e.g. the library's: a course
    /// deleted or replaced, also on another computer sharing the data. Worlds this ORQWA cannot
    /// read go too: they would only be built again.
    ///
    /// # Errors
    /// On I/O errors.
    pub fn keep_only(&self, fingerprints: &HashSet<String>) -> io::Result<()> {
        for entry in read_dir(&self.root)? {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let live = name
                .strip_suffix(&format!(".{EXTENSION}"))
                .is_some_and(|fingerprint| {
                    fingerprints.contains(fingerprint) && self.generator(fingerprint).is_some()
                });
            // A world being written is left to its writer, unless that was long ago.
            let writing = path.extension().is_some_and(|e| e == "part")
                && entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .is_ok_and(|at| at.elapsed().is_ok_and(|age| age < ABANDONED));
            if !live && !writing {
                remove(&path)?;
            }
        }
        Ok(())
    }

    /// Drops every kept world.
    ///
    /// # Errors
    /// On I/O errors.
    pub fn clear(&self) -> io::Result<()> {
        remove(&self.root)
    }

    /// How many bytes the kept worlds take on disk.
    #[must_use]
    pub fn size(&self) -> u64 {
        fn size_of(path: &Path) -> u64 {
            match std::fs::symlink_metadata(path) {
                Ok(m) if m.is_dir() => read_dir(path)
                    .unwrap_or_default()
                    .iter()
                    .map(|entry| size_of(&entry.path()))
                    .sum(),
                Ok(m) => m.len(),
                Err(_) => 0,
            }
        }
        size_of(&self.root)
    }
}

fn write_to(path: &Path, world: &World, generator: u32) -> io::Result<()> {
    let mut zip = ZipWriter::new(BufWriter::new(File::create(path)?));
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(LEVEL));
    let mut entry = |name: String, write: &dyn Fn(&mut dyn Write) -> io::Result<()>| {
        zip.start_file(name, options)?;
        let mut out = BufWriter::new(&mut zip);
        write(&mut out)?;
        out.flush()
    };
    entry(WHOLE.to_owned(), &|mut out| {
        stored::write_whole(world, generator, &mut out)
    })?;
    for (index, chunk) in world.chunks.iter().enumerate() {
        entry(format!("{CHUNKS}{index}"), &|mut out| {
            stored::write_chunk(chunk, &mut out)
        })?;
    }
    let mut file = zip
        .finish()?
        .into_inner()
        .map_err(io::IntoInnerError::into_error)?;
    file.flush()?;
    file.sync_all()
}

/// The entries of directory `path`; none if it does not exist.
fn read_dir(path: &Path) -> io::Result<Vec<std::fs::DirEntry>> {
    match std::fs::read_dir(path) {
        Ok(entries) => entries.collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// Removes the file or directory at `path`, if there is one.
fn remove(path: &Path) -> io::Result<()> {
    let removed = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match removed {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use orqwa_world::MeshData;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("orqwa-worlds-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn mesh(seed: f32) -> MeshData {
        MeshData {
            vertices: vec![[seed, 0.0, 1.0]; 3],
            normals: vec![[0.0, 1.0, 0.0]; 3],
            indices: vec![0, 1, 2],
            ..MeshData::default()
        }
    }

    /// A world of `chunks` chunks, each told apart by its centre.
    #[allow(clippy::cast_precision_loss)] // small test numbers
    fn world(chunks: usize) -> World {
        World {
            chunks: (0..chunks)
                .map(|i| TerrainChunk {
                    center: [i as f32, 0.0, 0.0],
                    mesh: mesh(i as f32),
                    buildings: MeshData::default(),
                    modelled: Vec::new(),
                    streets: MeshData::default(),
                    tracks: MeshData::default(),
                    urban: MeshData::default(),
                    water: MeshData::default(),
                    trees: orqwa_world::Trees::default(),
                    props: orqwa_world::Props::default(),
                })
                .collect(),
            road: mesh(-1.0),
            fallback_samples: 3,
            ..World::default()
        }
    }

    #[test]
    fn a_kept_world_reads_back_whole_and_in_order() {
        let cache = temp_dir("kept");
        let kept = KeptWorlds::new(&cache);
        assert!(!kept.has("f1"));
        assert_eq!(kept.read("f1").unwrap(), None);
        assert_eq!(kept.generator("f1"), None);
        assert_eq!(kept.size_of("f1"), None);
        // More chunks than cores, so that every worker reads several.
        let built = world(100);
        kept.write("f1", &built, 3).unwrap();
        assert!(kept.has("f1"));
        assert_eq!(kept.read("f1").unwrap(), Some((built, 3)));
        assert_eq!(
            kept.generator("f1"),
            Some(3),
            "told without reading the world"
        );
        // Another course's world is its own; a world kept again replaces the first.
        kept.write("f2", &world(0), 3).unwrap();
        kept.write("f1", &world(2), 4).unwrap();
        assert_eq!(kept.read("f1").unwrap(), Some((world(2), 4)));
        assert_eq!(kept.generator("f1"), Some(4));
        assert_eq!(kept.read("f2").unwrap(), Some((world(0), 3)));
        // Each world's own room, for the settings to list (R136), adds up to the total.
        let (f1, f2) = (kept.size_of("f1").unwrap(), kept.size_of("f2").unwrap());
        assert!(f1 > f2, "the world with more chunks takes more");
        assert_eq!(kept.size(), f1 + f2);
        std::fs::remove_dir_all(cache).unwrap();
    }

    #[test]
    fn a_damaged_world_is_an_error_not_a_world() {
        let cache = temp_dir("damaged");
        let kept = KeptWorlds::new(&cache);
        kept.write("f", &world(5), 1).unwrap();
        let file = kept.file("f");
        let bytes = std::fs::read(&file).unwrap();
        std::fs::write(&file, &bytes[..bytes.len() / 2]).unwrap();
        assert!(kept.read("f").is_err());
        std::fs::write(&file, b"not a zip").unwrap();
        assert!(kept.read("f").is_err());
        assert_eq!(kept.generator("f"), None);
        std::fs::remove_dir_all(cache).unwrap();
    }

    #[test]
    fn only_the_worlds_of_courses_still_there_are_kept() {
        let cache = temp_dir("sweep");
        let kept = KeptWorlds::new(&cache);
        for fingerprint in ["f1", "f2", "f3", "f5"] {
            kept.write(fingerprint, &world(1), 1).unwrap();
        }
        let writing = kept.file("f4").with_extension("world.part");
        std::fs::write(&writing, b"half").unwrap();
        // One this ORQWA cannot read, e.g. of another format: it would only be built again.
        std::fs::write(kept.file("f5"), b"unreadable").unwrap();

        // f2's course was deleted.
        let library = ["f1", "f3", "f5"].map(ToOwned::to_owned);
        kept.keep_only(&HashSet::from(library)).unwrap();
        assert!(kept.has("f1") && kept.has("f3"));
        assert!(!kept.has("f2"));
        assert!(!kept.has("f5"), "the unreadable one is gone");
        assert!(writing.exists(), "a world being written stays");

        kept.clear().unwrap();
        assert!(!kept.has("f1"));
        assert_eq!(kept.size(), 0);
        // Clearing nothing is no error.
        kept.clear().unwrap();
        kept.keep_only(&HashSet::new()).unwrap();
        std::fs::remove_dir_all(cache).unwrap();
    }
}
