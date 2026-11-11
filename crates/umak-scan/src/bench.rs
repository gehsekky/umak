//! Performance checks. `queries` times commands through the real command layer, including
//! JSON. `raw_io` measures the disk with no tag parsing, as a reference for scan times.

use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::thread;
use std::time::Instant;

use serde::Serialize;
use serde_json::{Value, json};
use umak_core::api::{Core, CoreEvent, EventSink, dispatch};
use umak_core::db::Database;
use umak_core::scan::walk;

/// The exit criterion for a keystroke or a page of a list, at the 95th percentile.
const TARGET_MS: f64 = 10.0;
const PAGE: u32 = 200;
const LIST_RUNS: usize = 300;
const WORD_SAMPLE_PAGES: u32 = 40;
const MAX_WORDS: usize = 150;
const MIN_WORD_LEN: usize = 4;
const SORTS: &[&str] = &["album", "artist", "title", "added"];
const ALBUM_SORTS: &[&str] = &["artist", "title", "year"];

struct NoEvents;

impl EventSink for NoEvents {
    fn emit(&self, _event: &CoreEvent) {}
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Timing {
    pub name: String,
    pub runs: usize,
    pub median_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    pub within_target: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryReport {
    pub tracks: u64,
    pub first_page_after_open_ms: f64,
    pub target_ms: f64,
    pub timings: Vec<Timing>,
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    let index = ((sorted.len() as f64 * fraction).ceil() as usize).saturating_sub(1);
    sorted[index.min(sorted.len() - 1)]
}

fn summarize(name: &str, mut samples: Vec<f64>) -> Timing {
    samples.sort_by(f64::total_cmp);
    let p95 = percentile(&samples, 0.95);
    Timing {
        name: name.to_owned(),
        runs: samples.len(),
        median_ms: percentile(&samples, 0.5),
        p95_ms: p95,
        p99_ms: percentile(&samples, 0.99),
        max_ms: samples[samples.len() - 1],
        within_target: p95 <= TARGET_MS,
    }
}

fn page_args(query: Option<&str>, sort: &str, descending: bool, offset: u64) -> Value {
    json!({
        "query": query,
        "sort": sort,
        "descending": descending,
        "offset": offset,
        "limit": PAGE,
    })
}

fn timed(core: &Core, command: &str, args: Value) -> Result<(f64, Value), String> {
    let started = Instant::now();
    let result = dispatch(core, command, args).map_err(|error| error.message.clone())?;
    Ok((started.elapsed().as_secs_f64() * 1000.0, result))
}

fn timed_page(core: &Core, args: Value) -> Result<(f64, Value), String> {
    timed(core, "library.tracks.page", args)
}

/// A repeatable sequence of positions spread over the whole list.
fn offsets(total: u64, runs: usize) -> impl Iterator<Item = u64> {
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    (0..runs).map(move |_| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % total.max(1)
    })
}

/// What someone might type: real words from the library, one keystroke at a time.
fn keystrokes(core: &Core, total: u64) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut seen = HashSet::new();
    for page in 0..WORD_SAMPLE_PAGES {
        let offset = total * u64::from(page) / u64::from(WORD_SAMPLE_PAGES);
        let (_, result) = timed_page(core, page_args(None, "album", false, offset))?;
        for track in result["tracks"].as_array().into_iter().flatten() {
            for field in ["title", "artist", "album"] {
                for word in track[field].as_str().unwrap_or("").split_whitespace() {
                    let word = word.to_lowercase();
                    let usable = word.chars().count() >= MIN_WORD_LEN
                        && word.chars().all(char::is_alphanumeric);
                    if usable && seen.insert(word.clone()) {
                        words.push(word);
                    }
                }
            }
        }
    }
    // Spread evenly, so the sample is not all from the start of the alphabet.
    let step = (words.len() / MAX_WORDS).max(1);
    let mut typed: Vec<String> = Vec::new();
    for word in words.iter().step_by(step) {
        let letters: Vec<char> = word.chars().collect();
        for length in 1..=letters.len() {
            typed.push(letters[..length].iter().collect());
        }
    }
    // Single letters and common words match the most, so they are the worst cases.
    typed.extend(('a'..='z').map(String::from));
    typed.extend(['0', '1', '2'].map(String::from));
    for common in [
        "the", "of", "and", "you", "love", "live", "the a", "a the", "of the",
    ] {
        typed.push(common.to_owned());
    }
    Ok(typed)
}

pub fn queries(database: &str) -> Result<QueryReport, String> {
    let db = Database::open(database).map_err(|error| error.to_string())?;
    let core = Core::new(db, Box::new(NoEvents)).map_err(|error| error.to_string())?;
    let (first_page_after_open_ms, first) = timed_page(&core, page_args(None, "album", false, 0))?;
    let total = first["total"].as_u64().unwrap_or(0);

    let mut timings = Vec::new();
    for sort in SORTS {
        let mut samples = Vec::with_capacity(LIST_RUNS);
        for (index, offset) in offsets(total, LIST_RUNS).enumerate() {
            let (ms, _) = timed_page(&core, page_args(None, sort, index % 2 == 1, offset))?;
            samples.push(ms);
        }
        timings.push(summarize(
            &format!("list page, any position, by {sort}"),
            samples,
        ));
    }

    let typed = keystrokes(&core, total)?;
    let mut samples = Vec::with_capacity(typed.len());
    for text in &typed {
        let (ms, _) = timed_page(&core, page_args(Some(text), "album", false, 0))?;
        samples.push(ms);
    }
    timings.push(summarize("search, per keystroke", samples));

    for sort in &SORTS[1..] {
        let mut samples = Vec::with_capacity(typed.len());
        for text in &typed {
            let (ms, _) = timed_page(&core, page_args(Some(text), sort, false, 0))?;
            samples.push(ms);
        }
        timings.push(summarize(&format!("search, sorted by {sort}"), samples));
    }

    let mut samples = Vec::with_capacity(typed.len());
    for text in &typed {
        let (ms, _) = timed_page(&core, page_args(Some(text), "album", false, 1000))?;
        samples.push(ms);
    }
    timings.push(summarize("search, deep in the results", samples));

    let (_, first) = timed(
        &core,
        "library.albums.page",
        page_args(None, "artist", false, 0),
    )?;
    let albums = first["total"].as_u64().unwrap_or(0);
    let mut album_ids = Vec::new();
    for sort in ALBUM_SORTS {
        let mut samples = Vec::with_capacity(LIST_RUNS);
        for (index, offset) in offsets(albums, LIST_RUNS).enumerate() {
            let args = page_args(None, sort, index % 2 == 1, offset);
            let (ms, page) = timed(&core, "library.albums.page", args)?;
            samples.push(ms);
            if let Some(id) = page["albums"][0]["id"].as_str() {
                album_ids.push(id.to_owned());
            }
        }
        timings.push(summarize(
            &format!("album page, any position, by {sort}"),
            samples,
        ));
    }
    let mut samples = Vec::with_capacity(typed.len());
    for text in &typed {
        let args = page_args(Some(text), "artist", false, 0);
        samples.push(timed(&core, "library.albums.page", args)?.0);
    }
    timings.push(summarize("album search, per keystroke", samples));
    let mut samples = Vec::with_capacity(album_ids.len());
    for id in &album_ids {
        samples.push(timed(&core, "library.albums.get", json!({ "id": id }))?.0);
    }
    timings.push(summarize("album with its tracks", samples));

    for by in ["audio", "tags"] {
        let mut samples = Vec::with_capacity(LIST_RUNS);
        for _ in 0..LIST_RUNS {
            let args = json!({ "by": by, "offset": 0, "limit": 100 });
            let started = Instant::now();
            dispatch(&core, "duplicates.list", args).map_err(|error| error.message.clone())?;
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        timings.push(summarize(
            &format!("duplicates by {by}, first page"),
            samples,
        ));
    }

    Ok(QueryReport {
        tracks: total,
        first_page_after_open_ms,
        target_ms: TARGET_MS,
        timings,
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawIoReport {
    pub files: u64,
    pub folders: Vec<String>,
    pub threads: usize,
    pub bytes_per_file: u64,
    pub list_ms: u64,
    pub read_ms: u64,
    pub bytes_read: u64,
    pub ms_per_file: f64,
}

/// Lists the folders, then reads the start of every file in the scanner's order and
/// with its thread count, doing nothing with the bytes. This is about the least any
/// import of these files could cost.
pub fn raw_io(
    folders: &[String],
    threads: usize,
    bytes_per_file: u64,
) -> Result<RawIoReport, String> {
    let started = Instant::now();
    let mut paths = Vec::new();
    for folder in folders {
        let root = Path::new(folder);
        for file in walk::walk(root, "").files {
            paths.push(root.join(file.rel_path));
        }
    }
    let list_ms = started.elapsed().as_millis() as u64;

    let started = Instant::now();
    let next = AtomicUsize::new(0);
    let bytes_read = AtomicU64::new(0);
    let threads = threads.max(1);
    thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                let mut buffer = vec![0; bytes_per_file as usize];
                while let Some(path) = paths.get(next.fetch_add(1, Ordering::Relaxed)) {
                    if let Ok(mut file) = File::open(path) {
                        let mut left = buffer.as_mut_slice();
                        while !left.is_empty() {
                            match file.read(left) {
                                Ok(0) | Err(_) => break,
                                Ok(count) => {
                                    bytes_read.fetch_add(count as u64, Ordering::Relaxed);
                                    left = &mut left[count..];
                                }
                            }
                        }
                    }
                }
            });
        }
    });
    let read_ms = started.elapsed().as_millis() as u64;
    Ok(RawIoReport {
        files: paths.len() as u64,
        folders: folders.to_vec(),
        threads,
        bytes_per_file,
        list_ms,
        read_ms,
        bytes_read: bytes_read.load(Ordering::Relaxed),
        ms_per_file: read_ms as f64 / paths.len().max(1) as f64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_pick_the_right_sample() {
        let samples: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&samples, 0.5), 50.0);
        assert_eq!(percentile(&samples, 0.95), 95.0);
        assert_eq!(percentile(&samples, 1.0), 100.0);
        assert_eq!(percentile(&[7.0], 0.95), 7.0);
    }

    #[test]
    fn a_timing_reports_whether_it_met_the_target() {
        assert!(summarize("fast", vec![1.0; 100]).within_target);
        let mut slow = vec![1.0; 90];
        slow.extend([50.0; 10]);
        let timing = summarize("slow", slow);
        assert!(!timing.within_target);
        assert_eq!(timing.max_ms, 50.0);
    }

    #[test]
    fn offsets_stay_in_range_and_repeat() {
        let first: Vec<u64> = offsets(1000, 50).collect();
        assert!(first.iter().all(|offset| *offset < 1000));
        assert_eq!(first, offsets(1000, 50).collect::<Vec<_>>());
        assert_eq!(offsets(0, 3).collect::<Vec<_>>(), [0, 0, 0]);
    }
}
