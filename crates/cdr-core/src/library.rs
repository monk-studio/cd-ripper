//! Where ripped files go in a music library, and how they get there safely.
//!
//! Library servers (Navidrome, Jellyfin) group by tags, so the layout only has
//! to be readable and collision-free. Multi-disc albums stay in one folder with
//! a `1-` style prefix, as MusicBrainz Picard does by default.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;

use crate::tags::Tags;

/// Placeholders: `{albumartist}`, `{artist}`, `{album}`, `{year}`, `{title}`,
/// `{track}` (two digits), `{disc}`, `{discprefix}` (`2-` on multi-disc
/// releases, else empty). `/` separates folders; `.flac` is appended.
pub const DEFAULT_TEMPLATE: &str = "{albumartist}/{album} ({year})/{discprefix}{track} - {title}";

const COPY_ATTEMPTS: u32 = 3;
/// Leaves room for a `.part` suffix within the usual 255-byte name limit.
const MAX_COMPONENT_BYTES: usize = 240;

/// Library-relative path of a track's FLAC file.
pub fn track_path(template: &str, tags: &Tags) -> PathBuf {
    let get = |key: &str| {
        tags.get(key)
            .and_then(|v| v.first())
            .map(String::as_str)
            .unwrap_or("")
    };
    let number = |key: &str| get(key).parse::<u32>().unwrap_or(0);

    let track = number("TRACKNUMBER");
    let (disc, disc_total) = (number("DISCNUMBER"), number("DISCTOTAL"));
    let artist = Some(get("ARTIST")).filter(|s| !s.is_empty());
    let album_artist = Some(get("ALBUMARTIST"))
        .filter(|s| !s.is_empty())
        .or(artist)
        .unwrap_or("Unknown Artist");
    let album = match get("ALBUM") {
        "" => format!("Unknown Album [{}]", get("MUSICBRAINZ_DISCID")),
        a => a.to_owned(),
    };
    let title = match get("TITLE") {
        "" => format!("Track {track:02}"),
        t => t.to_owned(),
    };
    let year: String = get("DATE").chars().take(4).collect();
    let disc_prefix = if disc_total > 1 {
        format!("{disc}-")
    } else {
        String::new()
    };

    let values = [
        ("{albumartist}", album_artist),
        ("{artist}", artist.unwrap_or(album_artist)),
        ("{album}", &album),
        ("{year}", &year),
        ("{title}", &title),
        ("{track}", &format!("{track:02}")),
        ("{disc}", &disc.to_string()),
        ("{discprefix}", &disc_prefix),
    ];
    let mut parts: Vec<String> = template
        .split('/')
        .map(|part| {
            // Substitute and sanitise each value before joining, so a "/" in a
            // title can't create a folder.
            let filled = values.iter().fold(part.to_owned(), |acc, (key, value)| {
                acc.replace(key, &sanitize_value(value))
            });
            sanitize_component(&tidy(&filled))
        })
        .filter(|c| !c.is_empty())
        .collect();
    // Appended, not set with `with_extension`: that would eat "Mr. Blue" to "Mr.flac".
    match parts.last_mut() {
        Some(name) => name.push_str(".flac"),
        None => parts.push(format!("{track:02}.flac")),
    }
    parts.iter().collect()
}

/// Characters that are illegal (Windows/SMB) or troublesome (macOS `:`) in names.
fn sanitize_value(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}

/// Drops brackets left empty by missing values and collapses whitespace.
fn tidy(s: &str) -> String {
    let s = s.replace("()", "").replace("[]", "");
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Makes one path component safe on every filesystem a library might live on.
fn sanitize_component(name: &str) -> String {
    // Trailing dots and spaces are stripped by Windows/SMB; leading dots hide files.
    let mut name = name
        .trim_end_matches(['.', ' '])
        .trim_start_matches('.')
        .to_owned();
    if name.len() > MAX_COMPONENT_BYTES {
        let mut cut = MAX_COMPONENT_BYTES;
        while !name.is_char_boundary(cut) {
            cut -= 1;
        }
        name.truncate(cut);
        name = name.trim_end_matches(['.', ' ']).to_owned();
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        name.push('_');
    }
    name
}

/// Copies `src` to `dest` through a hidden `.part` file and an atomic rename,
/// retrying the copy so a flaky network share doesn't leave half a file.
pub fn install(src: &Path, dest: &Path) -> anyhow::Result<()> {
    install_with(
        src,
        dest,
        |from, to| std::fs::copy(from, to).map(drop),
        Duration::from_secs(1),
    )
}

fn install_with(
    src: &Path,
    dest: &Path,
    copy: impl Fn(&Path, &Path) -> std::io::Result<()>,
    backoff: Duration,
) -> anyhow::Result<()> {
    let dir = dest.parent().context("destination has no folder")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = dest
        .file_name()
        .context("destination has no file name")?
        .to_string_lossy();
    let part = dir.join(format!(".{name}.part"));

    let mut last_err = None;
    for attempt in 1..=COPY_ATTEMPTS {
        match copy(src, &part) {
            Ok(()) => {
                std::fs::rename(&part, dest)
                    .with_context(|| format!("moving into {}", dest.display()))?;
                return Ok(());
            }
            Err(e) => {
                last_err = Some(e);
                if attempt < COPY_ATTEMPTS {
                    std::thread::sleep(backoff * attempt);
                }
            }
        }
    }
    let _ = std::fs::remove_file(&part);
    Err(last_err.unwrap())
        .with_context(|| format!("copying to {} ({COPY_ATTEMPTS} attempts)", dest.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn tags(pairs: &[(&str, &str)]) -> Tags {
        let mut t = Tags::default();
        for (k, v) in pairs {
            t.add(k, *v);
        }
        t
    }

    fn path(pairs: &[(&str, &str)]) -> String {
        track_path(DEFAULT_TEMPLATE, &tags(pairs))
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn default_layout() {
        let p = path(&[
            ("ALBUMARTIST", "Various Artists"),
            ("ALBUM", "鋼の錬金術師 THE BEST"),
            ("DATE", "2012-02-29"),
            ("TRACKNUMBER", "1"),
            ("DISCNUMBER", "1"),
            ("DISCTOTAL", "2"),
            ("TITLE", "メリッサ"),
        ]);
        assert_eq!(
            p,
            "Various Artists/鋼の錬金術師 THE BEST (2012)/1-01 - メリッサ.flac"
        );
    }

    #[test]
    fn single_disc_and_missing_year() {
        let p = path(&[
            ("ALBUMARTIST", "A"),
            ("ALBUM", "B"),
            ("TRACKNUMBER", "7"),
            ("DISCNUMBER", "1"),
            ("DISCTOTAL", "1"),
            ("TITLE", "C"),
        ]);
        assert_eq!(p, "A/B/07 - C.flac");
    }

    #[test]
    fn untagged_disc_is_unique_per_disc() {
        let p = path(&[("TRACKNUMBER", "3"), ("MUSICBRAINZ_DISCID", "4DEXQ_x-")]);
        assert_eq!(
            p,
            "Unknown Artist/Unknown Album [4DEXQ_x-]/03 - Track 03.flac"
        );
    }

    #[test]
    fn values_cannot_create_folders_or_illegal_names() {
        let p = path(&[
            ("ALBUMARTIST", "AC/DC"),
            ("ALBUM", "...Who: Me?"),
            ("TRACKNUMBER", "1"),
            ("TITLE", "Con"),
        ]);
        assert_eq!(p, "AC_DC/Who_ Me_/01 - Con.flac");
        assert_eq!(sanitize_component("CON"), "CON_");
        assert_eq!(sanitize_component("com1.flac"), "com1.flac_");
        assert_eq!(sanitize_component("Song. "), "Song");
    }

    #[test]
    fn dots_in_names_survive_the_extension() {
        let p = path(&[
            ("ALBUMARTIST", "A"),
            ("ALBUM", "B"),
            ("TRACKNUMBER", "1"),
            ("TITLE", "Mr. Blue"),
        ]);
        assert_eq!(p, "A/B/01 - Mr. Blue.flac");
        let t = tags(&[("ARTIST", "X"), ("TRACKNUMBER", "2"), ("TITLE", "Y")]);
        let custom = track_path("{artist}/{track}. {artist} - {title}", &t);
        assert_eq!(custom.to_string_lossy(), "X/02. X - Y.flac");
    }

    #[test]
    fn long_names_are_cut_on_char_boundaries() {
        let long = "鋼".repeat(200); // 600 bytes
        let cut = sanitize_component(&long);
        assert!(cut.len() <= MAX_COMPONENT_BYTES && cut.len() > MAX_COMPONENT_BYTES - 3);
        assert!(cut.chars().all(|c| c == '鋼'));
    }

    #[test]
    fn install_retries_then_renames_atomically() {
        let dir = std::env::temp_dir().join(format!("cdr-install-test-{}", std::process::id()));
        let src = dir.join("src.flac");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&src, b"audio").unwrap();
        let dest = dir.join("lib/Artist/01 - Song.flac");

        let calls = Cell::new(0);
        let flaky = |from: &Path, to: &Path| {
            calls.set(calls.get() + 1);
            if calls.get() < 3 {
                return Err(std::io::Error::other("share went away"));
            }
            std::fs::copy(from, to).map(drop)
        };
        install_with(&src, &dest, flaky, Duration::ZERO).unwrap();
        assert_eq!(calls.get(), 3);
        assert_eq!(std::fs::read(&dest).unwrap(), b"audio");
        assert!(!dest.with_file_name(".01 - Song.flac.part").exists());

        let always = |_: &Path, to: &Path| {
            std::fs::write(to, b"half")?;
            Err(std::io::Error::other("nope"))
        };
        let dest2 = dir.join("lib/Artist/02 - Song.flac");
        assert!(install_with(&src, &dest2, always, Duration::ZERO).is_err());
        assert!(!dest2.exists());
        assert!(
            !dest2.with_file_name(".02 - Song.flac.part").exists(),
            "no partial file left"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
