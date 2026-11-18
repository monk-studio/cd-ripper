//! Ripping a whole disc into the library: the steps every front end shares,
//! reported as they happen through [`Event`]s.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

use crate::accuraterip::{self, DiscIds};
use crate::coverart::{self, Cover};
use crate::drive::CdDrive;
use crate::musicbrainz::{DiscToc, Release};
use crate::offsets::{DriveDb, DriveStore, OffsetSource};
use crate::report::{self, RipReport, TrackReport};
use crate::rip::Ripper;
use crate::tags::Tags;
use crate::toc::Toc;
use crate::{flac, library};

/// What to rip and where to put it.
pub struct Plan<'a> {
    pub toc: &'a Toc,
    pub release: Option<&'a Release>,
    /// Library-absolute path of each audio track; empty to rip without filing.
    pub dests: Vec<PathBuf>,
    pub tags: Vec<Tags>,
    /// The drive's read offset, set by hand. `None` to use the one remembered
    /// for the drive, AccurateRip's drive database, or this disc (in that order).
    pub offset: Option<i32>,
    /// Where drive offsets are remembered and AccurateRip's drive database
    /// is cached ([`crate::offsets::data_dir`]); `None` to keep nothing.
    pub data_dir: Option<PathBuf>,
    /// Read every track twice.
    pub secure: bool,
}

#[derive(Debug, Clone)]
pub enum Event {
    /// Pressings in AccurateRip (`None`: the disc isn't in it).
    AccurateRip(Option<usize>),
    /// The drive offset the rip uses, and the offset this disc's pressing
    /// matched AccurateRip at, when that differs.
    Offset {
        offset: i32,
        source: OffsetSource,
        pressing: Option<i32>,
    },
    Cover(Option<Cover>),
    /// Sectors read of audio track `track` (0-based).
    Progress {
        track: usize,
        done: u32,
        total: u32,
    },
    TrackDone {
        track: usize,
        report: TrackReport,
    },
}

/// Each audio track's tags and its path in the library under `root`.
pub fn destinations(
    root: Option<&Path>,
    template: &str,
    release: Option<&Release>,
    disc_id: &str,
    tracks: usize,
) -> (Vec<Tags>, Vec<PathBuf>) {
    let tags: Vec<Tags> = (0..tracks)
        .map(|n| Tags::for_track(release, disc_id, n, tracks))
        .collect();
    let dests = match root {
        Some(root) => tags
            .iter()
            .map(|t| root.join(library::track_path(template, t)))
            .collect(),
        None => Vec::new(),
    };
    (tags, dests)
}

/// Rips every audio track of the disc and files it, with cover art and the
/// rip log, into the library. Returns the report; files are only written
/// when `plan.dests` is non-empty.
pub fn rip(
    drive: &mut dyn CdDrive,
    plan: Plan,
    mut on_event: impl FnMut(Event),
) -> anyhow::Result<RipReport> {
    let toc = plan.toc;
    let ids = DiscIds::from_toc(toc);
    let disc_id = DiscToc::from_toc(toc).disc_id();
    let drive_model = drive.model();
    let ar_cache = plan.data_dir.as_ref().map(|d| d.join("accuraterip"));
    let db = accuraterip::fetch(&ids, ar_cache.as_deref()).context("querying AccurateRip")?;
    on_event(Event::AccurateRip(db.as_ref().map(Vec::len)));

    let mut ripper = Ripper::new(drive, toc, 0);
    ripper.always_secure = plan.secure;
    // Where this disc's pressing lines up with AccurateRip: evidence about the
    // drive, and the alignment its tracks may match at.
    let this_disc = db.as_deref().and_then(|subs| ripper.detect_offset(subs));
    let store_path = plan.data_dir.as_ref().map(|d| d.join("drives.json"));
    let mut store = store_path
        .as_deref()
        .map(DriveStore::load)
        .unwrap_or_default();
    let model = drive_model.as_deref();
    if let (Some(model), Some(o)) = (model, this_disc) {
        store.observe(model, o);
    }
    let (offset, source) = match plan.offset {
        Some(o) => (o, OffsetSource::Manual),
        None => {
            let known = model.and_then(|m| store.known(m));
            let from_db = || {
                let (m, dir) = (model?, plan.data_dir.as_ref()?);
                let o = DriveDb::load(&dir.join("DriveOffsets.bin"))
                    .ok()?
                    .lookup(m)?;
                Some((o, OffsetSource::AccurateRipDrives))
            };
            match known
                .or_else(from_db)
                .or(this_disc.map(|o| (o, OffsetSource::ThisDisc)))
            {
                Some(found) => found,
                None => bail!(
                    "the drive's read offset is unknown and the disc isn't in AccurateRip; set it by hand"
                ),
            }
        }
    };
    if let (Some(m), OffsetSource::AccurateRipDrives) = (model, source) {
        store.remember(m, offset, source);
    }
    if let Some(path) = &store_path {
        store.save(path)?;
    }
    ripper.offset = offset;
    ripper.pressing_offsets = this_disc.filter(|&o| o != offset).into_iter().collect();
    on_event(Event::Offset {
        offset,
        source,
        pressing: this_disc.filter(|&o| o != offset),
    });

    // Encode locally, then copy into the library: a network share only sees finished files.
    let staging = std::env::temp_dir().join(format!("cdr-{}", std::process::id()));
    std::fs::create_dir_all(&staging)?;
    let result = (|| {
        let cover = match (plan.dests.is_empty(), plan.release) {
            (false, Some(r)) => coverart::fetch(r).ok().flatten(),
            _ => None,
        };
        on_event(Event::Cover(cover.clone()));
        if let Some(c) = &cover {
            let name = if c.mime_type == "image/png" {
                "cover.png"
            } else {
                "cover.jpg"
            };
            let staged = staging.join(name);
            std::fs::write(&staged, &c.data)?;
            let mut folders: Vec<&Path> = plan.dests.iter().filter_map(|d| d.parent()).collect();
            folders.dedup();
            for folder in folders {
                library::install(&staged, &folder.join(name))?;
            }
        }

        let mut report = RipReport {
            format: report::FORMAT,
            tool: format!("CDR {}", env!("CARGO_PKG_VERSION")),
            date: String::new(),
            drive: drive_model,
            read_offset: ripper.offset,
            offset_detected: source == OffsetSource::ThisDisc,
            offset_source: source,
            pressing_offset: ripper.pressing_offsets.first().copied(),
            secure_mode: plan.secure,
            artist: plan.release.map(|r| r.artist.clone()),
            album: plan.release.map(|r| r.title.clone()),
            musicbrainz_disc_id: disc_id,
            musicbrainz_release_id: plan.release.map(|r| r.id.clone()),
            accuraterip_disc_id: ids.name(),
            accuraterip_submissions: db.as_ref().map(Vec::len),
            tracks: Vec::new(),
        };
        // The log sits with the first track; file paths in it are relative to it.
        let log_dir = plan
            .dests
            .first()
            .and_then(|d| d.parent())
            .map(Path::to_path_buf);
        let audio: Vec<_> = toc.audio_tracks().collect();
        for (n, track) in audio.iter().enumerate() {
            let rip = ripper.rip_track(n, db.as_deref(), |done, total| {
                on_event(Event::Progress {
                    track: n,
                    done,
                    total,
                })
            });
            let mut entry = TrackReport::new(track.number, track.start, &rip);
            if let Some(m) = plan.release.and_then(|r| r.tracks.get(n)) {
                entry.title = Some(m.title.clone());
                entry.artist = Some(m.artist.clone());
            }
            if let Some(dest) = plan.dests.get(n) {
                let staged = staging.join(format!("{:02}.flac", track.number));
                flac::write(&staged, &rip.samples, &plan.tags[n], cover.as_ref())?;
                library::install(&staged, dest)?;
                std::fs::remove_file(&staged)?;
                let rel = log_dir.as_deref().and_then(|l| dest.strip_prefix(l).ok());
                entry.file = Some(rel.unwrap_or(dest).to_string_lossy().into_owned());
            }
            on_event(Event::TrackDone {
                track: n,
                report: entry.clone(),
            });
            report.tracks.push(entry);
        }

        report.date = report::rfc3339(std::time::SystemTime::now());
        if let Some(dir) = &log_dir {
            for (name, body) in [
                ("rip.log", report.to_text()),
                ("rip.json", report.to_json()),
            ] {
                let staged = staging.join(name);
                std::fs::write(&staged, body)?;
                library::install(&staged, &dir.join(name))?;
            }
        }
        Ok(report)
    })();
    std::fs::remove_dir_all(&staging).ok();
    result
}
