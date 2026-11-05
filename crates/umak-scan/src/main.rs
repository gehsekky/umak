//! Headless scan and benchmark tool that runs the core without a UI.
//!
//! Usage: umak-scan <database> <folder>... [--threads N] [--samples N]

use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use umak_core::api::{self, add_root};
use umak_core::db::{Database, DbError};
use umak_core::scan::{self, ScanOptions, ScanProgress};

struct Args {
    database: String,
    folders: Vec<String>,
    options: ScanOptions,
}

fn parse_args() -> Result<Args, String> {
    let mut positional = Vec::new();
    let mut options = ScanOptions::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut number = |name: &str| {
            args.next()
                .and_then(|value| value.parse::<usize>().ok())
                .ok_or(format!("{name} needs a number"))
        };
        match arg.as_str() {
            "--threads" => options.threads = number("--threads")?,
            "--samples" => options.fingerprint_samples = number("--samples")?,
            _ => positional.push(arg),
        }
    }
    let mut positional = positional.into_iter();
    let database = positional
        .next()
        .ok_or("usage: umak-scan <database> <folder>... [--threads N] [--samples N]")?;
    Ok(Args {
        database,
        folders: positional.collect(),
        options,
    })
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let db = Database::open(&args.database).map_err(|error: DbError| error.to_string())?;
    for folder in &args.folders {
        match add_root(&db, folder, None) {
            Ok(root) => println!("added {}", root.path),
            Err(error) if error.code == api::ErrorCode::Conflict => {}
            Err(error) => return Err(error.to_string()),
        }
    }

    let cancel = AtomicBool::new(false);
    let mut last_percent = None;
    let mut progress = |update: ScanProgress| {
        let percent = (update.done * 100).checked_div(update.total).unwrap_or(100) / 10;
        if last_percent != Some(percent) {
            last_percent = Some(percent);
            eprintln!("{} of {} files", update.done, update.total);
        }
    };
    let report = scan::scan_library(&db, None, args.options, &cancel, &mut progress)
        .map_err(|error| error.to_string())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
    );
    Ok(())
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
