//! Cover art. A track's art is the front cover in its tags, or the first picture if none is
//! marked front, or else an image in its folder named cover, folder or front. Each picture is
//! resized once into the art folder beside the database, named by a hash of the original, so
//! tracks sharing a picture share the files; the database records only which picture each
//! track shows.
//!
//! One background thread does the work. Requests from what is on screen go first; between
//! them it works through the tracks with no current entry, which it is asked to do at start
//! and after the library changes. `artReady` says which tracks it has done.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use lofty::config::{ParseOptions, ParsingMode};
use lofty::file::TaggedFileExt;
use lofty::picture::PictureType;
use lofty::probe::Probe;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::api::{ApiError, CoreEvent, ErrorCode, EventSink};
use crate::db::{Database, DbError};

/// The sizes kept of each picture, in pixels along the longer side: for grids and lists, and
/// for the player and the system's media controls. Smaller pictures are not enlarged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ArtSize {
    Small,
    Large,
}

impl ArtSize {
    const ALL: [Self; 2] = [Self::Small, Self::Large];

    pub fn pixels(self) -> u32 {
        match self {
            Self::Small => 256,
            Self::Large => 1024,
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "small" => Some(Self::Small),
            "large" => Some(Self::Large),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Large => "large",
        }
    }
}

const JPEG_QUALITY: u8 = 85;
/// Images in a folder taken as its art, in order of preference, compared ignoring case.
const FOLDER_NAMES: [&str; 3] = ["cover", "folder", "front"];
const FOLDER_EXTENSIONS: [&str; 3] = ["jpg", "jpeg", "png"];
/// Past a tag, how far to look for the audio. Real files pad beyond Lofty's default.
const MAX_JUNK_BYTES: usize = 1024 * 1024;
/// Tracks the backfill reads from the database at a time.
const BATCH: i64 = 200;
/// How often the backfill reports the tracks it has done.
const REPORT_EVERY: Duration = Duration::from_secs(1);
const MAX_WAITING: usize = 256;

/// The folder holding the art of the library in this database.
pub fn art_folder(db: &Database) -> PathBuf {
    db.path()
        .parent()
        .map_or_else(|| PathBuf::from("art"), |folder| folder.join("art"))
}

/// Where a picture's file of a size is, given the hash that names it. Hashes are checked, so a
/// request cannot name a file outside the folder.
pub fn art_file(folder: &Path, art: &str, size: ArtSize) -> Option<PathBuf> {
    if art.len() != 32 || !art.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(
        folder
            .join(&art[..2])
            .join(format!("{art}-{}.jpg", size.name())),
    )
}

/// The file an image name such as `<hash>-small` stands for, if there is one. Names come from
/// clients asking for pictures, so anything else names nothing.
pub fn image_file(folder: &Path, name: &str) -> Option<PathBuf> {
    let (art, size) = name.rsplit_once('-')?;
    let file = art_file(folder, art, ArtSize::parse(size)?)?;
    file.is_file().then_some(file)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Embedded,
    Folder,
    None,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Self::Embedded => "embedded",
            Self::Folder => "folder",
            Self::None => "none",
        }
    }
}

/// The picture in a file's tags: the front cover, or the first picture if none is.
fn embedded_picture(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let options = ParseOptions::new()
        .read_properties(false)
        .read_cover_art(true)
        .max_junk_bytes(MAX_JUNK_BYTES)
        .parsing_mode(ParsingMode::Relaxed);
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    let tagged = Probe::new(BufReader::new(file))
        .options(options)
        .guess_file_type()
        .map_err(|error| error.to_string())?
        .read()
        .map_err(|error| error.to_string())?;
    let pictures: Vec<_> = tagged
        .tags()
        .iter()
        .flat_map(|tag| tag.pictures())
        .filter(|picture| !picture.data().is_empty())
        .collect();
    let chosen = pictures
        .iter()
        .find(|picture| picture.pic_type() == PictureType::CoverFront)
        .or_else(|| pictures.first());
    Ok(chosen.map(|picture| picture.data().to_vec()))
}

/// The art image in a folder, if it has one.
fn folder_image(folder: &Path) -> Option<PathBuf> {
    let mut found: Vec<(usize, PathBuf)> = fs::read_dir(folder)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let stem = path.file_stem()?.to_str()?.to_lowercase();
            let extension = path.extension()?.to_str()?.to_lowercase();
            let rank = FOLDER_NAMES.iter().position(|name| *name == stem)?;
            FOLDER_EXTENSIONS
                .contains(&extension.as_str())
                .then_some((rank, path))
        })
        .collect();
    found.sort();
    found.into_iter().next().map(|(_, path)| path)
}

fn hash_of(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex()[..32].to_owned()
}

/// Resizes a picture into the art folder, unless it is there already, and returns its hash.
fn store(folder: &Path, bytes: &[u8]) -> Result<String, String> {
    let art = hash_of(bytes);
    let files: Vec<(ArtSize, PathBuf)> = ArtSize::ALL
        .iter()
        .filter_map(|size| Some((*size, art_file(folder, &art, *size)?)))
        .collect();
    if files.iter().all(|(_, file)| file.is_file()) {
        return Ok(art);
    }
    let picture = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| error.to_string())?
        .decode()
        .map_err(|error| format!("the picture could not be read: {error}"))?;
    for (size, file) in files {
        let pixels = size.pixels();
        let resized = if picture.width() > pixels || picture.height() > pixels {
            picture.resize(pixels, pixels, FilterType::Triangle)
        } else {
            picture.clone()
        };
        let mut encoded = Vec::new();
        JpegEncoder::new_with_quality(&mut encoded, JPEG_QUALITY)
            .encode_image(&resized.to_rgb8())
            .map_err(|error| error.to_string())?;
        let parent = file.parent().expect("art files are in a subfolder");
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        // Written whole under another name first, so a file by this name is always complete.
        let partial = file.with_extension("partial");
        fs::write(&partial, &encoded).map_err(|error| error.to_string())?;
        fs::rename(&partial, &file).map_err(|error| error.to_string())?;
    }
    Ok(art)
}

/// What a track shows, found and stored.
#[derive(Debug, Clone, PartialEq)]
struct Found {
    art: Option<String>,
    source: Source,
    error: Option<String>,
}

/// Remembers each folder's image while working through an album, so it is read once.
type FolderArt = HashMap<PathBuf, Option<String>>;

fn find(path: &Path, folder: &Path, folders: &mut FolderArt) -> Found {
    let mut error = None;
    match embedded_picture(path) {
        Ok(Some(bytes)) => match store(folder, &bytes) {
            Ok(art) => {
                return Found {
                    art: Some(art),
                    source: Source::Embedded,
                    error: None,
                };
            }
            // A damaged picture in the tags leaves the folder's to show.
            Err(message) => error = Some(message),
        },
        Ok(None) => {}
        Err(message) => error = Some(message),
    }
    let parent = path.parent().unwrap_or(Path::new("")).to_path_buf();
    let in_folder = folders
        .entry(parent)
        .or_insert_with_key(|parent| {
            let image = folder_image(parent)?;
            let bytes = fs::read(image).ok()?;
            store(folder, &bytes).ok()
        })
        .clone();
    match in_folder {
        Some(art) => Found {
            art: Some(art),
            source: Source::Folder,
            error,
        },
        None => Found {
            art: None,
            source: Source::None,
            error,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ArtState {
    /// Being looked for. `artReady` follows.
    Pending,
    Found,
    /// Neither the tags nor the folder have a picture, or the file is missing.
    None,
}

/// The art a track shows. `art` names it for the art URL scheme, with a size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TrackArt {
    pub track_id: String,
    pub state: ArtState,
    pub art: Option<String>,
}

struct TrackFile {
    id: i64,
    path: PathBuf,
    size: i64,
    mtime: i64,
}

const TRACK_FILE: &str = "SELECT t.id, r.path, t.rel_path, t.size, t.mtime, t.public_id
                          FROM tracks t JOIN library_roots r ON r.id = t.root_id";

fn track_file(row: &rusqlite::Row<'_>) -> rusqlite::Result<TrackFile> {
    let root: String = row.get(1)?;
    let rel_path: String = row.get(2)?;
    Ok(TrackFile {
        id: row.get(0)?,
        path: Path::new(&root).join(rel_path),
        size: row.get(3)?,
        mtime: row.get(4)?,
    })
}

/// The art stored for a track if it is current: the picture's hash, or `None` for no picture.
type Stored = Option<Option<String>>;

/// The track's file, and the art stored for it.
fn lookup(conn: &Connection, track_id: &str) -> Result<Option<(TrackFile, Stored)>, DbError> {
    let Some(file) = conn
        .prepare_cached(&format!("{TRACK_FILE} WHERE t.public_id = ?1"))?
        .query_row([track_id], track_file)
        .optional()?
    else {
        return Ok(None);
    };
    let stored = conn
        .prepare_cached(
            "SELECT art FROM track_art WHERE track_id = ?1 AND size = ?2 AND mtime = ?3",
        )?
        .query_row(params![file.id, file.size, file.mtime], |row| {
            row.get::<_, Option<String>>(0)
        })
        .optional()?;
    Ok(Some((file, stored)))
}

fn save(conn: &Connection, file: &TrackFile, found: &Found) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "INSERT OR REPLACE INTO track_art (track_id, size, mtime, art, source, error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?
    .execute(params![
        file.id,
        file.size,
        file.mtime,
        found.art,
        found.source.as_str(),
        found.error
    ])?;
    Ok(())
}

#[derive(Default)]
struct Queue {
    waiting: VecDeque<String>,
    backfill: bool,
    /// The backfill starts again from the first track, since the library has changed.
    restart: bool,
    closing: bool,
}

struct Shared {
    db: Arc<Database>,
    events: Arc<dyn EventSink>,
    folder: PathBuf,
    /// Whether to fill the cache when the library changes. Tools that only scan leave it off.
    filling: AtomicBool,
    queue: Mutex<Queue>,
    wake: Condvar,
    stop: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Finds and caches art on one thread, started at the first request.
pub struct ArtCache {
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl ArtCache {
    pub fn new(db: Arc<Database>, events: Arc<dyn EventSink>) -> Self {
        let folder = art_folder(&db);
        Self {
            shared: Arc::new(Shared {
                db,
                events,
                folder,
                filling: AtomicBool::new(false),
                queue: Mutex::default(),
                wake: Condvar::new(),
                stop: AtomicBool::new(false),
            }),
            thread: Mutex::new(None),
        }
    }

    pub fn folder(&self) -> &Path {
        &self.shared.folder
    }

    /// The track's art if it is known, or else starts looking for it.
    pub fn get(&self, track_id: &str) -> Result<TrackArt, ApiError> {
        let Some((file, stored)) = self.shared.db.read(|conn| lookup(conn, track_id))? else {
            return Err(ApiError::new(
                ErrorCode::NotFound,
                format!("no track {track_id}"),
            ));
        };
        let answer = |state, art| TrackArt {
            track_id: track_id.to_owned(),
            state,
            art,
        };
        Ok(match stored {
            Some(Some(art)) => answer(ArtState::Found, Some(art)),
            Some(None) => answer(ArtState::None, None),
            // Not stored, so it is looked for again when the file comes back.
            None if !file.path.is_file() => answer(ArtState::None, None),
            None => {
                self.wake(|queue| {
                    queue.waiting.retain(|waiting| waiting != track_id);
                    queue.waiting.push_front(track_id.to_owned());
                    queue.waiting.truncate(MAX_WAITING);
                });
                answer(ArtState::Pending, None)
            }
        })
    }

    /// From now on keeps every track's art cached: works through those with no current entry,
    /// between requests, now and whenever `library_changed` says to.
    pub fn keep_filled(&self) {
        self.shared.filling.store(true, Ordering::SeqCst);
        self.fill();
    }

    /// Fills in tracks added or changed, if the cache is being kept filled.
    pub fn library_changed(&self) {
        if self.shared.filling.load(Ordering::SeqCst) {
            self.fill();
        }
    }

    /// Whether there is work waiting: requests, or tracks left to fill in.
    pub fn is_busy(&self) -> bool {
        let queue = lock(&self.shared.queue);
        queue.backfill || !queue.waiting.is_empty()
    }

    fn fill(&self) {
        self.wake(|queue| {
            queue.backfill = true;
            queue.restart = true;
        });
    }

    fn wake(&self, change: impl FnOnce(&mut Queue)) {
        change(&mut lock(&self.shared.queue));
        self.shared.wake.notify_one();
        let mut thread = lock(&self.thread);
        if thread.is_none() {
            let shared = Arc::clone(&self.shared);
            *thread = Some(
                thread::Builder::new()
                    .name("umak-art".to_owned())
                    .spawn(move || run(&shared))
                    .expect("the art thread starts"),
            );
        }
    }
}

impl Drop for ArtCache {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        lock(&self.shared.queue).closing = true;
        self.shared.wake.notify_all();
        if let Some(thread) = lock(&self.thread).take() {
            let _ = thread.join();
        }
    }
}

enum Work {
    Request(String),
    Backfill,
}

fn run(shared: &Shared) {
    // The backfill's place, by track id. Tracks it cannot do, such as those on a drive that
    // is not connected, are passed over until it starts again.
    let mut after = 0_i64;
    loop {
        let work = {
            let mut queue = lock(&shared.queue);
            loop {
                if queue.closing {
                    return;
                }
                if std::mem::take(&mut queue.restart) {
                    after = 0;
                }
                if let Some(next) = queue.waiting.pop_front() {
                    break Work::Request(next);
                }
                if queue.backfill {
                    break Work::Backfill;
                }
                queue = shared
                    .wake
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let result = match work {
            Work::Request(track_id) => request(shared, &track_id),
            Work::Backfill => backfill(shared, &mut after),
        };
        if let Err(error) = result {
            eprintln!("failed to cache cover art: {}", error.message);
        }
    }
}

fn request(shared: &Shared, track_id: &str) -> Result<(), ApiError> {
    let Some((file, stored)) = shared.db.read(|conn| lookup(conn, track_id))? else {
        return Ok(());
    };
    if stored.is_none() && file.path.is_file() {
        let found = find(&file.path, &shared.folder, &mut FolderArt::new());
        shared
            .db
            .write(|conn| save(conn, &file, &found).map_err(DbError::from))?;
    }
    shared.events.emit(&CoreEvent::ArtReady {
        track_ids: vec![track_id.to_owned()],
    });
    Ok(())
}

/// Does one batch of tracks with no current entry, after the track id `after`, stopping early
/// for requests. Ends the backfill when there are none left.
fn backfill(shared: &Shared, after: &mut i64) -> Result<(), ApiError> {
    let batch: Vec<(String, TrackFile)> = shared.db.read(|conn| {
        conn.prepare_cached(&format!(
            "{TRACK_FILE} LEFT JOIN track_art a ON a.track_id = t.id
             WHERE t.id > ?1 AND t.missing_since IS NULL
               AND (a.track_id IS NULL OR a.size != t.size OR a.mtime != t.mtime)
             ORDER BY t.id LIMIT ?2"
        ))?
        .query_map(params![*after, BATCH], |row| {
            Ok((row.get::<_, String>(5)?, track_file(row)?))
        })
        .map_err(DbError::from)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(DbError::from)
    })?;
    if batch.is_empty() {
        lock(&shared.queue).backfill = false;
        return Ok(());
    }
    let mut folders = FolderArt::new();
    let mut done = Vec::new();
    let mut reported = Instant::now();
    for (track_id, file) in batch {
        if shared.stop.load(Ordering::SeqCst) || !lock(&shared.queue).waiting.is_empty() {
            break;
        }
        *after = file.id;
        if !file.path.is_file() {
            continue;
        }
        let found = find(&file.path, &shared.folder, &mut folders);
        shared
            .db
            .write(|conn| save(conn, &file, &found).map_err(DbError::from))?;
        done.push(track_id);
        if reported.elapsed() >= REPORT_EVERY {
            shared.events.emit(&CoreEvent::ArtReady {
                track_ids: std::mem::take(&mut done),
            });
            reported = Instant::now();
        }
    }
    if !done.is_empty() {
        shared.events.emit(&CoreEvent::ArtReady { track_ids: done });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use image::{ImageFormat, RgbImage};

    use super::*;

    /// A PNG of the size, its color telling pictures apart.
    fn png(width: u32, height: u32, shade: u8) -> Vec<u8> {
        let picture = RgbImage::from_fn(width, height, |x, _| {
            image::Rgb([shade, (x % 256) as u8, 0])
        });
        let mut bytes = Cursor::new(Vec::new());
        picture.write_to(&mut bytes, ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    fn synchsafe(size: usize) -> [u8; 4] {
        let size = size as u32;
        [
            (size >> 21 & 0x7f) as u8,
            (size >> 14 & 0x7f) as u8,
            (size >> 7 & 0x7f) as u8,
            (size & 0x7f) as u8,
        ]
    }

    fn mpeg_audio() -> Vec<u8> {
        let mut audio = Vec::new();
        for _ in 0..20 {
            audio.extend([0xff, 0xfb, 0x90, 0x00]);
            audio.extend([0x55; 413]);
        }
        audio
    }

    /// An MP3 whose ID3v2.3 tag holds these pictures, each with its picture type.
    fn mp3(pictures: &[(u8, &[u8])]) -> Vec<u8> {
        let mut frames = Vec::new();
        let title = b"\0Title";
        frames.extend_from_slice(b"TIT2");
        frames.extend((title.len() as u32).to_be_bytes());
        frames.extend([0, 0]);
        frames.extend_from_slice(title);
        for (kind, data) in pictures {
            let mut body = vec![0];
            body.extend_from_slice(b"image/png\0");
            body.push(*kind);
            body.push(0);
            body.extend_from_slice(data);
            frames.extend_from_slice(b"APIC");
            frames.extend((body.len() as u32).to_be_bytes());
            frames.extend([0, 0]);
            frames.extend(body);
        }
        let mut file = b"ID3\x03\x00\x00".to_vec();
        file.extend(synchsafe(frames.len()));
        file.extend(frames);
        file.extend(mpeg_audio());
        file
    }

    /// A FLAC file with a front cover in a picture block.
    fn flac(picture: &[u8]) -> Vec<u8> {
        let mut stream_info = vec![0x10, 0, 0x10, 0, 0, 0, 0, 0, 0, 0];
        let packed: u64 = (44_100 << 44) | (1 << 41) | (15 << 36) | 441_000;
        stream_info.extend(packed.to_be_bytes());
        stream_info.extend([0; 16]);
        let mut block = 3_u32.to_be_bytes().to_vec();
        let mime = b"image/png";
        block.extend((mime.len() as u32).to_be_bytes());
        block.extend_from_slice(mime);
        block.extend(0_u32.to_be_bytes());
        block.extend([0; 16]);
        block.extend((picture.len() as u32).to_be_bytes());
        block.extend_from_slice(picture);
        let mut file = b"fLaC".to_vec();
        file.extend([0, 0, 0, stream_info.len() as u8]);
        file.extend(stream_info);
        file.push(0x86);
        file.extend_from_slice(&(block.len() as u32).to_be_bytes()[1..]);
        file.extend(block);
        file.extend(mpeg_audio());
        file
    }

    fn found_in(dir: &Path, name: &str, bytes: &[u8]) -> Found {
        let path = dir.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        find(&path, &dir.join("art"), &mut FolderArt::new())
    }

    #[test]
    fn the_front_cover_is_taken_before_other_pictures_and_any_picture_before_none() {
        let dir = tempfile::tempdir().unwrap();
        let (back, front) = (png(40, 40, 1), png(40, 40, 2));
        let found = found_in(dir.path(), "a/both.mp3", &mp3(&[(4, &back), (3, &front)]));
        assert_eq!(found.art, Some(hash_of(&front)));
        assert_eq!(found.source, Source::Embedded);

        let found = found_in(dir.path(), "b/back.mp3", &mp3(&[(4, &back)]));
        assert_eq!(found.art, Some(hash_of(&back)));

        let found = found_in(dir.path(), "c/none.mp3", &mp3(&[]));
        assert_eq!(
            found,
            Found {
                art: None,
                source: Source::None,
                error: None
            }
        );
    }

    #[test]
    fn a_flac_picture_block_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let cover = png(30, 30, 3);
        let found = found_in(dir.path(), "a/one.flac", &flac(&cover));
        assert_eq!(found.art, Some(hash_of(&cover)));
        assert_eq!(found.source, Source::Embedded);
    }

    #[test]
    fn a_folder_image_stands_in_for_tags_without_a_picture() {
        let dir = tempfile::tempdir().unwrap();
        let (cover, folder) = (png(20, 20, 4), png(20, 20, 5));
        fs::create_dir(dir.path().join("album")).unwrap();
        fs::write(dir.path().join("album/Folder.jpg"), &folder).unwrap();
        fs::write(dir.path().join("album/COVER.PNG"), &cover).unwrap();
        fs::write(dir.path().join("album/back.jpg"), png(20, 20, 6)).unwrap();
        let found = found_in(dir.path(), "album/one.mp3", &mp3(&[]));
        assert_eq!(
            found.art,
            Some(hash_of(&cover)),
            "cover before folder, whatever the case"
        );
        assert_eq!(found.source, Source::Folder);

        // A damaged picture in the tags still leaves the folder's.
        let found = found_in(dir.path(), "album/two.mp3", &mp3(&[(3, b"not a picture")]));
        assert_eq!(found.art, Some(hash_of(&cover)));
        assert!(found.error.is_some());
    }

    #[test]
    fn pictures_are_resized_once_into_files_named_by_their_hash() {
        let dir = tempfile::tempdir().unwrap();
        let picture = png(600, 400, 7);
        let art = store(dir.path(), &picture).unwrap();
        let size_of = |size| {
            let file = art_file(dir.path(), &art, size).unwrap();
            image::open(file).unwrap()
        };
        let small = size_of(ArtSize::Small);
        assert_eq!((small.width(), small.height()), (256, 171));
        let large = size_of(ArtSize::Large);
        assert_eq!(
            (large.width(), large.height()),
            (600, 400),
            "never enlarged"
        );

        let file = art_file(dir.path(), &art, ArtSize::Small).unwrap();
        let written = fs::metadata(&file).unwrap().modified().unwrap();
        assert_eq!(store(dir.path(), &picture).unwrap(), art);
        assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), written);

        let named = image_file(dir.path(), &format!("{art}-large")).unwrap();
        assert_eq!(named, art_file(dir.path(), &art, ArtSize::Large).unwrap());
        for name in [
            format!("{art}-huge"),
            format!("{art}small"),
            "0123456789abcdef0123456789abcdef-small".to_owned(),
            "..-small".to_owned(),
        ] {
            assert_eq!(image_file(dir.path(), &name), None, "{name}");
        }

        assert!(store(dir.path(), b"not a picture").is_err());
        assert_eq!(art_file(dir.path(), "../../secret", ArtSize::Small), None);
        assert_eq!(art_file(dir.path(), &"g".repeat(32), ArtSize::Small), None);
    }

    #[derive(Default)]
    struct Recording(Mutex<Vec<String>>);

    impl EventSink for Recording {
        fn emit(&self, event: &CoreEvent) {
            if let CoreEvent::ArtReady { track_ids } = event {
                lock(&self.0).extend(track_ids.iter().cloned());
            }
        }
    }

    struct Fixture {
        dir: tempfile::TempDir,
        db: Arc<Database>,
        events: Arc<Recording>,
        cache: ArtCache,
    }

    /// Two tracks sharing a cover, one with none, and one whose file is missing.
    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let cover = png(300, 300, 8);
        fs::create_dir_all(dir.path().join("music/album")).unwrap();
        fs::create_dir_all(dir.path().join("music/bare")).unwrap();
        fs::write(dir.path().join("music/album/1.mp3"), mp3(&[(3, &cover)])).unwrap();
        fs::write(dir.path().join("music/album/2.mp3"), mp3(&[(3, &cover)])).unwrap();
        fs::write(dir.path().join("music/bare/3.mp3"), mp3(&[])).unwrap();
        let db = Arc::new(Database::open(dir.path().join("umak.db")).unwrap());
        let root = dir.path().join("music").to_string_lossy().into_owned();
        db.write(|conn| {
            conn.execute(
                "INSERT INTO library_roots (id, public_id, name, path, added_at)
                 VALUES (1, 'r', 'Music', ?1, 0)",
                [&root],
            )?;
            conn.execute_batch(
                "INSERT INTO albums (id, public_id, group_key, title, sort_title,
                                     album_artist_display, album_artist_sort)
                 VALUES (1, 'a', 'k', 'Album', 'album', 'X', 'x');",
            )?;
            for (name, path) in [
                ("one", "album/1.mp3"),
                ("two", "album/2.mp3"),
                ("bare", "bare/3.mp3"),
                ("gone", "bare/gone.mp3"),
            ] {
                conn.execute(
                    "INSERT INTO tracks (public_id, root_id, rel_path, size, mtime, title, album_id,
                         title_sort, artist_sort, album_sort, album_artist_sort, added_at, updated_at)
                     VALUES (?1, 1, ?2, 1, 1, ?1, 1, ?1, 'x', 'x', 'x', 0, 0)",
                    params![name, path],
                )?;
            }
            Ok::<_, DbError>(())
        })
        .unwrap();
        let events = Arc::new(Recording::default());
        let sink: Arc<dyn EventSink> = events.clone();
        let cache = ArtCache::new(Arc::clone(&db), sink);
        Fixture {
            dir,
            db,
            events,
            cache,
        }
    }

    impl Fixture {
        fn wait_for(&self, track_ids: &[&str]) {
            let started = Instant::now();
            while !track_ids
                .iter()
                .all(|id| lock(&self.events.0).iter().any(|done| done == id))
            {
                assert!(
                    started.elapsed() < Duration::from_secs(20),
                    "waiting for {track_ids:?}"
                );
                thread::sleep(Duration::from_millis(5));
            }
        }

        fn rows(&self) -> i64 {
            self.db
                .read(|conn| {
                    conn.query_row("SELECT count(*) FROM track_art", [], |row| row.get(0))
                        .map_err(DbError::from)
                })
                .unwrap()
        }

        fn files(&self) -> usize {
            let folder = self.cache.folder();
            fs::read_dir(folder)
                .map(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .map(|entry| fs::read_dir(entry.path()).unwrap().count())
                        .sum()
                })
                .unwrap_or(0)
        }
    }

    #[test]
    fn a_track_asked_for_is_looked_up_once_and_then_known() {
        let fixture = fixture();
        assert_eq!(fixture.cache.get("one").unwrap().state, ArtState::Pending);
        fixture.wait_for(&["one"]);
        let art = fixture.cache.get("one").unwrap();
        assert_eq!(art.state, ArtState::Found);
        assert!(
            art_file(
                fixture.cache.folder(),
                art.art.as_ref().unwrap(),
                ArtSize::Large
            )
            .unwrap()
            .is_file()
        );

        assert_eq!(fixture.cache.get("bare").unwrap().state, ArtState::Pending);
        fixture.wait_for(&["bare"]);
        assert_eq!(fixture.cache.get("bare").unwrap().state, ArtState::None);

        // A missing file is answered without being stored.
        assert_eq!(fixture.cache.get("gone").unwrap().state, ArtState::None);
        assert_eq!(fixture.rows(), 2);
        assert_eq!(
            fixture.cache.get("nothing").unwrap_err().code,
            ErrorCode::NotFound
        );
    }

    #[test]
    fn keeping_the_cache_filled_does_every_track_and_shares_pictures() {
        let fixture = fixture();
        fixture.cache.library_changed();
        thread::sleep(Duration::from_millis(100));
        assert_eq!(fixture.rows(), 0, "nothing until asked to keep it filled");

        fixture.cache.keep_filled();
        fixture.wait_for(&["one", "two", "bare"]);
        assert_eq!(fixture.rows(), 3, "all but the missing file");
        assert_eq!(fixture.files(), 2, "one picture, in two sizes");
        let one = fixture.cache.get("one").unwrap();
        assert_eq!(fixture.cache.get("two").unwrap().art, one.art);

        // A changed file is looked at again when the library changes.
        fixture
            .db
            .write(|conn| {
                conn.execute("UPDATE tracks SET size = 2 WHERE public_id = 'bare'", [])
                    .map_err(DbError::from)
            })
            .unwrap();
        fs::write(
            fixture.dir.path().join("music/bare/cover.jpg"),
            png(10, 10, 9),
        )
        .unwrap();
        lock(&fixture.events.0).clear();
        fixture.cache.library_changed();
        fixture.wait_for(&["bare"]);
        assert_eq!(fixture.cache.get("bare").unwrap().state, ArtState::Found);
    }
}
