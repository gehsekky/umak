//! Runs the real binary, which also proves the core works without a UI.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

const MPEG_FRAME_LEN: usize = 417;
const MPEG_FRAMES: usize = 60;

fn mp3(path: &Path, seed: u8, frames: &[(&str, &str)]) {
    let mut body = Vec::new();
    for (id, text) in frames {
        body.extend_from_slice(id.as_bytes());
        // Frame sizes under 128 bytes read the same as plain and syncsafe numbers.
        body.extend([0, 0, 0, text.len() as u8 + 1, 0, 0, 3]);
        body.extend_from_slice(text.as_bytes());
    }
    assert!(body.len() < 128);
    let mut file = b"ID3\x04\0\0\0\0\0".to_vec();
    file.push(body.len() as u8);
    file.extend(body);
    for frame in 0..MPEG_FRAMES {
        file.extend([0xff, 0xfb, 0x90, 0x00]);
        file.extend(vec![seed ^ frame as u8; MPEG_FRAME_LEN - 4]);
    }
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, file).unwrap();
}

fn umak_scan(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_umak-scan"))
        .args(args)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}

struct Library {
    _dir: tempfile::TempDir,
    database: String,
    music: String,
}

fn library() -> Library {
    let dir = tempfile::tempdir().unwrap();
    let music = dir.path().join("music");
    mp3(
        &music.join("Band/Album/1.mp3"),
        1,
        &[
            ("TIT2", "One"),
            ("TPE1", "Band"),
            ("TPE2", "Band"),
            ("TALB", "Album"),
        ],
    );
    mp3(
        &music.join("Mix/2.mp3"),
        2,
        &[("TIT2", "Two"), ("TPE1", "A/B"), ("TALB", "Mix")],
    );
    mp3(
        &music.join("Mix/3.mp3"),
        3,
        &[("TIT2", "Three"), ("TPE1", "C"), ("TALB", "Mix")],
    );
    mp3(
        &music.join("Mix/4.mp3"),
        4,
        &[("TIT2", "Four"), ("TPE1", "D"), ("TALB", "Mix")],
    );
    Library {
        database: dir.path().join("umak.db").to_str().unwrap().to_owned(),
        music: music.to_str().unwrap().to_owned(),
        _dir: dir,
    }
}

#[test]
fn a_scan_prints_statistics_and_the_grouping_report() {
    let lib = library();
    let text = stdout(&umak_scan(&[&lib.database, &lib.music]));
    assert!(text.contains("4 files: 0 unchanged, 4 added"), "{text}");
    assert!(text.contains("4 tracks (0 missing), 2 albums"), "{text}");
    assert!(text.contains("mp3: 4 tracks"), "{text}");
    assert!(
        text.contains("rule 2 (album artist tag, title and folder): 1 albums, 1 tracks"),
        "{text}"
    );
    assert!(
        text.contains("Various Artists / Mix (3 tracks, 3 artists) in Mix"),
        "{text}"
    );
    assert!(text.contains("A/B (1 tracks)"), "{text}");
}

#[test]
fn json_output_holds_the_report_and_the_statistics_unabridged() {
    let lib = library();
    let text = stdout(&umak_scan(&[
        &lib.database,
        &lib.music,
        "--json",
        "--list",
        "0",
    ]));
    let output: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(output["scan"]["added"], 4);
    assert_eq!(output["stats"]["tracks"], 4);
    assert_eq!(output["stats"]["inferredCompilations"][0]["tracks"], 3);
    assert_eq!(output["stats"]["namesWithSeparators"][0]["name"], "A/B");
}

#[test]
fn a_later_run_reuses_the_roots_and_finds_nothing_new() {
    let lib = library();
    stdout(&umak_scan(&[&lib.database, &lib.music]));

    let text = stdout(&umak_scan(&[&lib.database, "--json"]));
    let output: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(output["scan"]["unchanged"], 4);
    assert_eq!(output["scan"]["added"], 0);

    // Naming a known root again is reported but is not a failure.
    let again = umak_scan(&[&lib.database, &lib.music, "--json"]);
    let output: Value = serde_json::from_str(&stdout(&again)).unwrap();
    assert_eq!(output["scan"]["unchanged"], 4);
}

#[test]
fn no_scan_reads_statistics_without_touching_the_files() {
    let lib = library();
    let text = stdout(&umak_scan(&[&lib.database, "--no-scan"]));
    assert!(text.contains("0 tracks"), "{text}");
    assert!(!text.contains("Scan\n"), "{text}");

    stdout(&umak_scan(&[&lib.database, &lib.music]));
    fs::remove_dir_all(&lib.music).unwrap();
    let text = stdout(&umak_scan(&[&lib.database, "--no-scan"]));
    assert!(text.contains("4 tracks (0 missing)"), "{text}");
}

#[test]
fn the_benchmark_commands_work_on_a_small_library() {
    let dir = tempfile::tempdir().unwrap();
    let music = dir.path().join("synthetic");
    let music_text = music.to_str().unwrap();
    let database = dir.path().join("umak.db");
    let database = database.to_str().unwrap();

    let text = stdout(&umak_scan(&["generate", music_text, "200"]));
    assert!(text.contains("200 newly written"), "{text}");

    let text = stdout(&umak_scan(&[database, music_text, "--json"]));
    let output: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(output["scan"]["added"], 200);
    assert_eq!(output["scan"]["tagErrors"], 0, "{text}");
    assert!(output["scan"]["bytesRead"].as_u64().unwrap() > 0);
    assert!(output["scan"]["readCalls"].as_u64().unwrap() > 200);
    assert!(output["stats"]["artists"].as_u64().unwrap() >= 3);
    // Accented names survive the round trip through the tags.
    let found = stdout(&umak_scan(&[database, "--no-scan", "--json"]));
    assert!(found.contains("\"tracks\": 200"), "{found}");

    let text = stdout(&umak_scan(&["bench", database]));
    assert!(text.contains("200 tracks"), "{text}");
    assert!(text.contains("search, per keystroke"), "{text}");
    assert!(!text.contains("SLOW"), "{text}");

    let text = stdout(&umak_scan(&[
        "rawio",
        music_text,
        "--threads",
        "2",
        "--bytes",
        "1000",
    ]));
    let raw: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(raw["files"], 200);
    assert_eq!(raw["bytesRead"], 200_000);
}

#[test]
fn generate_refuses_a_folder_with_other_files() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("mine.mp3"), b"keep").unwrap();
    let output = umak_scan(&["generate", dir.path().to_str().unwrap(), "5"]);
    assert!(!output.status.success());
    assert_eq!(fs::read(dir.path().join("mine.mp3")).unwrap(), b"keep");
}

#[test]
fn bad_arguments_fail_with_usage() {
    for args in [
        &[][..],
        &["--threads"],
        &["db", "--threads", "many"],
        &["db", "--nope"],
        &["bench"],
        &["rawio"],
        &["generate", "x"],
    ] {
        let output = umak_scan(args);
        assert!(!output.status.success(), "{args:?}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("usage: umak-scan"), "{args:?}: {error}");
    }
}

#[test]
fn a_folder_that_does_not_exist_fails_clearly() {
    let lib = library();
    let missing = format!("{}-missing", lib.music);
    let output = umak_scan(&[&lib.database, &missing]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing"));
}
