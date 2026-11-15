//! Drive read offsets: a property of the drive, not of each disc.
//!
//! A disc matching AccurateRip at some offset only says how *its pressing*
//! lines up with the pressing in the database; the same drive gave +6 on one
//! disc and −36 on another (CDR-9). So rips always use the drive's offset,
//! found in this order: set by hand, remembered for the drive, AccurateRip's
//! drive database, or learned from several discs that agree. Only when none
//! of those is known does one disc's match stand in for it.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::USER_AGENT;

const DRIVE_DB_URL: &str = "http://www.accuraterip.com/accuraterip/DriveOffsets.bin";
/// The drive database changes slowly; AccurateRip asks clients to cache.
const DRIVE_DB_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 3600);
/// Discs that must agree on an offset before it is learned for a drive.
pub const LEARN_AFTER_DISCS: u32 = 3;

/// Where a rip's offset came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OffsetSource {
    /// Given for this rip.
    Manual,
    /// From AccurateRip's drive database, by model.
    AccurateRipDrives,
    /// Agreed on by several earlier discs in this drive.
    Learned,
    /// This disc's AccurateRip match: the pressing's alignment, which may
    /// not be the drive's.
    ThisDisc,
}

impl OffsetSource {
    pub fn describe(self) -> &'static str {
        match self {
            OffsetSource::Manual => "set by hand",
            OffsetSource::AccurateRipDrives => "AccurateRip drive database",
            OffsetSource::Learned => "learned from earlier discs",
            OffsetSource::ThisDisc => "from this disc only (drive offset unknown)",
        }
    }
}

/// AccurateRip's `DriveOffsets.bin`: 69-byte records of a little-endian i16
/// offset, a 32-byte "VENDOR - MODEL" name and a submission count.
#[derive(Debug, Clone, Default)]
pub struct DriveDb(Vec<(String, i32, u8)>);

impl DriveDb {
    pub fn parse(data: &[u8]) -> Self {
        Self(
            data.as_chunks::<69>()
                .0
                .iter()
                .map(|r| {
                    let offset = i16::from_le_bytes([r[0], r[1]]) as i32;
                    let name = r[2..34].split(|&b| b == 0).next().unwrap_or(&[]);
                    (normalize(&String::from_utf8_lossy(name)), offset, r[35])
                })
                .collect(),
        )
    }

    /// The offset for a drive reported as "VENDOR MODEL", preferring the
    /// best-attested entry when a name appears more than once.
    pub fn lookup(&self, model: &str) -> Option<i32> {
        let want = normalize(model);
        self.0
            .iter()
            .filter(|(name, ..)| *name == want)
            .max_by_key(|(.., n)| *n)
            .map(|(_, o, _)| *o)
    }

    /// The database, from a cached copy in `cache` (a file) when fresh enough.
    pub fn load(cache: &Path) -> anyhow::Result<Self> {
        let fresh = std::fs::metadata(cache)
            .and_then(|m| m.modified())
            .is_ok_and(|t| {
                SystemTime::now().duration_since(t).unwrap_or_default() < DRIVE_DB_MAX_AGE
            });
        if !fresh {
            let resp = ureq::get(DRIVE_DB_URL)
                .set("User-Agent", USER_AGENT)
                .call()?;
            let mut data = Vec::new();
            resp.into_reader().read_to_end(&mut data)?;
            if let Some(dir) = cache.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(cache, &data)?;
            return Ok(Self::parse(&data));
        }
        Ok(Self::parse(&std::fs::read(cache)?))
    }
}

/// "HL-DT-ST - BD-RE BP71N" and "HL-DT-ST BD-RE BP71N" both become
/// "HL-DT-ST BD-RE BP71N": the database pads vendors and adds " - ".
fn normalize(name: &str) -> String {
    name.replace(" - ", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_uppercase()
}

/// What we know about one drive.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveRecord {
    /// The drive's offset once known, and where it came from.
    pub offset: Option<i32>,
    pub source: Option<OffsetSource>,
    /// How many discs matched AccurateRip at each offset.
    #[serde(default)]
    pub observed: BTreeMap<i32, u32>,
}

/// Remembered drives, kept as JSON (`drives.json` in the data folder).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveStore {
    #[serde(default)]
    pub drives: BTreeMap<String, DriveRecord>,
}

impl DriveStore {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|d| serde_json::from_slice(&d).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    /// The drive's offset if known, and its source.
    pub fn known(&self, model: &str) -> Option<(i32, OffsetSource)> {
        let r = self.drives.get(&normalize(model))?;
        Some((r.offset?, r.source?))
    }

    pub fn remember(&mut self, model: &str, offset: i32, source: OffsetSource) {
        let r = self.drives.entry(normalize(model)).or_default();
        r.offset = Some(offset);
        r.source = Some(source);
    }

    /// Records that a disc matched AccurateRip at `offset`. Once
    /// [`LEARN_AFTER_DISCS`] discs agree, and they outnumber every other
    /// offset seen combined, it becomes the drive's offset — unless one came
    /// from a better source already.
    pub fn observe(&mut self, model: &str, offset: i32) {
        let r = self.drives.entry(normalize(model)).or_default();
        *r.observed.entry(offset).or_default() += 1;
        if matches!(
            r.source,
            Some(OffsetSource::Manual | OffsetSource::AccurateRipDrives)
        ) {
            return;
        }
        let total: u32 = r.observed.values().sum();
        if let Some((&best, &n)) = r.observed.iter().max_by_key(|(_, n)| **n)
            && n >= LEARN_AFTER_DISCS
            && n * 2 > total
        {
            r.offset = Some(best);
            r.source = Some(OffsetSource::Learned);
        }
    }
}

/// Where CDR keeps its data: the drive store and cached databases.
pub fn data_dir() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    if cfg!(target_os = "macos") {
        return home.map(|h| h.join("Library/Application Support/CDR"));
    }
    if cfg!(target_os = "windows") {
        return std::env::var_os("APPDATA").map(|d| std::path::PathBuf::from(d).join("CDR"));
    }
    match std::env::var_os("XDG_DATA_HOME") {
        Some(d) => Some(std::path::PathBuf::from(d).join("cdr")),
        None => home.map(|h| h.join(".local/share/cdr")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str, offset: i16, submissions: u8) -> Vec<u8> {
        let mut r = offset.to_le_bytes().to_vec();
        let mut n = name.as_bytes().to_vec();
        n.resize(32, 0);
        r.extend(n);
        r.extend([0, submissions]);
        r.resize(69, 0);
        r
    }

    #[test]
    fn drive_db_matches_drutil_style_names() {
        let mut data = record("HL-DT-ST - BD-RE BP71N", 6, 84);
        data.extend(record("HL       - BP55EB40", 103, 3));
        data.extend(record("HL-DT-ST - BD-RE BP71N", 7, 2));
        let db = DriveDb::parse(&data);
        assert_eq!(
            db.lookup("HL-DT-ST BD-RE BP71N"),
            Some(6),
            "best-attested entry wins"
        );
        assert_eq!(db.lookup("HL BP55EB40"), Some(103));
        assert_eq!(db.lookup("Some Other Drive"), None);
    }

    #[test]
    fn learning_needs_agreement() {
        let mut s = DriveStore::default();
        s.observe("D", -36);
        s.observe("D", 6);
        s.observe("D", 6);
        assert_eq!(s.known("D"), None, "two discs aren't enough");
        s.observe("D", 6);
        assert_eq!(s.known("D"), Some((6, OffsetSource::Learned)));
        s.observe("D", -36);
        s.observe("D", -36);
        assert_eq!(
            s.known("D"),
            Some((6, OffsetSource::Learned)),
            "no majority, keep what we had"
        );
    }

    #[test]
    fn better_sources_are_not_overridden_by_discs() {
        let mut s = DriveStore::default();
        s.remember("D", 6, OffsetSource::AccurateRipDrives);
        for _ in 0..5 {
            s.observe("D", -36);
        }
        assert_eq!(s.known("D"), Some((6, OffsetSource::AccurateRipDrives)));
    }

    #[test]
    fn store_round_trips_through_json() {
        let path = std::env::temp_dir().join(format!("cdr-drives-{}.json", std::process::id()));
        let mut s = DriveStore::default();
        s.remember("HL-DT-ST BD-RE BP71N", 6, OffsetSource::AccurateRipDrives);
        s.observe("HL-DT-ST BD-RE BP71N", -36);
        s.save(&path).unwrap();
        assert_eq!(DriveStore::load(&path), s);
        std::fs::remove_file(&path).unwrap();
    }
}
