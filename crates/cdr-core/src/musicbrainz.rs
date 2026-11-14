//! MusicBrainz disc IDs and release lookup.
//!
//! See https://musicbrainz.org/doc/Disc_ID_Calculation and
//! https://musicbrainz.org/doc/MusicBrainz_API. The API asks for a
//! descriptive User-Agent and at most one request per second; a rip needs one.

use base64::Engine;
use base64::alphabet::Alphabet;
use base64::engine::{GeneralPurpose, general_purpose::PAD};
use serde::Deserialize;
use sha1::{Digest, Sha1};

use crate::USER_AGENT;
use crate::toc::Toc;

const API: &str = "https://musicbrainz.org/ws/2";
/// Sector addresses in disc IDs count the 2s (150 sector) lead-in.
const LEAD_IN: u32 = 150;
/// MusicBrainz answers 503 when a client exceeds its rate limit or the
/// service is busy; it asks clients to back off and try again.
const ATTEMPTS: u32 = 4;

/// The audio session of a disc, as MusicBrainz sees it: audio tracks only,
/// ending before the data session on an enhanced CD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscToc {
    pub first: u8,
    pub last: u8,
    /// Lead-out address, including the lead-in.
    pub leadout: u32,
    /// Track start addresses, including the lead-in.
    pub offsets: Vec<u32>,
}

impl DiscToc {
    pub fn from_toc(toc: &Toc) -> Self {
        let audio: Vec<_> = toc.audio_tracks().collect();
        Self {
            first: audio[0].number,
            last: audio[audio.len() - 1].number,
            leadout: toc.audio_end() + LEAD_IN,
            offsets: audio.iter().map(|t| t.start + LEAD_IN).collect(),
        }
    }

    /// The 28-character MusicBrainz disc ID.
    pub fn disc_id(&self) -> String {
        let mut text = format!("{:02X}{:02X}{:08X}", self.first, self.last, self.leadout);
        for i in 0..99 {
            text += &format!("{:08X}", self.offsets.get(i).copied().unwrap_or(0));
        }
        let alphabet =
            Alphabet::new("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._")
                .expect("valid alphabet");
        let engine = GeneralPurpose::new(&alphabet, PAD);
        engine
            .encode(Sha1::digest(text.as_bytes()))
            .replace('=', "-")
    }

    /// The `toc` query parameter for fuzzy lookups: first, last, lead-out, offsets.
    pub fn toc_param(&self) -> String {
        let mut parts = vec![self.first as u32, self.last as u32, self.leadout];
        parts.extend(&self.offsets);
        parts
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join("+")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub id: String,
    pub release_group_id: Option<String>,
    pub title: String,
    pub artist: String,
    pub artist_ids: Vec<String>,
    pub date: Option<String>,
    pub country: Option<String>,
    pub label: Option<String>,
    pub catalog_number: Option<String>,
    pub barcode: Option<String>,
    /// Position of this disc in the release, and the release's disc count.
    pub disc_number: u32,
    pub disc_total: u32,
    pub tracks: Vec<TrackMeta>,
    /// True when MusicBrainz knows this exact disc ID; false for a TOC match.
    pub exact: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackMeta {
    pub number: u32,
    pub title: String,
    pub artist: String,
    pub artist_ids: Vec<String>,
    pub track_id: String,
    pub recording_id: String,
    pub length_ms: Option<u64>,
}

/// Looks up a disc by ID, falling back to a fuzzy TOC match. Releases come
/// back in MusicBrainz order; an empty list means nothing matched.
pub fn lookup(disc: &DiscToc) -> anyhow::Result<Vec<Release>> {
    let id = disc.disc_id();
    let url = format!(
        "{API}/discid/{id}?toc={}&inc=artist-credits+recordings+labels+release-groups&fmt=json",
        disc.toc_param()
    );
    let mut attempt = 1;
    let resp = loop {
        match ureq::get(&url).set("User-Agent", USER_AGENT).call() {
            Ok(resp) => break resp,
            Err(ureq::Error::Status(404, _)) => return Ok(Vec::new()),
            Err(ureq::Error::Status(503, _)) if attempt < ATTEMPTS => {
                std::thread::sleep(std::time::Duration::from_secs(2u64.pow(attempt)));
                attempt += 1;
            }
            Err(e) => return Err(e.into()),
        }
    };
    let body: api::DiscResponse = serde_json::from_reader(resp.into_reader())?;
    Ok(releases(body, &id, disc.offsets.len()))
}

fn releases(body: api::DiscResponse, disc_id: &str, track_count: usize) -> Vec<Release> {
    let exact = body.id.as_deref() == Some(disc_id);
    body.releases
        .into_iter()
        .filter_map(|r| {
            let disc_total = r.media.len() as u32;
            // The medium carrying this disc ID, or for a TOC match the first one with as many tracks.
            let medium = r
                .media
                .iter()
                .find(|m| m.discs.iter().any(|d| d.id == disc_id))
                .or_else(|| r.media.iter().find(|m| m.tracks.len() == track_count))?;
            let (artist, artist_ids) = credit(&r.artist_credit);
            let label_info = r.label_info.first();
            Some(Release {
                release_group_id: r.release_group.map(|g| g.id),
                title: r.title,
                artist: artist.clone(),
                artist_ids: artist_ids.clone(),
                date: r.date.filter(|d| !d.is_empty()),
                country: r.country,
                label: label_info
                    .and_then(|l| l.label.as_ref())
                    .map(|l| l.name.clone()),
                catalog_number: label_info.and_then(|l| l.catalog_number.clone()),
                barcode: r.barcode.filter(|b| !b.is_empty()),
                disc_number: medium.position,
                disc_total,
                tracks: medium
                    .tracks
                    .iter()
                    .map(|t| {
                        let (track_artist, track_artist_ids) = if t.artist_credit.is_empty() {
                            (artist.clone(), artist_ids.clone())
                        } else {
                            credit(&t.artist_credit)
                        };
                        TrackMeta {
                            number: t.position,
                            title: t.title.clone(),
                            artist: track_artist,
                            artist_ids: track_artist_ids,
                            track_id: t.id.clone(),
                            recording_id: t.recording.id.clone(),
                            length_ms: t.length,
                        }
                    })
                    .collect(),
                id: r.id,
                exact,
            })
        })
        .collect()
}

/// Joins an artist credit into its display name, plus the credited artist IDs.
fn credit(credits: &[api::ArtistCredit]) -> (String, Vec<String>) {
    let name = credits
        .iter()
        .map(|c| format!("{}{}", c.name, c.joinphrase))
        .collect();
    let ids = credits.iter().map(|c| c.artist.id.clone()).collect();
    (name, ids)
}

/// The subset of the MusicBrainz JSON we read.
mod api {
    use super::Deserialize;

    #[derive(Deserialize)]
    pub struct DiscResponse {
        pub id: Option<String>,
        #[serde(default)]
        pub releases: Vec<Release>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub struct Release {
        pub id: String,
        pub title: String,
        pub date: Option<String>,
        pub country: Option<String>,
        pub barcode: Option<String>,
        #[serde(default)]
        pub artist_credit: Vec<ArtistCredit>,
        #[serde(default)]
        pub label_info: Vec<LabelInfo>,
        pub release_group: Option<ReleaseGroup>,
        #[serde(default)]
        pub media: Vec<Medium>,
    }

    #[derive(Deserialize)]
    pub struct ArtistCredit {
        pub name: String,
        #[serde(default)]
        pub joinphrase: String,
        pub artist: Artist,
    }

    #[derive(Deserialize)]
    pub struct Artist {
        pub id: String,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub struct LabelInfo {
        pub catalog_number: Option<String>,
        pub label: Option<Label>,
    }

    #[derive(Deserialize)]
    pub struct Label {
        pub name: String,
    }

    #[derive(Deserialize)]
    pub struct ReleaseGroup {
        pub id: String,
    }

    #[derive(Deserialize)]
    pub struct Medium {
        pub position: u32,
        #[serde(default)]
        pub discs: Vec<Disc>,
        #[serde(default)]
        pub tracks: Vec<Track>,
    }

    #[derive(Deserialize)]
    pub struct Disc {
        pub id: String,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub struct Track {
        pub id: String,
        pub position: u32,
        pub title: String,
        pub length: Option<u64>,
        #[serde(default)]
        pub artist_credit: Vec<ArtistCredit>,
        pub recording: Recording,
    }

    #[derive(Deserialize)]
    pub struct Recording {
        pub id: String,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toc::{SESSION_GAP_SECTORS, Track};

    fn track(number: u8, session: u8, start: u32, is_audio: bool) -> Track {
        Track {
            number,
            session,
            start,
            is_audio,
            pre_emphasis: false,
        }
    }

    /// The disc ripped for CDR-1, which MusicBrainz knows by this ID.
    fn cdr1_disc() -> DiscToc {
        let starts = [
            0, 20640, 39837, 56938, 78633, 96783, 117095, 134180, 153349, 173212, 194737,
        ];
        let toc = Toc {
            tracks: starts
                .iter()
                .enumerate()
                .map(|(i, &s)| track(i as u8 + 1, 1, s, true))
                .collect(),
            leadout: 215393,
        };
        DiscToc::from_toc(&toc)
    }

    #[test]
    fn toc_param_includes_lead_in() {
        let disc = cdr1_disc();
        assert!(
            disc.toc_param().starts_with("1+11+215543+150+20790+"),
            "{}",
            disc.toc_param()
        );
    }

    #[test]
    fn disc_id_matches_musicbrainz() {
        // Confirmed against musicbrainz.org: release 47da3ce1-4e09-40c2-a06b-4cf31aa82a0c, disc 1.
        assert_eq!(cdr1_disc().disc_id(), "4DEXQtDeHzTzUxGYjGbjYHEJ7ho-");
    }

    #[test]
    fn enhanced_cd_uses_audio_session_only() {
        let toc = Toc {
            tracks: vec![
                track(1, 1, 0, true),
                track(2, 1, 10000, true),
                track(3, 2, 40000, false),
            ],
            leadout: 60000,
        };
        let disc = DiscToc::from_toc(&toc);
        assert_eq!((disc.first, disc.last), (1, 2));
        assert_eq!(disc.leadout, 40000 - SESSION_GAP_SECTORS + LEAD_IN);
        assert_eq!(disc.offsets, vec![150, 10150]);
    }

    #[test]
    fn picks_medium_by_disc_id_and_falls_back_to_artist() {
        let json = r#"{
            "id": "abc",
            "releases": [{
                "id": "rel", "title": "Best", "date": "2009", "country": "JP", "barcode": "",
                "artist-credit": [{"name": "A", "joinphrase": " & ", "artist": {"id": "a1"}},
                                  {"name": "B", "joinphrase": "", "artist": {"id": "b1"}}],
                "label-info": [{"catalog-number": "CAT-1", "label": {"name": "Lbl"}}],
                "release-group": {"id": "rg"},
                "media": [
                    {"position": 1, "discs": [{"id": "other"}], "tracks": []},
                    {"position": 2, "discs": [{"id": "abc"}], "tracks": [
                        {"id": "t1", "position": 1, "title": "One", "length": 1000,
                         "recording": {"id": "r1"}},
                        {"id": "t2", "position": 2, "title": "Two", "length": null,
                         "artist-credit": [{"name": "C", "artist": {"id": "c1"}}],
                         "recording": {"id": "r2"}}
                    ]}
                ]
            }]
        }"#;
        let rels = releases(serde_json::from_str(json).unwrap(), "abc", 2);
        assert_eq!(rels.len(), 1);
        let r = &rels[0];
        assert!(r.exact);
        assert_eq!((r.disc_number, r.disc_total), (2, 2));
        assert_eq!(r.artist, "A & B");
        assert_eq!(r.barcode, None);
        assert_eq!(r.catalog_number.as_deref(), Some("CAT-1"));
        assert_eq!(r.tracks[0].artist, "A & B");
        assert_eq!(r.tracks[1].artist, "C");
        assert_eq!(r.tracks[1].artist_ids, vec!["c1"]);
    }
}
