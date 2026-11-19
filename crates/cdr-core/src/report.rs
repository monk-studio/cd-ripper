//! The record of a rip: a log for people (`rip.log`), the same facts as JSON
//! for tools (`rip.json`), and a one-line verdict for the whole disc.
//!
//! `rip.json` carries a `format` number. Fields are only ever added; a
//! change in meaning gets a new format number.

use md5::{Digest, Md5};
use serde::Serialize;

use crate::SECTOR_SAMPLES;
use crate::offsets::OffsetSource;
use crate::rip::TrackRip;

pub const FORMAT: u32 = 1;

#[derive(Debug, Clone, Serialize)]
pub struct RipReport {
    pub format: u32,
    pub tool: String,
    /// When the rip finished, RFC 3339 in UTC.
    pub date: String,
    pub drive: Option<String>,
    /// The drive's read offset, which the audio is aligned to.
    pub read_offset: i32,
    /// The offset came from this disc alone, the drive's being unknown.
    pub offset_detected: bool,
    /// Where the read offset came from.
    pub offset_source: OffsetSource,
    /// The offset this disc's pressing matched AccurateRip at, when it
    /// differs from the drive's.
    pub pressing_offset: Option<i32>,
    /// Every track read twice, not only those AccurateRip couldn't confirm.
    pub secure_mode: bool,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub musicbrainz_disc_id: String,
    pub musicbrainz_release_id: Option<String>,
    /// As in the database file name, e.g. `012-00121bef-00ac3084-a009cb0c`.
    pub accuraterip_disc_id: String,
    /// Pressings in AccurateRip; `None` if the disc isn't in it.
    pub accuraterip_submissions: Option<usize>,
    pub tracks: Vec<TrackReport>,
    /// The library servers told about the album once it was filed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub library_servers: Vec<ServerScan>,
}

/// What a library server said when asked to pick the album up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServerScan {
    /// "Jellyfin" or "Navidrome".
    pub server: String,
    /// The libraries it's scanning (empty when it doesn't say).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scanning: Option<Vec<String>>,
    /// Why it couldn't be asked; the files are in place regardless.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ServerScan {
    /// One line for people: "Jellyfin: scanning Music", or why not.
    pub fn describe(&self) -> String {
        match (&self.scanning, &self.error) {
            (_, Some(e)) => format!("{}: not rescanned ({e})", self.server),
            (Some(names), None) if !names.is_empty() => {
                format!("{}: scanning {}", self.server, names.join(", "))
            }
            _ => format!("{}: scanning", self.server),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TrackReport {
    pub number: u8,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub start_sector: u32,
    pub length_sectors: u32,
    /// Path of the file in the library, relative to the log.
    pub file: Option<String>,
    /// MD5 of the PCM, the same value FLAC stores in STREAMINFO.
    pub audio_md5: String,
    pub crc_v1: String,
    pub crc_v2: String,
    pub accuraterip: Option<ArMatch>,
    /// Full reads of the track (1, or 2 in secure mode).
    pub passes: u32,
    pub reread_sectors: u32,
    /// Track positions (m:ss.ff) whose reads never agreed.
    pub suspicious: Vec<String>,
    /// Whether the drive reported C2 errors; `None` if it can't.
    pub c2_errors: Option<bool>,
    /// Whether some sectors were re-read with the drive slowed down.
    pub slowed_down: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ArMatch {
    pub confidence: u32,
    pub version: &'static str,
    /// Set when the track matched another pressing, at this offset; the
    /// audio itself stays aligned to the drive's offset.
    pub pressing_offset: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Every track confirmed by AccurateRip.
    Perfect,
    /// The rest read identically twice; nothing suspicious.
    Verified,
    /// Some track has suspicious positions.
    Check,
}

impl TrackReport {
    pub fn new(number: u8, start_sector: u32, rip: &TrackRip) -> Self {
        Self {
            number,
            title: None,
            artist: None,
            start_sector,
            length_sectors: (rip.samples.len() / SECTOR_SAMPLES) as u32,
            file: None,
            audio_md5: audio_md5(&rip.samples),
            crc_v1: format!("{:08x}", rip.checksums.v1),
            crc_v2: format!("{:08x}", rip.checksums.v2),
            accuraterip: rip.accurate.map(|(confidence, version)| ArMatch {
                confidence,
                version,
                pressing_offset: rip.pressing_offset,
            }),
            passes: rip.passes,
            reread_sectors: rip.reread_sectors,
            suspicious: rip
                .suspicious
                .iter()
                .map(|&s| timestamp(s / SECTOR_SAMPLES))
                .collect(),
            c2_errors: rip.c2,
            slowed_down: rip.slowed_down,
        }
    }

    /// `disc_matched`: some track on the disc matched AccurateRip.
    fn result(&self, in_database: bool, disc_matched: bool) -> &'static str {
        match (self.accuraterip, self.suspicious.is_empty()) {
            (Some(m), _) if m.pressing_offset.is_some() => "accurate, other pressing",
            (Some(_), _) => "accurate",
            (None, false) => "SUSPICIOUS",
            (None, true) if disc_matched => "CHECK, other tracks match AccurateRip",
            (None, true) if in_database => "secure, not in AccurateRip's pressings",
            (None, true) => "secure",
        }
    }
}

impl RipReport {
    /// Some track matched AccurateRip: this disc's pressing is in it.
    pub fn disc_matched(&self) -> bool {
        self.tracks.iter().any(|t| t.accuraterip.is_some())
    }

    /// Tracks whose reads didn't agree in places.
    pub fn suspicious_tracks(&self) -> Vec<u8> {
        self.tracks
            .iter()
            .filter(|t| !t.suspicious.is_empty())
            .map(|t| t.number)
            .collect()
    }

    /// Tracks that read the same twice but match no pressing, on a disc whose
    /// other tracks do match AccurateRip. Pressings differ across a whole
    /// disc, not one track, so a lone mismatch is more likely a misread the
    /// drive repeats than a different pressing.
    pub fn unconfirmed_tracks(&self) -> Vec<u8> {
        if !self.disc_matched() {
            return Vec::new();
        }
        self.tracks
            .iter()
            .filter(|t| t.accuraterip.is_none() && t.suspicious.is_empty())
            .map(|t| t.number)
            .collect()
    }

    pub fn verdict(&self) -> (Verdict, String) {
        let total = self.tracks.len();
        let list = |tracks: &[u8]| {
            let numbers: Vec<String> = tracks.iter().map(u8::to_string).collect();
            let which = if tracks.len() == 1 { "track" } else { "tracks" };
            format!("{which} {}", numbers.join(", "))
        };
        let (suspicious, unconfirmed) = (self.suspicious_tracks(), self.unconfirmed_tracks());
        let mut problems = Vec::new();
        if !suspicious.is_empty() {
            problems.push(format!(
                "{}: reads didn't agree in places",
                list(&suspicious)
            ));
        }
        if !unconfirmed.is_empty() {
            problems.push(format!(
                "{}: AccurateRip knows this disc but not this audio",
                list(&unconfirmed)
            ));
        }
        if !problems.is_empty() {
            return (Verdict::Check, format!("Check {}", problems.join("; ")));
        }
        if self.disc_matched() {
            return (
                Verdict::Perfect,
                format!("Perfect: all {total} tracks match AccurateRip"),
            );
        }
        (
            Verdict::Verified,
            format!("Verified: all {total} tracks read the same twice"),
        )
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("report serialises")
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        let mut line = |s: String| {
            out.push_str(&s);
            out.push('\n');
        };
        let opt = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".into());
        line(format!("{} rip log", self.tool));
        line(String::new());
        line(format!("Date:          {}", self.date));
        line(format!("Drive:         {}", opt(&self.drive)));
        line(format!(
            "Read offset:   {:+} samples ({})",
            self.read_offset,
            self.offset_source.describe()
        ));
        if let Some(p) = self.pressing_offset {
            line(format!(
                "Pressing:      matches AccurateRip at {p:+} samples"
            ));
        }
        line(format!(
            "Mode:          {}",
            if self.secure_mode {
                "secure (every track read twice)"
            } else {
                "fast read checked by AccurateRip, secure re-read otherwise"
            }
        ));
        line(format!(
            "Release:       {} — {}",
            opt(&self.artist),
            opt(&self.album)
        ));
        line(format!(
            "MusicBrainz:   disc {}, release {}",
            self.musicbrainz_disc_id,
            opt(&self.musicbrainz_release_id)
        ));
        line(format!(
            "AccurateRip:   disc {}, {}",
            self.accuraterip_disc_id,
            match self.accuraterip_submissions {
                Some(n) => format!("{n} pressing(s) in the database"),
                None => "not in the database".into(),
            }
        ));
        for scan in &self.library_servers {
            line(format!("Library:       {}", scan.describe()));
        }
        line(String::new());
        line(
            "Track  Start     Length    CRC v1    CRC v2    AccurateRip         Reads      C2   Result"
                .into(),
        );
        let in_db = self.accuraterip_submissions.is_some();
        let disc_matched = self.disc_matched();
        for t in &self.tracks {
            let ar = t
                .accuraterip
                .map(|m| match m.pressing_offset {
                    Some(p) => format!("{} conf {} @{p:+}", m.version, m.confidence),
                    None => format!("{} conf {}", m.version, m.confidence),
                })
                .unwrap_or_else(|| "-".into());
            let reads = match (t.reread_sectors, t.slowed_down) {
                (0, _) => t.passes.to_string(),
                (n, false) => format!("{}+{n}s", t.passes),
                (n, true) => format!("{}+{n}s slow", t.passes),
            };
            let c2 = match t.c2_errors {
                Some(true) => "yes",
                Some(false) => "no",
                None => "-",
            };
            line(format!(
                "{:5}  {}  {}  {}  {}  {:18}  {:9}  {:3}  {}",
                t.number,
                timestamp(t.start_sector as usize),
                timestamp(t.length_sectors as usize),
                t.crc_v1,
                t.crc_v2,
                ar,
                reads,
                c2,
                t.result(in_db, disc_matched)
            ));
        }
        let suspicious: Vec<_> = self
            .tracks
            .iter()
            .filter(|t| !t.suspicious.is_empty())
            .collect();
        if !suspicious.is_empty() {
            line(String::new());
            for t in suspicious {
                line(format!(
                    "Track {} suspicious at {}",
                    t.number,
                    t.suspicious.join(", ")
                ));
            }
        }
        line(String::new());
        line("Files and audio MD5:".into());
        for t in &self.tracks {
            let name = t
                .file
                .clone()
                .or_else(|| t.title.clone())
                .unwrap_or_else(|| format!("track {}", t.number));
            line(format!("{:5}  {}  {}", t.number, t.audio_md5, name));
        }
        line(String::new());
        line(format!("Result: {}", self.verdict().1));
        out
    }
}

/// MD5 of 16-bit little-endian interleaved PCM: what FLAC records in STREAMINFO.
pub fn audio_md5(samples: &[u32]) -> String {
    let mut md5 = Md5::new();
    for s in samples {
        md5.update(s.to_le_bytes());
    }
    md5.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// A sector count as m:ss.ff (minutes, seconds, CD frames).
pub fn timestamp(sectors: usize) -> String {
    format!(
        "{:2}:{:02}.{:02}",
        sectors / 75 / 60,
        sectors / 75 % 60,
        sectors % 75
    )
}

/// RFC 3339 UTC time for `std::time::SystemTime`, without a date library.
pub fn rfc3339(time: std::time::SystemTime) -> String {
    let secs = time
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + (month <= 2) as i64;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accuraterip::Checksums;
    use std::time::{Duration, UNIX_EPOCH};

    fn rip(accurate: Option<(u32, &'static str)>, passes: u32, suspicious: Vec<usize>) -> TrackRip {
        TrackRip {
            samples: vec![0x0001_0002; 75 * SECTOR_SAMPLES],
            checksums: Checksums { v1: 0xab, v2: 0xcd },
            accurate,
            passes,
            reread_sectors: suspicious.len() as u32,
            suspicious,
            c2: Some(false),
            slowed_down: false,
            pressing_offset: None,
        }
    }

    fn report(rips: &[TrackRip]) -> RipReport {
        RipReport {
            format: FORMAT,
            tool: "CDR test".into(),
            date: rfc3339(UNIX_EPOCH),
            drive: Some("DRIVE".into()),
            read_offset: 6,
            offset_detected: true,
            offset_source: OffsetSource::ThisDisc,
            pressing_offset: None,
            secure_mode: false,
            artist: Some("羊文学".into()),
            album: Some("12 hugs".into()),
            musicbrainz_disc_id: "disc".into(),
            musicbrainz_release_id: None,
            accuraterip_disc_id: "001-x".into(),
            accuraterip_submissions: Some(2),
            tracks: rips
                .iter()
                .enumerate()
                .map(|(i, r)| TrackReport::new(i as u8 + 1, i as u32 * 75, r))
                .collect(),
            library_servers: Vec::new(),
        }
    }

    #[test]
    fn verdict_levels() {
        let perfect = report(&[
            rip(Some((40, "v2")), 1, vec![]),
            rip(Some((37, "v2")), 1, vec![]),
        ]);
        assert_eq!(
            perfect.verdict(),
            (
                Verdict::Perfect,
                "Perfect: all 2 tracks match AccurateRip".into()
            )
        );

        // A pressing AccurateRip doesn't have: no track matches, all read the same twice.
        let verified = report(&[rip(None, 2, vec![]), rip(None, 2, vec![])]);
        assert_eq!(
            verified.verdict(),
            (
                Verdict::Verified,
                "Verified: all 2 tracks read the same twice".into()
            )
        );

        // The disc's pressing is in AccurateRip, but track 2 matches none of it.
        let lone = report(&[rip(Some((40, "v2")), 1, vec![]), rip(None, 2, vec![])]);
        assert_eq!(
            lone.verdict(),
            (
                Verdict::Check,
                "Check track 2: AccurateRip knows this disc but not this audio".into()
            )
        );
        assert_eq!(lone.unconfirmed_tracks(), [2]);
        assert!(
            lone.to_text()
                .contains("CHECK, other tracks match AccurateRip")
        );

        let both = report(&[
            rip(Some((40, "v2")), 1, vec![]),
            rip(None, 2, vec![]),
            rip(None, 2, vec![588 * 75]),
        ]);
        assert_eq!(
            both.verdict().1,
            "Check track 3: reads didn't agree in places; track 2: AccurateRip knows this disc but not this audio"
        );

        let check = report(&[rip(None, 2, vec![588 * 75 * 113]), rip(None, 2, vec![])]);
        assert_eq!(check.verdict().0, Verdict::Check);
        assert_eq!(check.tracks[0].suspicious, [" 1:53.00"]);
        assert!(check.to_text().contains("Track 1 suspicious at  1:53.00"));
    }

    #[test]
    fn json_is_versioned_and_complete() {
        let r = report(&[rip(Some((40, "v2")), 1, vec![])]);
        let v: serde_json::Value = serde_json::from_str(&r.to_json()).unwrap();
        assert_eq!(v["format"], 1);
        assert_eq!(v["tracks"][0]["accuraterip"]["confidence"], 40);
        assert_eq!(v["tracks"][0]["crc_v2"], "000000cd");
        assert_eq!(v["artist"], "羊文学");
    }

    #[test]
    fn audio_md5_matches_flac_streaminfo() {
        // FLAC's MD5 covers the interleaved little-endian PCM bytes.
        let samples = [0x0002_0001u32, 0xffff_8000];
        let bytes = [0x01, 0x00, 0x02, 0x00, 0x00, 0x80, 0xff, 0xff];
        let expected: String = Md5::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(audio_md5(&samples), expected);
    }

    #[test]
    fn rfc3339_dates() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        // A Gmail message's internalDate and its own RFC 3339 date.
        assert_eq!(
            rfc3339(UNIX_EPOCH + Duration::from_secs(1_794_320_474)),
            "2026-11-10T14:21:14Z"
        );
        assert_eq!(
            rfc3339(UNIX_EPOCH + Duration::from_secs(951_782_400)),
            "2000-02-29T00:00:00Z"
        );
    }

    #[test]
    fn the_log_says_what_the_library_servers_did() {
        let mut r = report(&[]);
        assert!(
            !r.to_json().contains("library_servers"),
            "left out when none"
        );
        r.library_servers = vec![
            ServerScan {
                server: "Jellyfin".into(),
                scanning: Some(vec!["Music".into()]),
                error: None,
            },
            ServerScan {
                server: "Navidrome".into(),
                scanning: None,
                error: Some("connection refused".into()),
            },
        ];
        let text = r.to_text();
        assert!(
            text.contains("Library:       Jellyfin: scanning Music"),
            "{text}"
        );
        assert!(
            text.contains("Library:       Navidrome: not rescanned (connection refused)"),
            "{text}"
        );
        let json: serde_json::Value = serde_json::from_str(&r.to_json()).unwrap();
        assert_eq!(json["library_servers"][0]["scanning"][0], "Music");
        assert_eq!(json["library_servers"][1]["error"], "connection refused");
    }
}
