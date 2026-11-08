//! Long operations run as background jobs: recorded in the database, cancelable, and
//! reporting progress as events, so any client can show them. Jobs that change the library
//! run one at a time; a conversion only reads it, so it runs beside them, one at a time too.

use std::collections::HashMap;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use ts_rs::TS;

use crate::api::{ApiError, CoreEvent, ErrorCode, EventSink};
use crate::convert_job::ConvertReport;
use crate::db::{Database, DbError, new_public_id};
use crate::scan::ScanReport;
use crate::transfer::ImportReport;

const LISTED_JOBS: i64 = 50;
/// How often a job waiting for the library looks again.
const LIBRARY_WAIT: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum JobKind {
    Scan,
    Import,
    TagPreview,
    TagWrite,
    Convert,
}

/// Jobs in the same lane run one at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lane {
    Library,
    Convert,
}

impl JobKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Scan => "scan",
            Self::Import => "import",
            Self::TagPreview => "tagPreview",
            Self::TagWrite => "tagWrite",
            Self::Convert => "convert",
        }
    }

    fn lane(self) -> Lane {
        match self {
            Self::Convert => Lane::Convert,
            Self::Scan | Self::Import | Self::TagPreview | Self::TagWrite => Lane::Library,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Scan => "scan",
            Self::Import => "import",
            Self::TagPreview => "tag preview",
            Self::TagWrite => "tag write",
            Self::Convert => "conversion",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct JobInfo {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub done: u64,
    pub total: Option<u64>,
    pub error: Option<String>,
    pub scan: Option<ScanReport>,
    pub import: Option<ImportReport>,
    pub convert: Option<ConvertReport>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
}

/// What a finished job reports.
#[derive(Default)]
pub struct JobOutcome {
    pub canceled: bool,
    pub scan: Option<ScanReport>,
    pub import: Option<ImportReport>,
    pub convert: Option<ConvertReport>,
}

/// A running job's view of the runner.
pub struct JobContext {
    id: String,
    kind: JobKind,
    cancel: Arc<AtomicBool>,
    db: Arc<Database>,
    events: Arc<dyn EventSink>,
    running: Arc<Mutex<HashMap<String, Running>>>,
}

impl JobContext {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn cancel_flag(&self) -> &AtomicBool {
        &self.cancel
    }

    /// Waits until no job is changing the library, then holds the others off until the claim
    /// is dropped, for a job in another lane that ends by changing the library. Returns
    /// `None` if the job is canceled while it waits.
    pub fn claim_library(&self) -> Option<LibraryClaim> {
        let key = format!("{}/library", self.id);
        loop {
            {
                let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
                if !running.values().any(|job| job.lane == Lane::Library) {
                    running.insert(
                        key.clone(),
                        Running {
                            kind: self.kind,
                            lane: Lane::Library,
                            cancel: Arc::clone(&self.cancel),
                        },
                    );
                    return Some(LibraryClaim {
                        key,
                        running: Arc::clone(&self.running),
                    });
                }
            }
            if self.cancel.load(Ordering::Relaxed) {
                return None;
            }
            thread::sleep(LIBRARY_WAIT);
        }
    }

    pub fn progress(&self, done: u64, total: u64) {
        let saved = self.db.write(|conn| {
            conn.prepare_cached(
                "UPDATE jobs SET progress_done = ?2, progress_total = ?3 WHERE public_id = ?1",
            )?
            .execute(params![self.id, done as i64, total as i64])?;
            Ok::<_, DbError>(())
        });
        if let Err(error) = saved {
            eprintln!("failed to save progress of job {}: {error}", self.id);
        }
        self.events.emit(&CoreEvent::JobProgress {
            id: self.id.clone(),
            kind: self.kind,
            done,
            total,
        });
    }
}

struct Running {
    kind: JobKind,
    lane: Lane,
    cancel: Arc<AtomicBool>,
}

/// Holds the library for a job from another lane. See `JobContext::claim_library`.
pub struct LibraryClaim {
    key: String,
    running: Arc<Mutex<HashMap<String, Running>>>,
}

impl Drop for LibraryClaim {
    fn drop(&mut self) {
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.key);
    }
}

pub struct JobRunner {
    db: Arc<Database>,
    events: Arc<dyn EventSink>,
    running: Arc<Mutex<HashMap<String, Running>>>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

impl JobRunner {
    /// Jobs still marked active belonged to a process that ended without finishing them.
    pub fn new(db: Arc<Database>, events: Arc<dyn EventSink>) -> Result<Self, DbError> {
        db.write(|conn| {
            conn.execute(
                "UPDATE jobs SET status = 'failed', error = 'interrupted', finished_at = ?1
                 WHERE status IN ('queued', 'running')",
                [now_ms()],
            )?;
            Ok::<_, DbError>(())
        })?;
        Ok(Self {
            db,
            events,
            running: Arc::default(),
        })
    }

    /// Starts `work` on its own thread and returns the job's ID.
    pub fn start(
        &self,
        kind: JobKind,
        work: impl FnOnce(&JobContext) -> Result<JobOutcome, String> + Send + 'static,
    ) -> Result<String, ApiError> {
        let id = new_public_id();
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(job) = running.values().find(|job| job.lane == kind.lane()) {
                let message = if job.kind.lane() == job.lane {
                    format!("a {} job is already running", job.kind.label())
                } else {
                    format!("a {} job is adding files to the library", job.kind.label())
                };
                return Err(ApiError {
                    code: ErrorCode::Conflict,
                    message,
                });
            }
            self.db.write(|conn| insert_job(conn, &id, kind))?;
            running.insert(
                id.clone(),
                Running {
                    kind,
                    lane: kind.lane(),
                    cancel: Arc::clone(&cancel),
                },
            );
        }

        let context = JobContext {
            id: id.clone(),
            kind,
            cancel,
            db: Arc::clone(&self.db),
            events: Arc::clone(&self.events),
            running: Arc::clone(&self.running),
        };
        let running = Arc::clone(&self.running);
        thread::spawn(move || {
            let result = panic::catch_unwind(AssertUnwindSafe(|| work(&context)))
                .unwrap_or_else(|_| Err("the job stopped unexpectedly".to_owned()));
            let saved = context
                .db
                .write(|conn| finish_job(conn, &context.id, &result));
            if let Err(error) = saved {
                eprintln!("failed to save the result of job {}: {error}", context.id);
            }
            running
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&context.id);
            context.events.emit(&CoreEvent::JobFinished {
                id: context.id.clone(),
                kind: context.kind,
            });
        });
        Ok(id)
    }

    /// Asks a job to stop. It finishes the work in hand first.
    pub fn cancel(&self, id: &str) -> bool {
        let running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        match running.get(id) {
            Some(job) => {
                job.cancel.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    pub fn list(&self) -> Result<Vec<JobInfo>, DbError> {
        self.db.read(|conn| {
            let mut statement = conn.prepare_cached(
                "SELECT public_id, kind, status, progress_done, progress_total, error, payload,
                        created_at, finished_at
                 FROM jobs ORDER BY created_at DESC, id DESC LIMIT ?1",
            )?;
            let rows = statement.query_map([LISTED_JOBS], |row| {
                let payload: Option<String> = row.get(6)?;
                let kind: String = row.get(1)?;
                let payload = payload.as_deref();
                Ok(JobInfo {
                    id: row.get(0)?,
                    status: row.get(2)?,
                    done: row.get::<_, i64>(3)? as u64,
                    total: row.get::<_, Option<i64>>(4)?.map(|total| total as u64),
                    error: row.get(5)?,
                    scan: report(payload, &kind, JobKind::Scan),
                    import: report(payload, &kind, JobKind::Import),
                    convert: report(payload, &kind, JobKind::Convert),
                    kind,
                    created_at: row.get(7)?,
                    finished_at: row.get(8)?,
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }
}

/// A finished job's report, read when the job is of the kind that writes it.
fn report<T: DeserializeOwned>(payload: Option<&str>, kind: &str, of: JobKind) -> Option<T> {
    payload
        .filter(|_| kind == of.as_str())
        .and_then(|text| serde_json::from_str(text).ok())
}

fn insert_job(conn: &Connection, id: &str, kind: JobKind) -> Result<(), DbError> {
    let now = now_ms();
    conn.execute(
        "INSERT INTO jobs (public_id, kind, status, created_at, started_at)
         VALUES (?1, ?2, 'running', ?3, ?3)",
        params![id, kind.as_str(), now],
    )?;
    Ok(())
}

fn finish_job(
    conn: &Connection,
    id: &str,
    result: &Result<JobOutcome, String>,
) -> Result<(), DbError> {
    let (status, error, payload) = match result {
        Ok(outcome) => (
            if outcome.canceled {
                "canceled"
            } else {
                "completed"
            },
            None,
            if let Some(report) = &outcome.import {
                serde_json::to_string(report).ok()
            } else if let Some(report) = &outcome.convert {
                serde_json::to_string(report).ok()
            } else {
                outcome
                    .scan
                    .as_ref()
                    .and_then(|report| serde_json::to_string(report).ok())
            },
        ),
        Err(message) => ("failed", Some(message.as_str()), None),
    };
    conn.execute(
        "UPDATE jobs SET status = ?2, error = ?3, payload = ?4, finished_at = ?5
         WHERE public_id = ?1",
        params![id, status, error, payload, now_ms()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::time::Duration;

    use tempfile::TempDir;

    use super::*;

    const WAIT: Duration = Duration::from_secs(10);

    struct ChannelSink(Mutex<Sender<CoreEvent>>);

    impl EventSink for ChannelSink {
        fn emit(&self, event: &CoreEvent) {
            let _ = self.0.lock().unwrap().send(event.clone());
        }
    }

    fn runner() -> (TempDir, JobRunner, Receiver<CoreEvent>) {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(dir.path().join("umak.db")).unwrap());
        let (sender, events) = mpsc::channel();
        let runner = JobRunner::new(db, Arc::new(ChannelSink(Mutex::new(sender)))).unwrap();
        (dir, runner, events)
    }

    fn wait_until_finished(events: &Receiver<CoreEvent>) {
        loop {
            if let CoreEvent::JobFinished { .. } = events.recv_timeout(WAIT).unwrap() {
                return;
            }
        }
    }

    fn ok() -> Result<JobOutcome, String> {
        Ok(JobOutcome::default())
    }

    #[test]
    fn a_job_reports_progress_and_is_recorded_when_it_finishes() {
        let (_dir, runner, events) = runner();
        let id = runner
            .start(JobKind::Scan, |context| {
                context.progress(3, 4);
                ok()
            })
            .unwrap();

        let CoreEvent::JobProgress { done, total, .. } = events.recv_timeout(WAIT).unwrap() else {
            panic!("expected progress first");
        };
        assert_eq!((done, total), (3, 4));
        wait_until_finished(&events);

        let job = &runner.list().unwrap()[0];
        assert_eq!(job.id, id);
        assert_eq!(job.status, "completed");
        assert_eq!((job.done, job.total), (3, Some(4)));
        assert!(job.finished_at.is_some());
    }

    #[test]
    fn a_second_job_of_a_kind_is_refused_until_the_first_ends() {
        let (_dir, runner, events) = runner();
        let (release, released) = mpsc::channel::<()>();
        runner
            .start(JobKind::Scan, move |_| {
                released.recv_timeout(WAIT).unwrap();
                ok()
            })
            .unwrap();

        let refused = runner.start(JobKind::Scan, |_| ok()).unwrap_err();
        assert_eq!(refused.code, ErrorCode::Conflict);
        assert_eq!(runner.list().unwrap().len(), 1);

        release.send(()).unwrap();
        wait_until_finished(&events);
        runner.start(JobKind::Scan, |_| ok()).unwrap();
        wait_until_finished(&events);
        assert_eq!(runner.list().unwrap().len(), 2);
    }

    #[test]
    fn a_conversion_runs_beside_a_library_job_but_not_beside_another_conversion() {
        let (_dir, runner, events) = runner();
        let (release, released) = mpsc::channel::<()>();
        runner
            .start(JobKind::Scan, move |_| {
                released.recv_timeout(WAIT).unwrap();
                ok()
            })
            .unwrap();
        let (finish, finished) = mpsc::channel::<()>();
        runner
            .start(JobKind::Convert, move |_| {
                finished.recv_timeout(WAIT).unwrap();
                ok()
            })
            .unwrap();

        let refused = runner.start(JobKind::Convert, |_| ok()).unwrap_err();
        assert_eq!(refused.message, "a conversion job is already running");
        let refused = runner.start(JobKind::TagWrite, |_| ok()).unwrap_err();
        assert_eq!(refused.message, "a scan job is already running");

        release.send(()).unwrap();
        finish.send(()).unwrap();
        wait_until_finished(&events);
        wait_until_finished(&events);
    }

    #[test]
    fn a_conversion_waits_for_the_library_and_holds_it_off_while_it_has_it() {
        let (_dir, runner, events) = runner();
        let (release, released) = mpsc::channel::<()>();
        runner
            .start(JobKind::Scan, move |_| {
                released.recv_timeout(WAIT).unwrap();
                ok()
            })
            .unwrap();
        let (claimed_sender, claimed) = mpsc::channel::<()>();
        let (finish, finished) = mpsc::channel::<()>();
        runner
            .start(JobKind::Convert, move |context| {
                let claim = context.claim_library().unwrap();
                claimed_sender.send(()).unwrap();
                finished.recv_timeout(WAIT).unwrap();
                drop(claim);
                ok()
            })
            .unwrap();

        // Still waiting while the scan runs.
        assert!(claimed.recv_timeout(Duration::from_millis(300)).is_err());
        release.send(()).unwrap();
        wait_until_finished(&events);
        claimed.recv_timeout(WAIT).unwrap();
        let refused = runner.start(JobKind::Scan, |_| ok()).unwrap_err();
        assert_eq!(
            refused.message,
            "a conversion job is adding files to the library"
        );

        finish.send(()).unwrap();
        wait_until_finished(&events);
        runner.start(JobKind::Scan, |_| ok()).unwrap();
        wait_until_finished(&events);
    }

    #[test]
    fn a_conversion_canceled_while_waiting_for_the_library_stops_waiting() {
        let (_dir, runner, events) = runner();
        let (release, released) = mpsc::channel::<()>();
        runner
            .start(JobKind::Scan, move |_| {
                released.recv_timeout(WAIT).unwrap();
                ok()
            })
            .unwrap();
        let id = runner
            .start(JobKind::Convert, |context| {
                assert!(context.claim_library().is_none());
                Ok(JobOutcome {
                    canceled: true,
                    ..JobOutcome::default()
                })
            })
            .unwrap();
        runner.cancel(&id);
        wait_until_finished(&events);
        release.send(()).unwrap();
        wait_until_finished(&events);
        let jobs = runner.list().unwrap();
        let job = jobs.iter().find(|job| job.id == id).unwrap();
        assert_eq!(job.status, "canceled");
    }

    #[test]
    fn canceling_sets_the_flag_and_the_job_records_it() {
        let (_dir, runner, events) = runner();
        let id = runner
            .start(JobKind::Scan, |context| {
                let deadline = std::time::Instant::now() + WAIT;
                while !context.cancel_flag().load(Ordering::Relaxed) {
                    assert!(std::time::Instant::now() < deadline);
                    thread::yield_now();
                }
                Ok(JobOutcome {
                    canceled: true,
                    ..JobOutcome::default()
                })
            })
            .unwrap();
        assert!(runner.cancel(&id));
        wait_until_finished(&events);
        assert_eq!(runner.list().unwrap()[0].status, "canceled");
        assert!(!runner.cancel(&id));
        assert!(!runner.cancel("no-such-job"));
    }

    #[test]
    fn a_failing_or_panicking_job_is_recorded_and_frees_its_kind() {
        let (_dir, runner, events) = runner();
        runner
            .start(JobKind::Scan, |_| Err("disk on fire".to_owned()))
            .unwrap();
        wait_until_finished(&events);
        let job = &runner.list().unwrap()[0];
        assert_eq!(job.status, "failed");
        assert_eq!(job.error.as_deref(), Some("disk on fire"));

        runner.start(JobKind::Scan, |_| panic!("boom")).unwrap();
        wait_until_finished(&events);
        assert_eq!(runner.list().unwrap()[0].status, "failed");
    }

    #[test]
    fn jobs_left_running_by_a_previous_process_are_marked_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("umak.db");
        let db = Arc::new(Database::open(&path).unwrap());
        db.write(|conn| insert_job(conn, "old", JobKind::Scan))
            .unwrap();
        drop(db);

        let db = Arc::new(Database::open(&path).unwrap());
        let (sender, _events) = mpsc::channel();
        let runner = JobRunner::new(db, Arc::new(ChannelSink(Mutex::new(sender)))).unwrap();
        let job = &runner.list().unwrap()[0];
        assert_eq!(job.status, "failed");
        assert_eq!(job.error.as_deref(), Some("interrupted"));
    }
}
