//! Lightweight player executable around `umak-audio`.
//!
//! By default it reads one JSON command per line from stdin and writes one JSON event
//! per line to stdout, and it exits when stdin closes, so it cannot outlive the app.

use std::io::{self, BufRead, Write};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::Instant;

use umak_audio::engine::{ChainParts, Engine, check_file};
use umak_audio::output::{AudioOutput, CpalOutput, NullOutput};
use umak_audio::protocol::{PlaybackState, PlayerCommand, PlayerEvent, TrackEntry};

const USAGE: &str = "usage: umak-play [options] [file]...

With no files, reads JSON commands from stdin and writes JSON events to stdout.
With files, plays them in order and exits.

  --null-output      discard audio at real-time speed instead of using a device
  --volume N         output gain from 0 to 1
  --check            decode each file fully without playing it, and report";
const NULL_RATE: u32 = 48_000;
const NULL_CHANNELS: u16 = 2;

struct Args {
    null_output: bool,
    volume: Option<f32>,
    check: bool,
    files: Vec<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut parsed = Args {
        null_output: false,
        volume: None,
        check: false,
        files: Vec::new(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--null-output" => parsed.null_output = true,
            "--check" => parsed.check = true,
            "--volume" => {
                let value = args.next().and_then(|value| value.parse().ok());
                parsed.volume = Some(value.ok_or(format!("--volume needs a number\n\n{USAGE}"))?);
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown option {other}\n\n{USAGE}"));
            }
            _ => parsed.files.push(arg),
        }
    }
    Ok(parsed)
}

fn write_event(event: &PlayerEvent) {
    if let Ok(line) = serde_json::to_string(event) {
        let mut stdout = io::stdout().lock();
        // The reader going away is handled by stdin closing.
        let _ = writeln!(stdout, "{line}");
        let _ = stdout.flush();
    }
}

fn make_output(null_output: bool) -> Result<Box<dyn AudioOutput>, String> {
    if null_output {
        Ok(Box::new(NullOutput::new(NULL_RATE, NULL_CHANNELS)))
    } else {
        Ok(Box::new(CpalOutput::default_device()?))
    }
}

/// Decodes files as playback would. One JSON line per file; failures do not stop it.
fn run_check(files: &[String]) -> ExitCode {
    let mut failed = false;
    for file in files {
        let started = Instant::now();
        let result = check_file(file, NULL_RATE);
        let elapsed = started.elapsed().as_millis() as u64;
        let line = match result {
            Ok(check) => serde_json::json!({
                "file": file,
                "ok": true,
                "sampleRate": check.sample_rate,
                "channels": check.channels,
                "frames": check.frames,
                "outputFrames": check.output_frames,
                "decodeErrors": check.decode_errors,
                "durationMs": check.duration_ms,
                "elapsedMs": elapsed,
            }),
            Err(message) => {
                failed = true;
                serde_json::json!({ "file": file, "ok": false, "message": message })
            }
        };
        println!("{line}");
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn play_files(args: &Args) -> ExitCode {
    let (sender, events) = mpsc::channel();
    let null_output = args.null_output;
    let engine = Engine::spawn(
        move || make_output(null_output),
        ChainParts::default(),
        move |event| {
            write_event(&event);
            let _ = sender.send(event);
        },
    );
    if let Some(volume) = args.volume {
        engine.send(PlayerCommand::SetVolume { volume });
    }
    let tracks = args
        .files
        .iter()
        .map(|file| TrackEntry {
            id: file.clone(),
            path: file.clone(),
            gain: Default::default(),
        })
        .collect();
    engine.send(PlayerCommand::Load {
        tracks,
        start_index: 0,
        start_ms: 0,
    });
    for event in events {
        match event {
            PlayerEvent::State {
                state: PlaybackState::Stopped,
                ..
            } => return ExitCode::SUCCESS,
            PlayerEvent::Error { .. } => return ExitCode::FAILURE,
            _ => {}
        }
    }
    ExitCode::FAILURE
}

fn serve(args: &Args) -> ExitCode {
    let null_output = args.null_output;
    let engine = Engine::spawn(
        move || make_output(null_output),
        ChainParts::default(),
        |event| write_event(&event),
    );
    if let Some(volume) = args.volume {
        engine.send(PlayerCommand::SetVolume { volume });
    }
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<PlayerCommand>(&line) {
            Ok(PlayerCommand::Quit) => break,
            Ok(command) => engine.send(command),
            Err(error) => write_event(&PlayerEvent::Error {
                message: format!("bad command: {error}"),
            }),
        }
    }
    engine.send(PlayerCommand::Quit);
    engine.join();
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("umak-play: {message}");
            return ExitCode::FAILURE;
        }
    };
    if args.check {
        run_check(&args.files)
    } else if args.files.is_empty() {
        serve(&args)
    } else {
        play_files(&args)
    }
}
