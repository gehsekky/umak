//! Converts audio by running FFmpeg as a separate process, so a crash or stall in an encoder
//! never reaches the app, and canceling only has to end that process.
//!
//! This crate must never depend on Tauri or UI code.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

/// Names the FFmpeg to use instead of searching for one.
pub const FFMPEG_ENV: &str = "UMAK_FFMPEG";

#[cfg(windows)]
const FFMPEG_FILE: &str = "ffmpeg.exe";
#[cfg(not(windows))]
const FFMPEG_FILE: &str = "ffmpeg";

/// How often FFmpeg reports progress, in seconds.
const REPORT_PERIOD: &str = "0.25";
/// How long a cancel can wait before it is noticed.
const CANCEL_POLL: Duration = Duration::from_millis(50);
/// How much of FFmpeg's error output a failure keeps, from the end.
const ERROR_TEXT_LIMIT: usize = 4096;
/// Marks output still being written, beside where it will end up.
const PARTIAL_SUFFIX: &str = ".umak-part";

#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
    #[error("FFmpeg was not found; install it or set {FFMPEG_ENV} to its path")]
    NotFound,
    #[error("{path} did not run as FFmpeg: {reason}")]
    NotFfmpeg { path: PathBuf, reason: String },
    #[error("this FFmpeg has no {0} encoder")]
    MissingEncoder(&'static str),
    #[error("{0} already exists")]
    OutputExists(PathBuf),
    #[error("the conversion was canceled")]
    Canceled,
    #[error("FFmpeg failed ({status}): {message}")]
    Failed { status: String, message: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// What to encode to. Bitrates are in kbps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Flac,
    Alac,
    /// LAME's variable bitrate quality, from 0 (best) to 9.
    Mp3Vbr(u8),
    Mp3Cbr(u32),
    Aac(u32),
    Opus(u32),
}

impl Encoding {
    /// FFmpeg's name for the encoder.
    pub fn encoder(self) -> &'static str {
        match self {
            Self::Flac => "flac",
            Self::Alac => "alac",
            Self::Mp3Vbr(_) | Self::Mp3Cbr(_) => "libmp3lame",
            Self::Aac(_) => "aac",
            Self::Opus(_) => "libopus",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Flac => "flac",
            Self::Alac | Self::Aac(_) => "m4a",
            Self::Mp3Vbr(_) | Self::Mp3Cbr(_) => "mp3",
            Self::Opus(_) => "opus",
        }
    }

    /// FFmpeg's name for the container, given outright because the output's name while it is
    /// written does not end in the extension.
    fn muxer(self) -> &'static str {
        match self {
            Self::Flac => "flac",
            Self::Alac | Self::Aac(_) => "ipod",
            Self::Mp3Vbr(_) | Self::Mp3Cbr(_) => "mp3",
            Self::Opus(_) => "opus",
        }
    }

    fn args(self) -> Vec<String> {
        let mut args = vec!["-c:a".to_owned(), self.encoder().to_owned()];
        match self {
            Self::Flac | Self::Alac => {}
            Self::Mp3Vbr(quality) => args.extend(["-q:a".to_owned(), quality.min(9).to_string()]),
            Self::Mp3Cbr(kbps) | Self::Aac(kbps) | Self::Opus(kbps) => {
                args.extend(["-b:a".to_owned(), format!("{kbps}k")])
            }
        }
        if matches!(self, Self::Mp3Vbr(_) | Self::Mp3Cbr(_)) {
            // Tags are written afterwards, by the same writer as every other tag edit.
            args.extend(["-id3v2_version", "0", "-write_id3v1", "0"].map(str::to_owned));
        }
        args
    }
}

/// One file to convert. The output must not exist yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversion {
    pub input: PathBuf,
    pub output: PathBuf,
    pub encoding: Encoding,
}

/// An FFmpeg executable that has answered as one.
#[derive(Debug, Clone)]
pub struct Ffmpeg {
    path: PathBuf,
    version: String,
    encoders: BTreeSet<String>,
}

impl Ffmpeg {
    /// Looks in `UMAK_FFMPEG`, then beside this executable, where the app bundles it, then on
    /// the path.
    pub fn find() -> Result<Self, ConvertError> {
        if let Some(path) = std::env::var_os(FFMPEG_ENV) {
            return Self::at(PathBuf::from(path));
        }
        let beside = std::env::current_exe()
            .ok()
            .and_then(|exe| Some(exe.parent()?.join(FFMPEG_FILE)));
        let on_path: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|paths| {
                std::env::split_paths(&paths)
                    .map(|dir| dir.join(FFMPEG_FILE))
                    .collect()
            })
            .unwrap_or_default();
        beside
            .into_iter()
            .chain(on_path)
            .find(|path| path.is_file())
            .ok_or(ConvertError::NotFound)
            .and_then(Self::at)
    }

    /// Runs the executable at `path` to learn its version and encoders.
    pub fn at(path: PathBuf) -> Result<Self, ConvertError> {
        let not_ffmpeg = |reason: String| ConvertError::NotFfmpeg {
            path: path.clone(),
            reason,
        };
        let ask = |arg: &str| -> Result<String, ConvertError> {
            let output = hidden(&path)
                .args(["-hide_banner", arg])
                .output()
                .map_err(|error| not_ffmpeg(error.to_string()))?;
            if !output.status.success() {
                return Err(not_ffmpeg(format!("{arg} ended with {}", output.status)));
            }
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        };
        let version = parse_version(&ask("-version")?)
            .ok_or_else(|| not_ffmpeg("no version in its output".to_owned()))?;
        let encoders = parse_audio_encoders(&ask("-encoders")?);
        Ok(Self {
            path,
            version,
            encoders,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn has_encoder(&self, name: &str) -> bool {
        self.encoders.contains(name)
    }

    /// Converts one file, calling `progress` with how much of the audio is done. Output is
    /// written beside its final name and moved there only when FFmpeg succeeds, so a cancel
    /// or failure leaves nothing behind. Only the first audio stream is kept, without tags,
    /// chapters or art.
    pub fn convert(
        &self,
        conversion: &Conversion,
        cancel: &AtomicBool,
        mut progress: impl FnMut(Duration),
    ) -> Result<(), ConvertError> {
        let encoding = conversion.encoding;
        if !self.has_encoder(encoding.encoder()) {
            return Err(ConvertError::MissingEncoder(encoding.encoder()));
        }
        let output = &conversion.output;
        if exists(output) {
            return Err(ConvertError::OutputExists(output.clone()));
        }

        // Declared before the process, so the file is removed after the process has ended
        // and let go of it.
        let partial = Partial(Some(partial_path(output)));
        let mut command = hidden(&self.path);
        command
            .args(["-hide_banner", "-nostdin", "-nostats", "-loglevel", "error"])
            .args(["-progress", "pipe:1", "-stats_period", REPORT_PERIOD])
            .arg("-i")
            .arg(file_url(&conversion.input))
            .args([
                "-map",
                "0:a:0",
                "-map_metadata",
                "-1",
                "-map_chapters",
                "-1",
            ])
            // Leaves out the encoder's name, which would otherwise be written as a tag.
            .args(["-fflags", "+bitexact"])
            .args(encoding.args())
            .args(["-f", encoding.muxer(), "-y"])
            .arg(file_url(partial.path()))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut process = Process(command.spawn()?);
        tie_to_this_process(&process.0);

        let stdout = process.0.stdout.take().expect("stdout is piped");
        let stderr = process.0.stderr.take().expect("stderr is piped");
        let (sender, reports) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some(done) = parse_out_time(&line)
                    && sender.send(done).is_err()
                {
                    break;
                }
            }
        });
        // Read to the end even when long, so FFmpeg never waits on a full pipe.
        let errors = thread::spawn(move || {
            let mut text = Vec::new();
            let _ = BufReader::new(stderr).read_to_end(&mut text);
            let start = text.len().saturating_sub(ERROR_TEXT_LIMIT);
            String::from_utf8_lossy(&text[start..]).trim().to_owned()
        });

        loop {
            if cancel.load(Ordering::Relaxed) {
                process.end();
                return Err(ConvertError::Canceled);
            }
            match reports.recv_timeout(CANCEL_POLL) {
                Ok(done) => progress(done),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        let status = process.0.wait()?;
        let message = errors.join().unwrap_or_default();
        if !status.success() {
            return Err(ConvertError::Failed {
                status: status.to_string(),
                message,
            });
        }
        // Checked again, since something else may have written there meanwhile, and a move
        // would replace it.
        if exists(output) {
            return Err(ConvertError::OutputExists(output.clone()));
        }
        fs::rename(partial.path(), output)?;
        partial.keep();
        Ok(())
    }
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn partial_path(output: &Path) -> PathBuf {
    let mut name = OsString::from(".");
    name.push(output.file_name().unwrap_or_default());
    name.push(PARTIAL_SUFFIX);
    output.with_file_name(name)
}

/// Names a file so FFmpeg never reads a colon in it as a protocol.
fn file_url(path: &Path) -> OsString {
    let mut url = OsString::from("file:");
    url.push(path);
    url
}

/// A command whose process opens no console window.
fn hidden(program: &Path) -> Command {
    let mut command = Command::new(program);
    command.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Ends the process if it is still running when dropped.
struct Process(Child);

impl Process {
    fn end(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            self.end();
        }
    }
}

/// Output being written, removed when dropped unless kept.
struct Partial(Option<PathBuf>);

impl Partial {
    fn path(&self) -> &Path {
        self.0.as_deref().expect("not kept yet")
    }

    fn keep(mut self) {
        self.0 = None;
    }
}

impl Drop for Partial {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

/// Makes Windows end the process when this one ends, even if it crashes, so a conversion
/// never runs on with nobody to finish it.
#[cfg(windows)]
fn tie_to_this_process(child: &Child) {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    // The handle is kept open for the life of this process; closing it is what ends the
    // children. Stored as a number, since a raw handle cannot be shared between threads.
    static JOB: OnceLock<Option<usize>> = OnceLock::new();
    let job = JOB.get_or_init(|| {
        // SAFETY: creates an unnamed job object and sets a plain limit on it, with a correctly
        // sized structure.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            (set != 0).then_some(job as usize)
        }
    });
    if let Some(job) = *job {
        // SAFETY: both handles are open; the child's is owned by `child`.
        unsafe { AssignProcessToJobObject(job as _, child.as_raw_handle() as _) };
    }
}

#[cfg(not(windows))]
fn tie_to_this_process(_child: &Child) {}

/// Reads the version from the first line of `ffmpeg -version`.
fn parse_version(text: &str) -> Option<String> {
    let rest = text.lines().next()?.strip_prefix("ffmpeg version ")?;
    rest.split_whitespace().next().map(str::to_owned)
}

/// Reads the audio encoders from `ffmpeg -encoders`, which lists one per line after a row of
/// dashes, each as six flag letters and then the name. The first flag is `A` for audio.
fn parse_audio_encoders(text: &str) -> BTreeSet<String> {
    text.lines()
        .skip_while(|line| !line.trim_start().starts_with("---"))
        .skip(1)
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let flags = words.next()?;
            let name = words.next()?;
            (flags.len() == 6 && flags.starts_with('A')).then(|| name.to_owned())
        })
        .collect()
}

/// Reads how much audio is done from a line of FFmpeg's progress output.
fn parse_out_time(line: &str) -> Option<Duration> {
    let micros = line
        .strip_prefix("out_time_us=")?
        .trim()
        .parse::<u64>()
        .ok()?;
    Some(Duration::from_micros(micros))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_version() {
        let text =
            "ffmpeg version 9.0.2-full_build-www.gyan.dev Copyright (c) 2000-2026\nbuilt with gcc";
        assert_eq!(
            parse_version(text).as_deref(),
            Some("9.0.2-full_build-www.gyan.dev")
        );
        assert_eq!(parse_version("ffprobe version 9.0"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn reads_only_audio_encoders() {
        let text = "Encoders:
 V..... = Video
 A..... = Audio
 ------
 V....D libx264              libx264 H.264
 A....D aac                  AAC (Advanced Audio Coding)
 A....D libmp3lame           libmp3lame MP3 (MPEG audio layer 3) (codec mp3)
 S..... ass                  ASS (Advanced SSA) subtitle
";
        let found: Vec<_> = parse_audio_encoders(text).into_iter().collect();
        assert_eq!(found, ["aac", "libmp3lame"]);
    }

    #[test]
    fn reads_progress_and_skips_the_rest() {
        assert_eq!(
            parse_out_time("out_time_us=1500000"),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(parse_out_time("out_time_us=N/A"), None);
        assert_eq!(parse_out_time("out_time_ms=1500000"), None);
        assert_eq!(parse_out_time("progress=end"), None);
    }

    #[test]
    fn writes_partial_output_beside_the_final_name() {
        let output = Path::new("out").join("Song.flac");
        assert_eq!(
            partial_path(&output),
            Path::new("out").join(".Song.flac.umak-part")
        );
    }

    #[test]
    fn names_files_so_colons_are_not_protocols() {
        assert_eq!(
            file_url(Path::new("a:b.mp3")),
            OsString::from("file:a:b.mp3")
        );
    }

    #[test]
    fn asks_each_encoder_for_its_quality() {
        assert_eq!(
            Encoding::Mp3Vbr(0).args()[..4],
            ["-c:a", "libmp3lame", "-q:a", "0"]
        );
        assert_eq!(Encoding::Mp3Vbr(12).args()[3], "9");
        assert_eq!(Encoding::Aac(256).args(), ["-c:a", "aac", "-b:a", "256k"]);
        assert_eq!(
            Encoding::Opus(160).args(),
            ["-c:a", "libopus", "-b:a", "160k"]
        );
        assert_eq!(Encoding::Flac.args(), ["-c:a", "flac"]);
        assert!(
            Encoding::Mp3Cbr(320)
                .args()
                .contains(&"-id3v2_version".to_owned())
        );
    }

    #[test]
    fn refuses_an_encoder_it_does_not_have() {
        let ffmpeg = Ffmpeg {
            path: PathBuf::from("ffmpeg-that-is-never-run"),
            version: "0".to_owned(),
            encoders: BTreeSet::from(["flac".to_owned()]),
        };
        let conversion = Conversion {
            input: PathBuf::from("in.wav"),
            output: PathBuf::from("out.opus"),
            encoding: Encoding::Opus(160),
        };
        let result = ffmpeg.convert(&conversion, &AtomicBool::new(false), |_| {});
        assert!(matches!(
            result,
            Err(ConvertError::MissingEncoder("libopus"))
        ));
    }
}
