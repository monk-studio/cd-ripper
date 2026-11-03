/// Sectors between the end of the first session and the first track of the
/// second (11250 lead-out/lead-in + 150 pregap). Enhanced CDs put a data track there.
pub const SESSION_GAP_SECTORS: u32 = 11400;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub number: u8,
    pub session: u8,
    /// Start sector (LBA, 0 = first sector after the 2s lead-in).
    pub start: u32,
    pub is_audio: bool,
    pub pre_emphasis: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toc {
    pub tracks: Vec<Track>,
    /// LBA of the disc lead-out.
    pub leadout: u32,
}

impl Toc {
    pub fn audio_tracks(&self) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter(|t| t.is_audio)
    }

    /// End (exclusive LBA) of the given track, accounting for the session gap
    /// before a following session.
    pub fn track_end(&self, index: usize) -> u32 {
        let track = &self.tracks[index];
        match self.tracks.get(index + 1) {
            Some(next) if next.session != track.session => next.start - SESSION_GAP_SECTORS,
            Some(next) => next.start,
            None => self.leadout,
        }
    }

    /// End (exclusive LBA) of the audio area — the lead-out, or the start of
    /// the data session on an enhanced CD.
    pub fn audio_end(&self) -> u32 {
        let last_audio = self.tracks.iter().rposition(|t| t.is_audio).unwrap_or(0);
        self.track_end(last_audio)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Msf {
    pub minute: u8,
    pub second: u8,
    pub frame: u8,
}

impl Msf {
    /// MSF counts from the start of the lead-in, LBA from 2 seconds later.
    pub fn to_lba(self) -> u32 {
        (self.minute as u32 * 60 + self.second as u32) * 75 + self.frame as u32 - 150
    }
}

/// Parses an IOKit `CDTOC` (READ TOC format 2, full TOC) buffer.
///
/// Layout: big-endian u16 length, u8 first session, u8 last session, then
/// 11-byte descriptors (session, adr/control, tno, point, MSF, zero, PMSF).
pub fn parse_full_toc(buf: &[u8]) -> anyhow::Result<Toc> {
    anyhow::ensure!(buf.len() >= 4, "TOC buffer too short");
    let length = u16::from_be_bytes([buf[0], buf[1]]) as usize + 2;
    let end = length.min(buf.len());
    let mut tracks = Vec::new();
    let mut leadout = None;

    for d in buf[4..end].as_chunks::<11>().0 {
        let session = d[0];
        let adr = d[1] >> 4;
        let control = d[1] & 0x0f;
        let point = d[3];
        let p = Msf {
            minute: d[8],
            second: d[9],
            frame: d[10],
        };
        if adr != 1 {
            continue;
        }
        match point {
            1..=99 => tracks.push(Track {
                number: point,
                session,
                start: p.to_lba(),
                is_audio: control & 0x04 == 0,
                pre_emphasis: control & 0x04 == 0 && control & 0x01 != 0,
            }),
            // Each session has its own lead-out; the disc's is the last one.
            0xa2 => leadout = Some(leadout.map_or(p.to_lba(), |l: u32| l.max(p.to_lba()))),
            _ => {}
        }
    }

    tracks.sort_by_key(|t| t.number);
    tracks.dedup_by_key(|t| t.number);
    anyhow::ensure!(!tracks.is_empty(), "no tracks in TOC");
    let leadout = leadout.ok_or_else(|| anyhow::anyhow!("no lead-out in TOC"))?;
    Ok(Toc { tracks, leadout })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desc(session: u8, control: u8, point: u8, m: u8, s: u8, f: u8) -> [u8; 11] {
        [session, 0x10 | control, 0, point, 0, 0, 0, 0, m, s, f]
    }

    fn toc_buf(descs: &[[u8; 11]]) -> Vec<u8> {
        let len = (2 + descs.len() * 11) as u16;
        let mut buf = len.to_be_bytes().to_vec();
        buf.extend([1, descs.iter().map(|d| d[0]).max().unwrap()]);
        descs.iter().for_each(|d| buf.extend(d));
        buf
    }

    #[test]
    fn parses_audio_cd() {
        let buf = toc_buf(&[
            desc(1, 0, 0xa0, 1, 0, 0),
            desc(1, 0, 0xa1, 2, 0, 0),
            desc(1, 0, 0xa2, 10, 0, 0),
            desc(1, 0, 1, 0, 2, 0),
            desc(1, 0, 2, 4, 0, 0),
        ]);
        let toc = parse_full_toc(&buf).unwrap();
        assert_eq!(toc.tracks.len(), 2);
        assert_eq!(toc.tracks[0].start, 0);
        assert_eq!(toc.tracks[1].start, 4 * 60 * 75 - 150);
        assert_eq!(toc.leadout, 10 * 60 * 75 - 150);
        assert_eq!(toc.audio_end(), toc.leadout);
    }

    #[test]
    fn enhanced_cd_audio_ends_before_session_gap() {
        let buf = toc_buf(&[
            desc(1, 0, 0xa2, 30, 0, 0),
            desc(1, 0, 1, 0, 2, 0),
            desc(2, 0x04, 0xa2, 40, 0, 0),
            desc(2, 0x04, 2, 33, 0, 0),
        ]);
        let toc = parse_full_toc(&buf).unwrap();
        assert!(!toc.tracks[1].is_audio);
        assert_eq!(toc.leadout, 40 * 60 * 75 - 150);
        assert_eq!(toc.audio_end(), toc.tracks[1].start - SESSION_GAP_SECTORS);
    }
}
