//! Export and import between installations.
//!
//! An export is one versioned JSON file holding what cannot be rebuilt from the music
//! files: library roots, each track's identity, per-user data, playlists and settings.
//! Tags and caches are left out, because a scan rebuilds them.
//!
//! An import scans the chosen folders first, so tracks exist with their real tags, then
//! matches exported tracks by stable ID, then relative path, then fingerprint. Exported
//! tracks with no file are kept as missing, so their ratings and playlist entries wait
//! for the file. On a conflict existing data is kept, unless the imported value is the
//! only one. Importing the same file again changes nothing.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::api::{ApiError, ErrorCode, ensure_no_overlap, resolve_root_path};
use crate::db::{Database, new_public_id};
use crate::scan::store::{self, Catalog, FileRecord};
use crate::scan::tags::FileInfo;
use crate::scan::{self, ScanOptions, ScanProgress, ScanReport};
use crate::user_data::MAX_RATING;

const FORMAT: &str = "umak-library-export";
/// The newest version this app writes. Older versions stay readable: add a conversion
/// in `read_export` when this changes, and keep the old fixture test passing.
pub const CURRENT_VERSION: u32 = 1;
const MAX_REPORTED_MISSING: usize = 200;
const POSITION_GAP: i64 = 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportFile {
    pub format: String,
    pub version: u32,
    pub exported_at: i64,
    pub app_version: String,
    pub roots: Vec<ExportRoot>,
    pub tracks: Vec<ExportTrack>,
    pub users: Vec<ExportUser>,
    pub settings: Vec<ExportSetting>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRoot {
    pub id: String,
    pub name: String,
    pub path: String,
    pub added_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportTrack {
    pub id: String,
    pub root: String,
    pub rel_path: String,
    /// Hexadecimal, or absent if the track has none.
    pub fingerprint: Option<String>,
    pub added_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportUser {
    pub name: String,
    pub track_data: Vec<ExportTrackData>,
    pub history: Vec<ExportPlay>,
    pub playlists: Vec<ExportPlaylist>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportTrackData {
    pub track: String,
    pub rating: Option<i64>,
    pub play_count: i64,
    pub last_played_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportPlay {
    pub track: String,
    pub played_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportPlaylist {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub rule: Option<String>,
    pub current_position: Option<i64>,
    pub current_ms: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    /// Track IDs in order. A track may appear more than once.
    pub tracks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportSetting {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ExportSummary {
    pub path: String,
    pub tracks: u32,
    pub playlists: u32,
    pub rated_tracks: u32,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PreviewRoot {
    pub id: String,
    pub name: String,
    /// Where the folder was on the machine that made the export.
    pub path: String,
    pub track_count: u32,
    /// A local folder that looks right: the root already here, or the same path.
    pub suggested_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub path: String,
    pub version: u32,
    pub exported_at: i64,
    pub app_version: String,
    pub roots: Vec<PreviewRoot>,
    pub track_count: u32,
    pub playlist_count: u32,
    pub rated_track_count: u32,
}

/// Where one exported root lives here. `path` is null if it is not on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RootChoice {
    pub id: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub tracks: u64,
    pub matched_by_id: u64,
    pub matched_by_path: u64,
    pub matched_by_fingerprint: u64,
    /// Exported tracks with no file here, kept as missing.
    pub missing: u64,
    /// The first of those, as `root name/relative path`.
    pub missing_paths: Vec<String>,
    pub playlists_added: u64,
    /// Playlists left alone because this library already has them.
    pub playlists_kept: u64,
    pub track_data_added: u64,
    /// Tracks where existing ratings or play counts took priority.
    pub track_data_kept: u64,
    pub history_added: u64,
    pub settings_added: u64,
    /// References to tracks the export does not list. A sound export has none.
    pub unknown_references: u64,
    pub scan: Option<ScanReport>,
    pub canceled: bool,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::InvalidArgs, message)
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).ok())
        .collect()
}

/// Reads everything into the export format, from one consistent view of the database.
/// Lists are ordered by stable ID, so two exports of the same library can be compared.
fn collect(conn: &Connection) -> rusqlite::Result<ExportFile> {
    let tx = conn.unchecked_transaction()?;
    let roots = tx
        .prepare("SELECT public_id, name, path, added_at FROM library_roots ORDER BY public_id")?
        .query_map([], |row| {
            Ok(ExportRoot {
                id: row.get(0)?,
                name: row.get(1)?,
                path: row.get(2)?,
                added_at: row.get(3)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    let tracks = tx
        .prepare(
            "SELECT t.public_id, r.public_id, t.rel_path, t.fingerprint, t.added_at
             FROM tracks t JOIN library_roots r ON r.id = t.root_id ORDER BY t.public_id",
        )?
        .query_map([], |row| {
            Ok(ExportTrack {
                id: row.get(0)?,
                root: row.get(1)?,
                rel_path: row.get(2)?,
                fingerprint: row
                    .get::<_, Option<Vec<u8>>>(3)?
                    .map(|bytes| to_hex(&bytes)),
                added_at: row.get(4)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    let mut users = Vec::new();
    let user_rows: Vec<(i64, String)> = tx
        .prepare("SELECT id, name FROM users ORDER BY id")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (user, name) in user_rows {
        users.push(ExportUser {
            name,
            track_data: tx
                .prepare(
                    "SELECT t.public_id, d.rating, d.play_count, d.last_played_at
                     FROM track_user_data d JOIN tracks t ON t.id = d.track_id
                     WHERE d.user_id = ?1 ORDER BY t.public_id",
                )?
                .query_map([user], |row| {
                    Ok(ExportTrackData {
                        track: row.get(0)?,
                        rating: row.get(1)?,
                        play_count: row.get(2)?,
                        last_played_at: row.get(3)?,
                    })
                })?
                .collect::<Result<_, _>>()?,
            history: tx
                .prepare(
                    "SELECT t.public_id, h.played_at
                     FROM play_history h JOIN tracks t ON t.id = h.track_id
                     WHERE h.user_id = ?1 ORDER BY h.played_at, t.public_id",
                )?
                .query_map([user], |row| {
                    Ok(ExportPlay {
                        track: row.get(0)?,
                        played_at: row.get(1)?,
                    })
                })?
                .collect::<Result<_, _>>()?,
            playlists: collect_playlists(&tx, user)?,
        });
    }
    let settings = tx
        .prepare("SELECT key, value FROM settings ORDER BY key")?
        .query_map([], |row| {
            Ok(ExportSetting {
                key: row.get(0)?,
                value: row.get(1)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(ExportFile {
        format: FORMAT.to_owned(),
        version: CURRENT_VERSION,
        exported_at: now_ms(),
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        roots,
        tracks,
        users,
        settings,
    })
}

fn collect_playlists(conn: &Connection, user: i64) -> rusqlite::Result<Vec<ExportPlaylist>> {
    let rows: Vec<(i64, ExportPlaylist)> = conn
        .prepare(
            "SELECT id, public_id, name, kind, rule, current_position, current_ms, created_at,
                    updated_at
             FROM playlists WHERE user_id = ?1 ORDER BY public_id",
        )?
        .query_map([user], |row| {
            Ok((
                row.get(0)?,
                ExportPlaylist {
                    id: row.get(1)?,
                    name: row.get(2)?,
                    kind: row.get(3)?,
                    rule: row.get(4)?,
                    current_position: row.get(5)?,
                    current_ms: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                    tracks: Vec::new(),
                },
            ))
        })?
        .collect::<Result<_, _>>()?;
    let mut items = conn.prepare(
        "SELECT t.public_id FROM playlist_items i JOIN tracks t ON t.id = i.track_id
         WHERE i.playlist_id = ?1 ORDER BY i.position",
    )?;
    let mut playlists = Vec::with_capacity(rows.len());
    for (id, mut playlist) in rows {
        playlist.tracks = items
            .query_map([id], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        playlists.push(playlist);
    }
    Ok(playlists)
}

/// Writes the export next to its target first, so a failed write cannot damage an
/// earlier export at that path.
pub fn export_library(db: &Database, path: &Path) -> Result<ExportSummary, ApiError> {
    let export = db.read(|conn| Ok::<_, ApiError>(collect(conn)?))?;
    let text = serde_json::to_vec_pretty(&export)
        .map_err(|error| ApiError::new(ErrorCode::Internal, error.to_string()))?;
    let failed = |error: std::io::Error| invalid(format!("{}: {error}", path.display()));
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    let written = fs::File::create(&temp).and_then(|mut file| {
        file.write_all(&text)?;
        file.sync_all()
    });
    if let Err(error) = written.and_then(|()| fs::rename(&temp, path)) {
        let _ = fs::remove_file(&temp);
        return Err(failed(error));
    }
    Ok(ExportSummary {
        path: path.display().to_string(),
        tracks: export.tracks.len() as u32,
        playlists: export
            .users
            .iter()
            .flat_map(|user| &user.playlists)
            .filter(|playlist| playlist.kind != "player")
            .count() as u32,
        rated_tracks: rated_tracks(&export),
        bytes: text.len() as u64,
    })
}

fn rated_tracks(export: &ExportFile) -> u32 {
    export
        .users
        .iter()
        .flat_map(|user| &user.track_data)
        .filter(|data| data.rating.is_some())
        .count() as u32
}

/// Reads an export of any known version into the current format.
pub fn read_export(path: &Path) -> Result<ExportFile, ApiError> {
    let text = fs::read(path).map_err(|error| invalid(format!("{}: {error}", path.display())))?;
    let value: Value = serde_json::from_slice(&text)
        .map_err(|_| invalid("this file is not a Umak library export"))?;
    if value.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return Err(invalid("this file is not a Umak library export"));
    }
    let version = value.get("version").and_then(Value::as_u64).unwrap_or(0);
    let export: ExportFile = match version {
        1 => serde_json::from_value(value)
            .map_err(|error| invalid(format!("this export is damaged: {error}")))?,
        version if version > u64::from(CURRENT_VERSION) => {
            return Err(invalid(format!(
                "this export is version {version}; this app reads up to version {CURRENT_VERSION}. Update the app to import it."
            )));
        }
        version => return Err(invalid(format!("unknown export version {version}"))),
    };
    validate(&export)?;
    Ok(export)
}

/// Rejects anything the database would refuse, or that could reach outside a root,
/// before any change is made.
fn validate(export: &ExportFile) -> Result<(), ApiError> {
    let roots: HashSet<&str> = export.roots.iter().map(|root| root.id.as_str()).collect();
    for track in &export.tracks {
        let escapes = track.rel_path.is_empty()
            || track.rel_path.starts_with('/')
            || track.rel_path.contains('\\')
            || track.rel_path.contains(':')
            || track.rel_path.split('/').any(|part| part == "..");
        if escapes {
            return Err(invalid(format!(
                "this export has a bad path: {}",
                track.rel_path
            )));
        }
        if !roots.contains(track.root.as_str()) {
            return Err(invalid(format!(
                "track {} names an unknown folder",
                track.id
            )));
        }
        if let Some(print) = &track.fingerprint
            && from_hex(print).is_none()
        {
            return Err(invalid(format!("track {} has a bad fingerprint", track.id)));
        }
    }
    for user in &export.users {
        for data in &user.track_data {
            if data
                .rating
                .is_some_and(|rating| !(0..=MAX_RATING).contains(&rating))
            {
                return Err(invalid(format!("track {} has a bad rating", data.track)));
            }
        }
        for playlist in &user.playlists {
            let kind_ok = match playlist.kind.as_str() {
                "manual" | "player" => playlist.rule.is_none(),
                "smart" => playlist.rule.is_some(),
                _ => false,
            };
            if !kind_ok {
                return Err(invalid(format!("playlist {} is damaged", playlist.name)));
            }
        }
    }
    Ok(())
}

/// Describes an export so the user can say where its folders live on this machine.
pub fn preview(db: &Database, path: &Path) -> Result<ImportPreview, ApiError> {
    let export = read_export(path)?;
    let local: Vec<(String, String)> = db.read(|conn| {
        Ok::<_, ApiError>(
            conn.prepare("SELECT public_id, path FROM library_roots")?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<Result<_, _>>()?,
        )
    })?;
    let mut counts: HashMap<&str, u32> = HashMap::new();
    for track in &export.tracks {
        *counts.entry(track.root.as_str()).or_default() += 1;
    }
    let roots = export
        .roots
        .iter()
        .map(|root| {
            let known = local
                .iter()
                .find(|(id, _)| *id == root.id)
                .map(|(_, path)| path);
            let same_path = Path::new(&root.path).is_dir().then_some(&root.path);
            PreviewRoot {
                id: root.id.clone(),
                name: root.name.clone(),
                path: root.path.clone(),
                track_count: counts.get(root.id.as_str()).copied().unwrap_or(0),
                suggested_path: known.or(same_path).cloned(),
            }
        })
        .collect();
    Ok(ImportPreview {
        path: path.display().to_string(),
        version: export.version,
        exported_at: export.exported_at,
        app_version: export.app_version.clone(),
        roots,
        track_count: export.tracks.len() as u32,
        playlist_count: export
            .users
            .iter()
            .flat_map(|user| &user.playlists)
            .filter(|playlist| playlist.kind != "player")
            .count() as u32,
        rated_track_count: rated_tracks(&export),
    })
}

/// A chosen folder, checked and made canonical, ready for `import_library`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedChoice {
    id: String,
    path: Option<String>,
}

/// Checks the file and the chosen folders, so problems show before a long job starts.
pub fn check_import(
    path: &Path,
    choices: &[RootChoice],
) -> Result<(ExportFile, Vec<CheckedChoice>), ApiError> {
    let export = read_export(path)?;
    let checked = choices
        .iter()
        .map(|choice| {
            if !export.roots.iter().any(|root| root.id == choice.id) {
                return Err(invalid(format!("the export has no folder {}", choice.id)));
            }
            let path = match &choice.path {
                Some(path) => {
                    let resolved = resolve_root_path(path)?;
                    Some(
                        resolved
                            .to_str()
                            .ok_or_else(|| invalid("the folder path is not valid Unicode"))?
                            .to_owned(),
                    )
                }
                None => None,
            };
            Ok(CheckedChoice {
                id: choice.id.clone(),
                path,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    Ok((export, checked))
}

/// Makes each exported root exist here. Returns local row IDs by exported root ID, and
/// the public IDs of the roots to scan.
fn prepare_roots(
    conn: &Connection,
    export: &ExportFile,
    choices: &[CheckedChoice],
) -> Result<(HashMap<String, i64>, Vec<String>), ApiError> {
    let mut local_ids = HashMap::new();
    let mut to_scan = Vec::new();
    for root in &export.roots {
        let chosen = choices
            .iter()
            .find(|choice| choice.id == root.id)
            .and_then(|choice| choice.path.clone());
        let by_id: Option<(i64, String)> = conn
            .query_row(
                "SELECT id, path FROM library_roots WHERE public_id = ?1",
                [&root.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let wanted = chosen.as_deref().unwrap_or(&root.path);
        let by_path: Option<(i64, String)> = conn
            .query_row(
                "SELECT id, public_id FROM library_roots WHERE path = ?1",
                [wanted],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        let (local, public_id) = match (by_id, by_path) {
            // Already known here. An explicit choice moves it.
            (Some((local, current)), _) => {
                if let Some(path) = &chosen
                    && *path != current
                {
                    let others = other_roots_overlap(conn, local, path)?;
                    if let Some(other) = others {
                        return Err(ApiError::new(
                            ErrorCode::Conflict,
                            format!("overlaps the library folder {other}"),
                        ));
                    }
                    conn.execute(
                        "UPDATE library_roots SET path = ?2 WHERE id = ?1",
                        params![local, path],
                    )?;
                }
                (local, root.id.clone())
            }
            // The same folder under another ID: this library's own root wins.
            (None, Some((local, public_id))) => (local, public_id),
            (None, None) => {
                if chosen.is_some() {
                    ensure_no_overlap(conn, Path::new(wanted))?;
                }
                conn.execute(
                    "INSERT INTO library_roots (public_id, name, path, added_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![root.id, root.name, wanted, root.added_at],
                )?;
                (conn.last_insert_rowid(), root.id.clone())
            }
        };
        local_ids.insert(root.id.clone(), local);
        if chosen.is_some() {
            to_scan.push(public_id);
        }
    }
    Ok((local_ids, to_scan))
}

fn other_roots_overlap(
    conn: &Connection,
    skip: i64,
    path: &str,
) -> rusqlite::Result<Option<String>> {
    let rows: Vec<String> = conn
        .prepare("SELECT path FROM library_roots WHERE id <> ?1")?
        .query_map([skip], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let path = Path::new(path);
    Ok(rows
        .into_iter()
        .find(|other| path.starts_with(other) || Path::new(other).starts_with(path)))
}

struct LocalTrack {
    id: i64,
    public_id: String,
    root: i64,
    rel_path: String,
    fingerprint: Option<Vec<u8>>,
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Decides which local track each exported track is, in three passes, so a weaker
/// match can never take a track a stronger one would claim.
fn match_tracks(
    export: &ExportFile,
    local: &[LocalTrack],
    roots: &HashMap<String, i64>,
    report: &mut ImportReport,
) -> HashMap<String, usize> {
    let by_public: HashMap<&str, usize> = local
        .iter()
        .enumerate()
        .map(|(index, track)| (track.public_id.as_str(), index))
        .collect();
    let by_path: HashMap<(i64, &str), usize> = local
        .iter()
        .enumerate()
        .map(|(index, track)| ((track.root, track.rel_path.as_str()), index))
        .collect();
    let mut by_print: HashMap<&[u8], Vec<usize>> = HashMap::new();
    for (index, track) in local.iter().enumerate() {
        if let Some(print) = &track.fingerprint {
            by_print.entry(print).or_default().push(index);
        }
    }

    let mut matched: HashMap<String, usize> = HashMap::new();
    let mut claimed: HashSet<usize> = HashSet::new();
    for track in &export.tracks {
        if let Some(index) = by_public.get(track.id.as_str()) {
            matched.insert(track.id.clone(), *index);
            claimed.insert(*index);
            report.matched_by_id += 1;
        }
    }
    for track in &export.tracks {
        if matched.contains_key(&track.id) {
            continue;
        }
        let key = (roots[&track.root], track.rel_path.as_str());
        if let Some(index) = by_path.get(&key) {
            matched.insert(track.id.clone(), *index);
            claimed.insert(*index);
            report.matched_by_path += 1;
        }
    }
    for track in &export.tracks {
        if matched.contains_key(&track.id) {
            continue;
        }
        let Some(print) = track.fingerprint.as_deref().and_then(from_hex) else {
            continue;
        };
        let Some(candidates) = by_print.get(print.as_slice()) else {
            continue;
        };
        let free = || candidates.iter().filter(|index| !claimed.contains(index));
        let wanted = file_name(&track.rel_path);
        let chosen = free()
            .find(|index| file_name(&local[**index].rel_path) == wanted)
            .or_else(|| free().next())
            .copied();
        if let Some(index) = chosen {
            matched.insert(track.id.clone(), index);
            claimed.insert(index);
            report.matched_by_fingerprint += 1;
        }
    }
    matched
}

fn merge_track_data(
    conn: &Connection,
    user: i64,
    track: i64,
    data: &ExportTrackData,
    report: &mut ImportReport,
) -> rusqlite::Result<()> {
    let existing: Option<(Option<i64>, i64, Option<i64>)> = conn
        .prepare_cached(
            "SELECT rating, play_count, last_played_at FROM track_user_data
             WHERE user_id = ?1 AND track_id = ?2",
        )?
        .query_row(params![user, track], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()?;
    let Some((rating, play_count, last_played)) = existing else {
        conn.prepare_cached(
            "INSERT INTO track_user_data (user_id, track_id, rating, play_count, last_played_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?
        .execute(params![
            user,
            track,
            data.rating,
            data.play_count,
            data.last_played_at
        ])?;
        report.track_data_added += 1;
        return Ok(());
    };
    let filled_in = (rating.is_none() && data.rating.is_some())
        || (play_count == 0 && data.play_count > 0)
        || (last_played.is_none() && data.last_played_at.is_some());
    let conflicts = (rating.is_some() && data.rating.is_some() && rating != data.rating)
        || (play_count > 0 && data.play_count > 0 && play_count != data.play_count)
        || (last_played.is_some()
            && data.last_played_at.is_some()
            && last_played != data.last_played_at);
    if filled_in {
        conn.prepare_cached(
            "UPDATE track_user_data SET rating = ?3, play_count = ?4, last_played_at = ?5
             WHERE user_id = ?1 AND track_id = ?2",
        )?
        .execute(params![
            user,
            track,
            rating.or(data.rating),
            if play_count > 0 {
                play_count
            } else {
                data.play_count
            },
            last_played.or(data.last_played_at),
        ])?;
        report.track_data_added += 1;
    }
    if conflicts {
        report.track_data_kept += 1;
    }
    Ok(())
}

fn merge_playlist(
    conn: &Connection,
    user: i64,
    playlist: &ExportPlaylist,
    tracks: &HashMap<String, i64>,
    report: &mut ImportReport,
) -> rusqlite::Result<()> {
    let items: Vec<i64> = playlist
        .tracks
        .iter()
        .filter_map(|id| {
            let track = tracks.get(id).copied();
            if track.is_none() {
                report.unknown_references += 1;
            }
            track
        })
        .collect();
    let complete = items.len() == playlist.tracks.len();

    let target = if playlist.kind == "player" {
        // Each user has one player playlist, whatever its ID. Its contents are kept if
        // it already has any.
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM playlists WHERE user_id = ?1 AND kind = 'player'",
                [user],
                |row| row.get(0),
            )
            .optional()?;
        let id = match existing {
            Some(id) => id,
            None => {
                conn.execute(
                    "INSERT INTO playlists (public_id, user_id, name, kind, created_at, updated_at)
                     VALUES (?1, ?2, ?3, 'player', ?4, ?5)",
                    params![
                        new_public_id(),
                        user,
                        playlist.name,
                        playlist.created_at,
                        playlist.updated_at
                    ],
                )?;
                conn.last_insert_rowid()
            }
        };
        let held: i64 = conn.query_row(
            "SELECT count(*) FROM playlist_items WHERE playlist_id = ?1",
            [id],
            |row| row.get(0),
        )?;
        if held > 0 {
            return Ok(());
        }
        // Dropped entries would shift the saved position onto the wrong track.
        if complete {
            conn.execute(
                "UPDATE playlists SET current_position = ?2, current_ms = ?3 WHERE id = ?1",
                params![id, playlist.current_position, playlist.current_ms],
            )?;
        }
        id
    } else {
        let exists: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM playlists WHERE public_id = ?1)",
            [&playlist.id],
            |row| row.get(0),
        )?;
        if exists {
            report.playlists_kept += 1;
            return Ok(());
        }
        conn.execute(
            "INSERT INTO playlists (public_id, user_id, name, kind, rule, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                playlist.id,
                user,
                playlist.name,
                playlist.kind,
                playlist.rule,
                playlist.created_at,
                playlist.updated_at
            ],
        )?;
        report.playlists_added += 1;
        conn.last_insert_rowid()
    };
    let mut insert = conn.prepare_cached(
        "INSERT INTO playlist_items (playlist_id, position, track_id) VALUES (?1, ?2, ?3)",
    )?;
    for (index, track) in items.iter().enumerate() {
        insert.execute(params![target, (index as i64 + 1) * POSITION_GAP, track])?;
    }
    Ok(())
}

/// Applies an export after its folders have been scanned: one transaction, so it either
/// happens whole or not at all.
fn merge(
    conn: &mut Connection,
    export: &ExportFile,
    roots: &HashMap<String, i64>,
    tracks_before: &HashSet<i64>,
    report: &mut ImportReport,
) -> Result<(), ApiError> {
    let tx = conn.transaction()?;
    let now = now_ms();
    let local: Vec<LocalTrack> = tx
        .prepare("SELECT id, public_id, root_id, rel_path, fingerprint FROM tracks")?
        .query_map([], |row| {
            Ok(LocalTrack {
                id: row.get(0)?,
                public_id: row.get(1)?,
                root: row.get(2)?,
                rel_path: row.get(3)?,
                fingerprint: row.get(4)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    let matched = match_tracks(export, &local, roots, report);
    report.tracks = export.tracks.len() as u64;

    let root_names: HashMap<&str, &str> = export
        .roots
        .iter()
        .map(|root| (root.id.as_str(), root.name.as_str()))
        .collect();
    let mut catalog = Catalog::default();
    let mut track_ids: HashMap<String, i64> = HashMap::with_capacity(export.tracks.len());
    let mut adopted: HashSet<i64> = HashSet::new();
    let empty = FileInfo::default();
    for track in &export.tracks {
        if let Some(index) = matched.get(&track.id) {
            let row = &local[*index];
            track_ids.insert(track.id.clone(), row.id);
            // A track this import just found takes the exported identity, so IDs stay
            // stable from one installation to the next. One that was here before keeps
            // its own: existing data wins.
            if !tracks_before.contains(&row.id) && adopted.insert(row.id) {
                tx.prepare_cached("UPDATE tracks SET public_id = ?2, added_at = ?3 WHERE id = ?1")?
                    .execute(params![row.id, track.id, track.added_at])?;
            }
            continue;
        }
        // No file for this track here. A later scan fills in its tags if one appears.
        let root = roots[&track.root];
        let print = track.fingerprint.as_deref().and_then(from_hex);
        store::apply_file(
            &tx,
            &mut catalog,
            &FileRecord {
                root_id: root,
                rel_path: &track.rel_path,
                size: 0,
                mtime: 0,
                existing: None,
                fingerprint: None,
                info: &empty,
            },
            now,
        )?;
        let id = tx.last_insert_rowid();
        tx.prepare_cached(
            "UPDATE tracks SET public_id = ?2, added_at = ?3, fingerprint = ?4, missing_since = ?5,
                               codec = NULL
             WHERE id = ?1",
        )?
        .execute(params![id, track.id, track.added_at, print, now])?;
        track_ids.insert(track.id.clone(), id);
        report.missing += 1;
        if report.missing_paths.len() < MAX_REPORTED_MISSING {
            let name = root_names.get(track.root.as_str()).copied().unwrap_or("");
            report
                .missing_paths
                .push(format!("{name}/{}", track.rel_path));
        }
    }
    store::refresh_albums(&tx, &catalog.touched_albums)?;

    for user in &export.users {
        tx.execute(
            "INSERT OR IGNORE INTO users (name) VALUES (?1)",
            [&user.name],
        )?;
        let user_id: i64 = tx.query_row(
            "SELECT id FROM users WHERE name = ?1",
            [&user.name],
            |row| row.get(0),
        )?;
        for data in &user.track_data {
            match track_ids.get(&data.track) {
                Some(track) => merge_track_data(&tx, user_id, *track, data, report)?,
                None => report.unknown_references += 1,
            }
        }
        for play in &user.history {
            let Some(track) = track_ids.get(&play.track) else {
                report.unknown_references += 1;
                continue;
            };
            report.history_added +=
                tx.prepare_cached(
                    "INSERT INTO play_history (user_id, track_id, played_at)
                     SELECT ?1, ?2, ?3 WHERE NOT EXISTS (
                         SELECT 1 FROM play_history
                         WHERE user_id = ?1 AND track_id = ?2 AND played_at = ?3)",
                )?
                .execute(params![user_id, track, play.played_at])? as u64;
        }
        for playlist in &user.playlists {
            merge_playlist(&tx, user_id, playlist, &track_ids, report)?;
        }
    }
    for setting in &export.settings {
        report.settings_added += tx
            .prepare_cached("INSERT OR IGNORE INTO settings (key, value) VALUES (?1, ?2)")?
            .execute(params![setting.key, setting.value])? as u64;
    }
    tx.commit()?;
    Ok(())
}

/// Runs a whole import: sets up the folders, scans them, then merges the export.
pub fn import_library(
    db: &Database,
    export: &ExportFile,
    choices: &[CheckedChoice],
    options: ScanOptions,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(ScanProgress),
) -> Result<ImportReport, ApiError> {
    let (roots, to_scan, tracks_before) = db.write(|conn| {
        let tx = conn.transaction()?;
        let (roots, to_scan) = prepare_roots(&tx, export, choices)?;
        let tracks_before: HashSet<i64> = tx
            .prepare("SELECT id FROM tracks")?
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        tx.commit()?;
        Ok::<_, ApiError>((roots, to_scan, tracks_before))
    })?;

    let mut report = ImportReport::default();
    if !to_scan.is_empty() {
        let scanned = scan::scan_library(db, Some(&to_scan), options, cancel, progress)?;
        report.canceled = scanned.canceled;
        report.scan = Some(scanned);
    }
    if report.canceled {
        return Ok(report);
    }
    db.write(|conn| merge(conn, export, &roots, &tracks_before, &mut report))?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::api::add_root;
    use crate::db::{DEFAULT_PLAYER_PLAYLIST_PUBLIC_ID, DEFAULT_USER_ID, DbError};
    use crate::playlists;
    use crate::scan::tests::{mp3_bytes, tagged};

    /// One installation: its own database and music folders.
    struct Install {
        dir: TempDir,
        db: Database,
    }

    fn install() -> Install {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("umak.db")).unwrap();
        Install { dir, db }
    }

    impl Install {
        fn folder(&self, name: &str) -> PathBuf {
            let path = self.dir.path().join(name);
            fs::create_dir_all(&path).unwrap();
            path
        }

        fn mp3(&self, rel: &str, seed: u32, title: &str) {
            let path = self.dir.path().join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, mp3_bytes(seed, &tagged(title, "Artist", "Album"))).unwrap();
        }

        fn add_root(&self, name: &str) -> String {
            let path = self.folder(name);
            add_root(&self.db, path.to_str().unwrap(), None).unwrap().id
        }

        fn scan(&self) -> ScanReport {
            let cancel = AtomicBool::new(false);
            scan::scan_library(&self.db, None, ScanOptions::default(), &cancel, &mut |_| {})
                .unwrap()
        }

        fn id_of(&self, title: &str) -> String {
            self.query(|conn| {
                conn.query_row(
                    "SELECT public_id FROM tracks WHERE title = ?1",
                    [title],
                    |row| row.get(0),
                )
            })
        }

        fn query<T>(&self, work: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> T {
            self.db
                .read(|conn| work(conn).map_err(DbError::from))
                .unwrap()
        }

        fn count(&self, sql: &str) -> i64 {
            self.query(|conn| conn.query_row(sql, [], |row| row.get(0)))
        }

        fn run(&self, sql: &str, args: impl rusqlite::Params) {
            self.db
                .write(|conn| {
                    conn.execute(sql, args)?;
                    Ok::<_, DbError>(())
                })
                .unwrap();
        }

        fn rate(&self, title: &str, rating: i64, plays: i64, last: Option<i64>) {
            self.run(
                "INSERT INTO track_user_data (user_id, track_id, rating, play_count, last_played_at)
                 SELECT ?1, id, ?3, ?4, ?5 FROM tracks WHERE title = ?2",
                params![DEFAULT_USER_ID, title, rating, plays, last],
            );
        }

        fn playlist(&self, name: &str, titles: &[&str]) -> String {
            let ids: Vec<String> = titles.iter().map(|title| self.id_of(title)).collect();
            self.db
                .write(|conn| {
                    let list = playlists::create(conn, name, 5)?;
                    playlists::add_tracks(conn, &list.id, &ids, 6)?;
                    Ok::<_, ApiError>(list.id)
                })
                .unwrap()
        }

        fn playlist_titles(&self, public_id: &str) -> Vec<String> {
            self.query(|conn| {
                conn.prepare(
                    "SELECT t.title FROM playlist_items i
                     JOIN playlists p ON p.id = i.playlist_id
                     JOIN tracks t ON t.id = i.track_id
                     WHERE p.public_id = ?1 ORDER BY i.position",
                )?
                .query_map([public_id], |row| row.get(0))?
                .collect()
            })
        }

        fn export(&self) -> PathBuf {
            let path = self.dir.path().join("export.json");
            export_library(&self.db, &path).unwrap();
            path
        }

        fn import(&self, file: &Path, choices: &[(&str, Option<&Path>)]) -> ImportReport {
            let choices: Vec<RootChoice> = choices
                .iter()
                .map(|(id, path)| RootChoice {
                    id: (*id).to_owned(),
                    path: path.map(|path| path.to_str().unwrap().to_owned()),
                })
                .collect();
            let (export, checked) = check_import(file, &choices).unwrap();
            let cancel = AtomicBool::new(false);
            import_library(
                &self.db,
                &export,
                &checked,
                ScanOptions::default(),
                &cancel,
                &mut |_| {},
            )
            .unwrap()
        }
    }

    /// A source library with a rating, history, playlists and a player list.
    fn source() -> (Install, String) {
        let source = install();
        let root = source.add_root("music");
        source.mp3("music/x/a.mp3", 1, "A");
        source.mp3("music/x/b.mp3", 2, "B");
        source.mp3("music/y/c.mp3", 3, "C");
        source.scan();
        source.rate("A", 9, 4, Some(111));
        source.run(
            "INSERT INTO play_history (user_id, track_id, played_at)
             SELECT 1, id, 111 FROM tracks WHERE title = 'A'",
            [],
        );
        source.run(
            "INSERT INTO settings (key, value) VALUES ('ui.columns', '[1,2]')",
            [],
        );
        (source, root)
    }

    #[test]
    fn a_clean_install_restores_everything_and_keeps_track_ids() {
        let (source, root) = source();
        let list = source.playlist("Road Trip", &["C", "A", "C"]);
        let (a, b) = (source.id_of("A"), source.id_of("B"));
        let added: i64 = source.count("SELECT added_at FROM tracks WHERE title = 'A'");
        let ids = vec![a.clone(), b.clone()];
        source
            .db
            .write(|conn| {
                playlists::set_player_tracks(conn, &ids)?;
                playlists::save_player_position(conn, Some(1), 4200)
            })
            .unwrap();
        let file = source.export();

        let target = install();
        let music = target.folder("copy");
        for (rel, seed, title) in [
            ("x/a.mp3", 1, "A"),
            ("x/b.mp3", 2, "B"),
            ("y/c.mp3", 3, "C"),
        ] {
            target.mp3(&format!("copy/{rel}"), seed, title);
        }
        let report = target.import(&file, &[(&root, Some(&music))]);

        assert_eq!(report.tracks, 3);
        assert_eq!(report.matched_by_path, 3);
        assert_eq!(report.missing, 0);
        assert_eq!(report.playlists_added, 1);
        assert_eq!(report.track_data_added, 1);
        assert_eq!(report.history_added, 1);
        assert_eq!(report.settings_added, 1);
        assert_eq!(report.unknown_references, 0);

        assert_eq!(target.id_of("A"), a, "the track keeps its stable ID");
        assert_eq!(
            target.count("SELECT added_at FROM tracks WHERE title = 'A'"),
            added
        );
        assert_eq!(target.count("SELECT rating FROM track_user_data"), 9);
        assert_eq!(target.count("SELECT play_count FROM track_user_data"), 4);
        assert_eq!(target.count("SELECT played_at FROM play_history"), 111);
        assert_eq!(target.playlist_titles(&list), ["C", "A", "C"]);
        assert_eq!(
            target.playlist_titles(DEFAULT_PLAYER_PLAYLIST_PUBLIC_ID),
            ["A", "B"]
        );
        let position = target.query(playlists_position);
        assert_eq!(position, (Some(1), Some(4200)));
        assert_eq!(
            target.query(|conn| conn.query_row(
                "SELECT value FROM settings WHERE key = 'ui.columns'",
                [],
                |row| row.get::<_, String>(0)
            )),
            "[1,2]"
        );
        // The real tags came from the local files, not the export.
        assert_eq!(
            target.count("SELECT count(*) FROM tracks WHERE missing_since IS NOT NULL"),
            0
        );
        assert_eq!(
            target.count("SELECT count(*) FROM tracks_fts WHERE tracks_fts MATCH 'artist'"),
            3
        );
        // The root keeps its ID but lives at the local path.
        assert_eq!(
            target.query(|conn| conn
                .query_row("SELECT public_id FROM library_roots", [], |row| row
                    .get::<_, String>(0))),
            root
        );
    }

    fn playlists_position(conn: &Connection) -> rusqlite::Result<(Option<i64>, Option<i64>)> {
        conn.query_row(
            "SELECT current_position, current_ms FROM playlists WHERE kind = 'player'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    #[test]
    fn files_in_a_different_layout_match_by_fingerprint_with_tags_edited() {
        let (source, root) = source();
        let list = source.playlist("Mix", &["A", "B"]);
        let a = source.id_of("A");
        let file = source.export();

        let target = install();
        let music = target.folder("elsewhere");
        target.mp3("elsewhere/renamed/first.mp3", 1, "A Retagged");
        target.mp3("elsewhere/x/b.mp3", 2, "B");
        target.mp3("elsewhere/y/c.mp3", 3, "C");
        let report = target.import(&file, &[(&root, Some(&music))]);

        assert_eq!(
            (report.matched_by_path, report.matched_by_fingerprint),
            (2, 1)
        );
        assert_eq!(report.missing, 0);
        assert_eq!(target.id_of("A Retagged"), a);
        assert_eq!(target.count("SELECT rating FROM track_user_data"), 9);
        assert_eq!(target.playlist_titles(&list), ["A Retagged", "B"]);
    }

    #[test]
    fn identical_copies_pair_up_by_file_name() {
        let source = install();
        let root = source.add_root("music");
        source.mp3("music/one/first.mp3", 1, "Song");
        source.mp3("music/one/second.mp3", 1, "Song");
        source.scan();
        source.run(
            "UPDATE tracks SET title = CASE WHEN rel_path LIKE '%first%' THEN 'First' ELSE 'Second' END",
            [],
        );
        let (first, second) = (source.id_of("First"), source.id_of("Second"));
        let file = source.export();

        let target = install();
        let music = target.folder("copy");
        target.mp3("copy/moved/second.mp3", 1, "Song");
        target.mp3("copy/moved/first.mp3", 1, "Song");
        let report = target.import(&file, &[(&root, Some(&music))]);
        assert_eq!(report.matched_by_fingerprint, 2);
        let id_at = |name: &str| {
            target.query(|conn| {
                conn.query_row(
                    "SELECT public_id FROM tracks WHERE rel_path = ?1",
                    [name],
                    |row| row.get::<_, String>(0),
                )
            })
        };
        assert_eq!(id_at("moved/first.mp3"), first);
        assert_eq!(id_at("moved/second.mp3"), second);
    }

    #[test]
    fn a_track_with_no_file_is_kept_missing_and_comes_back_when_the_file_does() {
        let (source, root) = source();
        let list = source.playlist("Mix", &["A", "B"]);
        let a = source.id_of("A");
        let file = source.export();

        let target = install();
        let music = target.folder("copy");
        target.mp3("copy/x/b.mp3", 2, "B");
        target.mp3("copy/y/c.mp3", 3, "C");
        let report = target.import(&file, &[(&root, Some(&music))]);

        assert_eq!(report.missing, 1);
        assert_eq!(report.missing_paths, ["music/x/a.mp3"]);
        let (title, missing, public_id, has_print): (String, bool, String, bool) =
            target.query(|conn| {
                conn.query_row(
                    "SELECT title, missing_since IS NOT NULL, public_id, fingerprint IS NOT NULL
                     FROM tracks WHERE rel_path = 'x/a.mp3'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
            });
        assert_eq!(
            title, "a",
            "named after the file until its tags can be read"
        );
        assert!(missing);
        assert_eq!(public_id, a);
        assert!(has_print);
        assert_eq!(target.count("SELECT rating FROM track_user_data"), 9);
        assert_eq!(target.playlist_titles(&list), ["a", "B"]);

        // The file arrives somewhere else entirely: the fingerprint finds its row.
        target.mp3("copy/found/a-again.mp3", 1, "A");
        let scanned = target.scan();
        assert_eq!(scanned.moved, 1);
        assert_eq!(target.id_of("A"), a);
        assert_eq!(
            target.count("SELECT count(*) FROM tracks WHERE missing_since IS NOT NULL"),
            0
        );
        assert_eq!(target.playlist_titles(&list), ["A", "B"]);
    }

    #[test]
    fn a_folder_not_on_this_machine_keeps_its_tracks_missing_until_chosen() {
        let (source, root) = source();
        let list = source.playlist("Mix", &["A"]);
        let file = source.export();

        let target = install();
        let report = target.import(&file, &[(&root, None)]);
        assert_eq!(report.missing, 3);
        assert!(report.scan.is_none());
        assert_eq!(target.count("SELECT rating FROM track_user_data"), 9);
        assert_eq!(target.playlist_titles(&list), ["a"]);

        // Importing again with the folder chosen finds the files, with no duplicates.
        let music = target.folder("copy");
        target.mp3("copy/x/a.mp3", 1, "A");
        target.mp3("copy/x/b.mp3", 2, "B");
        target.mp3("copy/y/c.mp3", 3, "C");
        let again = target.import(&file, &[(&root, Some(&music))]);
        assert_eq!(again.missing, 0);
        assert_eq!(again.matched_by_id, 3);
        assert_eq!(target.count("SELECT count(*) FROM tracks"), 3);
        assert_eq!(
            target.count("SELECT count(*) FROM tracks WHERE missing_since IS NOT NULL"),
            0
        );
        assert_eq!(target.playlist_titles(&list), ["A"]);
        assert_eq!(target.count("SELECT rating FROM track_user_data"), 9);
    }

    #[test]
    fn existing_data_is_kept_on_a_conflict_and_gaps_are_filled() {
        let (source, root) = source();
        source.rate("B", 7, 2, Some(50));
        source.run(
            "INSERT INTO settings (key, value) VALUES ('new.key', 'imported')",
            [],
        );
        source.run(
            "UPDATE settings SET value = 'imported' WHERE key = 'ui.columns'",
            [],
        );
        let list = source.playlist("Shared", &["A"]);
        let only_there = source.playlist("Only There", &["B"]);
        let file = source.export();

        // The same library on a second machine, already with its own opinions.
        let target = install();
        let music = target.folder("copy");
        target.mp3("copy/x/a.mp3", 1, "A");
        target.mp3("copy/x/b.mp3", 2, "B");
        target.mp3("copy/y/c.mp3", 3, "C");
        add_root(&target.db, music.to_str().unwrap(), None).unwrap();
        target.scan();
        target.run(
            "INSERT INTO track_user_data (user_id, track_id, rating, play_count, last_played_at)
             SELECT 1, id, 3, 0, NULL FROM tracks WHERE title = 'A'",
            [],
        );
        target.run(
            "INSERT INTO track_user_data (user_id, track_id, rating, play_count)
             SELECT 1, id, NULL, 5 FROM tracks WHERE title = 'B'",
            [],
        );
        target.run(
            "INSERT INTO settings (key, value) VALUES ('ui.columns', 'mine')",
            [],
        );
        let a_before = target.id_of("A");
        let added_before = target.count("SELECT added_at FROM tracks WHERE title = 'A'");
        // Its own copy of the shared playlist, with different contents.
        target.run(
            "INSERT INTO playlists (public_id, user_id, name, kind, created_at, updated_at)
             VALUES (?1, 1, 'My Version', 'manual', 0, 0)",
            [&list],
        );

        let report = target.import(&file, &[(&root, Some(&music))]);

        let row = |title: &str| -> (Option<i64>, i64, Option<i64>) {
            target.query(|conn| {
                conn.query_row(
                    "SELECT d.rating, d.play_count, d.last_played_at FROM track_user_data d
                     JOIN tracks t ON t.id = d.track_id WHERE t.title = ?1",
                    [title],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
            })
        };
        // A: the local rating wins; the local blanks take the imported values.
        assert_eq!(row("A"), (Some(3), 4, Some(111)));
        // B: the local play count wins; the local blank rating takes the imported one.
        assert_eq!(row("B"), (Some(7), 5, Some(50)));
        assert_eq!(report.track_data_kept, 2);

        assert_eq!(report.playlists_kept, 1);
        assert_eq!(report.playlists_added, 1);
        assert!(
            target.playlist_titles(&list).is_empty(),
            "the local playlist is untouched"
        );
        assert_eq!(target.playlist_titles(&only_there), ["B"]);

        let setting = |key: &str| -> String {
            target.query(|conn| {
                conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                    row.get(0)
                })
            })
        };
        assert_eq!(setting("ui.columns"), "mine");
        assert_eq!(setting("new.key"), "imported");

        // Tracks already here keep their own identity.
        assert_eq!(target.id_of("A"), a_before);
        assert_eq!(
            target.count("SELECT added_at FROM tracks WHERE title = 'A'"),
            added_before
        );
        // The folder that was already here stays one folder.
        assert_eq!(target.count("SELECT count(*) FROM library_roots"), 1);
    }

    #[test]
    fn a_player_list_already_in_use_is_kept() {
        let (source, root) = source();
        let ids = vec![source.id_of("A")];
        source
            .db
            .write(|conn| playlists::set_player_tracks(conn, &ids))
            .unwrap();
        let file = source.export();

        let target = install();
        let music = target.folder("copy");
        target.mp3("copy/x/a.mp3", 1, "A");
        target.mp3("copy/x/b.mp3", 2, "B");
        add_root(&target.db, music.to_str().unwrap(), None).unwrap();
        target.scan();
        let mine = vec![target.id_of("B")];
        target
            .db
            .write(|conn| playlists::set_player_tracks(conn, &mine))
            .unwrap();

        target.import(&file, &[(&root, Some(&music))]);
        assert_eq!(
            target.playlist_titles(DEFAULT_PLAYER_PLAYLIST_PUBLIC_ID),
            ["B"]
        );
    }

    #[test]
    fn ratings_plays_and_smart_playlists_survive_an_export_and_import() {
        use crate::smart::{SmartRule, SmartSort};
        let (source, root) = source();
        let rule = SmartRule {
            match_all: true,
            conditions: Vec::new(),
            sort: SmartSort::Plays,
            descending: true,
            limit: Some(1),
        };
        let (a, b) = (source.id_of("A"), source.id_of("B"));
        source
            .db
            .write(|conn| {
                crate::user_data::set_rating(conn, std::slice::from_ref(&a), Some(7))?;
                crate::user_data::record_play(conn, &b, 100)?;
                crate::user_data::record_play(conn, &b, 200)?;
                crate::playlists::create_smart(conn, "Most played", &rule, 1)?;
                Ok::<_, ApiError>(())
            })
            .unwrap();
        let file = source.export();

        let target = install();
        let music = target.folder("copy");
        target.mp3("copy/x/a.mp3", 1, "A");
        target.mp3("copy/x/b.mp3", 2, "B");
        target.import(&file, &[(&root, Some(&music))]);

        assert_eq!(
            target.query(|conn| conn.query_row(
                "SELECT d.rating FROM track_user_data d JOIN tracks t ON t.id = d.track_id
                 WHERE t.public_id = ?1",
                [&a],
                |row| row.get::<_, Option<i64>>(0)
            )),
            Some(7)
        );
        assert_eq!(
            target.query(|conn| conn.query_row(
                "SELECT d.play_count, d.last_played_at FROM track_user_data d
                 JOIN tracks t ON t.id = d.track_id WHERE t.public_id = ?1",
                [&b],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            )),
            (2, 200)
        );
        assert_eq!(
            target.count("SELECT count(*) FROM play_history"),
            source.count("SELECT count(*) FROM play_history")
        );

        let saved = target
            .db
            .read(crate::playlists::list)
            .unwrap()
            .into_iter()
            .find(|playlist| playlist.smart)
            .unwrap();
        assert_eq!(saved.name, "Most played");
        assert_eq!(saved.rule.as_ref(), Some(&rule));
        let page = target
            .db
            .read(|conn| crate::playlists::items(conn, &saved.id, 0, 10))
            .unwrap();
        assert_eq!(page.items.len(), 1);
    }

    #[test]
    fn importing_the_same_file_twice_changes_nothing_more() {
        let (source, root) = source();
        let list = source.playlist("Mix", &["A", "B"]);
        let file = source.export();

        let target = install();
        let music = target.folder("copy");
        target.mp3("copy/x/a.mp3", 1, "A");
        target.mp3("copy/x/b.mp3", 2, "B");
        target.import(&file, &[(&root, Some(&music))]);

        let snapshot = |install: &Install| -> Vec<i64> {
            [
                "SELECT count(*) FROM tracks",
                "SELECT count(*) FROM track_user_data",
                "SELECT count(*) FROM play_history",
                "SELECT count(*) FROM playlists",
                "SELECT count(*) FROM playlist_items",
                "SELECT count(*) FROM library_roots",
                "SELECT count(*) FROM settings",
                "SELECT count(*) FROM tracks WHERE missing_since IS NOT NULL",
            ]
            .iter()
            .map(|sql| install.count(sql))
            .collect()
        };
        let first = snapshot(&target);
        let report = target.import(&file, &[(&root, Some(&music))]);
        assert_eq!(snapshot(&target), first);
        assert_eq!(report.playlists_added, 0);
        assert_eq!(report.history_added, 0);
        assert_eq!(report.track_data_added, 0);
        assert_eq!(
            report.track_data_kept, 0,
            "matching values are not a conflict"
        );
        assert_eq!(
            report.missing, 0,
            "the placeholder from the first import counts as found"
        );
        assert_eq!(target.playlist_titles(&list).len(), 2);
    }

    #[test]
    fn export_then_import_then_export_keeps_the_same_identity() {
        let (source, root) = source();
        source.playlist("Mix", &["A"]);
        let first = read_export(&source.export()).unwrap();

        let target = install();
        let music = target.folder("copy");
        target.mp3("copy/x/a.mp3", 1, "A");
        target.mp3("copy/x/b.mp3", 2, "B");
        target.mp3("copy/y/c.mp3", 3, "C");
        target.import(&source.export(), &[(&root, Some(&music))]);
        let second = read_export(&target.export()).unwrap();

        assert_eq!(first.tracks, second.tracks);
        assert_eq!(first.users, second.users);
        assert_eq!(first.settings, second.settings);
        assert_eq!(first.roots[0].id, second.roots[0].id);
    }

    #[test]
    fn a_canceled_import_changes_no_user_data_and_can_run_again() {
        let (source, root) = source();
        source.playlist("Mix", &["A"]);
        let file = source.export();

        let target = install();
        let music = target.folder("copy");
        target.mp3("copy/x/a.mp3", 1, "A");
        let choices = [RootChoice {
            id: root.clone(),
            path: Some(music.to_str().unwrap().to_owned()),
        }];
        let (export, checked) = check_import(&file, &choices).unwrap();
        let cancel = AtomicBool::new(true);
        let report = import_library(
            &target.db,
            &export,
            &checked,
            ScanOptions::default(),
            &cancel,
            &mut |_| {},
        )
        .unwrap();
        assert!(report.canceled);
        assert_eq!(target.count("SELECT count(*) FROM track_user_data"), 0);
        assert_eq!(
            target.count("SELECT count(*) FROM playlists WHERE kind = 'manual'"),
            0
        );

        let report = target.import(&file, &[(&root, Some(&music))]);
        assert!(!report.canceled);
        assert_eq!(report.playlists_added, 1);
    }

    #[test]
    fn a_track_not_listed_in_the_export_is_counted_and_skipped() {
        let (source, root) = source();
        let path = source.export();
        let mut export = read_export(&path).unwrap();
        export.users[0].playlists.push(ExportPlaylist {
            id: "extra".to_owned(),
            name: "Extra".to_owned(),
            kind: "manual".to_owned(),
            rule: None,
            current_position: None,
            current_ms: None,
            created_at: 0,
            updated_at: 0,
            tracks: vec!["no-such-track".to_owned()],
        });
        fs::write(&path, serde_json::to_vec(&export).unwrap()).unwrap();

        let target = install();
        let report = target.import(&path, &[(&root, None)]);
        assert_eq!(report.unknown_references, 1);
        assert_eq!(report.playlists_added, 1);
    }

    #[test]
    fn the_export_holds_identity_and_user_data_but_no_tags() {
        let (source, _) = source();
        let path = source.dir.path().join("out.json");
        let summary = export_library(&source.db, &path).unwrap();
        assert_eq!(summary.tracks, 3);
        assert_eq!(summary.rated_tracks, 1);
        assert_eq!(
            summary.playlists, 0,
            "the player list is not counted as a saved playlist"
        );
        assert!(summary.bytes > 0);

        let text = fs::read_to_string(&path).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["format"], FORMAT);
        assert_eq!(value["version"], CURRENT_VERSION);
        let tracks = value["tracks"].as_array().unwrap();
        let a = tracks
            .iter()
            .find(|track| track["relPath"] == "x/a.mp3")
            .unwrap();
        assert_eq!(a["fingerprint"].as_str().unwrap().len(), 34);
        assert!(!text.contains("Artist"), "tags must not be exported");
        assert!(!text.contains("\"Album\""), "tags must not be exported");
    }

    #[test]
    fn a_failed_export_leaves_an_earlier_export_alone() {
        let (source, _) = source();
        let path = source.dir.path().join("keep.json");
        fs::write(&path, b"earlier export").unwrap();
        let bad = source.dir.path().join("no-such-folder").join("out.json");
        assert_eq!(
            export_library(&source.db, &bad).unwrap_err().code,
            ErrorCode::InvalidArgs
        );
        assert_eq!(fs::read(&path).unwrap(), b"earlier export");

        export_library(&source.db, &path).unwrap();
        assert!(read_export(&path).is_ok());
        let names: Vec<_> = fs::read_dir(source.dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            !names.iter().any(|name| name.ends_with(".tmp")),
            "{names:?}"
        );
    }

    fn write_json(dir: &TempDir, value: &Value) -> PathBuf {
        let path = dir.path().join("in.json");
        fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        path
    }

    fn base_export() -> Value {
        json!({
            "format": FORMAT,
            "version": 1,
            "exportedAt": 1,
            "appVersion": "0.1.0",
            "roots": [{ "id": "r", "name": "Music", "path": "M:/music", "addedAt": 1 }],
            "tracks": [{
                "id": "t", "root": "r", "relPath": "a/b.mp3", "fingerprint": "01ff", "addedAt": 1
            }],
            "users": [{
                "name": "default",
                "trackData": [{ "track": "t", "rating": 5, "playCount": 0, "lastPlayedAt": null }],
                "history": [],
                "playlists": []
            }],
            "settings": []
        })
    }

    #[test]
    fn files_that_are_not_valid_exports_are_refused_clearly() {
        let dir = tempfile::tempdir().unwrap();
        let cases: Vec<(Value, &str)> = vec![
            (json!({ "hello": "world" }), "not a Umak library export"),
            (
                json!({ "format": "other", "version": 1 }),
                "not a Umak library export",
            ),
            (json!({ "format": FORMAT, "version": 2 }), "Update the app"),
            (
                json!({ "format": FORMAT, "version": 0 }),
                "unknown export version",
            ),
            (json!({ "format": FORMAT, "version": 1 }), "damaged"),
        ];
        for (value, expected) in cases {
            let error = read_export(&write_json(&dir, &value)).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidArgs);
            assert!(error.message.contains(expected), "{}", error.message);
        }
        let text = dir.path().join("text.json");
        fs::write(&text, b"this is not json").unwrap();
        assert!(
            read_export(&text)
                .unwrap_err()
                .message
                .contains("not a Umak library export")
        );
        assert!(read_export(&dir.path().join("none.json")).is_err());
    }

    #[test]
    fn an_export_that_would_escape_a_root_or_break_a_rule_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        for bad_path in [
            "../secret.mp3",
            "a/../../b.mp3",
            "/etc/x.mp3",
            "C:/x.mp3",
            "a\\b.mp3",
            "",
        ] {
            let mut value = base_export();
            value["tracks"][0]["relPath"] = json!(bad_path);
            let error = read_export(&write_json(&dir, &value)).unwrap_err();
            assert!(
                error.message.contains("bad path"),
                "{bad_path}: {}",
                error.message
            );
        }
        let mut value = base_export();
        value["users"][0]["trackData"][0]["rating"] = json!(11);
        assert!(
            read_export(&write_json(&dir, &value))
                .unwrap_err()
                .message
                .contains("bad rating")
        );

        let mut value = base_export();
        value["tracks"][0]["fingerprint"] = json!("zz");
        assert!(read_export(&write_json(&dir, &value)).is_err());

        let mut value = base_export();
        value["tracks"][0]["root"] = json!("nope");
        assert!(read_export(&write_json(&dir, &value)).is_err());

        let mut value = base_export();
        value["users"][0]["playlists"] = json!([{
            "id": "p", "name": "P", "kind": "smart", "rule": null, "currentPosition": null,
            "currentMs": null, "createdAt": 0, "updatedAt": 0, "tracks": []
        }]);
        assert!(read_export(&write_json(&dir, &value)).is_err());
    }

    #[test]
    fn a_file_written_by_this_version_stays_readable() {
        // Do not regenerate this fixture. New versions must keep reading it.
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/export-v1.json");
        let export = read_export(&fixture).unwrap();
        assert_eq!(export.version, 1);
        assert_eq!(export.roots[0].name, "Music");
        assert_eq!(export.tracks.len(), 2);
        assert_eq!(export.users[0].track_data[0].rating, Some(8));
        assert_eq!(export.users[0].playlists[0].tracks.len(), 2);

        let target = install();
        let report = target.import(&fixture, &[(export.roots[0].id.as_str(), None)]);
        assert_eq!(report.missing, 2);
        assert_eq!(report.playlists_added, 1);
        assert_eq!(target.count("SELECT rating FROM track_user_data"), 8);
    }

    #[test]
    fn the_preview_describes_the_export_and_suggests_folders() {
        let (source, root) = source();
        source.playlist("Mix", &["A"]);
        let file = source.export();

        // Here the folder exists at the same path, as when restoring on one machine.
        let preview_here = preview(&source.db, &file).unwrap();
        assert_eq!(preview_here.track_count, 3);
        assert_eq!(preview_here.playlist_count, 1);
        assert_eq!(preview_here.rated_track_count, 1);
        assert_eq!(preview_here.roots[0].id, root);
        assert_eq!(preview_here.roots[0].track_count, 3);
        assert_eq!(
            preview_here.roots[0].suggested_path.as_ref(),
            Some(&preview_here.roots[0].path)
        );

        // Elsewhere, a folder that does not exist gets no suggestion.
        let export = read_export(&file).unwrap();
        let mut moved = export.clone();
        moved.roots[0].path = "no/such/folder".to_owned();
        let moved_file = source.dir.path().join("moved.json");
        fs::write(&moved_file, serde_json::to_vec(&moved).unwrap()).unwrap();
        let target = install();
        let elsewhere = preview(&target.db, &moved_file).unwrap();
        assert_eq!(elsewhere.roots[0].suggested_path, None);
    }

    #[test]
    fn choosing_an_unknown_or_bad_folder_is_refused_before_anything_changes() {
        let (source, root) = source();
        let file = source.export();
        let target = install();
        let unknown = [RootChoice {
            id: "nope".to_owned(),
            path: None,
        }];
        assert!(check_import(&file, &unknown).is_err());
        let bad = [RootChoice {
            id: root,
            path: Some("no/such/folder".to_owned()),
        }];
        assert!(check_import(&file, &bad).is_err());
        assert_eq!(target.count("SELECT count(*) FROM library_roots"), 0);
    }

    #[test]
    fn a_chosen_folder_overlapping_another_root_is_refused() {
        let (source, root) = source();
        let file = source.export();
        let target = install();
        let outer = target.folder("outer");
        add_root(&target.db, outer.to_str().unwrap(), None).unwrap();
        let inner = target.folder("outer/inner");
        let choices = [RootChoice {
            id: root,
            path: Some(inner.to_str().unwrap().to_owned()),
        }];
        let (export, checked) = check_import(&file, &choices).unwrap();
        let cancel = AtomicBool::new(false);
        let error = import_library(
            &target.db,
            &export,
            &checked,
            ScanOptions::default(),
            &cancel,
            &mut |_| {},
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Conflict);
        assert_eq!(target.count("SELECT count(*) FROM library_roots"), 1);
    }
}
