//! Reads the encoder delay and length of an MP4 audio track from its edit list, which FFmpeg
//! and other encoders write instead of the iTunes gapless tag. The edit list says which part
//! of the track to play: from a media time, the delay, for a duration, the length.

use std::io::{self, Read, Seek, SeekFrom};

/// Atoms are read only up to this size, so a damaged size cannot exhaust memory.
const MAX_READ: u64 = 16 * 1024 * 1024;

struct Header {
    kind: [u8; 4],
    /// Where the body starts and ends.
    body: u64,
    end: u64,
}

fn read_header<R: Read + Seek>(reader: &mut R, end: u64) -> io::Result<Option<Header>> {
    let at = reader.stream_position()?;
    if at + 8 > end {
        return Ok(None);
    }
    let mut bytes = [0; 8];
    reader.read_exact(&mut bytes)?;
    let mut size = u64::from(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
    let kind = [bytes[4], bytes[5], bytes[6], bytes[7]];
    let mut body = at + 8;
    if size == 1 {
        let mut large = [0; 8];
        reader.read_exact(&mut large)?;
        size = u64::from_be_bytes(large);
        body += 8;
    } else if size == 0 {
        size = end - at;
    }
    if size < body - at || at + size > end {
        return Ok(None);
    }
    Ok(Some(Header {
        kind,
        body,
        end: at + size,
    }))
}

/// Finds the child atom of this kind between `from` and `end`.
fn find<R: Read + Seek>(
    reader: &mut R,
    from: u64,
    end: u64,
    kind: &[u8; 4],
) -> io::Result<Option<Header>> {
    reader.seek(SeekFrom::Start(from))?;
    while let Some(header) = read_header(reader, end)? {
        if &header.kind == kind {
            return Ok(Some(header));
        }
        reader.seek(SeekFrom::Start(header.end))?;
    }
    Ok(None)
}

fn read_body<R: Read + Seek>(reader: &mut R, header: &Header) -> io::Result<Option<Vec<u8>>> {
    let len = header.end - header.body;
    if len > MAX_READ {
        return Ok(None);
    }
    reader.seek(SeekFrom::Start(header.body))?;
    let mut body = vec![0; len as usize];
    reader.read_exact(&mut body)?;
    Ok(Some(body))
}

fn be_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn be_u64(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

/// The timescale of an `mvhd` or `mdhd` body, which sits after the dates of its version.
fn timescale(body: &[u8]) -> Option<u32> {
    match body.first()? {
        0 => be_u32(body, 12),
        1 => be_u32(body, 20),
        _ => None,
    }
    .filter(|scale| *scale > 0)
}

/// The one edit that plays the media: its duration in the movie's timescale and its start
/// in the track's. Empty edits, which only delay the start, come before it. More than one
/// edit that plays media is not a plain delay and padding, so it gives nothing.
fn playing_edit(body: &[u8]) -> Option<(u64, u64)> {
    let version = *body.first()?;
    let count = be_u32(body, 4)? as usize;
    let entry_len = if version == 1 { 20 } else { 12 };
    let mut found = None;
    for index in 0..count {
        let at = 8 + index * entry_len;
        let (duration, media_time) = if version == 1 {
            (be_u64(body, at)?, be_u64(body, at + 8)? as i64)
        } else {
            (
                u64::from(be_u32(body, at)?),
                i64::from(be_u32(body, at + 4)? as i32),
            )
        };
        if media_time < 0 {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some((duration, media_time as u64));
    }
    found
}

/// The delay and length in frames at `sample_rate`, from the first sound track's edit list.
pub fn read_gapless<R: Read + Seek>(reader: &mut R, sample_rate: u32) -> Option<(u64, u64)> {
    read_gapless_inner(reader, sample_rate).ok().flatten()
}

fn read_gapless_inner<R: Read + Seek>(
    reader: &mut R,
    sample_rate: u32,
) -> io::Result<Option<(u64, u64)>> {
    let end = reader.seek(SeekFrom::End(0))?;
    let Some(moov) = find(reader, 0, end, b"moov")? else {
        return Ok(None);
    };
    let Some(mvhd) = find(reader, moov.body, moov.end, b"mvhd")? else {
        return Ok(None);
    };
    let Some(movie_scale) = read_body(reader, &mvhd)?.as_deref().and_then(timescale) else {
        return Ok(None);
    };

    let mut next = moov.body;
    while let Some(trak) = find(reader, next, moov.end, b"trak")? {
        next = trak.end;
        let Some(mdia) = find(reader, trak.body, trak.end, b"mdia")? else {
            continue;
        };
        let is_sound = match find(reader, mdia.body, mdia.end, b"hdlr")? {
            Some(hdlr) => read_body(reader, &hdlr)?
                .is_some_and(|body| body.get(8..12) == Some(b"soun".as_slice())),
            None => false,
        };
        if !is_sound {
            continue;
        }
        let Some(mdhd) = find(reader, mdia.body, mdia.end, b"mdhd")? else {
            return Ok(None);
        };
        let Some(media_scale) = read_body(reader, &mdhd)?.as_deref().and_then(timescale) else {
            return Ok(None);
        };
        let Some(edts) = find(reader, trak.body, trak.end, b"edts")? else {
            return Ok(None);
        };
        let Some(elst) = find(reader, edts.body, edts.end, b"elst")? else {
            return Ok(None);
        };
        let Some((duration, media_time)) =
            read_body(reader, &elst)?.as_deref().and_then(playing_edit)
        else {
            return Ok(None);
        };
        let rate = u64::from(sample_rate);
        let delay = media_time * rate / u64::from(media_scale);
        let length = duration * rate / u64::from(movie_scale);
        return Ok((length > 0).then_some((delay, length)));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn atom(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut bytes = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(body);
        bytes
    }

    fn header_v0(scale: u32, duration: u32) -> Vec<u8> {
        let mut body = vec![0; 12];
        body.extend(scale.to_be_bytes());
        body.extend(duration.to_be_bytes());
        body.extend([0; 80]);
        body
    }

    fn elst(entries: &[(u32, i32)]) -> Vec<u8> {
        let mut body = vec![0, 0, 0, 0];
        body.extend((entries.len() as u32).to_be_bytes());
        for (duration, media_time) in entries {
            body.extend(duration.to_be_bytes());
            body.extend(media_time.to_be_bytes());
            body.extend([0, 1, 0, 0]);
        }
        atom(b"elst", &body)
    }

    fn hdlr(kind: &[u8; 4]) -> Vec<u8> {
        let mut body = vec![0; 8];
        body.extend_from_slice(kind);
        body.extend([0; 13]);
        atom(b"hdlr", &body)
    }

    /// A file as FFmpeg writes it: the audio first, the movie atom after.
    fn file(
        movie_scale: u32,
        media_scale: u32,
        edits: &[(u32, i32)],
        handler: &[u8; 4],
    ) -> Vec<u8> {
        let mdia = [atom(b"mdhd", &header_v0(media_scale, 0)), hdlr(handler)].concat();
        let trak = [
            atom(b"tkhd", &[0; 84]),
            atom(b"edts", &elst(edits)),
            atom(b"mdia", &mdia),
        ]
        .concat();
        let moov = [
            atom(b"mvhd", &header_v0(movie_scale, 0)),
            atom(b"trak", &trak),
        ]
        .concat();
        [
            atom(b"ftyp", b"M4A "),
            atom(b"mdat", &[0; 4096]),
            atom(b"moov", &moov),
        ]
        .concat()
    }

    fn gapless(bytes: Vec<u8>, rate: u32) -> Option<(u64, u64)> {
        read_gapless(&mut Cursor::new(bytes), rate)
    }

    #[test]
    fn reads_the_delay_and_length() {
        // As FFmpeg writes AAC at 44.1 kHz.
        let bytes = file(44_100, 44_100, &[(146_999, 1024)], b"soun");
        assert_eq!(gapless(bytes, 44_100), Some((1024, 146_999)));
    }

    #[test]
    fn converts_between_timescales() {
        // A movie counted in milliseconds and a track in its own rate.
        let bytes = file(1000, 48_000, &[(2000, 2112)], b"soun");
        assert_eq!(gapless(bytes, 48_000), Some((2112, 96_000)));
    }

    #[test]
    fn skips_an_empty_edit_before_the_one_that_plays() {
        let bytes = file(44_100, 44_100, &[(500, -1), (44_100, 1024)], b"soun");
        assert_eq!(gapless(bytes, 44_100), Some((1024, 44_100)));
    }

    #[test]
    fn gives_nothing_it_cannot_trust() {
        // Two edits that play media are not a delay and padding.
        assert_eq!(
            gapless(
                file(44_100, 44_100, &[(1000, 0), (1000, 5000)], b"soun"),
                44_100
            ),
            None
        );
        // A video track's edits are not the audio's.
        assert_eq!(
            gapless(file(44_100, 44_100, &[(1000, 1024)], b"vide"), 44_100),
            None
        );
        // No edit list, or no movie at all.
        assert_eq!(gapless(file(44_100, 44_100, &[], b"soun"), 44_100), None);
        assert_eq!(gapless(atom(b"ftyp", b"M4A "), 44_100), None);
        assert_eq!(gapless(Vec::new(), 44_100), None);
        // A size that runs past the end.
        let mut damaged = file(44_100, 44_100, &[(146_999, 1024)], b"soun");
        let len = damaged.len();
        damaged.truncate(len - 10);
        assert_eq!(gapless(damaged, 44_100), None);
    }
}
