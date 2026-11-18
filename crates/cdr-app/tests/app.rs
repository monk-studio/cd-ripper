//! The app, headless, with a fake disc and a fake rip.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use cdr_app::backend::{AutoOpen, Backend, Disc, RipRequest, Server, set_backend};
use cdr_core::album::Event;
use cdr_core::musicbrainz::{Release, TrackMeta};
use cdr_core::offsets::OffsetSource;
use cdr_core::report::{ArMatch, RipReport, TrackReport};
use cdr_core::storage_keys;
use cdr_core::toc::{Toc, Track};
use nsh_engine::EngineConfig;
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
    /// No optical drive is connected.
    no_drive: AtomicBool,
    /// What the system opens on insert.
    auto_open: Mutex<Option<AutoOpen>>,
    /// Replaces the disc's MusicBrainz releases.
    releases: Mutex<Option<Vec<Release>>>,
    /// What the last rip was asked to write.
    ripped: Mutex<Option<RipRequest>>,
    /// The Keychain's API key.
    api_key: Mutex<Option<String>>,
    /// The servers told to rescan.
    rescanned: Mutex<Vec<Server>>,
}

impl Backend for Fake {
    fn read_disc(&self) -> anyhow::Result<Disc> {
        if self.denied.load(Ordering::SeqCst) {
            let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
            return Err(anyhow::Error::new(denied).context("opening /dev/rdisk9"));
        }
        anyhow::ensure!(!self.no_drive.load(Ordering::SeqCst), "no drive");
        anyhow::ensure!(!self.ejected.load(Ordering::SeqCst), "no disc");
        let mut d = disc();
        if let Some(releases) = self.releases.lock().unwrap().clone() {
            d.releases = releases;
        }
        Ok(d)
    }

    fn drive(&self) -> Option<String> {
        (!self.no_drive.load(Ordering::SeqCst)).then(|| "HL-DT-ST BD-RE BP71N".into())
    }

    fn app_opened_on_insert(&self) -> Option<AutoOpen> {
        *self.auto_open.lock().unwrap()
    }

    fn api_key(&self) -> Option<String> {
        self.api_key.lock().unwrap().clone()
    }

    fn set_api_key(&self, key: Option<&str>) -> anyhow::Result<()> {
        *self.api_key.lock().unwrap() = key.map(str::to_owned);
        Ok(())
    }

    fn check_server(&self, server: &Server) -> anyhow::Result<Vec<String>> {
        anyhow::ensure!(
            server.api_key == "good-key",
            "Jellyfin rejected the API key"
        );
        Ok(vec!["Music".into()])
    }

    fn rescan(&self, server: &Server) -> anyhow::Result<Vec<String>> {
        self.rescanned.lock().unwrap().push(server.clone());
        self.check_server(server)
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
        let dests = request.dests.clone();
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
                file: dests.get(n).map(|d| d.display().to_string()),
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
            secure_mode: {
                let secure = request.secure;
                *self.ripped.lock().unwrap() = Some(request);
                secure
            },
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
    start_in(fake, storage, None)
}

fn start_in(fake: Arc<Fake>, storage: &[(&str, &str)], locale: Option<&str>) -> Headless {
    set_backend(fake);
    let mut config = EngineConfig::new(cdr_app::root());
    // The Mac window's size.
    config.viewport = nsh_core::geometry::Size::new(
        nsh_core::geometry::Points(860.0),
        nsh_core::geometry::Points(640.0),
    );
    // On a Mac: the app has its menu bar and a settings window.
    config.environment.os = nsh_core::platform::Os::MacOs;
    // A user whose system language is `locale`: it leads their preferred languages.
    if let Some(tag) = locale {
        config.environment.locale = nsh_core::platform::Locale::parse(tag).unwrap();
        config.environment.locale_settings.languages =
            nsh_core::platform::Languages::parse([tag]).unwrap();
    }
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
    assert_eq!(app.screen().enabled("text:Rip CD"), Ok(false));
}

#[test]
fn rips_into_the_library_and_shows_the_verdict() {
    let _turn = serial();
    let library = std::env::temp_dir().join(format!("cdr-app-test-{}", std::process::id()));
    let mut app = start(&[(storage_keys::LIBRARY, library.to_str().unwrap())]);
    app.wait_for("text:12 hugs (like butterflies)").unwrap();
    app.press("text:Rip CD").unwrap();
    app.wait_for("text:Perfect rip").unwrap();
    assert_eq!(
        app.screen()
            .texts()
            .iter()
            .filter(|t| **t == "Accurate (40)")
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
    open_settings(&mut app);
    app.press("text:Ripping").unwrap();
    app.press("text:Read every track twice").unwrap();
    app.press_window_close("settings").unwrap();
    app.press("text:Rip CD").unwrap();
    app.wait_for("text:Perfect rip").unwrap();
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
    app.press("text:Rip CD").unwrap();
    app.wait_for("text:Perfect rip").unwrap();
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
    app.press("text:Rip CD").unwrap();
    app.wait_for("text:Perfect rip").unwrap();
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
    *fake.auto_open.lock().unwrap() = Some(AutoOpen::Music);
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

#[test]
fn speaks_chinese_when_the_system_does() {
    let _turn = serial();
    let lib = library();
    let mut app = start_in(
        Arc::new(Fake::default()),
        &[(storage_keys::LIBRARY, &lib)],
        Some("zh-CN"),
    );
    app.wait_for("text:12 hugs (like butterflies)").unwrap();
    open_settings(&mut app);
    app.wait_for("text:曲库文件夹").unwrap();
    app.press_window_close("settings").unwrap();
    app.press("text:开始抓取").unwrap();
    app.wait_for("text:完美抓取").unwrap();
    assert!(app.shows("准确（40）"));
    assert!(app.shows("弹出"));
}

fn tag(request: &RipRequest, track: usize, key: &str) -> Option<String> {
    request.tags[track]
        .get(key)
        .and_then(|v| v.first())
        .cloned()
}

fn taiwan_release() -> Release {
    let mut r = disc().releases[0].clone();
    r.id = "rel-tw".into();
    r.country = Some("TW".into());
    r.label = Some("Universal".into());
    r.catalog_number = Some("X-1".into());
    r
}

#[test]
fn several_releases_can_be_chosen_between() {
    let _turn = serial();
    let fake = Arc::new(Fake::default());
    *fake.releases.lock().unwrap() = Some(vec![disc().releases[0].clone(), taiwan_release()]);
    let mut app = start_with(fake, &[]);
    // The header shows the first release; the picker lists both.
    app.wait_for("text:Release 1 of 2").unwrap();
    let jp = "text:2023-12-06 · JP · F.C.L.S. · KSCL-3482";
    let tw = "text:2023-12-06 · TW · Universal · X-1";
    assert_eq!(app.screen().count(tw), Ok(0));
    app.press("text:Release 1 of 2").unwrap();
    app.wait_for(tw).unwrap();
    assert_eq!(app.screen().count(jp), Ok(2));
    app.press(tw).unwrap();
    app.wait_for("text:Release 2 of 2").unwrap();
    // The picker closes and the header follows the choice.
    assert_eq!(app.screen().count(tw), Ok(1));
    assert_eq!(app.screen().count(jp), Ok(0));
}

#[test]
fn unknown_disc_gets_tags_typed_in() {
    let _turn = serial();
    let fake = Arc::new(Fake::default());
    *fake.releases.lock().unwrap() = Some(vec![]);
    let lib = library();
    let mut app = start_with(fake.clone(), &[(storage_keys::LIBRARY, &lib)]);
    app.wait_for("text:Unknown album").unwrap();
    app.press("text:Add Tags").unwrap();
    assert_eq!(
        app.screen().enabled("text:Done"),
        Ok(false),
        "album and artist come first"
    );
    app.type_text("id:album-title", "Live Tape").unwrap();
    app.type_text("id:album-artist", "Someone").unwrap();
    app.type_text("id:album-year", "2024").unwrap();
    app.type_text("id:track-title-1", "Intro").unwrap();
    app.press("text:Done").unwrap();
    app.wait_for("text:Live Tape").unwrap();
    assert!(app.shows("Intro"));
    app.press("text:Rip CD").unwrap();
    app.wait_for("text:Perfect rip").unwrap();

    let ripped = fake.ripped.lock().unwrap();
    let request = ripped.as_ref().unwrap();
    assert_eq!(tag(request, 0, "ALBUM").as_deref(), Some("Live Tape"));
    assert_eq!(tag(request, 0, "ALBUMARTIST").as_deref(), Some("Someone"));
    assert_eq!(tag(request, 0, "ARTIST").as_deref(), Some("Someone"));
    assert_eq!(tag(request, 0, "TITLE").as_deref(), Some("Intro"));
    assert_eq!(tag(request, 0, "DATE").as_deref(), Some("2024"));
    assert_eq!(tag(request, 1, "TITLE"), None, "untyped titles stay empty");
    let path = request.dests[0].to_string_lossy();
    assert!(
        path.ends_with("Someone/Live Tape (2024)/01 - Intro.flac"),
        "{path}"
    );
}

#[test]
fn edits_to_a_known_release_keep_its_musicbrainz_ids() {
    let _turn = serial();
    let fake = Arc::new(Fake::default());
    let lib = library();
    let mut app = start_with(fake.clone(), &[(storage_keys::LIBRARY, &lib)]);
    app.wait_for("text:12 hugs (like butterflies)").unwrap();
    app.press("text:Edit Tags").unwrap();
    app.clear("id:track-title-2").unwrap();
    app.type_text("id:track-title-2", "more than words (live)")
        .unwrap();
    app.press("text:Done").unwrap();
    app.wait_for("text:more than words (live)").unwrap();
    app.press("text:Rip CD").unwrap();
    app.wait_for("text:Perfect rip").unwrap();

    let ripped = fake.ripped.lock().unwrap();
    let request = ripped.as_ref().unwrap();
    assert_eq!(
        tag(request, 1, "TITLE").as_deref(),
        Some("more than words (live)")
    );
    assert_eq!(
        tag(request, 1, "MUSICBRAINZ_ALBUMID").as_deref(),
        Some("rel")
    );
    assert_eq!(
        tag(request, 0, "TITLE").as_deref(),
        Some("Hug.m4a"),
        "other tracks untouched"
    );
}

fn open_settings(app: &mut Headless) {
    app.press_key("mod+,").unwrap();
    app.wait_for("id:template").unwrap();
}

#[test]
fn the_library_can_be_chosen_from_the_footer_until_there_is_one() {
    let _turn = serial();
    let mut app = start(&[]);
    app.wait_for("text:12 hugs (like butterflies)").unwrap();
    assert!(app.shows("Choose Library…"));
    let lib = library();
    let mut app = start(&[(storage_keys::LIBRARY, &lib)]);
    app.wait_for("text:12 hugs (like butterflies)").unwrap();
    assert!(!app.shows("Choose Library…"), "it lives in Settings then");
}

#[test]
fn the_file_name_template_shapes_the_library_path() {
    let _turn = serial();
    let fake = Arc::new(Fake::default());
    let lib = library();
    let mut app = start_with(fake.clone(), &[(storage_keys::LIBRARY, &lib)]);
    app.wait_for("text:12 hugs (like butterflies)").unwrap();
    open_settings(&mut app);
    app.type_text("id:template", "{album}/{track} {title}")
        .unwrap();
    app.wait_for("text:/For example: 12 hugs \\(like butterflies\\)\\/01 Hug\\.m4a\\.flac/")
        .unwrap();
    app.press_window_close("settings").unwrap();
    app.press("text:Rip CD").unwrap();
    app.wait_for("text:Perfect rip").unwrap();
    let ripped = fake.ripped.lock().unwrap();
    let path = ripped.as_ref().unwrap().dests[1]
        .to_string_lossy()
        .into_owned();
    assert!(
        path.ends_with("12 hugs (like butterflies)/02 more than words.flac"),
        "{path}"
    );
}

#[test]
fn a_read_offset_set_by_hand_reaches_the_rip() {
    let _turn = serial();
    let fake = Arc::new(Fake::default());
    let lib = library();
    let mut app = start_with(fake.clone(), &[(storage_keys::LIBRARY, &lib)]);
    app.wait_for("text:12 hugs (like butterflies)").unwrap();
    open_settings(&mut app);
    app.press("text:Ripping").unwrap();
    app.type_text("id:offset", "six").unwrap();
    app.wait_for("text:/Type a whole number of samples/")
        .unwrap();
    app.clear("id:offset").unwrap();
    app.type_text("id:offset", "+6").unwrap();
    app.press_window_close("settings").unwrap();
    app.press("text:Rip CD").unwrap();
    app.wait_for("text:Perfect rip").unwrap();
    assert_eq!(
        fake.ripped.lock().unwrap().as_ref().unwrap().offset,
        Some(6)
    );
}

#[test]
fn offset_is_automatic_unless_set() {
    let _turn = serial();
    let fake = Arc::new(Fake::default());
    let lib = library();
    let mut app = start_with(fake.clone(), &[(storage_keys::LIBRARY, &lib)]);
    app.wait_for("text:12 hugs (like butterflies)").unwrap();
    app.press("text:Rip CD").unwrap();
    app.wait_for("text:Perfect rip").unwrap();
    assert_eq!(fake.ripped.lock().unwrap().as_ref().unwrap().offset, None);
}

#[test]
fn jellyfin_is_checked_and_rescans_after_the_rip() {
    let _turn = serial();
    let fake = Arc::new(Fake::default());
    let lib = library();
    let mut app = start_with(fake.clone(), &[(storage_keys::LIBRARY, &lib)]);
    app.wait_for("text:12 hugs (like butterflies)").unwrap();
    open_settings(&mut app);
    app.press("text:Servers").unwrap();
    app.type_text("id:jellyfin-url", "http://nas.local:8096")
        .unwrap();
    app.type_text("id:jellyfin-key", "bad-key").unwrap();
    app.press("text:Check Connection").unwrap();
    app.wait_for("text:/rejected the API key/").unwrap();
    app.clear("id:jellyfin-key").unwrap();
    app.type_text("id:jellyfin-key", "good-key").unwrap();
    app.press("text:Check Connection").unwrap();
    app.wait_for("text:Connected · Music libraries: Music")
        .unwrap();
    assert_eq!(
        fake.api_key.lock().unwrap().as_deref(),
        Some("good-key"),
        "the key goes to the Keychain"
    );
    app.press_window_close("settings").unwrap();
    app.press("text:Rip CD").unwrap();
    app.wait_for("text:/Jellyfin is scanning Music/").unwrap();
    assert_eq!(
        *fake.rescanned.lock().unwrap(),
        vec![Server {
            url: "http://nas.local:8096".into(),
            api_key: "good-key".into()
        }]
    );
}

#[test]
fn no_rescan_without_a_server() {
    let _turn = serial();
    let fake = Arc::new(Fake::default());
    let lib = library();
    let mut app = start_with(fake.clone(), &[(storage_keys::LIBRARY, &lib)]);
    app.wait_for("text:12 hugs (like butterflies)").unwrap();
    app.press("text:Rip CD").unwrap();
    app.wait_for("text:Perfect rip").unwrap();
    assert!(fake.rescanned.lock().unwrap().is_empty());
}

#[test]
fn recommends_a_drive_until_one_is_plugged_in() {
    let _turn = serial();
    let fake = Arc::new(Fake::default());
    fake.no_drive.store(true, Ordering::SeqCst);
    let mut app = start_with(fake.clone(), &[]);
    app.wait_for("text:Connect a CD drive").unwrap();
    assert!(app.shows("Tested with CDR: LG BP71N"));
    fake.no_drive.store(false, Ordering::SeqCst);
    app.wait_for("text:12 hugs (like butterflies)").unwrap();
}

#[test]
fn a_drive_without_a_disc_asks_for_one() {
    let _turn = serial();
    let fake = Arc::new(Fake::default());
    fake.ejected.store(true, Ordering::SeqCst);
    let mut app = start_with(fake, &[]);
    app.wait_for("text:Insert an audio CD").unwrap();
    app.wait(std::time::Duration::from_secs(3)).unwrap();
    assert!(!app.shows("Connect a CD drive"));
}
