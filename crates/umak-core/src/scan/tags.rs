//! Reads a file's tags and audio properties into the tag model. Files are opened
//! read-only. Lofty's generic tag drops fields it has no key for, so MP3, FLAC and
//! MP4 are read through their own types to keep those fields as raw tags. Other
//! formats keep only the fields Lofty maps, until tag editing needs more.

use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Read, Seek};

use lofty::config::{ParseOptions, ParsingMode};
use lofty::file::{AudioFile, FileType, TaggedFile, TaggedFileExt};
use lofty::flac::FlacFile;
use lofty::id3::v2::{Frame, Id3v2Tag};
use lofty::mp4::{AtomData, AtomIdent, Ilst, Mp4Codec, Mp4File};
use lofty::mpeg::MpegFile;
use lofty::ogg::tag::VorbisComments;
use lofty::prelude::SplitTag;
use lofty::probe::Probe;
use lofty::tag::{Accessor, ItemKey, ItemValue, Tag};

use super::normalize::{artist_key, identity_key};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackTags {
    pub title: Option<String>,
    pub album: Option<String>,
    pub artists: Vec<String>,
    pub album_artists: Vec<String>,
    pub composers: Vec<String>,
    pub genres: Vec<String>,
    pub title_sort: Option<String>,
    pub album_sort: Option<String>,
    pub artist_sort: Option<String>,
    pub album_artist_sort: Option<String>,
    pub disc_no: Option<u32>,
    pub track_no: Option<u32>,
    pub year: Option<i64>,
    pub original_date: Option<String>,
    pub compilation: bool,
    pub rg_track_gain: Option<f64>,
    pub rg_track_peak: Option<f64>,
    pub rg_album_gain: Option<f64>,
    pub rg_album_peak: Option<f64>,
    pub musicbrainz_album_id: Option<String>,
    pub musicbrainz_recording_id: Option<String>,
    /// Fields with no column of their own, in file order.
    pub raw: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioProps {
    pub duration_ms: Option<i64>,
    pub codec: Option<String>,
    pub sample_rate: Option<i64>,
    pub bit_depth: Option<i64>,
    pub channels: Option<i64>,
    pub bitrate: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileInfo {
    pub tags: TrackTags,
    pub audio: AudioProps,
}

/// Keys that have a column or link table. Everything else is kept as a raw tag.
const MAPPED_KEYS: &[ItemKey] = &[
    ItemKey::TrackTitle,
    ItemKey::AlbumTitle,
    ItemKey::TrackArtist,
    ItemKey::TrackArtists,
    ItemKey::AlbumArtist,
    ItemKey::AlbumArtists,
    ItemKey::Composer,
    ItemKey::Genre,
    ItemKey::TrackTitleSortOrder,
    ItemKey::AlbumTitleSortOrder,
    ItemKey::TrackArtistSortOrder,
    ItemKey::AlbumArtistSortOrder,
    ItemKey::DiscNumber,
    ItemKey::TrackNumber,
    ItemKey::RecordingDate,
    ItemKey::Year,
    ItemKey::OriginalReleaseDate,
    ItemKey::FlagCompilation,
    ItemKey::ReplayGainTrackGain,
    ItemKey::ReplayGainTrackPeak,
    ItemKey::ReplayGainAlbumGain,
    ItemKey::ReplayGainAlbumPeak,
    ItemKey::MusicBrainzReleaseId,
    ItemKey::MusicBrainzRecordingId,
];

/// How far past a tag to look for the audio. Real files pad beyond Lofty's default.
const MAX_JUNK_BYTES: usize = 1024 * 1024;

type SplitResult = (Tag, Vec<(String, String)>);

pub fn read_file<R: Read + Seek>(reader: R) -> Result<FileInfo, String> {
    let options = ParseOptions::new()
        .read_cover_art(false)
        .max_junk_bytes(MAX_JUNK_BYTES)
        .parsing_mode(ParsingMode::Relaxed);
    let probe = Probe::new(BufReader::new(reader))
        .options(options)
        .guess_file_type()
        .map_err(|error| error.to_string())?;
    let file_type = probe.file_type();

    match file_type {
        Some(FileType::Mpeg) => {
            let mut reader = probe.into_inner();
            let mut file =
                MpegFile::read_from(&mut reader, options).map_err(|error| full_error(&error))?;
            let split = file.remove_id3v2().map(split_id3v2);
            Ok(finish(split, file.into(), Some("mp3".to_owned())))
        }
        Some(FileType::Flac) => {
            let mut reader = probe.into_inner();
            let mut file =
                FlacFile::read_from(&mut reader, options).map_err(|error| full_error(&error))?;
            let split = file.remove_vorbis_comments().map(split_vorbis);
            Ok(finish(split, file.into(), Some("flac".to_owned())))
        }
        Some(FileType::Mp4) => {
            let mut reader = probe.into_inner();
            let mut file =
                Mp4File::read_from(&mut reader, options).map_err(|error| full_error(&error))?;
            let codec = file.properties().codec().map(codec_name);
            let split = file.remove_ilst().map(split_ilst);
            Ok(finish(split, file.into(), codec))
        }
        _ => {
            let file = probe.read().map_err(|error| full_error(&error))?;
            let codec = file_type.map(|kind| format!("{kind:?}").to_lowercase());
            Ok(finish(None, file, codec))
        }
    }
}

/// Lofty's top-level message only names the format; the cause is in its source.
fn full_error(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

fn codec_name(codec: Mp4Codec) -> String {
    match codec {
        Mp4Codec::AAC => "aac",
        Mp4Codec::ALAC => "alac",
        Mp4Codec::MP3 => "mp3",
        Mp4Codec::FLAC => "flac",
        _ => "unknown",
    }
    .to_owned()
}

fn split_id3v2(tag: Id3v2Tag) -> SplitResult {
    let (remainder, generic) = tag.split_tag();
    let mut raw = Vec::new();
    for frame in &*remainder {
        match frame {
            Frame::UserText(frame) => {
                raw.push((frame.description.to_string(), frame.content.to_string()));
            }
            Frame::Text(text) => {
                raw.push((frame.id_str().to_owned(), text.value.to_string()));
            }
            Frame::Comment(frame) => {
                raw.push((
                    format!("COMM:{}", frame.description),
                    frame.content.to_string(),
                ));
            }
            Frame::UnsynchronizedText(frame) => {
                raw.push((
                    format!("USLT:{}", frame.description),
                    frame.content.to_string(),
                ));
            }
            _ => {}
        }
    }
    (generic, raw)
}

fn split_vorbis(tag: VorbisComments) -> SplitResult {
    let (remainder, generic) = tag.split_tag();
    let raw = remainder
        .items()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    (generic, raw)
}

fn split_ilst(tag: Ilst) -> SplitResult {
    let (remainder, generic) = tag.split_tag();
    let mut raw = Vec::new();
    for atom in &*remainder {
        let key: String = match atom.ident() {
            AtomIdent::Fourcc(code) => code.iter().map(|byte| *byte as char).collect(),
            AtomIdent::Freeform { name, .. } => name.to_string(),
        };
        for data in atom.data() {
            if let AtomData::UTF8(text) | AtomData::UTF16(text) = data {
                raw.push((key.clone(), text.clone()));
            }
        }
    }
    (generic, raw)
}

fn finish(split: Option<SplitResult>, file: TaggedFile, codec: Option<String>) -> FileInfo {
    let properties = file.properties();
    let audio = AudioProps {
        duration_ms: Some(properties.duration().as_millis() as i64),
        codec,
        sample_rate: properties.sample_rate().map(i64::from),
        bit_depth: properties.bit_depth().map(i64::from),
        channels: properties.channels().map(i64::from),
        bitrate: properties
            .audio_bitrate()
            .or_else(|| properties.overall_bitrate())
            .map(i64::from),
    };
    let tags = match split {
        Some((tag, raw)) => model_from(&tag, raw),
        None => match file.primary_tag().or_else(|| file.first_tag()) {
            Some(tag) => model_from(tag, Vec::new()),
            None => TrackTags::default(),
        },
    };
    FileInfo { tags, audio }
}

fn text_values(tag: &Tag, key: ItemKey) -> Vec<String> {
    tag.get_strings(key)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn first_text(tag: &Tag, key: ItemKey) -> Option<String> {
    tag.get_strings(key)
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Parses `-6.54 dB` or `0.98`.
fn leading_number(text: &str) -> Option<f64> {
    text.split_whitespace().next()?.parse().ok()
}

fn is_true(text: &str) -> bool {
    matches!(text.trim().to_ascii_lowercase().as_str(), "1" | "true")
}

/// Several values for one field are separate names. A single value is never split on
/// characters like `/` or `;`, because they appear in real names such as AC/DC.
fn prefer_list(list: Vec<String>, single: Vec<String>) -> Vec<String> {
    if list.is_empty() { single } else { list }
}

/// Keeps the first of values that name the same thing, which some taggers repeat.
fn unique(values: Vec<String>, key: impl Fn(&str) -> String) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(key(value)))
        .collect()
}

fn model_from(tag: &Tag, mut raw: Vec<(String, String)>) -> TrackTags {
    for item in tag.items() {
        let key = item.key();
        if MAPPED_KEYS.contains(&key) {
            continue;
        }
        if let ItemValue::Text(value) | ItemValue::Locator(value) = item.value() {
            let name = key
                .map_key(tag.tag_type())
                .map_or_else(|| format!("{key:?}"), str::to_owned);
            raw.push((name, value.clone()));
        }
    }
    let number = |key| first_text(tag, key).and_then(|text| leading_number(&text));
    TrackTags {
        title: first_text(tag, ItemKey::TrackTitle),
        album: first_text(tag, ItemKey::AlbumTitle),
        artists: unique(
            prefer_list(
                text_values(tag, ItemKey::TrackArtists),
                text_values(tag, ItemKey::TrackArtist),
            ),
            artist_key,
        ),
        album_artists: unique(
            prefer_list(
                text_values(tag, ItemKey::AlbumArtists),
                text_values(tag, ItemKey::AlbumArtist),
            ),
            artist_key,
        ),
        composers: unique(text_values(tag, ItemKey::Composer), artist_key),
        genres: unique(text_values(tag, ItemKey::Genre), identity_key),
        title_sort: first_text(tag, ItemKey::TrackTitleSortOrder),
        album_sort: first_text(tag, ItemKey::AlbumTitleSortOrder),
        artist_sort: first_text(tag, ItemKey::TrackArtistSortOrder),
        album_artist_sort: first_text(tag, ItemKey::AlbumArtistSortOrder),
        disc_no: tag.disk(),
        track_no: tag.track(),
        year: tag.date().map(|date| i64::from(date.year)),
        original_date: first_text(tag, ItemKey::OriginalReleaseDate),
        compilation: first_text(tag, ItemKey::FlagCompilation).is_some_and(|text| is_true(&text)),
        rg_track_gain: number(ItemKey::ReplayGainTrackGain),
        rg_track_peak: number(ItemKey::ReplayGainTrackPeak),
        rg_album_gain: number(ItemKey::ReplayGainAlbumGain),
        rg_album_peak: number(ItemKey::ReplayGainAlbumPeak),
        musicbrainz_album_id: first_text(tag, ItemKey::MusicBrainzReleaseId),
        musicbrainz_recording_id: first_text(tag, ItemKey::MusicBrainzRecordingId),
        raw,
    }
}

/// Numbers repeated raw keys so each value has a position.
pub fn positioned(raw: &[(String, String)]) -> Vec<(&str, i64, &str)> {
    let mut seen: HashMap<&str, i64> = HashMap::new();
    raw.iter()
        .map(|(key, value)| {
            let position = seen.entry(key).or_default();
            let row = (key.as_str(), *position, value.as_str());
            *position += 1;
            row
        })
        .collect()
}
