pub mod accuraterip;
pub mod drive;
pub mod toc;
pub mod wav;

/// Bytes of audio in one CD sector (588 stereo 16-bit samples).
pub const SECTOR_BYTES: usize = 2352;
/// Stereo samples in one CD sector.
pub const SECTOR_SAMPLES: usize = 588;
