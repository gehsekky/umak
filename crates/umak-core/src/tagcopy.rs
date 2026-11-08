//! Copies a file's tags and embedded art onto a converted copy of it, before the copy is put
//! in place. Values arrive as written: between files with the same kind of tag, the tag is
//! copied item for item, and ID3v2 frame for frame in the same version; between kinds, each
//! field goes to its counterpart, and fields of the user's own go across by name.
//!
//! Tags that describe the old file's encoding rather than the music are left behind, since
//! they would be wrong for the new one. Anything else the new format has no place for is
//! reported, never dropped silently, and the result is read back and compared to make sure.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, Seek};
use std::path::Path;

use lofty::TextEncoding;
use lofty::config::WriteOptions;
use lofty::file::{AudioFile, FileType, TaggedFileExt};
use lofty::flac::FlacFile;
use lofty::id3::v2::{ExtendedTextFrame, Frame, Id3v2Tag};
use lofty::mp4::{Atom, AtomData, AtomIdent, Ilst, Mp4File};
use lofty::mpeg::MpegFile;
use lofty::ogg::tag::VorbisComments;
use lofty::ogg::{OggPictureStorage, OpusFile};
use lofty::picture::{Picture, PictureInformation};
use lofty::prelude::SplitTag;
use lofty::probe::Probe;
use lofty::tag::{ItemKey, ItemValue, Tag, TagExt, TagItem, TagType};

use crate::tagwrite::{
    TagWriteError, copy_id3v2, format_error, keep_one_byte_atoms, parse_options,
};

/// Fields that describe how the old file was encoded, by generic key.
const ENCODING_KEYS: [ItemKey; 2] = [ItemKey::EncoderSoftware, ItemKey::EncoderSettings];
/// The same, by the names they have in each format.
const ENCODING_NAMES: [&str; 6] = [
    "TSSE",
    "ENCODER",
    "©too",
    "iTunSMPB",
    "Encoding Params",
    "iTunPGAP",
];
/// The mean of the freeform MP4 atoms that hold fields of the user's own.
const ITUNES_MEAN: &str = "com.apple.iTunes";

/// What was not carried across.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagCopyReport {
    /// Fields by the name they have in the source, and `picture` for art, each once.
    pub left_out: Vec<String>,
}

fn is_encoding_name(name: &str) -> bool {
    ENCODING_NAMES
        .iter()
        .any(|known| known.eq_ignore_ascii_case(name))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Id3v2,
    Vorbis,
    Mp4,
}

fn family(kind: FileType) -> Option<Family> {
    match kind {
        FileType::Mpeg => Some(Family::Id3v2),
        FileType::Flac | FileType::Opus | FileType::Vorbis => Some(Family::Vorbis),
        FileType::Mp4 => Some(Family::Mp4),
        _ => None,
    }
}

impl Family {
    fn tag_type(self) -> TagType {
        match self {
            Self::Id3v2 => TagType::Id3v2,
            Self::Vorbis => TagType::VorbisComments,
            Self::Mp4 => TagType::Mp4Ilst,
        }
    }
}

/// Everything a tag holds, in a form that does not depend on its format.
#[derive(Debug, Default)]
struct Contents {
    /// Fields with a generic key, which each format names in its own way.
    items: Vec<(ItemKey, String)>,
    /// Fields of the user's own, by name.
    custom: Vec<(String, String)>,
    /// Fields only the source's format has, by name.
    other: Vec<String>,
    pictures: Vec<Picture>,
}

impl Contents {
    fn from_split(tag: &Tag, custom: Vec<(String, String)>, other: Vec<String>) -> Self {
        let mut items = Vec::new();
        let mut other = other;
        for item in tag.items() {
            match item.value() {
                ItemValue::Text(text) | ItemValue::Locator(text) => {
                    items.push((item.key(), text.clone()));
                }
                ItemValue::Binary(_) => other.push(key_name(item.key(), tag.tag_type())),
            }
        }
        Self {
            items,
            custom,
            other,
            pictures: tag.pictures().to_vec(),
        }
    }

    fn without_encoding_fields(mut self) -> Self {
        self.items.retain(|(key, _)| !ENCODING_KEYS.contains(key));
        self.custom.retain(|(name, _)| !is_encoding_name(name));
        self.other.retain(|name| !is_encoding_name(name));
        self
    }
}

/// ID3 keeps lyrics in a frame the tag library names apart from the field the other formats
/// use, though both are the lyrics.
fn lyrics_key(key: ItemKey, target: Family) -> ItemKey {
    match key {
        ItemKey::Lyrics | ItemKey::UnsyncLyrics if target == Family::Id3v2 => ItemKey::UnsyncLyrics,
        ItemKey::UnsyncLyrics => ItemKey::Lyrics,
        key => key,
    }
}

/// A freeform MP4 name without its `----:mean:` prefix.
fn plain_name(name: &str) -> &str {
    match name.strip_prefix("----:") {
        Some(rest) => rest.split_once(':').map_or(rest, |(_, name)| name),
        None => name,
    }
}

/// The name a field with no place in the target can go across under: the one it had in the
/// source. An ID3 frame ID means nothing as a name of its own, so those have none.
fn carried_name(key: ItemKey, source: Family) -> Option<String> {
    let name = plain_name(key.map_key(source.tag_type())?);
    let is_frame_id = name.len() == 4
        && name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit());
    (!(source == Family::Id3v2 && is_frame_id)).then(|| name.to_owned())
}

fn key_name(key: ItemKey, tag_type: TagType) -> String {
    key.map_key(tag_type)
        .map_or_else(|| format!("{key:?}"), str::to_owned)
}

fn id3v2_contents(tag: Id3v2Tag) -> Contents {
    let (remainder, generic) = tag.split_tag();
    let mut custom = Vec::new();
    let mut other = Vec::new();
    for frame in &*remainder {
        match frame {
            // Version 2.4 keeps several values in one frame, apart by a null.
            Frame::UserText(frame) => custom.extend(
                frame
                    .content
                    .split('\0')
                    .map(|value| (frame.description.to_string(), value.to_owned())),
            ),
            Frame::Comment(frame) => other.push(format!("COMM:{}", frame.description)),
            Frame::UnsynchronizedText(frame) => other.push(format!("USLT:{}", frame.description)),
            frame => other.push(frame.id_str().to_owned()),
        }
    }
    Contents::from_split(&generic, custom, other)
}

fn vorbis_contents(tag: VorbisComments, pictures: Vec<Picture>) -> Contents {
    let (remainder, mut generic) = tag.split_tag();
    for picture in pictures {
        generic.push_picture(picture);
    }
    let custom = remainder
        .items()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    Contents::from_split(&generic, custom, Vec::new())
}

fn mp4_contents(tag: Ilst) -> Contents {
    let (remainder, generic) = tag.split_tag();
    let mut custom = Vec::new();
    let mut other = Vec::new();
    for atom in &*remainder {
        match atom.ident() {
            AtomIdent::Freeform { name, .. } => {
                for data in atom.data() {
                    match data {
                        AtomData::UTF8(text) | AtomData::UTF16(text) => {
                            custom.push((name.to_string(), text.clone()));
                        }
                        _ => other.push(name.to_string()),
                    }
                }
            }
            AtomIdent::Fourcc(code) => other.push(code.iter().map(|byte| *byte as char).collect()),
        }
    }
    Contents::from_split(&generic, custom, other)
}

/// Reads a file's tags, with art.
fn read_contents(path: &Path) -> Result<(FileType, Contents), TagWriteError> {
    // With the tag library's conversions, so that an ID3v2.3 date kept in TYER and TDAT
    // arrives as one date. Nothing read this way is saved back to the source.
    let options = parse_options(true).implicit_conversions(true);
    let mut reader = BufReader::new(File::open(path)?);
    let kind = Probe::new(&mut reader)
        .options(options)
        .guess_file_type()?
        .file_type()
        .ok_or_else(|| TagWriteError::Unsupported("the file's format is not known".to_owned()))?;
    reader.rewind()?;
    let contents = match kind {
        FileType::Mpeg => {
            let mut file =
                MpegFile::read_from(&mut reader, options).map_err(|error| format_error(&error))?;
            match file.remove_id3v2() {
                Some(tag) => id3v2_contents(tag),
                // An APE or ID3v1 tag alone, read for the fields the tag library maps.
                None => {
                    let tagged: lofty::file::TaggedFile = file.into();
                    match tagged.primary_tag().or_else(|| tagged.first_tag()) {
                        Some(tag) => Contents::from_split(tag, Vec::new(), Vec::new()),
                        None => Contents::default(),
                    }
                }
            }
        }
        FileType::Flac => {
            let mut file =
                FlacFile::read_from(&mut reader, options).map_err(|error| format_error(&error))?;
            let pictures = file
                .pictures()
                .iter()
                .map(|(picture, _)| picture.clone())
                .collect();
            let tag = file.remove_vorbis_comments().unwrap_or_default();
            vorbis_contents(tag, pictures)
        }
        FileType::Opus => {
            let mut file =
                OpusFile::read_from(&mut reader, options).map_err(|error| format_error(&error))?;
            let tag = std::mem::take(file.vorbis_comments_mut());
            vorbis_contents(tag, Vec::new())
        }
        FileType::Mp4 => {
            let mut file =
                Mp4File::read_from(&mut reader, options).map_err(|error| format_error(&error))?;
            mp4_contents(file.remove_ilst().unwrap_or_default())
        }
        other => {
            return Err(TagWriteError::Unsupported(format!(
                "tags in {other:?} files cannot be copied yet"
            )));
        }
    };
    Ok((kind, contents))
}

/// Builds a tag of the target's kind from the contents, noting what it cannot hold.
fn write_across(
    contents: &Contents,
    source: Family,
    target: Family,
    handle: &mut File,
    left_out: &mut Vec<String>,
) -> Result<(), TagWriteError> {
    let mut generic = Tag::new(target.tag_type());
    // Several values under one name stay together, in their order.
    let mut custom: Vec<(String, Vec<String>)> = Vec::new();
    let mut add_custom = |name: &str, value: &str| match custom
        .iter_mut()
        .find(|(known, _)| known.eq_ignore_ascii_case(name))
    {
        Some((_, values)) => values.push(value.to_owned()),
        None => custom.push((name.to_owned(), vec![value.to_owned()])),
    };
    for (key, value) in &contents.items {
        let key = lyrics_key(*key, target);
        let item = TagItem::new(key, ItemValue::Text(value.clone()));
        if key.map_key(target.tag_type()).is_some() && generic.push(item) {
            continue;
        }
        // A field the target has no place of its own for goes across under the name it
        // had, as fields of the user's own do, where that name means something.
        match carried_name(key, source) {
            Some(name) => add_custom(&name, value),
            None => left_out.push(key_name(key, source.tag_type())),
        }
    }
    for picture in &contents.pictures {
        generic.push_picture(picture.clone());
    }
    left_out.extend(contents.other.iter().cloned());
    for (name, value) in &contents.custom {
        add_custom(name, value);
    }

    let options = WriteOptions::new();
    match target {
        Family::Id3v2 => {
            let mut tag = Id3v2Tag::from(generic);
            for (name, values) in custom {
                tag.insert(Frame::UserText(ExtendedTextFrame::new(
                    TextEncoding::UTF8,
                    name.to_owned(),
                    values.join("\0"),
                )));
            }
            tag.save_to(handle, options)
                .map_err(|error| format_error(&error))?;
        }
        Family::Vorbis => {
            let mut tag = VorbisComments::from(generic);
            for (name, values) in custom {
                // A comment's name is plain ASCII, without `=`.
                if name
                    .bytes()
                    .all(|byte| (0x20..=0x7d).contains(&byte) && byte != b'=')
                {
                    for value in values {
                        tag.push(name.to_owned(), value.to_owned());
                    }
                } else {
                    left_out.push(name.to_owned());
                }
            }
            tag.save_to(handle, options)
                .map_err(|error| format_error(&error))?;
        }
        Family::Mp4 => {
            let mut tag = Ilst::from(generic);
            for (name, values) in custom {
                let ident = AtomIdent::Freeform {
                    mean: ITUNES_MEAN.into(),
                    name: name.to_owned().into(),
                };
                let data = values
                    .iter()
                    .map(|value| AtomData::UTF8((*value).to_owned()))
                    .collect();
                if let Some(atom) = Atom::from_collection(ident, data) {
                    tag.insert(atom);
                }
            }
            tag.save_to(handle, options)
                .map_err(|error| format_error(&error))?;
        }
    }
    Ok(())
}

/// Copies a tag between files with the same kind, item for item.
fn write_alike(source: &Path, kind: FileType, handle: &mut File) -> Result<(), TagWriteError> {
    let options = parse_options(true);
    let mut reader = BufReader::new(File::open(source)?);
    let write = WriteOptions::new();
    match family(kind) {
        Some(Family::Vorbis) => {
            let (tag, pictures): (VorbisComments, Vec<(Picture, PictureInformation)>) = match kind {
                FileType::Flac => {
                    let mut file = FlacFile::read_from(&mut reader, options)
                        .map_err(|error| format_error(&error))?;
                    let pictures = file.pictures().to_vec();
                    (file.remove_vorbis_comments().unwrap_or_default(), pictures)
                }
                _ => {
                    let mut file = OpusFile::read_from(&mut reader, options)
                        .map_err(|error| format_error(&error))?;
                    let tag = std::mem::take(file.vorbis_comments_mut());
                    let pictures = tag.pictures().to_vec();
                    (tag, pictures)
                }
            };
            let mut copy = VorbisComments::new();
            for (key, value) in tag.items() {
                if !is_encoding_name(key) {
                    copy.push(key.to_owned(), value.to_owned());
                }
            }
            for (picture, information) in pictures {
                copy.insert_picture(picture, Some(information))
                    .map_err(|error| format_error(&error))?;
            }
            copy.save_to(handle, write)
                .map_err(|error| format_error(&error))?;
        }
        Some(Family::Mp4) => {
            let mut file =
                Mp4File::read_from(&mut reader, options).map_err(|error| format_error(&error))?;
            let mut tag = file.remove_ilst().unwrap_or_default();
            tag.retain(|atom| match atom.ident() {
                AtomIdent::Fourcc(code) => code[..] != b"\xa9too"[..],
                AtomIdent::Freeform { name, .. } => !is_encoding_name(name),
            });
            keep_one_byte_atoms(&mut tag);
            tag.save_to(handle, write)
                .map_err(|error| format_error(&error))?;
        }
        Some(Family::Id3v2) | None => {
            return Err(TagWriteError::Invalid(format!(
                "{kind:?} tags are not copied this way"
            )));
        }
    }
    Ok(())
}

/// A value as each format can hold it: numbers are compared as numbers, since some formats
/// keep a track number as text and others as a number.
fn comparable(value: &str) -> String {
    let value = value.trim();
    value
        .parse::<u32>()
        .map_or_else(|_| value.to_owned(), |number| number.to_string())
}

/// The name a field has in the target, so that fields compare however each side came to
/// hold them: a field of the user's own may read back as one the target knows, as
/// `MUSICBRAINZ_ALBUMID` does in a Vorbis comment.
fn target_name(key: ItemKey, source: Family, target: Family) -> String {
    let key = lyrics_key(key, target);
    let name = key
        .map_key(target.tag_type())
        .or_else(|| key.map_key(source.tag_type()))
        .map_or_else(|| format!("{key:?}"), str::to_owned);
    plain_name(&name).to_uppercase()
}

/// The name of a field of the user's own as the target reads it back, which may be a field
/// the target knows: `totaldiscs` in a Vorbis comment is its disc total.
fn custom_target_name(name: &str, target: Family) -> String {
    let name = plain_name(name);
    match ItemKey::from_key(target.tag_type(), name) {
        Some(key) => target_name(key, target, target),
        None => name.to_uppercase(),
    }
}

/// Names whatever value of the source the copy lacks, once each, by the source's names. A
/// value a field holds twice counts once, since some formats keep one value per field.
fn missing_from(
    expected: &Contents,
    found: &Contents,
    source: Family,
    target: Family,
) -> Vec<String> {
    let mut missing = Vec::new();
    let mut note = |name: String| {
        if !missing.contains(&name) {
            missing.push(name);
        }
    };

    let mut have: HashSet<(String, String)> = HashSet::new();
    for (key, value) in &found.items {
        have.insert((target_name(*key, target, target), comparable(value)));
    }
    for (name, value) in &found.custom {
        have.insert((custom_target_name(name, target), comparable(value)));
    }
    for (key, value) in &expected.items {
        if !have.contains(&(target_name(*key, source, target), comparable(value))) {
            note(key_name(*key, source.tag_type()));
        }
    }
    for (name, value) in &expected.custom {
        if !have.contains(&(custom_target_name(name, target), comparable(value))) {
            note(name.clone());
        }
    }

    let mut have: Vec<&[u8]> = found.pictures.iter().map(Picture::data).collect();
    for picture in &expected.pictures {
        match have.iter().position(|data| *data == picture.data()) {
            Some(index) => {
                have.swap_remove(index);
            }
            None => note("picture".to_owned()),
        }
    }
    missing
}

/// Copies the tags and art of `source` onto `target`, a converted copy of it that has no
/// tags yet, and checks the result by reading it back.
pub fn copy_tags(source: &Path, target: &Path) -> Result<TagCopyReport, TagWriteError> {
    let (source_kind, contents) = read_contents(source)?;
    let contents = contents.without_encoding_fields();
    let (target_kind, _) = read_contents(target)?;
    let unsupported = |kind: FileType| {
        TagWriteError::Unsupported(format!("tags in {kind:?} files cannot be copied yet"))
    };
    let from = family(source_kind).ok_or_else(|| unsupported(source_kind))?;
    let to = family(target_kind).ok_or_else(|| unsupported(target_kind))?;

    let mut left_out = Vec::new();
    let mut handle = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(target)?;
    let copied_whole = match (from, to) {
        (Family::Id3v2, Family::Id3v2) => {
            // A version 2.2 tag cannot be copied as it is, and goes field by field instead.
            match copy_id3v2(&mut File::open(source)?, &mut handle, |id, description| {
                !is_encoding_name(id) && !description.is_some_and(is_encoding_name)
            }) {
                Ok(copied) => copied,
                Err(TagWriteError::Unsupported(_)) => false,
                Err(error) => return Err(error),
            }
        }
        _ if from == to && source_kind != FileType::Mpeg => {
            write_alike(source, source_kind, &mut handle)?;
            true
        }
        _ => false,
    };
    if !copied_whole {
        handle.rewind()?;
        write_across(&contents, from, to, &mut handle, &mut left_out)?;
    }
    handle.sync_all()?;
    drop(handle);

    // What was not written on purpose is expected to be missing; anything else is reported.
    let (_, found) = read_contents(target)?;
    let mut expected = contents;
    expected
        .items
        .retain(|(key, _)| !left_out.contains(&key_name(lyrics_key(*key, to), from.tag_type())));
    expected.custom.retain(|(name, _)| !left_out.contains(name));
    for name in missing_from(&expected, &found, from, to) {
        if !left_out.contains(&name) {
            left_out.push(name);
        }
    }
    let mut unique = Vec::new();
    for name in left_out {
        if !unique.contains(&name) {
            unique.push(name);
        }
    }
    Ok(TagCopyReport { left_out: unique })
}
