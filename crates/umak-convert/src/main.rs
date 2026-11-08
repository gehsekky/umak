//! Converts one file through FFmpeg, to try the converter without the app.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use umak_convert::{Conversion, Encoding, Ffmpeg};

const USAGE: &str = "usage: umak-convert info
       umak-convert <input> <output> <encoding>

Encodings: flac, alac, mp3-v0 to mp3-v9, mp3-<kbps>, aac-<kbps>, opus-<kbps>.
Set UMAK_FFMPEG to use a particular FFmpeg.";

fn parse_encoding(text: &str) -> Option<Encoding> {
    let (name, setting) = text.split_once('-').unwrap_or((text, ""));
    let number = || setting.parse::<u32>().ok().filter(|kbps| *kbps > 0);
    match name {
        "flac" if setting.is_empty() => Some(Encoding::Flac),
        "alac" if setting.is_empty() => Some(Encoding::Alac),
        "mp3" => match setting.strip_prefix('v') {
            Some(quality) => quality
                .parse()
                .ok()
                .filter(|q| *q <= 9)
                .map(Encoding::Mp3Vbr),
            None => number().map(Encoding::Mp3Cbr),
        },
        "aac" => number().map(Encoding::Aac),
        "opus" => number().map(Encoding::Opus),
        _ => None,
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let ffmpeg = match Ffmpeg::find() {
        Ok(ffmpeg) => ffmpeg,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    match args.as_slice() {
        [info] if info == "info" => {
            println!("{} at {}", ffmpeg.version(), ffmpeg.path().display());
            for encoder in ["flac", "alac", "libmp3lame", "aac", "libopus"] {
                let found = if ffmpeg.has_encoder(encoder) {
                    "yes"
                } else {
                    "no"
                };
                println!("  {encoder}: {found}");
            }
            ExitCode::SUCCESS
        }
        [input, output, encoding] => {
            let Some(encoding) = parse_encoding(encoding) else {
                eprintln!("{USAGE}");
                return ExitCode::FAILURE;
            };
            let conversion = Conversion {
                input: PathBuf::from(input),
                output: PathBuf::from(output),
                encoding,
                sample_rate: None,
                bits: None,
            };
            let started = std::time::Instant::now();
            let mut last = None;
            let result = ffmpeg.convert(
                &conversion,
                &AtomicBool::new(false),
                |done| last = Some(done),
                |_| Ok(()),
            );
            match result {
                Ok(()) => {
                    let audio = last.unwrap_or_default().as_secs_f64();
                    let took = started.elapsed().as_secs_f64();
                    println!("converted {audio:.1} s of audio in {took:.2} s");
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("{error}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_encodings() {
        assert_eq!(parse_encoding("flac"), Some(Encoding::Flac));
        assert_eq!(parse_encoding("mp3-v0"), Some(Encoding::Mp3Vbr(0)));
        assert_eq!(parse_encoding("mp3-320"), Some(Encoding::Mp3Cbr(320)));
        assert_eq!(parse_encoding("aac-256"), Some(Encoding::Aac(256)));
        assert_eq!(parse_encoding("opus-160"), Some(Encoding::Opus(160)));
        for wrong in ["mp3-v10", "aac", "aac-0", "flac-5", "wav", "opus-x"] {
            assert_eq!(parse_encoding(wrong), None, "{wrong}");
        }
    }
}
