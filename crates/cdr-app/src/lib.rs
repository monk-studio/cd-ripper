//! CDR's desktop app: insert a CD, pick the release and the library folder,
//! rip. The work runs on a blocking thread and reports back to the UI through
//! a channel; signals only change on the UI thread. Everything outside the
//! app goes through [`backend::Backend`], so tests can run it headless.

pub mod backend;

use std::path::PathBuf;
use std::time::Duration;

use backend::{AutoOpen, Disc, RipRequest, backend, is_access_denied};
use cdr_core::album::{self, Event};
use cdr_core::library::DEFAULT_TEMPLATE;
use cdr_core::musicbrainz::{DiscToc, Release};
use cdr_core::report::{RipReport, TrackReport, Verdict, timestamp};
use cdr_core::storage_keys;
use nsh_base::{button, progress, theme};
use nsh_core::file_dialog::OpenDialog;
use nsh_core::prelude::*;
use nsh_core::storage::Storage;

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

#[derive(Clone, PartialEq)]
enum Phase {
    NoDisc,
    Ready,
    Ripping,
    Done(Verdict, String),
    Failed(String),
}

#[derive(Clone, PartialEq)]
struct Row {
    number: u8,
    title: String,
    artist: String,
    length: String,
    /// 0–100 while reading.
    progress: f32,
    status: String,
    done: bool,
}

#[component]
pub fn App() -> impl IntoElement {
    let phase = signal(Phase::NoDisc);
    let disc = signal(None::<Disc>);
    let chosen = signal(0_usize);
    let library = signal(Storage::get_item(storage_keys::LIBRARY));
    let secure = signal(Storage::get_item(storage_keys::SECURE).as_deref() == Some("1"));
    // Another app the system opens on insert, which would read the disc too.
    let auto_open = signal(None::<AutoOpen>);
    let access_denied = signal(false);
    let rows = signal(Vec::<Row>::new());
    let message = signal(None::<String>);
    // Tasks live as long as the UI they were started in. Handlers run inside
    // buttons that come and go (Rip disappears once ripping starts), so the
    // tasks they start belong to the app instead.
    let app = Owner::current();

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
                        rows.set(rows_for(&found, found.releases.first()));
                        chosen.set(0);
                        disc.set(Some(found));
                        phase.set(Phase::Ready);
                    }
                    Ok(Err(e)) => access_denied.set(is_access_denied(&e)),
                    Err(_) => {}
                }
            }
            timer(Duration::from_secs(2)).await;
        }
    });

    let release = move || {
        disc.get()
            .and_then(|d| d.releases.get(chosen.get()).cloned())
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
        let (Some(d), Some(root)) = (disc.get(), library.get()) else {
            return;
        };
        let release = d.releases.get(chosen.get()).cloned();
        let disc_id = DiscToc::from_toc(&d.toc).disc_id();
        let count = d.toc.audio_tracks().count();
        let (tags, dests) = album::destinations(
            Some(&PathBuf::from(&root)),
            DEFAULT_TEMPLATE,
            release.as_ref(),
            &disc_id,
            count,
        );
        if let Some(existing) = dests.iter().find(|p| p.exists()) {
            message.set(Some(t!(
                "rip.already_in_library",
                path = existing.display()
            )));
            return;
        }
        message.set(None);
        rows.set(rows_for(&d, release.as_ref()));
        phase.set(Phase::Ripping);

        let secure_now = secure.get();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
        let work = blocking(move || {
            let request = RipRequest {
                release,
                dests,
                tags,
                secure: secure_now,
            };
            backend().rip(&d, request, &mut |event| {
                let _ = tx.send(event);
            })
        });
        app.run(|| {
            spawn(async move {
                while let Some(event) = rx.recv().await {
                    match event {
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
                            row.done = true;
                        }),
                        _ => {}
                    }
                }
                match work.await {
                    Ok(Ok(report)) => {
                        let (verdict, text) = verdict_line(&report);
                        phase.set(Phase::Done(verdict, text));
                    }
                    Ok(Err(e)) => phase.set(Phase::Failed(format!("{e:#}"))),
                    Err(e) => phase.set(Phase::Failed(e.to_string())),
                }
            })
        });
    };

    let reveal = move || {
        let Some(root) = library.get() else { return };
        let Some(d) = disc.get() else { return };
        let release = release();
        let disc_id = DiscToc::from_toc(&d.toc).disc_id();
        let count = d.toc.audio_tracks().count();
        let (_, dests) = album::destinations(
            Some(&PathBuf::from(root)),
            DEFAULT_TEMPLATE,
            release.as_ref(),
            &disc_id,
            count,
        );
        if let Some(first) = dests.first() {
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
                        phase.set(Phase::NoDisc);
                    }
                    Ok(Err(e)) => {
                        message.set(Some(t!("action.eject_failed", error = format!("{e:#}"))))
                    }
                    Err(e) => message.set(Some(t!("action.eject_failed", error = e))),
                }
            })
        });
    };

    let toggle_secure = move || {
        let on = !secure.get();
        let _ = Storage::set_item(storage_keys::SECURE, if on { "1" } else { "0" });
        secure.set(on);
    };

    view()
        .flex_1()
        .bg(move || theme().background)
        .text_color(move || theme().foreground)
        .child(auto_open_notice(auto_open, move || {
            app.run(check_auto_open)
        }))
        .child(
            when(
                move || phase.get() == Phase::NoDisc,
                move || {
                    view()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .child(
                            text(move || t!("start.title"))
                                .text_2xl()
                                .font_weight(FontWeight::SEMIBOLD),
                        )
                        .child(
                            text(move || t!("start.subtitle"))
                                .text_color(move || theme().muted_foreground),
                        )
                        .child(when(
                            move || access_denied.get(),
                            move || {
                                text(move || t!("start.access_denied"))
                                    .text_center()
                                    .text_color(move || theme().destructive)
                            },
                        ))
                },
            )
            .otherwise(move || {
                view()
                    .flex_1()
                    .p_6()
                    .gap_4()
                    .child(header(disc, chosen))
                    .child(release_picker(disc, chosen, phase))
                    .child(track_list(rows))
                    .child(when(
                        move || message.get().is_some(),
                        move || {
                            text(move || message.get().unwrap_or_default())
                                .text_color(move || theme().destructive)
                        },
                    ))
                    .child(footer(
                        phase,
                        library,
                        secure,
                        choose_folder,
                        toggle_secure,
                        start_rip,
                        reveal,
                        eject,
                    ))
            }),
        )
}

fn header(disc: Signal<Option<Disc>>, chosen: Signal<usize>) -> impl IntoElement {
    let release = move || {
        disc.get()
            .and_then(|d| d.releases.get(chosen.get()).cloned())
    };
    let cover = move || {
        release()
            .map(|r| format!("https://coverartarchive.org/release/{}/front-250", r.id))
            .unwrap_or_default()
    };
    view()
        .flex_row()
        .gap_4()
        .items_center()
        .child(
            view()
                .size(px(96.0))
                .rounded(px(8.0))
                .overflow_hidden()
                .bg(move || theme().muted)
                .child(when(
                    move || release().is_some(),
                    move || {
                        image(cover)
                            .size_full()
                            .object_fit(nsh_core::ResizeMode::Cover)
                    },
                )),
        )
        .child(
            view()
                .flex_1()
                .gap_1()
                .child(
                    text(move || {
                        release()
                            .map(|r| r.title)
                            .unwrap_or_else(|| t!("album.unknown"))
                    })
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD),
                )
                .child(text(move || {
                    release()
                        .map(|r| r.artist)
                        .unwrap_or_else(|| t!("album.not_found"))
                }))
                .child(
                    text(move || release().map(|r| details(&r)).unwrap_or_default())
                        .text_sm()
                        .text_color(move || theme().muted_foreground),
                ),
        )
}

/// Only shown when MusicBrainz knows several releases with this disc.
fn release_picker(
    disc: Signal<Option<Disc>>,
    chosen: Signal<usize>,
    phase: Signal<Phase>,
) -> impl IntoElement {
    let options = move || disc.get().map(|d| d.releases).unwrap_or_default();
    when(
        move || options().len() > 1,
        move || {
            view().flex_row().flex_wrap().gap_2().child(each(
                move || options().into_iter().enumerate().collect::<Vec<_>>(),
                |(i, r)| (*i, r.id.clone()),
                move |item, _| {
                    let i = move || item.get().0;
                    button()
                        .px_3()
                        .py_1()
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(move || {
                            if chosen.get() == i() {
                                theme().primary
                            } else {
                                theme().border
                            }
                        })
                        .disabled(move || phase.get() != Phase::Ready)
                        .on_press(move || chosen.set(i()))
                        .child(text(move || details(&item.get().1)).text_sm())
                },
            ))
        },
    )
}

fn track_list(rows: Signal<Vec<Row>>) -> impl IntoElement {
    scroll().flex_1().child(view().gap_1().child(each(
        move || rows.get(),
        |r| r.number,
        move |row, _| {
            view()
                .flex_row()
                .items_center()
                .gap_3()
                .py_1()
                .child(
                    text(move || format!("{:2}", row.get().number))
                        .w(px(24.0))
                        .text_color(move || theme().muted_foreground),
                )
                .child(
                    view()
                        .flex_1()
                        .child(text(move || row.get().title).number_of_lines(1))
                        .child(
                            text(move || row.get().artist)
                                .text_sm()
                                .number_of_lines(1)
                                .text_color(move || theme().muted_foreground),
                        ),
                )
                .child(
                    text(move || row.get().length)
                        .text_sm()
                        .tabular_nums()
                        .text_color(move || theme().muted_foreground),
                )
                .child(
                    view()
                        .w(px(220.0))
                        .gap_1()
                        .child(text(move || row.get().status).text_sm())
                        .child(when(
                            move || !row.get().done && row.get().progress > 0.0,
                            move || {
                                progress()
                                    .value(move || row.get().progress)
                                    .track_style(|s| {
                                        s.h(px(4.0)).rounded(px(2.0)).bg(move || theme().muted)
                                    })
                                    .indicator_style(|s| {
                                        s.h(px(4.0)).rounded(px(2.0)).bg(move || theme().primary)
                                    })
                            },
                        )),
                )
        },
    )))
}

#[allow(clippy::too_many_arguments)]
fn footer(
    phase: Signal<Phase>,
    library: Signal<Option<String>>,
    secure: Signal<bool>,
    choose_folder: impl Fn() + Clone + 'static,
    toggle_secure: impl Fn() + Clone + 'static,
    start_rip: impl Fn() + Clone + 'static,
    reveal: impl Fn() + Clone + 'static,
    eject: impl Fn() + Clone + 'static,
) -> impl IntoElement {
    let verdict_text = move || match phase.get() {
        Phase::Done(_, text) => text,
        Phase::Failed(e) => t!("rip.failed", error = e),
        Phase::Ripping => t!("rip.ripping"),
        _ => String::new(),
    };
    let verdict_color = move || match phase.get() {
        Phase::Done(Verdict::Check, _) | Phase::Failed(_) => theme().destructive,
        Phase::Done(..) => theme().primary,
        _ => theme().muted_foreground,
    };
    view()
        .gap_3()
        .pt_3()
        .border_t_1()
        .border_color(move || theme().border)
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_3()
                .child(
                    text(verdict_text)
                        .flex_1()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(verdict_color),
                )
                .child(checkbox_button(
                    "setting.secure",
                    secure,
                    move || phase.get() == Phase::Ripping,
                    toggle_secure,
                )),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_3()
                .child(
                    text(move || library.get().unwrap_or_else(|| t!("library.none")))
                        .flex_1()
                        .text_sm()
                        .number_of_lines(1)
                        .text_color(move || theme().muted_foreground),
                )
                .child(secondary_button(
                    "library.choose",
                    move || phase.get() == Phase::Ripping,
                    choose_folder,
                ))
                .child(when(move || matches!(phase.get(), Phase::Done(..)), {
                    let reveal = reveal.clone();
                    move || secondary_button("action.show_in_finder", || false, reveal.clone())
                }))
                .child(when(
                    move || matches!(phase.get(), Phase::Done(..) | Phase::Failed(_)),
                    {
                        let eject = eject.clone();
                        move || secondary_button("action.eject", || false, eject.clone())
                    },
                ))
                .child(when(move || phase.get() == Phase::Ready, {
                    let start_rip = start_rip.clone();
                    move || {
                        let start_rip = start_rip.clone();
                        button()
                            .px_4()
                            .py_2()
                            .rounded(move || theme().radius)
                            .bg(move || theme().primary)
                            .disabled(move || library.get().is_none())
                            .opacity(move || {
                                if library.get().is_none() {
                                    0.4_f32
                                } else {
                                    1.0
                                }
                            })
                            .active(|s| s.opacity(0.8))
                            .on_press(start_rip)
                            .child(
                                text(move || t!("action.rip"))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(move || theme().primary_foreground),
                            )
                    }
                })),
        )
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
                .gap_3()
                .mx_6()
                .mt_4()
                .p_3()
                .rounded(move || theme().radius)
                .bg(move || theme().muted)
                .child(
                    text(move || match auto_open.get() {
                        Some(AutoOpen::OtherApp) => t!("notice.auto_open_other"),
                        _ => t!("notice.auto_open_music"),
                    })
                    .flex_1()
                    .text_sm(),
                )
                .child(secondary_button(
                    "notice.check_again",
                    || false,
                    check_again.clone(),
                ))
        },
    )
}

/// A setting that is on or off: a box that shows a tick, and its label.
fn checkbox_button(
    label: &'static str,
    on: Signal<bool>,
    disabled: impl Fn() -> bool + Clone + 'static,
    on_press: impl Fn() + 'static,
) -> impl IntoElement {
    let dimmed = disabled.clone();
    button()
        .flex_row()
        .items_center()
        .gap_2()
        .selected(move || on.get())
        .disabled(disabled)
        .opacity(move || if dimmed() { 0.4_f32 } else { 1.0 })
        .on_press(on_press)
        .child(
            view()
                .size(px(16.0))
                .rounded(px(4.0))
                .border_1()
                .border_color(move || {
                    if on.get() {
                        theme().primary
                    } else {
                        theme().border
                    }
                })
                .bg(move || {
                    if on.get() {
                        theme().primary
                    } else {
                        theme().background
                    }
                })
                .items_center()
                .justify_center()
                .child(
                    text(move || if on.get() { "✓" } else { "" })
                        .text_xs()
                        .text_color(move || theme().primary_foreground),
                ),
        )
        .child(text(move || t!(label)).text_sm())
}

fn secondary_button(
    label: &'static str,
    disabled: impl Fn() -> bool + Clone + 'static,
    on_press: impl Fn() + 'static,
) -> impl IntoElement {
    let dimmed = disabled.clone();
    button()
        .px_3()
        .py_2()
        .rounded(move || theme().radius)
        .border_1()
        .border_color(move || theme().border)
        .disabled(disabled)
        .opacity(move || if dimmed() { 0.4_f32 } else { 1.0 })
        .active(|s| s.opacity(0.7))
        .on_press(on_press)
        .child(text(move || t!(label)))
}

fn rows_for(disc: &Disc, release: Option<&Release>) -> Vec<Row> {
    let audio: Vec<_> = disc
        .toc
        .tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.is_audio)
        .collect();
    audio
        .iter()
        .enumerate()
        .map(|(n, &(i, t))| {
            let meta = release.and_then(|r| r.tracks.get(n));
            Row {
                number: t.number,
                title: meta
                    .map(|m| m.title.clone())
                    .unwrap_or_else(|| t!("track.untitled", n = t.number)),
                artist: meta.map(|m| m.artist.clone()).unwrap_or_default(),
                length: timestamp((disc.toc.track_end(i) - t.start) as usize)
                    .trim()
                    .to_owned(),
                progress: 0.0,
                status: String::new(),
                done: false,
            }
        })
        .collect()
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
