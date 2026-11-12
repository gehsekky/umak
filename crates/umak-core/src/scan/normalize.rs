//! Text normalization for identity and sorting.

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

pub const VARIOUS_ARTISTS: &str = "Various Artists";
pub const UNKNOWN_ARTIST: &str = "Unknown Artist";
pub const UNKNOWN_ALBUM: &str = "Unknown Album";

const VARIOUS_KEY: &str = "various artists";
const VARIOUS_SPELLINGS: &[&str] = &[
    VARIOUS_KEY,
    "various",
    "various artist",
    "va",
    "v.a.",
    "v.a",
    "v/a",
];
/// Leading words left out when sorting names, unless the settings give others.
pub const SORT_ARTICLES: [&str; 3] = ["the", "a", "an"];

/// The identity of a name: spellings that differ only by case, whitespace or Unicode
/// composition share a key. Accents stay significant.
pub fn identity_key(name: &str) -> String {
    let collapsed = name.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.nfkc().collect::<String>().to_lowercase()
}

/// The artist key, with every spelling of "Various Artists" mapped to the reserved row.
pub fn artist_key(name: &str) -> String {
    let key = identity_key(name);
    if VARIOUS_SPELLINGS.contains(&key.as_str()) {
        VARIOUS_KEY.to_owned()
    } else {
        key
    }
}

pub fn is_various(name: &str) -> bool {
    artist_key(name) == VARIOUS_KEY
}

fn fold(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
        .nfkd()
        .filter(|ch| !is_combining_mark(*ch))
        .nfc()
        .collect::<String>()
        .to_lowercase()
}

/// A sort value from a sort tag, which already says how the name should sort.
pub fn sort_key_from_tag(tag: &str) -> String {
    fold(tag)
}

/// Articles as `sort_key` takes them, folded as names are.
pub fn sort_articles(articles: &[impl AsRef<str>]) -> Vec<String> {
    articles
        .iter()
        .map(|article| fold(article.as_ref()))
        .filter(|article| !article.is_empty())
        .collect()
}

/// A sort value computed from a display name: case and accents are ignored, and
/// leading punctuation and articles are dropped. An article is a word followed by a space,
/// or one ending in an apostrophe, such as "l'", followed by anything.
pub fn sort_key(name: &str, articles: &[String]) -> String {
    let folded = fold(name);
    let text = folded.trim_start_matches(|ch: char| !ch.is_alphanumeric());
    for article in articles {
        let Some(rest) = text.strip_prefix(article.as_str()) else {
            continue;
        };
        let rest = if article.ends_with(['\'', '\u{2019}']) {
            rest
        } else if let Some(rest) = rest.strip_prefix(' ') {
            rest
        } else {
            continue;
        };
        if !rest.is_empty() {
            return rest.to_owned();
        }
    }
    if text.is_empty() {
        folded
    } else {
        text.to_owned()
    }
}

/// Whether a folder name marks one disc of a release, like `CD1` or `Disc 2`.
fn is_disc_folder(name: &str) -> bool {
    let lower = name.trim().to_lowercase();
    let Some(rest) = ["disc", "disk", "cd"]
        .iter()
        .find_map(|prefix| lower.strip_prefix(prefix))
    else {
        return false;
    };
    let rest = rest.trim_start_matches([' ', '_', '-', '.']);
    !rest.is_empty() && rest.chars().all(|ch| ch.is_ascii_digit())
}

/// The folder that identifies a track's album: its parent, skipping any disc folders.
/// They can nest, as in `CD2/CD3`.
pub fn album_folder(rel_path: &str) -> &str {
    let mut folder = rel_path.rsplit_once('/').map_or("", |(parent, _)| parent);
    loop {
        match folder.rsplit_once('/') {
            Some((parent, name)) if is_disc_folder(name) => folder = parent,
            None if is_disc_folder(folder) => return "",
            _ => return folder,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn english() -> Vec<String> {
        sort_articles(&SORT_ARTICLES)
    }

    #[test]
    fn the_articles_given_are_left_out_including_elided_ones() {
        let articles = sort_articles(&["Die", "L'", "the"]);
        assert_eq!(sort_key("Die Toten Hosen", &articles), "toten hosen");
        assert_eq!(sort_key("L'Arc-en-Ciel", &articles), "arc-en-ciel");
        assert_eq!(sort_key("Lambchop", &articles), "lambchop");
        assert_eq!(
            sort_key("A Tribe Called Quest", &articles),
            "a tribe called quest"
        );
        assert_eq!(sort_key("The The", &[]), "the the");
    }

    #[test]
    fn identity_ignores_case_and_composition_but_keeps_accents() {
        assert_eq!(identity_key("The Beatles"), identity_key("the  BEATLES "));
        assert_eq!(identity_key("Björk"), identity_key("BJÖRK"));
        // Precomposed and combining forms of the same letter.
        assert_eq!(identity_key("Bj\u{f6}rk"), identity_key("Bjo\u{308}rk"));
        assert_ne!(identity_key("Björk"), identity_key("Bjork"));
    }

    #[test]
    fn various_artist_spellings_share_the_reserved_key() {
        for name in ["VA", "V.A.", "v/a", "Various", "various artists"] {
            assert!(is_various(name), "{name}");
            assert_eq!(artist_key(name), "various artists");
        }
        assert!(!is_various("Vangelis"));
        assert_eq!(artist_key("Vangelis"), "vangelis");
    }

    #[test]
    fn sort_keys_drop_accents_articles_and_leading_punctuation() {
        assert_eq!(sort_key("The Beatles", &english()), "beatles");
        assert_eq!(sort_key("Theory", &english()), "theory");
        assert_eq!(sort_key("Björk", &english()), "bjork");
        assert_eq!(sort_key("'Til Tuesday", &english()), "til tuesday");
        assert_eq!(
            sort_key("A Tribe Called Quest", &english()),
            "tribe called quest"
        );
        assert_eq!(
            sort_key("(What's the Story)", &english()),
            "what's the story)"
        );
        // A name that is only an article or punctuation stays usable.
        assert_eq!(sort_key("The", &english()), "the");
        assert_eq!(sort_key("...", &english()), "...");
        assert_eq!(sort_key("!!!", &english()), "!!!");
    }

    #[test]
    fn a_sort_tag_keeps_its_own_word_order() {
        assert_eq!(sort_key_from_tag("Beatles, The"), "beatles, the");
        assert_eq!(sort_key_from_tag("The Björk"), "the bjork");
    }

    #[test]
    fn disc_folders_count_as_their_parent() {
        for (path, folder) in [
            ("Artist/Album/CD1/01.mp3", "Artist/Album"),
            ("Artist/Album/Disc 2/01.mp3", "Artist/Album"),
            ("Artist/Album/cd_3/01.mp3", "Artist/Album"),
            ("Artist/Album/disk-12/01.mp3", "Artist/Album"),
            ("Artist/Album/CD2/CD3/01.mp3", "Artist/Album"),
            ("Artist/Album/01.mp3", "Artist/Album"),
            ("CD1/01.mp3", ""),
            ("01.mp3", ""),
            // A disc number inside a longer name identifies its own folder.
            ("Artist/Album (CD1)/01.mp3", "Artist/Album (CD1)"),
            ("Artist/CD Single/01.mp3", "Artist/CD Single"),
            ("Artist/Discography/01.mp3", "Artist/Discography"),
        ] {
            assert_eq!(album_folder(path), folder, "{path}");
        }
    }
}
