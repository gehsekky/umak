//! What a file's tags hold for some fields, exactly as written, so an edit can be undone
//! without loss. Going through the tag model would not do: it reads "2001-05-03" as the year
//! 2001, and an undo from there would lose the rest of the date.

use std::borrow::Cow;
use std::fs::File;
use std::path::Path;

use lofty::ape::{ApeItem, ApeTag};
use lofty::file::AudioFile;
use lofty::file::FileType;
use lofty::flac::FlacFile;
use lofty::id3::v1::Id3v1Tag;
use lofty::mp4::{Atom, AtomData, AtomIdent, DataType, Ilst, Mp4File};
use lofty::mpeg::MpegFile;
use lofty::ogg::tag::VorbisComments;
use lofty::tag::ItemValue;
use serde::{Deserialize, Serialize};

use super::{
    TagField, TagWriteError, ape_keys, format_error, id3v2, is_edited_atom, parse_options,
    vorbis_keys,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedFrame {
    pub id: [u8; 4],
    pub flags: [u8; 2],
    pub body: Vec<u8>,
}

/// The ID3v1 fields being edited. The others stay empty here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct SavedId3v1 {
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    year: Option<u16>,
    track: Option<u8>,
    genre: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum SavedApeValue {
    Text(String),
    Locator(String),
    Binary(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum SavedAtomName {
    Fourcc([u8; 4]),
    Freeform { mean: String, name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum SavedAtomData {
    Utf8(String),
    Utf16(String),
    Signed(i32),
    Unsigned(u32),
    Bool(bool),
    Other { code: u32, data: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SavedAtom {
    name: SavedAtomName,
    data: Vec<SavedAtomData>,
}

/// Everything an edit of these fields would replace, in each tag the file has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagSnapshot {
    fields: Vec<TagField>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    id3v2: Vec<SavedFrame>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id3v1: Option<SavedId3v1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    ape: Vec<(String, SavedApeValue)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    vorbis: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    mp4: Vec<SavedAtom>,
}

impl TagSnapshot {
    fn empty(fields: &[TagField]) -> Self {
        Self {
            fields: fields.to_vec(),
            id3v2: Vec::new(),
            id3v1: None,
            ape: Vec::new(),
            vorbis: Vec::new(),
            mp4: Vec::new(),
        }
    }

    pub(super) fn capture(
        path: &Path,
        kind: FileType,
        fields: &[TagField],
    ) -> Result<Self, TagWriteError> {
        let mut snapshot = Self::empty(fields);
        let mut handle = File::open(path)?;
        let options = parse_options(false);
        match kind {
            FileType::Mpeg => {
                snapshot.id3v2 = id3v2::saved_frames(&mut handle, fields)?;
                std::io::Seek::rewind(&mut handle)?;
                let file = MpegFile::read_from(&mut handle, options)
                    .map_err(|error| format_error(&error))?;
                snapshot.id3v1 = file.id3v1().map(|tag| save_id3v1(tag, fields));
                if let Some(tag) = file.ape() {
                    snapshot.ape = save_ape(tag, fields);
                }
            }
            FileType::Flac => {
                let file = FlacFile::read_from(&mut handle, options)
                    .map_err(|error| format_error(&error))?;
                if let Some(tag) = file.vorbis_comments() {
                    let keys: Vec<&str> = fields
                        .iter()
                        .flat_map(|field| vorbis_keys(*field).iter().copied())
                        .collect();
                    snapshot.vorbis = tag
                        .items()
                        .filter(|(key, _)| {
                            keys.iter().any(|edited| edited.eq_ignore_ascii_case(key))
                        })
                        .map(|(key, value)| (key.to_owned(), value.to_owned()))
                        .collect();
                }
            }
            FileType::Mp4 => {
                let file = Mp4File::read_from(&mut handle, options)
                    .map_err(|error| format_error(&error))?;
                if let Some(tag) = file.ilst() {
                    snapshot.mp4 = save_ilst(tag, fields)?;
                }
            }
            other => {
                return Err(TagWriteError::Unsupported(format!(
                    "tags in {other:?} files cannot be written yet"
                )));
            }
        }
        Ok(snapshot)
    }

    pub(super) fn restore_id3v2(&self, file: &mut File) -> Result<(), TagWriteError> {
        id3v2::restore_file(file, &self.fields, &self.id3v2)
    }

    pub(super) fn restore_id3v1(&self, tag: &mut Id3v1Tag) {
        let saved = self.id3v1.clone().unwrap_or_default();
        for field in &self.fields {
            match field {
                TagField::Title => tag.title.clone_from(&saved.title),
                TagField::Artists => tag.artist.clone_from(&saved.artist),
                TagField::Album => tag.album.clone_from(&saved.album),
                TagField::Year => tag.year = saved.year,
                TagField::TrackNumber => tag.track_number = saved.track,
                TagField::Genres => tag.genre = saved.genre,
                TagField::AlbumArtists | TagField::Composers | TagField::DiscNumber => {}
            }
        }
    }

    pub(super) fn restore_ape(&self, tag: &mut ApeTag) -> Result<(), TagWriteError> {
        for key in self.fields.iter().flat_map(|field| ape_keys(*field)) {
            tag.remove(&key);
        }
        for (key, value) in &self.ape {
            let value = match value {
                SavedApeValue::Text(text) => ItemValue::Text(text.clone()),
                SavedApeValue::Locator(text) => ItemValue::Locator(text.clone()),
                SavedApeValue::Binary(data) => ItemValue::Binary(data.clone()),
            };
            let item = ApeItem::new(key.clone(), value).map_err(|error| format_error(&error))?;
            tag.insert(item);
        }
        Ok(())
    }

    pub(super) fn restore_vorbis(&self, tag: &mut VorbisComments) {
        for key in self.fields.iter().flat_map(|field| vorbis_keys(*field)) {
            let _ = tag.remove(key).count();
        }
        for (key, value) in &self.vorbis {
            tag.push(key.clone(), value.clone());
        }
    }

    pub(super) fn restore_ilst(&self, tag: &mut Ilst) -> Result<(), TagWriteError> {
        tag.retain(|atom| !is_edited_atom(atom.ident(), &self.fields));
        for saved in &self.mp4 {
            let ident = match &saved.name {
                SavedAtomName::Fourcc(code) => AtomIdent::Fourcc(*code),
                SavedAtomName::Freeform { mean, name } => AtomIdent::Freeform {
                    mean: Cow::Owned(mean.clone()),
                    name: Cow::Owned(name.clone()),
                },
            };
            let data = saved.data.iter().map(restored_data).collect();
            if let Some(atom) = Atom::from_collection(ident, data) {
                tag.insert(atom);
            }
        }
        Ok(())
    }
}

fn save_id3v1(tag: &Id3v1Tag, fields: &[TagField]) -> SavedId3v1 {
    let mut saved = SavedId3v1::default();
    for field in fields {
        match field {
            TagField::Title => saved.title.clone_from(&tag.title),
            TagField::Artists => saved.artist.clone_from(&tag.artist),
            TagField::Album => saved.album.clone_from(&tag.album),
            TagField::Year => saved.year = tag.year,
            TagField::TrackNumber => saved.track = tag.track_number,
            TagField::Genres => saved.genre = tag.genre,
            TagField::AlbumArtists | TagField::Composers | TagField::DiscNumber => {}
        }
    }
    saved
}

fn save_ape(tag: &ApeTag, fields: &[TagField]) -> Vec<(String, SavedApeValue)> {
    let keys: Vec<String> = fields.iter().flat_map(|field| ape_keys(*field)).collect();
    tag.into_iter()
        .filter(|item| keys.iter().any(|key| key.eq_ignore_ascii_case(item.key())))
        .map(|item| {
            let value = match item.value() {
                ItemValue::Text(text) => SavedApeValue::Text(text.clone()),
                ItemValue::Locator(text) => SavedApeValue::Locator(text.clone()),
                ItemValue::Binary(data) => SavedApeValue::Binary(data.clone()),
            };
            (item.key().to_owned(), value)
        })
        .collect()
}

fn save_ilst(tag: &Ilst, fields: &[TagField]) -> Result<Vec<SavedAtom>, TagWriteError> {
    let mut saved = Vec::new();
    for atom in tag {
        if !is_edited_atom(atom.ident(), fields) {
            continue;
        }
        let name = match atom.ident() {
            AtomIdent::Fourcc(code) => SavedAtomName::Fourcc(*code),
            AtomIdent::Freeform { mean, name } => SavedAtomName::Freeform {
                mean: mean.clone().into_owned(),
                name: name.clone().into_owned(),
            },
        };
        let data = atom
            .data()
            .map(saved_data)
            .collect::<Result<Vec<_>, TagWriteError>>()?;
        saved.push(SavedAtom { name, data });
    }
    Ok(saved)
}

fn saved_data(data: &AtomData) -> Result<SavedAtomData, TagWriteError> {
    Ok(match data {
        AtomData::UTF8(text) => SavedAtomData::Utf8(text.clone()),
        AtomData::UTF16(text) => SavedAtomData::Utf16(text.clone()),
        AtomData::SignedInteger(value) => SavedAtomData::Signed(*value),
        AtomData::UnsignedInteger(value) => SavedAtomData::Unsigned(*value),
        AtomData::Bool(value) => SavedAtomData::Bool(*value),
        AtomData::Unknown { code, data } => SavedAtomData::Other {
            code: u32::from(*code),
            data: data.clone(),
        },
        AtomData::Picture(_) => {
            return Err(TagWriteError::Unsupported(
                "this file holds a picture where a text field should be".to_owned(),
            ));
        }
    })
}

fn restored_data(data: &SavedAtomData) -> AtomData {
    match data {
        SavedAtomData::Utf8(text) => AtomData::UTF8(text.clone()),
        SavedAtomData::Utf16(text) => AtomData::UTF16(text.clone()),
        SavedAtomData::Signed(value) => AtomData::SignedInteger(*value),
        SavedAtomData::Unsigned(value) => AtomData::UnsignedInteger(*value),
        SavedAtomData::Bool(value) => AtomData::Bool(*value),
        SavedAtomData::Other { code, data } => AtomData::Unknown {
            code: DataType::from(*code),
            data: data.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mp4_atoms_come_back_as_they_were_saved() {
        let mut tag = Ilst::new();
        tag.insert(Atom::new(
            AtomIdent::Fourcc(*b"\xa9day"),
            AtomData::UTF8("2001-05-03T00:00:00Z".to_owned()),
        ));
        tag.insert(Atom::new(
            AtomIdent::Fourcc(*b"trkn"),
            AtomData::Unknown {
                code: DataType::Reserved,
                data: vec![0, 0, 0, 3, 0, 12, 0, 0],
            },
        ));
        tag.insert(Atom::new(
            AtomIdent::Freeform {
                mean: Cow::Borrowed("com.apple.iTunes"),
                name: Cow::Borrowed("ARTISTS"),
            },
            AtomData::UTF8("One".to_owned()),
        ));
        tag.insert(
            Atom::from_collection(
                AtomIdent::Fourcc(*b"\xa9ART"),
                vec![
                    AtomData::UTF8("One".to_owned()),
                    AtomData::UTF8("Two".to_owned()),
                ],
            )
            .unwrap(),
        );
        tag.insert(Atom::new(
            AtomIdent::Fourcc(*b"\xa9cmt"),
            AtomData::UTF8("Not edited".to_owned()),
        ));
        let original = tag.clone();

        let fields = [TagField::Year, TagField::TrackNumber, TagField::Artists];
        let mut snapshot = TagSnapshot::empty(&fields);
        snapshot.mp4 = save_ilst(&tag, &fields).unwrap();
        assert_eq!(snapshot.mp4.len(), 4);
        // It is stored as text in between.
        let text = serde_json::to_string(&snapshot).unwrap();
        let snapshot: TagSnapshot = serde_json::from_str(&text).unwrap();

        tag.retain(|atom| !is_edited_atom(atom.ident(), &fields));
        tag.insert(Atom::new(
            AtomIdent::Fourcc(*b"\xa9day"),
            AtomData::UTF8("1999".to_owned()),
        ));
        snapshot.restore_ilst(&mut tag).unwrap();

        let sorted = |tag: &Ilst| {
            let mut atoms: Vec<String> = tag.into_iter().map(|atom| format!("{atom:?}")).collect();
            atoms.sort();
            atoms
        };
        assert_eq!(sorted(&tag), sorted(&original));
    }
}
