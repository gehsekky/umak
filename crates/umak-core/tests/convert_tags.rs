//! Converts tagged files of each format to each encoding through FFmpeg, copying the tags
//! and art across, and reads the results back. Needs FFmpeg, found as the app finds it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

use lofty::TextEncoding;
use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::{AudioFile, FileType, TaggedFileExt};
use lofty::id3::v2::{ExtendedTextFrame, Frame, Id3v2Tag, UnsynchronizedTextFrame};
use lofty::mp4::{Atom, AtomData, AtomIdent, Ilst};
use lofty::ogg::tag::VorbisComments;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::probe::Probe;
use lofty::tag::{Accessor, ItemKey, ItemValue, Tag, TagExt, TagItem, TagType};
use umak_convert::{Conversion, Encoding, Ffmpeg};
use umak_core::scan::tags::{TrackTags, read_file};
use umak_core::tagcopy::{TagCopyReport, copy_tags};

fn ffmpeg() -> Ffmpeg {
    Ffmpeg::find().unwrap_or_else(|error| panic!("these tests need FFmpeg: {error}"))
}

fn run_ffmpeg(ffmpeg: &Ffmpeg, args: &[&str], output: &Path) {
    let status = Command::new(ffmpeg.path())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .arg(output)
        .status()
        .unwrap();
    assert!(status.success(), "ffmpeg {args:?}");
}

/// Two pictures, a front cover in PNG and a back cover in JPEG, made by FFmpeg.
fn pictures(ffmpeg: &Ffmpeg, dir: &Path) -> Vec<Picture> {
    let mut pictures = Vec::new();
    for (name, color, kind, mime) in [
        ("front.png", "red", PictureType::CoverFront, MimeType::Png),
        ("back.jpg", "blue", PictureType::CoverBack, MimeType::Jpeg),
    ] {
        let path = dir.join(name);
        let source = format!("color=c={color}:s=16x16");
        run_ffmpeg(
            ffmpeg,
            &["-f", "lavfi", "-i", &source, "-frames:v", "1"],
            &path,
        );
        pictures.push(
            Picture::unchecked(fs::read(path).unwrap())
                .pic_type(kind)
                .mime_type(mime)
                .build(),
        );
    }
    pictures
}

const ARTISTS: [&str; 2] = ["First Artist", "Second Artist"];
const GENRES: [&str; 2] = ["Jazz", "Blues"];

/// The fields every source gets, through the tag library's own mapping.
fn generic_tag(tag_type: TagType, pictures: &[Picture]) -> Tag {
    let mut tag = Tag::new(tag_type);
    let mut push = |key: ItemKey, value: &str| {
        assert!(
            tag.push(TagItem::new(key, ItemValue::Text(value.to_owned()))),
            "{key:?} in {tag_type:?}"
        );
    };
    push(ItemKey::TrackTitle, "Tïtle — with “quotes”");
    push(ItemKey::AlbumTitle, "The Album");
    for artist in ARTISTS {
        push(ItemKey::TrackArtist, artist);
    }
    push(ItemKey::AlbumArtist, "AC/DC");
    push(ItemKey::Composer, "Writer");
    for genre in GENRES {
        push(ItemKey::Genre, genre);
    }
    push(ItemKey::RecordingDate, "2001-05-03");
    push(ItemKey::TrackNumber, "3");
    push(ItemKey::TrackTotal, "12");
    push(ItemKey::DiscNumber, "1");
    push(ItemKey::DiscTotal, "2");
    push(ItemKey::TrackArtistSortOrder, "Artist, First");
    push(ItemKey::AlbumTitleSortOrder, "Album, The");
    push(ItemKey::Comment, "A comment");
    if tag_type != TagType::Id3v2 {
        push(ItemKey::Lyrics, "Some words");
    }
    push(ItemKey::ReplayGainTrackGain, "-6.54 dB");
    push(ItemKey::ReplayGainTrackPeak, "0.987654");
    push(
        ItemKey::MusicBrainzReleaseId,
        "0f2c1b2a-1111-2222-3333-444455556666",
    );
    push(ItemKey::FlagCompilation, "1");
    for picture in pictures {
        tag.push_picture(picture.clone());
    }
    tag
}

/// Fields of the user's own, and ones that describe the old encoding and must not carry.
const CUSTOM: [(&str, &str); 2] = [("MY FIELD", "kept"), ("MY FIELD", "also kept")];
const ENCODING: [(&str, &str); 1] = [("iTunSMPB", " 00000000 00000840 000001CA 00000000000F0A36")];

fn tag_source(path: &Path, kind: FileType, id3_version_3: bool, pictures: &[Picture]) {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let write = if id3_version_3 {
        WriteOptions::new().use_id3v23(true)
    } else {
        WriteOptions::new()
    };
    match kind {
        FileType::Mpeg => {
            let mut tag = Id3v2Tag::from(generic_tag(TagType::Id3v2, pictures));
            let values: Vec<&str> = CUSTOM.iter().map(|(_, value)| *value).collect();
            tag.insert(Frame::UserText(ExtendedTextFrame::new(
                TextEncoding::UTF8,
                "MY FIELD",
                values.join("\0"),
            )));
            for (name, value) in ENCODING {
                tag.insert(Frame::UserText(ExtendedTextFrame::new(
                    TextEncoding::UTF8,
                    name,
                    value,
                )));
            }
            tag.insert(Frame::UnsynchronizedText(UnsynchronizedTextFrame::new(
                TextEncoding::UTF8,
                *b"eng",
                "",
                "Some words",
            )));
            tag.save_to(&mut file, write).unwrap();
        }
        FileType::Flac => {
            let mut tag = VorbisComments::from(generic_tag(TagType::VorbisComments, pictures));
            for (name, value) in CUSTOM.into_iter().chain(ENCODING) {
                tag.push(name.to_owned(), value.to_owned());
            }
            tag.push("ENCODER".to_owned(), "an old encoder".to_owned());
            tag.save_to(&mut file, write).unwrap();
        }
        FileType::Mp4 => {
            let mut tag = Ilst::from(generic_tag(TagType::Mp4Ilst, pictures));
            for (name, values) in [
                ("MY FIELD", CUSTOM.map(|(_, value)| value).to_vec()),
                ("iTunSMPB", ENCODING.map(|(_, value)| value).to_vec()),
            ] {
                let ident = AtomIdent::Freeform {
                    mean: "com.apple.iTunes".into(),
                    name: name.into(),
                };
                let data = values
                    .into_iter()
                    .map(|value| AtomData::UTF8(value.to_owned()))
                    .collect();
                tag.insert(Atom::from_collection(ident, data).unwrap());
            }
            tag.save_to(&mut file, write).unwrap();
        }
        other => panic!("no source of {other:?}"),
    }
}

struct Source {
    name: &'static str,
    path: PathBuf,
}

fn sources(ffmpeg: &Ffmpeg, dir: &Path, pictures: &[Picture]) -> Vec<Source> {
    let tone = ["-f", "lavfi", "-i", "sine=f=440:d=2", "-ac", "2"];
    let mut sources = Vec::new();
    for (name, file, encode, kind, v23) in [
        (
            "MP3 ID3v2.4",
            "v24.mp3",
            &["-c:a", "libmp3lame", "-q:a", "5"][..],
            FileType::Mpeg,
            false,
        ),
        (
            "MP3 ID3v2.3",
            "v23.mp3",
            &["-c:a", "libmp3lame", "-q:a", "5"][..],
            FileType::Mpeg,
            true,
        ),
        (
            "FLAC",
            "src.flac",
            &["-c:a", "flac"][..],
            FileType::Flac,
            false,
        ),
        (
            "ALAC",
            "alac.m4a",
            &["-c:a", "alac"][..],
            FileType::Mp4,
            false,
        ),
        (
            "AAC",
            "aac.m4a",
            &["-c:a", "aac", "-b:a", "128k"][..],
            FileType::Mp4,
            false,
        ),
    ] {
        let path = dir.join(file);
        let mut args: Vec<&str> = tone.to_vec();
        args.extend(args_without_tags());
        args.extend(encode);
        run_ffmpeg(ffmpeg, &args, &path);
        tag_source(&path, kind, v23, pictures);
        sources.push(Source { name, path });
    }
    sources
}

fn args_without_tags() -> [&'static str; 6] {
    [
        "-map_metadata",
        "-1",
        "-fflags",
        "+bitexact",
        "-id3v2_version",
        "0",
    ]
}

const TARGETS: [Encoding; 6] = [
    Encoding::Flac,
    Encoding::Alac,
    Encoding::Mp3Vbr(2),
    Encoding::Mp3Cbr(192),
    Encoding::Aac(128),
    Encoding::Opus(96),
];

fn convert(ffmpeg: &Ffmpeg, source: &Path, output: PathBuf, encoding: Encoding) -> TagCopyReport {
    let conversion = Conversion {
        input: source.to_owned(),
        output: output.clone(),
        encoding,
        sample_rate: None,
        bits: None,
    };
    let mut report = None;
    ffmpeg
        .convert(
            &conversion,
            &AtomicBool::new(false),
            |_| {},
            |partial| {
                report = Some(copy_tags(source, partial).map_err(|error| error.to_string())?);
                Ok(())
            },
        )
        .unwrap_or_else(|error| panic!("{}: {error}", output.display()));
    report.unwrap()
}

fn model(path: &Path) -> TrackTags {
    read_file(fs::File::open(path).unwrap()).unwrap().tags
}

/// The values of a field of the user's own, read through each format's own type.
fn custom(path: &Path, name: &str) -> Vec<String> {
    let mut file = fs::File::open(path).unwrap();
    let kind = Probe::open(path)
        .unwrap()
        .guess_file_type()
        .unwrap()
        .file_type();
    match kind {
        Some(FileType::Mpeg) => {
            let mpeg = lofty::mpeg::MpegFile::read_from(&mut file, ParseOptions::new()).unwrap();
            let Some(tag) = mpeg.id3v2() else {
                return Vec::new();
            };
            tag.into_iter()
                .filter_map(|frame| match frame {
                    Frame::UserText(text) if text.description.eq_ignore_ascii_case(name) => Some(
                        text.content
                            .split('\0')
                            .map(str::to_owned)
                            .collect::<Vec<_>>(),
                    ),
                    _ => None,
                })
                .flatten()
                .collect()
        }
        Some(FileType::Opus) => {
            let opus = lofty::ogg::OpusFile::read_from(&mut file, ParseOptions::new()).unwrap();
            opus.vorbis_comments()
                .get_all(name)
                .map(str::to_owned)
                .collect()
        }
        _ => model(path)
            .raw
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
            .collect(),
    }
}

fn lyrics(path: &Path) -> Vec<String> {
    let tagged = Probe::open(path).unwrap().read().unwrap();
    let mut found: Vec<String> = tagged
        .tags()
        .iter()
        .flat_map(|tag| {
            tag.get_strings(ItemKey::Lyrics)
                .chain(tag.get_strings(ItemKey::UnsyncLyrics))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect();
    if let Ok(mut file) = fs::File::open(path)
        && tagged.file_type() == FileType::Mpeg
        && let Ok(mpeg) = lofty::mpeg::MpegFile::read_from(&mut file, ParseOptions::new())
        && let Some(tag) = mpeg.id3v2()
    {
        found.extend(tag.unsync_text().map(|frame| frame.content.to_string()));
    }
    found.dedup();
    found
}

fn picture_data(path: &Path) -> Vec<Vec<u8>> {
    let tagged = Probe::open(path)
        .unwrap()
        .options(ParseOptions::new().read_cover_art(true))
        .read()
        .unwrap();
    let mut data: Vec<Vec<u8>> = tagged
        .tags()
        .iter()
        .flat_map(|tag| tag.pictures().iter().map(|picture| picture.data().to_vec()))
        .collect();
    data.sort();
    data
}

/// Decodes the whole file, which catches a tag written over the audio's place in the file.
fn decode_problem(ffmpeg: &Ffmpeg, path: &Path) -> Option<String> {
    let output = Command::new(ffmpeg.path())
        .args(["-hide_banner", "-v", "error", "-xerror", "-i"])
        .arg(path)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    let errors = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    (!output.status.success() || !errors.is_empty()).then_some(errors)
}

/// An ID3v2.4 tag holding text frames, as raw bytes.
fn raw_id3v24(frames: &[(&str, &str)]) -> Vec<u8> {
    let synchsafe = |size: usize| {
        (0..4)
            .rev()
            .map(move |shift| ((size >> (7 * shift)) & 0x7f) as u8)
    };
    let mut body = Vec::new();
    for (id, text) in frames {
        body.extend_from_slice(id.as_bytes());
        body.extend(synchsafe(text.len() + 1));
        body.extend([0, 0, 3]);
        body.extend_from_slice(text.as_bytes());
    }
    let mut tag = b"ID3\x04\x00\x00".to_vec();
    tag.extend(synchsafe(body.len()));
    tag.extend(body);
    tag
}

/// Some taggers add a tag after the one already there instead of changing it. Readers take
/// frames from both, a later frame replacing an earlier one of its kind, and so does the copy.
#[test]
fn stacked_id3_tags_arrive_as_the_library_reads_them() {
    let ffmpeg = ffmpeg();
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("stacked.mp3");
    let mut args: Vec<&str> = vec!["-f", "lavfi", "-i", "sine=d=1"];
    args.extend(args_without_tags());
    args.extend(["-c:a", "libmp3lame"]);
    run_ffmpeg(&ffmpeg, &args, &source);

    let mut first = Id3v2Tag::new();
    first.set_title("Title".to_owned());
    first.set_album("First Album".to_owned());
    first.insert(Frame::UserText(ExtendedTextFrame::new(
        TextEncoding::UTF8,
        "MY FIELD",
        "kept",
    )));
    first
        .save_to_path(&source, WriteOptions::new().use_id3v23(true))
        .unwrap();
    let mut bytes = fs::read(&source).unwrap();
    let size = bytes[6..10]
        .iter()
        .fold(0, |size, &byte| (size << 7) | usize::from(byte));
    let second = raw_id3v24(&[("TALB", "Second Album"), ("TDRC", "2009")]);
    bytes.splice(10 + size..10 + size, second);
    fs::write(&source, bytes).unwrap();
    let read = model(&source);
    assert_eq!(read.album.as_deref(), Some("Second Album"));

    for encoding in [Encoding::Mp3Vbr(2), Encoding::Flac] {
        let output = dir.path().join(format!("out.{}", encoding.extension()));
        let report = convert(&ffmpeg, &source, output.clone(), encoding);
        assert_eq!(report.left_out, Vec::<String>::new(), "{encoding:?}");
        let tags = model(&output);
        assert_eq!(tags.title.as_deref(), Some("Title"), "{encoding:?}");
        assert_eq!(tags.album.as_deref(), Some("Second Album"), "{encoding:?}");
        assert_eq!(tags.year, read.year, "{encoding:?}");
        assert_eq!(custom(&output, "MY FIELD"), ["kept"], "{encoding:?}");
    }
}

/// A rating written as text, as some players do, cannot become ID3's binary rating frame,
/// so it goes across under its own name.
#[test]
fn a_rating_in_text_goes_across_by_name() {
    let ffmpeg = ffmpeg();
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("rated.flac");
    run_ffmpeg(
        &ffmpeg,
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=d=1",
            "-c:a",
            "flac",
            "-fflags",
            "+bitexact",
            "-metadata",
            "TITLE=Rated",
            "-metadata",
            "RATING=2.5",
        ],
        &source,
    );
    for encoding in [Encoding::Mp3Vbr(2), Encoding::Aac(128)] {
        let output = dir.path().join(format!("out.{}", encoding.extension()));
        let report = convert(&ffmpeg, &source, output.clone(), encoding);
        assert_eq!(report.left_out, Vec::<String>::new(), "{encoding:?}");
        assert_eq!(custom(&output, "RATING"), ["2.5"], "{encoding:?}");
    }
}

#[test]
fn reports_what_the_new_format_cannot_hold() {
    let ffmpeg = ffmpeg();
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("odd.mp3");
    let mut args: Vec<&str> = vec!["-f", "lavfi", "-i", "sine=d=1"];
    args.extend(args_without_tags());
    args.extend(["-c:a", "libmp3lame"]);
    run_ffmpeg(&ffmpeg, &args, &source);

    let mut tag = Id3v2Tag::new();
    tag.set_title("Odd".to_owned());
    // An empty field, which has nothing to carry and is not reported.
    tag.insert(Frame::Text(lofty::id3::v2::TextInformationFrame::new(
        lofty::id3::v2::FrameId::Valid("TCOM".into()),
        TextEncoding::UTF8,
        String::new(),
    )));
    // A private frame, which only ID3 has, and a field name a Vorbis comment cannot use.
    tag.insert(Frame::Private(lofty::id3::v2::PrivateFrame::new(
        "com.example",
        vec![1, 2, 3],
    )));
    tag.insert(Frame::UserText(ExtendedTextFrame::new(
        TextEncoding::UTF8,
        "Ünïcode name",
        "value",
    )));
    // GIF art, which MP4 has a type for, as it does JPEG, PNG and BMP.
    let gif = dir.path().join("art.gif");
    run_ffmpeg(
        &ffmpeg,
        &["-f", "lavfi", "-i", "color=c=green:s=8x8", "-frames:v", "1"],
        &gif,
    );
    tag.insert_picture(
        Picture::unchecked(fs::read(&gif).unwrap())
            .pic_type(PictureType::CoverFront)
            .mime_type(MimeType::Gif)
            .build(),
    );
    // WebP art, which MP4 has no type for.
    let webp = dir.path().join("art.webp");
    run_ffmpeg(
        &ffmpeg,
        &["-f", "lavfi", "-i", "color=c=blue:s=8x8", "-frames:v", "1"],
        &webp,
    );
    tag.insert_picture(
        Picture::unchecked(fs::read(&webp).unwrap())
            .pic_type(PictureType::CoverBack)
            .mime_type(MimeType::Unknown("image/webp".to_owned()))
            .build(),
    );
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&source)
        .unwrap();
    tag.save_to(&mut file, WriteOptions::new()).unwrap();
    drop(file);

    let report = convert(
        &ffmpeg,
        &source,
        dir.path().join("odd.flac"),
        Encoding::Flac,
    );
    assert_eq!(report.left_out, ["PRIV", "Ünïcode name"]);
    assert_eq!(
        model(&dir.path().join("odd.flac")).title.as_deref(),
        Some("Odd")
    );

    let report = convert(
        &ffmpeg,
        &source,
        dir.path().join("odd.m4a"),
        Encoding::Aac(128),
    );
    assert!(report.left_out.contains(&"PRIV".to_owned()), "{report:?}");
    assert!(
        report.left_out.contains(&"picture".to_owned()),
        "{report:?}"
    );
    assert_eq!(
        picture_data(&dir.path().join("odd.m4a")),
        [fs::read(&gif).unwrap()],
        "GIF art is kept, and WebP is not"
    );
    // Freeform MP4 atoms take any name.
    assert!(
        !report.left_out.contains(&"Ünïcode name".to_owned()),
        "{report:?}"
    );

    // The same format keeps everything, frame for frame.
    let report = convert(
        &ffmpeg,
        &source,
        dir.path().join("odd2.mp3"),
        Encoding::Mp3Vbr(4),
    );
    assert!(report.left_out.is_empty(), "{report:?}");
}

#[test]
fn tags_and_art_survive_every_conversion() {
    let ffmpeg = ffmpeg();
    let dir = tempfile::tempdir().unwrap();
    let pictures = pictures(&ffmpeg, dir.path());
    let mut expected_pictures: Vec<Vec<u8>> = pictures.iter().map(|p| p.data().to_vec()).collect();
    expected_pictures.sort();
    let mut failures = Vec::new();
    for source in sources(&ffmpeg, dir.path(), &pictures) {
        let before = model(&source.path);
        assert_eq!(before.artists, ARTISTS, "{} source", source.name);
        assert_eq!(
            picture_data(&source.path),
            expected_pictures,
            "{} source art",
            source.name
        );
        let source_bytes = fs::read(&source.path).unwrap();
        for (index, encoding) in TARGETS.into_iter().enumerate() {
            let output = dir.path().join(format!(
                "{}-{index}.{}",
                source.path.file_stem().unwrap().to_string_lossy(),
                encoding.extension()
            ));
            let report = convert(&ffmpeg, &source.path, output.clone(), encoding);
            let after = model(&output);
            let label = format!("{} to {encoding:?}", source.name);
            println!("{label}: left out {:?}", report.left_out);

            let mut differs = Vec::new();
            macro_rules! same {
                ($($field:ident),*) => {$(
                    if before.$field != after.$field {
                        differs.push(format!("{}: {:?} became {:?}", stringify!($field), before.$field, after.$field));
                    }
                )*};
            }
            same!(
                title,
                album,
                artists,
                album_artists,
                composers,
                genres,
                year,
                track_no,
                disc_no,
                artist_sort,
                album_sort,
                compilation,
                rg_track_gain,
                rg_track_peak,
                musicbrainz_album_id
            );
            if custom(&output, "MY FIELD") != ["kept", "also kept"] {
                differs.push(format!("MY FIELD: {:?}", custom(&output, "MY FIELD")));
            }
            if !custom(&output, "iTunSMPB").is_empty() {
                differs.push("the old encoding's iTunSMPB came across".to_owned());
            }
            if custom(&output, "ENCODER").contains(&"an old encoder".to_owned()) {
                differs.push("the old ENCODER came across".to_owned());
            }
            if lyrics(&output) != ["Some words"] {
                differs.push(format!("lyrics: {:?}", lyrics(&output)));
            }
            if let Some(problem) = decode_problem(&ffmpeg, &output) {
                differs.push(format!("does not decode: {problem}"));
            }
            if source.name.starts_with("MP3") && output.extension().is_some_and(|ext| ext == "mp3")
            {
                let version = fs::read(&output).unwrap()[3];
                let expected = if source.name.ends_with("2.3") { 3 } else { 4 };
                if version != expected {
                    differs.push(format!("ID3v2.{version} instead of 2.{expected}"));
                }
            }
            if picture_data(&output) != expected_pictures {
                differs.push(format!("{} pictures", picture_data(&output).len()));
            }
            if !differs.is_empty() {
                failures.push(format!(
                    "{label}: {differs:?} (reported {:?})",
                    report.left_out
                ));
            }
        }
        assert_eq!(
            fs::read(&source.path).unwrap(),
            source_bytes,
            "{} changed",
            source.name
        );
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// What the player decodes from a converted file, with the encoder's silence taken off,
/// matches the source frame for frame, so converted albums still join without a gap.
#[cfg(feature = "engine")]
#[test]
fn converted_files_play_the_source_length() {
    use umak_audio::engine::check_file;

    let ffmpeg = ffmpeg();
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.flac");
    // A length that is not a whole number of encoder blocks, at two rates.
    for rate in ["44100", "48000"] {
        let tone = format!("sine=f=440:d=3.3333:sample_rate={rate}");
        let mut args: Vec<&str> = vec!["-f", "lavfi", "-i", &tone, "-ac", "2", "-c:a", "flac"];
        args.extend(args_without_tags());
        run_ffmpeg(&ffmpeg, &args, &source);
        let rate: u32 = rate.parse().unwrap();
        let expected = check_file(source.to_str().unwrap(), rate).unwrap().frames;
        for encoding in [
            Encoding::Aac(192),
            Encoding::Mp3Vbr(2),
            Encoding::Alac,
            Encoding::Flac,
        ] {
            let output = dir.path().join(format!("{rate}.{}", encoding.extension()));
            let _ = fs::remove_file(&output);
            convert(&ffmpeg, &source, output.clone(), encoding);
            let found = check_file(output.to_str().unwrap(), rate).unwrap();
            assert_eq!(found.frames, expected, "{encoding:?} at {rate} Hz");
            assert_eq!(found.decode_errors, 0, "{encoding:?} at {rate} Hz");
        }
    }
}
