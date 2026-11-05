//! Transport-neutral command layer.
//!
//! Every action is a typed `Command` addressed by name, and state changes are
//! reported as `CoreEvent`s. A transport adapter only forwards JSON to `dispatch`
//! and events from its `EventSink`, so Tauri IPC and a later HTTP server share one
//! implementation. To add a command, implement `Command` and list it in `commands!`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::db::{Database, DbError, new_public_id};
use crate::jobs::{JobInfo, JobKind, JobOutcome, JobRunner};
use crate::library::{self, TrackPage, TrackSort};
use crate::playlists::{self, Playlist, PlaylistPage};
use crate::scan::{self, ScanError, ScanOptions};
use crate::{AppInfo, app_info};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    UnknownCommand,
    InvalidArgs,
    NotFound,
    Conflict,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS, thiserror::Error)]
#[error("{message}")]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
}

impl ApiError {
    pub(crate) fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl From<DbError> for ApiError {
    fn from(error: DbError) -> Self {
        Self::new(ErrorCode::Internal, error.to_string())
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(error: rusqlite::Error) -> Self {
        Self::new(ErrorCode::Internal, error.to_string())
    }
}

impl From<ScanError> for ApiError {
    fn from(error: ScanError) -> Self {
        let code = match error {
            ScanError::UnknownRoot => ErrorCode::InvalidArgs,
            _ => ErrorCode::Internal,
        };
        Self::new(code, error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum CoreEvent {
    LibraryRootsChanged,
    /// Tracks, albums or artists changed, so views showing them should reload.
    LibraryChanged,
    /// A playlist or its contents changed.
    PlaylistsChanged,
    JobProgress {
        id: String,
        kind: JobKind,
        done: u64,
        total: u64,
    },
    /// The job's final state and result are available from `jobs.list`.
    JobFinished {
        id: String,
        kind: JobKind,
    },
}

/// Delivers events to whichever transport is attached.
pub trait EventSink: Send + Sync {
    fn emit(&self, event: &CoreEvent);
}

/// The state every command runs against.
pub struct Core {
    db: Arc<Database>,
    events: Arc<dyn EventSink>,
    jobs: JobRunner,
}

impl Core {
    pub fn new(db: Database, events: Box<dyn EventSink>) -> Result<Self, DbError> {
        let db = Arc::new(db);
        let events: Arc<dyn EventSink> = Arc::from(events);
        let jobs = JobRunner::new(Arc::clone(&db), Arc::clone(&events))?;
        Ok(Self { db, events, jobs })
    }
}

pub trait Command: DeserializeOwned + TS + 'static {
    const NAME: &'static str;
    /// A named struct, so results can gain fields and be exported to TypeScript.
    type Output: Serialize + TS + 'static;

    fn run(self, core: &Core) -> Result<Self::Output, ApiError>;
}

fn call<C: Command>(core: &Core, args: Value) -> Result<Value, ApiError> {
    let command: C = serde_json::from_value(args)
        .map_err(|error| ApiError::new(ErrorCode::InvalidArgs, format!("{}: {error}", C::NAME)))?;
    let output = command.run(core)?;
    serde_json::to_value(output)
        .map_err(|error| ApiError::new(ErrorCode::Internal, error.to_string()))
}

macro_rules! commands {
    ($($command:ty),* $(,)?) => {
        /// Runs a command by name with JSON arguments and returns its JSON result.
        pub fn dispatch(core: &Core, name: &str, args: Value) -> Result<Value, ApiError> {
            match name {
                $(<$command as Command>::NAME => call::<$command>(core, args),)*
                _ => Err(ApiError::new(
                    ErrorCode::UnknownCommand,
                    format!("unknown command: {name}"),
                )),
            }
        }

        #[cfg(test)]
        fn command_bindings(cfg: &ts_rs::Config) -> Vec<bindings::Signature> {
            vec![$(bindings::signature::<$command>(cfg),)*]
        }
    };
}

commands![
    GetAppInfo,
    ListRoots,
    AddRoot,
    StartScan,
    ListJobs,
    CancelJob,
    GetTrackPage,
    ListPlaylists,
    CreatePlaylist,
    RenamePlaylist,
    DeletePlaylist,
    GetPlaylistItems,
    AddPlaylistTracks,
    RemovePlaylistItems,
    MovePlaylistItems,
];

#[derive(Debug, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct GetAppInfo {}

impl Command for GetAppInfo {
    const NAME: &'static str = "app.info";
    type Output = AppInfo;

    fn run(self, _core: &Core) -> Result<AppInfo, ApiError> {
        Ok(app_info())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct LibraryRoot {
    pub id: String,
    pub name: String,
    pub path: String,
    pub added_at: i64,
    pub track_count: u32,
    pub missing_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct RootList {
    pub roots: Vec<LibraryRoot>,
}

#[derive(Debug, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ListRoots {}

impl Command for ListRoots {
    const NAME: &'static str = "library.roots.list";
    type Output = RootList;

    fn run(self, core: &Core) -> Result<RootList, ApiError> {
        core.db.read(|conn| {
            Ok(RootList {
                roots: select_roots(conn)?,
            })
        })
    }
}

#[derive(Debug, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct AddRoot {
    pub path: String,
    /// Defaults to the name of the folder.
    pub name: Option<String>,
}

impl Command for AddRoot {
    const NAME: &'static str = "library.roots.add";
    type Output = LibraryRoot;

    fn run(self, core: &Core) -> Result<LibraryRoot, ApiError> {
        let root = add_root(&core.db, &self.path, self.name)?;
        core.events.emit(&CoreEvent::LibraryRootsChanged);
        Ok(root)
    }
}

/// Registers a library folder. It must exist and must not overlap another root.
pub fn add_root(db: &Database, path: &str, name: Option<String>) -> Result<LibraryRoot, ApiError> {
    let path = resolve_root_path(path)?;
    let path_text = path
        .to_str()
        .ok_or_else(|| {
            ApiError::new(
                ErrorCode::InvalidArgs,
                "the folder path is not valid Unicode",
            )
        })?
        .to_owned();
    let name = name
        .or_else(|| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| path_text.clone());
    let root = LibraryRoot {
        id: new_public_id(),
        name,
        path: path_text,
        added_at: now_ms(),
        track_count: 0,
        missing_count: 0,
    };
    db.write(|conn| {
        let tx = conn.transaction()?;
        ensure_no_overlap(&tx, &path)?;
        tx.execute(
            "INSERT INTO library_roots (public_id, name, path, added_at) VALUES (?1, ?2, ?3, ?4)",
            params![root.id, root.name, root.path, root.added_at],
        )?;
        tx.commit()?;
        Ok::<_, ApiError>(())
    })?;
    Ok(root)
}

fn resolve_root_path(path: &str) -> Result<PathBuf, ApiError> {
    let invalid = |message: String| ApiError::new(ErrorCode::InvalidArgs, message);
    if !Path::new(path).is_absolute() {
        return Err(invalid(format!("not an absolute path: {path}")));
    }
    let resolved =
        dunce::canonicalize(path).map_err(|error| invalid(format!("{path}: {error}")))?;
    if !resolved.is_dir() {
        return Err(invalid(format!("not a folder: {path}")));
    }
    Ok(resolved)
}

/// Rejects a root inside or around an existing one, which would index files twice.
fn ensure_no_overlap(conn: &Connection, path: &Path) -> Result<(), ApiError> {
    for root in select_roots(conn)? {
        let existing = Path::new(&root.path);
        if path.starts_with(existing) || existing.starts_with(path) {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                format!("overlaps the library folder {}", root.path),
            ));
        }
    }
    Ok(())
}

fn select_roots(conn: &Connection) -> Result<Vec<LibraryRoot>, rusqlite::Error> {
    conn.prepare_cached(
        "SELECT r.public_id, r.name, r.path, r.added_at,
                (SELECT count(*) FROM tracks t WHERE t.root_id = r.id),
                (SELECT count(*) FROM tracks t
                 WHERE t.root_id = r.id AND t.missing_since IS NOT NULL)
         FROM library_roots r ORDER BY r.name, r.id",
    )?
    .query_map([], |row| {
        Ok(LibraryRoot {
            id: row.get(0)?,
            name: row.get(1)?,
            path: row.get(2)?,
            added_at: row.get(3)?,
            track_count: row.get(4)?,
            missing_count: row.get(5)?,
        })
    })?
    .collect()
}

/// A page of the library, in the given order, optionally limited to search matches.
#[derive(Debug, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct GetTrackPage {
    pub query: Option<String>,
    pub sort: TrackSort,
    pub descending: bool,
    pub offset: u32,
    pub limit: u32,
}

impl Command for GetTrackPage {
    const NAME: &'static str = "library.tracks.page";
    type Output = TrackPage;

    fn run(self, core: &Core) -> Result<TrackPage, ApiError> {
        core.db.read(|conn| {
            Ok(library::track_page(
                conn,
                self.query.as_deref(),
                self.sort,
                self.descending,
                self.offset,
                self.limit,
            )?)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct PlaylistList {
    pub playlists: Vec<Playlist>,
}

#[derive(Debug, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ListPlaylists {}

impl Command for ListPlaylists {
    const NAME: &'static str = "playlists.list";
    type Output = PlaylistList;

    fn run(self, core: &Core) -> Result<PlaylistList, ApiError> {
        core.db.read(|conn| {
            Ok(PlaylistList {
                playlists: playlists::list(conn)?,
            })
        })
    }
}

impl Core {
    /// Runs a playlist change in one transaction and announces it.
    fn change_playlists<T>(
        &self,
        work: impl FnOnce(&Connection, i64) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        let result = self.db.write(|conn| {
            let tx = conn.transaction()?;
            let result = work(&tx, now_ms())?;
            tx.commit()?;
            Ok::<_, ApiError>(result)
        })?;
        self.events.emit(&CoreEvent::PlaylistsChanged);
        Ok(result)
    }
}

#[derive(Debug, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CreatePlaylist {
    pub name: String,
}

impl Command for CreatePlaylist {
    const NAME: &'static str = "playlists.create";
    type Output = Playlist;

    fn run(self, core: &Core) -> Result<Playlist, ApiError> {
        core.change_playlists(|conn, now| playlists::create(conn, &self.name, now))
    }
}

#[derive(Debug, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RenamePlaylist {
    pub id: String,
    pub name: String,
}

impl Command for RenamePlaylist {
    const NAME: &'static str = "playlists.rename";
    type Output = Playlist;

    fn run(self, core: &Core) -> Result<Playlist, ApiError> {
        core.change_playlists(|conn, now| playlists::rename(conn, &self.id, &self.name, now))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct Deleted {
    pub id: String,
}

/// Deletes the playlist. Its tracks stay in the library.
#[derive(Debug, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DeletePlaylist {
    pub id: String,
}

impl Command for DeletePlaylist {
    const NAME: &'static str = "playlists.delete";
    type Output = Deleted;

    fn run(self, core: &Core) -> Result<Deleted, ApiError> {
        core.change_playlists(|conn, _| playlists::delete(conn, &self.id))?;
        Ok(Deleted { id: self.id })
    }
}

#[derive(Debug, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct GetPlaylistItems {
    pub id: String,
    pub offset: u32,
    pub limit: u32,
}

impl Command for GetPlaylistItems {
    const NAME: &'static str = "playlists.items";
    type Output = PlaylistPage;

    fn run(self, core: &Core) -> Result<PlaylistPage, ApiError> {
        core.db
            .read(|conn| playlists::items(conn, &self.id, self.offset, self.limit))
    }
}

/// Appends tracks to the end of a playlist.
#[derive(Debug, Deserialize, TS)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AddPlaylistTracks {
    pub id: String,
    pub track_ids: Vec<String>,
}

impl Command for AddPlaylistTracks {
    const NAME: &'static str = "playlists.addTracks";
    type Output = Playlist;

    fn run(self, core: &Core) -> Result<Playlist, ApiError> {
        core.change_playlists(|conn, now| {
            playlists::add_tracks(conn, &self.id, &self.track_ids, now)
        })
    }
}

#[derive(Debug, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RemovePlaylistItems {
    pub id: String,
    pub positions: Vec<i64>,
}

impl Command for RemovePlaylistItems {
    const NAME: &'static str = "playlists.removeItems";
    type Output = Playlist;

    fn run(self, core: &Core) -> Result<Playlist, ApiError> {
        core.change_playlists(|conn, now| {
            playlists::remove_items(conn, &self.id, &self.positions, now)
        })
    }
}

/// Moves entries together before another entry, or to the end when `before` is null.
#[derive(Debug, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MovePlaylistItems {
    pub id: String,
    pub positions: Vec<i64>,
    pub before: Option<i64>,
}

impl Command for MovePlaylistItems {
    const NAME: &'static str = "playlists.moveItems";
    type Output = Playlist;

    fn run(self, core: &Core) -> Result<Playlist, ApiError> {
        core.change_playlists(|conn, now| {
            playlists::move_items(conn, &self.id, &self.positions, self.before, now)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct JobRef {
    pub id: String,
}

/// Starts scanning in the background and returns at once. Progress arrives as events.
#[derive(Debug, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct StartScan {
    /// The roots to scan, or every root when null.
    pub roots: Option<Vec<String>>,
}

impl Command for StartScan {
    const NAME: &'static str = "library.scan.start";
    type Output = JobRef;

    fn run(self, core: &Core) -> Result<JobRef, ApiError> {
        let db = Arc::clone(&core.db);
        let events = Arc::clone(&core.events);
        let roots = self.roots;
        let id = core.jobs.start(JobKind::Scan, move |context| {
            let mut report_progress = |progress: scan::ScanProgress| {
                context.progress(progress.done, progress.total);
                // Each progress update follows a committed batch, so views can show
                // the new tracks while a long first import runs.
                if progress.done > 0 {
                    events.emit(&CoreEvent::LibraryChanged);
                }
            };
            let report = scan::scan_library(
                &db,
                roots.as_deref(),
                ScanOptions::default(),
                context.cancel_flag(),
                &mut report_progress,
            )
            .map_err(|error| error.to_string())?;
            if report.changed() {
                events.emit(&CoreEvent::LibraryChanged);
            }
            Ok(JobOutcome {
                canceled: report.canceled,
                scan: Some(report),
            })
        })?;
        Ok(JobRef { id })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct JobList {
    pub jobs: Vec<JobInfo>,
}

/// The most recent jobs, newest first.
#[derive(Debug, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ListJobs {}

impl Command for ListJobs {
    const NAME: &'static str = "jobs.list";
    type Output = JobList;

    fn run(self, core: &Core) -> Result<JobList, ApiError> {
        Ok(JobList {
            jobs: core.jobs.list()?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CancelResult {
    /// False when the job was not running, for example because it had just finished.
    pub requested: bool,
}

#[derive(Debug, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CancelJob {
    pub id: String,
}

impl Command for CancelJob {
    const NAME: &'static str = "jobs.cancel";
    type Output = CancelResult;

    fn run(self, core: &Core) -> Result<CancelResult, ApiError> {
        Ok(CancelResult {
            requested: core.jobs.cancel(&self.id),
        })
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

#[cfg(test)]
mod bindings {
    use super::*;

    pub struct Signature {
        pub name: &'static str,
        pub args: String,
        pub output: String,
    }

    pub fn signature<C: Command>(cfg: &ts_rs::Config) -> Signature {
        C::export_all(cfg).unwrap();
        <C::Output as TS>::export_all(cfg).unwrap();
        Signature {
            name: C::NAME,
            args: C::name(cfg),
            output: <C::Output as TS>::name(cfg),
        }
    }

    /// Renders the map from command name to argument and result types.
    pub fn command_map(signatures: &[Signature]) -> String {
        let mut types: Vec<&str> = signatures
            .iter()
            .flat_map(|signature| [signature.args.as_str(), signature.output.as_str()])
            .collect();
        types.sort_unstable();
        types.dedup();
        let mut text =
            String::from("// Generated from the commands! list in umak-core. Do not edit.\n");
        for name in types {
            text.push_str(&format!("import type {{ {name} }} from \"./{name}\";\n"));
        }
        text.push_str("\nexport type CommandMap = {\n");
        for signature in signatures {
            text.push_str(&format!(
                "  \"{}\": {{ args: {}; output: {} }};\n",
                signature.name, signature.args, signature.output
            ));
        }
        text.push_str("};\n");
        text
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::sync::{Arc, Mutex};

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    #[derive(Clone, Default)]
    struct RecordingSink(Arc<Mutex<Vec<CoreEvent>>>);

    impl EventSink for RecordingSink {
        fn emit(&self, event: &CoreEvent) {
            self.0.lock().unwrap().push(event.clone());
        }
    }

    struct Fixture {
        dir: TempDir,
        core: Core,
        events: RecordingSink,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("umak.db")).unwrap();
        let events = RecordingSink::default();
        let core = Core::new(db, Box::new(events.clone())).unwrap();
        Fixture { dir, core, events }
    }

    impl Fixture {
        fn folder(&self, name: &str) -> String {
            let path = self.dir.path().join(name);
            fs::create_dir_all(&path).unwrap();
            path.to_str().unwrap().to_owned()
        }

        fn add_root(&self, path: &str) -> Result<Value, ApiError> {
            dispatch(
                &self.core,
                "library.roots.add",
                json!({ "path": path, "name": null }),
            )
        }

        fn root_count(&self) -> usize {
            let list = dispatch(&self.core, "library.roots.list", json!({})).unwrap();
            list["roots"].as_array().unwrap().len()
        }
    }

    fn error_code(result: Result<Value, ApiError>) -> ErrorCode {
        result.unwrap_err().code
    }

    #[test]
    fn core_can_be_shared_across_threads() {
        fn assert_shareable<T: Send + Sync>() {}
        assert_shareable::<Core>();
    }

    #[test]
    fn dispatch_runs_a_command_by_name_and_returns_json() {
        let fixture = fixture();
        let info = dispatch(&fixture.core, "app.info", json!({})).unwrap();
        assert_eq!(info["name"], "Umak");
        assert_eq!(info["version"], env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn an_unknown_command_is_reported() {
        let fixture = fixture();
        assert_eq!(
            error_code(dispatch(&fixture.core, "no.such.command", json!({}))),
            ErrorCode::UnknownCommand
        );
    }

    #[test]
    fn malformed_or_unexpected_arguments_are_rejected() {
        let fixture = fixture();
        for (name, args) in [
            ("library.roots.add", json!({})),
            ("library.roots.add", json!({ "path": 5, "name": null })),
            ("app.info", json!({ "extra": true })),
            ("app.info", json!("text")),
        ] {
            assert_eq!(
                error_code(dispatch(&fixture.core, name, args)),
                ErrorCode::InvalidArgs
            );
        }
    }

    #[test]
    fn errors_serialize_with_a_code_and_message() {
        let error = ApiError::new(ErrorCode::UnknownCommand, "nope");
        assert_eq!(
            serde_json::to_value(error).unwrap(),
            json!({ "code": "unknownCommand", "message": "nope" })
        );
    }

    #[test]
    fn an_added_root_is_listed_with_an_opaque_id_and_an_event() {
        let fixture = fixture();
        let path = fixture.folder("Albums");
        let added = fixture.add_root(&path).unwrap();

        assert_eq!(added["name"], "Albums");
        assert!(uuid::Uuid::parse_str(added["id"].as_str().unwrap()).is_ok());
        assert!(added["addedAt"].as_i64().unwrap() > 0);

        let list = dispatch(&fixture.core, "library.roots.list", json!({})).unwrap();
        assert_eq!(list["roots"], json!([added]));
        assert_eq!(
            *fixture.events.0.lock().unwrap(),
            [CoreEvent::LibraryRootsChanged]
        );
    }

    #[test]
    fn an_explicit_root_name_is_used() {
        let fixture = fixture();
        let path = fixture.folder("Albums");
        let added = dispatch(
            &fixture.core,
            "library.roots.add",
            json!({ "path": path, "name": "My albums" }),
        )
        .unwrap();
        assert_eq!(added["name"], "My albums");
    }

    #[test]
    fn root_paths_must_be_existing_absolute_folders() {
        let fixture = fixture();
        let folder = fixture.folder("Albums");
        let file = Path::new(&folder).join("track.flac");
        fs::write(&file, b"").unwrap();
        let missing = Path::new(&folder).join("missing");

        for path in [
            "relative/path",
            missing.to_str().unwrap(),
            file.to_str().unwrap(),
        ] {
            assert_eq!(error_code(fixture.add_root(path)), ErrorCode::InvalidArgs);
        }
        assert_eq!(fixture.root_count(), 0);
        assert!(fixture.events.0.lock().unwrap().is_empty());
    }

    #[test]
    fn roots_may_not_repeat_or_overlap() {
        let fixture = fixture();
        let music = fixture.folder("Music");
        let inner = fixture.folder("Music/Albums");
        let outer = fixture.dir.path().to_str().unwrap().to_owned();
        let sibling = fixture.folder("Musical");

        fixture.add_root(&music).unwrap();
        for path in [&music, &inner, &outer] {
            assert_eq!(error_code(fixture.add_root(path)), ErrorCode::Conflict);
        }
        // A shared name prefix is not an overlap.
        fixture.add_root(&sibling).unwrap();
        assert_eq!(fixture.root_count(), 2);
    }

    #[test]
    fn a_scan_started_by_command_runs_in_the_background_and_reports_its_result() {
        let fixture = fixture();
        let path = fixture.folder("Albums");
        fs::write(Path::new(&path).join("not-audio.mp3"), b"text").unwrap();
        fixture.add_root(&path).unwrap();

        let job = dispatch(
            &fixture.core,
            "library.scan.start",
            json!({ "roots": null }),
        )
        .unwrap();
        let id = job["id"].as_str().unwrap().to_owned();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let finished = CoreEvent::JobFinished {
            id: id.clone(),
            kind: JobKind::Scan,
        };
        while !fixture.events.0.lock().unwrap().contains(&finished) {
            assert!(std::time::Instant::now() < deadline, "the scan never ended");
            std::thread::yield_now();
        }

        let list = dispatch(&fixture.core, "jobs.list", json!({})).unwrap();
        let listed = &list["jobs"][0];
        assert_eq!(listed["id"], id.as_str());
        assert_eq!(listed["status"], "completed");
        assert_eq!(listed["scan"]["added"], 1);
        assert_eq!(listed["scan"]["tagErrors"], 1);
        let events = fixture.events.0.lock().unwrap();
        assert!(events.contains(&CoreEvent::LibraryChanged));
        assert!(events.iter().any(|event| matches!(
            event,
            CoreEvent::JobProgress {
                done: 1,
                total: 1,
                ..
            }
        )));
    }

    #[test]
    fn scanning_an_unknown_root_fails_the_job() {
        let fixture = fixture();
        let job = dispatch(
            &fixture.core,
            "library.scan.start",
            json!({ "roots": ["no-such-root"] }),
        )
        .unwrap();
        let finished = CoreEvent::JobFinished {
            id: job["id"].as_str().unwrap().to_owned(),
            kind: JobKind::Scan,
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !fixture.events.0.lock().unwrap().contains(&finished) {
            assert!(std::time::Instant::now() < deadline, "the scan never ended");
            std::thread::yield_now();
        }
        let list = dispatch(&fixture.core, "jobs.list", json!({})).unwrap();
        assert_eq!(list["jobs"][0]["status"], "failed");
    }

    #[test]
    fn a_playlist_change_commits_all_or_nothing_and_announces_itself() {
        let fixture = fixture();
        let made = dispatch(&fixture.core, "playlists.create", json!({ "name": "Mix" })).unwrap();
        assert_eq!(made["name"], "Mix");
        assert_eq!(made["trackCount"], 0);
        assert_eq!(
            *fixture.events.0.lock().unwrap(),
            [CoreEvent::PlaylistsChanged]
        );

        let id = made["id"].as_str().unwrap();
        let error = dispatch(
            &fixture.core,
            "playlists.addTracks",
            json!({ "id": id, "trackIds": ["no-such-track"] }),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound);
        assert_eq!(fixture.events.0.lock().unwrap().len(), 1);

        let deleted = dispatch(&fixture.core, "playlists.delete", json!({ "id": id })).unwrap();
        assert_eq!(deleted["id"], id);
        let list = dispatch(&fixture.core, "playlists.list", json!({})).unwrap();
        assert_eq!(list["playlists"], json!([]));
    }

    #[test]
    fn track_pages_reject_an_unknown_sort() {
        let fixture = fixture();
        let args = |sort: &str| json!({ "query": null, "sort": sort, "descending": false, "offset": 0, "limit": 10 });
        let page = dispatch(&fixture.core, "library.tracks.page", args("title")).unwrap();
        assert_eq!(page["total"], 0);
        let error = dispatch(&fixture.core, "library.tracks.page", args("mood")).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgs);
    }

    #[test]
    fn listed_roots_report_their_track_counts() {
        let fixture = fixture();
        let path = fixture.folder("Albums");
        fs::write(Path::new(&path).join("a.mp3"), b"text").unwrap();
        fixture.add_root(&path).unwrap();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        scan::scan_library(
            &fixture.core.db,
            None,
            ScanOptions::default(),
            &cancel,
            &mut |_| {},
        )
        .unwrap();
        let list = dispatch(&fixture.core, "library.roots.list", json!({})).unwrap();
        assert_eq!(list["roots"][0]["trackCount"], 1);
        assert_eq!(list["roots"][0]["missingCount"], 0);
    }

    #[test]
    fn canceling_an_unknown_job_reports_that_nothing_was_requested() {
        let fixture = fixture();
        let result = dispatch(&fixture.core, "jobs.cancel", json!({ "id": "none" })).unwrap();
        assert_eq!(result["requested"], false);
    }

    #[test]
    fn command_names_are_unique() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = ts_rs::Config::new().with_out_dir(temp.path());
        let signatures = command_bindings(&cfg);
        let mut names: Vec<_> = signatures.iter().map(|signature| signature.name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count);
    }

    fn read_folder(dir: &Path) -> BTreeMap<String, String> {
        fs::read_dir(dir)
            .into_iter()
            .flatten()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    fs::read_to_string(entry.path())
                        .unwrap()
                        .replace("\r\n", "\n"),
                )
            })
            .collect()
    }

    /// Fails when the committed TypeScript types no longer match the Rust types.
    /// Run with `UMAK_UPDATE_BINDINGS=1` to regenerate them.
    #[test]
    fn typescript_bindings_are_current() {
        let committed =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/src/lib/api/generated");
        let temp = tempfile::tempdir().unwrap();
        let cfg = ts_rs::Config::new()
            .with_out_dir(temp.path())
            .with_large_int("number");

        let signatures = command_bindings(&cfg);
        CoreEvent::export_all(&cfg).unwrap();
        ApiError::export_all(&cfg).unwrap();
        fs::write(
            temp.path().join("commands.ts"),
            bindings::command_map(&signatures),
        )
        .unwrap();
        let generated = read_folder(temp.path());

        if std::env::var_os("UMAK_UPDATE_BINDINGS").is_some() {
            let _ = fs::remove_dir_all(&committed);
            fs::create_dir_all(&committed).unwrap();
            for (name, text) in &generated {
                fs::write(committed.join(name), text).unwrap();
            }
        }
        assert_eq!(
            read_folder(&committed),
            generated,
            "run the tests with UMAK_UPDATE_BINDINGS=1 to regenerate the TypeScript types"
        );
    }
}
