//! CDR's desktop app (CDR-13 prototype): insert a CD, pick the release and
//! the library folder, rip. The work runs on a blocking thread and reports
//! back to the UI through a channel; signals only change on the UI thread.

use std::path::PathBuf;
use std::time::Duration;

use cdr_core::album::{self, Event};
use cdr_core::drive::CdDrive;
use cdr_core::library::DEFAULT_TEMPLATE;
use cdr_core::musicbrainz::{self, DiscToc, Release};
use cdr_core::report::{TrackReport, Verdict, timestamp};
use cdr_core::storage_keys;
use cdr_core::toc::Toc;
use nsh_base::{button, progress, theme};
use nsh_core::file_dialog::OpenDialog;
use nsh_core::prelude::*;
use nsh_core::storage::Storage;

#[native_module("cdr/native")]
fn native(registry: &mut Registry) {
    registry.register::<App>();
}

/// A disc read and looked up, ready to rip.
#[derive(Clone, PartialEq)]
struct Disc {
    device: String,
    toc: Toc,
    releases: Vec<Release>,
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
    let rows = signal(Vec::<Row>::new());
    let message = signal(None::<String>);
    // Tasks live as long as the UI they were started in. Handlers run inside
    // buttons that come and go (Rip disappears once ripping starts), so the
    // tasks they start belong to the app instead.
    let app = Owner::current();

    // Look for a disc every two seconds while there is none.
    spawn(async move {
        loop {
            if phase.get() == Phase::NoDisc {
                if let Ok(Ok(found)) = blocking(read_disc).await {
                    rows.set(rows_for(&found, found.releases.first()));
                    chosen.set(0);
                    disc.set(Some(found));
                    phase.set(Phase::Ready);
                }
            }
            timer(Duration::from_secs(2)).await;
        }
    });

    let release = move || disc.get().and_then(|d| d.releases.get(chosen.get()).cloned());

    let choose_folder = move || {
        let picked = pick_paths(OpenDialog::folders().button_label("Use as Library"));
        app.run(|| spawn(async move {
            if let Ok(Some(paths)) = picked.await {
                if let Some(path) = paths.into_iter().next() {
                    let path = path.to_string_lossy().into_owned();
                    let _ = Storage::set_item(storage_keys::LIBRARY, path.clone());
                    library.set(Some(path));
                }
            }
        }));
    };

    let start_rip = move || {
        let (Some(d), Some(root)) = (disc.get(), library.get()) else { return };
        let release = d.releases.get(chosen.get()).cloned();
        let disc_id = DiscToc::from_toc(&d.toc).disc_id();
        let count = d.toc.audio_tracks().count();
        let (tags, dests) =
            album::destinations(Some(&PathBuf::from(&root)), DEFAULT_TEMPLATE, release.as_ref(), &disc_id, count);
        if let Some(existing) = dests.iter().find(|p| p.exists()) {
            message.set(Some(format!("Already in the library: {}", existing.display())));
            return;
        }
        message.set(None);
        rows.set(rows_for(&d, release.as_ref()));
        phase.set(Phase::Ripping);

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
        let work = blocking(move || {
            let mut drive = open_drive(&d.device)?;
            let plan = album::Plan {
                toc: &d.toc,
                release: release.as_ref(),
                dests,
                tags,
                offset: None,
                secure: false,
            };
            album::rip(drive.as_mut(), plan, |event| {
                let _ = tx.send(event);
            })
        });
        app.run(|| spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    Event::Progress { track, done, total } => rows.update(|rows| {
                        let row = &mut rows[track];
                        row.progress = done as f32 * 100.0 / total.max(1) as f32;
                        row.status = format!("Reading {:.0}%", row.progress);
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
                    let (verdict, text) = report.verdict();
                    phase.set(Phase::Done(verdict, text));
                }
                Ok(Err(e)) => phase.set(Phase::Failed(format!("{e:#}"))),
                Err(e) => phase.set(Phase::Failed(e.to_string())),
            }
        }));
    };

    let reveal = move || {
        let Some(root) = library.get() else { return };
        let Some(d) = disc.get() else { return };
        let release = release();
        let disc_id = DiscToc::from_toc(&d.toc).disc_id();
        let count = d.toc.audio_tracks().count();
        let (_, dests) =
            album::destinations(Some(&PathBuf::from(root)), DEFAULT_TEMPLATE, release.as_ref(), &disc_id, count);
        if let Some(first) = dests.first() {
            let _ = reveal_path(first.clone());
        }
    };

    let next_disc = move || {
        disc.set(None);
        rows.set(Vec::new());
        message.set(None);
        phase.set(Phase::NoDisc);
    };

    view()
        .flex_1()
        .bg(move || theme().background)
        .text_color(move || theme().foreground)
        .child(
            when(move || phase.get() == Phase::NoDisc, move || {
                view()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(text("Insert an audio CD").text_2xl().font_weight(FontWeight::SEMIBOLD))
                    .child(text("CDR reads it, checks every track against AccurateRip and files it into your library.")
                        .text_color(move || theme().muted_foreground))
            })
            .otherwise(move || {
                view()
                    .flex_1()
                    .p_6()
                    .gap_4()
                    .child(header(disc, chosen))
                    .child(release_picker(disc, chosen, phase))
                    .child(track_list(rows))
                    .child(when(move || message.get().is_some(), move || {
                        text(move || message.get().unwrap_or_default()).text_color(move || theme().destructive)
                    }))
                    .child(footer(phase, library, choose_folder, start_rip, reveal, next_disc))
            }),
        )
}

fn header(disc: Signal<Option<Disc>>, chosen: Signal<usize>) -> impl IntoElement {
    let release = move || disc.get().and_then(|d| d.releases.get(chosen.get()).cloned());
    let cover = move || {
        release().map(|r| format!("https://coverartarchive.org/release/{}/front-250", r.id)).unwrap_or_default()
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
                .child(when(move || release().is_some(), move || image(cover).size_full().object_fit(nsh_core::ResizeMode::Cover))),
        )
        .child(
            view()
                .flex_1()
                .gap_1()
                .child(
                    text(move || release().map(|r| r.title).unwrap_or_else(|| "Unknown album".into()))
                        .text_xl()
                        .font_weight(FontWeight::SEMIBOLD),
                )
                .child(text(move || release().map(|r| r.artist).unwrap_or_else(|| "Not found on MusicBrainz".into())))
                .child(
                    text(move || release().map(|r| details(&r)).unwrap_or_default())
                        .text_sm()
                        .text_color(move || theme().muted_foreground),
                ),
        )
}

/// Only shown when MusicBrainz knows several releases with this disc.
fn release_picker(disc: Signal<Option<Disc>>, chosen: Signal<usize>, phase: Signal<Phase>) -> impl IntoElement {
    let options = move || disc.get().map(|d| d.releases).unwrap_or_default();
    when(move || options().len() > 1, move || {
        view()
            .flex_row()
            .flex_wrap()
            .gap_2()
            .child(each(
                move || options().into_iter().enumerate().collect::<Vec<_>>(),
                |(i, r)| (*i, r.id.clone()),
                move |item, _| {
                    let i = move || item.get().0;
                    button()
                        .px_3()
                        .py_1()
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(move || if chosen.get() == i() { theme().primary } else { theme().border })
                        .disabled(move || phase.get() != Phase::Ready)
                        .on_press(move || chosen.set(i()))
                        .child(text(move || details(&item.get().1)).text_sm())
                },
            ))
    })
}

fn track_list(rows: Signal<Vec<Row>>) -> impl IntoElement {
    scroll().flex_1().child(
        view().gap_1().child(each(
            move || rows.get(),
            |r| r.number,
            move |row, _| {
                view()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .py_1()
                    .child(text(move || format!("{:2}", row.get().number)).w(px(24.0)).text_color(move || theme().muted_foreground))
                    .child(
                        view()
                            .flex_1()
                            .child(text(move || row.get().title).number_of_lines(1))
                            .child(text(move || row.get().artist).text_sm().number_of_lines(1).text_color(move || theme().muted_foreground)),
                    )
                    .child(text(move || row.get().length).text_sm().tabular_nums().text_color(move || theme().muted_foreground))
                    .child(
                        view()
                            .w(px(220.0))
                            .gap_1()
                            .child(text(move || row.get().status).text_sm())
                            .child(when(move || !row.get().done && row.get().progress > 0.0, move || {
                                progress()
                                    .value(move || row.get().progress)
                                    .track_style(|s| s.h(px(4.0)).rounded(px(2.0)).bg(move || theme().muted))
                                    .indicator_style(|s| s.h(px(4.0)).rounded(px(2.0)).bg(move || theme().primary))
                            })),
                    )
            },
        )),
    )
}

fn footer(
    phase: Signal<Phase>,
    library: Signal<Option<String>>,
    choose_folder: impl Fn() + Clone + 'static,
    start_rip: impl Fn() + Clone + 'static,
    reveal: impl Fn() + Clone + 'static,
    next_disc: impl Fn() + Clone + 'static,
) -> impl IntoElement {
    let verdict_text = move || match phase.get() {
        Phase::Done(_, text) => text,
        Phase::Failed(e) => format!("Rip failed: {e}"),
        Phase::Ripping => "Ripping…".into(),
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
        .child(text(verdict_text).font_weight(FontWeight::MEDIUM).text_color(verdict_color))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_3()
                .child(
                    text(move || library.get().unwrap_or_else(|| "No library folder chosen".into()))
                        .flex_1()
                        .text_sm()
                        .number_of_lines(1)
                        .text_color(move || theme().muted_foreground),
                )
                .child(secondary_button("Choose Library…", move || phase.get() == Phase::Ripping, choose_folder))
                .child(when(move || matches!(phase.get(), Phase::Done(..)), {
                    let reveal = reveal.clone();
                    move || secondary_button("Show in Finder", || false, reveal.clone())
                }))
                .child(when(move || matches!(phase.get(), Phase::Done(..) | Phase::Failed(_)), {
                    let next_disc = next_disc.clone();
                    move || secondary_button("Next Disc", || false, next_disc.clone())
                }))
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
                            .opacity(move || if library.get().is_none() { 0.4_f32 } else { 1.0 })
                            .active(|s| s.opacity(0.8))
                            .on_press(move || start_rip())
                            .child(text("Rip").font_weight(FontWeight::MEDIUM).text_color(move || theme().primary_foreground))
                    }
                })),
        )
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
        .child(text(label))
}

/// Reads the TOC of the disc in the first drive and looks it up. Blocking.
fn read_disc() -> anyhow::Result<Disc> {
    #[cfg(target_os = "macos")]
    let device = cdr_core::drive::macos::MacDrive::find_device()?;
    #[cfg(not(target_os = "macos"))]
    let device: String = anyhow::bail!("no drive backend for this platform yet");
    let toc = open_drive(&device)?.read_toc()?;
    let releases = musicbrainz::lookup(&DiscToc::from_toc(&toc))?;
    Ok(Disc { device, toc, releases })
}

fn open_drive(device: &str) -> anyhow::Result<Box<dyn CdDrive>> {
    #[cfg(target_os = "macos")]
    return Ok(Box::new(cdr_core::drive::macos::MacDrive::open(device)?));
    #[cfg(not(target_os = "macos"))]
    anyhow::bail!("no drive backend for {device} on this platform yet")
}

fn rows_for(disc: &Disc, release: Option<&Release>) -> Vec<Row> {
    let audio: Vec<_> = disc.toc.tracks.iter().enumerate().filter(|(_, t)| t.is_audio).collect();
    audio
        .iter()
        .enumerate()
        .map(|(n, &(i, t))| {
            let meta = release.and_then(|r| r.tracks.get(n));
            Row {
                number: t.number,
                title: meta.map(|m| m.title.clone()).unwrap_or_else(|| format!("Track {}", t.number)),
                artist: meta.map(|m| m.artist.clone()).unwrap_or_default(),
                length: timestamp((disc.toc.track_end(i) - t.start) as usize).trim().to_owned(),
                progress: 0.0,
                status: String::new(),
                done: false,
            }
        })
        .collect()
}

fn details(r: &Release) -> String {
    [r.date.as_deref(), r.country.as_deref(), r.label.as_deref(), r.catalog_number.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
}

fn status(t: &TrackReport) -> String {
    match (t.accuraterip, t.suspicious.is_empty()) {
        (Some(m), _) => format!("✓ Accurate ({})", m.confidence),
        (None, true) => "✓ Read the same twice".into(),
        (None, false) => format!("⚠ Check at {}", t.suspicious[0].trim()),
    }
}
