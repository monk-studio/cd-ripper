//! The settings window (⌘,): the library and its file names, how discs are
//! read, and the Jellyfin server to tell about new albums. Everything is
//! saved as it changes; the API key goes to the Keychain, the rest to the
//! app's storage.

use cdr_core::library::{DEFAULT_TEMPLATE, track_path};
use cdr_core::storage_keys;
use cdr_core::tags::Tags;
use nsh_base::button;
use nsh_core::desktop::WindowKind;
use nsh_core::element::desktop::window;
use nsh_core::element::input;
use nsh_core::geometry::{Points, Size};
use nsh_core::prelude::*;
use nsh_core::storage::Storage;

use crate::backend::{Server, backend};
use crate::look::{color, icon, icons};
use crate::{checkbox_button, display_path, small_button};

/// What the user set, shared by the main window and the settings window.
#[derive(Clone, Copy)]
pub struct Settings {
    pub library: Signal<Option<String>>,
    /// As typed; empty means the default.
    pub template: Signal<String>,
    pub secure: Signal<bool>,
    /// As typed; empty means CDR finds the offset.
    pub offset: Signal<String>,
    pub jellyfin_url: Signal<String>,
    pub api_key: Signal<String>,
}

impl Settings {
    /// The settings as saved.
    pub fn load() -> Self {
        let stored = |key| Storage::get_item(key).unwrap_or_default();
        Settings {
            library: signal(Storage::get_item(storage_keys::LIBRARY)),
            template: signal(stored(storage_keys::TEMPLATE)),
            secure: signal(Storage::get_item(storage_keys::SECURE).as_deref() == Some("1")),
            offset: signal(stored(storage_keys::OFFSET)),
            jellyfin_url: signal(stored(storage_keys::JELLYFIN_URL)),
            api_key: signal(backend().api_key().unwrap_or_default()),
        }
    }

    /// The file-name template to use.
    pub fn template(&self) -> String {
        let t = self.template.get();
        if t.trim().is_empty() {
            DEFAULT_TEMPLATE.to_owned()
        } else {
            t.trim().to_owned()
        }
    }

    /// The read offset the user set, `Err` when what they typed isn't one.
    pub fn offset(&self) -> Result<Option<i32>, ()> {
        parse_offset(&self.offset.get())
    }

    /// The Jellyfin server, once both its address and key are set.
    pub fn server(&self) -> Option<Server> {
        let url = self.jellyfin_url.get().trim().to_owned();
        let api_key = self.api_key.get().trim().to_owned();
        (!url.is_empty() && !api_key.is_empty()).then_some(Server { url, api_key })
    }

    pub fn toggle_secure(&self) {
        let on = !self.secure.get();
        let _ = Storage::set_item(storage_keys::SECURE, if on { "1" } else { "0" });
        self.secure.set(on);
    }
}

/// Drives' read offsets are a few hundred samples at most; past ±3000 it is
/// a typo.
fn parse_offset(text: &str) -> Result<Option<i32>, ()> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    match text.trim_start_matches('+').parse::<i32>() {
        Ok(n) if n.abs() <= 3000 => Ok(Some(n)),
        _ => Err(()),
    }
}

/// Saves `value` under `key`, or forgets it when empty.
fn store(key: &str, value: &str) {
    if value.trim().is_empty() {
        Storage::remove_item(key);
    } else {
        let _ = Storage::set_item(key, value.to_owned());
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Library,
    Ripping,
    Servers,
}

impl Tab {
    fn label(self) -> String {
        match self {
            Tab::Library => t!("settings.library"),
            Tab::Ripping => t!("settings.ripping"),
            Tab::Servers => t!("settings.servers"),
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Tab::Library => icons::FOLDER,
            Tab::Ripping => icons::DISC_SMALL,
            Tab::Servers => icons::SERVER,
        }
    }
}

/// The settings window, open while `open` holds.
pub fn settings_window(
    open: Signal<bool>,
    settings: Settings,
    choose_folder: impl Fn() + Clone + 'static,
) -> impl IntoElement {
    let tab = signal(Tab::Library);
    window()
        .id("settings")
        .kind(WindowKind::Dialog)
        .title(move || tab.get().label())
        .default_size(Size::new(Points(560.0), Points(440.0)))
        .open(open)
        .on_open_change(move |e| open.set(e.open))
        .content(move || {
            let choose_folder = choose_folder.clone();
            view()
                .flex_1()
                .bg(color::WINDOW)
                .text_color(color::TEXT)
                .text_size(px(13.0))
                .child(tabs(tab))
                .child(
                    view()
                        .flex_1()
                        .px(px(28.0))
                        .py(px(20.0))
                        .gap(px(14.0))
                        .child(when(
                            move || tab.get() == Tab::Library,
                            move || library_tab(settings, choose_folder.clone()),
                        ))
                        .child(when(
                            move || tab.get() == Tab::Ripping,
                            move || ripping_tab(settings),
                        ))
                        .child(when(
                            move || tab.get() == Tab::Servers,
                            move || servers_tab(settings),
                        )),
                )
        })
}

fn tabs(tab: Signal<Tab>) -> impl IntoElement {
    let item = move |this: Tab| {
        let on = move || tab.get() == this;
        let tint = move || if on() { color::ACCENT } else { color::MUTED };
        button()
            .w(px(76.0))
            .h(px(46.0))
            .rounded(px(7.0))
            .items_center()
            .justify_center()
            .gap(px(3.0))
            .bg(move || {
                if on() {
                    color::CURRENT
                } else {
                    color::TRANSPARENT
                }
            })
            .selected(on)
            .active(|s| s.opacity(0.7))
            .on_press(move || tab.set(this))
            .child(icon(this.icon(), 18.0).text_color(tint))
            .child(
                text(move || this.label())
                    .text_size(px(11.0))
                    .text_color(tint),
            )
    };
    view()
        .flex_row()
        .justify_center()
        .gap(px(4.0))
        .py(px(8.0))
        .bg(color::BAR)
        .border_b_1()
        .border_color(color::BORDER)
        .child(item(Tab::Library))
        .child(item(Tab::Ripping))
        .child(item(Tab::Servers))
}

/// A labelled row: the label on the left, right-aligned, then `control` and
/// a hint under it.
fn row(
    label: impl Fn() -> String + 'static,
    control: impl IntoElement,
    hint: impl Fn() -> String + 'static,
    hint_color: impl Fn() -> AdaptiveColor + 'static,
) -> impl IntoElement {
    view()
        .flex_row()
        .items_start()
        .gap(px(14.0))
        .child(
            text(label)
                .w(px(130.0))
                .flex_shrink_0()
                .text_right()
                .pt(px(6.0)),
        )
        .child(
            view()
                .flex_1()
                .min_w(px(0.0))
                .gap(px(4.0))
                .child(control)
                .child(text(hint).text_size(px(11.5)).text_color(hint_color)),
        )
}

fn field() -> nsh_core::element::Input {
    input()
        .h(px(28.0))
        .px(px(9.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(color::BORDER)
        .bg(color::FIELD)
        .text_size(px(12.5))
}

fn muted() -> AdaptiveColor {
    color::MUTED
}

fn library_tab(settings: Settings, choose_folder: impl Fn() + 'static) -> impl IntoElement {
    let template = settings.template;
    view()
        .gap(px(14.0))
        .child(row(
            || t!("settings.library_folder"),
            view()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(
                    text(move || {
                        settings
                            .library
                            .get()
                            .map(|l| display_path(l.as_ref()))
                            .unwrap_or_else(|| t!("library.none"))
                    })
                    .flex_1()
                    .min_w(px(0.0))
                    .number_of_lines(1)
                    .text_size(px(12.5)),
                )
                .child(small_button(|| t!("settings.choose"), None, choose_folder)),
            || t!("settings.library_folder.hint"),
            muted,
        ))
        .child(row(
            || t!("settings.file_names"),
            field()
                .test_id("template")
                .font_family("monospace")
                .placeholder(DEFAULT_TEMPLATE.to_owned())
                .value(move || template.get())
                .on_change(move |text: &str| {
                    store(storage_keys::TEMPLATE, text);
                    template.set(text.to_owned());
                }),
            move || {
                t!(
                    "settings.file_names.hint",
                    example = example_path(&settings.template())
                )
            },
            muted,
        ))
}

/// Where a sample track would go with `template`.
fn example_path(template: &str) -> String {
    let mut tags = Tags::default();
    for (key, value) in [
        ("ALBUMARTIST", "Hitsujibungaku"),
        ("ARTIST", "Hitsujibungaku"),
        ("ALBUM", "12 hugs (like butterflies)"),
        ("DATE", "2023"),
        ("TRACKNUMBER", "1"),
        ("TITLE", "Hug.m4a"),
        ("DISCNUMBER", "1"),
        ("DISCTOTAL", "1"),
    ] {
        tags.add(key, value);
    }
    format!("{}.flac", track_path(template, &tags).display())
}

fn ripping_tab(settings: Settings) -> impl IntoElement {
    let offset = settings.offset;
    let bad = move || settings.offset().is_err();
    view()
        .gap(px(14.0))
        .child(row(
            || t!("settings.reading"),
            view().pt(px(5.0)).child(checkbox_button(
                "setting.secure",
                settings.secure,
                move || settings.toggle_secure(),
            )),
            || t!("settings.reading.hint"),
            muted,
        ))
        .child(row(
            || t!("settings.offset"),
            field()
                .test_id("offset")
                .w(px(120.0))
                .placeholder(t!("settings.offset.automatic"))
                .value(move || offset.get())
                .on_change(move |text: &str| {
                    if parse_offset(text).is_ok() {
                        store(storage_keys::OFFSET, text.trim());
                    }
                    offset.set(text.to_owned());
                }),
            move || {
                if bad() {
                    t!("settings.offset.invalid")
                } else {
                    t!("settings.offset.hint")
                }
            },
            move || if bad() { color::BAD } else { color::MUTED },
        ))
}

/// What checking the server found.
#[derive(Clone, PartialEq)]
enum Check {
    NotYet,
    Checking,
    Ok(Vec<String>),
    Failed(String),
}

fn servers_tab(settings: Settings) -> impl IntoElement {
    let url = settings.jellyfin_url;
    let key = settings.api_key;
    let check = signal(Check::NotYet);
    let app = Owner::current();
    let run_check = move || {
        let Some(server) = settings.server() else {
            check.set(Check::Failed(t!("settings.jellyfin.incomplete")));
            return;
        };
        check.set(Check::Checking);
        app.run(|| {
            spawn(async move {
                check.set(
                    match blocking(move || backend().check_server(&server)).await {
                        Ok(Ok(names)) => Check::Ok(names),
                        Ok(Err(e)) => Check::Failed(format!("{e:#}")),
                        Err(e) => Check::Failed(e.to_string()),
                    },
                );
            })
        });
    };
    view()
        .gap(px(14.0))
        .child(row(
            || "Jellyfin".to_owned(),
            field()
                .test_id("jellyfin-url")
                .placeholder("http://nas.local:8096".to_owned())
                .value(move || url.get())
                .on_change(move |text: &str| {
                    store(storage_keys::JELLYFIN_URL, text.trim());
                    url.set(text.to_owned());
                    check.set(Check::NotYet);
                }),
            || t!("settings.jellyfin.hint"),
            muted,
        ))
        .child(row(
            || t!("settings.api_key"),
            field()
                .test_id("jellyfin-key")
                .masked(true)
                .value(move || key.get())
                .on_change(move |text: &str| {
                    let saved = backend().set_api_key(Some(text.trim()).filter(|k| !k.is_empty()));
                    key.set(text.to_owned());
                    check.set(match saved {
                        Ok(()) => Check::NotYet,
                        Err(e) => Check::Failed(t!("settings.api_key.not_saved", error = e)),
                    });
                }),
            || t!("settings.api_key.hint"),
            muted,
        ))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(10.0))
                .pl(px(144.0))
                .child(small_button(
                    || t!("settings.jellyfin.check"),
                    None,
                    run_check,
                ))
                .child(
                    text(move || match check.get() {
                        Check::NotYet => String::new(),
                        Check::Checking => t!("settings.jellyfin.checking"),
                        Check::Ok(names) => {
                            t!("settings.jellyfin.connected", libraries = names.join(", "))
                        }
                        Check::Failed(e) => e,
                    })
                    .flex_1()
                    .min_w(px(0.0))
                    .text_size(px(12.0))
                    .text_color(move || match check.get() {
                        Check::Ok(_) => color::OK,
                        Check::Failed(_) => color::BAD,
                        _ => color::MUTED,
                    }),
                ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_offsets() {
        assert_eq!(parse_offset(""), Ok(None));
        assert_eq!(parse_offset(" +6 "), Ok(Some(6)));
        assert_eq!(parse_offset("-1164"), Ok(Some(-1164)));
        assert_eq!(parse_offset("6 samples"), Err(()));
        assert_eq!(parse_offset("30000"), Err(()));
    }
}
