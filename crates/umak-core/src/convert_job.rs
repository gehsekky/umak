//! Batches of conversions, run as jobs. Each file's outcome is recorded as it finishes, so a
//! batch shows what became of every file, and the files that failed or never ran can be
//! tried again as a new batch with the same settings.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use umak_convert::{ConvertError, Ffmpeg, partial_path};

use crate::api::{self, ApiError, CoreEvent, ErrorCode, EventSink};
use crate::convert::{self, ConvertCodec, ConvertFileStatus, ConvertSettings, PlannedFile};
use crate::db::{Database, DbError};
use crate::jobs::{JobContext, JobOutcome};
use crate::scan::{self, ScanOptions};
use crate::tagcopy;

/// How often a batch reports its progress.
const REPORT_EVERY: Duration = Duration::from_millis(250);
const MAX_PAGE: u32 = 500;

/// What a batch is asked to do, kept with it so it can be tried again. See
/// `api::StartConversion`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertRequest {
    pub settings: ConvertSettings,
    pub folder: String,
    pub threads: Option<u32>,
    pub add_to_library: bool,
}

pub fn default_threads() -> u32 {
    (available_threads() / 2).max(1)
}

fn available_threads() -> u32 {
    thread::available_parallelism().map_or(1, |count| count.get() as u32)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ConvertFileState {
    /// Waiting, or being converted.
    Pending,
    Converted,
    Failed,
    /// Not converted because the batch was canceled.
    Canceled,
    /// Left alone, as in the plan; see `ConvertFileStatus`.
    Exists,
    SameOutput,
    Missing,
}

impl ConvertFileState {
    const ALL: [Self; 7] = [
        Self::Pending,
        Self::Converted,
        Self::Failed,
        Self::Canceled,
        Self::Exists,
        Self::SameOutput,
        Self::Missing,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Converted => "converted",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::Exists => "exists",
            Self::SameOutput => "sameOutput",
            Self::Missing => "missing",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.as_str() == text)
    }

    fn planned(status: ConvertFileStatus) -> Self {
        match status {
            ConvertFileStatus::Ready => Self::Pending,
            ConvertFileStatus::Exists => Self::Exists,
            ConvertFileStatus::SameOutput => Self::SameOutput,
            ConvertFileStatus::Missing => Self::Missing,
        }
    }
}

/// How a batch's files stand, while it runs and once it ends.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ConvertReport {
    pub pending: u64,
    pub converted: u64,
    pub failed: u64,
    pub canceled: u64,
    pub exists: u64,
    pub same_output: u64,
    pub missing: u64,
    /// Converted files that lack some of their source's tag fields or pictures, because the
    /// new format has no place for them.
    pub incomplete_tags: u64,
    /// How many tracks the library gained, when the batch was to add its files.
    pub added_to_library: Option<u64>,
    /// Why the converted files could not be added to the library.
    pub library_error: Option<String>,
}

/// A batch checked and planned, ready to start.
pub struct Prepared {
    files: Vec<PlannedFile>,
    request: ConvertRequest,
    threads: u32,
    folder: PathBuf,
}

/// Checks a request and plans its files, so that mistakes are reported before a job starts.
/// Creates the output folder.
pub fn prepare(
    db: &Database,
    ffmpeg: &Ffmpeg,
    track_ids: &[String],
    request: ConvertRequest,
) -> Result<Prepared, ApiError> {
    let invalid = |message: String| ApiError::new(ErrorCode::InvalidArgs, message);
    request.settings.check()?;
    let encoder = request.settings.encoder();
    if !ffmpeg.has_encoder(encoder) {
        return Err(ApiError::new(
            ErrorCode::Unavailable,
            format!("this FFmpeg has no {encoder} encoder"),
        ));
    }
    let available = available_threads();
    let threads = request.threads.unwrap_or_else(default_threads);
    if threads == 0 || threads > available {
        return Err(invalid(format!(
            "the number of files at once must be from 1 to {available}"
        )));
    }
    if request.add_to_library && request.settings.codec == ConvertCodec::Opus {
        return Err(invalid(
            "Umak cannot play Opus, so Opus files cannot be added to the library".to_owned(),
        ));
    }
    if !Path::new(&request.folder).is_absolute() {
        return Err(invalid(format!("not an absolute path: {}", request.folder)));
    }
    fs::create_dir_all(&request.folder)
        .map_err(|error| invalid(format!("could not create {}: {error}", request.folder)))?;
    let folder = dunce::canonicalize(&request.folder)
        .map_err(|error| invalid(format!("{}: {error}", request.folder)))?;
    let folder_text = folder.to_string_lossy().into_owned();
    let files = db.read(|conn| {
        if request.add_to_library {
            library_target(conn, &folder)?;
        }
        convert::plan_files(conn, track_ids, &request.settings, &folder_text)
    })?;
    Ok(Prepared {
        files,
        request,
        threads,
        folder,
    })
}

/// The tracks a batch did not convert because they failed or never ran, with its request.
pub fn retry_request(
    conn: &Connection,
    job_id: &str,
) -> Result<(Vec<String>, ConvertRequest), ApiError> {
    let request = load_request(conn, job_id)?;
    let mut statement = conn.prepare_cached(
        "SELECT t.public_id FROM conversion_files f
         JOIN conversions c ON c.job_id = f.job_id
         JOIN jobs j ON j.id = c.job_id
         JOIN tracks t ON t.id = f.track_id
         WHERE j.public_id = ?1 AND f.status IN ('failed', 'canceled', 'pending')
         ORDER BY f.position",
    )?;
    let ids = statement
        .query_map([job_id], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    if ids.is_empty() {
        return Err(ApiError::new(
            ErrorCode::InvalidArgs,
            "every file of this batch was converted or left alone on purpose",
        ));
    }
    Ok((ids, request))
}

fn load_request(conn: &Connection, job_id: &str) -> Result<ConvertRequest, ApiError> {
    let row: Option<(String, String, u32, bool)> = conn
        .prepare_cached(
            "SELECT c.settings, c.folder, c.threads, c.add_to_library FROM conversions c
             JOIN jobs j ON j.id = c.job_id WHERE j.public_id = ?1",
        )?
        .query_row([job_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .optional()?;
    let (settings, folder, threads, add_to_library) = row
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("no conversion job {job_id}")))?;
    let settings = serde_json::from_str(&settings)
        .map_err(|error| ApiError::new(ErrorCode::Internal, error.to_string()))?;
    Ok(ConvertRequest {
        settings,
        folder,
        threads: Some(threads),
        add_to_library,
    })
}

/// Where converted files join the library.
enum LibraryTarget {
    /// Under an existing root, at this folder relative to it ("" for the root itself).
    Root { id: String, folder: String },
    /// The output folder becomes a root of its own.
    NewRoot,
}

fn library_target(conn: &Connection, folder: &Path) -> Result<LibraryTarget, ApiError> {
    let mut statement = conn.prepare_cached("SELECT public_id, path FROM library_roots")?;
    let roots = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (id, path) in roots {
        let root = Path::new(&path);
        if let Ok(inside) = folder.strip_prefix(root) {
            let folder = inside
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            return Ok(LibraryTarget::Root { id, folder });
        }
        if root.starts_with(folder) {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                format!(
                    "the folder holds the library folder {path}, so it cannot be added to the library"
                ),
            ));
        }
    }
    Ok(LibraryTarget::NewRoot)
}

/// One file's outcome, from a worker.
struct Finished {
    index: usize,
    state: ConvertFileState,
    message: Option<String>,
    left_out: Vec<String>,
}

/// Runs a prepared batch. Called on the job's thread.
pub fn run(
    db: &Database,
    events: &dyn EventSink,
    context: &JobContext,
    ffmpeg: &Ffmpeg,
    prepared: Prepared,
) -> Result<JobOutcome, String> {
    let Prepared {
        files,
        request,
        threads,
        folder,
    } = prepared;
    let job = db
        .write(|conn| record(conn, context.id(), &request, threads, &folder, &files))
        .map_err(|error| error.to_string())?;
    let cancel = context.cancel_flag();
    let ready: Vec<usize> = (0..files.len())
        .filter(|&index| files[index].status == ConvertFileStatus::Ready)
        .collect();
    let total = ready.len() as u64;
    context.progress(0, total);

    let next = AtomicUsize::new(0);
    let (sender, outcomes) = mpsc::channel::<Finished>();
    thread::scope(|scope| {
        for _ in 0..(threads as usize).min(ready.len()) {
            let sender = sender.clone();
            let (files, ready, next) = (&files, &ready, &next);
            scope.spawn(move || {
                while !cancel.load(Ordering::Relaxed) {
                    let Some(&index) = ready.get(next.fetch_add(1, Ordering::Relaxed)) else {
                        break;
                    };
                    let outcome = convert_one(ffmpeg, &files[index], cancel);
                    if sender.send(Finished { index, ..outcome }).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);

        let mut done = 0;
        let mut last_report = Instant::now();
        for finished in outcomes {
            let saved = db.write(|conn| save_outcome(conn, job, &finished));
            if let Err(error) = saved {
                eprintln!(
                    "failed to save a converted file of job {}: {error}",
                    context.id()
                );
            }
            if finished.state != ConvertFileState::Canceled {
                done += 1;
            }
            if last_report.elapsed() >= REPORT_EVERY {
                last_report = Instant::now();
                context.progress(done, total);
            }
        }
        context.progress(done, total);
    });

    let canceled = cancel.load(Ordering::Relaxed);
    if canceled {
        db.write(|conn| {
            conn.execute(
                "UPDATE conversion_files SET status = 'canceled'
                 WHERE job_id = ?1 AND status = 'pending'",
                [job],
            )?;
            Ok::<_, DbError>(())
        })
        .map_err(|error| error.to_string())?;
    }
    let mut report = db
        .read(|conn| counts(conn, job))
        .map_err(|error| error.to_string())?;
    if request.add_to_library && !canceled && report.converted > 0 {
        match add_to_library(db, events, context, &folder) {
            Ok(Some(added)) => report.added_to_library = Some(added),
            Ok(None) => {}
            Err(message) => report.library_error = Some(message),
        }
    }
    Ok(JobOutcome {
        canceled: cancel.load(Ordering::Relaxed),
        convert: Some(report),
        ..JobOutcome::default()
    })
}

fn convert_one(
    ffmpeg: &Ffmpeg,
    file: &PlannedFile,
    cancel: &std::sync::atomic::AtomicBool,
) -> Finished {
    let outcome = |state, message: Option<String>, left_out| Finished {
        index: 0,
        state,
        message,
        left_out,
    };
    let conversion = &file.conversion;
    if let Some(parent) = conversion.output.parent()
        && let Err(error) = fs::create_dir_all(parent)
    {
        return outcome(
            ConvertFileState::Failed,
            Some(format!("could not create {}: {error}", parent.display())),
            Vec::new(),
        );
    }
    let mut left_out = Vec::new();
    let result = ffmpeg.convert(
        conversion,
        cancel,
        |_| {},
        |partial| {
            let report = tagcopy::copy_tags(&conversion.input, partial)
                .map_err(|error| format!("could not copy the tags: {error}"))?;
            left_out = report.left_out;
            Ok(())
        },
    );
    match result {
        Ok(()) => outcome(ConvertFileState::Converted, None, left_out),
        Err(ConvertError::Canceled) => outcome(ConvertFileState::Canceled, None, Vec::new()),
        // Something else wrote there after the plan was made.
        Err(ConvertError::OutputExists(_)) => outcome(ConvertFileState::Exists, None, Vec::new()),
        Err(error) => outcome(
            ConvertFileState::Failed,
            Some(error.to_string()),
            Vec::new(),
        ),
    }
}

fn record(
    conn: &mut Connection,
    job_id: &str,
    request: &ConvertRequest,
    threads: u32,
    folder: &Path,
    files: &[PlannedFile],
) -> Result<i64, DbError> {
    let tx = conn.transaction()?;
    let job: i64 = tx.query_row(
        "SELECT id FROM jobs WHERE public_id = ?1",
        [job_id],
        |row| row.get(0),
    )?;
    let settings = serde_json::to_string(&request.settings).expect("settings serialize");
    tx.execute(
        "INSERT INTO conversions (job_id, settings, folder, threads, add_to_library)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            job,
            settings,
            folder.to_string_lossy(),
            threads,
            request.add_to_library
        ],
    )?;
    {
        let mut insert = tx.prepare_cached(
            "INSERT INTO conversion_files (job_id, position, track_id, source_path, output_path,
                                           status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for (position, file) in files.iter().enumerate() {
            insert.execute(params![
                job,
                position as i64,
                file.track,
                file.rel_path,
                file.conversion.output.to_string_lossy(),
                ConvertFileState::planned(file.status).as_str(),
            ])?;
        }
    }
    tx.commit()?;
    Ok(job)
}

fn save_outcome(conn: &Connection, job: i64, finished: &Finished) -> Result<(), DbError> {
    let left_out = (!finished.left_out.is_empty())
        .then(|| serde_json::to_string(&finished.left_out).expect("names serialize"));
    conn.prepare_cached(
        "UPDATE conversion_files SET status = ?3, message = ?4, left_out = ?5
         WHERE job_id = ?1 AND position = ?2",
    )?
    .execute(params![
        job,
        finished.index as i64,
        finished.state.as_str(),
        finished.message,
        left_out
    ])?;
    Ok(())
}

fn counts(conn: &Connection, job: i64) -> Result<ConvertReport, DbError> {
    let mut report = ConvertReport::default();
    let mut statement = conn.prepare_cached(
        "SELECT status, count(*), count(left_out) FROM conversion_files WHERE job_id = ?1
         GROUP BY status",
    )?;
    let rows = statement.query_map([job], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)? as u64,
            row.get::<_, i64>(2)? as u64,
        ))
    })?;
    for row in rows {
        let (status, count, incomplete) = row?;
        let field = match ConvertFileState::parse(&status) {
            Some(ConvertFileState::Pending) => &mut report.pending,
            Some(ConvertFileState::Converted) => &mut report.converted,
            Some(ConvertFileState::Failed) => &mut report.failed,
            Some(ConvertFileState::Canceled) => &mut report.canceled,
            Some(ConvertFileState::Exists) => &mut report.exists,
            Some(ConvertFileState::SameOutput) => &mut report.same_output,
            Some(ConvertFileState::Missing) => &mut report.missing,
            None => continue,
        };
        *field = count;
        report.incomplete_tags += incomplete;
    }
    Ok(report)
}

/// Scans the output folder into the library once no other job is changing it. Returns how
/// many tracks were added, or `None` if the job was canceled first.
fn add_to_library(
    db: &Database,
    events: &dyn EventSink,
    context: &JobContext,
    folder: &Path,
) -> Result<Option<u64>, String> {
    let Some(_claim) = context.claim_library() else {
        return Ok(None);
    };
    // Looked up again, since the roots may have changed while the batch ran.
    let target = db
        .read(|conn| library_target(conn, folder))
        .map_err(|error| error.message)?;
    let cancel = context.cancel_flag();
    let mut progress = |_| {};
    let report = match target {
        LibraryTarget::Root { id, folder } if !folder.is_empty() => scan::scan_paths(
            db,
            &id,
            &[folder],
            ScanOptions::default(),
            cancel,
            &mut progress,
        ),
        LibraryTarget::Root { id, .. } => scan::scan_library(
            db,
            Some(&[id]),
            ScanOptions::default(),
            cancel,
            &mut progress,
        ),
        LibraryTarget::NewRoot => {
            let root = api::add_root(db, &folder.to_string_lossy(), None)
                .map_err(|error| error.message)?;
            events.emit(&CoreEvent::LibraryRootsChanged);
            scan::scan_library(
                db,
                Some(&[root.id]),
                ScanOptions::default(),
                cancel,
                &mut progress,
            )
        }
    }
    .map_err(|error| error.to_string())?;
    if report.changed() {
        events.emit(&CoreEvent::LibraryChanged);
    }
    Ok(Some(report.added))
}

/// A batch's request and how its files stand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ConversionInfo {
    pub job_id: String,
    /// The job's status: running, completed, canceled or failed.
    pub status: String,
    /// Why the job itself failed. Files that failed are counted in the report instead.
    pub error: Option<String>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    pub settings: ConvertSettings,
    pub folder: String,
    pub threads: u32,
    pub add_to_library: bool,
    pub report: ConvertReport,
}

const INFO_COLUMNS: &str = "j.id, j.public_id, j.status, j.error, j.created_at, j.finished_at,
     j.payload, c.settings, c.folder, c.threads, c.add_to_library
     FROM conversions c JOIN jobs j ON j.id = c.job_id";

fn read_info(conn: &Connection, row: &rusqlite::Row) -> Result<ConversionInfo, ApiError> {
    let job: i64 = row.get(0)?;
    // A finished batch keeps its report, with the library result, in its job.
    let payload: Option<String> = row.get(6)?;
    let report = payload
        .and_then(|text| serde_json::from_str(&text).ok())
        .map_or_else(|| counts(conn, job), Ok)?;
    let settings: String = row.get(7)?;
    Ok(ConversionInfo {
        job_id: row.get(1)?,
        status: row.get(2)?,
        error: row.get(3)?,
        created_at: row.get(4)?,
        finished_at: row.get(5)?,
        settings: serde_json::from_str(&settings)
            .map_err(|error| ApiError::new(ErrorCode::Internal, error.to_string()))?,
        folder: row.get(8)?,
        threads: row.get(9)?,
        add_to_library: row.get(10)?,
        report,
    })
}

pub fn info(conn: &Connection, job_id: &str) -> Result<ConversionInfo, ApiError> {
    let mut statement =
        conn.prepare_cached(&format!("SELECT {INFO_COLUMNS} WHERE j.public_id = ?1"))?;
    let mut rows = statement.query([job_id])?;
    match rows.next()? {
        Some(row) => read_info(conn, row),
        None => Err(ApiError::new(
            ErrorCode::NotFound,
            format!("no conversion job {job_id}"),
        )),
    }
}

/// The most recent batches, newest first.
pub fn list(conn: &Connection, limit: u32) -> Result<Vec<ConversionInfo>, ApiError> {
    let mut statement = conn.prepare_cached(&format!(
        "SELECT {INFO_COLUMNS} ORDER BY j.created_at DESC, j.id DESC LIMIT ?1"
    ))?;
    let mut rows = statement.query([limit])?;
    let mut conversions = Vec::new();
    while let Some(row) = rows.next()? {
        conversions.push(read_info(conn, row)?);
    }
    Ok(conversions)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ConvertFile {
    /// `None` once the track has left the library.
    pub track_id: Option<String>,
    /// The source, under its library root.
    pub source_path: String,
    pub output_path: String,
    pub state: ConvertFileState,
    pub message: Option<String>,
    /// Tag fields and pictures the converted file lacks, by their names in the source.
    pub left_out: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ConvertFilePage {
    pub total: u32,
    pub files: Vec<ConvertFile>,
}

/// A batch's files in order. With `problems_only`, only those not converted, or converted
/// without all of their tags.
pub fn files(
    conn: &Connection,
    job_id: &str,
    problems_only: bool,
    offset: u32,
    limit: u32,
) -> Result<ConvertFilePage, ApiError> {
    let job: i64 = conn
        .query_row(
            "SELECT c.job_id FROM conversions c JOIN jobs j ON j.id = c.job_id
             WHERE j.public_id = ?1",
            [job_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("no conversion job {job_id}")))?;
    let filter = if problems_only {
        "AND (f.status <> 'converted' OR f.left_out IS NOT NULL)"
    } else {
        ""
    };
    let total: u32 = conn.query_row(
        &format!("SELECT count(*) FROM conversion_files f WHERE f.job_id = ?1 {filter}"),
        [job],
        |row| row.get(0),
    )?;
    let mut statement = conn.prepare_cached(&format!(
        "SELECT t.public_id, f.source_path, f.output_path, f.status, f.message, f.left_out
         FROM conversion_files f LEFT JOIN tracks t ON t.id = f.track_id
         WHERE f.job_id = ?1 {filter}
         ORDER BY f.position LIMIT ?2 OFFSET ?3"
    ))?;
    let files = statement
        .query_map(params![job, limit.min(MAX_PAGE), offset], |row| {
            let status: String = row.get(3)?;
            let left_out: Option<String> = row.get(5)?;
            Ok(ConvertFile {
                track_id: row.get(0)?,
                source_path: row.get(1)?,
                output_path: row.get(2)?,
                state: ConvertFileState::parse(&status).unwrap_or(ConvertFileState::Failed),
                message: row.get(4)?,
                left_out: left_out
                    .and_then(|text| serde_json::from_str(&text).ok())
                    .unwrap_or_default(),
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(ConvertFilePage { total, files })
}

/// Files left waiting by a process that ended mid-batch are marked failed, and whatever
/// partial output they left is removed. FFmpeg ends with the process, so nothing holds them.
pub fn recover_interrupted(db: &Database) -> Result<(), DbError> {
    let waiting: Vec<String> = db.read(|conn| {
        let mut statement =
            conn.prepare("SELECT output_path FROM conversion_files WHERE status = 'pending'")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        Ok::<_, DbError>(rows.collect::<Result<_, _>>()?)
    })?;
    if waiting.is_empty() {
        return Ok(());
    }
    for output in &waiting {
        let _ = fs::remove_file(partial_path(Path::new(output)));
    }
    db.write(|conn| {
        conn.execute(
            "UPDATE conversion_files
             SET status = 'failed', message = 'the app stopped before this file was converted'
             WHERE status = 'pending'",
            [],
        )?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_states_read_back_as_written() {
        for state in ConvertFileState::ALL {
            assert_eq!(ConvertFileState::parse(state.as_str()), Some(state));
        }
    }

    #[test]
    fn a_batch_cut_short_by_the_app_stopping_is_cleaned_up_and_can_be_retried() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("umak.db")).unwrap();
        let output = dir.path().join("out").join("song.mp3");
        fs::create_dir_all(output.parent().unwrap()).unwrap();
        let partial = partial_path(&output);
        fs::write(&partial, b"half a file").unwrap();
        db.write(|conn| {
            conn.execute(
                "INSERT INTO jobs (public_id, kind, status, created_at) VALUES ('job', 'convert', 'failed', 0)",
                [],
            )?;
            conn.execute(
                "INSERT INTO conversions (job_id, settings, folder, threads, add_to_library)
                 VALUES (1, '{\"codec\":\"flac\"}', 'out', 2, 0)",
                [],
            )?;
            conn.execute(
                "INSERT INTO conversion_files (job_id, position, source_path, output_path, status)
                 VALUES (1, 0, 'a/song.flac', ?1, 'pending'),
                        (1, 1, 'a/other.flac', ?2, 'converted')",
                params![output.to_string_lossy(), dir.path().join("other").to_string_lossy()],
            )?;
            Ok::<_, DbError>(())
        })
        .unwrap();

        recover_interrupted(&db).unwrap();
        assert!(!partial.exists());
        let page = db.read(|conn| files(conn, "job", false, 0, 10)).unwrap();
        assert_eq!(page.files[0].state, ConvertFileState::Failed);
        assert_eq!(page.files[1].state, ConvertFileState::Converted);
        let report = db.read(|conn| info(conn, "job")).unwrap().report;
        assert_eq!((report.failed, report.converted), (1, 1));
    }
}
