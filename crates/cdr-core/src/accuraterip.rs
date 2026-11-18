//! AccurateRip disc IDs, checksums, database lookup and offset detection.
//!
//! Database access is regulated by its owner (Illustrate). Agreed terms
//! (CDR-2): free while the app is donation-funded; clients must identify
//! themselves in the User-Agent and cache lookups; submitting isn't possible.

use std::io::Read;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::toc::Toc;
use crate::{SECTOR_SAMPLES, USER_AGENT};

/// Samples skipped at the start of the first track and the end of the last one.
const FIRST_TRACK_SKIP: usize = 5 * SECTOR_SAMPLES - 1;
const LAST_TRACK_SKIP: usize = 5 * SECTOR_SAMPLES;
/// The offset-finding checksum covers one sector starting at sector 450 of a track.
pub const OFFSET_CRC_SECTOR: u32 = 450;
/// Offsets searched by `detect_offset`, in samples either way.
pub const OFFSET_SEARCH: i32 = 4 * SECTOR_SAMPLES as i32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscIds {
    pub audio_tracks: u32,
    pub id1: u32,
    pub id2: u32,
    pub cddb: u32,
}

impl DiscIds {
    // Verified only on plain audio CDs; enhanced-CD handling follows the
    // usual convention (the data track counts towards the CDDB id only).
    pub fn from_toc(toc: &Toc) -> Self {
        let mut id1 = 0u32;
        let mut id2 = 0u32;
        let mut audio = 0u32;
        let mut cddb_sum = 0u32;
        for t in &toc.tracks {
            if t.is_audio {
                audio += 1;
                id1 = id1.wrapping_add(t.start);
                id2 = id2.wrapping_add(t.start.max(1).wrapping_mul(t.number as u32));
            }
            cddb_sum += digit_sum(t.start / 75 + 2);
        }
        let end = toc.audio_end();
        id1 = id1.wrapping_add(end);
        id2 = id2.wrapping_add(end.wrapping_mul(audio + 1));
        let length = toc.leadout / 75 - toc.tracks[0].start / 75;
        let cddb = ((cddb_sum % 255) << 24) | (length << 8) | toc.tracks.len() as u32;
        Self {
            audio_tracks: audio,
            id1,
            id2,
            cddb,
        }
    }

    /// The disc's name in the database, e.g. `012-00121bef-00ac3084-a009cb0c`.
    pub fn name(&self) -> String {
        format!(
            "{:03}-{:08x}-{:08x}-{:08x}",
            self.audio_tracks, self.id1, self.id2, self.cddb
        )
    }

    pub fn url(&self) -> String {
        format!(
            "http://www.accuraterip.com/accuraterip/{:x}/{:x}/{:x}/dBAR-{}.bin",
            self.id1 & 0xf,
            self.id1 >> 4 & 0xf,
            self.id1 >> 8 & 0xf,
            self.name(),
        )
    }
}

fn digit_sum(mut n: u32) -> u32 {
    let mut sum = 0;
    while n > 0 {
        sum += n % 10;
        n /= 10;
    }
    sum
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackEntry {
    pub confidence: u8,
    /// v1 or v2 checksum — the database doesn't say which.
    pub crc: u32,
    /// Checksum of the sector at 450, for offset detection.
    pub offset_crc: u32,
}

/// One pressing's results; `tracks[i]` is audio track i.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    pub tracks: Vec<TrackEntry>,
}

pub fn parse_response(data: &[u8]) -> anyhow::Result<Vec<Submission>> {
    let u32_at = |p: usize| u32::from_le_bytes(data[p..p + 4].try_into().unwrap());
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        anyhow::ensure!(pos + 13 <= data.len(), "truncated AccurateRip header");
        let count = data[pos] as usize;
        pos += 13;
        anyhow::ensure!(pos + count * 9 <= data.len(), "truncated AccurateRip entry");
        let tracks = (0..count)
            .map(|i| {
                let p = pos + i * 9;
                TrackEntry {
                    confidence: data[p],
                    crc: u32_at(p + 1),
                    offset_crc: u32_at(p + 5),
                }
            })
            .collect();
        pos += count * 9;
        out.push(Submission { tracks });
    }
    Ok(out)
}

/// How long a disc's entry is reused before asking again. Submissions only
/// accumulate, so an older copy is still valid evidence, just less of it.
const FOUND_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 3600);
/// A disc that wasn't in the database may be added; ask again sooner.
const MISSING_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

/// The database entry for a disc; `Ok(None)` means the disc isn't in it.
/// With `cache` (a folder), a recent answer is reused instead of asking
/// again, as AccurateRip asks clients to; when asking fails, any earlier
/// answer is used.
pub fn fetch(ids: &DiscIds, cache: Option<&Path>) -> anyhow::Result<Option<Vec<Submission>>> {
    let raw = lookup_cached(ids, cache, SystemTime::now(), || download(ids))?;
    raw.map(|data| parse_response(&data)).transpose()
}

/// The raw entry from AccurateRip; `Ok(None)` when it has none (404).
fn download(ids: &DiscIds) -> anyhow::Result<Option<Vec<u8>>> {
    match ureq::get(&ids.url()).set("User-Agent", USER_AGENT).call() {
        Ok(resp) => {
            let mut data = Vec::new();
            resp.into_reader().read_to_end(&mut data)?;
            Ok(Some(data))
        }
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// `download`, through the cache in `dir`: `dBAR-<name>.bin` holds an entry
/// as AccurateRip sent it, `dBAR-<name>.missing` (empty) records that it had
/// none.
fn lookup_cached(
    ids: &DiscIds,
    dir: Option<&Path>,
    now: SystemTime,
    download: impl FnOnce() -> anyhow::Result<Option<Vec<u8>>>,
) -> anyhow::Result<Option<Vec<u8>>> {
    let Some(dir) = dir else {
        return download();
    };
    let found = dir.join(format!("dBAR-{}.bin", ids.name()));
    let missing = dir.join(format!("dBAR-{}.missing", ids.name()));
    let age = |path: &Path| {
        let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
        Some(now.duration_since(modified).unwrap_or_default())
    };
    let cached = || std::fs::read(&found).ok();
    if age(&found).is_some_and(|a| a < FOUND_MAX_AGE)
        && let Some(data) = cached()
    {
        return Ok(Some(data));
    }
    if age(&missing).is_some_and(|a| a < MISSING_MAX_AGE) {
        return Ok(None);
    }
    match download() {
        Ok(answer) => {
            // A cache that can't be written only costs a lookup next time.
            let _ = std::fs::create_dir_all(dir);
            match &answer {
                Some(data) => {
                    let _ = std::fs::write(&found, data);
                    let _ = std::fs::remove_file(&missing);
                }
                None => {
                    let _ = std::fs::write(&missing, b"");
                }
            }
            Ok(answer)
        }
        // Offline: an older entry is still valid evidence.
        Err(e) => cached().map(Some).ok_or(e),
    }
}

/// Interprets little-endian 16-bit stereo PCM as AccurateRip samples (left in the low half).
pub fn samples(pcm: &[u8]) -> Vec<u32> {
    pcm.as_chunks::<4>()
        .0
        .iter()
        .map(|&c| u32::from_le_bytes(c))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checksums {
    pub v1: u32,
    pub v2: u32,
}

pub fn checksums(track: &[u32], first: bool, last: bool) -> Checksums {
    let start = if first { FIRST_TRACK_SKIP } else { 0 };
    let end = if last {
        track.len().saturating_sub(LAST_TRACK_SKIP)
    } else {
        track.len()
    };
    let mut v1 = 0u32;
    let mut v2 = 0u32;
    for (i, &s) in track.iter().enumerate().take(end).skip(start) {
        let mult = i as u32 + 1;
        v1 = v1.wrapping_add(s.wrapping_mul(mult));
        let p = s as u64 * mult as u64;
        v2 = v2.wrapping_add(p as u32).wrapping_add((p >> 32) as u32);
    }
    Checksums { v1, v2 }
}

/// Searches `±OFFSET_SEARCH` samples around `sector_450` — where a track's
/// sector 450 would start if the drive had no offset — in an uncorrected
/// stream, for positions whose checksum is among `known` (checksum,
/// confidence). Returns the read offset with the most confidence behind it
/// (nearest to 0 on a tie): corrected sample `n` is `stream[n + offset]`.
///
/// A disc can match several pressings at different offsets; the first one
/// found is not necessarily the one most submissions agree on.
pub fn detect_offset(stream: &[u32], sector_450: usize, known: &[(u32, u32)]) -> Option<i32> {
    let base = sector_450 as i64;
    (-OFFSET_SEARCH..OFFSET_SEARCH)
        .filter_map(|off| {
            let from = base + off as i64;
            if from < 0 || from as usize + SECTOR_SAMPLES > stream.len() {
                return None;
            }
            let crc = stream[from as usize..from as usize + SECTOR_SAMPLES]
                .iter()
                .enumerate()
                .fold(0u32, |acc, (j, &s)| {
                    acc.wrapping_add(s.wrapping_mul(j as u32 + 1))
                });
            let confidence: u32 = known
                .iter()
                .filter(|(c, _)| crc != 0 && *c == crc)
                .map(|(_, n)| n)
                .sum();
            (confidence > 0).then_some((off, confidence))
        })
        .max_by_key(|&(off, confidence)| (confidence, std::cmp::Reverse(off.unsigned_abs())))
        .map(|(off, _)| off)
}

/// Highest confidence among database entries matching either checksum
/// version for audio track `track` (0-based), with the version that matched.
pub fn best_match(
    subs: &[Submission],
    track: usize,
    sums: Checksums,
) -> Option<(u32, &'static str)> {
    subs.iter()
        .filter_map(|s| s.tracks.get(track))
        .filter_map(|e| match e.crc {
            c if c == sums.v2 => Some((e.confidence as u32, "v2")),
            c if c == sums.v1 => Some((e.confidence as u32, "v1")),
            _ => None,
        })
        .max_by_key(|&(conf, _)| conf)
}

/// Sector-450 checksums the database knows for audio track `track`, with
/// each pressing's confidence.
pub fn offset_crcs(subs: &[Submission], track: usize) -> Vec<(u32, u32)> {
    subs.iter()
        .filter_map(|s| s.tracks.get(track))
        .filter(|e| e.offset_crc != 0)
        .map(|e| (e.offset_crc, e.confidence as u32))
        .collect()
}

/// Applies a read offset, zero-filling samples the drive couldn't deliver.
pub fn apply_offset(stream: &[u32], offset: i32) -> Vec<u32> {
    (0..stream.len() as i64)
        .map(|n| {
            let src = n + offset as i64;
            if src < 0 || src >= stream.len() as i64 {
                0
            } else {
                stream[src as usize]
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OFFSET_CRC_START: usize = OFFSET_CRC_SECTOR as usize * SECTOR_SAMPLES;
    use crate::toc::Track;

    fn audio(number: u8, start: u32) -> Track {
        Track {
            number,
            session: 1,
            start,
            is_audio: true,
            pre_emphasis: false,
        }
    }

    fn ids() -> DiscIds {
        DiscIds {
            audio_tracks: 3,
            id1: 0x1234,
            id2: 0x5678,
            cddb: 0x9abc,
        }
    }

    fn cache_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cdr-ar-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    const DAY: Duration = Duration::from_secs(24 * 3600);

    #[test]
    fn found_entries_are_reused_for_30_days() {
        let dir = cache_dir("found");
        let now = SystemTime::now();
        let got = lookup_cached(&ids(), Some(&dir), now, || Ok(Some(vec![1, 2])));
        assert_eq!(got.unwrap(), Some(vec![1, 2]));
        let again = lookup_cached(&ids(), Some(&dir), now + 29 * DAY, || {
            panic!("asked again within 30 days")
        });
        assert_eq!(again.unwrap(), Some(vec![1, 2]));
        let later = lookup_cached(&ids(), Some(&dir), now + 31 * DAY, || Ok(Some(vec![3])));
        assert_eq!(later.unwrap(), Some(vec![3]), "refreshed after 30 days");
    }

    #[test]
    fn missing_discs_are_asked_again_after_a_week() {
        let dir = cache_dir("missing");
        let now = SystemTime::now();
        assert_eq!(
            lookup_cached(&ids(), Some(&dir), now, || Ok(None)).unwrap(),
            None
        );
        let soon = lookup_cached(&ids(), Some(&dir), now + 6 * DAY, || panic!("asked again"));
        assert_eq!(soon.unwrap(), None);
        let later = lookup_cached(&ids(), Some(&dir), now + 8 * DAY, || Ok(Some(vec![7])));
        assert_eq!(later.unwrap(), Some(vec![7]));
        let next = lookup_cached(&ids(), Some(&dir), now + 9 * DAY, || panic!("asked again"));
        assert_eq!(
            next.unwrap(),
            Some(vec![7]),
            "the found entry replaces the miss"
        );
    }

    #[test]
    fn an_old_entry_stands_in_when_accuraterip_is_unreachable() {
        let dir = cache_dir("offline");
        let now = SystemTime::now();
        lookup_cached(&ids(), Some(&dir), now, || Ok(Some(vec![5]))).unwrap();
        let offline = lookup_cached(&ids(), Some(&dir), now + 90 * DAY, || {
            anyhow::bail!("no network")
        });
        assert_eq!(offline.unwrap(), Some(vec![5]));
        let fresh = cache_dir("offline-empty");
        assert!(lookup_cached(&ids(), Some(&fresh), now, || anyhow::bail!("no network")).is_err());
    }

    #[test]
    fn without_a_cache_it_always_asks() {
        let got = lookup_cached(&ids(), None, SystemTime::now(), || Ok(Some(vec![9])));
        assert_eq!(got.unwrap(), Some(vec![9]));
    }

    #[test]
    fn disc_ids_follow_definition() {
        let toc = Toc {
            tracks: vec![audio(1, 0), audio(2, 15000)],
            leadout: 30000,
        };
        let ids = DiscIds::from_toc(&toc);
        assert_eq!(ids.id1, 15000 + 30000);
        assert_eq!(ids.id2, 1 + 15000 * 2 + 30000 * 3);
        // digit sums of 2 and 202 → 2 + 4; length 400s; 2 tracks.
        assert_eq!(ids.cddb, (6 << 24) | (400 << 8) | 2);
        assert!(
            ids.url()
                .ends_with("8/c/f/dBAR-002-0000afc8-0001d4c1-06019002.bin"),
            "{}",
            ids.url()
        );
    }

    #[test]
    fn parses_response_chunks() {
        let mut data = vec![2u8];
        data.extend([0u8; 12]);
        for (conf, crc, off) in [(5u8, 0x11111111u32, 0xaaaa_aaaau32), (7, 0x22222222, 0)] {
            data.push(conf);
            data.extend(crc.to_le_bytes());
            data.extend(off.to_le_bytes());
        }
        let subs = parse_response(&data).unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(
            subs[0].tracks[1],
            TrackEntry {
                confidence: 7,
                crc: 0x22222222,
                offset_crc: 0
            }
        );
    }

    #[test]
    fn checksum_skips_edges_of_disc() {
        let track = vec![1u32; 10 * SECTOR_SAMPLES];
        let middle = checksums(&track, false, false);
        let n = track.len() as u32;
        assert_eq!(middle.v1, n * (n + 1) / 2);
        let first = checksums(&track, true, false);
        let skipped = FIRST_TRACK_SKIP as u32;
        assert_eq!(first.v1, middle.v1 - skipped * (skipped + 1) / 2);
    }

    #[test]
    fn v2_folds_high_bits() {
        let track = vec![0xffff_ffffu32, 0xffff_ffff];
        let c = checksums(&track, false, false);
        // 0xffffffff * 2 = 0x1_ffff_fffe → 0xfffffffe + 1
        assert_eq!(
            c.v2,
            0xffff_ffffu32.wrapping_add(0xffff_fffe).wrapping_add(1)
        );
    }

    #[test]
    fn detection_prefers_the_pressing_most_submissions_agree_on() {
        let truth: Vec<u32> = (0..500 * SECTOR_SAMPLES as u32)
            .map(|i| i.wrapping_mul(2654435761))
            .collect();
        let crc_at = |from: usize| {
            truth[from..from + SECTOR_SAMPLES]
                .iter()
                .enumerate()
                .fold(0u32, |a, (j, &s)| {
                    a.wrapping_add(s.wrapping_mul(j as u32 + 1))
                })
        };
        // A minority pressing 36 samples early, the majority 6 samples late.
        let known = [
            (crc_at(OFFSET_CRC_START - 36), 4),
            (crc_at(OFFSET_CRC_START + 6), 96),
        ];
        assert_eq!(detect_offset(&truth, OFFSET_CRC_START, &known), Some(6));
    }

    #[test]
    fn detects_and_applies_offset() {
        let truth: Vec<u32> = (0..500 * SECTOR_SAMPLES as u32)
            .map(|i| i.wrapping_mul(2654435761))
            .collect();
        let known_crc = truth[OFFSET_CRC_START..OFFSET_CRC_START + SECTOR_SAMPLES]
            .iter()
            .enumerate()
            .fold(0u32, |a, (j, &s)| {
                a.wrapping_add(s.wrapping_mul(j as u32 + 1))
            });
        // A drive that delivers everything 6 samples late.
        let read = apply_offset(&truth, -6);
        let offset = detect_offset(&read, OFFSET_CRC_START, &[(known_crc, 1)]).unwrap();
        assert_eq!(offset, 6);
        let fixed = apply_offset(&read, offset);
        assert_eq!(fixed[..fixed.len() - 6], truth[..truth.len() - 6]);
    }
}
