//! Lists the audio files under a folder. Metadata comes from the directory listing
//! where the OS provides it, so an unchanged library costs no per-file disk reads.

use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

/// Formats Lofty can read. Playlist files are deliberately not imported.
const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "m4a", "m4b", "aac", "ogg", "oga", "opus", "spx", "wav", "aif", "aiff", "ape",
    "wv", "mpc",
];
/// Service folders created by Windows and NAS systems.
const SKIPPED_FOLDERS: &[&str] = &[
    "$RECYCLE.BIN",
    "System Volume Information",
    "@eaDir",
    "#recycle",
];
/// macOS stores file metadata in a sibling named `._<file>`, with the same extension.
const APPLE_DOUBLE_PREFIX: &str = "._";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskFile {
    /// Relative to the walked root, with forward slashes.
    pub rel_path: String,
    pub size: i64,
    pub mtime: i64,
}

#[derive(Debug, Default)]
pub struct Walk {
    pub files: Vec<DiskFile>,
    /// Folders that could not be listed, so the tracks under them are of unknown state.
    pub unreadable: Vec<String>,
    /// Paths skipped because they are not valid Unicode.
    pub invalid_names: Vec<String>,
}

pub fn is_audio_file(name: &str) -> bool {
    if name.starts_with(APPLE_DOUBLE_PREFIX) {
        return false;
    }
    let Some((_, extension)) = name.rsplit_once('.') else {
        return false;
    };
    AUDIO_EXTENSIONS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(extension))
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

pub fn modified_ms(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

/// Walks `start`, a path relative to `root`. Symbolic links and junctions are not
/// followed, so a link cannot index files twice or form a loop.
pub fn walk(root: &Path, start: &str) -> Walk {
    let mut result = Walk::default();
    let mut pending = vec![start.to_owned()];
    while let Some(folder) = pending.pop() {
        let entries = match fs::read_dir(root.join(&folder)) {
            Ok(entries) => entries,
            Err(_) => {
                result.unreadable.push(folder);
                continue;
            }
        };
        let mut subfolders = Vec::new();
        for entry in entries {
            let Ok(entry) = entry else {
                result.unreadable.push(folder.clone());
                break;
            };
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                result
                    .invalid_names
                    .push(join(&folder, &name.to_string_lossy()));
                continue;
            };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if !SKIPPED_FOLDERS.contains(&name) {
                    subfolders.push(join(&folder, name));
                }
            } else if file_type.is_file() && is_audio_file(name) {
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                result.files.push(DiskFile {
                    rel_path: join(&folder, name),
                    size: metadata.len() as i64,
                    mtime: modified_ms(&metadata),
                });
            }
        }
        // Reversed so the stack visits folders in listing order, which keeps related
        // files close together on disk.
        pending.extend(subfolders.into_iter().rev());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"12345").unwrap();
    }

    #[test]
    fn only_audio_files_are_listed_with_relative_forward_slash_paths() {
        let dir = tempfile::tempdir().unwrap();
        for rel in [
            "Artist/Album/01.MP3",
            "Artist/Album/02.flac",
            "Artist/Album/CD1/03.m4a",
            "top.opus",
            "Artist/Album/cover.jpg",
            "Artist/Album/album.m3u",
            "Artist/Album/album.m3u8",
            "Artist/Album/album.cue",
            "Artist/Album/._01.mp3",
            "Artist/Album/noextension",
            "@eaDir/thumb.mp3",
            "$RECYCLE.BIN/old.mp3",
        ] {
            write(dir.path(), rel);
        }
        let mut paths: Vec<_> = walk(dir.path(), "")
            .files
            .into_iter()
            .map(|file| file.rel_path)
            .collect();
        paths.sort();
        assert_eq!(
            paths,
            [
                "Artist/Album/01.MP3",
                "Artist/Album/02.flac",
                "Artist/Album/CD1/03.m4a",
                "top.opus"
            ]
        );
    }

    #[test]
    fn size_and_modified_time_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.mp3");
        let result = walk(dir.path(), "");
        assert_eq!(result.files[0].size, 5);
        assert!(result.files[0].mtime > 0);
    }

    #[test]
    fn a_missing_start_folder_is_reported_as_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let result = walk(dir.path(), "gone");
        assert!(result.files.is_empty());
        assert_eq!(result.unreadable, ["gone"]);
    }

    #[test]
    fn a_walk_can_start_at_a_subfolder() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "A/1.mp3");
        write(dir.path(), "B/2.mp3");
        let result = walk(dir.path(), "B");
        assert_eq!(result.files.len(), 1);
        assert_eq!(result.files[0].rel_path, "B/2.mp3");
    }
}
