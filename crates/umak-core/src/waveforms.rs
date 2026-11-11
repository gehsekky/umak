//! Waveforms for the seek bar. Each is computed from the audio once, on a background thread,
//! and kept in the database until the file changes.
//!
//! Asking for a waveform that is not ready starts computing it and answers that it is pending;
//! `waveformReady` follows when it is done, or has failed. The latest request goes first, since
//! it is for the track the listener has just moved to.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use ts_rs::TS;

use crate::api::{ApiError, CoreEvent, ErrorCode, EventSink};
use crate::db::{Database, DbError};

/// Columns across a waveform, more than a seek bar is usually wide.
pub const WAVEFORM_COLUMNS: usize = 1000;
/// Raise to make every stored waveform out of date, when the way they are computed changes.
const FORMAT: i64 = 1;
/// Requests beyond this many waiting drop the oldest, which nobody is looking at any more.
const MAX_WAITING: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum WaveformState {
    Ready,
    /// Being computed. `waveformReady` follows.
    Pending,
    Failed,
}

/// A track's waveform. `peaks` holds the loudest sample of each column and `levels` the root
/// mean square of its samples, both from 0 to 255 at full scale. Empty unless ready.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TrackWaveform {
    pub track_id: String,
    pub state: WaveformState,
    pub peaks: Vec<u8>,
    pub levels: Vec<u8>,
    pub error: Option<String>,
}

type Computed = Result<(Vec<u8>, Vec<u8>), String>;

#[cfg(feature = "engine")]
fn compute(path: &Path, stop: &AtomicBool) -> Computed {
    let waveform = umak_audio::waveform::waveform(path, WAVEFORM_COLUMNS, stop)?;
    Ok((waveform.peaks, waveform.levels))
}

#[cfg(not(feature = "engine"))]
fn compute(_path: &Path, _stop: &AtomicBool) -> Computed {
    Err("this build cannot decode audio".to_owned())
}

/// What identifies the file a waveform was computed from.
#[derive(Debug, Clone, PartialEq)]
struct FileIdentity {
    size: i64,
    mtime: i64,
    fingerprint: Option<Vec<u8>>,
}

impl FileIdentity {
    fn matches(&self, stored: &FileIdentity) -> bool {
        (self.size == stored.size && self.mtime == stored.mtime)
            || matches!((&self.fingerprint, &stored.fingerprint), (Some(a), Some(b)) if a == b)
    }
}

struct TrackFile {
    id: i64,
    path: PathBuf,
    identity: FileIdentity,
}

fn track_file(conn: &Connection, public_id: &str) -> Result<Option<TrackFile>, ApiError> {
    Ok(conn
        .prepare_cached(
            "SELECT t.id, r.path, t.rel_path, t.size, t.mtime, t.fingerprint
             FROM tracks t JOIN library_roots r ON r.id = t.root_id
             WHERE t.public_id = ?1",
        )?
        .query_row([public_id], |row| {
            let root: String = row.get(1)?;
            let rel_path: String = row.get(2)?;
            Ok(TrackFile {
                id: row.get(0)?,
                path: Path::new(&root).join(rel_path),
                identity: FileIdentity {
                    size: row.get(3)?,
                    mtime: row.get(4)?,
                    fingerprint: row.get(5)?,
                },
            })
        })
        .optional()?)
}

/// The stored waveform, if it is current for this file.
fn stored(conn: &Connection, file: &TrackFile) -> Result<Option<Computed>, ApiError> {
    let row = conn
        .prepare_cached(
            "SELECT format, size, mtime, fingerprint, peaks, levels, error
             FROM waveforms WHERE track_id = ?1",
        )?
        .query_row([file.id], |row| {
            let identity = FileIdentity {
                size: row.get(1)?,
                mtime: row.get(2)?,
                fingerprint: row.get(3)?,
            };
            let peaks: Option<Vec<u8>> = row.get(4)?;
            let levels: Option<Vec<u8>> = row.get(5)?;
            let error: Option<String> = row.get(6)?;
            let computed = match (peaks, levels) {
                (Some(peaks), Some(levels)) => Ok((peaks, levels)),
                _ => Err(error.unwrap_or_default()),
            };
            Ok((row.get::<_, i64>(0)?, identity, computed))
        })
        .optional()?;
    Ok(row.and_then(|(format, identity, computed)| {
        (format == FORMAT && file.identity.matches(&identity)).then_some(computed)
    }))
}

fn save(conn: &Connection, file: &TrackFile, computed: &Computed) -> rusqlite::Result<()> {
    let (peaks, levels, error) = match computed {
        Ok((peaks, levels)) => (Some(peaks), Some(levels), None),
        Err(error) => (None, None, Some(error)),
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64);
    conn.prepare_cached(
        "INSERT OR REPLACE INTO waveforms
             (track_id, format, size, mtime, fingerprint, peaks, levels, error, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?
    .execute(params![
        file.id,
        FORMAT,
        file.identity.size,
        file.identity.mtime,
        file.identity.fingerprint,
        peaks,
        levels,
        error,
        now
    ])?;
    Ok(())
}

fn answer(track_id: &str, state: WaveformState, computed: Option<Computed>) -> TrackWaveform {
    let (peaks, levels, error) = match computed {
        Some(Ok((peaks, levels))) => (peaks, levels, None),
        Some(Err(error)) => (Vec::new(), Vec::new(), Some(error)),
        None => (Vec::new(), Vec::new(), None),
    };
    TrackWaveform {
        track_id: track_id.to_owned(),
        state,
        peaks,
        levels,
        error,
    }
}

#[derive(Default)]
struct Queue {
    waiting: VecDeque<String>,
    working: Option<String>,
    closing: bool,
}

struct Shared {
    db: Arc<Database>,
    events: Arc<dyn EventSink>,
    queue: Mutex<Queue>,
    wake: Condvar,
    stop: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Computes waveforms on one thread, started at the first request.
pub struct Waveforms {
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Waveforms {
    pub fn new(db: Arc<Database>, events: Arc<dyn EventSink>) -> Self {
        Self {
            shared: Arc::new(Shared {
                db,
                events,
                queue: Mutex::default(),
                wake: Condvar::new(),
                stop: AtomicBool::new(false),
            }),
            thread: Mutex::new(None),
        }
    }

    /// The track's waveform if it is current, or else starts computing it.
    pub fn get(&self, track_id: &str) -> Result<TrackWaveform, ApiError> {
        let (file, stored) = self.shared.db.read(|conn| {
            let Some(file) = track_file(conn, track_id)? else {
                return Err(ApiError::new(
                    ErrorCode::NotFound,
                    format!("no track {track_id}"),
                ));
            };
            let stored = stored(conn, &file)?;
            Ok::<_, ApiError>((file, stored))
        })?;
        match stored {
            Some(Ok(computed)) => Ok(answer(track_id, WaveformState::Ready, Some(Ok(computed)))),
            Some(Err(error)) => Ok(answer(track_id, WaveformState::Failed, Some(Err(error)))),
            // Not stored, since the file may come back as it was, and then it should be
            // computed after all.
            None if !file.path.is_file() => Ok(answer(
                track_id,
                WaveformState::Failed,
                Some(Err("the file is missing".to_owned())),
            )),
            None => {
                self.request(track_id);
                Ok(answer(track_id, WaveformState::Pending, None))
            }
        }
    }

    fn request(&self, track_id: &str) {
        {
            let mut queue = lock(&self.shared.queue);
            if queue.working.as_deref() == Some(track_id) {
                return;
            }
            queue.waiting.retain(|waiting| waiting != track_id);
            queue.waiting.push_front(track_id.to_owned());
            queue.waiting.truncate(MAX_WAITING);
        }
        self.shared.wake.notify_one();
        let mut thread = lock(&self.thread);
        if thread.is_none() {
            let shared = Arc::clone(&self.shared);
            *thread = Some(
                thread::Builder::new()
                    .name("umak-waveforms".to_owned())
                    .spawn(move || run(&shared))
                    .expect("the waveform thread starts"),
            );
        }
    }
}

impl Drop for Waveforms {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        lock(&self.shared.queue).closing = true;
        self.shared.wake.notify_all();
        if let Some(thread) = lock(&self.thread).take() {
            let _ = thread.join();
        }
    }
}

fn run(shared: &Shared) {
    loop {
        let track_id = {
            let mut queue = lock(&shared.queue);
            loop {
                if queue.closing {
                    return;
                }
                if let Some(next) = queue.waiting.pop_front() {
                    queue.working = Some(next.clone());
                    break next;
                }
                queue = shared
                    .wake
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        if let Err(error) = work(shared, &track_id) {
            eprintln!(
                "failed to compute the waveform of {track_id}: {}",
                error.message
            );
        }
        lock(&shared.queue).working = None;
        if shared.stop.load(Ordering::SeqCst) {
            return;
        }
        shared.events.emit(&CoreEvent::WaveformReady { track_id });
    }
}

fn work(shared: &Shared, track_id: &str) -> Result<(), ApiError> {
    let Some(file) = shared.db.read(|conn| track_file(conn, track_id))? else {
        return Ok(());
    };
    let computed = compute(&file.path, &shared.stop);
    if shared.stop.load(Ordering::SeqCst) {
        return Ok(());
    }
    shared
        .db
        .write(|conn| save(conn, &file, &computed).map_err(DbError::from))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, Instant};

    use super::*;

    #[derive(Default)]
    struct Recording(Mutex<Vec<CoreEvent>>);

    impl EventSink for Recording {
        fn emit(&self, event: &CoreEvent) {
            lock(&self.0).push(event.clone());
        }
    }

    /// Two seconds of a 16-bit mono tone at half scale.
    fn wav(path: &Path) {
        let rate = 8_000_u32;
        let mut samples = Vec::new();
        for frame in 0..rate * 2 {
            let value = (frame as f32 * 400.0 * std::f32::consts::TAU / rate as f32).sin();
            samples.extend_from_slice(&((value * 0.5 * f32::from(i16::MAX)) as i16).to_le_bytes());
        }
        let mut file = b"RIFF".to_vec();
        file.extend((36 + samples.len() as u32).to_le_bytes());
        file.extend_from_slice(b"WAVEfmt ");
        file.extend(16_u32.to_le_bytes());
        file.extend(1_u16.to_le_bytes());
        file.extend(1_u16.to_le_bytes());
        file.extend(rate.to_le_bytes());
        file.extend((rate * 2).to_le_bytes());
        file.extend(2_u16.to_le_bytes());
        file.extend(16_u16.to_le_bytes());
        file.extend_from_slice(b"data");
        file.extend((samples.len() as u32).to_le_bytes());
        file.extend(samples);
        fs::write(path, file).unwrap();
    }

    struct Fixture {
        // Read only by the tests that decode.
        #[cfg_attr(not(feature = "engine"), allow(dead_code))]
        dir: tempfile::TempDir,
        db: Arc<Database>,
        events: Arc<Recording>,
        waveforms: Waveforms,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(dir.path().join("umak.db")).unwrap());
        let root = dir.path().to_string_lossy().into_owned();
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
            for (name, file) in [
                ("tone", "tone.wav"),
                ("bad", "bad.wav"),
                ("gone", "gone.wav"),
            ] {
                conn.execute(
                    "INSERT INTO tracks (public_id, root_id, rel_path, size, mtime, fingerprint,
                         title, album_id, title_sort, artist_sort, album_sort, album_artist_sort,
                         added_at, updated_at)
                     VALUES (?1, 1, ?2, 1, 1, x'0102', ?1, 1, ?1, 'x', 'x', 'x', 0, 0)",
                    params![name, file],
                )?;
            }
            Ok::<_, DbError>(())
        })
        .unwrap();
        wav(&dir.path().join("tone.wav"));
        fs::write(dir.path().join("bad.wav"), b"not audio at all").unwrap();
        let events = Arc::new(Recording::default());
        let sink: Arc<dyn EventSink> = events.clone();
        let waveforms = Waveforms::new(Arc::clone(&db), sink);
        Fixture {
            dir,
            db,
            events,
            waveforms,
        }
    }

    impl Fixture {
        fn wait_for(&self, track_id: &str) -> TrackWaveform {
            let started = Instant::now();
            loop {
                let ready = lock(&self.events.0).iter().any(|event| {
                    matches!(event, CoreEvent::WaveformReady { track_id: id } if id == track_id)
                });
                if ready {
                    lock(&self.events.0).clear();
                    return self.waveforms.get(track_id).unwrap();
                }
                assert!(
                    started.elapsed() < Duration::from_secs(20),
                    "no waveform for {track_id}"
                );
                thread::sleep(Duration::from_millis(5));
            }
        }

        #[cfg(feature = "engine")]
        fn set_identity(&self, size: i64, mtime: i64, fingerprint: &[u8]) {
            self.db
                .write(|conn| {
                    conn.execute(
                        "UPDATE tracks SET size = ?1, mtime = ?2, fingerprint = ?3
                         WHERE public_id = 'tone'",
                        params![size, mtime, fingerprint],
                    )
                    .map_err(DbError::from)
                })
                .unwrap();
        }
    }

    // Decoding needs the engine.
    #[cfg(feature = "engine")]
    #[test]
    fn a_waveform_is_computed_once_and_then_read_from_the_cache() {
        let fixture = fixture();
        let first = fixture.waveforms.get("tone").unwrap();
        assert_eq!(first.state, WaveformState::Pending);
        let ready = fixture.wait_for("tone");
        assert_eq!(ready.state, WaveformState::Ready);
        // Two seconds at 8 kHz is fewer blocks of 256 frames than there are columns, so each
        // block is a column, the last one partly filled.
        assert_eq!(ready.peaks.len(), 63);
        assert!(ready.peaks.iter().all(|peak| (126..=130).contains(peak)));
        assert!(ready.levels.iter().all(|level| (88..=92).contains(level)));

        // From the cache: no recompute, even with the file gone.
        fs::remove_file(fixture.dir.path().join("tone.wav")).unwrap();
        assert_eq!(fixture.waveforms.get("tone").unwrap(), ready);
        thread::sleep(Duration::from_millis(50));
        assert!(lock(&fixture.events.0).is_empty());
    }

    // Decoding needs the engine.
    #[cfg(feature = "engine")]
    #[test]
    fn a_changed_file_is_computed_again_unless_its_audio_is_the_same() {
        let fixture = fixture();
        fixture.waveforms.get("tone").unwrap();
        fixture.wait_for("tone");

        // A tag edit: a new size and time, the same audio.
        fixture.set_identity(2, 2, &[1, 2]);
        assert_eq!(
            fixture.waveforms.get("tone").unwrap().state,
            WaveformState::Ready
        );
        // Different audio.
        fixture.set_identity(3, 3, &[9]);
        assert_eq!(
            fixture.waveforms.get("tone").unwrap().state,
            WaveformState::Pending
        );
        assert_eq!(fixture.wait_for("tone").state, WaveformState::Ready);
    }

    #[test]
    fn failures_are_kept_but_a_missing_file_is_only_reported() {
        let fixture = fixture();
        assert_eq!(
            fixture.waveforms.get("bad").unwrap().state,
            WaveformState::Pending
        );
        let failed = fixture.wait_for("bad");
        assert_eq!(failed.state, WaveformState::Failed);
        assert!(failed.error.is_some());
        assert_eq!(fixture.waveforms.get("bad").unwrap(), failed);

        let gone = fixture.waveforms.get("gone").unwrap();
        assert_eq!(gone.state, WaveformState::Failed);
        assert_eq!(gone.error.as_deref(), Some("the file is missing"));
        let stored: i64 = fixture
            .db
            .read(|conn| {
                conn.query_row("SELECT count(*) FROM waveforms", [], |row| row.get(0))
                    .map_err(DbError::from)
            })
            .unwrap();
        assert_eq!(stored, 1, "only the damaged file's failure is stored");
        assert_eq!(
            fixture.waveforms.get("nothing").unwrap_err().code,
            ErrorCode::NotFound
        );
    }

    #[test]
    fn the_latest_request_goes_first() {
        let fixture = fixture();
        {
            // Hold the queue so both wait, then let the worker choose.
            let mut queue = lock(&fixture.waveforms.shared.queue);
            queue.waiting.push_front("bad".to_owned());
            queue.waiting.push_front("tone".to_owned());
        }
        fixture.waveforms.request("tone");
        let started = Instant::now();
        while lock(&fixture.events.0).len() < 2 {
            assert!(started.elapsed() < Duration::from_secs(20));
            thread::sleep(Duration::from_millis(5));
        }
        let order: Vec<String> = lock(&fixture.events.0)
            .iter()
            .filter_map(|event| match event {
                CoreEvent::WaveformReady { track_id } => Some(track_id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(order, ["tone", "bad"]);
    }
}
