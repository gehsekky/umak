//! End-to-end scanner tests against small audio files built byte by byte, so the
//! fixtures do not depend on the tag-writing code of the library under test.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use rusqlite::params;
use tempfile::TempDir;

use super::*;
use crate::api::add_root;
use crate::db::{DEFAULT_USER_ID, DbError};

const MPEG_FRAME_LEN: usize = 417;
const MPEG_FRAMES: usize = 60;

/// An ID3v2.4 tag from `(frame id, text)` pairs. `TXXX:NAME` makes a custom field.
fn id3_tag(frames: &[(&str, &str)]) -> Vec<u8> {
    fn syncsafe(value: usize) -> [u8; 4] {
        [21, 14, 7, 0].map(|shift| ((value >> shift) & 0x7f) as u8)
    }
    let mut body = Vec::new();
    for (id, text) in frames {
        let (id, content) = match id.split_once(':') {
            Some((id, name)) => (id, format!("{name}\0{text}")),
            None => (*id, (*text).to_owned()),
        };
        body.extend_from_slice(id.as_bytes());
        body.extend(syncsafe(content.len() + 1));
        body.extend([0, 0, 3]);
        body.extend_from_slice(content.as_bytes());
    }
    let mut tag = b"ID3\x04\0\0".to_vec();
    tag.extend(syncsafe(body.len()));
    tag.extend(body);
    tag
}

/// A valid MPEG stream whose payload depends on `seed`.
fn mpeg_audio(seed: u32) -> Vec<u8> {
    let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    let mut audio = Vec::with_capacity(MPEG_FRAME_LEN * MPEG_FRAMES);
    for _ in 0..MPEG_FRAMES {
        audio.extend([0xff, 0xfb, 0x90, 0x00]);
        for _ in 4..MPEG_FRAME_LEN {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            audio.push((state >> 24) as u8);
        }
    }
    audio
}

pub(crate) fn mp3_bytes(seed: u32, frames: &[(&str, &str)]) -> Vec<u8> {
    [id3_tag(frames), mpeg_audio(seed)].concat()
}

/// A FLAC file with a stream header, the given Vorbis comments, and arbitrary frames.
fn flac_bytes(seed: u32, comments: &[(&str, &str)]) -> Vec<u8> {
    let mut comment_block = 0_u32.to_le_bytes().to_vec();
    comment_block.extend((comments.len() as u32).to_le_bytes());
    for (key, value) in comments {
        let entry = format!("{key}={value}");
        comment_block.extend((entry.len() as u32).to_le_bytes());
        comment_block.extend_from_slice(entry.as_bytes());
    }

    let mut stream_info = vec![0x10, 0, 0x10, 0, 0, 0, 0, 0, 0, 0];
    // 96000 Hz, 2 channels, 24 bits, 960000 samples.
    let packed: u64 = (96_000 << 44) | (1 << 41) | (23 << 36) | 960_000;
    stream_info.extend(packed.to_be_bytes());
    stream_info.extend([0; 16]);

    let mut file = b"fLaC".to_vec();
    file.extend([0, 0, 0, stream_info.len() as u8]);
    file.extend(stream_info);
    file.push(0x84);
    file.extend_from_slice(&(comment_block.len() as u32).to_be_bytes()[1..]);
    file.extend(comment_block);
    file.extend(mpeg_audio(seed));
    file
}

struct Library {
    dir: TempDir,
    db: Database,
    root_id: String,
}

fn library() -> Library {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("umak.db")).unwrap();
    let music = dir.path().join("music");
    fs::create_dir(&music).unwrap();
    let root_id = add_root(&db, music.to_str().unwrap(), None).unwrap().id;
    Library { dir, db, root_id }
}

struct Row {
    public_id: String,
    title: String,
    artist_display: String,
    album_id: i64,
    missing: bool,
}

struct AlbumRow {
    public_id: String,
    title: String,
    artist: String,
    compilation: String,
    rule: i64,
    links: Vec<String>,
}

impl Library {
    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join("music").join(rel)
    }

    fn write(&self, rel: &str, bytes: &[u8]) {
        let path = self.path(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn mp3(&self, rel: &str, seed: u32, frames: &[(&str, &str)]) {
        self.write(rel, &mp3_bytes(seed, frames));
    }

    fn scan(&self) -> ScanReport {
        self.scan_with(ScanOptions::default())
    }

    fn scan_with(&self, options: ScanOptions) -> ScanReport {
        let cancel = AtomicBool::new(false);
        scan_library(&self.db, None, options, &cancel, &mut |_| {}).unwrap()
    }

    fn scan_only(&self, paths: &[&str]) -> ScanReport {
        let paths: Vec<String> = paths.iter().map(|path| (*path).to_owned()).collect();
        let cancel = AtomicBool::new(false);
        scan_paths(
            &self.db,
            &self.root_id,
            &paths,
            ScanOptions::default(),
            &cancel,
            &mut |_| {},
        )
        .unwrap()
    }

    fn query<T>(&self, work: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> T {
        self.db
            .read(|conn| work(conn).map_err(DbError::from))
            .unwrap()
    }

    fn count(&self, sql: &str) -> i64 {
        self.query(|conn| conn.query_row(sql, [], |row| row.get(0)))
    }

    fn track(&self, rel: &str) -> Row {
        self.query(|conn| {
            conn.query_row(
                "SELECT public_id, title, artist_display, album_id, missing_since IS NOT NULL
                 FROM tracks WHERE rel_path = ?1",
                [rel],
                |row| {
                    Ok(Row {
                        public_id: row.get(0)?,
                        title: row.get(1)?,
                        artist_display: row.get(2)?,
                        album_id: row.get(3)?,
                        missing: row.get(4)?,
                    })
                },
            )
        })
    }

    fn album_of(&self, rel: &str) -> AlbumRow {
        let id = self.track(rel).album_id;
        self.query(|conn| {
            let links = conn
                .prepare(
                    "SELECT ar.name FROM album_artists aa JOIN artists ar ON ar.id = aa.artist_id
                     WHERE aa.album_id = ?1 ORDER BY aa.position",
                )?
                .query_map([id], |row| row.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            conn.query_row(
                "SELECT public_id, title, album_artist_display, compilation, grouping_rule
                 FROM albums WHERE id = ?1",
                [id],
                |row| {
                    Ok(AlbumRow {
                        public_id: row.get(0)?,
                        title: row.get(1)?,
                        artist: row.get(2)?,
                        compilation: row.get(3)?,
                        rule: row.get(4)?,
                        links,
                    })
                },
            )
        })
    }

    fn search(&self, query: &str) -> i64 {
        self.query(|conn| {
            conn.query_row(
                "SELECT count(*) FROM tracks_fts WHERE tracks_fts MATCH ?1",
                [query],
                |row| row.get(0),
            )
        })
    }

    fn rate(&self, rel: &str, rating: i64) {
        self.db
            .write(|conn| {
                conn.execute(
                    "INSERT INTO track_user_data (user_id, track_id, rating)
                     SELECT ?1, id, ?2 FROM tracks WHERE rel_path = ?3",
                    params![DEFAULT_USER_ID, rating, rel],
                )?;
                Ok::<_, DbError>(())
            })
            .unwrap();
    }
}

pub(crate) fn tagged<'a>(
    title: &'a str,
    artist: &'a str,
    album: &'a str,
) -> Vec<(&'a str, &'a str)> {
    vec![("TIT2", title), ("TPE1", artist), ("TALB", album)]
}

#[test]
fn a_first_scan_imports_mp3_tags_properties_and_custom_fields() {
    let lib = library();
    lib.mp3(
        "Artist/Album/01.mp3",
        1,
        &[
            ("TIT2", "First Song"),
            ("TPE1", "Solo Artist"),
            ("TPE2", "Album Person"),
            ("TALB", "The Album"),
            ("TCOM", "A Composer"),
            ("TCON", "Rock"),
            ("TRCK", "3/12"),
            ("TPOS", "2/2"),
            ("TDRC", "1994-12-31"),
            ("TXXX:REPLAYGAIN_TRACK_GAIN", "-6.54 dB"),
            ("TXXX:REPLAYGAIN_TRACK_PEAK", "0.98"),
            ("TXXX:MY CUSTOM FIELD", "keep me"),
        ],
    );
    let report = lib.scan();
    assert_eq!(report.added, 1);
    assert_eq!(report.tag_errors, 0, "{:?}", report.issues);

    type Fields = (
        String,
        i64,
        i64,
        Option<i64>,
        Option<f64>,
        Option<f64>,
        String,
        String,
        String,
        Option<String>,
        Option<i64>,
        Vec<u8>,
    );
    let fields: Fields = lib.query(|conn| {
        conn.query_row(
            "SELECT title, disc_no, track_no, year, rg_track_gain, rg_track_peak, title_sort,
                    album_sort, album_artist_sort, codec, sample_rate, fingerprint FROM tracks",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                ))
            },
        )
    });
    assert_eq!(fields.0, "First Song");
    assert_eq!((fields.1, fields.2), (2, 3));
    assert_eq!(fields.3, Some(1994));
    assert_eq!(fields.4, Some(-6.54));
    assert_eq!(fields.5, Some(0.98));
    assert_eq!(fields.6, "first song");
    assert_eq!(fields.7, "album");
    assert_eq!(fields.8, "album person");
    assert_eq!(fields.9.as_deref(), Some("mp3"));
    assert_eq!(fields.10, Some(44100));
    assert_eq!(fields.11.len(), 17);

    assert_eq!(
        lib.count(
            "SELECT count(*) FROM raw_tags WHERE key = 'MY CUSTOM FIELD' AND value = 'keep me'"
        ),
        1
    );
    assert_eq!(
        lib.count(
            "SELECT count(*) FROM track_artists ta JOIN artists a ON a.id = ta.artist_id
             WHERE ta.role = 'composer' AND a.name = 'A Composer'"
        ),
        1
    );
    assert_eq!(lib.search("first"), 1);
    assert_eq!(lib.search("rock"), 1);
    assert_eq!(lib.album_of("Artist/Album/01.mp3").links, ["Album Person"]);
}

#[test]
fn flac_comments_properties_and_repeated_values_are_read() {
    let lib = library();
    lib.write(
        "a.flac",
        &flac_bytes(
            1,
            &[
                ("TITLE", "Hi Res"),
                ("ARTIST", "First"),
                ("ARTIST", "Second"),
                ("ALBUM", "Album"),
                ("GENRE", "Jazz"),
                ("GENRE", "Blues"),
                ("LOCATION", "Hampton, VA"),
                ("MY_FIELD", "one"),
                ("MY_FIELD", "two"),
            ],
        ),
    );
    let report = lib.scan();
    assert_eq!(report.tag_errors, 0, "{:?}", report.issues);

    assert_eq!(lib.track("a.flac").artist_display, "First, Second");
    assert_eq!(
        lib.count("SELECT count(*) FROM track_artists WHERE role = 'artist'"),
        2
    );
    assert_eq!(lib.count("SELECT count(*) FROM track_genres"), 2);
    assert_eq!(
        lib.count("SELECT count(*) FROM raw_tags WHERE key = 'MY_FIELD'"),
        2
    );
    let (codec, rate, depth, channels): (String, i64, i64, i64) = lib.query(|conn| {
        conn.query_row(
            "SELECT codec, sample_rate, bit_depth, channels FROM tracks",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
    });
    assert_eq!(
        (codec.as_str(), rate, depth, channels),
        ("flac", 96_000, 24, 2)
    );
}

#[test]
fn an_id3_multi_value_frame_makes_separate_artists_and_a_slash_does_not() {
    let lib = library();
    lib.mp3("a.mp3", 1, &[("TPE1", "One\0Two"), ("TALB", "A")]);
    lib.mp3("b.mp3", 2, &[("TPE1", "AC/DC"), ("TALB", "B")]);
    lib.scan();
    assert_eq!(lib.track("a.mp3").artist_display, "One, Two");
    assert_eq!(lib.track("b.mp3").artist_display, "AC/DC");
    assert_eq!(
        lib.count("SELECT count(*) FROM artists WHERE name IN ('AC', 'DC')"),
        0
    );
}

#[test]
fn padding_after_the_tag_beyond_the_library_default_is_still_read() {
    let lib = library();
    let tag = id3_tag(&tagged("Padded", "X", "Y"));
    let bytes = [tag, vec![0; 5000], mpeg_audio(1)].concat();
    lib.write("padded.mp3", &bytes);
    lib.mp3("plain.mp3", 1, &tagged("Plain", "X", "Y"));
    let report = lib.scan();
    assert_eq!(report.tag_errors, 0, "{:?}", report.issues);
    assert_eq!(lib.track("padded.mp3").title, "Padded");

    let prints = lib.query(|conn| {
        conn.query_row(
            "SELECT count(DISTINCT fingerprint) FROM tracks",
            [],
            |row| row.get::<_, i64>(0),
        )
    });
    assert_eq!(prints, 1, "padding must not change the fingerprint");
}

#[test]
fn a_name_repeated_in_a_tag_is_kept_once() {
    let lib = library();
    lib.write(
        "a.flac",
        &flac_bytes(
            1,
            &[
                ("TITLE", "Song"),
                ("ALBUM", "Album"),
                ("ALBUMARTIST", "Stravinsky, Igor"),
                ("ALBUMARTIST", "STRAVINSKY, IGOR"),
                ("ARTIST", "One"),
                ("ARTIST", "one"),
                ("GENRE", "Rock"),
                ("GENRE", "rock"),
            ],
        ),
    );
    lib.scan();
    let album = lib.album_of("a.flac");
    assert_eq!(album.artist, "Stravinsky, Igor");
    assert_eq!(album.links, ["Stravinsky, Igor"]);
    assert_eq!(lib.track("a.flac").artist_display, "One");
    assert_eq!(lib.count("SELECT count(*) FROM track_genres"), 1);
}

#[test]
fn a_rescan_with_no_changes_writes_nothing() {
    let lib = library();
    lib.mp3("a.mp3", 1, &tagged("A", "X", "Y"));
    lib.mp3("b.mp3", 2, &tagged("B", "X", "Y"));
    lib.scan();
    let before: i64 = lib.count("SELECT max(updated_at) FROM tracks");

    let report = lib.scan();
    assert_eq!(report.files_found, 2);
    assert_eq!(report.unchanged, 2);
    assert!(!report.changed());
    assert_eq!(report.tag_ms, 0);
    assert_eq!(report.fingerprint_ms, 0);
    assert_eq!(lib.count("SELECT max(updated_at) FROM tracks"), before);
}

#[test]
fn a_retag_updates_in_place_and_cleans_up_what_it_replaced() {
    let lib = library();
    lib.mp3("a.mp3", 1, &tagged("Old Title", "Old Artist", "Old Album"));
    lib.scan();
    let old = lib.track("a.mp3");
    lib.rate("a.mp3", 8);

    lib.mp3(
        "a.mp3",
        1,
        &tagged("A Longer New Title", "New Artist", "New Album"),
    );
    let report = lib.scan();
    assert_eq!(report.updated, 1);
    assert_eq!(report.added, 0);

    let new = lib.track("a.mp3");
    assert_eq!(new.public_id, old.public_id);
    assert_eq!(new.title, "A Longer New Title");
    assert_eq!(lib.count("SELECT count(*) FROM track_user_data"), 1);
    assert_eq!(
        lib.count("SELECT count(*) FROM artists WHERE name = 'Old Artist'"),
        0
    );
    assert_eq!(
        lib.count("SELECT count(*) FROM albums WHERE title = 'Old Album'"),
        0
    );
    assert_eq!(lib.search("old"), 0);
    assert_eq!(lib.search("longer"), 1);
}

#[test]
fn a_moved_file_keeps_its_id_rating_and_playlist_entry_even_after_a_retag() {
    let lib = library();
    lib.mp3("old/a.mp3", 1, &tagged("Song", "X", "Y"));
    lib.mp3("old/b.mp3", 2, &tagged("Other", "X", "Y"));
    lib.scan();
    let original = lib.track("old/a.mp3");
    lib.rate("old/a.mp3", 9);
    lib.db
        .write(|conn| {
            conn.execute(
                "INSERT INTO playlist_items (playlist_id, position, track_id)
                 SELECT 1, 1000, id FROM tracks WHERE rel_path = 'old/a.mp3'",
                [],
            )?;
            Ok::<_, DbError>(())
        })
        .unwrap();

    fs::remove_file(lib.path("old/a.mp3")).unwrap();
    lib.mp3("new/place/a.mp3", 1, &tagged("Renamed Tag", "X", "Y"));
    let report = lib.scan();
    assert_eq!(report.moved, 1, "{report:?}");
    assert_eq!((report.added, report.missing), (0, 0));

    let moved = lib.track("new/place/a.mp3");
    assert_eq!(moved.public_id, original.public_id);
    assert_eq!(moved.title, "Renamed Tag");
    assert!(!moved.missing);
    assert_eq!(lib.count("SELECT rating FROM track_user_data"), 9);
    assert_eq!(lib.count("SELECT count(*) FROM playlist_items"), 1);
    assert_eq!(lib.count("SELECT count(*) FROM tracks"), 2);
}

#[test]
fn a_file_moved_in_a_later_scan_still_finds_its_missing_track() {
    let lib = library();
    lib.mp3("a.mp3", 1, &tagged("Song", "X", "Y"));
    lib.mp3("keep.mp3", 2, &tagged("Keep", "X", "Y"));
    lib.scan();
    let original = lib.track("a.mp3");

    fs::remove_file(lib.path("a.mp3")).unwrap();
    assert_eq!(lib.scan().missing, 1);
    assert!(lib.track("a.mp3").missing);

    lib.mp3("elsewhere/a.mp3", 1, &tagged("Song", "X", "Y"));
    let report = lib.scan();
    assert_eq!((report.moved, report.added), (1, 0));
    assert_eq!(lib.track("elsewhere/a.mp3").public_id, original.public_id);
}

#[test]
fn identical_copies_moving_together_pair_up_by_file_name() {
    let lib = library();
    lib.mp3("x/first.mp3", 1, &tagged("Song", "X", "Y"));
    lib.mp3("x/second.mp3", 1, &tagged("Song", "X", "Y"));
    lib.scan();
    let first = lib.track("x/first.mp3").public_id;
    let second = lib.track("x/second.mp3").public_id;

    fs::remove_dir_all(lib.path("x")).unwrap();
    lib.mp3("y/second.mp3", 1, &tagged("Song", "X", "Y"));
    lib.mp3("y/first.mp3", 1, &tagged("Song", "X", "Y"));
    assert_eq!(lib.scan().moved, 2);
    assert_eq!(lib.track("y/first.mp3").public_id, first);
    assert_eq!(lib.track("y/second.mp3").public_id, second);
}

#[test]
fn a_deleted_file_is_marked_missing_and_clears_when_it_returns() {
    let lib = library();
    let bytes = mp3_bytes(1, &tagged("Song", "X", "Y"));
    lib.write("a.mp3", &bytes);
    lib.mp3("b.mp3", 2, &tagged("Keep", "X", "Y"));
    lib.scan();
    let original = lib.track("a.mp3");
    let saved_time = fs::metadata(lib.path("a.mp3")).unwrap().modified().unwrap();

    fs::remove_file(lib.path("a.mp3")).unwrap();
    let report = lib.scan();
    assert_eq!(report.missing, 1);
    let row = lib.track("a.mp3");
    assert!(row.missing);
    assert_eq!(row.public_id, original.public_id);
    assert_eq!(lib.search("song"), 1);

    lib.write("a.mp3", &bytes);
    let file = File::options().write(true).open(lib.path("a.mp3")).unwrap();
    file.set_modified(saved_time).unwrap();
    drop(file);
    let report = lib.scan();
    assert_eq!((report.restored, report.added, report.updated), (1, 0, 0));
    assert!(!lib.track("a.mp3").missing);
}

#[test]
fn a_root_that_is_offline_or_suddenly_empty_loses_no_tracks() {
    let lib = library();
    lib.mp3("a.mp3", 1, &tagged("Song", "X", "Y"));
    lib.scan();

    fs::remove_file(lib.path("a.mp3")).unwrap();
    let report = lib.scan();
    assert_eq!(report.missing, 0);
    assert_eq!(report.offline_roots.len(), 1);
    assert!(!lib.track("a.mp3").missing);

    fs::remove_dir(lib.path("")).unwrap();
    let report = lib.scan();
    assert_eq!(report.missing, 0);
    assert_eq!(report.offline_roots.len(), 1);
    assert!(!lib.track("a.mp3").missing);
}

#[test]
fn tracks_under_a_folder_that_could_not_be_listed_are_not_marked_missing() {
    let known: HashMap<String, KnownTrack> = ["locked/a.mp3", "open/b.mp3", "lockedish/c.mp3"]
        .iter()
        .enumerate()
        .map(|(index, path)| {
            (
                (*path).to_owned(),
                KnownTrack {
                    id: index as i64,
                    size: 1,
                    mtime: 1,
                    missing: false,
                },
            )
        })
        .collect();
    let root = Root {
        id: 1,
        path: PathBuf::from("music"),
    };
    let mut plan = Plan::default();
    add_to_plan(&mut plan, &root, &known, Vec::new(), |path| {
        !is_under(path, "locked")
    });
    plan.gone.sort_unstable();
    // "lockedish" shares a prefix with "locked" but is a different folder.
    assert_eq!(plan.gone, [1, 2]);
}

#[test]
fn an_unreadable_file_is_kept_with_a_title_from_its_name_and_reported() {
    let lib = library();
    lib.write("Some Song.mp3", b"this is not audio");
    lib.write("empty.flac", b"");
    lib.mp3("good.mp3", 1, &tagged("Good", "X", "Y"));
    let report = lib.scan();
    assert_eq!(report.added, 3);
    assert_eq!(report.tag_errors, 2);
    assert_eq!(report.issues.len(), 2);
    let row = lib.track("Some Song.mp3");
    assert_eq!(row.title, "Some Song");
    assert_eq!(row.artist_display, normalize::UNKNOWN_ARTIST);
    assert_eq!(
        lib.album_of("Some Song.mp3").title,
        normalize::UNKNOWN_ALBUM
    );
}

#[test]
fn scanning_never_changes_a_music_file() {
    let lib = library();
    lib.mp3("a.mp3", 1, &tagged("Song", "X", "Y"));
    lib.write("b.flac", &flac_bytes(2, &[("TITLE", "Song")]));
    lib.write("c.mp3", b"garbage");
    let read = |rel: &str| {
        let path = lib.path(rel);
        (
            fs::read(&path).unwrap(),
            fs::metadata(&path).unwrap().modified().unwrap(),
        )
    };
    let before = ["a.mp3", "b.flac", "c.mp3"].map(read);
    lib.scan();
    assert_eq!(["a.mp3", "b.flac", "c.mp3"].map(read), before);
    assert_eq!(fs::read_dir(lib.path("")).unwrap().count(), 3);
}

#[test]
fn rule_1_groups_by_musicbrainz_id_across_folders() {
    let lib = library();
    let id = "AAAAAAAA-0000-4000-8000-000000000001";
    let frames = |title: &'static str, album: &'static str| {
        vec![
            ("TIT2", title),
            ("TPE1", "Artist"),
            ("TPE2", "Album Artist"),
            ("TALB", album),
            ("TXXX:MusicBrainz Album Id", id),
        ]
    };
    lib.mp3("one/a.mp3", 1, &frames("A", "Album"));
    lib.mp3("two/b.mp3", 2, &frames("B", "Album Renamed"));
    lib.scan();
    let album = lib.album_of("one/a.mp3");
    assert_eq!(album.rule, 1);
    assert_eq!(
        lib.track("one/a.mp3").album_id,
        lib.track("two/b.mp3").album_id
    );
}

#[test]
fn rule_2_groups_by_album_artist_title_and_folder_ignoring_case() {
    let lib = library();
    let tags = |title, artist, album| vec![("TIT2", title), ("TPE2", artist), ("TALB", album)];
    lib.mp3("x/a.mp3", 1, &tags("A", "The Band", "Live"));
    lib.mp3("x/b.mp3", 2, &tags("B", "the band", "LIVE"));
    lib.mp3("x/CD2/c.mp3", 3, &tags("C", "The Band", "Live"));
    lib.mp3("x/d.mp3", 4, &tags("D", "Other Band", "Live"));
    lib.mp3("y/e.mp3", 5, &tags("E", "The Band", "Live"));
    lib.scan();

    let id = |rel| lib.track(rel).album_id;
    assert_eq!(lib.album_of("x/a.mp3").rule, 2);
    assert_eq!(id("x/a.mp3"), id("x/b.mp3"));
    assert_eq!(id("x/a.mp3"), id("x/CD2/c.mp3"));
    assert_ne!(id("x/a.mp3"), id("x/d.mp3"));
    assert_ne!(id("x/a.mp3"), id("y/e.mp3"));
    assert_eq!(
        lib.count("SELECT count(*) FROM artists WHERE norm_key = 'the band'"),
        1
    );
}

#[test]
fn rule_3_puts_a_flagged_album_without_an_album_artist_under_various_artists() {
    let lib = library();
    let tags = |title, artist| {
        vec![
            ("TIT2", title),
            ("TPE1", artist),
            ("TALB", "Hits"),
            ("TCMP", "1"),
        ]
    };
    lib.mp3("v/a.mp3", 1, &tags("A", "One"));
    lib.mp3("v/b.mp3", 2, &tags("B", "Two"));
    lib.scan();
    let album = lib.album_of("v/a.mp3");
    assert_eq!(album.rule, 3);
    assert_eq!(album.artist, normalize::VARIOUS_ARTISTS);
    assert_eq!(album.compilation, "tagged");
    assert_eq!(album.links, [normalize::VARIOUS_ARTISTS]);
    assert_eq!(lib.track("v/a.mp3").artist_display, "One");
    assert_eq!(lib.track("v/a.mp3").album_id, lib.track("v/b.mp3").album_id);
}

#[test]
fn spellings_of_various_artists_use_the_reserved_row() {
    let lib = library();
    let tags = |title, va| vec![("TIT2", title), ("TPE2", va), ("TALB", "Mix")];
    lib.mp3("a/a.mp3", 1, &tags("A", "VA"));
    lib.mp3("b/b.mp3", 2, &tags("B", "V.A."));
    lib.mp3("c/c.mp3", 3, &tags("C", "Various"));
    lib.scan();
    for rel in ["a/a.mp3", "b/b.mp3", "c/c.mp3"] {
        let album = lib.album_of(rel);
        assert_eq!(album.artist, normalize::VARIOUS_ARTISTS, "{rel}");
        assert_eq!(album.compilation, "tagged");
        assert_eq!(album.links, [normalize::VARIOUS_ARTISTS]);
    }
    assert_eq!(
        lib.count("SELECT count(*) FROM artists WHERE norm_key = 'various artists'"),
        1
    );
    assert_eq!(
        lib.count("SELECT count(*) FROM artists WHERE name IN ('VA', 'V.A.', 'Various')"),
        0
    );
}

#[test]
fn rule_4_finds_a_majority_artist_or_infers_a_compilation() {
    let lib = library();
    let album = "Untagged Album";
    // Exactly half the tracks by one artist still makes them the album artist.
    lib.mp3("half/1.mp3", 1, &tagged("1", "Main", album));
    lib.mp3("half/2.mp3", 2, &tagged("2", "Main", album));
    lib.mp3("half/3.mp3", 3, &tagged("3", "Guest A", album));
    lib.mp3("half/4.mp3", 4, &tagged("4", "Guest B", album));
    // One track short of half is a compilation.
    lib.mp3("mix/1.mp3", 5, &tagged("1", "One", album));
    lib.mp3("mix/2.mp3", 6, &tagged("2", "Two", album));
    lib.mp3("mix/3.mp3", 7, &tagged("3", "Three", album));
    lib.scan();

    let half = lib.album_of("half/1.mp3");
    assert_eq!(half.rule, 4);
    assert_eq!(half.artist, "Main");
    assert_eq!(half.compilation, "none");
    assert_eq!(half.links, ["Main"]);

    let mix = lib.album_of("mix/1.mp3");
    assert_eq!(mix.rule, 4);
    assert_eq!(mix.artist, normalize::VARIOUS_ARTISTS);
    assert_eq!(mix.compilation, "inferred");
    assert_eq!(mix.links, [normalize::VARIOUS_ARTISTS]);
    assert_eq!(lib.track("mix/1.mp3").artist_display, "One");
    let sort: String = lib.query(|conn| {
        conn.query_row(
            "SELECT album_artist_sort FROM tracks WHERE rel_path = 'mix/1.mp3'",
            [],
            |row| row.get(0),
        )
    });
    assert_eq!(sort, "various artists");
}

#[test]
fn a_judged_album_keeps_its_id_and_is_rejudged_as_tracks_arrive() {
    let lib = library();
    let album = "Growing Album";
    lib.mp3("g/1.mp3", 1, &tagged("1", "Solo", album));
    lib.scan();
    let first = lib.album_of("g/1.mp3");
    assert_eq!(first.artist, "Solo");
    assert_eq!(first.compilation, "none");

    lib.mp3("g/2.mp3", 2, &tagged("2", "Guest One", album));
    lib.mp3("g/3.mp3", 3, &tagged("3", "Guest Two", album));
    lib.scan();
    let grown = lib.album_of("g/1.mp3");
    assert_eq!(grown.public_id, first.public_id);
    assert_eq!(grown.artist, normalize::VARIOUS_ARTISTS);
    assert_eq!(grown.compilation, "inferred");
    assert_eq!(
        lib.count("SELECT count(*) FROM artists WHERE name = 'Various Artists'"),
        1
    );
}

#[test]
fn a_sort_tag_overrides_the_computed_sort_value() {
    let lib = library();
    lib.mp3(
        "a.mp3",
        1,
        &[
            ("TIT2", "Song"),
            ("TPE1", "David Bowie"),
            ("TSOP", "Bowie, David"),
            ("TPE2", "David Bowie"),
            ("TSO2", "Bowie, David"),
            ("TALB", "Heroes"),
        ],
    );
    lib.scan();
    let (artist, album_artist): (String, String) = lib.query(|conn| {
        conn.query_row(
            "SELECT artist_sort, album_artist_sort FROM tracks",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    });
    assert_eq!(artist, "bowie, david");
    assert_eq!(album_artist, "bowie, david");
}

impl Library {
    fn set(&self, key: &str, value: serde_json::Value) {
        self.db
            .write(|conn| crate::settings::write(conn, key, &value))
            .unwrap();
    }

    fn sorts(&self, rel: &str) -> (String, String, String, String) {
        self.query(|conn| {
            conn.query_row(
                "SELECT t.title_sort, t.artist_sort, t.album_sort, ar.sort_name
                 FROM tracks t
                 JOIN track_artists ta ON ta.track_id = t.id AND ta.role = 'artist'
                 JOIN artists ar ON ar.id = ta.artist_id
                 WHERE t.rel_path = ?1",
                [rel],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
        })
    }
}

#[test]
fn the_file_types_and_skipped_folders_follow_the_settings() {
    let lib = library();
    lib.mp3("a/1.mp3", 1, &tagged("One", "Artist", "Album"));
    lib.write("a/2.flac", &flac_bytes(2, &[("TITLE", "Two")]));
    lib.mp3("Extras/3.mp3", 3, &tagged("Three", "Artist", "Album"));
    lib.mp3("@eaDir/4.mp3", 4, &tagged("Four", "Artist", "Album"));
    lib.scan();
    assert_eq!(lib.count("SELECT count(*) FROM tracks"), 3);

    lib.set("library.fileTypes", serde_json::json!(["flac"]));
    lib.set("library.skippedFolders", serde_json::json!(["extras"]));
    let report = lib.scan();
    assert_eq!(report.files_found, 1);
    assert!(lib.track("a/1.mp3").missing);
    assert!(lib.track("Extras/3.mp3").missing);
    assert!(!lib.track("a/2.flac").missing);

    lib.set("library.fileTypes", serde_json::json!(["mp3", "flac"]));
    lib.set("library.skippedFolders", serde_json::json!([]));
    lib.scan();
    assert_eq!(
        lib.count("SELECT count(*) FROM tracks WHERE missing_since IS NULL"),
        4
    );
}

#[test]
fn new_articles_sort_names_again_and_sort_tags_still_win() {
    let lib = library();
    lib.mp3(
        "1.mp3",
        1,
        &tagged("Das Lied", "Die Toten Hosen", "The Album"),
    );
    lib.mp3(
        "2.mp3",
        2,
        &[
            ("TIT2", "The Song"),
            ("TPE1", "The Band"),
            ("TSOP", "The Band"),
            ("TALB", "Other"),
        ],
    );
    lib.scan();
    assert_eq!(
        lib.sorts("1.mp3"),
        (
            "das lied".to_owned(),
            "die toten hosen".to_owned(),
            "album".to_owned(),
            "die toten hosen".to_owned()
        )
    );
    // An unchanged library reads nothing again.
    assert_eq!(lib.scan().unchanged, 2);

    lib.set("library.sortArticles", serde_json::json!(["Die", "Das"]));
    let report = lib.scan();
    assert_eq!(report.unchanged, 0, "every file is read again");
    assert_eq!(
        lib.sorts("1.mp3"),
        (
            "lied".to_owned(),
            "toten hosen".to_owned(),
            "the album".to_owned(),
            "toten hosen".to_owned()
        )
    );
    // The tag said how to sort the artist; the title had no tag.
    let (title, artist, _, artist_row) = lib.sorts("2.mp3");
    assert_eq!(title, "the song");
    assert_eq!(
        (artist.as_str(), artist_row.as_str()),
        ("the band", "the band")
    );
    let album_sort: String = lib.query(|conn| {
        conn.query_row(
            "SELECT a.sort_title FROM albums a JOIN tracks t ON t.album_id = a.id
             WHERE t.rel_path = '1.mp3'",
            [],
            |row| row.get(0),
        )
    });
    assert_eq!(album_sort, "the album");
    assert_eq!(lib.scan().unchanged, 2, "sorted once");
}

#[test]
fn a_canceled_scan_stops_early_and_the_next_scan_finishes_the_work() {
    let lib = library();
    for index in 0..5 {
        lib.mp3(
            &format!("{index}.mp3"),
            index,
            &tagged("Song", "Artist", "Album"),
        );
    }
    let cancel = AtomicBool::new(true);
    let report = scan_library(&lib.db, None, ScanOptions::default(), &cancel, &mut |_| {}).unwrap();
    assert!(report.canceled);
    assert_eq!(report.added, 0);

    let report = lib.scan();
    assert!(!report.canceled);
    assert_eq!(report.added, 5);
}

#[test]
fn scanning_with_many_threads_gives_the_same_library() {
    let contents = |threads| {
        let lib = library();
        for index in 0..40_u32 {
            lib.mp3(
                &format!("album{}/{index:02}.mp3", index % 4),
                index,
                &tagged("Song", "Artist", "Album"),
            );
        }
        let report = lib.scan_with(ScanOptions {
            threads,
            fingerprint_samples: 3,
        });
        assert_eq!(report.added, 40);
        lib.query(|conn| {
            conn.prepare(
                "SELECT t.rel_path, t.title, a.group_key, hex(t.fingerprint)
                 FROM tracks t JOIN albums a ON a.id = t.album_id ORDER BY t.rel_path",
            )?
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()
        })
        .into_iter()
        .map(|(path, title, key, print)| {
            let key = key
                .rsplit_once(':')
                .map_or(key.clone(), |(_, folder)| folder.to_owned());
            (path, title, key, print)
        })
        .collect::<Vec<_>>()
    };
    assert_eq!(contents(1), contents(8));
}

#[test]
fn changed_paths_can_be_scanned_without_walking_the_library() {
    let lib = library();
    lib.mp3("x/a.mp3", 1, &tagged("A", "X", "Y"));
    lib.mp3("x/b.mp3", 2, &tagged("B", "X", "Y"));
    lib.mp3("y/c.mp3", 3, &tagged("C", "X", "Y"));
    lib.scan();

    // Each of these changes on disk, but only the named paths are looked at.
    lib.mp3("x/a.mp3", 1, &tagged("A Edited Title", "X", "Y"));
    lib.mp3("y/c.mp3", 3, &tagged("C Edited Title", "X", "Y"));
    lib.mp3("x/new.mp3", 4, &tagged("New", "X", "Y"));
    fs::remove_file(lib.path("x/b.mp3")).unwrap();

    let report = lib.scan_only(&["x/a.mp3", "x/new.mp3", "x/b.mp3"]);
    assert_eq!(
        (report.updated, report.added, report.missing),
        (1, 1, 1),
        "{report:?}"
    );
    assert_eq!(lib.track("x/a.mp3").title, "A Edited Title");
    assert!(lib.track("x/b.mp3").missing);
    assert_eq!(lib.track("y/c.mp3").title, "C");

    // A removed folder marks every track under it, and only those.
    lib.mp3("z/d.mp3", 5, &tagged("D", "X", "Y"));
    lib.scan_only(&["z"]);
    assert!(!lib.track("z/d.mp3").missing);
    fs::remove_dir_all(lib.path("x")).unwrap();
    let report = lib.scan_only(&["x"]);
    assert_eq!(report.missing, 2);
    assert!(lib.track("x/a.mp3").missing);
    assert!(!lib.track("y/c.mp3").missing);
    assert!(!lib.track("z/d.mp3").missing);
}

#[test]
fn an_unknown_root_is_an_error_and_a_chosen_root_scans_alone() {
    let lib = library();
    let cancel = AtomicBool::new(false);
    let unknown = ["no-such-root".to_owned()];
    assert!(matches!(
        scan_library(
            &lib.db,
            Some(&unknown),
            ScanOptions::default(),
            &cancel,
            &mut |_| {}
        ),
        Err(ScanError::UnknownRoot)
    ));

    lib.mp3("a.mp3", 1, &tagged("A", "X", "Y"));
    let other = lib.dir.path().join("other");
    fs::create_dir(&other).unwrap();
    fs::write(other.join("b.mp3"), mp3_bytes(2, &tagged("B", "X", "Y"))).unwrap();
    add_root(&lib.db, other.to_str().unwrap(), None).unwrap();

    let chosen = [lib.root_id.clone()];
    let report = scan_library(
        &lib.db,
        Some(&chosen),
        ScanOptions::default(),
        &cancel,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(report.added, 1);
    assert_eq!(lib.scan().added, 1);
}

#[test]
fn progress_reaches_the_total() {
    let lib = library();
    for index in 0..3 {
        lib.mp3(&format!("{index}.mp3"), index, &tagged("S", "A", "B"));
    }
    let cancel = AtomicBool::new(false);
    let mut updates = Vec::new();
    scan_library(
        &lib.db,
        None,
        ScanOptions::default(),
        &cancel,
        &mut |update| {
            updates.push(update);
        },
    )
    .unwrap();
    assert_eq!(updates.first(), Some(&ScanProgress { done: 0, total: 3 }));
    assert_eq!(updates.last(), Some(&ScanProgress { done: 3, total: 3 }));
}

#[test]
fn scanned_copies_are_found_as_duplicates() {
    use crate::duplicates::{DuplicateMatch, duplicate_page};

    let lib = library();
    let tags = [
        ("TIT2", "Song"),
        ("TPE1", "Artist"),
        ("TALB", "Album"),
        ("TRCK", "1"),
    ];
    lib.mp3("copy/same.mp3", 1, &tags);
    lib.mp3("other/same.mp3", 1, &[("TIT2", "Retagged")]);
    lib.mp3("reencoded/song.mp3", 2, &tags);
    lib.mp3("elsewhere/different.mp3", 3, &[("TIT2", "Different")]);
    lib.scan();

    let find = |by| {
        lib.db
            .read(|conn| Ok::<_, DbError>(duplicate_page(conn, by, 0, 100)?))
            .unwrap()
            .groups
            .into_iter()
            .map(|group| group.tracks.into_iter().map(|t| t.rel_path).collect())
            .collect::<Vec<Vec<String>>>()
    };
    assert_eq!(
        find(DuplicateMatch::Audio),
        [["copy/same.mp3", "other/same.mp3"]]
    );
    assert_eq!(
        find(DuplicateMatch::Tags),
        [["copy/same.mp3", "reencoded/song.mp3"]]
    );

    fs::remove_file(lib.path("other/same.mp3")).unwrap();
    lib.scan();
    assert!(find(DuplicateMatch::Audio).is_empty());
}
