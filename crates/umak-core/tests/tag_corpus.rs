//! Writes tags to a corpus of real files, to find what small fixtures cannot. This changes
//! the files, so it runs only on a folder of copies that holds the marker file below.
//!
//! `UMAK_TAG_CORPUS=<folder> cargo test --release -p umak-core --test tag_corpus -- --ignored --nocapture`

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use umak_core::tagwrite::{FieldEdit, TagField, write_tags};

const MARKER: &str = ".umak-tag-corpus";

fn audio_files(folder: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(folder).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            audio_files(&path, found);
        } else if path.extension().is_some_and(|extension| {
            let extension = extension.to_string_lossy().to_lowercase();
            ["mp3", "flac", "m4a"].contains(&extension.as_str())
        }) {
            found.push(path);
        }
    }
}

fn edit(field: TagField, values: &[&str]) -> FieldEdit {
    FieldEdit {
        field,
        values: values.iter().map(|value| (*value).to_owned()).collect(),
    }
}

#[test]
#[ignore = "changes files; needs a folder of copies"]
fn report_on_writing_a_corpus_of_real_files() {
    let folder = PathBuf::from(std::env::var("UMAK_TAG_CORPUS").expect("set UMAK_TAG_CORPUS"));
    assert!(
        folder.join(MARKER).exists(),
        "refusing to write to a folder without {MARKER}"
    );
    let mut files = Vec::new();
    audio_files(&folder, &mut files);

    let edits = vec![
        edit(TagField::Title, &["Écrit par Umak 音楽"]),
        edit(TagField::Album, &["Test Album"]),
        edit(TagField::Artists, &["First Artist", "Second Artist"]),
        edit(TagField::AlbumArtists, &["Test Band"]),
        edit(TagField::Composers, &["Test Writer"]),
        edit(TagField::Genres, &["Test Genre"]),
        edit(TagField::Year, &["2004"]),
        edit(TagField::TrackNumber, &["7"]),
        edit(TagField::DiscNumber, &["2"]),
    ];

    let mut results: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    let mut errors: BTreeMap<String, u32> = BTreeMap::new();
    let started = Instant::now();
    for path in &files {
        let extension = path.extension().unwrap().to_string_lossy().to_lowercase();
        let tally = results.entry(extension).or_default();
        match write_tags(path, &edits) {
            Ok(()) => tally.0 += 1,
            Err(error) => {
                tally.1 += 1;
                let text = error.to_string();
                let start: String = text.chars().take(400).collect();
                println!("FAILED {}: {start}", path.display());
                *errors.entry(text.chars().take(60).collect()).or_default() += 1;
            }
        }
    }
    println!(
        "{} files in {:.1} s",
        files.len(),
        started.elapsed().as_secs_f64()
    );
    for (extension, (good, bad)) in &results {
        println!("{extension}: {good} written, {bad} refused");
    }
    for (message, count) in &errors {
        println!("{count} x {message}");
    }
}
