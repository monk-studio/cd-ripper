//! Reading tracks accurately, one track in memory at a time.
//!
//! Each track is first read once at full speed. If AccurateRip confirms it,
//! that's the rip. Otherwise the track is read again after flushing the
//! drive's cache, and every sector where the two reads differ, the drive
//! reported C2 errors, or the read failed, is re-read until two clean reads
//! agree. Sectors that never settle are reported as suspicious.

use std::collections::HashMap;

use crate::accuraterip::{self, Checksums, OFFSET_CRC_SECTOR, OFFSET_SEARCH, Submission};
use crate::drive::CdDrive;
use crate::toc::Toc;
use crate::{SECTOR_BYTES, SECTOR_SAMPLES};

/// Sectors read elsewhere on the disc to push a region out of the drive's
/// cache. Audio caches are rarely over 4 MB (~1780 sectors).
pub const FLUSH_SECTORS: u32 = 2048;
/// Rounds of re-reading unsettled sectors before calling them suspicious.
pub const REREAD_ROUNDS: u32 = 8;
/// Rounds after which the rest are read at the drive's slowest speed:
/// damaged spots often read cleanly when the disc turns slowly (XLD does
/// the same).
pub const SLOW_AFTER_ROUNDS: u32 = REREAD_ROUNDS / 2;
const SECTORS_PER_READ: u32 = 32;

pub struct Ripper<'a> {
    drive: &'a mut dyn CdDrive,
    toc: &'a Toc,
    /// The drive's read offset in samples: corrected sample `n` is raw
    /// sample `n + offset`. Output always uses it.
    pub offset: i32,
    /// Offsets at which this disc matched other pressings in AccurateRip.
    /// Only for checking: a track that matches at one of them is accurate,
    /// but its audio stays aligned to the drive offset.
    pub pressing_offsets: Vec<i32>,
    pub flush_sectors: u32,
    /// Read every track twice, even when AccurateRip confirms the first read.
    pub always_secure: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackRip {
    pub samples: Vec<u32>,
    pub checksums: Checksums,
    /// AccurateRip confidence and checksum version, if it matched.
    pub accurate: Option<(u32, &'static str)>,
    /// When it matched another pressing: the offset that pressing lines up at.
    pub pressing_offset: Option<i32>,
    /// Full reads of the track: 1 when AccurateRip matched the first, else 2.
    pub passes: u32,
    /// Sectors that needed re-reading after the full passes.
    pub reread_sectors: u32,
    /// Track-relative sample positions of sectors that never read the same
    /// way twice (or couldn't be read at all).
    pub suspicious: Vec<usize>,
    /// Whether the drive reported C2 errors for this read (None: no C2 support).
    pub c2: Option<bool>,
    /// Whether some sectors had to be re-read with the drive slowed down.
    pub slowed_down: bool,
}

/// One read of a run of sectors.
struct Reading {
    pcm: Vec<u8>,
    /// Per sector: true if the drive flagged C2 errors or the read failed.
    bad: Vec<bool>,
    c2_supported: bool,
}

impl<'a> Ripper<'a> {
    pub fn new(drive: &'a mut dyn CdDrive, toc: &'a Toc, offset: i32) -> Self {
        Self {
            drive,
            toc,
            offset,
            pressing_offsets: Vec::new(),
            flush_sectors: FLUSH_SECTORS,
            always_secure: false,
        }
    }

    /// Finds the drive's read offset from AccurateRip's sector-450 checksums,
    /// reading only a few sectors around sector 450 of each track until one matches.
    pub fn detect_offset(&mut self, subs: &[Submission]) -> Option<i32> {
        let margin = (OFFSET_SEARCH as u32).div_ceil(SECTOR_SAMPLES as u32) + 1;
        for (n, i) in audio_tracks(self.toc) {
            let start = self.toc.tracks[i].start;
            if self.toc.track_end(i) < start + OFFSET_CRC_SECTOR + margin + 1 {
                continue;
            }
            let known = accuraterip::offset_crcs(subs, n);
            if known.is_empty() {
                continue;
            }
            let lo = (start + OFFSET_CRC_SECTOR - margin) as i64;
            let reading = self.read(lo, lo + 2 * margin as i64 + 1);
            let stream = accuraterip::samples(&reading.pcm);
            if let Some(offset) =
                accuraterip::detect_offset(&stream, margin as usize * SECTOR_SAMPLES, &known)
            {
                return Some(offset);
            }
        }
        None
    }

    /// Rips audio track `n` (0-based among audio tracks), verifying against
    /// `db` when the disc is in AccurateRip. `progress` gets (sectors done, total).
    pub fn rip_track(
        &mut self,
        n: usize,
        db: Option<&[Submission]>,
        mut progress: impl FnMut(u32, u32),
    ) -> TrackRip {
        let audio = audio_tracks(self.toc);
        let (_, i) = audio[n];
        let (first, last) = (n == 0, n == audio.len() - 1);
        let start = self.toc.tracks[i].start as i64 * SECTOR_SAMPLES as i64;
        let len = (self.toc.track_end(i) - self.toc.tracks[i].start) as usize * SECTOR_SAMPLES;
        let raw_start = start + self.offset as i64;
        // Extra sectors either side, so other pressings' alignments can be checked too.
        let reach = self
            .pressing_offsets
            .iter()
            .map(|p| (p - self.offset).unsigned_abs())
            .max();
        let margin = reach.unwrap_or(0).div_ceil(SECTOR_SAMPLES as u32) as i64;
        let lo = raw_start.div_euclid(SECTOR_SAMPLES as i64) - margin;
        let hi = (raw_start + len as i64 + SECTOR_SAMPLES as i64 - 1)
            .div_euclid(SECTOR_SAMPLES as i64)
            + margin;
        let count = (hi - lo) as u32;
        let skip = (raw_start - lo * SECTOR_SAMPLES as i64) as usize;

        let extract_at = |pcm: &[u8], shift: i64| {
            let from = (skip as i64 + shift) as usize;
            accuraterip::samples(&pcm[from * 4..(from + len) * 4])
        };
        let extract = |pcm: &[u8]| extract_at(pcm, 0);
        let pressings = self.pressing_offsets.clone();
        let drive_offset = self.offset;
        // Checksums of the output (drive offset); a match there, else at another pressing's alignment.
        let verify_pcm = |pcm: &[u8]| {
            let sums = accuraterip::checksums(&extract(pcm), first, last);
            let Some(subs) = db else {
                return (sums, None, None);
            };
            if let Some(m) = accuraterip::best_match(subs, n, sums) {
                return (sums, Some(m), None);
            }
            for &p in &pressings {
                let alt = accuraterip::checksums(
                    &extract_at(pcm, (p - drive_offset) as i64),
                    first,
                    last,
                );
                if let Some(m) = accuraterip::best_match(subs, n, alt) {
                    return (sums, Some(m), Some(p));
                }
            }
            (sums, None, None)
        };

        let first_pass = self.read_reporting(lo, hi, |d| progress(d, count * 2));
        let samples = extract(&first_pass.pcm);
        let (checksums, accurate, pressing_offset) = verify_pcm(&first_pass.pcm);
        let c2 = first_pass
            .c2_supported
            .then(|| first_pass.bad.iter().any(|&b| b));
        if accurate.is_some() && !self.always_secure {
            progress(count * 2, count * 2);
            return TrackRip {
                samples,
                checksums,
                accurate,
                pressing_offset,
                passes: 1,
                reread_sectors: 0,
                suspicious: Vec::new(),
                c2,
                slowed_down: false,
            };
        }

        // Secure mode: a second full read from the disc surface, not the cache.
        self.flush_cache(lo, hi);
        let second_pass = self.read_reporting(lo, hi, |d| progress(count + d, count * 2));
        let mut pcm = second_pass.pcm.clone();
        // Per unsettled sector: every reading so far (data, was it clean).
        let mut history: HashMap<usize, Vec<(Vec<u8>, bool)>> = HashMap::new();
        for k in 0..count as usize {
            let a = &first_pass.pcm[k * SECTOR_BYTES..(k + 1) * SECTOR_BYTES];
            let b = &second_pass.pcm[k * SECTOR_BYTES..(k + 1) * SECTOR_BYTES];
            if a != b || first_pass.bad[k] || second_pass.bad[k] {
                history.insert(
                    k,
                    vec![
                        (a.to_vec(), !first_pass.bad[k]),
                        (b.to_vec(), !second_pass.bad[k]),
                    ],
                );
            }
        }
        let reread_sectors = history.len() as u32;

        let mut slowed_down = false;
        for round in 0..REREAD_ROUNDS {
            if history.is_empty() {
                break;
            }
            let mut pending: Vec<usize> = history.keys().copied().collect();
            pending.sort_unstable();
            // Flush at full speed, then slow down just for the sectors themselves.
            self.flush_cache(
                lo + pending[0] as i64,
                lo + *pending.last().unwrap() as i64 + 1,
            );
            let slow = round >= SLOW_AFTER_ROUNDS;
            if slow {
                slowed_down |= self.drive.set_speed(Some(1)).is_ok();
            }
            for run in runs(&pending) {
                let reading = self.read(lo + run.start as i64, lo + run.end as i64);
                for k in run.clone() {
                    let j = k - run.start;
                    let data = &reading.pcm[j * SECTOR_BYTES..(j + 1) * SECTOR_BYTES];
                    let clean = !reading.bad[j];
                    let readings = history.get_mut(&k).unwrap();
                    // Settled: a clean read identical to an earlier read.
                    if clean && readings.iter().any(|(d, _)| d == data) {
                        pcm[k * SECTOR_BYTES..(k + 1) * SECTOR_BYTES].copy_from_slice(data);
                        history.remove(&k);
                    } else {
                        readings.push((data.to_vec(), clean));
                    }
                }
            }
            if slow {
                let _ = self.drive.set_speed(None);
            }
        }

        // Whatever is left: keep the reading seen most often, preferring clean ones.
        let mut suspicious = Vec::new();
        for (k, readings) in &history {
            let best = readings
                .iter()
                .max_by_key(|(d, clean)| (readings.iter().filter(|(o, _)| o == d).count(), *clean))
                .unwrap();
            pcm[k * SECTOR_BYTES..(k + 1) * SECTOR_BYTES].copy_from_slice(&best.0);
            suspicious.push(
                (*k * SECTOR_SAMPLES)
                    .saturating_sub(skip)
                    .min(len.saturating_sub(1)),
            );
        }
        suspicious.sort_unstable();
        suspicious.dedup();

        let samples = extract(&pcm);
        let (checksums, accurate, pressing_offset) = verify_pcm(&pcm);
        let c2 = (first_pass.c2_supported || second_pass.c2_supported)
            .then(|| first_pass.bad.iter().chain(&second_pass.bad).any(|&b| b));
        TrackRip {
            samples,
            checksums,
            accurate,
            pressing_offset,
            passes: 2,
            reread_sectors,
            suspicious,
            c2,
            slowed_down,
        }
    }

    fn read_reporting(&mut self, lo: i64, hi: i64, mut progress: impl FnMut(u32)) -> Reading {
        let mut out = Reading {
            pcm: Vec::new(),
            bad: Vec::new(),
            c2_supported: false,
        };
        let mut at = lo;
        while at < hi {
            let to = (at + SECTORS_PER_READ as i64).min(hi);
            let r = self.read(at, to);
            out.pcm.extend(r.pcm);
            out.bad.extend(r.bad);
            out.c2_supported |= r.c2_supported;
            at = to;
            progress((at - lo) as u32);
        }
        out
    }

    /// Reads sectors `[lo, hi)`. Sectors outside the audio area (lead-in,
    /// lead-out) can't be read and come back as silence, not flagged.
    fn read(&mut self, lo: i64, hi: i64) -> Reading {
        let n = (hi - lo) as usize;
        let mut out = Reading {
            pcm: vec![0; n * SECTOR_BYTES],
            bad: vec![false; n],
            c2_supported: false,
        };
        let from = lo.max(0);
        let to = hi.min(self.toc.audio_end() as i64);
        let mut at = from;
        while at < to {
            let end = (at + SECTORS_PER_READ as i64).min(to);
            let k = (at - lo) as usize;
            let buf = &mut out.pcm[k * SECTOR_BYTES..(k + (end - at) as usize) * SECTOR_BYTES];
            match self.drive.read_audio_c2(at as u32, buf) {
                Ok(flags) => {
                    if let Some(flags) = flags {
                        out.c2_supported = true;
                        out.bad[k..k + flags.len()].copy_from_slice(&flags);
                    }
                }
                // Isolate the failing sectors so the rest of the chunk survives.
                Err(_) => {
                    for s in at..end {
                        let j = (s - lo) as usize;
                        let buf = &mut out.pcm[j * SECTOR_BYTES..(j + 1) * SECTOR_BYTES];
                        match self.drive.read_audio_c2(s as u32, buf) {
                            Ok(Some(flags)) => {
                                out.c2_supported = true;
                                out.bad[j] = flags[0];
                            }
                            Ok(None) => {}
                            Err(_) => {
                                buf.fill(0);
                                out.bad[j] = true;
                            }
                        }
                    }
                }
            }
            at = end;
        }
        out
    }

    /// Reads `flush_sectors` far from `[lo, hi)` so re-reads come from the disc.
    fn flush_cache(&mut self, lo: i64, hi: i64) {
        let end = self.toc.audio_end() as i64;
        let size = (self.flush_sectors as i64).min(end);
        // Whichever end of the disc is further from the region.
        let from = if lo >= end - hi { 0 } else { end - size };
        let mut buf = vec![0; SECTORS_PER_READ as usize * SECTOR_BYTES];
        let mut at = from;
        while at < from + size {
            let n = (SECTORS_PER_READ as i64).min(from + size - at) as usize;
            let _ = self
                .drive
                .read_audio(at as u32, &mut buf[..n * SECTOR_BYTES]);
            at += n as i64;
        }
    }
}

/// (audio index, TOC index) of each audio track.
fn audio_tracks(toc: &Toc) -> Vec<(usize, usize)> {
    toc.tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.is_audio)
        .map(|(i, _)| i)
        .enumerate()
        .collect()
}

/// Groups sorted sector indices into contiguous ranges.
fn runs(sorted: &[usize]) -> Vec<std::ops::Range<usize>> {
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    for &k in sorted {
        match out.last_mut() {
            Some(r) if r.end == k => r.end += 1,
            _ => out.push(k..k + 1),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::accuraterip::TrackEntry;
    use crate::toc::Track;

    #[derive(Clone, Copy)]
    enum Fault {
        /// The first `n` physical reads return different wrong data.
        BadReads(u32),
        /// The first `n` physical reads return the same wrong data, C2-flagged.
        StickyC2(u32),
        /// Every physical read returns different wrong data.
        Unstable,
        /// Every read fails.
        Unreadable,
        /// Reads wrong, differently each time, unless the drive is at 1x.
        NeedsSlow,
    }

    /// A drive with a FIFO sector cache, so re-reads without a flush return
    /// whatever was read last, like real drives.
    struct FakeDrive {
        pcm: Vec<u8>,
        toc: Toc,
        faults: HashMap<u32, Fault>,
        physical_reads: HashMap<u32, u32>,
        cache: VecDeque<(u32, Vec<u8>, bool)>,
        cache_size: usize,
        c2: bool,
        /// Current speed (`None`: fastest) and every change asked for.
        speed: Option<u16>,
        speed_changes: Vec<Option<u16>>,
    }

    impl FakeDrive {
        fn new(pcm: Vec<u8>, toc: Toc) -> Self {
            Self {
                pcm,
                toc,
                faults: HashMap::new(),
                physical_reads: HashMap::new(),
                cache: VecDeque::new(),
                cache_size: 48,
                c2: false,
                speed: None,
                speed_changes: Vec::new(),
            }
        }

        fn sector(&mut self, lba: u32) -> anyhow::Result<(Vec<u8>, bool)> {
            if let Some((_, data, bad)) = self.cache.iter().find(|(l, _, _)| *l == lba) {
                return Ok((data.clone(), *bad));
            }
            let count = self.physical_reads.entry(lba).or_default();
            *count += 1;
            let count = *count;
            let truth =
                self.pcm[lba as usize * SECTOR_BYTES..(lba as usize + 1) * SECTOR_BYTES].to_vec();
            let garble = |seed: u32| {
                truth
                    .iter()
                    .map(|b| b ^ (seed as u8 | 1))
                    .collect::<Vec<_>>()
            };
            let (data, bad) = match self.faults.get(&lba) {
                Some(Fault::Unreadable) => anyhow::bail!("medium error at {lba}"),
                Some(Fault::BadReads(n)) if count <= *n => (garble(count * 2), true),
                Some(Fault::StickyC2(n)) if count <= *n => (garble(0x40), true),
                Some(Fault::Unstable) => (garble(count * 2), true),
                Some(Fault::NeedsSlow) if self.speed != Some(1) => (garble(count * 2), true),
                _ => (truth, false),
            };
            self.cache.push_back((lba, data.clone(), bad));
            if self.cache.len() > self.cache_size {
                self.cache.pop_front();
            }
            Ok((data, bad))
        }
    }

    impl CdDrive for FakeDrive {
        fn read_toc(&mut self) -> anyhow::Result<Toc> {
            Ok(self.toc.clone())
        }

        fn read_audio(&mut self, lba: u32, buf: &mut [u8]) -> anyhow::Result<()> {
            self.read_audio_c2(lba, buf).map(drop)
        }

        fn read_audio_c2(&mut self, lba: u32, buf: &mut [u8]) -> anyhow::Result<Option<Vec<bool>>> {
            let mut flags = Vec::new();
            for (k, out) in buf.as_chunks_mut::<SECTOR_BYTES>().0.iter_mut().enumerate() {
                let (data, bad) = self.sector(lba + k as u32)?;
                out.copy_from_slice(&data);
                flags.push(bad);
            }
            Ok(self.c2.then_some(flags))
        }

        fn set_speed(&mut self, speed: Option<u16>) -> anyhow::Result<()> {
            self.speed = speed;
            self.speed_changes.push(speed);
            Ok(())
        }
    }

    /// Four tracks; the long last one gives flushes somewhere far to read.
    fn disc() -> (Vec<u8>, Toc) {
        let starts = [0, 40, 80, 120];
        let tracks = starts
            .iter()
            .enumerate()
            .map(|(i, &start)| Track {
                number: i as u8 + 1,
                session: 1,
                start,
                is_audio: true,
                pre_emphasis: false,
            })
            .collect();
        let pcm = (0..300 * SECTOR_BYTES as u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        (
            pcm,
            Toc {
                tracks,
                leadout: 300,
            },
        )
    }

    fn truth(pcm: &[u8], from: u32, to: u32) -> Vec<u32> {
        accuraterip::samples(&pcm[from as usize * SECTOR_BYTES..to as usize * SECTOR_BYTES])
    }

    fn rip(drive: &mut FakeDrive, n: usize, db: Option<&[Submission]>) -> TrackRip {
        let toc = drive.toc.clone();
        let mut ripper = Ripper::new(drive, &toc, 0);
        ripper.flush_sectors = 64;
        ripper.rip_track(n, db, |_, _| {})
    }

    #[test]
    fn clean_track_not_in_database_is_read_twice() {
        let (pcm, toc) = disc();
        let mut drive = FakeDrive::new(pcm.clone(), toc);
        let r = rip(&mut drive, 1, None);
        assert_eq!(r.samples, truth(&pcm, 40, 80));
        assert_eq!((r.passes, r.reread_sectors), (2, 0));
        assert!(r.suspicious.is_empty());
        assert_eq!(
            drive.physical_reads[&50], 2,
            "second pass must come from the disc"
        );
        assert!(drive.speed_changes.is_empty());
    }

    #[test]
    fn accuraterip_match_skips_the_second_pass() {
        let (pcm, toc) = disc();
        let sums = accuraterip::checksums(&truth(&pcm, 40, 80), false, false);
        let entry = |crc| TrackEntry {
            confidence: 7,
            crc,
            offset_crc: 0,
        };
        let db = [Submission {
            tracks: vec![entry(0), entry(sums.v2), entry(0), entry(0)],
        }];
        let mut drive = FakeDrive::new(pcm, toc);
        let r = rip(&mut drive, 1, Some(&db));
        assert_eq!(r.accurate, Some((7, "v2")));
        assert_eq!(r.passes, 1);
        assert_eq!(drive.physical_reads[&50], 1);
    }

    #[test]
    fn always_secure_reads_twice_even_when_accurate() {
        let (pcm, toc) = disc();
        let sums = accuraterip::checksums(&truth(&pcm, 40, 80), false, false);
        let entry = |crc| TrackEntry {
            confidence: 7,
            crc,
            offset_crc: 0,
        };
        let db = [Submission {
            tracks: vec![entry(0), entry(sums.v2), entry(0), entry(0)],
        }];
        let mut drive = FakeDrive::new(pcm, toc.clone());
        let mut ripper = Ripper::new(&mut drive, &toc, 0);
        ripper.flush_sectors = 64;
        ripper.always_secure = true;
        let r = ripper.rip_track(1, Some(&db), |_, _| {});
        assert_eq!((r.accurate, r.passes), (Some((7, "v2")), 2));
        assert!(r.suspicious.is_empty());
    }

    #[test]
    fn other_pressing_is_accurate_but_audio_keeps_the_drive_offset() {
        // AccurateRip's pressing lines up 42 samples later than this disc.
        let (pcm, toc) = disc();
        let all = accuraterip::samples(&pcm);
        let at = 40 * SECTOR_SAMPLES + 42;
        let theirs = accuraterip::checksums(&all[at..at + 40 * SECTOR_SAMPLES], false, false);
        let entry = |crc| TrackEntry {
            confidence: 9,
            crc,
            offset_crc: 0,
        };
        let db = [Submission {
            tracks: vec![entry(0), entry(theirs.v2), entry(0), entry(0)],
        }];

        let mut drive = FakeDrive::new(pcm.clone(), toc.clone());
        let mut ripper = Ripper::new(&mut drive, &toc, 0);
        ripper.flush_sectors = 64;
        ripper.pressing_offsets = vec![42];
        let r = ripper.rip_track(1, Some(&db), |_, _| {});
        assert_eq!(r.accurate, Some((9, "v2")));
        assert_eq!(r.pressing_offset, Some(42));
        assert_eq!(
            r.passes, 1,
            "a match at another pressing needs no second read"
        );
        assert_eq!(
            r.samples,
            truth(&pcm, 40, 80),
            "audio stays at the drive offset"
        );

        // Without knowing about that pressing, the track isn't confirmed.
        let mut drive = FakeDrive::new(pcm.clone(), toc.clone());
        let mut ripper = Ripper::new(&mut drive, &toc, 0);
        ripper.flush_sectors = 64;
        let r = ripper.rip_track(1, Some(&db), |_, _| {});
        assert_eq!((r.accurate, r.passes), (None, 2));
    }

    #[test]
    fn transient_error_is_caught_and_reread_from_the_disc() {
        let (pcm, toc) = disc();
        let mut drive = FakeDrive::new(pcm.clone(), toc);
        drive.faults.insert(45, Fault::BadReads(1));
        let r = rip(&mut drive, 1, None);
        assert_eq!(r.samples, truth(&pcm, 40, 80));
        assert_eq!(r.reread_sectors, 1);
        assert!(r.suspicious.is_empty());
        assert!(
            !r.slowed_down && drive.speed_changes.is_empty(),
            "one re-read fixed it; no need to slow down"
        );
    }

    #[test]
    fn without_a_cache_flush_the_error_would_slip_through() {
        // Guards the flush: the whole track fits in the cache, so a second
        // pass without one returns the same bad data and "agrees" with it.
        let (pcm, toc) = disc();
        let mut drive = FakeDrive::new(pcm.clone(), toc.clone());
        drive.faults.insert(45, Fault::BadReads(1));
        let mut ripper = Ripper::new(&mut drive, &toc, 0);
        ripper.flush_sectors = 0;
        let r = ripper.rip_track(1, None, |_, _| {});
        assert_ne!(r.samples, truth(&pcm, 40, 80));
    }

    #[test]
    fn identical_reads_with_c2_errors_are_not_trusted() {
        let (pcm, toc) = disc();
        let mut drive = FakeDrive::new(pcm.clone(), toc);
        drive.c2 = true;
        drive.faults.insert(45, Fault::StickyC2(3));
        let r = rip(&mut drive, 1, None);
        assert_eq!(r.samples, truth(&pcm, 40, 80));
        assert_eq!(r.c2, Some(true));
        assert!(r.suspicious.is_empty());
        assert!(
            drive.physical_reads[&45] >= 5,
            "needs two clean reads after three bad ones"
        );
    }

    #[test]
    fn unstable_sector_is_reported_where_it_is() {
        let (pcm, toc) = disc();
        let mut drive = FakeDrive::new(pcm.clone(), toc);
        drive.faults.insert(50, Fault::Unstable);
        let r = rip(&mut drive, 1, None);
        assert_eq!(r.suspicious, vec![10 * SECTOR_SAMPLES]);
        let expected = truth(&pcm, 40, 80);
        let bad = 10 * SECTOR_SAMPLES..11 * SECTOR_SAMPLES;
        assert_eq!(r.samples[..bad.start], expected[..bad.start]);
        assert_eq!(r.samples[bad.end..], expected[bad.end..]);
        assert_eq!(drive.physical_reads[&50], 2 + REREAD_ROUNDS);
        assert!(r.slowed_down);
        assert_eq!(drive.speed, None, "full speed restored");
    }

    #[test]
    fn sector_that_only_reads_slowly_is_fixed_at_1x() {
        let (pcm, toc) = disc();
        let mut drive = FakeDrive::new(pcm.clone(), toc);
        drive.faults.insert(45, Fault::NeedsSlow);
        let r = rip(&mut drive, 1, None);
        assert_eq!(r.samples, truth(&pcm, 40, 80));
        assert!(r.suspicious.is_empty());
        assert!(r.slowed_down);
        // Slow only for the rounds after SLOW_AFTER_ROUNDS, back to full speed after each.
        let slow_rounds = drive
            .speed_changes
            .iter()
            .filter(|s| **s == Some(1))
            .count() as u32;
        assert_eq!(slow_rounds, 2, "two clean reads at 1x settle it");
        assert_eq!(drive.speed, None, "full speed restored");
        assert_eq!(drive.physical_reads[&45], 2 + SLOW_AFTER_ROUNDS + 2);
    }

    #[test]
    fn unreadable_sector_doesnt_take_its_neighbours_down() {
        let (pcm, toc) = disc();
        let mut drive = FakeDrive::new(pcm.clone(), toc);
        drive.faults.insert(60, Fault::Unreadable);
        let r = rip(&mut drive, 1, None);
        assert_eq!(r.suspicious, vec![20 * SECTOR_SAMPLES]);
        let expected = truth(&pcm, 40, 80);
        assert_eq!(
            r.samples[..20 * SECTOR_SAMPLES],
            expected[..20 * SECTOR_SAMPLES]
        );
        assert_eq!(
            r.samples[21 * SECTOR_SAMPLES..],
            expected[21 * SECTOR_SAMPLES..]
        );
    }

    #[test]
    fn offset_is_detected_from_a_small_read_and_corrected() {
        // One 470-sector track; the drive delivers everything 30 samples late.
        let corrected: Vec<u32> = (0..470 * SECTOR_SAMPLES as u32)
            .map(|i| i.wrapping_mul(2_654_435_761))
            .collect();
        let mut raw = vec![0u32; 30];
        raw.extend(&corrected[..corrected.len() - 30]);
        let pcm: Vec<u8> = raw.iter().flat_map(|s| s.to_le_bytes()).collect();
        let track = Track {
            number: 1,
            session: 1,
            start: 0,
            is_audio: true,
            pre_emphasis: false,
        };
        let toc = Toc {
            tracks: vec![track],
            leadout: 470,
        };

        let at = OFFSET_CRC_SECTOR as usize * SECTOR_SAMPLES;
        let offset_crc = corrected[at..at + SECTOR_SAMPLES]
            .iter()
            .enumerate()
            .fold(0u32, |a, (j, &s)| {
                a.wrapping_add(s.wrapping_mul(j as u32 + 1))
            });
        let sums = accuraterip::checksums(&corrected, true, true);
        let db = [Submission {
            tracks: vec![TrackEntry {
                confidence: 3,
                crc: sums.v2,
                offset_crc,
            }],
        }];

        let mut drive = FakeDrive::new(pcm, toc.clone());
        let mut ripper = Ripper::new(&mut drive, &toc, 0);
        assert_eq!(ripper.detect_offset(&db), Some(30));
        ripper.offset = 30;
        let r = ripper.rip_track(0, Some(&db), |_, _| {});
        assert_eq!(r.accurate, Some((3, "v2")));
        assert_eq!(
            r.samples[..corrected.len() - 30],
            corrected[..corrected.len() - 30]
        );
        let total: u32 = drive.physical_reads.values().sum();
        assert!(
            total <= 470 + 20,
            "one pass plus the detection window, got {total}"
        );
    }

    #[test]
    fn runs_group_contiguous_sectors() {
        assert_eq!(runs(&[1, 2, 3, 7, 9, 10]), vec![1..4, 7..8, 9..11]);
    }
}
