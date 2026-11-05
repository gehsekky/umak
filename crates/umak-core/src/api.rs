//! Transport-neutral command layer.
//!
//! Every action is a typed `Command` addressed by name, and state changes are
//! reported as `CoreEvent`s. A transport adapter only forwards JSON to `dispatch`
//! and events from its `EventSink`, so Tauri IPC and a later HTTP server share one
//! implementation. To add a command, implement `Command` and list it in `commands!`.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::db::{Database, DbError, new_public_id};
use crate::{AppInfo, app_info};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    UnknownCommand,
    InvalidArgs,
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
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum CoreEvent {
    LibraryRootsChanged,
}

/// Delivers events to whichever transport is attached.
pub trait EventSink: Send + Sync {
    fn emit(&self, event: &CoreEvent);
}

/// The state every command runs against.
pub struct Core {
    db: Database,
    events: Box<dyn EventSink>,
}

impl Core {
    pub fn new(db: Database, events: Box<dyn EventSink>) -> Self {
        Self { db, events }
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

commands![GetAppInfo, ListRoots, AddRoot];

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
        let path = resolve_root_path(&self.path)?;
        let path_text = path
            .to_str()
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::InvalidArgs,
                    "the folder path is not valid Unicode",
                )
            })?
            .to_owned();
        let name = self
            .name
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
        };
        core.db.write(|conn| {
            let tx = conn.transaction()?;
            ensure_no_overlap(&tx, &path)?;
            tx.execute(
                "INSERT INTO library_roots (public_id, name, path, added_at) VALUES (?1, ?2, ?3, ?4)",
                params![root.id, root.name, root.path, root.added_at],
            )?;
            tx.commit()?;
            Ok::<_, ApiError>(())
        })?;
        core.events.emit(&CoreEvent::LibraryRootsChanged);
        Ok(root)
    }
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
        "SELECT public_id, name, path, added_at FROM library_roots ORDER BY name, id",
    )?
    .query_map([], |row| {
        Ok(LibraryRoot {
            id: row.get(0)?,
            name: row.get(1)?,
            path: row.get(2)?,
            added_at: row.get(3)?,
        })
    })?
    .collect()
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
        let core = Core::new(db, Box::new(events.clone()));
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
