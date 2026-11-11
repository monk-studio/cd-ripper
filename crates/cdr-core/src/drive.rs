//! The platform boundary: everything above this trait is platform-independent.

use crate::toc::Toc;

pub trait CdDrive {
    fn read_toc(&mut self) -> anyhow::Result<Toc>;

    /// Reads `buf.len() / 2352` raw CD-DA sectors starting at `lba`, as
    /// little-endian interleaved 16-bit stereo samples.
    fn read_audio(&mut self, lba: u32, buf: &mut [u8]) -> anyhow::Result<()>;

    /// "VENDOR MODEL" as reported by the drive, if known.
    fn model(&self) -> Option<String> {
        None
    }
}

pub mod image;
#[cfg(target_os = "macos")]
pub mod macos;
