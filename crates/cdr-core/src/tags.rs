//! Vorbis comment tags, named the way MusicBrainz Picard writes them so that
//! Navidrome, Jellyfin and other library servers group albums correctly.

use crate::musicbrainz::Release;

/// MusicBrainz's special "Various Artists" artist.
const VARIOUS_ARTISTS_ID: &str = "89ad4ac3-39f7-470e-963a-56509c546377";

/// Ordered, possibly multi-valued tags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tags(Vec<(String, Vec<String>)>);

impl Tags {
    pub fn add(&mut self, key: &str, value: impl Into<String>) {
        let value = value.into();
        if value.is_empty() {
            return;
        }
        match self.0.iter_mut().find(|(k, _)| k == key) {
            Some((_, values)) => values.push(value),
            None => self.0.push((key.to_owned(), vec![value])),
        }
    }

    pub fn fields(&self) -> &[(String, Vec<String>)] {
        &self.0
    }

    pub fn get(&self, key: &str) -> Option<&[String]> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_slice())
    }

    /// Tags for audio track `index` (0-based) of `track_total`, with or without a release.
    pub fn for_track(
        release: Option<&Release>,
        disc_id: &str,
        index: usize,
        track_total: usize,
    ) -> Self {
        let mut tags = Self::default();
        tags.add("TRACKNUMBER", (index + 1).to_string());
        tags.add("TRACKTOTAL", track_total.to_string());
        tags.add("TOTALTRACKS", track_total.to_string());
        tags.add("MUSICBRAINZ_DISCID", disc_id);
        let Some(r) = release else {
            return tags;
        };
        let opt = |v: &Option<String>| v.clone().unwrap_or_default();

        tags.add("ALBUM", &r.title);
        tags.add("ALBUMARTIST", &r.artist);
        tags.add("DISCNUMBER", r.disc_number.to_string());
        tags.add("DISCTOTAL", r.disc_total.to_string());
        tags.add("TOTALDISCS", r.disc_total.to_string());
        tags.add("DATE", opt(&r.date));
        tags.add("RELEASECOUNTRY", opt(&r.country));
        tags.add("LABEL", opt(&r.label));
        tags.add("CATALOGNUMBER", opt(&r.catalog_number));
        tags.add("BARCODE", opt(&r.barcode));
        tags.add("MUSICBRAINZ_ALBUMID", &r.id);
        tags.add("MUSICBRAINZ_RELEASEGROUPID", opt(&r.release_group_id));
        for id in &r.artist_ids {
            tags.add("MUSICBRAINZ_ALBUMARTISTID", id);
        }
        if r.artist_ids.iter().any(|id| id == VARIOUS_ARTISTS_ID) {
            tags.add("COMPILATION", "1");
        }

        if let Some(t) = r.tracks.get(index) {
            tags.add("TITLE", &t.title);
            tags.add("ARTIST", &t.artist);
            // Picard's naming: TRACKID is the recording, RELEASETRACKID the track on this release.
            tags.add("MUSICBRAINZ_TRACKID", &t.recording_id);
            tags.add("MUSICBRAINZ_RELEASETRACKID", &t.track_id);
            for id in &t.artist_ids {
                tags.add("MUSICBRAINZ_ARTISTID", id);
            }
        }
        tags
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::musicbrainz::TrackMeta;

    fn release() -> Release {
        Release {
            id: "rel".into(),
            release_group_id: Some("rg".into()),
            title: "鋼の錬金術師 THE BEST".into(),
            artist: "Various Artists".into(),
            artist_ids: vec![VARIOUS_ARTISTS_ID.into()],
            date: Some("2012-02-29".into()),
            country: Some("JP".into()),
            label: Some("Aniplex".into()),
            catalog_number: Some("SVWC-7818".into()),
            barcode: None,
            disc_number: 1,
            disc_total: 2,
            tracks: vec![TrackMeta {
                number: 1,
                title: "メリッサ".into(),
                artist: "ポルノグラフィティ".into(),
                artist_ids: vec!["porno".into()],
                track_id: "trk".into(),
                recording_id: "rec".into(),
                length_ms: None,
            }],
            exact: true,
        }
    }

    #[test]
    fn release_tags() {
        let tags = Tags::for_track(Some(&release()), "disc", 0, 11);
        let one = |k: &str| tags.get(k).map(|v| v.join(";"));
        assert_eq!(one("TITLE").as_deref(), Some("メリッサ"));
        assert_eq!(one("ARTIST").as_deref(), Some("ポルノグラフィティ"));
        assert_eq!(one("ALBUMARTIST").as_deref(), Some("Various Artists"));
        assert_eq!(one("TRACKNUMBER").as_deref(), Some("1"));
        assert_eq!(one("TRACKTOTAL").as_deref(), Some("11"));
        assert_eq!(one("DISCNUMBER").as_deref(), Some("1"));
        assert_eq!(one("DISCTOTAL").as_deref(), Some("2"));
        assert_eq!(one("MUSICBRAINZ_TRACKID").as_deref(), Some("rec"));
        assert_eq!(one("MUSICBRAINZ_RELEASETRACKID").as_deref(), Some("trk"));
        assert_eq!(one("COMPILATION").as_deref(), Some("1"));
        assert_eq!(one("BARCODE"), None, "empty values are skipped");
    }

    #[test]
    fn untagged_disc_keeps_numbering() {
        let tags = Tags::for_track(None, "disc", 4, 11);
        assert_eq!(tags.get("TRACKNUMBER"), Some(&["5".to_string()][..]));
        assert_eq!(tags.get("TITLE"), None);
    }
}
