//! Conversion settings, the built-in presets, and where each converted file goes. Output
//! mirrors each file's folders under its library root, inside a folder the user chooses, and
//! never replaces a file that is already there.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use umak_convert::{Conversion, Encoding, SampleBits};

use crate::api::{ApiError, ErrorCode};

/// Bitrates LAME can write at a constant rate.
const MP3_BITRATES: [u32; 14] = [
    32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
];
const AAC_BITRATES: std::ops::RangeInclusive<u32> = 32..=512;
const OPUS_BITRATES: std::ops::RangeInclusive<u32> = 6..=510;
/// Rates a file can be brought down to.
const SAMPLE_RATES: [u32; 7] = [32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000];
/// How many problem files a plan lists; the counts cover them all.
const LISTED_PROBLEMS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ConvertCodec {
    Flac,
    Alac,
    Mp3,
    Aac,
    Opus,
}

impl ConvertCodec {
    fn label(self) -> &'static str {
        match self {
            Self::Flac => "FLAC",
            Self::Alac => "ALAC",
            Self::Mp3 => "MP3",
            Self::Aac => "AAC",
            Self::Opus => "Opus",
        }
    }

    fn is_lossless(self) -> bool {
        matches!(self, Self::Flac | Self::Alac)
    }

    /// The highest rate the format holds. Opus always runs at 48 kHz inside, whatever it is
    /// given, so it takes no limit.
    fn rate_limit(self) -> Option<u32> {
        match self {
            Self::Mp3 => Some(48_000),
            Self::Aac => Some(96_000),
            Self::Flac | Self::Alac | Self::Opus => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConvertSettings {
    pub codec: ConvertCodec,
    /// MP3 only: LAME's variable bitrate quality, from 0 (best) to 9. MP3 takes this or a
    /// bitrate, not both.
    pub mp3_quality: Option<u8>,
    /// In kbps: constant for MP3, a target for AAC and Opus. Lossless formats take none.
    pub bitrate_kbps: Option<u32>,
    /// Files above this rate are brought down to it, or to a lower rate that divides theirs
    /// evenly, so 176.4 kHz goes to 88.2 or 44.1 kHz rather than 48. Lower rates are left alone.
    pub max_sample_rate: Option<u32>,
    /// Lossless output only: files deeper than this, 16 or 24 bits, are reduced to it with
    /// dither. Shallower files are left alone.
    pub max_bit_depth: Option<u8>,
}

impl ConvertSettings {
    pub fn check(&self) -> Result<(), ApiError> {
        let invalid = |message: String| Err(ApiError::new(ErrorCode::InvalidArgs, message));
        let codec = self.codec;
        let label = codec.label();
        match (codec, self.mp3_quality, self.bitrate_kbps) {
            (ConvertCodec::Flac | ConvertCodec::Alac, None, None) => {}
            (ConvertCodec::Flac | ConvertCodec::Alac, _, _) => {
                return invalid(format!(
                    "{label} is lossless and takes no bitrate or quality"
                ));
            }
            (ConvertCodec::Mp3, Some(quality), None) if quality <= 9 => {}
            (ConvertCodec::Mp3, None, Some(kbps)) if MP3_BITRATES.contains(&kbps) => {}
            (ConvertCodec::Mp3, _, _) => {
                return invalid(
                    "MP3 needs a quality from 0 to 9, or one of LAME's bitrates from 32 to 320 kbps"
                        .to_owned(),
                );
            }
            (ConvertCodec::Aac, None, Some(kbps)) if AAC_BITRATES.contains(&kbps) => {}
            (ConvertCodec::Opus, None, Some(kbps)) if OPUS_BITRATES.contains(&kbps) => {}
            (ConvertCodec::Aac | ConvertCodec::Opus, _, _) => {
                let range = if codec == ConvertCodec::Aac {
                    AAC_BITRATES
                } else {
                    OPUS_BITRATES
                };
                return invalid(format!(
                    "{label} needs a bitrate from {} to {} kbps",
                    range.start(),
                    range.end()
                ));
            }
        }
        if let Some(rate) = self.max_sample_rate {
            if codec == ConvertCodec::Opus {
                return invalid("Opus always plays at 48 kHz and takes no sample rate".to_owned());
            }
            if !SAMPLE_RATES.contains(&rate) {
                return invalid(format!("{rate} Hz is not a sample rate to convert to"));
            }
            if let Some(limit) = codec.rate_limit()
                && rate > limit
            {
                return invalid(format!("{label} holds at most {limit} Hz"));
            }
        }
        match (self.max_bit_depth, codec.is_lossless()) {
            (None, _) | (Some(16 | 24), true) => Ok(()),
            (Some(_), true) => invalid("the bit depth must be 16 or 24".to_owned()),
            (Some(_), false) => invalid(format!("{label} sets its own bit depth")),
        }
    }

    fn encoding(&self) -> Encoding {
        match self.codec {
            ConvertCodec::Flac => Encoding::Flac,
            ConvertCodec::Alac => Encoding::Alac,
            ConvertCodec::Mp3 => match self.mp3_quality {
                Some(quality) => Encoding::Mp3Vbr(quality),
                None => Encoding::Mp3Cbr(self.bitrate_kbps.unwrap_or(320)),
            },
            ConvertCodec::Aac => Encoding::Aac(self.bitrate_kbps.unwrap_or(256)),
            ConvertCodec::Opus => Encoding::Opus(self.bitrate_kbps.unwrap_or(160)),
        }
    }

    pub fn extension(&self) -> &'static str {
        self.encoding().extension()
    }

    /// FFmpeg's name for the encoder these settings need.
    pub fn encoder(&self) -> &'static str {
        self.encoding().encoder()
    }

    /// The rate to resample a file to, or `None` to keep its own. A file whose rate the
    /// library does not know is left to FFmpeg, which picks one the encoder takes.
    fn output_rate(&self, source: Option<u32>) -> Option<u32> {
        let limit = match (self.max_sample_rate, self.codec.rate_limit()) {
            (Some(chosen), Some(format)) => chosen.min(format),
            (chosen, format) => chosen.or(format)?,
        };
        let source = source?;
        (source > limit).then(|| rate_at_most(source, limit))
    }

    /// The depth to reduce a lossless file to, or `None` to keep its own. A lossy source has
    /// no depth of its own, and comes out at 16 bits.
    fn output_bits(&self, source_bits: Option<u32>, source_lossy: bool) -> Option<SampleBits> {
        if !self.codec.is_lossless() {
            return None;
        }
        if source_lossy {
            return Some(SampleBits::Sixteen);
        }
        match (self.max_bit_depth, source_bits) {
            (Some(16), Some(bits)) if bits > 16 => Some(SampleBits::Sixteen),
            (Some(24), Some(bits)) if bits > 24 => Some(SampleBits::TwentyFour),
            _ => None,
        }
    }
}

/// The highest rate at or under `limit` that divides `source` by a power of two, so that
/// resampling keeps a whole-number ratio; `limit` itself when there is none.
fn rate_at_most(source: u32, limit: u32) -> u32 {
    let mut rate = source;
    while rate > limit && rate.is_multiple_of(2) && SAMPLE_RATES.contains(&(rate / 2)) {
        rate /= 2;
    }
    if rate <= limit { rate } else { limit }
}

fn is_lossy(codec: Option<&str>) -> bool {
    matches!(codec, Some("mp3" | "aac" | "opus" | "vorbis"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ConvertPreset {
    pub id: &'static str,
    pub name: &'static str,
    pub settings: ConvertSettings,
}

pub fn presets() -> Vec<ConvertPreset> {
    let settings = |codec, mp3_quality, bitrate_kbps| ConvertSettings {
        codec,
        mp3_quality,
        bitrate_kbps,
        max_sample_rate: None,
        max_bit_depth: None,
    };
    vec![
        ConvertPreset {
            id: "flac",
            name: "FLAC",
            settings: settings(ConvertCodec::Flac, None, None),
        },
        ConvertPreset {
            id: "mp3-v0",
            name: "MP3 V0",
            settings: settings(ConvertCodec::Mp3, Some(0), None),
        },
        ConvertPreset {
            id: "mp3-320",
            name: "MP3 320 kbps",
            settings: settings(ConvertCodec::Mp3, None, Some(320)),
        },
        ConvertPreset {
            id: "aac-256",
            name: "AAC 256 kbps",
            settings: settings(ConvertCodec::Aac, None, Some(256)),
        },
        ConvertPreset {
            id: "opus-160",
            name: "Opus 160 kbps",
            settings: settings(ConvertCodec::Opus, None, Some(160)),
        },
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ConvertFileStatus {
    Ready,
    /// A file is already where the output would go. It is left alone, so converting again
    /// into the same folder carries on where an earlier batch stopped.
    Exists,
    /// Another file in the batch would go to the same place, so neither is converted.
    SameOutput,
    /// The source file is missing from disk.
    Missing,
}

/// One file of a batch, ready to hand to the converter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile {
    pub track: i64,
    pub track_id: String,
    pub rel_path: String,
    pub status: ConvertFileStatus,
    pub lossy_source: bool,
    pub conversion: Conversion,
}

/// Where each track goes and whether it can be converted, in the order given.
pub fn plan_files(
    conn: &Connection,
    track_ids: &[String],
    settings: &ConvertSettings,
    folder: &str,
) -> Result<Vec<PlannedFile>, ApiError> {
    settings.check()?;
    let invalid = |message: String| ApiError::new(ErrorCode::InvalidArgs, message);
    if track_ids.is_empty() {
        return Err(invalid("no tracks to convert".to_owned()));
    }
    let folder = output_folder(folder)?;
    let encoding = settings.encoding();
    let mut lookup = conn.prepare_cached(
        "SELECT t.id, r.path, t.rel_path, t.missing_since IS NOT NULL, t.codec, t.sample_rate,
                t.bit_depth
         FROM tracks t JOIN library_roots r ON r.id = t.root_id WHERE t.public_id = ?1",
    )?;
    let mut seen = HashSet::new();
    let mut existing = ExistingFiles::default();
    let mut files = Vec::with_capacity(track_ids.len());
    for id in track_ids {
        if !seen.insert(id.as_str()) {
            return Err(invalid(format!("track {id} was given twice")));
        }
        type Row = (
            i64,
            String,
            String,
            bool,
            Option<String>,
            Option<u32>,
            Option<u32>,
        );
        let (track, root, rel_path, missing, codec, rate, bits): Row = lookup
            .query_row([id], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            })
            .optional()?
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("no track {id}")))?;
        let output = output_path(&folder, &rel_path, encoding.extension());
        let lossy_source = is_lossy(codec.as_deref());
        let status = if missing {
            ConvertFileStatus::Missing
        } else if existing.contains(&output) {
            ConvertFileStatus::Exists
        } else {
            ConvertFileStatus::Ready
        };
        files.push(PlannedFile {
            track,
            track_id: id.clone(),
            status,
            lossy_source,
            conversion: Conversion {
                input: rel_path
                    .split('/')
                    .fold(PathBuf::from(root), |path, part| path.join(part)),
                output,
                encoding,
                sample_rate: settings.output_rate(rate),
                bits: settings.output_bits(bits, lossy_source),
            },
            rel_path,
        });
    }

    // Windows and macOS treat names that differ only in case as the same file.
    let mut taken: HashMap<String, usize> = HashMap::new();
    for file in &files {
        *taken
            .entry(same_file_key(&file.conversion.output))
            .or_default() += 1;
    }
    for file in &mut files {
        if file.status == ConvertFileStatus::Ready
            && taken[&same_file_key(&file.conversion.output)] > 1
        {
            file.status = ConvertFileStatus::SameOutput;
        }
    }
    Ok(files)
}

fn same_file_key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

/// The names in each output folder, read once per folder, since asking about each file on
/// its own takes about a second for a whole library.
#[derive(Default)]
struct ExistingFiles {
    folders: HashMap<PathBuf, HashSet<String>>,
}

impl ExistingFiles {
    fn contains(&mut self, path: &Path) -> bool {
        let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else {
            return false;
        };
        let names = self.folders.entry(folder.to_owned()).or_insert_with(|| {
            // A folder that cannot be read, usually because it does not exist yet, holds
            // nothing in the way.
            fs::read_dir(folder)
                .map(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .map(|entry| entry.file_name().to_string_lossy().to_lowercase())
                        .collect()
                })
                .unwrap_or_default()
        });
        names.contains(&name.to_string_lossy().to_lowercase())
    }
}

/// The chosen folder, which need not exist yet.
fn output_folder(folder: &str) -> Result<PathBuf, ApiError> {
    let path = Path::new(folder);
    if !path.is_absolute() {
        return Err(ApiError::new(
            ErrorCode::InvalidArgs,
            format!("not an absolute path: {folder}"),
        ));
    }
    Ok(dunce::canonicalize(path).unwrap_or_else(|_| path.to_owned()))
}

fn output_path(folder: &Path, rel_path: &str, extension: &str) -> PathBuf {
    let mut path = rel_path
        .split('/')
        .fold(folder.to_owned(), |path, part| path.join(part));
    path.set_extension(extension);
    path
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ConvertProblem {
    pub track_id: String,
    /// The source, under its library root.
    pub rel_path: String,
    pub output_path: String,
    pub status: ConvertFileStatus,
}

/// What a batch would do, shown before it starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ConvertPlan {
    pub ready: u32,
    pub exists: u32,
    pub same_output: u32,
    pub missing: u32,
    /// Lossy sources among the ready files. Encoding them again loses more of the audio, or,
    /// to a lossless format, only makes them larger.
    pub lossy_sources: u32,
    pub extension: String,
    /// Where the first ready file goes, to show what the layout looks like.
    pub example_output: Option<String>,
    /// The files that will not be converted, up to a limit.
    pub problems: Vec<ConvertProblem>,
}

pub fn summarize(files: &[PlannedFile], settings: &ConvertSettings) -> ConvertPlan {
    let count = |status| files.iter().filter(|file| file.status == status).count() as u32;
    let ready = || {
        files
            .iter()
            .filter(|file| file.status == ConvertFileStatus::Ready)
    };
    ConvertPlan {
        ready: count(ConvertFileStatus::Ready),
        exists: count(ConvertFileStatus::Exists),
        same_output: count(ConvertFileStatus::SameOutput),
        missing: count(ConvertFileStatus::Missing),
        lossy_sources: ready().filter(|file| file.lossy_source).count() as u32,
        extension: settings.extension().to_owned(),
        example_output: ready()
            .next()
            .map(|file| file.conversion.output.to_string_lossy().into_owned()),
        problems: files
            .iter()
            .filter(|file| file.status != ConvertFileStatus::Ready)
            .take(LISTED_PROBLEMS)
            .map(|file| ConvertProblem {
                track_id: file.track_id.clone(),
                rel_path: file.rel_path.clone(),
                output_path: file.conversion.output.to_string_lossy().into_owned(),
                status: file.status,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::params;

    use super::*;
    use crate::db::{Database, DbError};

    fn settings(codec: ConvertCodec) -> ConvertSettings {
        ConvertSettings {
            codec,
            mp3_quality: None,
            bitrate_kbps: None,
            max_sample_rate: None,
            max_bit_depth: None,
        }
    }

    fn mp3_v0() -> ConvertSettings {
        ConvertSettings {
            mp3_quality: Some(0),
            ..settings(ConvertCodec::Mp3)
        }
    }

    #[test]
    fn every_preset_is_valid() {
        for preset in presets() {
            preset
                .settings
                .check()
                .unwrap_or_else(|error| panic!("{}: {error}", preset.id));
        }
    }

    #[test]
    fn checks_settings_for_each_format() {
        let valid = |settings: ConvertSettings| settings.check().is_ok();
        let flac = settings(ConvertCodec::Flac);
        assert!(valid(flac.clone()));
        assert!(!valid(ConvertSettings {
            bitrate_kbps: Some(320),
            ..flac.clone()
        }));
        assert!(valid(ConvertSettings {
            max_bit_depth: Some(16),
            ..flac.clone()
        }));
        assert!(!valid(ConvertSettings {
            max_bit_depth: Some(20),
            ..flac.clone()
        }));
        assert!(valid(ConvertSettings {
            max_sample_rate: Some(192_000),
            ..flac.clone()
        }));
        assert!(!valid(ConvertSettings {
            max_sample_rate: Some(50_000),
            ..flac
        }));

        let mp3 = settings(ConvertCodec::Mp3);
        assert!(!valid(mp3.clone()), "MP3 needs a quality or a bitrate");
        assert!(valid(ConvertSettings {
            mp3_quality: Some(9),
            ..mp3.clone()
        }));
        assert!(!valid(ConvertSettings {
            mp3_quality: Some(10),
            ..mp3.clone()
        }));
        assert!(valid(ConvertSettings {
            bitrate_kbps: Some(192),
            ..mp3.clone()
        }));
        assert!(!valid(ConvertSettings {
            bitrate_kbps: Some(200),
            ..mp3.clone()
        }));
        assert!(!valid(ConvertSettings {
            mp3_quality: Some(0),
            bitrate_kbps: Some(320),
            ..mp3
        }));
        assert!(!valid(ConvertSettings {
            max_sample_rate: Some(96_000),
            ..mp3_v0()
        }));
        assert!(!valid(ConvertSettings {
            max_bit_depth: Some(16),
            ..mp3_v0()
        }));

        let aac = ConvertSettings {
            bitrate_kbps: Some(256),
            ..settings(ConvertCodec::Aac)
        };
        assert!(valid(aac.clone()));
        assert!(valid(ConvertSettings {
            max_sample_rate: Some(96_000),
            ..aac.clone()
        }));
        assert!(!valid(ConvertSettings {
            max_sample_rate: Some(192_000),
            ..aac.clone()
        }));
        assert!(!valid(ConvertSettings {
            bitrate_kbps: Some(16),
            ..aac
        }));

        let opus = ConvertSettings {
            bitrate_kbps: Some(160),
            ..settings(ConvertCodec::Opus)
        };
        assert!(valid(opus.clone()));
        assert!(!valid(ConvertSettings {
            max_sample_rate: Some(48_000),
            ..opus
        }));
    }

    #[test]
    fn brings_rates_down_within_their_family() {
        assert_eq!(rate_at_most(192_000, 48_000), 48_000);
        assert_eq!(rate_at_most(176_400, 48_000), 44_100);
        assert_eq!(rate_at_most(176_400, 96_000), 88_200);
        assert_eq!(rate_at_most(88_200, 48_000), 44_100);
        assert_eq!(
            rate_at_most(96_000, 44_100),
            44_100,
            "no lower rate in its family fits"
        );
        assert_eq!(rate_at_most(44_100, 32_000), 32_000);
    }

    #[test]
    fn resamples_only_above_the_limit() {
        let capped = ConvertSettings {
            max_sample_rate: Some(48_000),
            ..settings(ConvertCodec::Flac)
        };
        assert_eq!(capped.output_rate(Some(44_100)), None);
        assert_eq!(capped.output_rate(Some(48_000)), None);
        assert_eq!(capped.output_rate(Some(96_000)), Some(48_000));
        assert_eq!(capped.output_rate(Some(176_400)), Some(44_100));
        assert_eq!(capped.output_rate(None), None);
        assert_eq!(
            settings(ConvertCodec::Flac).output_rate(Some(192_000)),
            None
        );
        // MP3 holds at most 48 kHz, so it comes down without being asked.
        assert_eq!(mp3_v0().output_rate(Some(88_200)), Some(44_100));
        assert_eq!(mp3_v0().output_rate(Some(44_100)), None);
    }

    #[test]
    fn reduces_depth_only_for_lossless_output() {
        let capped = ConvertSettings {
            max_bit_depth: Some(16),
            ..settings(ConvertCodec::Flac)
        };
        assert_eq!(
            capped.output_bits(Some(24), false),
            Some(SampleBits::Sixteen)
        );
        assert_eq!(capped.output_bits(Some(16), false), None);
        assert_eq!(
            settings(ConvertCodec::Flac).output_bits(Some(24), false),
            None
        );
        assert_eq!(
            settings(ConvertCodec::Alac).output_bits(None, true),
            Some(SampleBits::Sixteen)
        );
        assert_eq!(mp3_v0().output_bits(Some(24), false), None);
    }

    struct Source<'a> {
        root: i64,
        rel_path: &'a str,
        codec: &'a str,
        missing: bool,
    }

    fn source<'a>(root: i64, rel_path: &'a str, codec: &'a str) -> Source<'a> {
        Source {
            root,
            rel_path,
            codec,
            missing: false,
        }
    }

    fn library(sources: &[Source<'_>]) -> (tempfile::TempDir, Database, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("umak.db")).unwrap();
        let ids = db
            .write(|conn| {
                conn.execute_batch(
                    "INSERT INTO library_roots (id, public_id, name, path, added_at)
                         VALUES (1, 'r1', 'Albums', 'M:/Albums', 0),
                                (2, 'r2', 'Live', 'M:/Live', 0);
                     INSERT INTO albums (id, public_id, group_key, title, sort_title,
                                         album_artist_display, album_artist_sort)
                         VALUES (1, 'a', 'k', 'Album', 'album', 'X', 'x');",
                )?;
                let mut ids = Vec::new();
                for (index, source) in sources.iter().enumerate() {
                    let id = format!("t{index}");
                    conn.execute(
                        "INSERT INTO tracks (public_id, root_id, rel_path, size, mtime,
                             missing_since, codec, sample_rate, bit_depth, title, album_id,
                             title_sort, artist_sort, album_sort, album_artist_sort, added_at,
                             updated_at)
                         VALUES (?1, ?2, ?3, 1, 1, ?4, ?5, 96000, 24, 'T', 1, 't', 'x',
                                 'album', 'x', 0, 0)",
                        params![
                            id,
                            source.root,
                            source.rel_path,
                            source.missing.then_some(1),
                            source.codec
                        ],
                    )?;
                    ids.push(id);
                }
                Ok::<_, DbError>(ids)
            })
            .unwrap();
        (dir, db, ids)
    }

    fn plan(
        db: &Database,
        ids: &[String],
        settings: &ConvertSettings,
        folder: &Path,
    ) -> Result<Vec<PlannedFile>, ApiError> {
        db.read(|conn| plan_files(conn, ids, settings, folder.to_str().unwrap()))
    }

    #[test]
    fn mirrors_folders_under_the_chosen_one() {
        let (dir, db, ids) = library(&[source(1, "Band/Album/01 Song.flac", "flac")]);
        let out = dir.path().join("out");
        let capped = ConvertSettings {
            max_sample_rate: Some(48_000),
            ..mp3_v0()
        };
        let files = plan(&db, &ids, &capped, &out).unwrap();
        let file = &files[0];
        assert_eq!(file.status, ConvertFileStatus::Ready);
        assert_eq!(
            file.conversion.input,
            Path::new("M:/Albums")
                .join("Band")
                .join("Album")
                .join("01 Song.flac")
        );
        assert_eq!(
            file.conversion.output,
            out.join("Band").join("Album").join("01 Song.mp3")
        );
        assert_eq!(file.conversion.encoding, Encoding::Mp3Vbr(0));
        assert_eq!(file.conversion.sample_rate, Some(48_000));
        assert_eq!(file.conversion.bits, None);
        assert!(!file.lossy_source);
    }

    #[test]
    fn finds_what_cannot_be_converted() {
        let sources = [
            source(1, "A/song.flac", "flac"),
            // The same place under another root, and the same name in another format.
            source(2, "A/song.flac", "flac"),
            source(1, "A/SONG.mp3", "mp3"),
            source(1, "B/done.flac", "flac"),
            Source {
                missing: true,
                ..source(1, "B/gone.flac", "flac")
            },
            source(1, "B/fine.mp3", "mp3"),
            source(1, "B/name.with.dots.flac", "flac"),
        ];
        let (dir, db, ids) = library(&sources);
        let out = dir.path().join("out");
        fs::create_dir_all(out.join("B")).unwrap();
        fs::write(out.join("B").join("done.m4a"), "converted earlier").unwrap();
        let aac = ConvertSettings {
            bitrate_kbps: Some(256),
            ..settings(ConvertCodec::Aac)
        };
        let files = plan(&db, &ids, &aac, &out).unwrap();
        let statuses: Vec<_> = files.iter().map(|file| file.status).collect();
        use ConvertFileStatus::*;
        assert_eq!(
            statuses,
            [
                SameOutput, SameOutput, SameOutput, Exists, Missing, Ready, Ready
            ]
        );
        assert_eq!(
            files[6].conversion.output,
            out.join("B").join("name.with.dots.m4a")
        );

        let summary = summarize(&files, &aac);
        assert_eq!(
            (
                summary.ready,
                summary.exists,
                summary.same_output,
                summary.missing
            ),
            (2, 1, 3, 1)
        );
        assert_eq!(summary.lossy_sources, 1);
        assert_eq!(summary.extension, "m4a");
        assert_eq!(
            summary.example_output.as_deref(),
            Some(out.join("B").join("fine.m4a").to_str().unwrap())
        );
        assert_eq!(summary.problems.len(), 5);
        assert_eq!(
            fs::read_to_string(out.join("B").join("done.m4a")).unwrap(),
            "converted earlier"
        );
    }

    #[test]
    fn rejects_bad_requests() {
        let (dir, db, ids) = library(&[source(1, "a.flac", "flac")]);
        let out = dir.path().join("out");
        let code = |result: Result<Vec<PlannedFile>, ApiError>| result.unwrap_err().code;
        assert_eq!(
            code(plan(&db, &[], &mp3_v0(), &out)),
            ErrorCode::InvalidArgs
        );
        let twice = [ids[0].clone(), ids[0].clone()];
        assert_eq!(
            code(plan(&db, &twice, &mp3_v0(), &out)),
            ErrorCode::InvalidArgs
        );
        let unknown = ["nope".to_owned()];
        assert_eq!(
            code(plan(&db, &unknown, &mp3_v0(), &out)),
            ErrorCode::NotFound
        );
        let no_quality = settings(ConvertCodec::Mp3);
        assert_eq!(
            code(plan(&db, &ids, &no_quality, &out)),
            ErrorCode::InvalidArgs
        );
        let relative = Path::new("relative");
        assert_eq!(
            code(plan(&db, &ids, &mp3_v0(), relative)),
            ErrorCode::InvalidArgs
        );
    }
}
