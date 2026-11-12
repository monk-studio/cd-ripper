//! The platform boundary: everything above this trait is platform-independent.

use crate::toc::Toc;

pub trait CdDrive {
    fn read_toc(&mut self) -> anyhow::Result<Toc>;

    /// Reads `buf.len() / 2352` raw CD-DA sectors starting at `lba`, as
    /// little-endian interleaved 16-bit stereo samples.
    fn read_audio(&mut self, lba: u32, buf: &mut [u8]) -> anyhow::Result<()>;

    /// Like `read_audio`, also returning one flag per sector that is set when
    /// the drive reported C2 (uncorrectable) errors in it. `Ok(None)` means the
    /// drive can't report C2; the audio is still read.
    fn read_audio_c2(&mut self, lba: u32, buf: &mut [u8]) -> anyhow::Result<Option<Vec<bool>>> {
        self.read_audio(lba, buf).map(|()| None)
    }

    /// "VENDOR MODEL" as reported by the drive, if known.
    fn model(&self) -> Option<String> {
        None
    }
}

pub mod image;
#[cfg(target_os = "macos")]
pub mod macos;
