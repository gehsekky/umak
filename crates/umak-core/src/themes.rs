//! The user's own themes: JSON files in a folder beside the database. The core only reads
//! them; the UI knows the theme tokens, so it checks each file.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use ts_rs::TS;

use crate::db::Database;

/// A theme file is a short list of tokens. Anything much larger is not one.
const MAX_THEME_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct UserThemes {
    /// Where to put theme files.
    pub folder: String,
    /// In order of file name.
    pub files: Vec<ThemeFile>,
    /// Why the folder could not be read, if it could not.
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct ThemeFile {
    /// The file's name, which names the theme to the settings.
    pub file: String,
    pub text: Option<String>,
    /// Why the file could not be read, if it could not.
    pub error: Option<String>,
}

pub fn folder(db: &Database) -> PathBuf {
    db.path()
        .parent()
        .map_or_else(|| PathBuf::from("themes"), |folder| folder.join("themes"))
}

/// Reads every `.json` file in the folder, making the folder if it is missing so it is there to
/// be found.
pub fn read_folder(folder: &Path) -> UserThemes {
    let mut themes = UserThemes {
        folder: folder.to_string_lossy().into_owned(),
        files: Vec::new(),
        error: None,
    };
    let entries = fs::create_dir_all(folder).and_then(|()| fs::read_dir(folder));
    let entries = match entries {
        Ok(entries) => entries,
        Err(error) => {
            themes.error = Some(error.to_string());
            return themes;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_json = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
        if !is_json || !path.is_file() {
            continue;
        }
        let file = entry.file_name().to_string_lossy().into_owned();
        let read = match entry.metadata() {
            Ok(metadata) if metadata.len() > MAX_THEME_BYTES => Err(format!(
                "is {} KB, too large for a theme",
                metadata.len() / 1024
            )),
            _ => fs::read_to_string(&path).map_err(|error| error.to_string()),
        };
        let (text, error) = match read {
            Ok(text) => (Some(text), None),
            Err(error) => (None, Some(error)),
        };
        themes.files.push(ThemeFile { file, text, error });
    }
    themes.files.sort_by_key(|theme| theme.file.to_lowercase());
    themes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_files_are_read_in_name_order_and_the_folder_is_made() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("themes");
        assert_eq!(read_folder(&folder).files, []);
        assert!(folder.is_dir());

        fs::write(folder.join("b.JSON"), "{}").unwrap();
        fs::write(folder.join("a.json"), "{\"tokens\": {}}").unwrap();
        fs::write(folder.join("notes.txt"), "not a theme").unwrap();
        fs::write(
            folder.join("huge.json"),
            vec![b' '; MAX_THEME_BYTES as usize + 1],
        )
        .unwrap();
        let themes = read_folder(&folder);
        assert_eq!(themes.error, None);
        let names: Vec<&str> = themes.files.iter().map(|file| file.file.as_str()).collect();
        assert_eq!(names, ["a.json", "b.JSON", "huge.json"]);
        assert_eq!(themes.files[0].text.as_deref(), Some("{\"tokens\": {}}"));
        assert_eq!(themes.files[2].text, None);
        assert_eq!(
            themes.files[2].error.as_deref(),
            Some("is 256 KB, too large for a theme")
        );
    }
}
