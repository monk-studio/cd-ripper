//! CDR's desktop app: insert a CD, pick the release and the library folder,
//! rip. The work runs on a blocking thread and reports back to the UI through
//! a channel; signals only change on the UI thread. Everything outside the
//! app goes through [`backend::Backend`], so tests can run it headless.

pub mod backend;
mod look;
mod settings;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use backend::{AutoOpen, Disc, RipRequest, backend, is_access_denied};
use cdr_core::album::{self, Event};
use cdr_core::musicbrainz::{DiscToc, Release, TrackMeta};
use cdr_core::offsets::OffsetSource;
use cdr_core::report::{RipReport, TrackReport, Verdict};
use cdr_core::storage_keys;
use look::{color, icon, icon_with, icons};
use nsh_base::{button, progress};
use nsh_core::desktop::MenuRole;
use nsh_core::element::desktop::{MenuItem, set_application_menu};
use nsh_core::element::input;
use nsh_core::file_dialog::OpenDialog;
use nsh_core::prelude::*;
use nsh_core::storage::Storage;
use settings::{Settings, settings_window};

/// The translations in `locales/`. `nsh run` and `nsh package` compile them
/// in themselves; [`root`] does it for `cargo run` and the tests.
const LOCALES: &str = include_str!("../locales/app.json");

/// The app's root with its translations.
pub fn root() -> nsh_core::element::Root {
    nsh_core::element::Root::of::<App>().translations_json(&by_language(LOCALES))
}

/// rust-i18n's one-file layout (`{"key": {"en": …}}`) as the engine takes
/// it (`{"en": {"key": …}}`).
fn by_language(json: &str) -> String {
    let keys: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(json).expect("locales/app.json");
    let mut out: std::collections::BTreeMap<String, serde_json::Map<String, serde_json::Value>> =
        Default::default();
    for (key, texts) in keys.iter().filter(|(k, _)| *k != "_version") {
        for (lang, text) in texts.as_object().into_iter().flatten() {
            out.entry(lang.clone())
                .or_default()
                .insert(key.clone(), text.clone());
        }
    }
    serde_json::to_string(&out).unwrap()
}

#[native_module("cdr/native")]
fn native(registry: &mut Registry) {
    registry.register::<App>();
}

/// A running rip's stop flag; the same flag is the same rip.
#[derive(Clone)]
struct StopFlag(Arc<AtomicBool>);

impl PartialEq for StopFlag {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// A line shown above the footer.
#[derive(Clone, PartialEq)]
struct Note {
    text: String,
    /// Something went wrong (shown in red), rather than news.
    problem: bool,
}

impl Note {
    fn problem(text: String) -> Self {
        Note {
            text,
            problem: true,
        }
    }

    fn info(text: String) -> Self {
        Note {
            text,
            problem: false,
        }
    }
}

#[derive(Clone, PartialEq)]
enum Phase {
    NoDisc,
    Ready,
    Ripping,
    /// The verdict, what it means for this disc, and how the rip went
    /// (time and speed).
    Done(Verdict, String, String),
    Failed(String),
}

#[derive(Clone, PartialEq)]
struct Row {
    number: u8,
    title: String,
    /// Shown only when it isn't the album artist.
    artist: String,
    length: String,
    sectors: u32,
    /// 0–100 while reading.
    progress: f32,
    status: String,
    /// The track read well (AccurateRip or two agreeing reads).
    good: bool,
    done: bool,
}

#[component]
pub fn App() -> impl IntoElement {
    let phase = signal(Phase::NoDisc);
    let disc = signal(None::<Disc>);
    let chosen = signal(0_usize);
    let picking = signal(false);
    let settings = Settings::load();
    let library = settings.library;
    let settings_open = signal(false);
    // What Jellyfin said after the rip: the libraries it scans, or why not.
    let rescanned = signal(None::<Result<Vec<String>, String>>);
    // Another app the system opens on insert, which would read the disc too.
    let auto_open = signal(None::<AutoOpen>);
    let access_denied = signal(false);
    // No optical drive at all: Macs have none built in.
    let no_drive = signal(false);
    // The user's tags, over the chosen release; and the draft while editing.
    let edited = signal(None::<Release>);
    let editing = signal(None::<Release>);
    let rows = signal(Vec::<Row>::new());
    // A line above the footer: a problem, or (Stop) just what happened.
    let message = signal(None::<Note>);
    let offset_line = signal(String::new());
    let started = signal(None::<Instant>);
    // The running rip's stop flag; and whether Stop was pressed.
    let stop_flag = signal(None::<StopFlag>);
    let stopping = signal(false);
    // Tasks live as long as the UI they were started in. Handlers run inside
    // buttons that come and go (Rip disappears once ripping starts), so the
    // tasks they start belong to the app instead.
    let app = Owner::current();

    set_application_menu(vec![
        MenuItem::submenu(
            "CDR",
            [
                MenuItem::role(MenuRole::About),
                MenuItem::separator(),
                MenuItem::new(t!("menu.settings"))
                    .shortcut("mod+,")
                    .on_select(move || settings_open.set(true)),
                MenuItem::separator(),
                MenuItem::role(MenuRole::Hide),
                MenuItem::role(MenuRole::HideOthers),
                MenuItem::role(MenuRole::ShowAll),
                MenuItem::separator(),
                MenuItem::role(MenuRole::Quit),
            ],
        ),
        // The text fields (tags, settings) need these.
        MenuItem::submenu(
            t!("menu.edit"),
            [
                MenuItem::role(MenuRole::Undo),
                MenuItem::role(MenuRole::Redo),
                MenuItem::separator(),
                MenuItem::role(MenuRole::Cut),
                MenuItem::role(MenuRole::Copy),
                MenuItem::role(MenuRole::Paste),
                MenuItem::role(MenuRole::SelectAll),
            ],
        ),
        MenuItem::submenu(
            t!("menu.window"),
            [
                MenuItem::role(MenuRole::Minimize),
                MenuItem::role(MenuRole::Zoom),
                MenuItem::separator(),
                MenuItem::role(MenuRole::Close),
            ],
        ),
    ]);

    let check_auto_open = move || {
        spawn(async move {
            auto_open.set(
                blocking(|| backend().app_opened_on_insert())
                    .await
                    .ok()
                    .flatten(),
            );
        });
    };
    check_auto_open();

    // Look for a disc every two seconds while there is none.
    spawn(async move {
        loop {
            if phase.get() == Phase::NoDisc {
                match blocking(|| backend().read_disc()).await {
                    Ok(Ok(found)) => {
                        access_denied.set(false);
                        no_drive.set(false);
                        rows.set(rows_for(&found, found.releases.first()));
                        chosen.set(0);
                        picking.set(false);
                        edited.set(None);
                        editing.set(None);
                        disc.set(Some(found));
                        phase.set(Phase::Ready);
                    }
                    Ok(Err(e)) => {
                        access_denied.set(is_access_denied(&e));
                        let missing = blocking(|| backend().drive().is_none()).await;
                        no_drive.set(missing.unwrap_or(false));
                    }
                    Err(_) => {}
                }
            }
            timer(Duration::from_secs(2)).await;
        }
    });

    let release = move || {
        edited.get().or_else(|| {
            disc.get()
                .and_then(|d| d.releases.get(chosen.get()).cloned())
        })
    };
    let show_release = move || {
        if let Some(d) = disc.get() {
            rows.set(rows_for(&d, release().as_ref()));
        }
    };
    let choose_release = move |i: usize| {
        chosen.set(i);
        edited.set(None);
        picking.set(false);
        show_release();
    };
    let start_editing = move || {
        let Some(d) = disc.get() else { return };
        picking.set(false);
        editing.set(Some(release().unwrap_or_else(|| blank_release(&d))));
    };
    let finish_editing = move || {
        let Some(mut draft) = editing.get() else {
            return;
        };
        // Track artists that followed the album artist keep following it.
        let before = release().map(|r| r.artist).unwrap_or_default();
        for t in &mut draft.tracks {
            if t.artist.is_empty() || t.artist == before {
                t.artist = draft.artist.clone();
            }
        }
        edited.set(Some(draft));
        editing.set(None);
        show_release();
    };

    // Where this disc's tracks go under the library.
    let destinations = move || {
        let root = library.get()?;
        let d = disc.get()?;
        let release = release();
        let disc_id = DiscToc::from_toc(&d.toc).disc_id();
        let count = d.toc.audio_tracks().count();
        Some(album::destinations(
            Some(&PathBuf::from(root)),
            &settings.template(),
            release.as_ref(),
            &disc_id,
            count,
        ))
    };
    let album_folder = move || {
        destinations()
            .and_then(|(_, dests)| dests.first().and_then(|p| p.parent()).map(display_path))
    };

    let choose_folder = move || {
        let picked = pick_paths(OpenDialog::folders().button_label(t!("library.use")));
        app.run(|| {
            spawn(async move {
                if let Ok(Some(paths)) = picked.await
                    && let Some(path) = paths.into_iter().next()
                {
                    let path = path.to_string_lossy().into_owned();
                    let _ = Storage::set_item(storage_keys::LIBRARY, path.clone());
                    library.set(Some(path));
                }
            })
        });
    };

    let start_rip = move || {
        let (Some(d), Some((tags, dests))) = (disc.get(), destinations()) else {
            return;
        };
        let release = release();
        let Ok(offset) = settings.offset() else {
            message.set(Some(Note::problem(t!("rip.bad_offset"))));
            return;
        };
        if let Some(existing) = dests.iter().find(|p| p.exists()) {
            message.set(Some(Note::problem(t!(
                "rip.already_in_library",
                path = existing.display()
            ))));
            return;
        }
        message.set(None);
        rescanned.set(None);
        picking.set(false);
        offset_line.set(String::new());
        rows.set(rows_for(&d, release.as_ref()));
        let shown = release.clone();
        started.set(Some(Instant::now()));
        phase.set(Phase::Ripping);

        let stop = Arc::new(AtomicBool::new(false));
        stop_flag.set(Some(StopFlag(stop.clone())));
        stopping.set(false);
        let secure_now = settings.secure.get();
        let server = settings.server();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
        let work = blocking(move || {
            let request = RipRequest {
                release,
                dests,
                tags,
                secure: secure_now,
                offset,
                stop,
                server,
            };
            backend().rip(&d, request, &mut |event| {
                let _ = tx.send(event);
            })
        });
        app.run(|| {
            spawn(async move {
                while let Some(event) = rx.recv().await {
                    match event {
                        Event::Offset { offset, source, .. } => offset_line.set(t!(
                            "rip.offset",
                            offset = format!("{offset:+}"),
                            source = t!(offset_source_key(source))
                        )),
                        Event::Progress { track, done, total } => rows.update(|rows| {
                            let row = &mut rows[track];
                            row.progress = done as f32 * 100.0 / total.max(1) as f32;
                            row.status =
                                t!("track.reading", percent = format!("{:.0}", row.progress));
                        }),
                        Event::TrackDone { track, report } => rows.update(|rows| {
                            let row = &mut rows[track];
                            row.progress = 100.0;
                            row.status = status(&report);
                            row.good = report.accuraterip.is_some() || report.suspicious.is_empty();
                            row.done = true;
                        }),
                        _ => {}
                    }
                }
                match work.await {
                    Ok(Ok(report)) => {
                        let (verdict, text) = verdict_line(&report);
                        let took = started.get().map(|s| s.elapsed()).unwrap_or_default();
                        let sectors: u32 = rows.get().iter().map(|r| r.sectors).sum();
                        // What the library server said (the rip asked it to rescan).
                        rescanned.set(report.library_servers.first().map(
                            |scan| match &scan.error {
                                Some(e) => Err(e.clone()),
                                None => Ok(scan.scanning.clone().unwrap_or_default()),
                            },
                        ));
                        phase.set(Phase::Done(verdict, text, how_it_went(took, sectors)));
                    }
                    Ok(Err(e)) if album::is_stopped(&e) => {
                        // Back to where it was before Rip, with nothing kept.
                        if let Some(d) = disc.get() {
                            rows.set(rows_for(&d, shown.as_ref()));
                        }
                        message.set(Some(Note::info(t!("rip.stopped"))));
                        phase.set(Phase::Ready);
                    }
                    Ok(Err(e)) => phase.set(Phase::Failed(format!("{e:#}"))),
                    Err(e) => phase.set(Phase::Failed(e.to_string())),
                }
                stop_flag.set(None);
                stopping.set(false);
            })
        });
    };

    let reveal = move || {
        if let Some((_, dests)) = destinations()
            && let Some(first) = dests.first()
        {
            // Fire and forget: it opens Finder or fails quietly.
            drop(reveal_path(first.clone()));
        }
    };

    // Eject rather than just reset: the disc still in the drive would be found again.
    let eject = move || {
        let Some(d) = disc.get() else { return };
        app.run(|| {
            spawn(async move {
                match blocking(move || backend().eject(&d)).await {
                    Ok(Ok(())) => {
                        disc.set(None);
                        rows.set(Vec::new());
                        message.set(None);
                        rescanned.set(None);
                        editing.set(None);
                        picking.set(false);
                        phase.set(Phase::NoDisc);
                    }
                    Ok(Err(e)) => message.set(Some(Note::problem(t!(
                        "action.eject_failed",
                        error = format!("{e:#}")
                    )))),
                    Err(e) => {
                        message.set(Some(Note::problem(t!("action.eject_failed", error = e))))
                    }
                }
            })
        });
    };

    let stop_rip = move || {
        if let Some(flag) = stop_flag.get() {
            flag.0.store(true, Ordering::Relaxed);
            stopping.set(true);
        }
    };

    let releases = move || disc.get().map(|d| d.releases).unwrap_or_default();

    view()
        .flex_1()
        .bg(color::WINDOW)
        .text_color(color::TEXT)
        .text_size(px(13.0))
        .child(settings_window(settings_open, settings, choose_folder))
        // Above every screen: it matters most while ripping.
        .child(auto_open_notice(auto_open, move || {
            app.run(check_auto_open)
        }))
        .child(
            when(
                move || phase.get() == Phase::NoDisc,
                move || waiting(library, access_denied, no_drive),
            )
            .otherwise(move || {
                view()
                    .flex_1()
                    .child(verdict_banner(phase))
                    .child(
                        when(
                            move || phase.get() == Phase::Ready,
                            move || {
                                album_header(
                                    release,
                                    releases,
                                    chosen,
                                    picking,
                                    move || editing.get().is_none(),
                                    start_editing,
                                )
                            },
                        )
                        .otherwise(move || progress_header(release, phase, rows, started)),
                    )
                    .child(
                        when(
                            move || editing.get().is_some(),
                            move || tag_editor(editing, finish_editing),
                        )
                        .otherwise(move || {
                            when(
                                move || picking.get(),
                                move || release_picker(releases, chosen, choose_release),
                            )
                            .otherwise(move || track_list(rows, phase))
                        }),
                    )
                    .child(when(
                        move || matches!(rescanned.get(), Some(Err(_))),
                        move || {
                            text(move || match rescanned.get() {
                                Some(Err(e)) => t!("jellyfin.failed", error = e),
                                _ => String::new(),
                            })
                            .px(px(24.0))
                            .py(px(8.0))
                            .text_size(px(12.0))
                            .text_color(color::WARN)
                        },
                    ))
                    .child(when(
                        move || message.get().is_some(),
                        move || {
                            text(move || message.get().map(|n| n.text).unwrap_or_default())
                                .px(px(24.0))
                                .py(px(8.0))
                                .text_size(px(12.0))
                                .text_color(move || {
                                    if message.get().is_some_and(|n| n.problem) {
                                        color::BAD
                                    } else {
                                        color::MUTED
                                    }
                                })
                        },
                    ))
                    .child(footer(
                        phase,
                        library,
                        album_folder,
                        offset_line,
                        rescanned,
                        stopping,
                        Actions {
                            choose_folder,
                            start_rip,
                            stop_rip,
                            reveal,
                            eject,
                        },
                    ))
            }),
        )
}

/// Drives CDR is developed and tested on, recommended when none is connected.
const TESTED_DRIVES: &[&str] = &["LG BP71N"];

/// No disc yet: what to do, and where the albums will go. Without a drive,
/// which drive to get.
fn waiting(
    library: Signal<Option<String>>,
    access_denied: Signal<bool>,
    no_drive: Signal<bool>,
) -> impl IntoElement {
    view()
        .flex_1()
        .items_center()
        .justify_center()
        .gap(px(14.0))
        .p(px(24.0))
        .child(icon(icons::DISC, 104.0).text_color(color::MUTED))
        .child(
            text(move || {
                if no_drive.get() {
                    t!("start.no_drive.title")
                } else {
                    t!("start.title")
                }
            })
            .text_size(px(22.0))
            .font_weight(FontWeight::SEMIBOLD),
        )
        .child(
            text(move || {
                if no_drive.get() {
                    t!("start.no_drive.subtitle")
                } else {
                    t!("start.subtitle")
                }
            })
            .max_w(px(440.0))
            .text_center()
            .text_size(px(14.0))
            .text_color(color::MUTED),
        )
        .child(when(
            move || no_drive.get(),
            || {
                view()
                    .mt(px(6.0))
                    .max_w(px(440.0))
                    .px(px(16.0))
                    .py(px(12.0))
                    .gap(px(4.0))
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(color::BORDER)
                    .bg(color::STRIPE)
                    .child(
                        text(move || {
                            t!("start.no_drive.tested", drives = TESTED_DRIVES.join(", "))
                        })
                        .font_weight(FontWeight::SEMIBOLD),
                    )
                    .child(
                        text(move || t!("start.no_drive.others"))
                            .text_size(px(12.0))
                            .text_color(color::MUTED),
                    )
            },
        ))
        .child(when(
            move || access_denied.get(),
            move || {
                text(move || t!("start.access_denied"))
                    .max_w(px(480.0))
                    .text_center()
                    .text_color(color::BAD)
            },
        ))
        .child(
            view()
                .mt(px(10.0))
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .child(icon(icons::FOLDER, 13.0).text_color(color::MUTED))
                .child(
                    text(move || {
                        library
                            .get()
                            .map(|l| t!("start.library", path = display_path(l.as_ref())))
                            .unwrap_or_else(|| t!("library.none"))
                    })
                    .text_size(px(12.0))
                    .text_color(color::MUTED),
                ),
        )
}

fn cover_url(release: &Release) -> String {
    format!(
        "https://coverartarchive.org/release/{}/front-250",
        release.id
    )
}

/// The album's cover, or a plain square while there is none.
fn cover(release: impl Fn() -> Option<Release> + Copy + 'static, size: f32) -> impl IntoElement {
    view()
        .size(px(size))
        .flex_shrink_0()
        .rounded(px(if size > 80.0 { 8.0 } else { 5.0 }))
        .overflow_hidden()
        .bg(color::STRIPE)
        .border_1()
        .border_color(color::BORDER)
        .child(when(
            move || release().is_some_and(|r| !r.id.is_empty()),
            move || {
                image(move || release().map(|r| cover_url(&r)).unwrap_or_default())
                    .size_full()
                    .object_fit(nsh_core::ResizeMode::Cover)
            },
        ))
}

/// Before ripping: the album, which release it is, and its tags.
fn album_header(
    release: impl Fn() -> Option<Release> + Copy + 'static,
    releases: impl Fn() -> Vec<Release> + Copy + 'static,
    chosen: Signal<usize>,
    picking: Signal<bool>,
    can_edit: impl Fn() -> bool + Copy + 'static,
    edit: impl Fn() + Clone + 'static,
) -> impl IntoElement {
    let found = move || release().is_some_and(|r| !r.id.is_empty());
    view()
        .flex_row()
        .gap(px(20.0))
        .px(px(24.0))
        .pt(px(20.0))
        .pb(px(18.0))
        .border_b_1()
        .border_color(color::BORDER)
        .child(cover(release, 132.0))
        .child(
            view()
                .flex_1()
                .min_w(px(0.0))
                .child(
                    text(move || {
                        if found() {
                            t!("album.found")
                        } else {
                            t!("album.not_found")
                        }
                    })
                    .text_size(px(11.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(move || if found() { color::OK } else { color::WARN }),
                )
                .child(
                    text(move || {
                        release()
                            .map(|r| r.title)
                            .filter(|t| !t.is_empty())
                            .unwrap_or_else(|| t!("album.unknown"))
                    })
                    .mt(px(6.0))
                    .number_of_lines(2)
                    .text_size(px(24.0))
                    .font_weight(FontWeight::BOLD),
                )
                .child(
                    text(move || release().map(|r| r.artist).unwrap_or_default())
                        .mt(px(2.0))
                        .number_of_lines(1)
                        .text_size(px(15.0))
                        .text_color(color::MUTED),
                )
                .child(
                    view()
                        .mt(px(8.0))
                        .flex_row()
                        .gap(px(4.0))
                        .child(
                            text(move || release().map(|r| details(&r)).unwrap_or_default())
                                .text_size(px(12.0))
                                .text_color(color::MUTED),
                        )
                        .child(
                            text(move || {
                                let has_details =
                                    release().is_some_and(|r| !details(&r).is_empty());
                                let summary = release()
                                    .map(|r| r.tracks.len())
                                    .map(|n| t!("album.tracks", count = n))
                                    .unwrap_or_default();
                                match (has_details, summary.is_empty()) {
                                    (true, false) => format!("· {summary}"),
                                    _ => summary,
                                }
                            })
                            .text_size(px(12.0))
                            .text_color(color::MUTED),
                        ),
                )
                .child(view().flex_1().min_h(px(12.0)))
                .child(
                    view()
                        .flex_row()
                        .gap(px(8.0))
                        .child(when(
                            move || releases().len() > 1 && can_edit(),
                            move || {
                                small_button(
                                    move || {
                                        t!(
                                            "release.n_of",
                                            n = chosen.get() + 1,
                                            total = releases().len()
                                        )
                                    },
                                    Some(icons::CHEVRON),
                                    move || picking.update(|p| *p = !*p),
                                )
                            },
                        ))
                        .child(when(can_edit, move || {
                            small_button(
                                move || {
                                    if release().is_some() {
                                        t!("album.edit")
                                    } else {
                                        t!("album.add_tags")
                                    }
                                },
                                Some(icons::PENCIL),
                                edit.clone(),
                            )
                        })),
                ),
        )
}

/// While ripping and after: a smaller header, with the album's progress.
fn progress_header(
    release: impl Fn() -> Option<Release> + Copy + 'static,
    phase: Signal<Phase>,
    rows: Signal<Vec<Row>>,
    started: Signal<Option<Instant>>,
) -> impl IntoElement {
    let ripping = move || phase.get() == Phase::Ripping;
    let fraction = move || {
        let rows = rows.get();
        let total: u32 = rows.iter().map(|r| r.sectors).sum();
        let done: f32 = rows
            .iter()
            .map(|r| r.progress / 100.0 * r.sectors as f32)
            .sum();
        if total == 0 { 0.0 } else { done / total as f32 }
    };
    let position = move || {
        let rows = rows.get();
        let n = rows
            .iter()
            .position(|r| !r.done)
            .unwrap_or(rows.len().saturating_sub(1));
        t!(
            "rip.position",
            n = n + 1,
            total = rows.len(),
            percent = format!("{:.0}", fraction() * 100.0)
        )
    };
    let left = move || {
        let f = fraction();
        let Some(start) = started.get() else {
            return String::new();
        };
        let elapsed = start.elapsed().as_secs_f32();
        if f < 0.02 || elapsed < 3.0 {
            return String::new();
        }
        let sectors: u32 = rows.get().iter().map(|r| r.sectors).sum();
        let speed = f * sectors as f32 / 75.0 / elapsed;
        let minutes = (elapsed * (1.0 - f) / f / 60.0).round() as u32;
        let left = if minutes < 1 {
            t!("rip.under_a_minute")
        } else {
            t!("rip.minutes_left", minutes = minutes)
        };
        format!("{left} · {speed:.1}×")
    };
    view()
        .flex_row()
        .items_center()
        .gap(px(18.0))
        .px(px(24.0))
        .py(px(16.0))
        .border_b_1()
        .border_color(color::BORDER)
        .child(cover(release, 64.0))
        .child(
            view()
                .flex_1()
                .min_w(px(0.0))
                .child(
                    view()
                        .flex_row()
                        .items_baseline()
                        .gap(px(10.0))
                        .child(
                            text(move || {
                                release()
                                    .map(|r| r.title)
                                    .filter(|t| !t.is_empty())
                                    .unwrap_or_else(|| t!("album.unknown"))
                            })
                            .number_of_lines(1)
                            .text_size(px(18.0))
                            .font_weight(FontWeight::BOLD),
                        )
                        .child(
                            text(move || release().map(|r| r.artist).unwrap_or_default())
                                .flex_1()
                                .number_of_lines(1)
                                .text_color(color::MUTED),
                        ),
                )
                .child(when(ripping, move || {
                    view()
                        .child(
                            progress()
                                .mt(px(10.0))
                                .value(move || fraction() * 100.0)
                                .track_style(|s| s.h(px(6.0)).rounded(px(3.0)).bg(color::TRACK))
                                .indicator_style(|s| {
                                    s.h(px(6.0)).rounded(px(3.0)).bg(color::ACCENT)
                                }),
                        )
                        .child(
                            view()
                                .mt(px(7.0))
                                .flex_row()
                                .child(
                                    text(position)
                                        .flex_1()
                                        .text_size(px(12.0))
                                        .tabular_nums()
                                        .text_color(color::MUTED),
                                )
                                .child(
                                    text(left)
                                        .text_size(px(12.0))
                                        .tabular_nums()
                                        .text_color(color::MUTED),
                                ),
                        )
                })),
        )
}

/// After the rip: the verdict, big. Failures show here too.
fn verdict_banner(phase: Signal<Phase>) -> impl IntoElement {
    let kind = move || match phase.get() {
        Phase::Done(Verdict::Perfect | Verdict::Verified, ..) => Some(true),
        Phase::Done(Verdict::Check, ..) | Phase::Failed(_) => Some(false),
        _ => None,
    };
    let good = move || kind() == Some(true);
    when(
        move || kind().is_some(),
        move || {
            view()
                .flex_row()
                .items_center()
                .gap(px(16.0))
                .mx(px(12.0))
                .mt(px(16.0))
                .mb(px(4.0))
                .px(px(11.0))
                .py(px(15.0))
                .rounded(px(12.0))
                .border_1()
                .border_color(move || {
                    if good() {
                        color::OK_BORDER
                    } else {
                        color::BAD_BORDER
                    }
                })
                .bg(move || if good() { color::OK_BG } else { color::BAD_BG })
                .child(
                    view()
                        .size(px(44.0))
                        .flex_shrink_0()
                        .rounded(px(22.0))
                        .items_center()
                        .justify_center()
                        .bg(move || if good() { color::OK } else { color::BAD })
                        .child(
                            icon_with(
                                move || if good() { icons::CHECK } else { icons::ALERT },
                                22.0,
                            )
                            .text_color(color::ON_COLOR),
                        ),
                )
                .child(
                    view()
                        .flex_1()
                        .min_w(px(0.0))
                        .gap(px(3.0))
                        .child(
                            text(move || match phase.get() {
                                Phase::Done(Verdict::Perfect, ..) => t!("verdict.perfect.title"),
                                Phase::Done(Verdict::Verified, ..) => t!("verdict.verified.title"),
                                Phase::Done(Verdict::Check, ..) => t!("verdict.check.title"),
                                _ => t!("rip.failed.title"),
                            })
                            .text_size(px(18.0))
                            .font_weight(FontWeight::BOLD),
                        )
                        .child(
                            text(move || match phase.get() {
                                Phase::Done(_, text, how) => format!("{text} · {how}"),
                                Phase::Failed(e) => e,
                                _ => String::new(),
                            })
                            .text_color(color::MUTED),
                        ),
                )
        },
    )
}

/// Only shown when MusicBrainz knows several releases with this disc: each
/// with its cover and what tells pressings apart.
fn release_picker(
    releases: impl Fn() -> Vec<Release> + Copy + 'static,
    chosen: Signal<usize>,
    choose: impl Fn(usize) + Copy + 'static,
) -> impl IntoElement {
    scroll()
        .flex_1()
        .child(view().px(px(12.0)).py(px(8.0)).gap(px(4.0)).child(each(
            move || releases().into_iter().enumerate().collect::<Vec<_>>(),
            |(i, r)| (*i, r.id.clone()),
            move |item, _| {
                let i = move || item.get().0;
                let this = move || Some(item.get().1);
                button()
                    .flex_row()
                    .items_center()
                    .gap(px(12.0))
                    .px(px(12.0))
                    .py(px(8.0))
                    .rounded(px(8.0))
                    .bg(move || {
                        if chosen.get() == i() {
                            color::CURRENT
                        } else {
                            color::TRANSPARENT
                        }
                    })
                    .selected(move || chosen.get() == i())
                    .active(|s| s.opacity(0.7))
                    .on_press(move || choose(i()))
                    .child(cover(this, 40.0))
                    .child(
                        view()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(
                                text(move || item.get().1.title)
                                    .number_of_lines(1)
                                    .font_weight(FontWeight::MEDIUM),
                            )
                            .child(
                                text(move || details(&item.get().1))
                                    .text_size(px(12.0))
                                    .text_color(color::MUTED),
                            ),
                    )
            },
        )))
}

/// The album's and every track's tags, editable. Album and album artist are
/// needed to file the album; empty track titles become "Track N".
fn tag_editor(
    draft: Signal<Option<Release>>,
    done: impl Fn() + Clone + 'static,
) -> impl IntoElement {
    let edit = move |f: &dyn Fn(&mut Release)| {
        draft.update(|d| {
            if let Some(d) = d {
                f(d)
            }
        })
    };
    let complete = move || {
        draft
            .get()
            .is_some_and(|r| !r.title.trim().is_empty() && !r.artist.trim().is_empty())
    };
    let field_input = || {
        input()
            .flex_1()
            .h(px(28.0))
            .px(px(8.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(color::BORDER)
            .bg(color::FIELD)
    };
    let field = move |key: &'static str,
                      id: &'static str,
                      value: fn(&Release) -> String,
                      set: fn(&mut Release, &str)| {
        view()
            .flex_row()
            .items_center()
            .gap(px(12.0))
            .child(
                text(move || t!(key))
                    .w(px(110.0))
                    .text_right()
                    .text_color(color::MUTED),
            )
            .child(
                field_input()
                    .test_id(id)
                    .value(move || draft.get().map(|r| value(&r)).unwrap_or_default())
                    .on_change(move |text: &str| edit(&|r| set(r, text))),
            )
    };
    view()
        .flex_1()
        .gap(px(8.0))
        .px(px(24.0))
        .pt(px(16.0))
        .pb(px(12.0))
        .child(field(
            "editor.album",
            "album-title",
            |r| r.title.clone(),
            |r, v| r.title = v.to_owned(),
        ))
        .child(field(
            "editor.artist",
            "album-artist",
            |r| r.artist.clone(),
            |r, v| r.artist = v.to_owned(),
        ))
        .child(field(
            "editor.year",
            "album-year",
            |r| r.date.clone().unwrap_or_default(),
            |r, v| r.date = Some(v.trim().to_owned()).filter(|d| !d.is_empty()),
        ))
        .child(
            scroll()
                .flex_1()
                .mt(px(8.0))
                .child(view().gap(px(6.0)).child(each(
                    move || {
                        draft
                            .get()
                            .map(|r| r.tracks)
                            .unwrap_or_default()
                            .into_iter()
                            .enumerate()
                            .collect::<Vec<_>>()
                    },
                    |(n, _)| *n,
                    move |item, _| {
                        let n = move || item.get().0;
                        view()
                            .flex_row()
                            .items_center()
                            .gap(px(12.0))
                            .child(
                                text(move || item.get().1.number.to_string())
                                    .w(px(110.0))
                                    .text_right()
                                    .tabular_nums()
                                    .text_color(color::MUTED),
                            )
                            .child(
                                field_input()
                                    .test_id(&format!(
                                        "track-title-{}",
                                        item.get_untracked().1.number
                                    ))
                                    .placeholder(move || {
                                        t!("track.untitled", n = item.get().1.number)
                                    })
                                    .value(move || item.get().1.title)
                                    .on_change(move |text: &str| {
                                        let i = n();
                                        edit(&|r| {
                                            if let Some(t) = r.tracks.get_mut(i) {
                                                t.title = text.to_owned();
                                            }
                                        })
                                    }),
                            )
                    },
                ))),
        )
        .child(view().flex_row().justify_end().child(primary_button(
            move || t!("editor.done"),
            move || !complete(),
            done.clone(),
        )))
}

fn track_list(rows: Signal<Vec<Row>>, phase: Signal<Phase>) -> impl IntoElement {
    scroll()
        .flex_1()
        .child(view().px(px(12.0)).py(px(6.0)).child(each(
            move || rows.get(),
            |r| r.number,
            move |row, index| {
                let current = move || {
                    phase.get() == Phase::Ripping && !row.get().done && row.get().progress > 0.0
                };
                let pending = move || {
                    phase.get() == Phase::Ripping && !row.get().done && row.get().progress == 0.0
                };
                view()
                    .flex_row()
                    .items_center()
                    .gap(px(12.0))
                    .h(px(30.0))
                    .px(px(12.0))
                    .rounded(px(6.0))
                    .bg(move || {
                        if current() {
                            color::CURRENT
                        } else if index.get() % 2 == 1 {
                            color::STRIPE
                        } else {
                            color::TRANSPARENT
                        }
                    })
                    .child(
                        text(move || row.get().number.to_string())
                            .w(px(20.0))
                            .text_right()
                            .tabular_nums()
                            .text_color(color::MUTED),
                    )
                    .child(
                        view()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex_row()
                            .items_baseline()
                            .gap(px(8.0))
                            .child(text(move || row.get().title).number_of_lines(1).text_color(
                                move || {
                                    if pending() { color::FAINT } else { color::TEXT }
                                },
                            ))
                            .child(
                                text(move || row.get().artist)
                                    .flex_1()
                                    .number_of_lines(1)
                                    .text_size(px(12.0))
                                    .text_color(color::MUTED),
                            ),
                    )
                    .child(when(current, move || {
                        progress()
                            .w(px(120.0))
                            .value(move || row.get().progress)
                            .track_style(|s| s.h(px(4.0)).rounded(px(2.0)).bg(color::TRACK))
                            .indicator_style(|s| s.h(px(4.0)).rounded(px(2.0)).bg(color::ACCENT))
                    }))
                    .child(when(
                        move || !row.get().status.is_empty(),
                        move || {
                            let tint = move || {
                                if !row.get().done {
                                    color::ACCENT
                                } else if row.get().good {
                                    color::OK
                                } else {
                                    color::WARN
                                }
                            };
                            view()
                                .flex_row()
                                .items_center()
                                .gap(px(5.0))
                                .child(when(
                                    move || row.get().done,
                                    move || {
                                        icon_with(
                                            move || {
                                                if row.get().good {
                                                    icons::CHECK
                                                } else {
                                                    icons::ALERT
                                                }
                                            },
                                            12.0,
                                        )
                                        .text_color(tint)
                                    },
                                ))
                                .child(
                                    text(move || row.get().status)
                                        .text_size(px(12.0))
                                        .tabular_nums()
                                        .text_color(tint),
                                )
                        },
                    ))
                    .child(
                        text(move || row.get().length)
                            .w(px(40.0))
                            .text_right()
                            .tabular_nums()
                            .text_color(color::MUTED),
                    )
            },
        )))
}

/// What the footer's buttons do.
struct Actions<A, B, C, D, E> {
    choose_folder: A,
    start_rip: B,
    stop_rip: C,
    reveal: D,
    eject: E,
}

/// Where the album goes, and what to do next.
fn footer(
    phase: Signal<Phase>,
    library: Signal<Option<String>>,
    album_folder: impl Fn() -> Option<String> + Copy + 'static,
    offset_line: Signal<String>,
    rescanned: Signal<Option<Result<Vec<String>, String>>>,
    stopping: Signal<bool>,
    actions: Actions<
        impl Fn() + Clone + 'static,
        impl Fn() + Clone + 'static,
        impl Fn() + Clone + 'static,
        impl Fn() + Clone + 'static,
        impl Fn() + Clone + 'static,
    >,
) -> impl IntoElement {
    let Actions {
        choose_folder,
        start_rip,
        stop_rip,
        reveal,
        eject,
    } = actions;
    let ready = move || phase.get() == Phase::Ready;
    let finished = move || matches!(phase.get(), Phase::Done(..) | Phase::Failed(_));
    let place = move || match phase.get() {
        Phase::Ripping => offset_line.get(),
        Phase::Done(..) => {
            let saved = album_folder()
                .map(|p| t!("rip.saved_to", path = p))
                .unwrap_or_default();
            match rescanned.get() {
                Some(Ok(names)) => format!(
                    "{saved} · {}",
                    t!("jellyfin.scanning", libraries = names.join(", "))
                ),
                _ => saved,
            }
        }
        _ => album_folder().unwrap_or_else(|| t!("library.none")),
    };
    view()
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .h(px(60.0))
        .flex_shrink_0()
        .px(px(24.0))
        .border_t_1()
        .border_color(color::BORDER)
        .bg(color::BAR)
        .child(when(
            move || phase.get() != Phase::Ripping,
            move || icon(icons::FOLDER, 14.0).text_color(color::MUTED),
        ))
        .child(
            text(place)
                .flex_1()
                .min_w(px(0.0))
                .number_of_lines(1)
                .text_size(px(12.0))
                .text_color(color::MUTED),
        )
        // Settings has it too; here only until there is a library.
        .child(when(move || ready() && library.get().is_none(), {
            let choose_folder = choose_folder.clone();
            move || small_button(move || t!("library.choose"), None, choose_folder.clone())
        }))
        .child(when(move || phase.get() == Phase::Ripping, {
            let stop_rip = stop_rip.clone();
            move || {
                secondary_button(
                    move || {
                        if stopping.get() {
                            t!("action.stopping")
                        } else {
                            t!("action.stop")
                        }
                    },
                    stop_rip.clone(),
                )
            }
        }))
        .child(when(move || matches!(phase.get(), Phase::Done(..)), {
            let reveal = reveal.clone();
            move || secondary_button(move || t!("action.show_in_finder"), reveal.clone())
        }))
        .child(when(ready, {
            let eject = eject.clone();
            move || secondary_button(move || t!("action.eject"), eject.clone())
        }))
        .child(when(ready, {
            let start_rip = start_rip.clone();
            move || {
                primary_button(
                    move || t!("action.rip"),
                    move || library.get().is_none(),
                    start_rip.clone(),
                )
            }
        }))
        .child(when(finished, {
            let eject = eject.clone();
            move || primary_button(move || t!("action.eject"), || false, eject.clone())
        }))
}

/// Shown while the system opens another app on insert: it would read the
/// disc at the same time and slow ripping down.
fn auto_open_notice(
    auto_open: Signal<Option<AutoOpen>>,
    check_again: impl Fn() + Clone + 'static,
) -> impl IntoElement {
    when(
        move || auto_open.get().is_some(),
        move || {
            view()
                .flex_row()
                .items_center()
                .gap(px(12.0))
                .mx(px(12.0))
                .mt(px(14.0))
                .px(px(11.0))
                .py(px(10.0))
                .rounded(px(10.0))
                .border_1()
                .border_color(color::WARN_BORDER)
                .bg(color::WARN_BG)
                .child(icon(icons::ALERT, 18.0).text_color(color::WARN))
                .child(
                    text(move || match auto_open.get() {
                        Some(AutoOpen::OtherApp) => t!("notice.auto_open_other"),
                        _ => t!("notice.auto_open_music"),
                    })
                    .flex_1()
                    .text_size(px(12.5)),
                )
                .child(small_button(
                    move || t!("notice.check_again"),
                    None,
                    check_again.clone(),
                ))
        },
    )
}

/// A setting that is on or off: a box that shows a tick, and its label.
fn checkbox_button(
    label: &'static str,
    on: Signal<bool>,
    on_press: impl Fn() + 'static,
) -> impl IntoElement {
    button()
        .flex_row()
        .items_center()
        .gap(px(7.0))
        .selected(move || on.get())
        .on_press(on_press)
        .child(
            view()
                .size(px(16.0))
                .rounded(px(4.0))
                .border_1()
                .border_color(move || {
                    if on.get() {
                        color::ACCENT
                    } else {
                        color::BORDER_STRONG
                    }
                })
                .bg(move || {
                    if on.get() {
                        color::ACCENT
                    } else {
                        color::BUTTON
                    }
                })
                .items_center()
                .justify_center()
                .child(when(
                    move || on.get(),
                    || icon(icons::CHECK, 11.0).text_color(color::ON_COLOR),
                )),
        )
        .child(text(move || t!(label)).text_size(px(12.5)))
}

/// A compact button in the header and notices, with an optional icon.
fn small_button(
    label: impl Fn() -> String + 'static,
    with_icon: Option<&'static str>,
    on_press: impl Fn() + 'static,
) -> impl IntoElement {
    button()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .h(px(28.0))
        .px(px(10.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(color::BORDER_STRONG)
        .bg(color::BUTTON)
        .active(|s| s.opacity(0.7))
        .on_press(on_press)
        .child(when(
            move || with_icon.is_some(),
            move || icon(with_icon.unwrap_or_default(), 12.0).text_color(color::MUTED),
        ))
        .child(text(label).text_size(px(12.5)))
}

fn secondary_button(
    label: impl Fn() -> String + 'static,
    on_press: impl Fn() + 'static,
) -> impl IntoElement {
    button()
        .h(px(32.0))
        .px(px(14.0))
        .justify_center()
        .rounded(px(7.0))
        .border_1()
        .border_color(color::BORDER_STRONG)
        .bg(color::BUTTON)
        .active(|s| s.opacity(0.7))
        .on_press(on_press)
        .child(text(label))
}

/// The one thing to do next, in the accent color.
fn primary_button(
    label: impl Fn() -> String + 'static,
    disabled: impl Fn() -> bool + Clone + 'static,
    on_press: impl Fn() + 'static,
) -> impl IntoElement {
    let dimmed = disabled.clone();
    button()
        .h(px(32.0))
        .px(px(20.0))
        .justify_center()
        .rounded(px(7.0))
        .bg(color::ACCENT)
        .disabled(disabled)
        .opacity(move || if dimmed() { 0.4_f32 } else { 1.0 })
        .active(|s| s.opacity(0.8))
        .on_press(on_press)
        .child(
            text(label)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(color::ON_COLOR),
        )
}

fn rows_for(disc: &Disc, release: Option<&Release>) -> Vec<Row> {
    let audio: Vec<_> = disc
        .toc
        .tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.is_audio)
        .collect();
    let album_artist = release.map(|r| r.artist.as_str()).unwrap_or_default();
    audio
        .iter()
        .enumerate()
        .map(|(n, &(i, t))| {
            let meta = release.and_then(|r| r.tracks.get(n));
            let sectors = disc.toc.track_end(i) - t.start;
            Row {
                number: t.number,
                title: meta
                    .map(|m| m.title.clone())
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| t!("track.untitled", n = t.number)),
                artist: meta
                    .map(|m| m.artist.clone())
                    .filter(|a| a != album_artist)
                    .unwrap_or_default(),
                length: minutes_seconds(sectors as usize),
                sectors,
                progress: 0.0,
                status: String::new(),
                good: false,
                done: false,
            }
        })
        .collect()
}

/// `m:ss` of `sectors` of audio.
fn minutes_seconds(sectors: usize) -> String {
    let seconds = sectors / 75;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// `path` with the home folder as `~`.
fn display_path(path: &std::path::Path) -> String {
    let path = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && path.starts_with(&home) => {
            format!("~{}", &path[home.len()..])
        }
        _ => path,
    }
}

fn details(r: &Release) -> String {
    [
        r.date.as_deref(),
        r.country.as_deref(),
        r.label.as_deref(),
        r.catalog_number.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

fn offset_source_key(source: OffsetSource) -> &'static str {
    match source {
        OffsetSource::Manual => "offset.manual",
        OffsetSource::AccurateRipDrives => "offset.accuraterip",
        OffsetSource::Learned => "offset.learned",
        OffsetSource::ThisDisc => "offset.this_disc",
    }
}

/// How long the rip took and how fast it read.
fn how_it_went(took: Duration, sectors: u32) -> String {
    let secs = took.as_secs();
    let speed = sectors as f32 / 75.0 / took.as_secs_f32().max(0.001);
    t!(
        "rip.took",
        minutes = secs / 60,
        seconds = secs % 60,
        speed = format!("{speed:.1}")
    )
}

fn status(t: &TrackReport) -> String {
    match (t.accuraterip, t.suspicious.is_empty()) {
        (Some(m), _) if m.pressing_offset.is_some() => {
            t!("track.accurate_other_pressing", confidence = m.confidence)
        }
        (Some(m), _) => t!("track.accurate", confidence = m.confidence),
        (None, true) => t!("track.secure"),
        (None, false) => t!("track.check", position = t.suspicious[0].trim()),
    }
}

/// The disc's verdict in the app's language ([`RipReport::verdict`] decides it).
fn verdict_line(report: &RipReport) -> (Verdict, String) {
    let total = report.tracks.len();
    let accurate = report
        .tracks
        .iter()
        .filter(|t| t.accuraterip.is_some())
        .count();
    let verdict = report.verdict().0;
    let text = match verdict {
        Verdict::Perfect => t!("verdict.perfect", total = total),
        Verdict::Verified => t!("verdict.verified", accurate = accurate, total = total),
        Verdict::Check => {
            let tracks: Vec<String> = report
                .tracks
                .iter()
                .filter(|t| !t.suspicious.is_empty())
                .map(|t| t.number.to_string())
                .collect();
            t!("verdict.check", tracks = tracks.join(", "))
        }
    };
    (verdict, text)
}

/// A release with nothing known yet but the disc's tracks, for typing tags in.
fn blank_release(disc: &Disc) -> Release {
    let tracks = disc
        .toc
        .audio_tracks()
        .map(|t| TrackMeta {
            number: t.number as u32,
            title: String::new(),
            artist: String::new(),
            artist_ids: Vec::new(),
            track_id: String::new(),
            recording_id: String::new(),
            length_ms: None,
        })
        .collect();
    Release {
        id: String::new(),
        release_group_id: None,
        title: String::new(),
        artist: String::new(),
        artist_ids: Vec::new(),
        date: None,
        country: None,
        label: None,
        catalog_number: None,
        barcode: None,
        disc_number: 1,
        disc_total: 1,
        tracks,
        exact: false,
    }
}
