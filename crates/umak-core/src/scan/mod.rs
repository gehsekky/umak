//! Library import. A scan finds what changed, marks what disappeared, then reads the
//! changed files on worker threads while this thread writes them in batches.
//!
//! Everything after the first step works on a set of changed paths, so a file watcher
//! can feed `scan_paths` directly. Music files are only ever opened for reading.

pub mod fingerprint;
pub mod normalize;
pub mod store;
pub mod tags;
pub mod walk;

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::db::{Database, DbError};
use crate::settings;
use store::{Applied, Catalog, FileRecord};
use tags::FileInfo;
use walk::{DiskFile, Walk};

const BATCH_ROWS: usize = 2000;
/// A batch commits at least this often, so the UI sees progress on a slow disk.
const BATCH_INTERVAL: Duration = Duration::from_secs(1);
const RESULT_QUEUE: usize = 256;
const MAX_REPORTED_ISSUES: usize = 200;

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("unknown library folder")]
    UnknownRoot,
    #[error("{0}")]
    Settings(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanOptions {
    /// Worker threads reading files. A hard drive may do best with very few.
    pub threads: usize,
    /// Audio samples per fingerprint, each past the first costing a disk seek. Zero
    /// disables fingerprints, and with them the detection of moved files.
    pub fingerprint_samples: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            threads: 4,
            fingerprint_samples: 1,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ScanIssue {
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub files_found: u64,
    pub unchanged: u64,
    pub added: u64,
    pub updated: u64,
    pub moved: u64,
    pub missing: u64,
    pub restored: u64,
    /// Files stored without tags because their tags could not be read.
    pub tag_errors: u64,
    /// Files skipped because they could not be opened.
    pub read_errors: u64,
    /// Roots left untouched because they could not be listed or appear empty.
    pub offline_roots: Vec<String>,
    /// The first problems found; the counts above cover all of them.
    pub issues: Vec<ScanIssue>,
    pub canceled: bool,
    pub threads: u64,
    pub walk_ms: u64,
    /// Thread time summed over the workers, including the file reads each one made and
    /// the opening of the file.
    pub tag_ms: u64,
    pub fingerprint_ms: u64,
    /// The part of that thread time spent waiting on file opens, reads and seeks.
    pub io_ms: u64,
    pub bytes_read: u64,
    pub read_calls: u64,
    pub write_ms: u64,
    pub total_ms: u64,
}

impl ScanReport {
    fn add_issue(&mut self, path: &str, message: String) {
        if self.issues.len() < MAX_REPORTED_ISSUES {
            self.issues.push(ScanIssue {
                path: path.to_owned(),
                message,
            });
        }
    }

    /// Whether the library's contents changed.
    pub fn changed(&self) -> bool {
        self.added + self.updated + self.moved + self.missing + self.restored > 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanProgress {
    pub done: u64,
    pub total: u64,
}

#[derive(Debug, Clone)]
struct Root {
    id: i64,
    path: PathBuf,
}

#[derive(Debug)]
struct KnownTrack {
    id: i64,
    size: i64,
    mtime: i64,
    missing: bool,
}

struct Job {
    root_id: i64,
    root_path: Arc<PathBuf>,
    file: DiskFile,
    existing: Option<i64>,
}

#[derive(Default)]
struct Plan {
    jobs: Vec<Job>,
    gone: Vec<i64>,
    returned: Vec<i64>,
    unchanged: u64,
    /// Whether every file is read again, unchanged or not, to sort by new articles.
    resort: bool,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

fn select_roots(conn: &Connection, only: Option<&[String]>) -> Result<Vec<Root>, ScanError> {
    let mut statement =
        conn.prepare("SELECT id, public_id, path FROM library_roots ORDER BY id")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let all = rows.collect::<Result<Vec<_>, _>>()?;
    if let Some(wanted) = only
        && let Some(_unknown) = wanted
            .iter()
            .find(|id| !all.iter().any(|(_, public_id, _)| public_id == *id))
    {
        return Err(ScanError::UnknownRoot);
    }
    Ok(all
        .into_iter()
        .filter(|(_, public_id, _)| only.is_none_or(|wanted| wanted.contains(public_id)))
        .map(|(id, _, path)| Root {
            id,
            path: PathBuf::from(path),
        })
        .collect())
}

fn known_tracks(conn: &Connection, root: i64) -> rusqlite::Result<HashMap<String, KnownTrack>> {
    let mut statement = conn.prepare(
        "SELECT rel_path, id, size, mtime, missing_since IS NOT NULL FROM tracks WHERE root_id = ?1",
    )?;
    let rows = statement.query_map([root], |row| {
        Ok((
            row.get::<_, String>(0)?,
            KnownTrack {
                id: row.get(1)?,
                size: row.get(2)?,
                mtime: row.get(3)?,
                missing: row.get(4)?,
            },
        ))
    })?;
    rows.collect()
}

fn is_under(path: &str, folder: &str) -> bool {
    folder.is_empty()
        || path
            .strip_prefix(folder)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Compares files on disk with the database. `is_covered` says whether a known track
/// should have been seen, so a folder that could not be listed loses no tracks.
fn add_to_plan(
    plan: &mut Plan,
    root: &Root,
    known: &HashMap<String, KnownTrack>,
    files: Vec<DiskFile>,
    is_covered: impl Fn(&str) -> bool,
) {
    let root_path = Arc::new(root.path.clone());
    let mut seen = std::collections::HashSet::with_capacity(files.len());
    for file in files {
        seen.insert(file.rel_path.clone());
        match known.get(&file.rel_path) {
            Some(track) if track.size == file.size && track.mtime == file.mtime && !plan.resort => {
                if track.missing {
                    plan.returned.push(track.id);
                }
                plan.unchanged += 1;
            }
            existing => plan.jobs.push(Job {
                root_id: root.id,
                root_path: Arc::clone(&root_path),
                file,
                existing: existing.map(|track| track.id),
            }),
        }
    }
    for (path, track) in known {
        if !track.missing && !seen.contains(path) && is_covered(path) {
            plan.gone.push(track.id);
        }
    }
}

/// Scans whole roots, or every root when `only` is `None`.
pub fn scan_library(
    db: &Database,
    only: Option<&[String]>,
    options: ScanOptions,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(ScanProgress),
) -> Result<ScanReport, ScanError> {
    let started = Instant::now();
    let mut report = ScanReport::default();
    let roots = db.read(|conn| select_roots(conn, only))?;
    let (rules, articles, sorted_with) = db.read(|conn| {
        Ok::<_, ScanError>((
            settings::walk_rules(conn),
            settings::sort_articles(conn),
            settings::sorted_with(conn).map_err(ScanError::Settings)?,
        ))
    })?;
    let mut plan = Plan {
        resort: articles != sorted_with,
        ..Plan::default()
    };
    for root in &roots {
        let known = db.read(|conn| Ok::<_, ScanError>(known_tracks(conn, root.id)?))?;
        let walked = walk::walk(&root.path, "", &rules);
        let is_offline = walked.unreadable.iter().any(String::is_empty)
            || (walked.files.is_empty() && !known.is_empty());
        if is_offline {
            report.offline_roots.push(root.path.display().to_string());
            continue;
        }
        let Walk {
            files,
            unreadable,
            invalid_names,
        } = walked;
        for folder in &unreadable {
            report.add_issue(folder, "could not list this folder".to_owned());
        }
        for name in &invalid_names {
            report.read_errors += 1;
            report.add_issue(name, "the file name is not valid Unicode".to_owned());
        }
        report.files_found += files.len() as u64;
        add_to_plan(&mut plan, root, &known, files, |path| {
            !unreadable.iter().any(|folder| is_under(path, folder))
        });
    }
    report.walk_ms = started.elapsed().as_millis() as u64;
    let resort = plan.resort;
    run_plan(db, plan, options, cancel, progress, &mut report)?;
    // Only a whole library read through can be called sorted, and then even the tracks of a
    // folder that was offline keep their old values until they are read.
    if resort && only.is_none() && !report.canceled {
        db.write(|conn| settings::save_sorted_with(conn, &articles).map_err(ScanError::Settings))?;
    }
    report.total_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

/// Scans specific files or folders under one root, given relative to it. A path that
/// no longer exists marks its track, or every track under it, as missing.
pub fn scan_paths(
    db: &Database,
    root_id: &str,
    rel_paths: &[String],
    options: ScanOptions,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(ScanProgress),
) -> Result<ScanReport, ScanError> {
    let started = Instant::now();
    let mut report = ScanReport::default();
    let wanted = [root_id.to_owned()];
    let root = db
        .read(|conn| select_roots(conn, Some(&wanted)))?
        .pop()
        .ok_or(ScanError::UnknownRoot)?;
    let known = db.read(|conn| Ok::<_, ScanError>(known_tracks(conn, root.id)?))?;
    let rules = db.read(|conn| Ok::<_, ScanError>(settings::walk_rules(conn)))?;

    let mut files = Vec::new();
    for rel_path in rel_paths {
        match fs::metadata(root.path.join(rel_path)) {
            Ok(metadata) if metadata.is_dir() => {
                files.extend(walk::walk(&root.path, rel_path, &rules).files);
            }
            Ok(metadata) if rules.is_audio_file(rel_path) => files.push(DiskFile {
                rel_path: rel_path.clone(),
                size: metadata.len() as i64,
                mtime: walk::modified_ms(&metadata),
            }),
            _ => {}
        }
    }
    report.files_found = files.len() as u64;

    let mut plan = Plan::default();
    add_to_plan(&mut plan, &root, &known, files, |path| {
        rel_paths
            .iter()
            .any(|changed| path == changed || is_under(path, changed))
    });
    run_plan(db, plan, options, cancel, progress, &mut report)?;
    report.total_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

#[derive(Default)]
struct WorkerTimes {
    tag_ns: AtomicU64,
    fingerprint_ns: AtomicU64,
    /// Thread time inside file open, read and seek calls.
    io_ns: AtomicU64,
    bytes_read: AtomicU64,
    read_calls: AtomicU64,
}

/// A file that records the time spent waiting on the OS, so a scan can tell disk time
/// from time in its own code.
#[derive(Clone, Copy)]
struct TimedFile<'a> {
    file: &'a File,
    times: &'a WorkerTimes,
}

impl TimedFile<'_> {
    fn timed<T>(&self, work: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        let result = work();
        self.times
            .io_ns
            .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
        result
    }
}

impl Read for TimedFile<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut file = self.file;
        let count = self.timed(|| file.read(buffer))?;
        self.times
            .bytes_read
            .fetch_add(count as u64, Ordering::Relaxed);
        self.times.read_calls.fetch_add(1, Ordering::Relaxed);
        Ok(count)
    }
}

impl Seek for TimedFile<'_> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let mut file = self.file;
        self.timed(|| file.seek(position))
    }
}

struct Scanned {
    job_index: usize,
    outcome: Result<ReadFile, String>,
}

struct ReadFile {
    info: FileInfo,
    fingerprint: Option<Vec<u8>>,
    tag_error: Option<String>,
}

fn read_one(job: &Job, samples: usize, times: &WorkerTimes) -> Result<ReadFile, String> {
    let path: PathBuf = Path::new(job.root_path.as_path()).join(&job.file.rel_path);
    let started = Instant::now();
    let opened = File::open(&path);
    times
        .io_ns
        .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
    let file = opened.map_err(|error| error.to_string())?;
    let timed = TimedFile { file: &file, times };

    // A malformed file must not take the scan down with it.
    let parsed = panic::catch_unwind(AssertUnwindSafe(|| tags::read_file(timed)))
        .unwrap_or_else(|_| Err("the tag reader failed on this file".to_owned()));
    times
        .tag_ns
        .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);

    let started = Instant::now();
    let fingerprint = if samples == 0 {
        None
    } else {
        let mut reader = timed;
        fingerprint::fingerprint(&mut reader, samples).ok()
    };
    times
        .fingerprint_ns
        .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);

    let (info, tag_error) = match parsed {
        Ok(info) => (info, None),
        Err(error) => (FileInfo::default(), Some(error)),
    };
    Ok(ReadFile {
        info,
        fingerprint,
        tag_error,
    })
}

fn run_plan(
    db: &Database,
    plan: Plan,
    options: ScanOptions,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(ScanProgress),
    report: &mut ScanReport,
) -> Result<(), ScanError> {
    let Plan {
        jobs,
        gone,
        returned,
        unchanged,
        resort,
    } = plan;
    let threads = options.threads.max(1);
    report.threads = threads as u64;
    report.unchanged = unchanged;
    report.restored = returned.len() as u64;

    let now = now_ms();
    let write_started = Instant::now();
    // Tracks that vanished are marked first, so a new file can match one by
    // fingerprint and take over its ID.
    if !gone.is_empty() || !returned.is_empty() {
        db.write(|conn| {
            let tx = conn.transaction()?;
            store::mark_missing(&tx, &gone, now)?;
            store::mark_present(&tx, &returned)?;
            tx.commit()?;
            Ok::<_, ScanError>(())
        })?;
    }
    let mut write_time = write_started.elapsed();

    let total = jobs.len() as u64;
    let mut done = 0;
    progress(ScanProgress { done, total });

    let next = AtomicUsize::new(0);
    let times = WorkerTimes::default();
    let mut catalog = db.read(|conn| Ok::<_, ScanError>(Catalog::new(conn)))?;
    catalog.resort = resort;

    thread::scope(|scope| -> Result<(), ScanError> {
        let (sender, receiver) = mpsc::sync_channel::<Scanned>(RESULT_QUEUE);
        for _ in 0..threads {
            let sender = sender.clone();
            let (jobs, next, times) = (&jobs, &next, &times);
            scope.spawn(move || {
                while !cancel.load(Ordering::Relaxed) {
                    let job_index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(job) = jobs.get(job_index) else {
                        break;
                    };
                    let outcome = read_one(job, options.fingerprint_samples, times);
                    if sender.send(Scanned { job_index, outcome }).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);

        let mut batch = Vec::new();
        let mut last_commit = Instant::now();
        loop {
            let wait = BATCH_INTERVAL.saturating_sub(last_commit.elapsed());
            let open = match receiver.recv_timeout(wait) {
                Ok(item) => {
                    batch.push(item);
                    true
                }
                Err(RecvTimeoutError::Timeout) => true,
                Err(RecvTimeoutError::Disconnected) => false,
            };
            let is_due = batch.len() >= BATCH_ROWS || last_commit.elapsed() >= BATCH_INTERVAL;
            if !batch.is_empty() && (is_due || !open) {
                let started = Instant::now();
                done += batch.len() as u64;
                write_batch(db, &jobs, &batch, &mut catalog, now, report)?;
                batch.clear();
                write_time += started.elapsed();
                progress(ScanProgress { done, total });
            }
            if is_due {
                last_commit = Instant::now();
            }
            if !open {
                return Ok(());
            }
        }
    })?;

    let started = Instant::now();
    if catalog.changed {
        db.write(|conn| {
            let tx = conn.transaction()?;
            store::refresh_albums(&tx, &catalog.touched_albums)?;
            store::remove_orphans(&tx)?;
            tx.commit()?;
            Ok::<_, ScanError>(())
        })?;
    }
    report.missing = db.read(|conn| Ok::<_, ScanError>(store::count_missing(conn, &gone)?))?;
    write_time += started.elapsed();

    report.canceled = cancel.load(Ordering::Relaxed) && done < total;
    report.tag_ms = times.tag_ns.load(Ordering::Relaxed) / 1_000_000;
    report.fingerprint_ms = times.fingerprint_ns.load(Ordering::Relaxed) / 1_000_000;
    report.write_ms = write_time.as_millis() as u64;
    report.io_ms = times.io_ns.load(Ordering::Relaxed) / 1_000_000;
    report.bytes_read = times.bytes_read.load(Ordering::Relaxed);
    report.read_calls = times.read_calls.load(Ordering::Relaxed);
    Ok(())
}

fn write_batch(
    db: &Database,
    jobs: &[Job],
    batch: &[Scanned],
    catalog: &mut Catalog,
    now: i64,
    report: &mut ScanReport,
) -> Result<(), ScanError> {
    db.write(|conn| {
        let tx = conn.transaction()?;
        for item in batch {
            let job = &jobs[item.job_index];
            let read = match &item.outcome {
                Ok(read) => read,
                Err(message) => {
                    report.read_errors += 1;
                    report.add_issue(&job.file.rel_path, message.clone());
                    continue;
                }
            };
            if let Some(message) = &read.tag_error {
                report.tag_errors += 1;
                report.add_issue(&job.file.rel_path, message.clone());
            }
            let record = FileRecord {
                root_id: job.root_id,
                rel_path: &job.file.rel_path,
                size: job.file.size,
                mtime: job.file.mtime,
                existing: job.existing,
                fingerprint: read.fingerprint.as_deref(),
                info: &read.info,
            };
            match store::apply_file(&tx, catalog, &record, now)? {
                Applied::Added => report.added += 1,
                Applied::Updated => report.updated += 1,
                Applied::Moved => report.moved += 1,
            }
        }
        tx.commit()?;
        Ok(())
    })
}

#[cfg(test)]
pub(crate) mod tests;
