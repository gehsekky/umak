//! Sampled audio fingerprints, which identify a file after it moves.
//!
//! The samples come from the audio region only, so editing tags does not change the
//! result, and only a small part of a file is read however large it is.

use std::io::{self, Read, Seek, SeekFrom};

const VERSION: u8 = 1;
const HASH_LEN: usize = 16;
const SAMPLE_LEN: u64 = 64 * 1024;
const ID3V1_LEN: u64 = 128;
const APE_FOOTER_LEN: u64 = 32;

/// Hashes the audio region's length and up to `samples` chunks of it: the first chunk,
/// then the last, then the middle. Each one past the first costs a disk seek.
pub fn fingerprint<R: Read + Seek>(reader: &mut R, samples: usize) -> io::Result<Vec<u8>> {
    let (start, end) = audio_region(reader)?;
    let length = end - start;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&length.to_le_bytes());

    let mut buffer = vec![0; SAMPLE_LEN as usize];
    if length <= SAMPLE_LEN * samples.max(1) as u64 {
        read_into_hash(reader, start, length, &mut buffer, &mut hasher)?;
    } else {
        let last = end - SAMPLE_LEN;
        let middle = start + (length - SAMPLE_LEN) / 2;
        for offset in [start, last, middle].into_iter().take(samples) {
            read_into_hash(reader, offset, SAMPLE_LEN, &mut buffer, &mut hasher)?;
        }
    }

    let mut output = vec![VERSION];
    output.extend_from_slice(&hasher.finalize().as_bytes()[..HASH_LEN]);
    Ok(output)
}

fn read_into_hash<R: Read + Seek>(
    reader: &mut R,
    offset: u64,
    mut length: u64,
    buffer: &mut [u8],
    hasher: &mut blake3::Hasher,
) -> io::Result<()> {
    reader.seek(SeekFrom::Start(offset))?;
    while length > 0 {
        let chunk = length.min(buffer.len() as u64) as usize;
        reader.read_exact(&mut buffer[..chunk])?;
        hasher.update(&buffer[..chunk]);
        length -= chunk as u64;
    }
    Ok(())
}

/// Finds the byte range of a file's audio data from its container.
fn audio_region<R: Read + Seek>(reader: &mut R) -> io::Result<(u64, u64)> {
    let size = reader.seek(SeekFrom::End(0))?;
    let start = id3v2_len(reader, size)?;
    let head = read_at::<12, R>(reader, start, size)?;

    let region = match (&head[..4], &head[4..8], &head[8..12]) {
        (b"fLaC", _, _) => flac_region(reader, start, size)?,
        (_, b"ftyp", _) => mp4_region(reader, start, size)?,
        (b"RIFF", _, b"WAVE") => chunk_region(reader, start, size, b"data", Endian::Little)?,
        (b"FORM", _, b"AIFF" | b"AIFC") => chunk_region(reader, start, size, b"SSND", Endian::Big)?,
        _ => None,
    };
    match region {
        Some(region) => Ok(region),
        None => {
            let start = skip_padding(reader, start, size)?;
            Ok((start, tail_tags_start(reader, start, size)?))
        }
    }
}

/// Moves past zero bytes after a tag. Tag editors resize this padding, so it must not
/// count as audio.
fn skip_padding<R: Read + Seek>(reader: &mut R, start: u64, size: u64) -> io::Result<u64> {
    let mut offset = start;
    let mut buffer = [0; 4096];
    reader.seek(SeekFrom::Start(offset))?;
    while offset < size {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        match buffer[..count].iter().position(|byte| *byte != 0) {
            Some(position) => return Ok(offset + position as u64),
            None => offset += count as u64,
        }
    }
    Ok(start)
}

/// Reads `N` bytes at `offset`, or returns zeros if they would run past the end.
fn read_at<const N: usize, R: Read + Seek>(
    reader: &mut R,
    offset: u64,
    size: u64,
) -> io::Result<[u8; N]> {
    let mut bytes = [0; N];
    if offset.saturating_add(N as u64) <= size {
        reader.seek(SeekFrom::Start(offset))?;
        reader.read_exact(&mut bytes)?;
    }
    Ok(bytes)
}

/// The length of an ID3v2 tag at the start of the file, or zero.
fn id3v2_len<R: Read + Seek>(reader: &mut R, size: u64) -> io::Result<u64> {
    let header = read_at::<10, R>(reader, 0, size)?;
    if &header[..3] != b"ID3" {
        return Ok(0);
    }
    let body = header[6..10]
        .iter()
        .fold(0_u64, |total, byte| (total << 7) | u64::from(byte & 0x7f));
    let footer = if header[5] & 0x10 != 0 { 10 } else { 0 };
    Ok((10 + body + footer).min(size))
}

/// Where the tags at the end of a file begin: APE and ID3v1, in either order.
fn tail_tags_start<R: Read + Seek>(reader: &mut R, start: u64, size: u64) -> io::Result<u64> {
    let mut end = size;
    loop {
        if end >= start + ID3V1_LEN && &read_at::<3, R>(reader, end - ID3V1_LEN, size)? == b"TAG" {
            end -= ID3V1_LEN;
            continue;
        }
        if end >= start + APE_FOOTER_LEN {
            let footer = read_at::<32, R>(reader, end - APE_FOOTER_LEN, size)?;
            if &footer[..8] == b"APETAGEX" {
                let body = u64::from(u32::from_le_bytes(footer[12..16].try_into().unwrap()));
                let flags = u32::from_le_bytes(footer[20..24].try_into().unwrap());
                let header = if flags & 0x8000_0000 != 0 {
                    APE_FOOTER_LEN
                } else {
                    0
                };
                let total = body + header;
                if total >= APE_FOOTER_LEN && end >= start + total {
                    end -= total;
                    continue;
                }
            }
        }
        return Ok(end);
    }
}

fn flac_region<R: Read + Seek>(
    reader: &mut R,
    start: u64,
    size: u64,
) -> io::Result<Option<(u64, u64)>> {
    let mut offset = start + 4;
    loop {
        if offset + 4 > size {
            return Ok(None);
        }
        let header = read_at::<4, R>(reader, offset, size)?;
        let length = u64::from(u32::from_be_bytes([0, header[1], header[2], header[3]]));
        offset += 4 + length;
        if header[0] & 0x80 != 0 {
            return Ok((offset <= size).then_some((offset, size)));
        }
    }
}

fn mp4_region<R: Read + Seek>(
    reader: &mut R,
    start: u64,
    size: u64,
) -> io::Result<Option<(u64, u64)>> {
    let mut offset = start;
    while offset + 8 <= size {
        let header = read_at::<8, R>(reader, offset, size)?;
        let short = u64::from(u32::from_be_bytes(header[..4].try_into().unwrap()));
        let (header_len, atom_len) = match short {
            0 => (8, size - offset),
            1 => {
                let long = read_at::<8, R>(reader, offset + 8, size)?;
                (16, u64::from_be_bytes(long))
            }
            length => (8, length),
        };
        if atom_len < header_len {
            return Ok(None);
        }
        if &header[4..8] == b"mdat" {
            let end = offset.saturating_add(atom_len).min(size);
            return Ok(Some((offset + header_len, end)));
        }
        offset = offset.saturating_add(atom_len);
    }
    Ok(None)
}

#[derive(Clone, Copy)]
enum Endian {
    Little,
    Big,
}

/// Walks the chunks of a RIFF or AIFF file to find the audio chunk.
fn chunk_region<R: Read + Seek>(
    reader: &mut R,
    start: u64,
    size: u64,
    audio_chunk: &[u8; 4],
    endian: Endian,
) -> io::Result<Option<(u64, u64)>> {
    let mut offset = start + 12;
    while offset + 8 <= size {
        let header = read_at::<8, R>(reader, offset, size)?;
        let length_bytes: [u8; 4] = header[4..8].try_into().unwrap();
        let length = u64::from(match endian {
            Endian::Little => u32::from_le_bytes(length_bytes),
            Endian::Big => u32::from_be_bytes(length_bytes),
        });
        if &header[..4] == audio_chunk {
            let end = (offset + 8).saturating_add(length).min(size);
            return Ok(Some((offset + 8, end)));
        }
        offset += 8 + length + (length & 1);
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    const ALL_SAMPLES: usize = 3;

    /// Bytes that differ at every position, so a wrong offset changes the hash.
    fn audio(length: usize, seed: u32) -> Vec<u8> {
        let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
        (0..length)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 24) as u8
            })
            .collect()
    }

    fn print(bytes: &[u8]) -> Vec<u8> {
        fingerprint(&mut Cursor::new(bytes), ALL_SAMPLES).unwrap()
    }

    fn id3v2(body_len: usize) -> Vec<u8> {
        let syncsafe = |shift: u32| ((body_len >> shift) & 0x7f) as u8;
        let mut tag = vec![b'I', b'D', b'3', 4, 0, 0];
        tag.extend([syncsafe(21), syncsafe(14), syncsafe(7), syncsafe(0)]);
        tag.resize(10 + body_len, b'x');
        tag
    }

    fn ape_tag(body_len: u32) -> Vec<u8> {
        let mut tag = vec![b'y'; body_len as usize - 32];
        tag.extend_from_slice(b"APETAGEX");
        tag.extend(2000_u32.to_le_bytes());
        tag.extend(body_len.to_le_bytes());
        tag.extend(0_u32.to_le_bytes());
        tag.extend(0_u32.to_le_bytes());
        tag.extend([0; 8]);
        tag
    }

    fn mp3(front: usize, audio_bytes: &[u8], back: &[u8]) -> Vec<u8> {
        let mut file = if front == 0 { Vec::new() } else { id3v2(front) };
        file.extend_from_slice(audio_bytes);
        file.extend_from_slice(back);
        file
    }

    #[test]
    fn mp3_tags_at_either_end_do_not_change_the_result() {
        let sound = audio(500_000, 1);
        let plain = print(&sound);

        assert_eq!(print(&mp3(1000, &sound, &[])), plain);
        assert_eq!(print(&mp3(250_000, &sound, &[])), plain);

        let mut id3v1 = vec![0; 128];
        id3v1[..3].copy_from_slice(b"TAG");
        assert_eq!(print(&mp3(0, &sound, &id3v1)), plain);

        let ape = ape_tag(200);
        assert_eq!(print(&mp3(0, &sound, &ape)), plain);

        let ape_then_id3v1 = [ape, id3v1].concat();
        assert_eq!(print(&mp3(4000, &sound, &ape_then_id3v1)), plain);
    }

    #[test]
    fn padding_after_an_id3_tag_does_not_change_the_result() {
        let sound = audio(500_000, 8);
        let plain = print(&mp3(1000, &sound, &[]));
        let padded = [id3v2(1000), vec![0; 5000], sound.clone()].concat();
        assert_eq!(print(&padded), plain);

        // A file of nothing but padding still gets a result.
        print(&[id3v2(10), vec![0; 100]].concat());
    }

    fn flac(comment_len: usize, sound: &[u8]) -> Vec<u8> {
        let mut file = b"fLaC".to_vec();
        file.extend([0, 0, 0, 34]);
        file.extend([7; 34]);
        file.push(0x84);
        file.extend_from_slice(&(comment_len as u32).to_be_bytes()[1..]);
        file.extend(vec![b'c'; comment_len]);
        file.extend_from_slice(sound);
        file
    }

    #[test]
    fn flac_metadata_does_not_change_the_result_but_audio_does() {
        let sound = audio(500_000, 2);
        let short = print(&flac(100, &sound));
        assert_eq!(print(&flac(90_000, &sound)), short);
        // An ID3v2 tag before fLaC is invalid but found in real files.
        let prefixed = [id3v2(500), flac(100, &sound)].concat();
        assert_eq!(print(&prefixed), short);

        let mut changed = sound.clone();
        changed[0] ^= 1;
        assert_ne!(print(&flac(100, &changed)), short);
    }

    fn atom(name: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut bytes = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(name);
        bytes.extend_from_slice(body);
        bytes
    }

    #[test]
    fn mp4_metadata_on_either_side_of_the_audio_does_not_change_the_result() {
        let sound = audio(500_000, 3);
        let ftyp = atom(b"ftyp", b"M4A \0\0\0\0");
        let mdat = atom(b"mdat", &sound);
        let moov = |len: usize| atom(b"moov", &vec![b'm'; len]);

        let moov_last = [ftyp.clone(), mdat.clone(), moov(500)].concat();
        let moov_first = [ftyp.clone(), moov(90_000), mdat.clone()].concat();
        assert_eq!(print(&moov_last), print(&moov_first));

        // 64-bit atom size.
        let mut large = vec![0, 0, 0, 1];
        large.extend_from_slice(b"mdat");
        large.extend(((sound.len() + 16) as u64).to_be_bytes());
        large.extend_from_slice(&sound);
        let large_mdat = [ftyp, large, moov(10)].concat();
        assert_eq!(print(&large_mdat), print(&moov_last));
    }

    #[test]
    fn wav_and_aiff_chunks_around_the_audio_do_not_change_the_result() {
        let sound = audio(500_000, 4);
        let wav = |extra: &[u8]| {
            let mut file = b"RIFF\0\0\0\0WAVE".to_vec();
            file.extend_from_slice(b"fmt ");
            file.extend(16_u32.to_le_bytes());
            file.extend([1; 16]);
            file.extend_from_slice(extra);
            file.extend_from_slice(b"data");
            file.extend((sound.len() as u32).to_le_bytes());
            file.extend_from_slice(&sound);
            file
        };
        let list = [b"LIST".as_slice(), &3_u32.to_le_bytes(), b"abc\0"].concat();
        assert_eq!(print(&wav(&[])), print(&wav(&list)));

        let aiff = |extra: &[u8]| {
            let mut file = b"FORM\0\0\0\0AIFF".to_vec();
            file.extend_from_slice(extra);
            file.extend_from_slice(b"SSND");
            file.extend((sound.len() as u32).to_be_bytes());
            file.extend_from_slice(&sound);
            file
        };
        let name = [b"NAME".as_slice(), &4_u32.to_be_bytes(), b"abcd"].concat();
        assert_eq!(print(&aiff(&[])), print(&aiff(&name)));
    }

    #[test]
    fn small_files_are_hashed_whole_and_different_files_differ() {
        let one = print(&audio(1000, 5));
        assert_eq!(one.len(), 1 + HASH_LEN);
        assert_eq!(one[0], VERSION);
        assert_ne!(one, print(&audio(1000, 6)));
        assert_ne!(one, print(&audio(1001, 5)));
        assert_eq!(print(&[]).len(), 1 + HASH_LEN);
    }

    #[test]
    fn each_sample_covers_its_part_of_a_large_file() {
        let sound = audio(1_000_000, 7);
        let base = print(&sound);
        for position in [10, 500_000, 999_990] {
            let mut changed = sound.clone();
            changed[position] ^= 1;
            assert_ne!(print(&changed), base, "byte {position} is not covered");
        }
    }

    #[test]
    fn damaged_containers_fall_back_without_panicking() {
        let truncated_flac = {
            let mut file = b"fLaC".to_vec();
            file.extend([0x84, 0xff, 0xff, 0xff]);
            file
        };
        print(&truncated_flac);
        let zero_atom = [atom(b"ftyp", b"M4A "), vec![0, 0, 0, 4], b"free".to_vec()].concat();
        print(&zero_atom);
        let huge_id3 = {
            let mut tag = b"ID3\x04\0\0".to_vec();
            tag.extend([0x7f; 4]);
            tag
        };
        print(&huge_id3);
    }
}
