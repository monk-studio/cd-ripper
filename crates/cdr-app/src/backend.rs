//! What the app needs from the outside world — the drive, MusicBrainz,
//! the rip pipeline — behind one trait, so headless tests can stand in a
//! fake disc and a fake rip.

use std::path::PathBuf;
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
}

pub trait Backend: Send + Sync {
    /// The disc in the first drive, looked up. Blocking; an error means no
    /// disc (yet) or a failed lookup.
    fn read_disc(&self) -> anyhow::Result<Disc>;

    /// Rips `disc` as `request` says. Blocking; `on_event` reports progress.
    fn rip(
        &self,
        disc: &Disc,
        request: RipRequest,
        on_event: &mut dyn FnMut(Event),
    ) -> anyhow::Result<RipReport>;

    /// Ejects `disc` from its drive. Blocking.
    fn eject(&self, disc: &Disc) -> anyhow::Result<()>;
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
            offset: None,
            secure: request.secure,
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
