//! AccurateRip disc IDs, checksums, database lookup and offset detection.
//!
//! Database access is regulated by its owner (Illustrate). Agreed terms
//! (CDR-2): free while the app is donation-funded; clients must identify
//! themselves in the User-Agent and cache lookups; submitting isn't possible.

use std::io::Read;

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

    pub fn url(&self) -> String {
        format!(
            "http://www.accuraterip.com/accuraterip/{:x}/{:x}/{:x}/dBAR-{:03}-{:08x}-{:08x}-{:08x}.bin",
            self.id1 & 0xf,
            self.id1 >> 4 & 0xf,
            self.id1 >> 8 & 0xf,
            self.audio_tracks,
            self.id1,
            self.id2,
            self.cddb,
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

/// Fetches the database entry for a disc. `Ok(None)` means the disc isn't in it.
pub fn fetch(ids: &DiscIds) -> anyhow::Result<Option<Vec<Submission>>> {
    match ureq::get(&ids.url()).set("User-Agent", USER_AGENT).call() {
        Ok(resp) => {
            let mut data = Vec::new();
            resp.into_reader().read_to_end(&mut data)?;
            Ok(Some(parse_response(&data)?))
        }
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(e) => Err(e.into()),
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
/// stream, for a position whose checksum is in `known`. Returns the read
/// offset: corrected sample `n` is `stream[n + offset]`.
pub fn detect_offset(stream: &[u32], sector_450: usize, known: &[u32]) -> Option<i32> {
    let base = sector_450 as i64;
    (-OFFSET_SEARCH..OFFSET_SEARCH).find(|&off| {
        let from = base + off as i64;
        if from < 0 || from as usize + SECTOR_SAMPLES > stream.len() {
            return false;
        }
        let crc = stream[from as usize..from as usize + SECTOR_SAMPLES]
            .iter()
            .enumerate()
            .fold(0u32, |acc, (j, &s)| {
                acc.wrapping_add(s.wrapping_mul(j as u32 + 1))
            });
        crc != 0 && known.contains(&crc)
    })
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

/// Sector-450 checksums the database knows for audio track `track`.
pub fn offset_crcs(subs: &[Submission], track: usize) -> Vec<u32> {
    subs.iter()
        .filter_map(|s| s.tracks.get(track))
        .map(|e| e.offset_crc)
        .filter(|&c| c != 0)
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
        let offset = detect_offset(&read, OFFSET_CRC_START, &[known_crc]).unwrap();
        assert_eq!(offset, 6);
        let fixed = apply_offset(&read, offset);
        assert_eq!(fixed[..fixed.len() - 6], truth[..truth.len() - 6]);
    }
}
