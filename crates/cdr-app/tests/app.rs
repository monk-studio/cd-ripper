//! The app, headless, with a fake disc and a fake rip.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use cdr_app::backend::{Backend, Disc, RipRequest, set_backend};
use cdr_core::album::Event;
use cdr_core::musicbrainz::{Release, TrackMeta};
use cdr_core::offsets::OffsetSource;
use cdr_core::report::{ArMatch, RipReport, TrackReport};
use cdr_core::storage_keys;
use cdr_core::toc::{Toc, Track};
use nsh_engine::{EngineConfig, Root};
use nsh_testing::{Headless, MemoryStorage};

/// The backend is global, so tests take turns.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn disc() -> Disc {
    let track = |number: u8, start| Track {
        number,
        session: 1,
        start,
        is_audio: true,
        pre_emphasis: false,
    };
    let meta = |number: u32, title: &str| TrackMeta {
        number,
        title: title.into(),
        artist: "羊文学".into(),
        artist_ids: vec![],
        track_id: format!("t{number}"),
        recording_id: format!("r{number}"),
        length_ms: None,
    };
    Disc {
        device: "/dev/fake".into(),
        toc: Toc {
            tracks: vec![track(1, 0), track(2, 4844), track(3, 26532)],
            leadout: 41625,
        },
        releases: vec![Release {
            id: "rel".into(),
            release_group_id: None,
            title: "12 hugs (like butterflies)".into(),
            artist: "羊文学".into(),
            artist_ids: vec![],
            date: Some("2023-12-06".into()),
            country: Some("JP".into()),
            label: Some("F.C.L.S.".into()),
            catalog_number: Some("KSCL-3482".into()),
            barcode: None,
            disc_number: 1,
            disc_total: 1,
            tracks: vec![
                meta(1, "Hug.m4a"),
                meta(2, "more than words"),
                meta(3, "Addiction"),
            ],
            exact: true,
        }],
    }
}

/// Rips instantly: every track accurate at confidence 40. Remembers what it
/// was asked to do.
#[derive(Default)]
struct Fake {
    ejected: AtomicBool,
    secure: AtomicBool,
    /// The system refuses access to the disc.
    denied: AtomicBool,
    /// What the system opens on insert.
    auto_open: Mutex<Option<String>>,
}

impl Backend for Fake {
    fn read_disc(&self) -> anyhow::Result<Disc> {
        if self.denied.load(Ordering::SeqCst) {
            let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
            return Err(anyhow::Error::new(denied).context("opening /dev/rdisk9"));
        }
        anyhow::ensure!(!self.ejected.load(Ordering::SeqCst), "no disc");
        Ok(disc())
    }

    fn app_opened_on_insert(&self) -> Option<String> {
        self.auto_open.lock().unwrap().clone()
    }

    fn eject(&self, _disc: &Disc) -> anyhow::Result<()> {
        self.ejected.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn rip(
        &self,
        disc: &Disc,
        request: RipRequest,
        on_event: &mut dyn FnMut(Event),
    ) -> anyhow::Result<RipReport> {
        self.secure.store(request.secure, Ordering::SeqCst);
        let mut tracks = Vec::new();
        for (n, t) in disc.toc.tracks.iter().enumerate() {
            on_event(Event::Progress {
                track: n,
                done: 50,
                total: 100,
            });
            let report = TrackReport {
                number: t.number,
                title: None,
                artist: None,
                start_sector: t.start,
                length_sectors: 0,
                file: request.dests.get(n).map(|d| d.display().to_string()),
                audio_md5: String::new(),
                crc_v1: String::new(),
                crc_v2: String::new(),
                accuraterip: Some(ArMatch {
                    confidence: 40,
                    version: "v2",
                    pressing_offset: None,
                }),
                passes: 1,
                reread_sectors: 0,
                suspicious: vec![],
                c2_errors: Some(false),
                slowed_down: false,
            };
            on_event(Event::TrackDone {
                track: n,
                report: report.clone(),
            });
            tracks.push(report);
        }
        Ok(RipReport {
            format: 1,
            tool: "fake".into(),
            date: String::new(),
            drive: None,
            read_offset: 6,
            offset_detected: false,
            offset_source: OffsetSource::AccurateRipDrives,
            pressing_offset: None,
            secure_mode: request.secure,
            artist: None,
            album: None,
            musicbrainz_disc_id: String::new(),
            musicbrainz_release_id: None,
            accuraterip_disc_id: String::new(),
            accuraterip_submissions: Some(1),
            tracks,
        })
    }
}

fn start(storage: &[(&str, &str)]) -> Headless {
    start_with(Arc::new(Fake::default()), storage)
}

fn start_with(fake: Arc<Fake>, storage: &[(&str, &str)]) -> Headless {
    set_backend(fake);
    let mut config = EngineConfig::new(Root::of::<cdr_app::App>());
    config.storage = Some(MemoryStorage::new(
        storage.iter().map(|(k, v)| (k.to_string(), v.to_string())),
    ));
    Headless::start(config).unwrap()
}

#[test]
fn shows_the_disc_and_needs_a_library_before_ripping() {
    let _turn = serial();
    let mut app = start(&[]);
    app.wait_for("text:12 hugs (like butterflies)").unwrap();
    assert!(app.shows("more than words"));
    assert!(app.shows("No library folder chosen"));
    assert_eq!(app.screen().enabled("text:Rip"), Ok(false));
}

#[test]
fn rips_into_the_library_and_shows_the_verdict() {
    let _turn = serial();
    let library = std::env::temp_dir().join(format!("cdr-app-test-{}", std::process::id()));
    let mut app = start(&[(storage_keys::LIBRARY, library.to_str().unwrap())]);
    app.wait_for("text:12 hugs (like butterflies)").unwrap();
    app.press("text:Rip").unwrap();
    app.wait_for("text:Perfect: all 3 tracks match AccurateRip")
        .unwrap();
    assert_eq!(
        app.screen()
            .texts()
            .iter()
            .filter(|t| **t == "✓ Accurate (40)")
            .count(),
        3
    );
    assert!(app.shows("Show in Finder"));
    assert!(app.shows("Eject"));
}

fn library() -> String {
    let dir = std::env::temp_dir().join(format!("cdr-app-test-{}", std::process::id()));
    dir.to_string_lossy().into_owned()
}

#[test]
fn secure_mode_setting_reaches_the_rip() {
    let _turn = serial();
    let fake = Arc::new(Fake::default());
    let lib = library();
    let mut app = start_with(fake.clone(), &[(storage_keys::LIBRARY, &lib)]);
    app.wait_for("text:12 hugs (like butterflies)").unwrap();
    app.press("text:Read every track twice").unwrap();
    app.press("text:Rip").unwrap();
    app.wait_for("text:Perfect: all 3 tracks match AccurateRip")
        .unwrap();
    assert!(fake.secure.load(Ordering::SeqCst));
}

#[test]
fn secure_mode_is_remembered() {
    let _turn = serial();
    let fake = Arc::new(Fake::default());
    let lib = library();
    let mut app = start_with(
        fake.clone(),
        &[(storage_keys::LIBRARY, &lib), (storage_keys::SECURE, "1")],
    );
    app.wait_for("text:12 hugs (like butterflies)").unwrap();
    app.press("text:Rip").unwrap();
    app.wait_for("text:Perfect: all 3 tracks match AccurateRip")
        .unwrap();
    assert!(
        fake.secure.load(Ordering::SeqCst),
        "the stored setting applies without touching it"
    );
}

#[test]
fn eject_after_ripping_waits_for_the_next_disc() {
    let _turn = serial();
    let fake = Arc::new(Fake::default());
    let lib = library();
    let mut app = start_with(fake.clone(), &[(storage_keys::LIBRARY, &lib)]);
    app.wait_for("text:12 hugs (like butterflies)").unwrap();
    app.press("text:Rip").unwrap();
    app.wait_for("text:Eject").unwrap();
    app.press("text:Eject").unwrap();
    app.wait_for("text:Insert an audio CD").unwrap();
    assert!(fake.ejected.load(Ordering::SeqCst));
    // The disc is gone: across a disc poll (every 2 s) the app stays put.
    app.wait(std::time::Duration::from_secs(3)).unwrap();
    assert!(!app.shows("12 hugs (like butterflies)"));
    assert!(app.shows("Insert an audio CD"));
}

#[test]
fn warns_while_music_opens_on_insert_until_fixed() {
    let _turn = serial();
    let fake = Arc::new(Fake::default());
    *fake.auto_open.lock().unwrap() = Some("the Music app".into());
    let mut app = start_with(fake.clone(), &[]);
    app.wait_for("text:/macOS opens the Music app when you insert a CD/")
        .unwrap();
    // The user changes the setting, then checks again.
    *fake.auto_open.lock().unwrap() = None;
    app.press("text:Check again").unwrap();
    app.wait_for_gone(
        "text:/macOS opens the Music app/",
        std::time::Duration::from_secs(5),
    )
    .unwrap();
}

#[test]
fn says_when_the_system_refuses_access_to_the_disc() {
    let _turn = serial();
    let fake = Arc::new(Fake::default());
    fake.denied.store(true, Ordering::SeqCst);
    let mut app = start_with(fake, &[]);
    app.wait_for("text:/CDR isn't allowed to read discs/")
        .unwrap();
    assert!(app.shows("Insert an audio CD"));
}
