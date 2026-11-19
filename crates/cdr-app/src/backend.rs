//! What the app needs from the outside world — the drive, MusicBrainz,
//! the rip pipeline — behind one trait, so headless tests can stand in a
//! fake disc and a fake rip.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use cdr_core::album::{self, Event};
use cdr_core::drive::CdDrive;
use cdr_core::musicbrainz::{self, DiscToc, Release};
use cdr_core::report::RipReport;
use cdr_core::tags::Tags;
use cdr_core::toc::Toc;

/// A disc read and looked up, ready to rip.
#[derive(Debug, Clone, PartialEq)]
pub struct Disc {
    pub device: String,
    pub toc: Toc,
    pub releases: Vec<Release>,
}

/// One rip, decided in the UI.
pub struct RipRequest {
    pub release: Option<Release>,
    pub dests: Vec<PathBuf>,
    pub tags: Vec<Tags>,
    pub secure: bool,
    /// The drive's read offset the user set; `None`: CDR finds it.
    pub offset: Option<i32>,
    /// Set to stop the rip; it then fails with [`album::Stopped`].
    pub stop: Arc<AtomicBool>,
    /// Told to rescan once the album is filed; the report says how it went.
    pub server: Option<Server>,
}

/// A Jellyfin server to tell about new albums.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub url: String,
    pub api_key: String,
}

pub trait Backend: Send + Sync {
    /// The disc in the first drive, looked up. Blocking; an error means no
    /// disc (yet) or a failed lookup.
    fn read_disc(&self) -> anyhow::Result<Disc>;

    /// The optical drive connected, disc or not, by model; `None` when there
    /// is none. Blocking.
    fn drive(&self) -> Option<String>;

    /// Rips `disc` as `request` says. Blocking; `on_event` reports progress.
    fn rip(
        &self,
        disc: &Disc,
        request: RipRequest,
        on_event: &mut dyn FnMut(Event),
    ) -> anyhow::Result<RipReport>;

    /// Ejects `disc` from its drive. Blocking.
    fn eject(&self, disc: &Disc) -> anyhow::Result<()>;

    /// The app the system opens when a music CD goes in, if any. It reads the
    /// disc at the same time as CDR and slows ripping down. Blocking.
    fn app_opened_on_insert(&self) -> Option<AutoOpen>;

    /// The Jellyfin API key kept in the Keychain, if any.
    fn api_key(&self) -> Option<String>;

    /// Keeps `key` in the Keychain, or removes it with `None`.
    fn set_api_key(&self, key: Option<&str>) -> anyhow::Result<()>;

    /// The server's music libraries: whether its address and key work.
    /// Blocking.
    fn check_server(&self, server: &Server) -> anyhow::Result<Vec<String>>;
}

/// The Keychain item the Jellyfin API key is kept in.
#[cfg(target_os = "macos")]
const KEYCHAIN_SERVICE: &str = "com.monk-studio.cdr";
#[cfg(target_os = "macos")]
const KEYCHAIN_ACCOUNT: &str = "jellyfin";

/// What the system opens when a music CD goes in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoOpen {
    Music,
    OtherApp,
}

/// Whether `e` is the system refusing access to the disc (on macOS, until the
/// user allows access to removable volumes).
pub fn is_access_denied(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
    })
}

/// The drive, MusicBrainz and AccurateRip.
pub struct Real;

impl Backend for Real {
    fn read_disc(&self) -> anyhow::Result<Disc> {
        let device = find_device()?;
        let toc = open_drive(&device)?.read_toc()?;
        let releases = musicbrainz::lookup(&DiscToc::from_toc(&toc))?;
        Ok(Disc {
            device,
            toc,
            releases,
        })
    }

    fn drive(&self) -> Option<String> {
        #[cfg(target_os = "macos")]
        return cdr_core::drive::macos::MacDrive::connected_drive();
        #[cfg(not(target_os = "macos"))]
        None
    }

    fn rip(
        &self,
        disc: &Disc,
        request: RipRequest,
        on_event: &mut dyn FnMut(Event),
    ) -> anyhow::Result<RipReport> {
        let mut drive = open_drive(&disc.device)?;
        let plan = album::Plan {
            toc: &disc.toc,
            release: request.release.as_ref(),
            dests: request.dests,
            tags: request.tags,
            offset: request.offset,
            secure: request.secure,
            stop: Some(request.stop),
            servers: request
                .server
                .map(|s| album::LibraryServer::Jellyfin {
                    url: s.url,
                    api_key: s.api_key,
                })
                .into_iter()
                .collect(),
            data_dir: cdr_core::offsets::data_dir(),
        };
        album::rip(drive.as_mut(), plan, on_event)
    }

    fn eject(&self, disc: &Disc) -> anyhow::Result<()> {
        #[cfg(target_os = "macos")]
        {
            // The disc's own device, so the right drive opens when there are several.
            let out = std::process::Command::new("diskutil")
                .args(["eject", &disc.device])
                .output()?;
            anyhow::ensure!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        anyhow::bail!("can't eject {} on this platform yet", disc.device)
    }

    fn app_opened_on_insert(&self) -> Option<AutoOpen> {
        #[cfg(target_os = "macos")]
        {
            // System Settings › CDs & DVDs › "When you insert a music CD".
            // Unset means the default, which opens Music; action 1 is Ignore.
            let out = std::process::Command::new("defaults")
                .args([
                    "read",
                    "com.apple.digihub",
                    "com.apple.digihub.cd.music.appeared",
                ])
                .output()
                .ok()?;
            let action = digihub_action(&String::from_utf8_lossy(&out.stdout));
            match action {
                Some(1) => None,
                Some(100 | 101) | None => Some(AutoOpen::Music),
                Some(_) => Some(AutoOpen::OtherApp),
            }
        }
        #[cfg(not(target_os = "macos"))]
        None
    }

    fn api_key(&self) -> Option<String> {
        #[cfg(target_os = "macos")]
        {
            let bytes = security_framework::passwords::get_generic_password(
                KEYCHAIN_SERVICE,
                KEYCHAIN_ACCOUNT,
            )
            .ok()?;
            String::from_utf8(bytes).ok().filter(|k| !k.is_empty())
        }
        #[cfg(not(target_os = "macos"))]
        None
    }

    fn set_api_key(&self, key: Option<&str>) -> anyhow::Result<()> {
        #[cfg(target_os = "macos")]
        {
            use security_framework::passwords;
            match key.filter(|k| !k.is_empty()) {
                Some(key) => passwords::set_generic_password(
                    KEYCHAIN_SERVICE,
                    KEYCHAIN_ACCOUNT,
                    key.as_bytes(),
                )?,
                None => {
                    // Nothing to remove is fine.
                    let _ = passwords::delete_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT);
                }
            }
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = key;
            anyhow::bail!("no keychain on this platform yet")
        }
    }

    fn check_server(&self, server: &Server) -> anyhow::Result<Vec<String>> {
        cdr_core::jellyfin::Jellyfin::new(&server.url, &server.api_key).music_libraries()
    }
}

static BACKEND: Mutex<Option<Arc<dyn Backend>>> = Mutex::new(None);

/// Replaces the real backend (for tests).
pub fn set_backend(backend: Arc<dyn Backend>) {
    *BACKEND.lock().unwrap() = Some(backend);
}

pub(crate) fn backend() -> Arc<dyn Backend> {
    BACKEND
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(|| Arc::new(Real))
}

fn find_device() -> anyhow::Result<String> {
    #[cfg(target_os = "macos")]
    return cdr_core::drive::macos::MacDrive::find_device();
    #[cfg(not(target_os = "macos"))]
    anyhow::bail!("no drive backend for this platform yet")
}

fn open_drive(device: &str) -> anyhow::Result<Box<dyn CdDrive>> {
    #[cfg(target_os = "macos")]
    return Ok(Box::new(cdr_core::drive::macos::MacDrive::open(device)?));
    #[cfg(not(target_os = "macos"))]
    anyhow::bail!("no drive backend for {device} on this platform yet")
}

/// The `action` in `defaults read com.apple.digihub
/// com.apple.digihub.cd.music.appeared`, which prints `{\n    action = 101;\n}`.
fn digihub_action(text: &str) -> Option<u32> {
    let rest = text.split("action =").nth(1)?;
    rest.split(';').next()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_digihub_action() {
        assert_eq!(digihub_action("{\n    action = 101;\n}\n"), Some(101));
        assert_eq!(digihub_action("{\n    action = 1;\n}\n"), Some(1));
        assert_eq!(digihub_action(""), None, "unset: the system default");
    }
}
