//! Batches of tag changes. A batch is worked out from the files first, so every change can be
//! seen before anything is written. Writing goes file by file and can stop at any point, the
//! library follows each write, and what each write replaced is kept so it can be undone.
//!
//! An undo is a batch like any other: it has its own preview, and can itself be undone.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::api::{ApiError, ErrorCode};
use crate::db::{Database, DbError};
use crate::library::MAX_PAGE;
use crate::scan::store::{self, Catalog, FileRecord};
use crate::scan::tags::{TrackTags, read_file};
use crate::scan::walk;
use crate::tagwrite::{
    FieldEdit, TagField, TagSnapshot, TagWriteError, clean_edits, field_values, read_tags,
    remove_leftover, restore_tags, snapshot_tags, write_previewed,
};

const BATCH_ROWS: usize = 500;
const BATCH_INTERVAL: Duration = Duration::from_secs(1);

const EDIT_PREVIEWING: &str = "previewing";
const EDIT_READY: &str = "ready";
const EDIT_WRITING: &str = "writing";
const EDIT_DONE: &str = "done";
const EDIT_CANCELED: &str = "canceled";
const EDIT_INTERRUPTED: &str = "interrupted";

const ITEM_PENDING: &str = "pending";
const ITEM_BLOCKED: &str = "blocked";
const ITEM_WRITING: &str = "writing";
const ITEM_WRITTEN: &str = "written";
const ITEM_FAILED: &str = "failed";
const ITEM_SKIPPED: &str = "skipped";
const ITEM_UNSURE: &str = "unsure";

/// The same edits for a set of tracks. A batch can hold several of these, so each track can
/// get its own values, such as its own track number.
#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrackEdits {
    pub track_ids: Vec<String>,
    pub edits: Vec<FieldEdit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct FieldChange {
    pub field: TagField,
    pub before: Vec<String>,
    pub after: Vec<String>,
}

/// A batch. `status` is one of previewing, ready, writing, done, canceled or interrupted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TagEdit {
    pub id: String,
    /// The batch this one undoes.
    pub undoes: Option<String>,
    /// The latest batch that undid this one.
    pub undone_by: Option<String>,
    pub status: String,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    /// Tracks asked about whose tags already held the new values.
    pub unchanged: u32,
    /// Files that will be, or were to be, changed.
    pub changing: u32,
    /// Files that cannot be changed, with the reason on each item.
    pub blocked: u32,
    pub written: u32,
    pub failed: u32,
    /// Files not reached, because the batch was canceled or interrupted.
    pub skipped: u32,
    /// Files being written when the app stopped, which may hold either version.
    pub unsure: u32,
}

/// One file in a batch. `status` is one of pending, blocked, writing, written, failed, skipped
/// or unsure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TagEditItem {
    pub position: u32,
    pub track_id: String,
    pub rel_path: String,
    pub title: String,
    pub artist: String,
    pub status: String,
    pub message: Option<String>,
    pub changes: Vec<FieldChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct TagEditPage {
    pub total: u32,
    pub edits: Vec<TagEdit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct TagEditItemPage {
    pub total: u32,
    pub items: Vec<TagEditItem>,
}

/// What a track's file holds in every editable field, or why it could not be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TrackTagValues {
    pub track_id: String,
    /// Null when the track is no longer in the library.
    pub rel_path: Option<String>,
    pub fields: Vec<FieldEdit>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct TrackTagValuesList {
    pub tracks: Vec<TrackTagValues>,
}

const EDITABLE_FIELDS: [TagField; 9] = [
    TagField::Title,
    TagField::Album,
    TagField::Artists,
    TagField::AlbumArtists,
    TagField::Composers,
    TagField::Genres,
    TagField::Year,
    TagField::TrackNumber,
    TagField::DiscNumber,
];

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

fn not_found() -> ApiError {
    ApiError::new(ErrorCode::NotFound, "no such tag edit")
}

fn json_error(error: serde_json::Error) -> ApiError {
    ApiError::new(ErrorCode::Internal, error.to_string())
}

/// What a batch does to one track, before its file has been looked at.
pub enum Target {
    Edit {
        track: i64,
        path: PathBuf,
        edits: Vec<FieldEdit>,
    },
    Undo {
        track: i64,
        path: PathBuf,
        changes: Vec<FieldChange>,
        restore: String,
    },
}

fn track_path(root: &str, rel_path: &str) -> PathBuf {
    Path::new(root).join(rel_path)
}

/// Reads the files themselves, because the library fills gaps for display: a track with no
/// title tag shows its file name, and an album can be given an inferred artist. Those are not
/// values to offer for editing.
pub fn read_values(db: &Database, track_ids: &[String]) -> Result<TrackTagValuesList, ApiError> {
    if track_ids.len() > MAX_PAGE as usize {
        return Err(ApiError::new(
            ErrorCode::InvalidArgs,
            format!("at most {MAX_PAGE} tracks can be read at once"),
        ));
    }
    let located: Vec<(String, Option<(String, String)>)> = db.read(|conn| {
        let mut lookup = conn.prepare_cached(
            "SELECT r.path, t.rel_path FROM tracks t
             JOIN library_roots r ON r.id = t.root_id WHERE t.public_id = ?1",
        )?;
        track_ids
            .iter()
            .map(|id| {
                let found = lookup
                    .query_row([id], |row| Ok((row.get(0)?, row.get(1)?)))
                    .optional()?;
                Ok((id.clone(), found))
            })
            .collect::<Result<_, ApiError>>()
    })?;
    let tracks = located
        .into_iter()
        .map(|(track_id, found)| {
            let Some((root, rel_path)) = found else {
                return TrackTagValues {
                    track_id,
                    rel_path: None,
                    fields: Vec::new(),
                    error: Some("this track is no longer in the library".to_owned()),
                };
            };
            let (fields, error) = match read_tags(&track_path(&root, &rel_path)) {
                Ok(tags) => (
                    EDITABLE_FIELDS
                        .iter()
                        .map(|&field| FieldEdit {
                            field,
                            values: field_values(&tags, field),
                        })
                        .collect(),
                    None,
                ),
                Err(error) => (Vec::new(), Some(describe(&error))),
            };
            TrackTagValues {
                track_id,
                rel_path: Some(rel_path),
                fields,
                error,
            }
        })
        .collect();
    Ok(TrackTagValuesList { tracks })
}

/// Checks a request and finds the tracks it names.
pub fn edit_targets(conn: &Connection, requests: &[TrackEdits]) -> Result<Vec<Target>, ApiError> {
    let invalid = |message: String| ApiError::new(ErrorCode::InvalidArgs, message);
    let mut lookup = conn.prepare_cached(
        "SELECT t.id, r.path, t.rel_path FROM tracks t
         JOIN library_roots r ON r.id = t.root_id WHERE t.public_id = ?1",
    )?;
    let mut seen = std::collections::HashSet::new();
    let mut targets = Vec::new();
    for request in requests {
        let edits = clean_edits(&request.edits).map_err(|error| invalid(error.to_string()))?;
        if edits.is_empty() {
            return Err(invalid("no fields to change".to_owned()));
        }
        for id in &request.track_ids {
            if !seen.insert(id) {
                return Err(invalid(format!("track {id} was given twice")));
            }
            let (track, root, rel_path): (i64, String, String) = lookup
                .query_row([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .optional()?
                .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("no track {id}")))?;
            targets.push(Target::Edit {
                track,
                path: track_path(&root, &rel_path),
                edits: edits.clone(),
            });
        }
    }
    if targets.is_empty() {
        return Err(invalid("no tracks to change".to_owned()));
    }
    Ok(targets)
}

/// What undoing a batch has to do. A file not written by the batch has nothing to undo.
pub fn undo_targets(conn: &Connection, edit_id: &str) -> Result<Vec<Target>, ApiError> {
    let (edit, status): (i64, String) = conn
        .prepare_cached("SELECT id, status FROM tag_edits WHERE public_id = ?1")?
        .query_row([edit_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?
        .ok_or_else(not_found)?;
    if ![EDIT_DONE, EDIT_CANCELED, EDIT_INTERRUPTED].contains(&status.as_str()) {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            "this edit has not been written",
        ));
    }
    let mut statement = conn.prepare_cached(
        "SELECT i.track_id, r.path, t.rel_path, i.changes, i.snapshot
         FROM tag_edit_items i
         JOIN tracks t ON t.id = i.track_id
         JOIN library_roots r ON r.id = t.root_id
         WHERE i.edit_id = ?1 AND i.status IN (?2, ?3) ORDER BY i.position",
    )?;
    let rows = statement.query_map(params![edit, ITEM_WRITTEN, ITEM_UNSURE], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;
    let mut targets = Vec::new();
    for row in rows {
        let (track, root, rel_path, changes, snapshot) = row?;
        let Some(restore) = snapshot else { continue };
        targets.push(Target::Undo {
            track,
            path: track_path(&root, &rel_path),
            changes: serde_json::from_str(&changes).map_err(json_error)?,
            restore,
        });
    }
    if targets.is_empty() {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            "this edit changed no files, so there is nothing to undo",
        ));
    }
    Ok(targets)
}

/// A row to store for a file that has something to show.
struct NewItem {
    track: i64,
    status: &'static str,
    message: Option<String>,
    changes: Vec<FieldChange>,
    snapshot: Option<TagSnapshot>,
    restore: Option<String>,
}

fn describe(error: &TagWriteError) -> String {
    match error {
        TagWriteError::Io(cause) if cause.kind() == io::ErrorKind::NotFound => {
            "this file is missing".to_owned()
        }
        other => other.to_string(),
    }
}

/// Sort tags are left alone by an edit, so a new name would sort by the old one's tag.
fn sort_tag_notes(tags: &TrackTags, changes: &[FieldChange]) -> Option<String> {
    let notes: Vec<String> = changes
        .iter()
        .filter_map(|change| {
            let (name, tag) = match change.field {
                TagField::Title => ("title", &tags.title_sort),
                TagField::Album => ("album", &tags.album_sort),
                TagField::Artists => ("artist", &tags.artist_sort),
                TagField::AlbumArtists => ("album artist", &tags.album_artist_sort),
                _ => return None,
            };
            tag.as_ref()
                .map(|value| format!("the {name} sort tag stays \"{value}\""))
        })
        .collect();
    (!notes.is_empty()).then(|| notes.join("; "))
}

fn plan_edit(track: i64, path: &Path, edits: &[FieldEdit]) -> Option<NewItem> {
    let blocked = |error: &TagWriteError, changes: Vec<FieldChange>| NewItem {
        track,
        status: ITEM_BLOCKED,
        message: Some(describe(error)),
        changes,
        snapshot: None,
        restore: None,
    };
    let requested = || {
        edits
            .iter()
            .map(|edit| FieldChange {
                field: edit.field,
                before: Vec::new(),
                after: edit.values.clone(),
            })
            .collect()
    };
    let tags = match read_tags(path) {
        Ok(tags) => tags,
        Err(error) => return Some(blocked(&error, requested())),
    };
    let changes: Vec<FieldChange> = edits
        .iter()
        .filter_map(|edit| {
            let before = field_values(&tags, edit.field);
            (before != edit.values).then(|| FieldChange {
                field: edit.field,
                before,
                after: edit.values.clone(),
            })
        })
        .collect();
    if changes.is_empty() {
        return None;
    }
    let fields: Vec<TagField> = changes.iter().map(|change| change.field).collect();
    match snapshot_tags(path, &fields) {
        Ok(snapshot) => Some(NewItem {
            track,
            status: ITEM_PENDING,
            message: sort_tag_notes(&tags, &changes),
            changes,
            snapshot: Some(snapshot),
            restore: None,
        }),
        Err(error) => Some(blocked(&error, changes)),
    }
}

fn plan_undo(track: i64, path: &Path, changes: &[FieldChange], restore: &str) -> Option<NewItem> {
    let reversed = |current: &TrackTags| -> Vec<FieldChange> {
        changes
            .iter()
            .map(|change| FieldChange {
                field: change.field,
                before: field_values(current, change.field),
                after: change.before.clone(),
            })
            .collect()
    };
    let blocked = |message: String, changes: Vec<FieldChange>| NewItem {
        track,
        status: ITEM_BLOCKED,
        message: Some(message),
        changes,
        snapshot: None,
        restore: None,
    };
    let tags = match read_tags(path) {
        Ok(tags) => tags,
        Err(error) => {
            let unknown = changes
                .iter()
                .map(|change| FieldChange {
                    field: change.field,
                    before: change.after.clone(),
                    after: change.before.clone(),
                })
                .collect();
            return Some(blocked(describe(&error), unknown));
        }
    };
    let holds = |want: fn(&FieldChange) -> &Vec<String>| {
        changes
            .iter()
            .all(|change| &field_values(&tags, change.field) == want(change))
    };
    if holds(|change| &change.before) {
        return None;
    }
    if !holds(|change| &change.after) {
        return Some(blocked(
            "this file's tags have changed since that edit, so it was left alone".to_owned(),
            reversed(&tags),
        ));
    }
    let fields: Vec<TagField> = changes.iter().map(|change| change.field).collect();
    match snapshot_tags(path, &fields) {
        Ok(snapshot) => Some(NewItem {
            track,
            status: ITEM_PENDING,
            message: None,
            changes: reversed(&tags),
            snapshot: Some(snapshot),
            restore: Some(restore.to_owned()),
        }),
        Err(error) => Some(blocked(describe(&error), reversed(&tags))),
    }
}

fn store_items(
    conn: &mut Connection,
    edit: i64,
    next_position: &mut i64,
    items: &mut Vec<NewItem>,
) -> Result<(), ApiError> {
    let tx = conn.transaction()?;
    {
        let mut insert = tx.prepare_cached(
            "INSERT INTO tag_edit_items (edit_id, position, track_id, status, message, changes,
                                         snapshot, restore)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for item in items.drain(..) {
            let changes = serde_json::to_string(&item.changes).map_err(json_error)?;
            let snapshot = item
                .snapshot
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(json_error)?;
            insert.execute(params![
                edit,
                *next_position,
                item.track,
                item.status,
                item.message,
                changes,
                snapshot,
                item.restore
            ])?;
            *next_position += 1;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Works out a batch from the files, which are only read. Returns whether it was canceled, in
/// which case nothing is kept. Only one batch waits to be written at a time, so a new preview
/// replaces any earlier one.
pub fn preview(
    db: &Database,
    edit_id: &str,
    undoes: Option<&str>,
    targets: &[Target],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<bool, ApiError> {
    let edit = db.write(|conn| {
        conn.execute(
            "DELETE FROM tag_edits WHERE status = ?1",
            params![EDIT_READY],
        )?;
        conn.execute(
            "INSERT INTO tag_edits (public_id, undoes_id, status, created_at)
             VALUES (?1, (SELECT id FROM tag_edits WHERE public_id = ?2), ?3, ?4)",
            params![edit_id, undoes, EDIT_PREVIEWING, now_ms()],
        )?;
        Ok::<_, ApiError>(conn.last_insert_rowid())
    })?;

    let total = targets.len() as u64;
    let mut items = Vec::new();
    let mut position = 0;
    let mut unchanged = 0_i64;
    let mut last_store = Instant::now();
    let mut canceled = false;
    progress(0, total);
    for (index, target) in targets.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            canceled = true;
            break;
        }
        let item = match target {
            Target::Edit { track, path, edits } => plan_edit(*track, path, edits),
            Target::Undo {
                track,
                path,
                changes,
                restore,
            } => plan_undo(*track, path, changes, restore),
        };
        match item {
            Some(item) => items.push(item),
            None => unchanged += 1,
        }
        if items.len() >= BATCH_ROWS || last_store.elapsed() >= BATCH_INTERVAL {
            db.write(|conn| store_items(conn, edit, &mut position, &mut items))?;
            last_store = Instant::now();
        }
        progress(index as u64 + 1, total);
    }

    db.write(|conn| {
        if canceled {
            conn.execute("DELETE FROM tag_edits WHERE id = ?1", [edit])?;
            return Ok(());
        }
        store_items(conn, edit, &mut position, &mut items)?;
        conn.execute(
            "UPDATE tag_edits SET status = ?2, unchanged = ?3 WHERE id = ?1",
            params![edit, EDIT_READY, unchanged],
        )?;
        Ok::<_, ApiError>(())
    })?;
    Ok(canceled)
}

struct PendingWrite {
    position: i64,
    track: i64,
    root_id: i64,
    root: String,
    rel_path: String,
    fingerprint: Option<Vec<u8>>,
    changes: Vec<FieldChange>,
    seen: TagSnapshot,
    restore: Option<TagSnapshot>,
}

fn pending_writes(conn: &Connection, edit: i64) -> Result<Vec<PendingWrite>, ApiError> {
    let mut statement = conn.prepare_cached(
        "SELECT i.position, i.track_id, t.root_id, r.path, t.rel_path, t.fingerprint, i.changes,
                i.snapshot, i.restore
         FROM tag_edit_items i
         JOIN tracks t ON t.id = i.track_id
         JOIN library_roots r ON r.id = t.root_id
         WHERE i.edit_id = ?1 AND i.status = ?2 ORDER BY i.position",
    )?;
    let rows = statement.query_map(params![edit, ITEM_PENDING], |row| {
        Ok((
            (
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<Vec<u8>>>(5)?,
            ),
            (
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, Option<String>>(8)?,
            ),
        ))
    })?;
    let mut writes = Vec::new();
    for row in rows {
        let ((position, track, root_id, root, rel_path, fingerprint), (changes, seen, restore)) =
            row?;
        writes.push(PendingWrite {
            position,
            track,
            root_id,
            root,
            rel_path,
            fingerprint,
            changes: serde_json::from_str(&changes).map_err(json_error)?,
            seen: serde_json::from_str(&seen).map_err(json_error)?,
            restore: restore
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .map_err(json_error)?,
        });
    }
    Ok(writes)
}

fn set_item(
    conn: &Connection,
    edit: i64,
    position: i64,
    status: &str,
    message: Option<&str>,
) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "UPDATE tag_edit_items SET status = ?3, message = coalesce(?4, message)
         WHERE edit_id = ?1 AND position = ?2",
    )?
    .execute(params![edit, position, status, message])?;
    Ok(())
}

/// Reads a file that was just written and brings the library up to date with it, in the same
/// transaction that records the write. The audio is unchanged, so the fingerprint still holds.
fn record_written(
    conn: &mut Connection,
    catalog: &mut Catalog,
    edit: i64,
    write: &PendingWrite,
    path: &Path,
) -> Result<(), ApiError> {
    let read = || -> Result<_, String> {
        let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
        let info = read_file(File::open(path).map_err(|error| error.to_string())?)?;
        Ok((metadata, info))
    };
    let (metadata, info) = read().map_err(|message| {
        ApiError::new(
            ErrorCode::Internal,
            format!("a file was written, but could not be read again: {message}"),
        )
    })?;
    let tx = conn.transaction()?;
    store::apply_file(
        &tx,
        catalog,
        &FileRecord {
            root_id: write.root_id,
            rel_path: &write.rel_path,
            size: metadata.len() as i64,
            mtime: walk::modified_ms(&metadata),
            existing: Some(write.track),
            fingerprint: write.fingerprint.as_deref(),
            info: &info,
        },
        now_ms(),
    )?;
    store::refresh_albums(&tx, &catalog.touched_albums)?;
    catalog.touched_albums.clear();
    set_item(&tx, edit, write.position, ITEM_WRITTEN, None)?;
    tx.commit()?;
    Ok(())
}

/// Writes a batch that is ready. The file comes first, then the library, so a stop in between
/// leaves a file a rescan will pick up. Returns whether it was canceled.
pub fn write(
    db: &Database,
    edit_id: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<bool, ApiError> {
    let result = write_inner(db, edit_id, cancel, progress);
    if result.is_err() {
        recover_interrupted(db)?;
    }
    result
}

fn write_inner(
    db: &Database,
    edit_id: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<bool, ApiError> {
    let (edit, writes) = db.write(|conn| {
        let tx = conn.transaction()?;
        let edit: i64 = tx
            .query_row(
                "SELECT id FROM tag_edits WHERE public_id = ?1",
                [edit_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(not_found)?;
        let claimed = tx.execute(
            "UPDATE tag_edits SET status = ?2 WHERE id = ?1 AND status = ?3",
            params![edit, EDIT_WRITING, EDIT_READY],
        )?;
        if claimed == 0 {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "this edit is not waiting to be written",
            ));
        }
        let writes = pending_writes(&tx, edit)?;
        tx.commit()?;
        Ok((edit, writes))
    })?;

    let total = writes.len() as u64;
    let mut catalog = Catalog::default();
    let mut canceled = false;
    progress(0, total);
    for (index, write) in writes.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            canceled = true;
            break;
        }
        let path = track_path(&write.root, &write.rel_path);
        // Marked first, so that a stop during the write is known about afterwards.
        db.write(|conn| {
            set_item(conn, edit, write.position, ITEM_WRITING, None)?;
            Ok::<_, ApiError>(())
        })?;
        let expected: Vec<FieldEdit> = write
            .changes
            .iter()
            .map(|change| FieldEdit {
                field: change.field,
                values: change.after.clone(),
            })
            .collect();
        let written = match &write.restore {
            Some(saved) => restore_tags(&path, saved, &expected, &write.seen),
            None => write_previewed(&path, &expected, &write.seen),
        };
        match written {
            Ok(()) => {
                db.write(|conn| record_written(conn, &mut catalog, edit, write, &path))?;
            }
            Err(error) => db.write(|conn| {
                set_item(
                    conn,
                    edit,
                    write.position,
                    ITEM_FAILED,
                    Some(&describe(&error)),
                )?;
                Ok::<_, ApiError>(())
            })?,
        }
        progress(index as u64 + 1, total);
    }

    db.write(|conn| {
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE tag_edit_items SET status = ?3 WHERE edit_id = ?1 AND status = ?2",
            params![edit, ITEM_PENDING, ITEM_SKIPPED],
        )?;
        if catalog.changed {
            store::remove_orphans(&tx)?;
        }
        tx.execute(
            "UPDATE tag_edits SET status = ?2, finished_at = ?3 WHERE id = ?1",
            params![
                edit,
                if canceled { EDIT_CANCELED } else { EDIT_DONE },
                now_ms()
            ],
        )?;
        tx.commit()?;
        Ok::<_, ApiError>(())
    })?;
    Ok(canceled)
}

/// Throws away a batch that has not been written.
pub fn discard(conn: &Connection, edit_id: &str) -> Result<(), ApiError> {
    let removed = conn.execute(
        "DELETE FROM tag_edits WHERE public_id = ?1 AND status = ?2",
        params![edit_id, EDIT_READY],
    )?;
    if removed == 0 {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            "this edit is not waiting to be written",
        ));
    }
    Ok(())
}

/// Tidies up after an app that stopped during a preview or a write. A file being written may
/// hold either version, and its temporary copy may still be beside it. An undo looks at what
/// the file holds, so it can still handle such a file.
pub fn recover_interrupted(db: &Database) -> Result<(), DbError> {
    let in_flight: Vec<PathBuf> = db.read(|conn| {
        let mut statement = conn.prepare(
            "SELECT r.path, t.rel_path FROM tag_edit_items i
             JOIN tracks t ON t.id = i.track_id
             JOIN library_roots r ON r.id = t.root_id
             WHERE i.status = ?1",
        )?;
        let rows = statement.query_map([ITEM_WRITING], |row| {
            Ok(track_path(
                &row.get::<_, String>(0)?,
                &row.get::<_, String>(1)?,
            ))
        })?;
        Ok::<_, DbError>(rows.collect::<Result<_, _>>()?)
    })?;
    in_flight.iter().for_each(|path| remove_leftover(path));

    db.write(|conn| {
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM tag_edits WHERE status = ?1", [EDIT_PREVIEWING])?;
        tx.execute(
            "UPDATE tag_edit_items SET status = ?2,
                 message = 'the app stopped during this write, so the file may hold either version'
             WHERE status = ?1",
            params![ITEM_WRITING, ITEM_UNSURE],
        )?;
        tx.execute(
            "UPDATE tag_edit_items SET status = ?2
             WHERE status = ?1
               AND edit_id IN (SELECT id FROM tag_edits WHERE status = ?3)",
            params![ITEM_PENDING, ITEM_SKIPPED, EDIT_WRITING],
        )?;
        tx.execute(
            "UPDATE tag_edits SET status = ?2, finished_at = ?3 WHERE status = ?1",
            params![EDIT_WRITING, EDIT_INTERRUPTED, now_ms()],
        )?;
        tx.commit()?;
        Ok(())
    })
}

const EDIT_COLUMNS: &str = "e.id, e.public_id,
    (SELECT public_id FROM tag_edits WHERE id = e.undoes_id),
    (SELECT u.public_id FROM tag_edits u
     WHERE u.undoes_id = e.id AND u.status NOT IN ('previewing', 'ready')
     ORDER BY u.id DESC LIMIT 1),
    e.status, e.created_at, e.finished_at, e.unchanged";

fn edit_row(conn: &Connection, row: &rusqlite::Row<'_>) -> rusqlite::Result<TagEdit> {
    let mut edit = TagEdit {
        id: row.get(1)?,
        undoes: row.get(2)?,
        undone_by: row.get(3)?,
        status: row.get(4)?,
        created_at: row.get(5)?,
        finished_at: row.get(6)?,
        unchanged: row.get(7)?,
        changing: 0,
        blocked: 0,
        written: 0,
        failed: 0,
        skipped: 0,
        unsure: 0,
    };
    let mut counts = conn.prepare_cached(
        "SELECT status, count(*) FROM tag_edit_items WHERE edit_id = ?1 GROUP BY status",
    )?;
    let rows = counts.query_map([row.get::<_, i64>(0)?], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
    })?;
    for row in rows {
        let (status, count) = row?;
        match status.as_str() {
            ITEM_BLOCKED => edit.blocked = count,
            ITEM_WRITTEN => edit.written = count,
            ITEM_FAILED => edit.failed = count,
            ITEM_SKIPPED => edit.skipped = count,
            ITEM_UNSURE => edit.unsure = count,
            _ => {}
        }
        if status != ITEM_BLOCKED {
            edit.changing += count;
        }
    }
    Ok(edit)
}

pub fn get(conn: &Connection, edit_id: &str) -> Result<TagEdit, ApiError> {
    conn.prepare_cached(&format!(
        "SELECT {EDIT_COLUMNS} FROM tag_edits e WHERE e.public_id = ?1"
    ))?
    .query_row([edit_id], |row| edit_row(conn, row))
    .optional()?
    .ok_or_else(not_found)
}

/// Batches that were written, newest first.
pub fn history(conn: &Connection, offset: u32, limit: u32) -> Result<TagEditPage, ApiError> {
    let shown = "e.status NOT IN ('previewing', 'ready')";
    let total = conn
        .prepare_cached(&format!("SELECT count(*) FROM tag_edits e WHERE {shown}"))?
        .query_row([], |row| row.get(0))?;
    let edits = conn
        .prepare_cached(&format!(
            "SELECT {EDIT_COLUMNS} FROM tag_edits e WHERE {shown}
             ORDER BY e.id DESC LIMIT ?1 OFFSET ?2"
        ))?
        .query_map(params![limit.min(MAX_PAGE), offset], |row| {
            edit_row(conn, row)
        })?
        .collect::<Result<_, _>>()?;
    Ok(TagEditPage { total, edits })
}

/// A batch's files, in the order they were asked for. `problems` keeps only those that were
/// not, or may not have been, written.
pub fn items(
    conn: &Connection,
    edit_id: &str,
    problems: bool,
    offset: u32,
    limit: u32,
) -> Result<TagEditItemPage, ApiError> {
    let edit: i64 = conn
        .prepare_cached("SELECT id FROM tag_edits WHERE public_id = ?1")?
        .query_row([edit_id], |row| row.get(0))
        .optional()?
        .ok_or_else(not_found)?;
    let shown = if problems {
        format!(
            "i.status IN ('{ITEM_BLOCKED}', '{ITEM_FAILED}', '{ITEM_SKIPPED}', '{ITEM_UNSURE}')"
        )
    } else {
        "1".to_owned()
    };
    let total = conn
        .prepare_cached(&format!(
            "SELECT count(*) FROM tag_edit_items i WHERE i.edit_id = ?1 AND {shown}"
        ))?
        .query_row([edit], |row| row.get(0))?;
    let mut statement = conn.prepare_cached(&format!(
        "SELECT i.position, t.public_id, t.rel_path, t.title, t.artist_display, i.status,
                i.message, i.changes
         FROM tag_edit_items i JOIN tracks t ON t.id = i.track_id
         WHERE i.edit_id = ?1 AND {shown} ORDER BY i.position LIMIT ?3 OFFSET ?2"
    ))?;
    let rows = statement.query_map(params![edit, offset, limit.min(MAX_PAGE)], |row| {
        let changes: String = row.get(7)?;
        Ok((
            TagEditItem {
                position: row.get(0)?,
                track_id: row.get(1)?,
                rel_path: row.get(2)?,
                title: row.get(3)?,
                artist: row.get(4)?,
                status: row.get(5)?,
                message: row.get(6)?,
                changes: Vec::new(),
            },
            changes,
        ))
    })?;
    let mut items = Vec::new();
    for row in rows {
        let (mut item, changes) = row?;
        item.changes = serde_json::from_str(&changes).map_err(json_error)?;
        items.push(item);
    }
    Ok(TagEditItemPage { total, items })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use serde_json::{Value, json};
    use tempfile::TempDir;

    use super::*;
    use crate::api::{Core, CoreEvent, EventSink, add_root, dispatch};
    use crate::scan::tests::mp3_bytes;
    use crate::scan::{ScanOptions, scan_library};

    const WAIT: Duration = Duration::from_secs(20);

    struct ChannelSink(Mutex<Sender<CoreEvent>>);

    impl EventSink for ChannelSink {
        fn emit(&self, event: &CoreEvent) {
            let _ = self.0.lock().unwrap().send(event.clone());
        }
    }

    struct Fixture {
        dir: TempDir,
        core: Core,
        events: Receiver<CoreEvent>,
    }

    impl Fixture {
        /// A library of MP3s, each given as a path and its ID3v2.4 frames.
        fn new(files: &[(&str, &[(&str, &str)])]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let music = dir.path().join("music");
            fs::create_dir_all(&music).unwrap();
            for (index, (rel, frames)) in files.iter().enumerate() {
                let path = music.join(rel);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, mp3_bytes(index as u32 + 1, frames)).unwrap();
            }
            let db = Database::open(dir.path().join("umak.db")).unwrap();
            add_root(&db, music.to_str().unwrap(), None).unwrap();
            scan_library(
                &db,
                None,
                ScanOptions::default(),
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
            let (sender, events) = mpsc::channel();
            let core = Core::new(db, Box::new(ChannelSink(Mutex::new(sender)))).unwrap();
            Self { dir, core, events }
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.dir.path().join("music").join(rel)
        }

        fn call(&self, name: &str, args: Value) -> Value {
            dispatch(&self.core, name, args).unwrap()
        }

        fn wait_for_job(&self) {
            loop {
                let event = self.events.recv_timeout(WAIT).unwrap();
                if matches!(event, CoreEvent::JobFinished { .. }) {
                    return;
                }
            }
        }

        fn job_status(&self) -> (String, Option<String>) {
            let jobs = self.call("jobs.list", json!({}));
            let job = &jobs["jobs"][0];
            (
                job["status"].as_str().unwrap().to_owned(),
                job["error"].as_str().map(str::to_owned),
            )
        }

        /// Every track's ID and title, by title.
        fn tracks(&self) -> Vec<(String, String)> {
            let page = self.call(
                "library.tracks.page",
                json!({"query": null, "sort": "title", "descending": false, "offset": 0, "limit": 100}),
            );
            page["tracks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|track| {
                    (
                        track["id"].as_str().unwrap().to_owned(),
                        track["title"].as_str().unwrap().to_owned(),
                    )
                })
                .collect()
        }

        fn track_id(&self, title: &str) -> String {
            self.tracks()
                .into_iter()
                .find(|(_, found)| found == title)
                .unwrap_or_else(|| panic!("no track titled {title}"))
                .0
        }

        fn preview(&self, track_ids: &[String], field: &str, values: &[&str]) -> String {
            let started = self.call(
                "tags.preview.start",
                json!({"changes": [{
                    "trackIds": track_ids,
                    "edits": [{"field": field, "values": values}],
                }]}),
            );
            self.wait_for_job();
            started["editId"].as_str().unwrap().to_owned()
        }

        fn edit(&self, id: &str) -> Value {
            self.call("tags.edits.get", json!({"id": id}))
        }

        fn items(&self, id: &str) -> Vec<Value> {
            let page = self.call(
                "tags.edits.items",
                json!({"id": id, "problems": false, "offset": 0, "limit": 100}),
            );
            page["items"].as_array().unwrap().clone()
        }

        fn write(&self, id: &str) {
            self.call("tags.edits.write", json!({"id": id}));
            self.wait_for_job();
        }

        fn undo(&self, id: &str) -> String {
            let started = self.call("tags.edits.undo", json!({"id": id}));
            self.wait_for_job();
            started["editId"].as_str().unwrap().to_owned()
        }
    }

    const ONE: &[(&str, &str)] = &[("TIT2", "One"), ("TPE1", "Band"), ("TALB", "First")];
    const TWO: &[(&str, &str)] = &[("TIT2", "Two"), ("TPE1", "Band"), ("TALB", "First")];

    #[test]
    fn current_values_come_from_the_file_not_from_what_the_library_fills_in() {
        let fixture = Fixture::new(&[
            ("a/1.mp3", ONE),
            ("a/no title.mp3", &[("TPE1", "Band")]),
            ("a/gone.mp3", TWO),
        ]);
        let tracks = fixture.tracks();
        let ids: Vec<String> = ["One", "no title", "Two"]
            .iter()
            .map(|title| {
                tracks
                    .iter()
                    .find(|(_, found)| found == title)
                    .unwrap()
                    .0
                    .clone()
            })
            .collect();
        fs::remove_file(fixture.path("a/gone.mp3")).unwrap();
        let mut asked: Vec<&str> = ids.iter().map(String::as_str).collect();
        asked.push("not-a-track");

        let list = fixture.call("tags.read", json!({"trackIds": asked}));

        let tracks = list["tracks"].as_array().unwrap();
        let value = |track: &Value, field: &str| {
            track["fields"]
                .as_array()
                .unwrap()
                .iter()
                .find(|found| found["field"] == field)
                .unwrap()["values"]
                .clone()
        };
        assert_eq!(tracks[0]["relPath"], "a/1.mp3");
        assert_eq!(tracks[0]["error"], Value::Null);
        assert_eq!(value(&tracks[0], "title"), json!(["One"]));
        assert_eq!(value(&tracks[0], "album"), json!(["First"]));
        assert_eq!(tracks[0]["fields"].as_array().unwrap().len(), 9);
        // The list of the library shows the file's name, but the tag is empty.
        assert_eq!(value(&tracks[1], "title"), json!([]));
        assert_eq!(value(&tracks[1], "album"), json!([]));
        assert_eq!(tracks[2]["error"], "this file is missing");
        assert_eq!(tracks[3]["relPath"], Value::Null);
        assert!(tracks[3]["error"].as_str().unwrap().contains("no longer"));
    }

    #[test]
    fn a_batch_can_be_narrowed_to_the_files_with_problems() {
        let fixture = Fixture::new(&[("a/1.mp3", ONE), ("a/2.mp3", TWO)]);
        let ids = [fixture.track_id("One"), fixture.track_id("Two")];
        fs::remove_file(fixture.path("a/2.mp3")).unwrap();
        let edit = fixture.preview(&ids, "album", &["Second"]);
        let page = fixture.call(
            "tags.edits.items",
            json!({"id": edit, "problems": true, "offset": 0, "limit": 100}),
        );
        assert_eq!(page["total"], 1);
        assert_eq!(page["items"].as_array().unwrap().len(), 1);
        assert_eq!(page["items"][0]["relPath"], "a/2.mp3");
        let past_it = fixture.call(
            "tags.edits.items",
            json!({"id": edit, "problems": true, "offset": 1, "limit": 100}),
        );
        assert_eq!(past_it["items"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn a_preview_shows_every_change_and_touches_nothing() {
        let fixture = Fixture::new(&[("a/1.mp3", ONE), ("a/2.mp3", TWO)]);
        let before = fs::read(fixture.path("a/1.mp3")).unwrap();
        let ids = [fixture.track_id("One"), fixture.track_id("Two")];

        let edit = fixture.preview(&ids, "album", &["Second"]);

        let info = fixture.edit(&edit);
        assert_eq!(info["status"], "ready");
        assert_eq!(info["changing"], 2);
        let items = fixture.items(&edit);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["title"], "One");
        assert_eq!(items[0]["status"], "pending");
        assert_eq!(
            items[0]["changes"],
            json!([{"field": "album", "before": ["First"], "after": ["Second"]}])
        );
        assert_eq!(fs::read(fixture.path("a/1.mp3")).unwrap(), before);
        // Nothing appears in the history until it is written.
        let history = fixture.call("tags.edits.list", json!({"offset": 0, "limit": 10}));
        assert_eq!(history["total"], 0);
    }

    #[test]
    fn a_track_already_holding_the_values_is_counted_not_listed() {
        let fixture = Fixture::new(&[
            ("a/1.mp3", ONE),
            ("a/2.mp3", &[("TIT2", "Two"), ("TALB", "Second")]),
        ]);
        let ids = [fixture.track_id("One"), fixture.track_id("Two")];
        let edit = fixture.preview(&ids, "album", &["Second"]);
        let info = fixture.edit(&edit);
        assert_eq!(
            (&info["changing"], &info["unchanged"]),
            (&json!(1), &json!(1))
        );
        assert_eq!(fixture.items(&edit).len(), 1);
    }

    #[test]
    fn files_that_cannot_be_changed_are_shown_with_the_reason() {
        let fixture = Fixture::new(&[
            ("a/1.mp3", ONE),
            ("a/2.mp3", TWO),
            ("a/3.mp3", &[("TIT2", "Three")]),
        ]);
        let ids = [
            fixture.track_id("One"),
            fixture.track_id("Two"),
            fixture.track_id("Three"),
        ];
        fs::remove_file(fixture.path("a/2.mp3")).unwrap();
        let mut bytes = fs::read(fixture.path("a/3.mp3")).unwrap();
        bytes[3] = 2;
        fs::write(fixture.path("a/3.mp3"), bytes).unwrap();

        let edit = fixture.preview(&ids, "album", &["Second"]);

        let info = fixture.edit(&edit);
        assert_eq!(
            (&info["changing"], &info["blocked"]),
            (&json!(1), &json!(2))
        );
        let items = fixture.items(&edit);
        assert_eq!(items[1]["status"], "blocked");
        assert_eq!(items[1]["message"], "this file is missing");
        assert_eq!(items[2]["status"], "blocked");

        // Only the one that can be written is.
        fixture.write(&edit);
        let info = fixture.edit(&edit);
        assert_eq!((&info["written"], &info["blocked"]), (&json!(1), &json!(2)));
    }

    #[test]
    fn a_sort_tag_left_behind_is_pointed_out() {
        let fixture = Fixture::new(&[(
            "a/1.mp3",
            &[("TIT2", "One"), ("TPE1", "The Band"), ("TSOP", "Band, The")],
        )]);
        let edit = fixture.preview(&[fixture.track_id("One")], "artists", &["Other"]);
        let message = fixture.items(&edit)[0]["message"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(message.contains("Band, The"), "{message}");
    }

    #[test]
    fn writing_changes_the_files_and_the_library_follows() {
        let fixture = Fixture::new(&[("a/1.mp3", ONE), ("a/2.mp3", TWO)]);
        let ids = [fixture.track_id("One"), fixture.track_id("Two")];
        let edit = fixture.preview(&ids, "album", &["Second"]);

        fixture.write(&edit);

        assert_eq!(fixture.job_status().0, "completed");
        let info = fixture.edit(&edit);
        assert_eq!(info["status"], "done");
        assert_eq!(info["written"], 2);
        assert!(info["finishedAt"].is_i64());
        assert_eq!(
            read_tags(&fixture.path("a/1.mp3"))
                .unwrap()
                .album
                .as_deref(),
            Some("Second")
        );

        // The track keeps its ID, the search finds the new value, and the old album is gone.
        let found = fixture.call(
            "library.tracks.page",
            json!({"query": "second", "sort": "title", "descending": false, "offset": 0, "limit": 10}),
        );
        assert_eq!(found["total"], 2);
        assert_eq!(found["tracks"][0]["id"], ids[0].as_str());
        assert_eq!(found["tracks"][0]["album"], "Second");
        let albums: i64 = fixture.core_count("SELECT count(*) FROM albums");
        assert_eq!(albums, 1);

        // A scan finds nothing new to do, because the library recorded the new file stamps.
        let scan_args = json!({"roots": null});
        fixture.call("library.scan.start", scan_args);
        fixture.wait_for_job();
        let jobs = fixture.call("jobs.list", json!({}));
        assert_eq!(jobs["jobs"][0]["scan"]["updated"], 0);
        assert_eq!(jobs["jobs"][0]["scan"]["unchanged"], 2);
    }

    #[test]
    fn a_file_changed_after_the_preview_is_left_alone() {
        let fixture = Fixture::new(&[("a/1.mp3", ONE), ("a/2.mp3", TWO)]);
        let ids = [fixture.track_id("One"), fixture.track_id("Two")];
        let edit = fixture.preview(&ids, "album", &["Second"]);

        crate::tagwrite::write_tags(
            &fixture.path("a/2.mp3"),
            &[FieldEdit {
                field: TagField::Album,
                values: vec!["Changed elsewhere".to_owned()],
            }],
        )
        .unwrap();
        let held = fs::read(fixture.path("a/2.mp3")).unwrap();

        fixture.write(&edit);

        let info = fixture.edit(&edit);
        assert_eq!((&info["written"], &info["failed"]), (&json!(1), &json!(1)));
        assert_eq!(fs::read(fixture.path("a/2.mp3")).unwrap(), held);
        let items = fixture.items(&edit);
        assert_eq!(items[1]["status"], "failed");
        assert!(
            items[1]["message"]
                .as_str()
                .unwrap()
                .contains("after the preview")
        );
    }

    #[test]
    fn a_batch_can_only_be_written_once_and_a_new_preview_replaces_a_waiting_one() {
        let fixture = Fixture::new(&[("a/1.mp3", ONE)]);
        let ids = [fixture.track_id("One")];
        let first = fixture.preview(&ids, "album", &["Second"]);
        let second = fixture.preview(&ids, "album", &["Third"]);
        let gone = dispatch(&fixture.core, "tags.edits.get", json!({"id": first})).unwrap_err();
        assert_eq!(gone.code, ErrorCode::NotFound);

        fixture.write(&second);
        let again = dispatch(&fixture.core, "tags.edits.write", json!({"id": second})).unwrap_err();
        assert_eq!(again.code, ErrorCode::Conflict);
    }

    #[test]
    fn a_waiting_batch_can_be_discarded() {
        let fixture = Fixture::new(&[("a/1.mp3", ONE)]);
        let edit = fixture.preview(&[fixture.track_id("One")], "album", &["Second"]);
        fixture.call("tags.edits.discard", json!({"id": edit}));
        let gone = dispatch(&fixture.core, "tags.edits.get", json!({"id": edit})).unwrap_err();
        assert_eq!(gone.code, ErrorCode::NotFound);
    }

    #[test]
    fn bad_requests_are_refused_before_a_job_starts() {
        let fixture = Fixture::new(&[("a/1.mp3", ONE)]);
        let id = fixture.track_id("One");
        for (changes, code) in [
            (json!([]), ErrorCode::InvalidArgs),
            (
                json!([{"trackIds": [id], "edits": []}]),
                ErrorCode::InvalidArgs,
            ),
            (
                json!([{"trackIds": [id, id], "edits": [{"field": "album", "values": ["A"]}]}]),
                ErrorCode::InvalidArgs,
            ),
            (
                json!([{"trackIds": [id], "edits": [{"field": "year", "values": ["soon"]}]}]),
                ErrorCode::InvalidArgs,
            ),
            (
                json!([{"trackIds": ["nope"], "edits": [{"field": "album", "values": ["A"]}]}]),
                ErrorCode::NotFound,
            ),
        ] {
            let error = dispatch(
                &fixture.core,
                "tags.preview.start",
                json!({"changes": changes}),
            )
            .unwrap_err();
            assert_eq!(error.code, code);
        }
        assert_eq!(fixture.call("jobs.list", json!({}))["jobs"], json!([]));
    }

    #[test]
    fn undo_puts_back_what_was_there_including_what_the_model_cannot_hold() {
        let dated: &[(&str, &str)] = &[
            ("TIT2", "One"),
            ("TDRC", "2001-05-03"),
            ("TRCK", "03/12"),
            ("TPE1", "Band"),
        ];
        let fixture = Fixture::new(&[("a/1.mp3", dated)]);
        let id = fixture.track_id("One");
        let original = read_tags(&fixture.path("a/1.mp3")).unwrap();

        let started = fixture.call(
            "tags.preview.start",
            json!({"changes": [{"trackIds": [id], "edits": [
                {"field": "year", "values": ["1999"]},
                {"field": "trackNumber", "values": ["7"]},
                {"field": "artists", "values": ["Other", "Another"]},
            ]}]}),
        );
        fixture.wait_for_job();
        let edit = started["editId"].as_str().unwrap().to_owned();
        fixture.write(&edit);
        assert_eq!(
            read_tags(&fixture.path("a/1.mp3")).unwrap().year,
            Some(1999)
        );

        let undo = fixture.undo(&edit);

        // The undo is previewed like any other change.
        let items = fixture.items(&undo);
        assert_eq!(items[0]["status"], "pending");
        assert_eq!(items[0]["changes"][0]["before"], json!(["1999"]));
        assert_eq!(items[0]["changes"][0]["after"], json!(["2001"]));
        fixture.write(&undo);

        assert_eq!(fixture.edit(&undo)["written"], 1);
        assert_eq!(fixture.edit(&undo)["undoes"], edit.as_str());
        assert_eq!(fixture.edit(&edit)["undoneBy"], undo.as_str());
        assert_eq!(read_tags(&fixture.path("a/1.mp3")).unwrap(), original);
        let bytes = fs::read(fixture.path("a/1.mp3")).unwrap();
        assert!(bytes.windows(10).any(|window| window == b"2001-05-03"));
        assert!(bytes.windows(5).any(|window| window == b"03/12"));

        // The library is back as well.
        let track = fixture.call(
            "library.tracks.page",
            json!({"query": null, "sort": "title", "descending": false, "offset": 0, "limit": 1}),
        );
        assert_eq!(track["tracks"][0]["artist"], "Band");
    }

    #[test]
    fn an_undo_can_itself_be_undone() {
        let fixture = Fixture::new(&[("a/1.mp3", ONE)]);
        let edit = fixture.preview(&[fixture.track_id("One")], "album", &["Second"]);
        fixture.write(&edit);
        let undo = fixture.undo(&edit);
        fixture.write(&undo);
        let redo = fixture.undo(&undo);
        fixture.write(&redo);
        assert_eq!(
            read_tags(&fixture.path("a/1.mp3"))
                .unwrap()
                .album
                .as_deref(),
            Some("Second")
        );
        let history = fixture.call("tags.edits.list", json!({"offset": 0, "limit": 10}));
        assert_eq!(history["total"], 3);
        assert_eq!(history["edits"][0]["id"], redo.as_str());
    }

    #[test]
    fn undo_leaves_alone_a_file_changed_since_the_edit() {
        let fixture = Fixture::new(&[("a/1.mp3", ONE), ("a/2.mp3", TWO)]);
        let ids = [fixture.track_id("One"), fixture.track_id("Two")];
        let edit = fixture.preview(&ids, "album", &["Second"]);
        fixture.write(&edit);

        crate::tagwrite::write_tags(
            &fixture.path("a/2.mp3"),
            &[FieldEdit {
                field: TagField::Album,
                values: vec!["Third".to_owned()],
            }],
        )
        .unwrap();

        let undo = fixture.undo(&edit);
        let items = fixture.items(&undo);
        assert_eq!(items[0]["status"], "pending");
        assert_eq!(items[1]["status"], "blocked");
        fixture.write(&undo);
        assert_eq!(
            read_tags(&fixture.path("a/1.mp3"))
                .unwrap()
                .album
                .as_deref(),
            Some("First")
        );
        assert_eq!(
            read_tags(&fixture.path("a/2.mp3"))
                .unwrap()
                .album
                .as_deref(),
            Some("Third")
        );
    }

    #[test]
    fn undoing_twice_finds_nothing_left_to_do() {
        let fixture = Fixture::new(&[("a/1.mp3", ONE)]);
        let edit = fixture.preview(&[fixture.track_id("One")], "album", &["Second"]);
        fixture.write(&edit);
        let undo = fixture.undo(&edit);
        fixture.write(&undo);
        let again = fixture.undo(&edit);
        let info = fixture.edit(&again);
        assert_eq!(
            (&info["changing"], &info["unchanged"]),
            (&json!(0), &json!(1))
        );
    }

    #[test]
    fn a_canceled_write_keeps_what_it_did_and_that_part_can_be_undone() {
        let files: Vec<(String, Vec<(&str, String)>)> = (0..6)
            .map(|index| {
                (
                    format!("a/{index}.mp3"),
                    vec![("TIT2", format!("T{index}")), ("TALB", "First".to_owned())],
                )
            })
            .collect();
        let frames: Vec<Vec<(&str, &str)>> = files
            .iter()
            .map(|(_, frames)| {
                frames
                    .iter()
                    .map(|(id, text)| (*id, text.as_str()))
                    .collect()
            })
            .collect();
        let list: Vec<(&str, &[(&str, &str)])> = files
            .iter()
            .zip(&frames)
            .map(|((rel, _), frames)| (rel.as_str(), frames.as_slice()))
            .collect();
        let fixture = Fixture::new(&list);
        let ids: Vec<String> = fixture.tracks().into_iter().map(|(id, _)| id).collect();
        let edit = fixture.preview(&ids, "album", &["Second"]);

        // Cancel from the first progress report past the start, so one file has been written.
        let db = fixture.core_db();
        let cancel = AtomicBool::new(false);
        let canceled = write(&db, &edit, &cancel, &mut |done, _| {
            if done == 1 {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .unwrap();
        assert!(canceled);

        let info = fixture.edit(&edit);
        assert_eq!(info["status"], "canceled");
        assert_eq!((&info["written"], &info["skipped"]), (&json!(1), &json!(5)));

        let undo = fixture.undo(&edit);
        assert_eq!(fixture.edit(&undo)["changing"], 1);
        fixture.write(&undo);
        let page = fixture.call(
            "library.tracks.page",
            json!({"query": "first", "sort": "title", "descending": false, "offset": 0, "limit": 10}),
        );
        assert_eq!(page["total"], 6);
    }

    #[test]
    fn a_write_cut_short_is_known_about_after_a_restart() {
        let fixture = Fixture::new(&[("a/1.mp3", ONE), ("a/2.mp3", TWO)]);
        let ids = [fixture.track_id("One"), fixture.track_id("Two")];
        let edit = fixture.preview(&ids, "album", &["Second"]);
        let db = fixture.core_db();

        // As a stop during the first write leaves things.
        crate::tagwrite::write_tags(
            &fixture.path("a/1.mp3"),
            &[FieldEdit {
                field: TagField::Album,
                values: vec!["Second".to_owned()],
            }],
        )
        .unwrap();
        let leftover = fixture.path("a/1.mp3.umak-tmp");
        fs::write(&leftover, b"partial").unwrap();
        db.write(|conn| {
            conn.execute("UPDATE tag_edits SET status = 'writing'", [])?;
            conn.execute(
                "UPDATE tag_edit_items SET status = 'writing' WHERE position = 0",
                [],
            )?;
            Ok::<_, ApiError>(())
        })
        .unwrap();

        recover_interrupted(&db).unwrap();

        assert!(!leftover.exists());
        let info = fixture.edit(&edit);
        assert_eq!(info["status"], "interrupted");
        assert_eq!((&info["unsure"], &info["skipped"]), (&json!(1), &json!(1)));

        // The file did take the new values, so an undo can put them back.
        let undo = fixture.undo(&edit);
        assert_eq!(fixture.edit(&undo)["changing"], 1);
        fixture.write(&undo);
        assert_eq!(
            read_tags(&fixture.path("a/1.mp3"))
                .unwrap()
                .album
                .as_deref(),
            Some("First")
        );
    }

    #[test]
    fn a_preview_stopped_by_a_restart_leaves_nothing_behind() {
        let fixture = Fixture::new(&[("a/1.mp3", ONE)]);
        let edit = fixture.preview(&[fixture.track_id("One")], "album", &["Second"]);
        let db = fixture.core_db();
        db.write(|conn| {
            conn.execute("UPDATE tag_edits SET status = 'previewing'", [])?;
            Ok::<_, ApiError>(())
        })
        .unwrap();
        recover_interrupted(&db).unwrap();
        let gone = dispatch(&fixture.core, "tags.edits.get", json!({"id": edit})).unwrap_err();
        assert_eq!(gone.code, ErrorCode::NotFound);
    }

    impl Fixture {
        fn core_db(&self) -> Arc<Database> {
            self.core.database()
        }

        fn core_count(&self, sql: &str) -> i64 {
            self.core_db()
                .read(|conn| Ok::<_, DbError>(conn.query_row(sql, [], |row| row.get(0))?))
                .unwrap()
        }
    }
}
