pub mod accuraterip;
pub mod drive;
pub mod musicbrainz;
pub mod toc;
pub mod wav;

/// Bytes of audio in one CD sector (588 stereo 16-bit samples).
pub const SECTOR_BYTES: usize = 2352;
/// Stereo samples in one CD sector.
pub const SECTOR_SAMPLES: usize = 588;

/// Sent with every request to online databases, which ask clients to identify themselves.
pub const USER_AGENT: &str = concat!(
    "CDR/",
    env!("CARGO_PKG_VERSION"),
    " (+https://monk-studio.com)"
);
