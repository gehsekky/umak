//! Writes synthetic tagged MP3 files, to bring a test library up to a target size. The
//! output is deterministic, so running it again changes nothing.

use std::fs;
use std::path::Path;

/// Marks a folder as ours, so this tool can never write among real music.
const MARKER: &str = ".umak-synthetic";
const MPEG_FRAME_LEN: usize = 417;
const MPEG_FRAMES: usize = 60;
const TRACKS_PER_ARTIST: usize = 60;
const MIN_ALBUM_TRACKS: usize = 8;
const ALBUM_TRACK_RANGE: u64 = 7;

const WORDS: &[&str] = &[
    "amber",
    "anchor",
    "arrow",
    "ashes",
    "autumn",
    "avenue",
    "ballad",
    "banner",
    "barrel",
    "beacon",
    "blossom",
    "border",
    "bridge",
    "bronze",
    "canyon",
    "carbon",
    "castle",
    "cedar",
    "chapter",
    "circle",
    "clover",
    "coast",
    "comet",
    "copper",
    "coral",
    "cradle",
    "crimson",
    "crystal",
    "dancer",
    "dawn",
    "delta",
    "desert",
    "drifter",
    "ember",
    "empire",
    "engine",
    "evening",
    "falcon",
    "feather",
    "fever",
    "fjord",
    "flame",
    "forest",
    "fountain",
    "garden",
    "ghost",
    "glacier",
    "golden",
    "granite",
    "harbor",
    "harvest",
    "hollow",
    "horizon",
    "island",
    "ivory",
    "jasmine",
    "journey",
    "juniper",
    "kingdom",
    "lantern",
    "legend",
    "lighthouse",
    "lotus",
    "lunar",
    "marble",
    "meadow",
    "melody",
    "mirror",
    "monsoon",
    "morning",
    "mountain",
    "nectar",
    "needle",
    "night",
    "north",
    "ocean",
    "orchard",
    "origin",
    "paper",
    "phantom",
    "planet",
    "prairie",
    "quartz",
    "quiet",
    "rainfall",
    "raven",
    "ribbon",
    "river",
    "rocket",
    "saffron",
    "sailor",
    "scarlet",
    "shadow",
    "silver",
    "sparrow",
    "spiral",
    "starlight",
    "stone",
    "summer",
    "sunset",
    "tempest",
    "thunder",
    "timber",
    "tunnel",
    "valley",
    "velvet",
    "violet",
    "voyage",
    "walnut",
    "wander",
    "willow",
    "winter",
    "wonder",
    "zephyr",
];
const NAME_ACCENTS: &[&str] = &[
    "Björk",
    "Sigur Rós",
    "Motörhead",
    "Beyoncé",
    "Mötley",
    "Zoë",
];
const GENRES: &[&str] = &[
    "Rock",
    "Jazz",
    "Folk",
    "Electronic",
    "Classical",
    "Hip Hop",
    "Blues",
];

/// A small repeatable random source, so every run makes the same library.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, limit: u64) -> u64 {
        self.next() % limit
    }

    fn word(&mut self) -> &'static str {
        WORDS[self.below(WORDS.len() as u64) as usize]
    }
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

fn phrase(random: &mut Random, words: u64) -> String {
    (0..words)
        .map(|_| capitalized(random.word()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn syncsafe(value: usize) -> [u8; 4] {
    [21, 14, 7, 0].map(|shift| ((value >> shift) & 0x7f) as u8)
}

fn id3_tag(frames: &[(&str, &str)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (id, text) in frames {
        body.extend_from_slice(id.as_bytes());
        body.extend(syncsafe(text.len() + 1));
        body.extend([0, 0, 3]);
        body.extend_from_slice(text.as_bytes());
    }
    let mut tag = b"ID3\x04\0\0".to_vec();
    tag.extend(syncsafe(body.len()));
    tag.extend(body);
    tag
}

fn mpeg_audio(random: &mut Random) -> Vec<u8> {
    let mut audio = Vec::with_capacity(MPEG_FRAME_LEN * MPEG_FRAMES);
    for _ in 0..MPEG_FRAMES {
        audio.extend([0xff, 0xfb, 0x90, 0x00]);
        for _ in (4..MPEG_FRAME_LEN).step_by(8) {
            audio.extend(random.next().to_le_bytes());
        }
        audio.truncate(audio.len() - audio.len() % MPEG_FRAME_LEN);
    }
    audio
}

fn safe_name(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_alphanumeric() || ch == ' ' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// Writes `count` tracks and returns how many files it created. Refuses a folder with
/// anything in it, unless that folder was made by this tool.
pub fn generate(folder: &Path, count: usize) -> Result<usize, String> {
    let failed = |error: std::io::Error| format!("{}: {error}", folder.display());
    fs::create_dir_all(folder).map_err(failed)?;
    let is_ours = folder.join(MARKER).exists();
    let is_empty = fs::read_dir(folder).map_err(failed)?.next().is_none();
    if !is_ours && !is_empty {
        return Err(format!(
            "{} has other files in it, so nothing was written",
            folder.display()
        ));
    }
    fs::write(
        folder.join(MARKER),
        b"Synthetic test tracks made by umak-scan.\n",
    )
    .map_err(failed)?;

    let mut random = Random(0x9e37_79b9_7f4a_7c15);
    let artists: Vec<String> = (0..count.div_ceil(TRACKS_PER_ARTIST))
        .map(|index| match NAME_ACCENTS.get(index) {
            Some(name) => format!("{name} {}", capitalized(random.word())),
            None if index % 5 == 0 => format!("The {}s", phrase(&mut random, 1)),
            None => phrase(&mut random, 2),
        })
        .collect();

    let mut created = 0;
    let mut written = 0;
    let mut album_number = 0;
    while written < count {
        let artist = &artists[album_number % artists.len()];
        album_number += 1;
        let album_words = 1 + random.below(3);
        let album = phrase(&mut random, album_words);
        let year = 1960 + random.below(65);
        let genre = GENRES[random.below(GENRES.len() as u64) as usize];
        let tracks = MIN_ALBUM_TRACKS + random.below(ALBUM_TRACK_RANGE) as usize;
        let directory = folder
            .join(safe_name(artist))
            .join(format!("{year} - {} {album_number}", safe_name(&album)));
        fs::create_dir_all(&directory).map_err(failed)?;
        for number in 1..=tracks.min(count - written) {
            let title_words = 1 + random.below(4);
            let title = phrase(&mut random, title_words);
            let track_number = format!("{number}/{tracks}");
            let year_text = year.to_string();
            let tag = id3_tag(&[
                ("TIT2", &title),
                ("TPE1", artist),
                ("TPE2", artist),
                ("TALB", &album),
                ("TRCK", &track_number),
                ("TDRC", &year_text),
                ("TCON", genre),
            ]);
            // Drawn even for a file that exists, so its contents do not depend on the
            // files around it.
            let audio = mpeg_audio(&mut random);
            let path = directory.join(format!("{number:02} {}.mp3", safe_name(&title)));
            if !path.exists() {
                let mut bytes = tag;
                bytes.extend(audio);
                fs::write(&path, bytes).map_err(failed)?;
                created += 1;
            }
            written += 1;
        }
    }
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(folder: &Path) -> Vec<String> {
        let mut found = Vec::new();
        let mut pending = vec![folder.to_path_buf()];
        while let Some(current) = pending.pop() {
            for entry in fs::read_dir(current).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|extension| extension == "mp3") {
                    found.push(path.to_str().unwrap().to_owned());
                }
            }
        }
        found.sort();
        found
    }

    #[test]
    fn it_writes_exactly_the_requested_number_of_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("synthetic");
        assert_eq!(generate(&folder, 137).unwrap(), 137);
        assert_eq!(files(&folder).len(), 137);
    }

    #[test]
    fn running_it_again_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("synthetic");
        generate(&folder, 40).unwrap();
        let first = files(&folder);
        let bytes = fs::read(&first[0]).unwrap();
        assert_eq!(generate(&folder, 40).unwrap(), 0);
        assert_eq!(files(&folder), first);
        assert_eq!(fs::read(&first[0]).unwrap(), bytes);
    }

    #[test]
    fn a_folder_with_other_files_is_refused_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("my song.mp3"), b"precious").unwrap();
        let error = generate(dir.path(), 5).unwrap_err();
        assert!(error.contains("nothing was written"), "{error}");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        assert_eq!(
            fs::read(dir.path().join("my song.mp3")).unwrap(),
            b"precious"
        );
    }

    #[test]
    fn audio_differs_between_files_so_fingerprints_do_too() {
        let mut random = Random(1);
        let first = mpeg_audio(&mut random);
        let second = mpeg_audio(&mut random);
        assert_eq!(first.len(), MPEG_FRAME_LEN * MPEG_FRAMES);
        assert_ne!(first, second);
        assert_eq!(&first[MPEG_FRAME_LEN..MPEG_FRAME_LEN + 2], [0xff, 0xfb]);
    }
}
