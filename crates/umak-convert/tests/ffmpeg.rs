//! Runs the real FFmpeg, found as the app would find it, or named by `UMAK_FFMPEG`. Output is
//! read back with Lofty, independently of FFmpeg.

use std::f64::consts::TAU;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use lofty::file::TaggedFile;
use lofty::file::{AudioFile, FileType, TaggedFileExt};
use lofty::mp4::{Mp4Codec, Mp4File};
use lofty::probe::Probe;
use lofty::tag::ItemKey;
use umak_convert::{Conversion, ConvertError, Encoding, Ffmpeg, SampleBits};

fn ffmpeg() -> Ffmpeg {
    Ffmpeg::find().unwrap_or_else(|error| panic!("these tests need FFmpeg: {error}"))
}

/// Writes a sine tone as 16-bit PCM WAV, without FFmpeg.
fn write_wav(path: &Path, rate: u32, channels: u16, seconds: f64) {
    write_wav_bits(path, rate, channels, seconds, 16);
}

/// Writes a sine tone as PCM WAV of 16 or 24 bits, without FFmpeg.
fn write_wav_bits(path: &Path, rate: u32, channels: u16, seconds: f64, bits: u16) {
    let bytes_per_sample = bits / 8;
    let frames = (f64::from(rate) * seconds) as u32;
    let block = channels * bytes_per_sample;
    let data_len = frames * u32::from(block);
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * u32::from(block)).to_le_bytes());
    bytes.extend_from_slice(&block.to_le_bytes());
    bytes.extend_from_slice(&bits.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for frame in 0..frames {
        let value = (f64::from(frame) / f64::from(rate) * 440.0 * TAU).sin() * 0.3;
        let full = f64::from((1u32 << (bits - 1)) - 1);
        let sample = ((value * full) as i32).to_le_bytes();
        for _ in 0..channels {
            bytes.extend_from_slice(&sample[..usize::from(bytes_per_sample)]);
        }
    }
    fs::write(path, bytes).unwrap();
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// FLAC always holds the encoder's name, which Lofty reads as a tag; nothing else may be there.
fn untagged(file: &TaggedFile) -> bool {
    file.tags().iter().all(|tag| {
        tag.pictures().is_empty()
            && tag
                .items()
                .all(|item| item.key() == ItemKey::EncoderSoftware)
    })
}

fn conversion(input: &Path, output: PathBuf, encoding: Encoding) -> Conversion {
    Conversion {
        input: input.to_owned(),
        output,
        encoding,
        sample_rate: None,
        bits: None,
    }
}

#[test]
fn finds_ffmpeg_and_its_encoders() {
    let ffmpeg = ffmpeg();
    assert!(!ffmpeg.version().is_empty());
    for encoder in ["flac", "alac", "libmp3lame", "aac", "libopus"] {
        assert!(ffmpeg.has_encoder(encoder), "no {encoder} encoder");
    }
    assert!(
        !ffmpeg.has_encoder("libx264"),
        "video encoders are not listed"
    );
}

#[test]
fn converts_to_every_encoding() {
    let ffmpeg = ffmpeg();
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("tone.wav");
    write_wav(&input, 44_100, 2, 3.0);
    let cases = [
        (Encoding::Flac, FileType::Flac),
        (Encoding::Alac, FileType::Mp4),
        (Encoding::Mp3Vbr(0), FileType::Mpeg),
        (Encoding::Mp3Cbr(320), FileType::Mpeg),
        (Encoding::Aac(256), FileType::Mp4),
        (Encoding::Opus(160), FileType::Opus),
    ];
    for (index, (encoding, file_type)) in cases.into_iter().enumerate() {
        let output = dir
            .path()
            .join(format!("out{index}.{}", encoding.extension()));
        let mut reports = Vec::new();
        ffmpeg
            .convert(
                &conversion(&input, output.clone(), encoding),
                &AtomicBool::new(false),
                |done| reports.push(done),
                |_| Ok(()),
            )
            .unwrap_or_else(|error| panic!("{encoding:?}: {error}"));

        let tagged = Probe::open(&output).unwrap().read().unwrap();
        assert_eq!(tagged.file_type(), file_type, "{encoding:?}");
        let length = tagged.properties().duration();
        let off = length.abs_diff(Duration::from_secs(3));
        assert!(
            off < Duration::from_millis(60),
            "{encoding:?} lasts {length:?}"
        );
        assert!(untagged(&tagged), "{encoding:?} has tags");
        assert!(
            reports
                .last()
                .is_some_and(|done| *done >= Duration::from_millis(2900)),
            "{encoding:?} reported {reports:?}"
        );
        assert!(reports.is_sorted(), "{encoding:?} reported {reports:?}");

        if encoding == Encoding::Alac || matches!(encoding, Encoding::Aac(_)) {
            let mut file = fs::File::open(&output).unwrap();
            let mp4 = Mp4File::read_from(&mut file, Default::default()).unwrap();
            let expected = if encoding == Encoding::Alac {
                Mp4Codec::ALAC
            } else {
                Mp4Codec::AAC
            };
            assert_eq!(mp4.properties().codec(), Some(expected));
        }
    }
    assert!(
        names_in(dir.path())
            .iter()
            .all(|name| !name.ends_with(".umak-part")),
        "partial files left: {:?}",
        names_in(dir.path())
    );
}

#[test]
fn picks_a_rate_the_encoder_takes() {
    let ffmpeg = ffmpeg();
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("high.wav");
    write_wav(&input, 96_000, 2, 1.0);
    for encoding in [Encoding::Mp3Vbr(2), Encoding::Opus(128)] {
        let output = dir.path().join(format!("high.{}", encoding.extension()));
        ffmpeg
            .convert(
                &conversion(&input, output.clone(), encoding),
                &AtomicBool::new(false),
                |_| {},
                |_| Ok(()),
            )
            .unwrap_or_else(|error| panic!("{encoding:?}: {error}"));
        let rate = Probe::open(&output)
            .unwrap()
            .read()
            .unwrap()
            .properties()
            .sample_rate();
        assert!(
            rate.is_some_and(|rate| rate <= 48_000),
            "{encoding:?} at {rate:?}"
        );
    }
}

#[test]
fn reduces_rate_and_depth_when_asked() {
    let ffmpeg = ffmpeg();
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("hires.wav");
    write_wav_bits(&input, 96_000, 2, 1.0, 24);
    let cases = [
        (Encoding::Flac, None, None, 96_000, 24),
        (
            Encoding::Flac,
            Some(48_000),
            Some(SampleBits::Sixteen),
            48_000,
            16,
        ),
        (Encoding::Alac, None, Some(SampleBits::Sixteen), 96_000, 16),
        (
            Encoding::Alac,
            Some(44_100),
            Some(SampleBits::TwentyFour),
            44_100,
            24,
        ),
        (
            Encoding::Aac(256),
            Some(48_000),
            Some(SampleBits::Sixteen),
            48_000,
            0,
        ),
    ];
    for (index, (encoding, sample_rate, bits, rate, depth)) in cases.into_iter().enumerate() {
        let output = dir
            .path()
            .join(format!("out{index}.{}", encoding.extension()));
        let conversion = Conversion {
            sample_rate,
            bits,
            ..conversion(&input, output.clone(), encoding)
        };
        ffmpeg
            .convert(&conversion, &AtomicBool::new(false), |_| {}, |_| Ok(()))
            .unwrap_or_else(|error| panic!("{index}: {error}"));
        let tagged = Probe::open(&output).unwrap().read().unwrap();
        let properties = tagged.properties();
        assert_eq!(properties.sample_rate(), Some(rate), "{index}");
        if depth > 0 {
            assert_eq!(properties.bit_depth(), Some(depth), "{index}");
        }
        let off = properties.duration().abs_diff(Duration::from_secs(1));
        assert!(
            off < Duration::from_millis(30),
            "{index} lasts {:?}",
            properties.duration()
        );
    }
}

#[test]
fn cancel_ends_ffmpeg_and_leaves_nothing() {
    let ffmpeg = ffmpeg();
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("long.wav");
    write_wav(&input, 44_100, 2, 300.0);
    let output = dir.path().join("long.mp3");
    let cancel = AtomicBool::new(false);
    let mut reports = 0;
    let started = Instant::now();
    let result = ffmpeg.convert(
        &conversion(&input, output.clone(), Encoding::Mp3Vbr(0)),
        &cancel,
        |_| {
            reports += 1;
            cancel.store(true, Ordering::Relaxed);
        },
        |_| Ok(()),
    );
    assert!(matches!(result, Err(ConvertError::Canceled)), "{result:?}");
    assert_eq!(reports, 1, "it stopped at the first report");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(names_in(dir.path()), ["long.wav"]);
}

#[test]
fn a_failure_reports_ffmpeg_and_leaves_nothing() {
    let ffmpeg = ffmpeg();
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("not audio.wav");
    fs::write(&input, "this is not audio").unwrap();
    let result = ffmpeg.convert(
        &conversion(&input, dir.path().join("out.flac"), Encoding::Flac),
        &AtomicBool::new(false),
        |_| {},
        |_| Ok(()),
    );
    match result {
        Err(ConvertError::Failed { message, .. }) => assert!(!message.is_empty()),
        other => panic!("{other:?}"),
    }
    assert_eq!(names_in(dir.path()), ["not audio.wav"]);
}

#[test]
fn a_failed_finish_leaves_nothing() {
    let ffmpeg = ffmpeg();
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("tone.wav");
    write_wav(&input, 44_100, 2, 0.5);
    let mut handed = None;
    let result = ffmpeg.convert(
        &conversion(&input, dir.path().join("out.flac"), Encoding::Flac),
        &AtomicBool::new(false),
        |_| {},
        |partial| {
            handed = Some(partial.to_owned());
            assert!(partial.is_file(), "the output is there to finish");
            Err("no tags for you".to_owned())
        },
    );
    assert!(matches!(&result, Err(ConvertError::Finish(message)) if message == "no tags for you"));
    assert!(handed.is_some_and(|path| path.ends_with(".out.flac.umak-part")));
    assert_eq!(names_in(dir.path()), ["tone.wav"]);
}

#[test]
fn never_writes_over_a_file() {
    let ffmpeg = ffmpeg();
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("tone.wav");
    write_wav(&input, 44_100, 1, 0.5);
    let before = fs::read(&input).unwrap();
    for output in [input.clone(), dir.path().join("taken.flac")] {
        fs::write(dir.path().join("taken.flac"), "keep me").unwrap();
        let result = ffmpeg.convert(
            &conversion(&input, output.clone(), Encoding::Flac),
            &AtomicBool::new(false),
            |_| {},
            |_| Ok(()),
        );
        assert!(
            matches!(result, Err(ConvertError::OutputExists(_))),
            "{result:?}"
        );
    }
    assert_eq!(fs::read(&input).unwrap(), before);
    assert_eq!(
        fs::read_to_string(dir.path().join("taken.flac")).unwrap(),
        "keep me"
    );
}

#[test]
fn keeps_only_the_first_audio_stream_without_art() {
    let ffmpeg = ffmpeg();
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("tone.wav");
    write_wav(&input, 44_100, 2, 1.0);
    // A FLAC with embedded art and tags, made by FFmpeg itself as a source.
    let art = dir.path().join("cover.png");
    let status = std::process::Command::new(ffmpeg.path())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=8x8",
        ])
        .args(["-frames:v", "1"])
        .arg(&art)
        .status()
        .unwrap();
    assert!(status.success());
    let source = dir.path().join("tagged.flac");
    let status = std::process::Command::new(ffmpeg.path())
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&input)
        .arg("-i")
        .arg(&art)
        .args(["-map", "0:a", "-map", "1", "-c:a", "flac", "-c:v", "copy"])
        .args(["-disposition:v", "attached_pic", "-metadata", "title=Tone"])
        .arg(&source)
        .status()
        .unwrap();
    assert!(status.success());
    let tagged = Probe::open(&source).unwrap().read().unwrap();
    assert!(
        !tagged.primary_tag().unwrap().pictures().is_empty(),
        "the source has art"
    );

    let output = dir.path().join("plain.flac");
    ffmpeg
        .convert(
            &conversion(&source, output.clone(), Encoding::Flac),
            &AtomicBool::new(false),
            |_| {},
            |_| Ok(()),
        )
        .unwrap();
    let plain = Probe::open(&output).unwrap().read().unwrap();
    assert!(untagged(&plain));
}
