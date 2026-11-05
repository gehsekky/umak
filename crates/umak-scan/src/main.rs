//! Headless scan and benchmark tool that runs the core without a UI.

use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use serde::Serialize;
use umak_core::api::{self, add_root};
use umak_core::db::{Database, DbError};
use umak_core::scan::{self, ScanOptions, ScanProgress, ScanReport};
use umak_core::stats::{self, AlbumNote, LibraryStats};

const USAGE: &str = "usage: umak-scan <database> [folder]... [options]

Adds any folders as library roots, scans every root, and prints statistics.

  --threads N   worker threads reading files
  --samples N   audio samples per fingerprint; 0 disables move detection
  --no-scan     print statistics for the database as it is
  --list N      how many albums and names to list in each section (default 20)
  --json        print everything, unabridged, as JSON";
const DEFAULT_LIST: usize = 20;
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
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("umak-scan: {error}");
            ExitCode::FAILURE
        }
    }
}
