//! Edits an ID3v2 tag frame by frame. The tag library rewrites every frame when it saves:
//! writing ID3v2.3 it discards frames such as TSOP and damages URL frames. Here, frames
//! that are not being edited keep their exact bytes, and the audio moves only if the new
//! tag does not fit in the old one's space.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};

use super::{FieldEdit, TagField, TagWriteError, list_field_names};

const HEADER_LEN: usize = 10;
const FRAME_HEADER_LEN: usize = 10;
/// Room left when a tag has to grow, so the next edit need not move the audio.
const NEW_PADDING: usize = 1024;
const SHIFT_BLOCK: u64 = 1024 * 1024;

const FLAG_UNSYNC: u8 = 0x80;
const FLAG_EXTENDED: u8 = 0x40;
const FLAG_FOOTER: u8 = 0x10;

const ENCODING_LATIN1: u8 = 0;
const ENCODING_UTF16: u8 = 1;
const ENCODING_UTF16BE: u8 = 2;
const ENCODING_UTF8: u8 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
struct RawFrame {
    id: [u8; 4],
    flags: [u8; 2],
    body: Vec<u8>,
}

fn read_synchsafe(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .fold(0, |value, byte| (value << 7) | usize::from(byte & 0x7f))
}

fn write_synchsafe(value: usize) -> [u8; 4] {
    [21, 14, 7, 0].map(|shift| ((value >> shift) & 0x7f) as u8)
}

fn is_frame_id(id: &[u8]) -> bool {
    id.iter()
        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

/// Reads the frames starting at `start`. Sizes are in the given form. Fails on anything
/// that is neither a frame nor padding, rather than lose what it could not read.
fn parse_frames(body: &[u8], start: usize, synchsafe: bool) -> Option<Vec<RawFrame>> {
    let mut frames = Vec::new();
    let mut at = start;
    while at + FRAME_HEADER_LEN <= body.len() {
        if body[at] == 0 {
            // Padding, which must be all zeros to the end.
            return body[at..].iter().all(|byte| *byte == 0).then_some(frames);
        }
        let id = &body[at..at + 4];
        if !is_frame_id(id) {
            return None;
        }
        let size_bytes = &body[at + 4..at + 8];
        let size = if synchsafe {
            if size_bytes.iter().any(|byte| byte & 0x80 != 0) {
                return None;
            }
            read_synchsafe(size_bytes)
        } else {
            u32::from_be_bytes(size_bytes.try_into().ok()?) as usize
        };
        let from = at + FRAME_HEADER_LEN;
        let to = from.checked_add(size)?;
        if to > body.len() {
            return None;
        }
        frames.push(RawFrame {
            id: id.try_into().ok()?,
            flags: [body[at + 8], body[at + 9]],
            body: body[from..to].to_vec(),
        });
        at = to;
    }
    // Fewer bytes than a frame header can only be padding.
    body[at..].iter().all(|byte| *byte == 0).then_some(frames)
}

fn remove_unsync(bytes: &[u8]) -> Vec<u8> {
    let mut clear = Vec::with_capacity(bytes.len());
    let mut after_ff = false;
    for byte in bytes {
        if !(after_ff && *byte == 0) {
            clear.push(*byte);
        }
        after_ff = *byte == 0xff;
    }
    clear
}

fn decode_text(body: &[u8]) -> Option<String> {
    let (encoding, rest) = body.split_first()?;
    let text = match *encoding {
        ENCODING_LATIN1 => rest.iter().map(|byte| *byte as char).collect(),
        ENCODING_UTF8 => String::from_utf8_lossy(rest).into_owned(),
        ENCODING_UTF16 | ENCODING_UTF16BE => {
            let big_endian = *encoding == ENCODING_UTF16BE || rest.starts_with(&[0xfe, 0xff]);
            let units: Vec<u16> = rest
                .chunks_exact(2)
                .map(|pair| {
                    let pair = [pair[0], pair[1]];
                    if big_endian {
                        u16::from_be_bytes(pair)
                    } else {
                        u16::from_le_bytes(pair)
                    }
                })
                .collect();
            String::from_utf16_lossy(&units)
                .trim_start_matches('\u{feff}')
                .to_owned()
        }
        _ => return None,
    };
    Some(text.trim_end_matches('\0').to_owned())
}

/// The description of a TXXX frame, which comes first in its text.
fn user_text_name(frame: &RawFrame) -> Option<String> {
    let text = decode_text(&frame.body)?;
    Some(text.split('\0').next()?.to_owned())
}

/// A text frame body in the best encoding the version has for this text.
fn encode_text(values: &[String], version: u8) -> Vec<u8> {
    let joined = values.join("\0");
    if version >= 4 {
        let mut body = vec![ENCODING_UTF8];
        body.extend_from_slice(joined.as_bytes());
        return body;
    }
    if joined.is_ascii() {
        let mut body = vec![ENCODING_LATIN1];
        body.extend_from_slice(joined.as_bytes());
        return body;
    }
    let mut body = vec![ENCODING_UTF16, 0xff, 0xfe];
    for unit in joined.encode_utf16() {
        body.extend_from_slice(&unit.to_le_bytes());
    }
    body
}

fn text_frame(id: &[u8; 4], values: &[String], version: u8) -> RawFrame {
    RawFrame {
        id: *id,
        flags: [0, 0],
        body: encode_text(values, version),
    }
}

/// The "/total" part of an existing number frame, so a new number keeps it.
fn number_total(frames: &[RawFrame], id: &[u8; 4]) -> Option<String> {
    let frame = frames.iter().find(|frame| &frame.id == id)?;
    let text = decode_text(&frame.body)?;
    let (_, total) = text.split_once('/')?;
    (!total.trim().is_empty()).then(|| total.trim().to_owned())
}

/// Puts a new frame where the first of the old ones was, and removes the rest, so the
/// frames that stay also stay in order.
fn replace_frames(
    frames: &mut Vec<RawFrame>,
    remove: impl Fn(&RawFrame) -> bool,
    new: Option<RawFrame>,
) {
    let at = frames.iter().position(&remove);
    frames.retain(|frame| !remove(frame));
    if let Some(new) = new {
        frames.insert(at.unwrap_or(frames.len()).min(frames.len()), new);
    }
}

fn apply_edit(frames: &mut Vec<RawFrame>, edit: &FieldEdit, version: u8) {
    let single =
        |id: &'static [u8; 4]| -> (&'static [u8; 4], Vec<&'static [u8; 4]>) { (id, vec![id]) };
    let (id, also_remove) = match edit.field {
        TagField::Title => single(b"TIT2"),
        TagField::Album => single(b"TALB"),
        TagField::Artists => single(b"TPE1"),
        TagField::AlbumArtists => single(b"TPE2"),
        TagField::Composers => single(b"TCOM"),
        TagField::Genres => single(b"TCON"),
        TagField::TrackNumber => single(b"TRCK"),
        TagField::DiscNumber => single(b"TPOS"),
        // A date can be spread across several frames, and any left behind would be read
        // instead of the new one. Each version has its own frame for the year.
        TagField::Year if version >= 4 => (b"TDRC", vec![b"TDRC", b"TYER", b"TDAT", b"TIME"]),
        TagField::Year => (b"TYER", vec![b"TYER", b"TDAT", b"TIME", b"TRDA", b"TDRC"]),
    };

    let values: Vec<String> = match edit.field {
        TagField::TrackNumber | TagField::DiscNumber => {
            let total = number_total(frames, id);
            edit.values
                .first()
                .map(|number| match total {
                    Some(total) => format!("{number}/{total}"),
                    None => number.clone(),
                })
                .into_iter()
                .collect()
        }
        _ => edit.values.clone(),
    };
    let new = (!values.is_empty()).then(|| text_frame(id, &values, version));
    replace_frames(frames, |frame| also_remove.contains(&&frame.id), new);

    // Custom fields holding these values, which the reader would otherwise prefer.
    let names = list_field_names(edit.field);
    if !names.is_empty() {
        frames.retain(|frame| {
            &frame.id != b"TXXX"
                || !user_text_name(frame)
                    .is_some_and(|name| names.iter().any(|list| list.eq_ignore_ascii_case(&name)))
        });
    }
}

fn serialize(frames: &[RawFrame], version: u8) -> Vec<u8> {
    let mut body = Vec::new();
    for frame in frames {
        body.extend_from_slice(&frame.id);
        if version >= 4 {
            body.extend(write_synchsafe(frame.body.len()));
        } else {
            body.extend((frame.body.len() as u32).to_be_bytes());
        }
        body.extend(frame.flags);
        body.extend_from_slice(&frame.body);
    }
    body
}

/// Moves everything from `from` to the end of the file forward by `by` bytes. It works from
/// the end, so nothing is overwritten before it has been moved.
fn shift_forward(file: &mut File, from: u64, by: u64) -> io::Result<()> {
    let len = file.metadata()?.len();
    let mut end = len;
    let mut buffer = vec![0; SHIFT_BLOCK as usize];
    while end > from {
        let start = end.saturating_sub(SHIFT_BLOCK).max(from);
        let count = (end - start) as usize;
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buffer[..count])?;
        file.seek(SeekFrom::Start(start + by))?;
        file.write_all(&buffer[..count])?;
        end = start;
    }
    Ok(())
}

/// How many zero bytes follow a tag. Some taggers pad outside the tag, and a program looking
/// for the audio may give up before getting through them, so they become the tag's own
/// padding. The audio stays where it is either way.
fn zeros_after(file: &mut File, from: u64) -> io::Result<u64> {
    let mut count = 0;
    let mut buffer = [0; 4096];
    file.seek(SeekFrom::Start(from))?;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(count);
        }
        match buffer[..read].iter().position(|byte| *byte != 0) {
            Some(position) => return Ok(count + position as u64),
            None => count += read as u64,
        }
    }
}

/// Applies the edits to the tag at the start of the file, making one if there is none.
pub fn edit_file(file: &mut File, edits: &[FieldEdit]) -> Result<(), TagWriteError> {
    let damaged =
        || TagWriteError::Unsupported("this file's ID3v2 tag could not be read".to_owned());

    let mut header = [0; HEADER_LEN];
    file.seek(SeekFrom::Start(0))?;
    let has_tag = file.read_exact(&mut header).is_ok() && &header[..3] == b"ID3";
    let (version, flags, mut old_size, mut frames) = if has_tag {
        let (version, flags) = (header[3], header[5]);
        if version != 3 && version != 4 {
            return Err(TagWriteError::Unsupported(format!(
                "this file has an ID3v2.{version} tag, which cannot be written without converting it"
            )));
        }
        if flags & FLAG_FOOTER != 0 {
            return Err(TagWriteError::Unsupported(
                "this file's ID3v2 tag has a footer, which cannot be written yet".to_owned(),
            ));
        }
        let size = read_synchsafe(&header[6..10]);
        let mut raw = vec![0; size];
        file.read_exact(&mut raw).map_err(|_| damaged())?;
        // In version 2.3 unsynchronization covers the whole tag, and in 2.4 each frame.
        let body = if version == 3 && flags & FLAG_UNSYNC != 0 {
            remove_unsync(&raw)
        } else {
            raw
        };
        let start = if flags & FLAG_EXTENDED == 0 {
            0
        } else if version == 3 {
            4 + u32::from_be_bytes(
                body.get(..4)
                    .ok_or_else(damaged)?
                    .try_into()
                    .map_err(|_| damaged())?,
            ) as usize
        } else {
            read_synchsafe(body.get(..4).ok_or_else(damaged)?)
        };
        let synchsafe = version >= 4;
        // Some programs write version 2.4 with plain sizes.
        let parsed = parse_frames(&body, start, synchsafe)
            .or_else(|| {
                if synchsafe {
                    parse_frames(&body, start, false)
                } else {
                    None
                }
            })
            .ok_or_else(damaged)?;
        (version, flags, size, parsed)
    } else {
        (3, 0, 0, Vec::new())
    };

    for edit in edits {
        apply_edit(&mut frames, edit, version);
    }

    if has_tag {
        old_size += zeros_after(file, (HEADER_LEN + old_size) as u64)? as usize;
    }
    let body = serialize(&frames, version);
    let old_total = if has_tag { HEADER_LEN + old_size } else { 0 };
    // Unsynchronization and the extended header are dropped. Both are optional, and the
    // frames are written out in the clear.
    let new_flags = flags & !(FLAG_EXTENDED | if version == 3 { FLAG_UNSYNC } else { 0 });
    let fits = has_tag && body.len() <= old_size;
    let new_size = if fits {
        old_size
    } else {
        body.len() + NEW_PADDING
    };
    if !fits {
        let by = (HEADER_LEN + new_size - old_total) as u64;
        shift_forward(file, old_total as u64, by)?;
    }

    let mut tag = Vec::with_capacity(HEADER_LEN + new_size);
    tag.extend_from_slice(b"ID3");
    tag.extend([version, 0, new_flags]);
    tag.extend(write_synchsafe(new_size));
    tag.extend_from_slice(&body);
    tag.resize(HEADER_LEN + new_size, 0);
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&tag)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(id: &[u8; 4], body: &[u8]) -> RawFrame {
        RawFrame {
            id: *id,
            flags: [0, 0],
            body: body.to_vec(),
        }
    }

    fn values(items: &[&str]) -> Vec<String> {
        items.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn frames_parse_and_write_back_identically_in_both_versions() {
        let frames = vec![
            frame(b"TIT2", b"\0Title"),
            frame(b"PRIV", &[1, 2, 3, 0xff, 0]),
        ];
        for version in [3, 4] {
            let body = serialize(&frames, version);
            let parsed = parse_frames(&body, 0, version == 4).unwrap();
            assert_eq!(parsed, frames);
        }
    }

    #[test]
    fn padding_ends_the_frames_but_anything_else_is_refused() {
        let mut body = serialize(&[frame(b"TIT2", b"\0Title")], 3);
        body.extend([0; 100]);
        assert_eq!(parse_frames(&body, 0, false).unwrap().len(), 1);

        let mut junk = serialize(&[frame(b"TIT2", b"\0Title")], 3);
        junk.extend([0, 0, 7, 0, 0]);
        assert_eq!(parse_frames(&junk, 0, false), None);

        let mut bad_id = serialize(&[frame(b"TIT2", b"\0Title")], 3);
        bad_id.extend(b"ti@2\0\0\0\x01\0\0x");
        assert_eq!(parse_frames(&bad_id, 0, false), None);

        // A frame claiming to run past the end of the tag.
        let mut long = serialize(&[frame(b"TIT2", b"\0Title")], 3);
        long.truncate(long.len() - 2);
        assert_eq!(parse_frames(&long, 0, false), None);
    }

    #[test]
    fn text_uses_what_each_version_can_hold() {
        assert_eq!(encode_text(&values(&["Plain"]), 3), b"\0Plain");
        assert_eq!(encode_text(&values(&["Café"]), 4)[0], ENCODING_UTF8);
        let wide = encode_text(&values(&["音"]), 3);
        assert_eq!(&wide[..3], &[ENCODING_UTF16, 0xff, 0xfe]);
        assert_eq!(decode_text(&wide).unwrap(), "音");
    }

    #[test]
    fn text_decodes_in_every_encoding() {
        assert_eq!(decode_text(b"\0Caf\xe9").unwrap(), "Café");
        assert_eq!(decode_text(b"\x03Caf\xc3\xa9\0").unwrap(), "Café");
        assert_eq!(decode_text(&[1, 0xff, 0xfe, b'A', 0, 0, 0]).unwrap(), "A");
        assert_eq!(decode_text(&[1, 0xfe, 0xff, 0, b'A']).unwrap(), "A");
        assert_eq!(decode_text(&[2, 0, b'A']).unwrap(), "A");
        assert_eq!(decode_text(&[9, 1]), None);
    }

    #[test]
    fn unsynchronization_comes_off_only_where_it_was_added() {
        assert_eq!(
            remove_unsync(&[1, 0xff, 0, 0xe0, 0xff, 0, 0, 5]),
            [1, 0xff, 0xe0, 0xff, 0, 5]
        );
    }

    #[test]
    fn a_new_frame_takes_the_place_of_the_old_and_others_stay_in_order() {
        let mut frames = vec![
            frame(b"TALB", b"\0A"),
            frame(b"TIT2", b"\0Old"),
            frame(b"TPE1", b"\0B"),
        ];
        apply_edit(
            &mut frames,
            &FieldEdit {
                field: TagField::Title,
                values: values(&["New"]),
            },
            3,
        );
        let ids: Vec<&[u8; 4]> = frames.iter().map(|frame| &frame.id).collect();
        assert_eq!(ids, [b"TALB", b"TIT2", b"TPE1"]);
        assert_eq!(frames[1].body, b"\0New");
    }

    #[test]
    fn numbers_keep_their_totals() {
        let mut frames = vec![frame(b"TRCK", b"\x003/12")];
        apply_edit(
            &mut frames,
            &FieldEdit {
                field: TagField::TrackNumber,
                values: values(&["7"]),
            },
            3,
        );
        assert_eq!(frames[0].body, b"\x007/12");
    }

    #[test]
    fn a_year_replaces_every_frame_a_date_was_spread_across() {
        let mut frames = vec![
            frame(b"TYER", b"\x001999"),
            frame(b"TDAT", b"\x000101"),
            frame(b"TIME", b"\x001200"),
        ];
        let edit = FieldEdit {
            field: TagField::Year,
            values: values(&["2004"]),
        };
        apply_edit(&mut frames, &edit, 3);
        assert_eq!(frames, [frame(b"TYER", b"\x002004")]);

        let mut newer = vec![frame(b"TDRC", b"\x031999-01-01")];
        apply_edit(&mut newer, &edit, 4);
        assert_eq!(newer, [frame(b"TDRC", b"\x032004")]);
    }

    #[test]
    fn a_custom_list_field_goes_whatever_its_case_and_others_stay() {
        let mut frames = vec![
            frame(b"TPE1", b"\0Old"),
            frame(b"TXXX", b"\0Artists\0Old"),
            frame(b"TXXX", b"\0ARTISTS\0Old"),
            frame(b"TXXX", b"\0MY FIELD\0kept"),
        ];
        apply_edit(
            &mut frames,
            &FieldEdit {
                field: TagField::Artists,
                values: values(&["New"]),
            },
            3,
        );
        assert_eq!(
            frames,
            [
                frame(b"TPE1", b"\0New"),
                frame(b"TXXX", b"\0MY FIELD\0kept")
            ]
        );
    }
}
