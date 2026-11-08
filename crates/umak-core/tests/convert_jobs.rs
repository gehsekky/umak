//! Runs conversion batches through the command layer, as the app does: recording each file,
//! canceling, retrying and adding the results to the library. Needs FFmpeg, found as the app
//! finds it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use umak_convert::Ffmpeg;
use umak_core::api::{ApiError, Core, CoreEvent, ErrorCode, EventSink, dispatch};
use umak_core::db::Database;

const WAIT: Duration = Duration::from_secs(120);

struct ChannelSink(Mutex<Sender<CoreEvent>>);

impl EventSink for ChannelSink {
    fn emit(&self, event: &CoreEvent) {
        let _ = self.0.lock().unwrap().send(event.clone());
    }
}

struct Fixture {
    dir: TempDir,
    ffmpeg: Ffmpeg,
    core: Core,
    events: Receiver<CoreEvent>,
}

impl Fixture {
    /// A library of FLAC files, `seconds` long, at music/Artist/Album/song N.flac.
    fn new(count: usize, seconds: u32) -> Self {
        let ffmpeg =
            Ffmpeg::find().unwrap_or_else(|error| panic!("these tests need FFmpeg: {error}"));
        let dir = tempfile::tempdir().unwrap();
        let (sender, events) = mpsc::channel();
        let db = Database::open(dir.path().join("umak.db")).unwrap();
        let core = Core::new(db, Box::new(ChannelSink(Mutex::new(sender)))).unwrap();
        let fixture = Self {
            dir,
            ffmpeg,
            core,
            events,
        };
        for number in 1..=count {
            fixture.write_source(number, seconds);
        }
        let music = fixture.path("music");
        fixture.call("library.roots.add", json!({ "path": music }));
        fixture.call("library.scan.start", json!({ "roots": null }));
        fixture.wait_for_job();
        fixture
    }

    fn path(&self, rel: &str) -> String {
        self.dir.path().join(rel).to_string_lossy().into_owned()
    }

    fn source(&self, number: usize) -> PathBuf {
        self.dir
            .path()
            .join(format!("music/Artist/Album/song {number}.flac"))
    }

    fn write_source(&self, number: usize, seconds: u32) {
        let path = self.source(number);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let tone = format!("sine=f={}:d={seconds}", 200 + number * 50);
        let title = format!("title=Song {number}");
        let track = format!("track={number}");
        let status = Command::new(self.ffmpeg.path())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i", &tone, "-ac", "2", "-c:a", "flac"])
            .args(["-metadata", &title, "-metadata", &track])
            .args(["-metadata", "artist=Artist", "-metadata", "album=Album"])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn try_call(&self, name: &str, args: Value) -> Result<Value, ApiError> {
        dispatch(&self.core, name, args)
    }

    fn call(&self, name: &str, args: Value) -> Value {
        self.try_call(name, args)
            .unwrap_or_else(|error| panic!("{name}: {error}"))
    }

    fn wait_for_job(&self) {
        loop {
            if let CoreEvent::JobFinished { .. } = self.events.recv_timeout(WAIT).unwrap() {
                return;
            }
        }
    }

    fn track_ids(&self, query: Option<&str>) -> Vec<String> {
        let page = self.call(
            "library.tracks.page",
            json!({ "query": query, "sort": "title", "descending": false, "offset": 0, "limit": 100 }),
        );
        page["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|track| track["id"].as_str().unwrap().to_owned())
            .collect()
    }

    fn start(&self, settings: Value, folder: &str, threads: u32, add: bool) -> String {
        let job = self.call(
            "convert.start",
            json!({
                "trackIds": self.track_ids(None),
                "settings": settings,
                "folder": folder,
                "threads": threads,
                "addToLibrary": add,
            }),
        );
        job["id"].as_str().unwrap().to_owned()
    }

    fn job(&self, id: &str) -> Value {
        let jobs = self.call("jobs.list", json!({}));
        jobs["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|job| job["id"] == id)
            .unwrap()
            .clone()
    }

    fn problems(&self, id: &str) -> Value {
        self.call(
            "convert.files",
            json!({ "jobId": id, "problemsOnly": true, "offset": 0, "limit": 100 }),
        )
    }
}

fn mp3_v0() -> Value {
    json!({ "codec": "mp3", "mp3Quality": 0 })
}

fn flac() -> Value {
    json!({ "codec": "flac" })
}

/// Every file under `dir`, by path relative to it.
fn files_under(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut folders = vec![dir.to_owned()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                folders.push(path);
            } else {
                let rel = path.strip_prefix(dir).unwrap().to_string_lossy();
                found.push(rel.replace('\\', "/"));
            }
        }
    }
    found.sort();
    found
}

#[test]
fn a_batch_converts_every_file_and_adds_them_to_the_library_as_a_new_root() {
    let fixture = Fixture::new(4, 2);
    let out = fixture.path("out");
    let id = fixture.start(mp3_v0(), &out, 2, true);
    fixture.wait_for_job();

    let job = fixture.job(&id);
    assert_eq!(job["kind"], "convert");
    assert_eq!(job["status"], "completed");
    assert_eq!(
        (job["done"].as_u64(), job["total"].as_u64()),
        (Some(4), Some(4))
    );
    let report = &job["convert"];
    assert_eq!(report["converted"], 4);
    assert_eq!(report["failed"], 0);
    assert_eq!(report["incompleteTags"], 0);
    assert_eq!(report["addedToLibrary"], 4);
    assert_eq!(fixture.problems(&id)["total"], 0);
    assert_eq!(
        files_under(Path::new(&out)),
        (1..=4)
            .map(|number| format!("Artist/Album/song {number}.mp3"))
            .collect::<Vec<_>>()
    );

    // The new files are in the library, with their tags, under a root of their own.
    let roots = fixture.call("library.roots.list", json!({}));
    assert_eq!(roots["roots"].as_array().unwrap().len(), 2);
    let page = fixture.call(
        "library.tracks.page",
        json!({ "query": "Song 3", "sort": "title", "descending": false, "offset": 0, "limit": 10 }),
    );
    let codecs: Vec<&str> = page["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|track| track["codec"].as_str().unwrap())
        .collect();
    assert_eq!(codecs.len(), 2, "{page}");
    assert!(
        codecs.contains(&"mp3") && codecs.contains(&"flac"),
        "{codecs:?}"
    );

    let info = fixture.call("convert.get", json!({ "jobId": id }));
    assert_eq!(info["settings"]["codec"], "mp3");
    assert_eq!(info["threads"], 2);
    assert_eq!(info["addToLibrary"], true);

    // Converting again into the same folder leaves the files already there alone. The new
    // root's own tracks are in the batch too, and would go where they already are.
    let again = fixture.call(
        "convert.start",
        json!({
            "trackIds": fixture.track_ids(Some("Album")),
            "settings": mp3_v0(),
            "folder": out,
            "addToLibrary": false,
        }),
    )["id"]
        .as_str()
        .unwrap()
        .to_owned();
    fixture.wait_for_job();
    let report = &fixture.job(&again)["convert"];
    assert_eq!(report["converted"], 0);
    assert_eq!(report["exists"], 8);
    assert!(report["addedToLibrary"].is_null());
}

#[test]
fn a_failed_file_is_reported_and_converts_on_retry_into_its_root() {
    let fixture = Fixture::new(3, 1);
    // Ruined after the scan, so the library still lists it.
    let ruined = fixture.source(2);
    let original = fs::read(&ruined).unwrap();
    fs::write(&ruined, b"not audio at all").unwrap();

    let out = fixture.path("music/converted");
    let id = fixture.start(flac(), &out, 2, true);
    fixture.wait_for_job();
    let job = fixture.job(&id);
    assert_eq!(job["status"], "completed");
    let report = &job["convert"];
    assert_eq!(
        (report["converted"].as_u64(), report["failed"].as_u64()),
        (Some(2), Some(1))
    );
    assert_eq!(report["addedToLibrary"], 2);
    let problems = fixture.problems(&id);
    assert_eq!(problems["total"], 1);
    let failed = &problems["files"][0];
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["sourcePath"], "Artist/Album/song 2.flac");
    assert!(
        failed["message"]
            .as_str()
            .unwrap()
            .contains("FFmpeg failed"),
        "{failed}"
    );
    assert!(!Path::new(failed["outputPath"].as_str().unwrap()).exists());
    // Still one root: the folder is inside it.
    let roots = fixture.call("library.roots.list", json!({}));
    assert_eq!(roots["roots"].as_array().unwrap().len(), 1);

    fs::write(&ruined, original).unwrap();
    let retry = fixture.call("convert.retry", json!({ "jobId": id }))["id"]
        .as_str()
        .unwrap()
        .to_owned();
    fixture.wait_for_job();
    let report = &fixture.job(&retry)["convert"];
    assert_eq!(
        (report["converted"].as_u64(), report["failed"].as_u64()),
        (Some(1), Some(0))
    );
    assert_eq!(report["addedToLibrary"], 1);
    assert_eq!(fixture.track_ids(Some("Song")).len(), 6);
    assert_eq!(
        files_under(Path::new(&out)),
        (1..=3)
            .map(|number| format!("Artist/Album/song {number}.flac"))
            .collect::<Vec<_>>()
    );

    let nothing_left = fixture
        .try_call("convert.retry", json!({ "jobId": retry }))
        .unwrap_err();
    assert_eq!(nothing_left.code, ErrorCode::InvalidArgs);
}

#[test]
fn canceling_leaves_no_partial_files_and_the_rest_can_be_retried() {
    let fixture = Fixture::new(6, 60);
    let out = fixture.path("out");
    let id = fixture.start(
        json!({ "codec": "aac", "bitrateKbps": 256 }),
        &out,
        1,
        false,
    );
    // Canceled once FFmpeg is under way on the first file.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        fixture.call("jobs.cancel", json!({ "id": id }))["requested"],
        true
    );
    fixture.wait_for_job();

    let job = fixture.job(&id);
    assert_eq!(job["status"], "canceled");
    let report = &job["convert"];
    let converted = report["converted"].as_u64().unwrap();
    assert!(report["canceled"].as_u64().unwrap() >= 1, "{report}");
    assert_eq!(report["pending"], 0);
    assert_eq!(converted + report["canceled"].as_u64().unwrap(), 6);
    // Only whole files: no partial output, and nothing for the canceled ones.
    let written = if Path::new(&out).exists() {
        files_under(Path::new(&out))
    } else {
        Vec::new()
    };
    assert_eq!(written.len() as u64, converted, "{written:?}");
    assert!(
        written.iter().all(|name| name.ends_with(".m4a")),
        "{written:?}"
    );

    let retry = fixture.call("convert.retry", json!({ "jobId": id }))["id"]
        .as_str()
        .unwrap()
        .to_owned();
    fixture.wait_for_job();
    let report = &fixture.job(&retry)["convert"];
    assert_eq!(report["converted"].as_u64().unwrap() + converted, 6);
    assert_eq!(files_under(Path::new(&out)).len(), 6);
}

#[test]
fn requests_that_cannot_work_are_refused_before_a_job_starts() {
    let fixture = Fixture::new(1, 1);
    let ids = fixture.track_ids(None);
    let refused = |settings: Value, folder: String, threads: Option<u32>, add: bool| {
        fixture
            .try_call(
                "convert.start",
                json!({
                    "trackIds": ids,
                    "settings": settings,
                    "folder": folder,
                    "threads": threads,
                    "addToLibrary": add,
                }),
            )
            .unwrap_err()
    };
    let opus = json!({ "codec": "opus", "bitrateKbps": 160 });
    let error = refused(opus, fixture.path("out"), None, true);
    assert_eq!(error.code, ErrorCode::InvalidArgs);
    assert!(error.message.contains("Opus"), "{error}");

    // A folder around the library would index the library twice.
    let error = refused(flac(), fixture.path(""), None, true);
    assert_eq!(error.code, ErrorCode::Conflict, "{error}");

    let error = refused(flac(), "relative/folder".to_owned(), None, false);
    assert_eq!(error.code, ErrorCode::InvalidArgs);
    let error = refused(flac(), fixture.path("out"), Some(0), false);
    assert_eq!(error.code, ErrorCode::InvalidArgs);

    assert_eq!(
        fixture.call("jobs.list", json!({}))["jobs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
