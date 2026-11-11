//! Headless scan and benchmark tool that runs the core without a UI.

mod bench;
mod generate;

use std::fs;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use umak_core::api::{self, CoreEvent, EventSink, add_root};
use umak_core::art::ArtCache;
use umak_core::db::{Database, DbError};
use umak_core::scan::{self, ScanOptions, ScanProgress, ScanReport};
use umak_core::stats::{self, AlbumNote, LibraryStats};

const USAGE: &str = "usage: umak-scan <database> [folder]... [options]

Adds any folders as library roots, scans every root, and prints statistics.

Other commands:

  umak-scan bench <database>                  time queries against the 10 ms target
  umak-scan rawio <folder>... [--threads N] [--bytes N]
                                              time listing and reading with no parsing
  umak-scan generate <folder> <count>         write synthetic tracks for testing
  umak-scan art <database>                    fill the cover art cache beside the database

  --threads N   worker threads reading files
  --samples N   audio samples per fingerprint; 0 disables move detection
  --no-scan     print statistics for the database as it is
  --list N      how many albums and names to list in each section (default 20)
  --json        print everything, unabridged, as JSON";
const DEFAULT_LIST: usize = 20;
/// About what the scanner reads from each file, measured on a real library.
const DEFAULT_RAW_BYTES: u64 = 128 * 1024;
const BYTES_PER_GIB: f64 = 1024.0 * 1024.0 * 1024.0;
const MS_PER_HOUR: f64 = 3_600_000.0;

struct Args {
    database: String,
    folders: Vec<String>,
    options: ScanOptions,
    scan: bool,
    list: usize,
    json: bool,
}

fn parse_args(raw: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut positional = Vec::new();
    let mut options = ScanOptions::default();
    let (mut scan, mut list, mut json) = (true, DEFAULT_LIST, false);
    let mut raw = raw;
    while let Some(arg) = raw.next() {
        let mut number = |name: &str| {
            raw.next()
                .and_then(|value| value.parse::<usize>().ok())
                .ok_or(format!("{name} needs a number\n\n{USAGE}"))
        };
        match arg.as_str() {
            "--threads" => options.threads = number("--threads")?,
            "--samples" => options.fingerprint_samples = number("--samples")?,
            "--list" => list = number("--list")?,
            "--no-scan" => scan = false,
            "--json" => json = true,
            other if other.starts_with("--") => {
                return Err(format!("unknown option {other}\n\n{USAGE}"));
            }
            _ => positional.push(arg),
        }
    }
    let mut positional = positional.into_iter();
    let database = positional.next().ok_or(USAGE)?;
    Ok(Args {
        database,
        folders: positional.collect(),
        options,
        scan,
        list,
        json,
    })
}

#[derive(Serialize)]
struct Output<'a> {
    scan: Option<&'a ScanReport>,
    stats: &'a LibraryStats,
}

fn print_json(value: &impl Serialize) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    println!("{text}");
    Ok(())
}

fn run_bench(mut args: impl Iterator<Item = String>) -> Result<ExitCode, String> {
    let database = args.next().ok_or(USAGE)?;
    let report = bench::queries(&database)?;
    println!(
        "{} tracks; first page after opening: {:.1} ms; target {} ms at the 95th percentile",
        report.tracks, report.first_page_after_open_ms, report.target_ms
    );
    for timing in &report.timings {
        println!(
            "  {}  {:<36} {:>5} runs  median {:>6.2}  p95 {:>6.2}  p99 {:>6.2}  max {:>6.2} ms",
            if timing.within_target { "ok  " } else { "SLOW" },
            timing.name,
            timing.runs,
            timing.median_ms,
            timing.p95_ms,
            timing.p99_ms,
            timing.max_ms,
        );
    }
    let met = report.timings.iter().all(|timing| timing.within_target);
    Ok(if met {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Fills the art cache beside a database and reports how long it took.
fn run_art(mut args: impl Iterator<Item = String>) -> Result<ExitCode, String> {
    let database = args.next().ok_or(USAGE)?;
    let db = Arc::new(Database::open(&database).map_err(|error| error.to_string())?);
    let done = Arc::new(AtomicU64::new(0));
    let cache = ArtCache::new(Arc::clone(&db), Arc::new(CountArt(Arc::clone(&done))));
    let started = Instant::now();
    cache.keep_filled();
    let mut reported = 0;
    while cache.is_busy() {
        thread::sleep(Duration::from_millis(200));
        let now = done.load(Ordering::Relaxed);
        if now >= reported + 1000 {
            println!("{now} tracks in {:.0} s", started.elapsed().as_secs_f64());
            reported = now;
        }
    }
    let seconds = started.elapsed().as_secs_f64();
    let tracks = done.load(Ordering::Relaxed);
    let count = |sql: &str| -> Result<i64, String> {
        db.read(|conn| {
            conn.query_row(sql, [], |row| row.get(0))
                .map_err(DbError::from)
        })
        .map_err(|error| error.to_string())
    };
    println!(
        "{tracks} tracks in {seconds:.1} s, {:.0} a second",
        tracks as f64 / seconds.max(0.001)
    );
    println!(
        "Art from tags: {}, from folders: {}, none: {}; {} pictures",
        count("SELECT count(*) FROM track_art WHERE source = 'embedded'")?,
        count("SELECT count(*) FROM track_art WHERE source = 'folder'")?,
        count("SELECT count(*) FROM track_art WHERE source = 'none'")?,
        count("SELECT count(DISTINCT art) FROM track_art")?,
    );
    let bytes = folder_bytes(cache.folder());
    println!("Art folder: {:.1} MB", bytes as f64 / 1e6);
    Ok(ExitCode::SUCCESS)
}

/// Counts the tracks the art cache reports done.
struct CountArt(Arc<AtomicU64>);

impl EventSink for CountArt {
    fn emit(&self, event: &CoreEvent) {
        if let CoreEvent::ArtReady { track_ids } = event {
            self.0.fetch_add(track_ids.len() as u64, Ordering::Relaxed);
        }
    }
}

fn folder_bytes(folder: &Path) -> u64 {
    fs::read_dir(folder).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .map(|entry| match entry.metadata() {
                Ok(metadata) if metadata.is_dir() => folder_bytes(&entry.path()),
                Ok(metadata) => metadata.len(),
                Err(_) => 0,
            })
            .sum()
    })
}

fn run_raw_io(args: impl Iterator<Item = String>) -> Result<ExitCode, String> {
    let mut folders = Vec::new();
    let mut threads = ScanOptions::default().threads;
    let mut bytes = DEFAULT_RAW_BYTES;
    let mut args = args;
    while let Some(arg) = args.next() {
        let mut number = |name: &str| {
            args.next()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or(format!("{name} needs a number\n\n{USAGE}"))
        };
        match arg.as_str() {
            "--threads" => threads = number("--threads")? as usize,
            "--bytes" => bytes = number("--bytes")?,
            other if other.starts_with("--") => {
                return Err(format!("unknown option {other}\n\n{USAGE}"));
            }
            _ => folders.push(arg),
        }
    }
    if folders.is_empty() {
        return Err(USAGE.to_owned());
    }
    print_json(&bench::raw_io(&folders, threads, bytes)?)?;
    Ok(ExitCode::SUCCESS)
}

fn run_generate(mut args: impl Iterator<Item = String>) -> Result<ExitCode, String> {
    let folder = args.next().ok_or(USAGE)?;
    let count = args
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or(USAGE)?;
    let created = generate::generate(Path::new(&folder), count)?;
    println!("{count} synthetic tracks in {folder}; {created} newly written");
    Ok(ExitCode::SUCCESS)
}

fn run() -> Result<(), String> {
    let args = parse_args(std::env::args().skip(1))?;
    let db = Database::open(&args.database).map_err(|error: DbError| error.to_string())?;
    for folder in &args.folders {
        match add_root(&db, folder, None) {
            Ok(root) => eprintln!("added {}", root.path),
            // Scanning again with the same folder is normal.
            Err(error) if error.code == api::ErrorCode::Conflict => {
                eprintln!("{folder}: {error}");
            }
            Err(error) => return Err(error.to_string()),
        }
    }

    let report = if args.scan {
        let cancel = AtomicBool::new(false);
        let mut last_step = None;
        let mut progress = |update: ScanProgress| {
            let step = (update.done * 100).checked_div(update.total).unwrap_or(100) / 10;
            if last_step != Some(step) {
                last_step = Some(step);
                eprintln!("{} of {} files", update.done, update.total);
            }
        };
        Some(
            scan::scan_library(&db, None, args.options, &cancel, &mut progress)
                .map_err(|error| error.to_string())?,
        )
    } else {
        None
    };
    let stats = stats::library_stats(&db).map_err(|error| error.to_string())?;

    if args.json {
        let output = Output {
            scan: report.as_ref(),
            stats: &stats,
        };
        let text = serde_json::to_string_pretty(&output).map_err(|error| error.to_string())?;
        println!("{text}");
    } else {
        if let Some(report) = &report {
            print_scan(report, args.list);
        }
        print_stats(&stats, args.list);
    }
    Ok(())
}

fn print_scan(report: &ScanReport, list: usize) {
    println!("Scan");
    println!(
        "  {} files: {} unchanged, {} added, {} updated, {} moved, {} restored, {} newly missing",
        report.files_found,
        report.unchanged,
        report.added,
        report.updated,
        report.moved,
        report.restored,
        report.missing,
    );
    println!(
        "  {} ms total with {} threads: {} ms walking, {} ms writing; \
         thread time {} ms reading tags, {} ms fingerprinting",
        report.total_ms,
        report.threads,
        report.walk_ms,
        report.write_ms,
        report.tag_ms,
        report.fingerprint_ms,
    );
    let thread_ms = report.tag_ms + report.fingerprint_ms;
    println!(
        "  of that thread time, {} ms waiting on the disk and {} ms in our code; {:.1} MB read in {} calls",
        report.io_ms,
        thread_ms.saturating_sub(report.io_ms),
        report.bytes_read as f64 / 1e6,
        report.read_calls,
    );
    println!(
        "  {} tag errors, {} read errors",
        report.tag_errors, report.read_errors
    );
    if report.canceled {
        println!("  canceled before finishing");
    }
    for root in &report.offline_roots {
        println!("  OFFLINE, left untouched: {root}");
    }
    for issue in report.issues.iter().take(list) {
        println!("  {}: {}", issue.path, issue.message);
    }
    println!();
}

fn print_albums(title: &str, albums: &[AlbumNote], list: usize) {
    println!("{title}: {}", albums.len());
    for album in albums.iter().take(list) {
        println!(
            "  {} / {} ({} tracks, {} artists) in {}",
            album.artist, album.title, album.tracks, album.track_artists, album.folder
        );
    }
    if albums.len() > list {
        println!("  and {} more", albums.len() - list);
    }
    println!();
}

fn print_stats(stats: &LibraryStats, list: usize) {
    println!("Library");
    println!(
        "  {} tracks ({} missing), {} albums, {} artists, {} genres",
        stats.tracks, stats.missing_tracks, stats.albums, stats.artists, stats.genres
    );
    println!(
        "  {:.1} GiB, {:.1} hours",
        stats.total_bytes as f64 / BYTES_PER_GIB,
        stats.total_duration_ms as f64 / MS_PER_HOUR
    );
    for root in &stats.roots {
        println!(
            "  {}: {} tracks, {} missing",
            root.path, root.tracks, root.missing
        );
    }
    println!();

    println!("Formats");
    for codec in &stats.codecs {
        let rates = match (codec.min_sample_rate, codec.max_sample_rate) {
            (Some(low), Some(high)) => format!("{low} to {high} Hz"),
            _ => "unknown rate".to_owned(),
        };
        let depth = codec
            .max_bit_depth
            .map_or(String::new(), |bits| format!(", up to {bits}-bit"));
        println!(
            "  {}: {} tracks, {:.1} GiB, {rates}{depth}",
            codec.codec,
            codec.tracks,
            codec.bytes as f64 / BYTES_PER_GIB
        );
    }
    println!();

    println!("Album grouping");
    for group in &stats.grouping {
        let meaning = match group.rule {
            1 => "MusicBrainz album ID",
            2 => "album artist tag, title and folder",
            3 => "compilation flag, no album artist",
            _ => "title and folder only, artist judged",
        };
        println!(
            "  rule {} ({meaning}): {} albums, {} tracks",
            group.rule, group.albums, group.tracks
        );
    }
    println!("  flagged as compilations: {}", stats.tagged_compilations);
    println!();

    print_albums(
        "Albums with no album artist tag",
        &stats.albums_without_album_artist,
        list,
    );
    print_albums("Inferred compilations", &stats.inferred_compilations, list);

    let gaps = &stats.tag_gaps;
    println!("Tracks missing a tag");
    println!("  title: {}", gaps.no_title);
    println!("  artist: {}", gaps.no_artist);
    println!("  album: {}", gaps.no_album);
    println!("  track number: {}", gaps.no_track_number);
    println!("  year: {}", gaps.no_year);
    println!("  ReplayGain: {}", gaps.no_replay_gain);
    println!("  MusicBrainz ID: {}", gaps.no_musicbrainz_id);
    println!();

    println!(
        "Artist values containing a separator, each kept as one artist: {}",
        stats.names_with_separators.len()
    );
    for name in stats.names_with_separators.iter().take(list) {
        println!("  {} ({} tracks)", name.name, name.tracks);
    }
    if stats.names_with_separators.len() > list {
        println!("  and {} more", stats.names_with_separators.len() - list);
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1).peekable();
    let result = match args.peek().map(String::as_str) {
        Some("bench") => run_bench(args.skip(1)),
        Some("rawio") => run_raw_io(args.skip(1)),
        Some("generate") => run_generate(args.skip(1)),
        Some("art") => run_art(args.skip(1)),
        _ => run().map(|()| ExitCode::SUCCESS),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("umak-scan: {error}");
            ExitCode::FAILURE
        }
    }
}
