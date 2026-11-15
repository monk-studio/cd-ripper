pub mod accuraterip;
pub mod album;
pub mod coverart;
pub mod drive;
pub mod flac;
pub mod jellyfin;
pub mod library;
pub mod musicbrainz;
pub mod navidrome;
pub mod offsets;
pub mod report;
pub mod rip;
pub mod tags;
pub mod toc;

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

/// Names of the settings the apps remember between runs.
pub mod storage_keys {
    /// The music library folder rips are filed into.
    pub const LIBRARY: &str = "library";
    /// "1" to read every track twice (secure mode).
    pub const SECURE: &str = "secure";
}
