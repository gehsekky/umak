//! Writes tags to a corpus of real files, to find what small fixtures cannot. This changes
//! the files, so it runs only on a folder of copies that holds the marker file below.
//!
//! `UMAK_TAG_CORPUS=<folder> cargo test --release -p umak-core --test tag_corpus -- --ignored --nocapture`

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use umak_core::api::{Core, CoreEvent, EventSink, add_root, dispatch};
use umak_core::db::Database;
use umak_core::scan::{ScanOptions, scan_library};
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

struct ChannelSink(Mutex<Sender<CoreEvent>>);

impl EventSink for ChannelSink {
    fn emit(&self, event: &CoreEvent) {
        let _ = self.0.lock().unwrap().send(event.clone());
    }
}

fn wait_for_job(events: &Receiver<CoreEvent>) {
    while !matches!(
        events.recv_timeout(Duration::from_secs(1800)).unwrap(),
        CoreEvent::JobFinished { .. }
    ) {}
}

/// Runs a preview and a write through the commands, and reports how they went.
fn preview_and_write(
    core: &Core,
    events: &Receiver<CoreEvent>,
    label: &str,
    start: &str,
    args: Value,
) -> String {
    let started = Instant::now();
    let edit = dispatch(core, start, args).unwrap()["editId"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_for_job(events);
    let preview_seconds = started.elapsed().as_secs_f64();
    let started = Instant::now();
    dispatch(core, "tags.edits.write", json!({"id": edit})).unwrap();
    wait_for_job(events);
    let info = dispatch(core, "tags.edits.get", json!({"id": edit})).unwrap();
    println!(
        "{label}: preview {preview_seconds:.1} s, write {:.1} s, {} written, {} blocked, {} failed, {} unchanged",
        started.elapsed().as_secs_f64(),
        info["written"],
        info["blocked"],
        info["failed"],
        info["unchanged"],
    );
    let items = dispatch(
        core,
        "tags.edits.items",
        json!({"id": edit, "offset": 0, "limit": 1000}),
    )
    .unwrap();
    for item in items["items"].as_array().unwrap() {
        if item["status"] != "written" {
            println!(
                "{label} {} {}: {}",
                item["status"], item["relPath"], item["message"]
            );
        }
    }
    edit
}

/// Edits every field through the same commands the app uses, then undoes it, so that the
/// result can be compared with the originals.
#[test]
#[ignore = "changes files; needs a folder of copies"]
fn report_on_editing_and_undoing_a_corpus_of_real_files() {
    let folder = PathBuf::from(std::env::var("UMAK_TAG_CORPUS").expect("set UMAK_TAG_CORPUS"));
    assert!(
        folder.join(MARKER).exists(),
        "refusing to write to a folder without {MARKER}"
    );
    let data = tempfile::tempdir().unwrap();
    let db = Database::open(data.path().join("umak.db")).unwrap();
    add_root(&db, folder.to_str().unwrap(), None).unwrap();
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

    let page = dispatch(
        &core,
        "library.tracks.page",
        json!({"query": null, "sort": "title", "descending": false, "offset": 0, "limit": 1000}),
    )
    .unwrap();
    let ids: Vec<Value> = page["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|track| track["id"].clone())
        .collect();
    println!("{} tracks", ids.len());

    let edit = preview_and_write(
        &core,
        &events,
        "edit",
        "tags.preview.start",
        json!({"changes": [{"trackIds": ids, "edits": [
            {"field": "title", "values": ["Écrit par Umak 音楽"]},
            {"field": "album", "values": ["Test Album"]},
            {"field": "artists", "values": ["First Artist", "Second Artist"]},
            {"field": "albumArtists", "values": ["Test Band"]},
            {"field": "composers", "values": ["Test Writer"]},
            {"field": "genres", "values": ["Test Genre"]},
            {"field": "year", "values": ["2004"]},
            {"field": "trackNumber", "values": ["77"]},
            {"field": "discNumber", "values": ["9"]},
        ]}]}),
    );
    preview_and_write(
        &core,
        &events,
        "undo",
        "tags.edits.undo",
        json!({"id": edit}),
    );
}
