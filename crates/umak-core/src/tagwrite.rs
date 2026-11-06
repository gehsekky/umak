//! Writes tags to music files. This is the one path that changes them, and it is built so
//! a failure at any point leaves the original exactly as it was: the edit is made on a
//! temporary copy, the copy is checked, and only then does it replace the original.
//!
//! Tags stay in the file's own format. ID3v2 tags are edited frame by frame in `id3v2`,
//! and an ID3v1 or APE tag beside them is kept in step, so another player does not find
//! the old values there. FLAC and MP4 tags go through the tag library.

mod id3v2;

use std::collections::HashSet;
use std::fs::{self, File, FileTimes, OpenOptions};
use std::io::{self, BufReader, Seek};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use lofty::ape::{ApeItem, ApeTag};
use lofty::config::{ParseOptions, ParsingMode, WriteOptions};
use lofty::file::{AudioFile, FileType};
use lofty::flac::FlacFile;
use lofty::id3::v1::Id3v1Tag;
use lofty::id3::v2::Frame;
use lofty::mp4::{Atom, AtomData, AtomIdent, DataType, Ilst, Mp4File};
use lofty::mpeg::MpegFile;
use lofty::ogg::OggPictureStorage;
use lofty::ogg::tag::VorbisComments;
use lofty::probe::Probe;
use lofty::tag::items::Timestamp;
use lofty::tag::{Accessor, ItemValue};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::scan::fingerprint::audio_hash;
use crate::scan::tags::{FileInfo, TrackTags, full_error, read_file};

/// How far past a tag to look for the audio, as the scanner does.
const MAX_JUNK_BYTES: usize = 1024 * 1024;
const TEMP_SUFFIX: &str = ".umak-tmp";
/// For the one kind of tag that holds a single value where the edit gives several.
const SINGLE_VALUE_JOIN: &str = "/";
/// How an APE tag separates several values.
const APE_SEPARATOR: &str = "\0";

/// The fields that can be edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum TagField {
    Title,
    Album,
    Artists,
    AlbumArtists,
    Composers,
    Genres,
    Year,
    TrackNumber,
    DiscNumber,
}

/// A new value for a field. No values removes the field. Only the list fields can hold
/// more than one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct FieldEdit {
    pub field: TagField,
    pub values: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum TagWriteError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Unsupported(String),
    #[error("this file is marked read-only")]
    ReadOnly,
    #[error("the file changed while it was being edited, so nothing was written")]
    Changed,
    #[error("the check after writing failed, so nothing was written: {0}")]
    Verification(String),
    #[error("{0}")]
    Format(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// What to check that a file has not changed by.
#[derive(PartialEq)]
struct Stamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
}

fn stamp(path: &Path) -> io::Result<Stamp> {
    let metadata = fs::metadata(path)?;
    Ok(Stamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

fn parse_options(read_cover_art: bool) -> ParseOptions {
    ParseOptions::new()
        .read_cover_art(read_cover_art)
        .max_junk_bytes(MAX_JUNK_BYTES)
        .parsing_mode(ParsingMode::Relaxed)
}

fn format_error(error: &dyn std::error::Error) -> TagWriteError {
    TagWriteError::Format(full_error(error))
}

/// Removes the temporary file unless it took the original's place.
struct TempFile {
    path: PathBuf,
    keep: bool,
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn temp_path(path: &Path) -> Result<PathBuf, TagWriteError> {
    let name = path
        .file_name()
        .ok_or_else(|| TagWriteError::Invalid("that path has no file name".to_owned()))?;
    let mut temp = name.to_os_string();
    temp.push(TEMP_SUFFIX);
    Ok(path.with_file_name(temp))
}

/// Checks the edits and tidies their values.
fn clean_edits(edits: &[FieldEdit]) -> Result<Vec<FieldEdit>, TagWriteError> {
    let mut seen = HashSet::new();
    let mut cleaned = Vec::with_capacity(edits.len());
    for edit in edits {
        if !seen.insert(edit.field) {
            return Err(TagWriteError::Invalid(format!(
                "{:?} was edited twice",
                edit.field
            )));
        }
        let mut unique = HashSet::new();
        let values: Vec<String> = edit
            .values
            .iter()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty() && unique.insert(value.clone()))
            .collect();
        let is_list = matches!(
            edit.field,
            TagField::Artists | TagField::AlbumArtists | TagField::Composers | TagField::Genres
        );
        if !is_list && values.len() > 1 {
            return Err(TagWriteError::Invalid(format!(
                "{:?} holds only one value",
                edit.field
            )));
        }
        if matches!(
            edit.field,
            TagField::Year | TagField::TrackNumber | TagField::DiscNumber
        ) && let Some(value) = values.first()
        {
            let valid = match edit.field {
                TagField::Year => value.parse::<u16>().is_ok_and(|year| year > 0),
                _ => value.parse::<u32>().is_ok_and(|number| number > 0),
            };
            if !valid {
                return Err(TagWriteError::Invalid(format!(
                    "\"{value}\" is not a valid {:?}",
                    edit.field
                )));
            }
        }
        cleaned.push(FieldEdit {
            field: edit.field,
            values,
        });
    }
    Ok(cleaned)
}

/// The fields every tag type has a common way to set.
fn apply_common<T: Accessor>(tag: &mut T, edit: &FieldEdit) {
    let first = edit.values.first().cloned();
    match (edit.field, first) {
        (TagField::Title, Some(value)) => tag.set_title(value),
        (TagField::Title, None) => tag.remove_title(),
        (TagField::Album, Some(value)) => tag.set_album(value),
        (TagField::Album, None) => tag.remove_album(),
        (TagField::Year, Some(value)) => tag.set_date(Timestamp {
            year: value.parse().unwrap_or_default(),
            ..Timestamp::default()
        }),
        (TagField::Year, None) => tag.remove_date(),
        (TagField::TrackNumber, Some(value)) => tag.set_track(value.parse().unwrap_or_default()),
        (TagField::TrackNumber, None) => tag.remove_track(),
        (TagField::DiscNumber, Some(value)) => tag.set_disk(value.parse().unwrap_or_default()),
        (TagField::DiscNumber, None) => tag.remove_disk(),
        _ => {}
    }
}

fn id3_frame_ids(field: TagField) -> &'static [&'static str] {
    match field {
        TagField::Title => &["TIT2"],
        TagField::Album => &["TALB"],
        TagField::Artists => &["TPE1"],
        TagField::AlbumArtists => &["TPE2"],
        TagField::Composers => &["TCOM"],
        TagField::Genres => &["TCON"],
        TagField::Year => &["TDRC", "TYER", "TDAT", "TIME"],
        TagField::TrackNumber => &["TRCK"],
        TagField::DiscNumber => &["TPOS"],
    }
}

/// Custom fields that also hold this field's values, which the reader prefers.
pub(crate) fn list_field_names(field: TagField) -> &'static [&'static str] {
    match field {
        TagField::Artists => &["ARTISTS"],
        TagField::AlbumArtists => &[
            "ALBUMARTISTS",
            "ALBUM ARTISTS",
            "ALBUMARTIST",
            "ALBUM ARTIST",
        ],
        _ => &[],
    }
}

fn apply_id3v1(tag: &mut Id3v1Tag, edit: &FieldEdit) {
    let joined = edit.values.join(SINGLE_VALUE_JOIN);
    match edit.field {
        TagField::Artists if joined.is_empty() => tag.remove_artist(),
        TagField::Artists => tag.set_artist(joined),
        TagField::Genres => match edit.values.first() {
            Some(genre) => tag.set_genre(genre.clone()),
            None => tag.remove_genre(),
        },
        // Its one disc-less field set has no place for these.
        TagField::AlbumArtists | TagField::Composers | TagField::DiscNumber => {}
        _ => apply_common(tag, edit),
    }
}

/// Every key in an APE tag that an edit of this field changes.
fn ape_keys(field: TagField) -> Vec<String> {
    let keys: &[&str] = match field {
        TagField::Title => &["Title"],
        TagField::Album => &["Album"],
        TagField::Year => &["Year"],
        TagField::TrackNumber => &["Track"],
        TagField::DiscNumber => &["Disc"],
        TagField::Artists => &["Artist"],
        TagField::AlbumArtists => &["Album Artist"],
        TagField::Composers => &["Composer"],
        TagField::Genres => &["Genre"],
    };
    keys.iter().map(|key| (*key).to_owned()).collect()
}

fn ape_key(field: TagField) -> Option<&'static str> {
    match field {
        TagField::Artists => Some("Artist"),
        TagField::AlbumArtists => Some("Album Artist"),
        TagField::Composers => Some("Composer"),
        TagField::Genres => Some("Genre"),
        _ => None,
    }
}

fn apply_ape(tag: &mut ApeTag, edit: &FieldEdit) -> Result<(), TagWriteError> {
    let Some(key) = ape_key(edit.field) else {
        apply_common(tag, edit);
        return Ok(());
    };
    tag.remove(key);
    if !edit.values.is_empty() {
        let value = ItemValue::Text(edit.values.join(APE_SEPARATOR));
        let item = ApeItem::new(key.to_owned(), value).map_err(|error| format_error(&error))?;
        tag.insert(item);
    }
    Ok(())
}

/// Every key a reader might find this field under. The first is the one to write.
fn vorbis_keys(field: TagField) -> &'static [&'static str] {
    match field {
        TagField::Title => &["TITLE"],
        TagField::Album => &["ALBUM"],
        TagField::Artists => &["ARTIST", "ARTISTS"],
        TagField::AlbumArtists => &["ALBUMARTIST", "ALBUM ARTIST", "ALBUMARTISTS"],
        TagField::Composers => &["COMPOSER"],
        TagField::Genres => &["GENRE"],
        TagField::Year => &["DATE", "YEAR"],
        TagField::TrackNumber => &["TRACKNUMBER", "TRACK"],
        TagField::DiscNumber => &["DISCNUMBER", "DISC"],
    }
}

fn apply_vorbis(tag: &mut VorbisComments, edit: &FieldEdit) {
    let keys = vorbis_keys(edit.field);
    for key in keys {
        let _ = tag.remove(key).count();
    }
    for value in &edit.values {
        tag.push(keys[0].to_owned(), value.clone());
    }
}

fn mp4_atom(field: TagField) -> Option<[u8; 4]> {
    match field {
        TagField::Artists => Some(*b"\xa9ART"),
        TagField::AlbumArtists => Some(*b"aART"),
        TagField::Composers => Some(*b"\xa9wrt"),
        TagField::Genres => Some(*b"\xa9gen"),
        _ => None,
    }
}

fn apply_ilst(tag: &mut Ilst, edit: &FieldEdit) {
    let Some(code) = mp4_atom(edit.field) else {
        apply_common(tag, edit);
        return;
    };
    let ident = AtomIdent::Fourcc(code);
    // Whatever the case: real files hold stray atoms such as aArt, which readers also find.
    tag.retain(|atom| {
        !matches!(atom.ident(), AtomIdent::Fourcc(found) if found.eq_ignore_ascii_case(&code))
    });
    if edit.field == TagField::Genres {
        // The older numeric form, which readers also look at.
        tag.remove_genre();
    }
    // Whatever the case of the name, since readers ignore it.
    let names = list_field_names(edit.field);
    tag.retain(|atom| {
        !matches!(atom.ident(), AtomIdent::Freeform { name, .. }
            if names.iter().any(|list| list.eq_ignore_ascii_case(name)))
    });
    let data = edit.values.iter().cloned().map(AtomData::UTF8).collect();
    if let Some(atom) = Atom::from_collection(ident, data) {
        tag.insert(atom);
    }
}

/// iTunes stores these as one byte. The tag library reads them as numbers and would write
/// them back as four, so they are handed over as the bytes they were.
const ONE_BYTE_ATOMS: [[u8; 4]; 6] = [*b"rtng", *b"stik", *b"akID", *b"pcst", *b"hdvd", *b"shwm"];

fn keep_one_byte_atoms(tag: &mut Ilst) {
    for code in ONE_BYTE_ATOMS {
        let ident = AtomIdent::Fourcc(code);
        let number = tag.get(&ident).and_then(|atom| {
            atom.data().find_map(|data| match data {
                AtomData::SignedInteger(value) => u8::try_from(*value).ok(),
                AtomData::UnsignedInteger(value) => u8::try_from(*value).ok(),
                _ => None,
            })
        });
        if let Some(number) = number {
            let data = AtomData::Unknown {
                code: DataType::BeSignedInteger,
                data: vec![number],
            };
            tag.replace_atom(Atom::new(ident, data));
        }
    }
}

fn file_type(path: &Path) -> Result<Option<FileType>, TagWriteError> {
    let probe = Probe::new(BufReader::new(File::open(path)?))
        .options(parse_options(false))
        .guess_file_type()?;
    Ok(probe.file_type())
}

fn read_back(path: &Path) -> Result<FileInfo, TagWriteError> {
    read_file(File::open(path)?).map_err(TagWriteError::Format)
}

/// Opens a file for a tag library that reads and then writes through one handle.
fn open_rw(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).open(path)
}

/// Makes the edits in the file at `path`, in the tag format it already has.
fn apply_edits(path: &Path, kind: FileType, edits: &[FieldEdit]) -> Result<(), TagWriteError> {
    let mut handle = open_rw(path)?;
    let options = parse_options(true);
    match kind {
        FileType::Mpeg => {
            id3v2::edit_file(&mut handle, edits)?;
            handle.rewind()?;
            // The tags at the end of the file. Reading the ID3v2 tag again is not needed,
            // and leaving it out keeps the tag library from writing it.
            let mut file =
                MpegFile::read_from(&mut handle, options).map_err(|error| format_error(&error))?;
            if file.id3v1().is_some() || file.ape().is_some() {
                let _ = file.remove_id3v2();
                for edit in edits {
                    if let Some(tag) = file.id3v1_mut() {
                        apply_id3v1(tag, edit);
                    }
                    if let Some(tag) = file.ape_mut() {
                        apply_ape(tag, edit)?;
                    }
                }
                handle.rewind()?;
                file.save_to(&mut handle, WriteOptions::new())
                    .map_err(|error| format_error(&error))?;
            }
        }
        FileType::Flac => {
            let mut file =
                FlacFile::read_from(&mut handle, options).map_err(|error| format_error(&error))?;
            if file.vorbis_comments().is_none() {
                file.set_vorbis_comments(VorbisComments::new());
            }
            if let Some(tag) = file.vorbis_comments_mut() {
                edits.iter().for_each(|edit| apply_vorbis(tag, edit));
            }
            // An ID3 tag before the FLAC stream stays in the file as it is. The tag library
            // cannot write one, so it must not be asked to.
            let _ = file.remove_id3v2();
            handle.rewind()?;
            file.save_to(&mut handle, WriteOptions::new())
                .map_err(|error| format_error(&error))?;
        }
        FileType::Mp4 => {
            let mut file =
                Mp4File::read_from(&mut handle, options).map_err(|error| format_error(&error))?;
            if file.ilst().is_none() {
                file.set_ilst(Ilst::new());
            }
            if let Some(tag) = file.ilst_mut() {
                edits.iter().for_each(|edit| apply_ilst(tag, edit));
                keep_one_byte_atoms(tag);
            }
            handle.rewind()?;
            file.save_to(&mut handle, WriteOptions::new())
                .map_err(|error| format_error(&error))?;
        }
        other => {
            return Err(TagWriteError::Unsupported(format!(
                "tags in {other:?} files cannot be written yet"
            )));
        }
    }
    handle.sync_all()?;
    Ok(())
}

/// One item of a tag: a name and a hash of its whole content.
type Item = (String, [u8; 32]);

fn item(name: String, content: &str) -> Item {
    (name, *blake3::hash(content.as_bytes()).as_bytes())
}

/// Every item in the file's tags that these edits should not touch, including ones the
/// tag model does not know, such as cover art and private frames.
fn untouched_items(
    path: &Path,
    kind: FileType,
    edited: &[TagField],
) -> Result<Vec<Item>, TagWriteError> {
    let mut handle = File::open(path)?;
    let options = parse_options(true);
    let mut items: Vec<Item> = match kind {
        FileType::Mpeg => {
            let file =
                MpegFile::read_from(&mut handle, options).map_err(|error| format_error(&error))?;
            let skip: HashSet<&str> = edited
                .iter()
                .flat_map(|field| id3_frame_ids(*field).iter().copied())
                .collect();
            let lists: HashSet<&str> = edited
                .iter()
                .flat_map(|field| list_field_names(*field).iter().copied())
                .collect();
            let mut items = Vec::new();
            for frame in file.id3v2().into_iter().flatten() {
                let is_edited = skip.contains(frame.id_str())
                    || matches!(frame, Frame::UserText(text)
                        if lists.iter().any(|name| name.eq_ignore_ascii_case(&text.description)));
                if !is_edited {
                    items.push(item(frame.id_str().to_owned(), &format!("{frame:?}")));
                }
            }
            let ape_skip: Vec<String> = edited.iter().flat_map(|field| ape_keys(*field)).collect();
            for entry in file.ape().into_iter().flatten() {
                if !ape_skip
                    .iter()
                    .any(|key| key.eq_ignore_ascii_case(entry.key()))
                {
                    items.push(item(format!("APE {}", entry.key()), &format!("{entry:?}")));
                }
            }
            items
        }
        FileType::Flac => {
            let file =
                FlacFile::read_from(&mut handle, options).map_err(|error| format_error(&error))?;
            let skip: HashSet<&str> = edited
                .iter()
                .flat_map(|field| vorbis_keys(*field).iter().copied())
                .collect();
            let mut items: Vec<Item> = file
                .vorbis_comments()
                .map(|tag| {
                    tag.items()
                        .filter(|(key, _)| !skip.contains(key.to_ascii_uppercase().as_str()))
                        .map(|(key, value)| item(key.to_ascii_uppercase(), value))
                        .collect()
                })
                .unwrap_or_default();
            for (picture, info) in file.pictures() {
                items.push(item("PICTURE".to_owned(), &format!("{picture:?}{info:?}")));
            }
            items
        }
        FileType::Mp4 => {
            let file =
                Mp4File::read_from(&mut handle, options).map_err(|error| format_error(&error))?;
            let is_edited = |ident: &AtomIdent<'_>| match ident {
                AtomIdent::Fourcc(code) => edited.iter().any(|field| match field {
                    TagField::Title => code == b"\xa9nam",
                    TagField::Album => code == b"\xa9alb",
                    TagField::Year => code == b"\xa9day",
                    TagField::TrackNumber => code == b"trkn",
                    TagField::DiscNumber => code == b"disk",
                    TagField::Genres => code == b"\xa9gen" || code == b"gnre",
                    other => mp4_atom(*other).is_some_and(|atom| atom.eq_ignore_ascii_case(code)),
                }),
                AtomIdent::Freeform { name, .. } => edited
                    .iter()
                    .flat_map(|field| list_field_names(*field).iter())
                    .any(|list| list.eq_ignore_ascii_case(name)),
            };
            file.ilst()
                .map(|tag| {
                    tag.into_iter()
                        .filter(|atom| !is_edited(atom.ident()))
                        .map(|atom| item(format!("{:?}", atom.ident()), &format!("{atom:?}")))
                        .collect()
                })
                .unwrap_or_default()
        }
        _ => Vec::new(),
    };
    items.sort();
    Ok(items)
}

/// What the tag model should read after the edits.
fn expected_tags(before: &TrackTags, edits: &[FieldEdit]) -> TrackTags {
    let mut expected = before.clone();
    for edit in edits {
        let first = edit.values.first().cloned();
        match edit.field {
            TagField::Title => expected.title = first,
            TagField::Album => expected.album = first,
            TagField::Artists => expected.artists = edit.values.clone(),
            TagField::AlbumArtists => expected.album_artists = edit.values.clone(),
            TagField::Composers => expected.composers = edit.values.clone(),
            TagField::Genres => expected.genres = edit.values.clone(),
            TagField::Year => expected.year = first.and_then(|value| value.parse().ok()),
            TagField::TrackNumber => expected.track_no = first.and_then(|value| value.parse().ok()),
            TagField::DiscNumber => expected.disc_no = first.and_then(|value| value.parse().ok()),
        }
    }
    expected
}

/// Without the raw tags the edits may change: other names for the same fields.
fn without_edited_raw(tags: &TrackTags, edits: &[FieldEdit]) -> TrackTags {
    let mut tags = tags.clone();
    let keys: Vec<&str> = edits
        .iter()
        .flat_map(|edit| {
            list_field_names(edit.field)
                .iter()
                .chain(id3_frame_ids(edit.field))
                .chain(vorbis_keys(edit.field))
                .copied()
        })
        .collect();
    tags.raw
        .retain(|(key, _)| !keys.iter().any(|edited| edited.eq_ignore_ascii_case(key)));
    tags.raw.sort();
    tags
}

/// Names what differs, with the values, so a refusal can be understood.
fn differing_fields(expected: &TrackTags, found: &TrackTags) -> String {
    macro_rules! compare {
        ($($field:ident),* $(,)?) => {{
            let mut parts = Vec::new();
            $(if expected.$field != found.$field {
                parts.push(format!(
                    "{}: expected {:?}, found {:?}",
                    stringify!($field), expected.$field, found.$field
                ));
            })*
            parts.join("; ")
        }};
    }
    compare!(
        title,
        album,
        artists,
        album_artists,
        composers,
        genres,
        title_sort,
        album_sort,
        artist_sort,
        album_artist_sort,
        disc_no,
        track_no,
        year,
        original_date,
        compilation,
        rg_track_gain,
        rg_track_peak,
        rg_album_gain,
        rg_album_peak,
        musicbrainz_album_id,
        musicbrainz_recording_id,
        raw,
    )
}

/// Checks the edited copy against the original: every field must read as intended, every
/// other item must be unchanged, and the audio must be identical byte for byte.
fn verify(
    original: &Path,
    temp: &Path,
    kind: FileType,
    edits: &[FieldEdit],
) -> Result<(), TagWriteError> {
    let before = read_back(original)?;
    let after = read_back(temp)
        .map_err(|error| TagWriteError::Verification(format!("it cannot be read: {error}")))?;

    let expected = without_edited_raw(&expected_tags(&before.tags, edits), edits);
    let found = without_edited_raw(&after.tags, edits);
    if expected != found {
        return Err(TagWriteError::Verification(format!(
            "these fields read back differently: {}",
            differing_fields(&expected, &found)
        )));
    }

    let edited: Vec<TagField> = edits.iter().map(|edit| edit.field).collect();
    let items_before = untouched_items(original, kind, &edited)?;
    let items_after = untouched_items(temp, kind, &edited)?;
    if items_before != items_after {
        let changed: Vec<&str> = items_before
            .iter()
            .filter(|item| !items_after.contains(item))
            .chain(
                items_after
                    .iter()
                    .filter(|item| !items_before.contains(item)),
            )
            .map(|(name, _)| name.as_str())
            .collect();
        return Err(TagWriteError::Verification(format!(
            "tags the edit should not touch changed: {}",
            changed.join(", ")
        )));
    }

    if audio_hash(&mut File::open(original)?)? != audio_hash(&mut File::open(temp)?)? {
        return Err(TagWriteError::Verification("the audio changed".to_owned()));
    }
    Ok(())
}

/// Replaces the original with the checked copy, keeping when the file was created.
fn replace(original: &Path, temp: &Path) -> io::Result<()> {
    let created = fs::metadata(original)?.created().ok();
    let handle = OpenOptions::new().write(true).open(temp)?;
    if let Some(created) = created {
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileTimesExt;
            handle.set_times(FileTimes::new().set_created(created))?;
        }
        #[cfg(not(windows))]
        let _ = (created, FileTimes::new());
    }
    handle.sync_all()?;
    drop(handle);
    fs::rename(temp, original)
}

/// Writes the edits to a file. On any error the file is exactly as it was.
pub fn write_tags(path: &Path, edits: &[FieldEdit]) -> Result<(), TagWriteError> {
    write_with(path, edits, |_| {})
}

/// `before_verifying` lets a test interfere between the edit and the check.
fn write_with(
    path: &Path,
    edits: &[FieldEdit],
    before_verifying: impl FnOnce(&Path),
) -> Result<(), TagWriteError> {
    let edits = clean_edits(edits)?;
    if fs::metadata(path)?.permissions().readonly() {
        return Err(TagWriteError::ReadOnly);
    }
    let Some(kind) = file_type(path)? else {
        return Err(TagWriteError::Unsupported(
            "this file's format was not recognized".to_owned(),
        ));
    };
    let started = stamp(path)?;

    let temp_file = temp_path(path)?;
    let mut temp = TempFile {
        path: temp_file.clone(),
        keep: false,
    };
    fs::copy(path, &temp_file)?;
    // The tag library has panicked on damaged files, which must not take the app down.
    panic::catch_unwind(AssertUnwindSafe(|| {
        apply_edits(&temp_file, kind, &edits)?;
        before_verifying(&temp_file);
        verify(path, &temp_file, kind, &edits)
    }))
    .map_err(|_| TagWriteError::Format("the tag library failed on this file".to_owned()))??;

    if stamp(path)? != started {
        return Err(TagWriteError::Changed);
    }
    replace(path, &temp_file)?;
    temp.keep = true;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;
    use crate::scan::fingerprint::fingerprint;

    const FRAME_LEN: usize = 417;

    fn syncsafe(value: usize) -> [u8; 4] {
        [21, 14, 7, 0].map(|shift| ((value >> shift) & 0x7f) as u8)
    }

    /// An ID3v2 tag of the given minor version from raw `(id, body)` frames.
    fn id3_tag(version: u8, frames: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (id, content) in frames {
            body.extend_from_slice(id.as_bytes());
            if version == 4 {
                body.extend(syncsafe(content.len()));
            } else {
                body.extend((content.len() as u32).to_be_bytes());
            }
            body.extend([0, 0]);
            body.extend_from_slice(content);
        }
        let mut tag = vec![b'I', b'D', b'3', version, 0, 0];
        tag.extend(syncsafe(body.len()));
        tag.extend(body);
        tag
    }

    fn latin1(text: &str) -> Vec<u8> {
        let mut body = vec![0];
        body.extend_from_slice(text.as_bytes());
        body
    }

    fn user_text(name: &str, value: &str) -> Vec<u8> {
        let mut body = vec![0];
        body.extend_from_slice(name.as_bytes());
        body.push(0);
        body.extend_from_slice(value.as_bytes());
        body
    }

    fn mpeg_audio() -> Vec<u8> {
        let mut audio = Vec::new();
        let mut state = 7_u32;
        for _ in 0..60 {
            audio.extend([0xff, 0xfb, 0x90, 0x00]);
            for _ in 4..FRAME_LEN {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                audio.push((state >> 24) as u8);
            }
        }
        audio
    }

    fn id3v1(title: &str) -> Vec<u8> {
        let mut tag = b"TAG".to_vec();
        let mut field = title.as_bytes().to_vec();
        field.resize(30, 0);
        tag.extend(field);
        tag.resize(127, 0);
        tag.push(255);
        tag
    }

    /// The usual shape of a file in the library: ID3v2.3 with custom and binary frames.
    fn standard_frames() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("TIT2", latin1("Old Title")),
            ("TPE1", latin1("Old Artist")),
            ("TALB", latin1("Old Album")),
            ("TRCK", latin1("3/12")),
            ("TCON", latin1("Rock")),
            ("TYER", latin1("1999")),
            ("TXXX", user_text("MY FIELD", "kept")),
            ("PRIV", b"owner\0private bytes\x01\x02\x03".to_vec()),
            ("COMM", {
                let mut body = vec![0];
                body.extend_from_slice(b"eng\0a comment");
                body
            }),
        ]
    }

    fn mp3_v23() -> Vec<u8> {
        [id3_tag(3, &standard_frames()), mpeg_audio()].concat()
    }

    fn flac() -> Vec<u8> {
        let comments = [
            ("TITLE", "Old Title"),
            ("ARTIST", "Old Artist"),
            ("ALBUM", "Old Album"),
            ("TRACKNUMBER", "3"),
            ("TRACKTOTAL", "12"),
            ("GENRE", "Rock"),
            ("DATE", "1999"),
            ("MY FIELD", "kept"),
        ];
        let mut block = 0_u32.to_le_bytes().to_vec();
        block.extend((comments.len() as u32).to_le_bytes());
        for (key, value) in comments {
            let entry = format!("{key}={value}");
            block.extend((entry.len() as u32).to_le_bytes());
            block.extend_from_slice(entry.as_bytes());
        }
        let mut stream_info = vec![0x10, 0, 0x10, 0, 0, 0, 0, 0, 0, 0];
        let packed: u64 = (44_100 << 44) | (1 << 41) | (15 << 36) | 441_000;
        stream_info.extend(packed.to_be_bytes());
        stream_info.extend([0; 16]);
        let mut file = b"fLaC".to_vec();
        file.extend([0, 0, 0, stream_info.len() as u8]);
        file.extend(stream_info);
        file.push(0x84);
        file.extend_from_slice(&(block.len() as u32).to_be_bytes()[1..]);
        file.extend(block);
        file.extend(mpeg_audio());
        file
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        path: PathBuf,
    }

    fn fixture(name: &str, bytes: &[u8]) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        fs::write(&path, bytes).unwrap();
        Fixture { _dir: dir, path }
    }

    fn edit(field: TagField, values: &[&str]) -> FieldEdit {
        FieldEdit {
            field,
            values: values.iter().map(|value| (*value).to_owned()).collect(),
        }
    }

    fn tags_of(path: &Path) -> TrackTags {
        read_back(path).unwrap().tags
    }

    fn all_edits() -> Vec<FieldEdit> {
        vec![
            edit(TagField::Title, &["New Title"]),
            edit(TagField::Album, &["New Album"]),
            edit(TagField::Artists, &["First", "Second"]),
            edit(TagField::AlbumArtists, &["Band"]),
            edit(TagField::Composers, &["Writer"]),
            edit(TagField::Genres, &["Jazz", "Blues"]),
            edit(TagField::Year, &["2004"]),
            edit(TagField::TrackNumber, &["7"]),
            edit(TagField::DiscNumber, &["2"]),
        ]
    }

    fn assert_all_edits_applied(tags: &TrackTags) {
        assert_eq!(tags.title.as_deref(), Some("New Title"));
        assert_eq!(tags.album.as_deref(), Some("New Album"));
        assert_eq!(tags.artists, ["First", "Second"]);
        assert_eq!(tags.album_artists, ["Band"]);
        assert_eq!(tags.composers, ["Writer"]);
        assert_eq!(tags.genres, ["Jazz", "Blues"]);
        assert_eq!(tags.year, Some(2004));
        assert_eq!(tags.track_no, Some(7));
        assert_eq!(tags.disc_no, Some(2));
    }

    fn no_temp_left(path: &Path) {
        let names: Vec<String> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
    }

    #[test]
    fn every_field_can_be_written_in_mp3_and_flac() {
        for (name, bytes) in [("a.mp3", mp3_v23()), ("a.flac", flac())] {
            let file = fixture(name, &bytes);
            write_tags(&file.path, &all_edits()).unwrap();
            assert_all_edits_applied(&tags_of(&file.path));
            no_temp_left(&file.path);
        }
    }

    #[test]
    fn an_id3v23_tag_stays_id3v23() {
        let file = fixture("a.mp3", &mp3_v23());
        write_tags(&file.path, &all_edits()).unwrap();
        let bytes = fs::read(&file.path).unwrap();
        assert_eq!(&bytes[..4], b"ID3\x03");
        // The version's own date frame, not the newer one.
        let tag_end = bytes
            .windows(4)
            .position(|w| w == [0xff, 0xfb, 0x90, 0x00])
            .unwrap();
        let tag = &bytes[..tag_end];
        assert!(tag.windows(4).any(|w| w == b"TYER"));
        assert!(!tag.windows(4).any(|w| w == b"TDRC"));
    }

    #[test]
    fn an_id3v24_tag_stays_id3v24() {
        let file = fixture(
            "a.mp3",
            &[id3_tag(4, &[("TIT2", latin1("Old"))]), mpeg_audio()].concat(),
        );
        write_tags(&file.path, &[edit(TagField::Title, &["New"])]).unwrap();
        assert_eq!(&fs::read(&file.path).unwrap()[..4], b"ID3\x04");
    }

    /// The frames the tag library would lose or damage, in their exact bytes.
    fn delicate_frames() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("TIT2", latin1("Old")),
            ("TSOP", latin1("Sort Name")),
            ("TSOT", latin1("Sort Title")),
            ("TMOO", latin1("Calm")),
            // A URL in a frame declared as UTF-16, with its address in Latin-1.
            (
                "WXXX",
                [&[1, 0xff, 0xfe, 0, 0][..], b"http://example.com"].concat(),
            ),
            // A comment ending in a terminator.
            ("COMM", [&[0][..], b"eng\0text\0"].concat()),
            // The older spelling of a JPEG's type.
            (
                "APIC",
                [
                    &[0][..],
                    b"image/jpg\0\x03\0",
                    &[0xff, 0xd8, 0xff, 0xe0, 1, 2],
                ]
                .concat(),
            ),
        ]
    }

    #[test]
    fn frames_that_were_not_edited_keep_their_exact_bytes() {
        for version in [3, 4] {
            let frames = delicate_frames();
            let file = fixture("a.mp3", &[id3_tag(version, &frames), mpeg_audio()].concat());
            write_tags(&file.path, &[edit(TagField::Title, &["New"])]).unwrap();
            let bytes = fs::read(&file.path).unwrap();
            for (id, body) in &frames[1..] {
                let wanted = [id.as_bytes(), body].concat();
                let found = bytes.windows(4).position(|window| window == id.as_bytes());
                let at = found.unwrap_or_else(|| panic!("{id} is gone"));
                assert_eq!(&bytes[at..at + 4], &wanted[..4]);
                assert_eq!(&bytes[at + 10..at + 10 + body.len()], &body[..], "{id}");
            }
        }
    }

    #[test]
    fn the_audio_does_not_move_when_the_new_tag_fits() {
        let mut tag = id3_tag(3, &[("TIT2", latin1("A long enough old title to shrink"))]);
        // Add padding inside the tag.
        let body_len = tag.len() - 10 + 500;
        tag.resize(tag.len() + 500, 0);
        tag[6..10].copy_from_slice(&syncsafe(body_len));
        let bytes = [tag.clone(), mpeg_audio()].concat();
        let file = fixture("a.mp3", &bytes);
        write_tags(&file.path, &[edit(TagField::Title, &["Short"])]).unwrap();
        let after = fs::read(&file.path).unwrap();
        assert_eq!(after.len(), bytes.len());
        assert_eq!(&after[tag.len()..], &bytes[tag.len()..]);
    }

    #[test]
    fn a_tag_that_grows_moves_the_audio_intact_and_leaves_room() {
        let file = fixture("a.mp3", &mp3_v23());
        let long = "A very long title ".repeat(200);
        write_tags(&file.path, &[edit(TagField::Title, &[long.trim()])]).unwrap();
        assert_eq!(tags_of(&file.path).title.as_deref(), Some(long.trim()));
        let after = fs::read(&file.path).unwrap();
        let audio = mpeg_audio();
        assert!(after.windows(audio.len()).any(|window| window == audio));

        // A second small edit fits in the room left, so the file stays the same size.
        let before = after.len();
        write_tags(&file.path, &[edit(TagField::Album, &["New Album"])]).unwrap();
        assert_eq!(fs::metadata(&file.path).unwrap().len() as usize, before);
    }

    #[test]
    fn a_tag_with_unreadable_contents_is_refused_rather_than_lost() {
        let mut tag = id3_tag(3, &[("TIT2", latin1("Old"))]);
        let body_len = tag.len() - 10 + 8;
        tag.extend([0, 0, 0, 0, 9, 9, 9, 9]);
        tag[6..10].copy_from_slice(&syncsafe(body_len));
        let bytes = [tag, mpeg_audio()].concat();
        let file = fixture("a.mp3", &bytes);
        let error = write_tags(&file.path, &[edit(TagField::Title, &["New"])]).unwrap_err();
        assert!(matches!(error, TagWriteError::Unsupported(_)), "{error}");
        assert_eq!(fs::read(&file.path).unwrap(), bytes);
    }

    #[test]
    fn padding_outside_the_tag_is_taken_in_and_the_audio_stays_put() {
        // More zeros than the tag library looks past, then an ID3v1 tag it must write.
        let tag = id3_tag(3, &[("TIT2", latin1("Old"))]);
        let gap = vec![0; 8000];
        let audio = mpeg_audio();
        let bytes = [tag.clone(), gap.clone(), audio.clone(), id3v1("Old")].concat();
        let file = fixture("a.mp3", &bytes);
        write_tags(&file.path, &[edit(TagField::Title, &["New"])]).unwrap();
        assert_eq!(tags_of(&file.path).title.as_deref(), Some("New"));
        let after = fs::read(&file.path).unwrap();
        assert_eq!(after.len(), bytes.len());
        let audio_at = tag.len() + gap.len();
        assert_eq!(&after[audio_at..audio_at + audio.len()], &audio[..]);
    }

    #[test]
    fn a_file_with_no_tag_gets_one() {
        let file = fixture("a.mp3", &mpeg_audio());
        write_tags(&file.path, &[edit(TagField::Title, &["New"])]).unwrap();
        assert_eq!(tags_of(&file.path).title.as_deref(), Some("New"));
        assert_eq!(&fs::read(&file.path).unwrap()[..4], b"ID3\x03");
    }

    #[test]
    fn an_id3v22_tag_is_refused_and_left_alone() {
        // Version 2.2 frames have three-letter names and three-byte sizes.
        let content = latin1("Old");
        let mut body = b"TT2".to_vec();
        body.extend_from_slice(&(content.len() as u32).to_be_bytes()[1..]);
        body.extend(content);
        let mut tag = vec![b'I', b'D', b'3', 2, 0, 0];
        tag.extend(syncsafe(body.len()));
        tag.extend(body);
        let bytes = [tag, mpeg_audio()].concat();
        let file = fixture("a.mp3", &bytes);

        let error = write_tags(&file.path, &[edit(TagField::Title, &["New"])]).unwrap_err();
        assert!(matches!(error, TagWriteError::Unsupported(_)), "{error}");
        assert_eq!(fs::read(&file.path).unwrap(), bytes);
        no_temp_left(&file.path);
    }

    #[test]
    fn the_audio_is_identical_byte_for_byte_and_the_fingerprint_holds() {
        for (name, bytes) in [("a.mp3", mp3_v23()), ("a.flac", flac())] {
            let file = fixture(name, &bytes);
            let print = fingerprint(&mut File::open(&file.path).unwrap(), 1).unwrap();
            write_tags(&file.path, &all_edits()).unwrap();
            let after = fs::read(&file.path).unwrap();
            let audio = mpeg_audio();
            assert!(
                after.windows(audio.len()).any(|window| window == audio),
                "{name}: the audio bytes are not intact"
            );
            assert_eq!(
                fingerprint(&mut File::open(&file.path).unwrap(), 1).unwrap(),
                print
            );
        }
    }

    #[test]
    fn fields_that_were_not_edited_are_kept_including_unknown_frames() {
        let file = fixture("a.mp3", &mp3_v23());
        write_tags(&file.path, &[edit(TagField::Title, &["New"])]).unwrap();
        let tags = tags_of(&file.path);
        assert_eq!(tags.artists, ["Old Artist"]);
        assert_eq!(tags.track_no, Some(3));
        assert!(
            tags.raw
                .iter()
                .any(|(key, value)| key == "MY FIELD" && value == "kept")
        );
        let bytes = fs::read(&file.path).unwrap();
        assert!(
            bytes.windows(19).any(|w| w == b"owner\0private bytes"),
            "the private frame"
        );
        assert!(bytes.windows(9).any(|w| w == b"a comment"));

        let file = fixture("a.flac", &flac());
        write_tags(&file.path, &[edit(TagField::Title, &["New"])]).unwrap();
        let tags = tags_of(&file.path);
        assert_eq!(tags.artists, ["Old Artist"]);
        assert!(
            tags.raw
                .iter()
                .any(|(key, value)| key == "MY FIELD" && value == "kept")
        );
    }

    #[test]
    fn editing_a_track_number_keeps_the_total() {
        let file = fixture("a.mp3", &mp3_v23());
        write_tags(&file.path, &[edit(TagField::TrackNumber, &["7"])]).unwrap();
        let mut handle = File::open(&file.path).unwrap();
        let read = MpegFile::read_from(&mut handle, parse_options(false)).unwrap();
        let tag = read.id3v2().unwrap();
        assert_eq!((tag.track(), tag.track_total()), (Some(7), Some(12)));
    }

    #[test]
    fn an_id3v1_tag_is_kept_in_step() {
        let bytes = [mp3_v23(), id3v1("Old Title")].concat();
        let file = fixture("a.mp3", &bytes);
        write_tags(&file.path, &[edit(TagField::Title, &["New Title"])]).unwrap();
        let after = fs::read(&file.path).unwrap();
        let tail = &after[after.len() - 128..];
        assert_eq!(&tail[..3], b"TAG");
        assert_eq!(&tail[3..12], b"New Title");
    }

    #[test]
    fn a_field_can_be_removed() {
        for (name, bytes) in [("a.mp3", mp3_v23()), ("a.flac", flac())] {
            let file = fixture(name, &bytes);
            write_tags(
                &file.path,
                &[edit(TagField::Album, &[]), edit(TagField::Genres, &[])],
            )
            .unwrap();
            let tags = tags_of(&file.path);
            assert_eq!(tags.album, None, "{name}");
            assert!(tags.genres.is_empty(), "{name}");
            assert_eq!(tags.title.as_deref(), Some("Old Title"), "{name}");
        }
    }

    #[test]
    fn a_custom_list_field_gives_way_to_the_edit() {
        let bytes = [
            id3_tag(
                3,
                &[
                    ("TPE1", latin1("A feat. B")),
                    ("TXXX", user_text("ARTISTS", "A")),
                ],
            ),
            mpeg_audio(),
        ]
        .concat();
        let file = fixture("a.mp3", &bytes);
        write_tags(&file.path, &[edit(TagField::Artists, &["C"])]).unwrap();
        assert_eq!(tags_of(&file.path).artists, ["C"]);
    }

    #[test]
    fn a_failed_check_leaves_the_original_untouched() {
        let bytes = mp3_v23();
        let file = fixture("a.mp3", &bytes);
        let error = write_with(&file.path, &[edit(TagField::Title, &["New"])], |temp| {
            // Damage a byte of the copy's audio, as a bad write could.
            let mut copy = fs::read(temp).unwrap();
            let last = copy.len() - 1;
            copy[last] ^= 0xff;
            fs::write(temp, copy).unwrap();
        })
        .unwrap_err();
        assert!(matches!(error, TagWriteError::Verification(_)), "{error}");
        assert_eq!(fs::read(&file.path).unwrap(), bytes);
        no_temp_left(&file.path);
    }

    #[test]
    fn a_lost_frame_fails_the_check() {
        let bytes = mp3_v23();
        let file = fixture("a.mp3", &bytes);
        let error = write_with(&file.path, &[edit(TagField::Title, &["New"])], |temp| {
            let mut handle = open_rw(temp).unwrap();
            let mut copy = MpegFile::read_from(&mut handle, parse_options(true)).unwrap();
            let _ = copy
                .id3v2_mut()
                .unwrap()
                .remove(&lofty::id3::v2::FrameId::Valid("PRIV".into()));
            handle.rewind().unwrap();
            copy.save_to(&mut handle, WriteOptions::new().use_id3v23(true))
                .unwrap();
        })
        .unwrap_err();
        assert!(matches!(error, TagWriteError::Verification(_)), "{error}");
        assert_eq!(fs::read(&file.path).unwrap(), bytes);
    }

    #[test]
    fn a_file_that_changes_meanwhile_is_not_overwritten() {
        let file = fixture("a.mp3", &mp3_v23());
        let original = file.path.clone();
        let error = write_with(&file.path, &[edit(TagField::Title, &["New"])], |_| {
            // Another program saves the file during the edit.
            let mut bytes = fs::read(&original).unwrap();
            bytes.extend([0; 16]);
            fs::write(&original, bytes).unwrap();
        });
        assert!(error.is_err());
        assert_ne!(tags_of(&file.path).title.as_deref(), Some("New"));
        no_temp_left(&file.path);
    }

    #[test]
    fn a_read_only_file_is_refused() {
        let bytes = mp3_v23();
        let file = fixture("a.mp3", &bytes);
        let mut permissions = fs::metadata(&file.path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&file.path, permissions.clone()).unwrap();
        let error = write_tags(&file.path, &[edit(TagField::Title, &["New"])]).unwrap_err();
        assert!(matches!(error, TagWriteError::ReadOnly));
        assert_eq!(fs::read(&file.path).unwrap(), bytes);
        // So the temporary folder can be removed.
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(&file.path, permissions).unwrap();
    }

    #[test]
    fn a_damaged_file_is_refused_and_left_alone() {
        let bytes = b"not music at all, but with a music name".to_vec();
        let file = fixture("a.mp3", &bytes);
        assert!(write_tags(&file.path, &[edit(TagField::Title, &["New"])]).is_err());
        assert_eq!(fs::read(&file.path).unwrap(), bytes);
        no_temp_left(&file.path);
    }

    #[test]
    fn bad_edits_are_refused_before_touching_anything() {
        let bytes = mp3_v23();
        let file = fixture("a.mp3", &bytes);
        for edits in [
            vec![edit(TagField::TrackNumber, &["seven"])],
            vec![edit(TagField::Year, &["0"])],
            vec![edit(TagField::Year, &["99999"])],
            vec![edit(TagField::Title, &["One", "Two"])],
            vec![
                edit(TagField::Title, &["One"]),
                edit(TagField::Title, &["Two"]),
            ],
        ] {
            let error = write_tags(&file.path, &edits).unwrap_err();
            assert!(matches!(error, TagWriteError::Invalid(_)), "{error}");
        }
        assert_eq!(fs::read(&file.path).unwrap(), bytes);
    }

    #[test]
    fn values_are_tidied_but_kept_as_given() {
        let file = fixture("a.mp3", &mp3_v23());
        write_tags(
            &file.path,
            &[edit(
                TagField::Artists,
                &[" AC/DC ", "", "AC/DC", "Dr. Dre"],
            )],
        )
        .unwrap();
        // Never split on a slash inside a value.
        assert_eq!(tags_of(&file.path).artists, ["AC/DC", "Dr. Dre"]);
    }

    #[test]
    fn writing_works_while_the_file_is_open_for_playback() {
        let file = fixture("a.mp3", &mp3_v23());
        let mut playing = File::open(&file.path).unwrap();
        let mut head = [0; 3];
        playing.read_exact(&mut head).unwrap();
        write_tags(&file.path, &[edit(TagField::Title, &["New"])]).unwrap();
        assert_eq!(tags_of(&file.path).title.as_deref(), Some("New"));
    }

    #[test]
    fn a_leftover_temporary_file_from_a_crash_does_no_harm() {
        let file = fixture("a.mp3", &mp3_v23());
        fs::write(temp_path(&file.path).unwrap(), b"left behind").unwrap();
        write_tags(&file.path, &[edit(TagField::Title, &["New"])]).unwrap();
        assert_eq!(tags_of(&file.path).title.as_deref(), Some("New"));
        no_temp_left(&file.path);
    }

    #[test]
    fn the_creation_time_survives() {
        let file = fixture("a.mp3", &mp3_v23());
        let created = fs::metadata(&file.path).unwrap().created().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        write_tags(&file.path, &[edit(TagField::Title, &["New"])]).unwrap();
        assert_eq!(
            fs::metadata(&file.path).unwrap().created().unwrap(),
            created
        );
    }
}
