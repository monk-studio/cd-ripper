//! A disc made of per-track WAV files (`01.wav`, `02.wav`, ...), for exercising
//! the whole rip pipeline without a drive. Tracks are laid out back to back
//! from LBA 0, like a plain audio CD whose first track has no pregap.

use std::path::Path;

use anyhow::{Context, bail, ensure};

use super::CdDrive;
use crate::SECTOR_BYTES;
use crate::toc::{Toc, Track};

pub struct ImageDrive {
    toc: Toc,
    pcm: Vec<u8>,
}

impl ImageDrive {
    pub fn open(dir: &Path) -> anyhow::Result<Self> {
        let mut pcm = Vec::new();
        let mut tracks = Vec::new();
        for number in 1..=99u8 {
            let path = dir.join(format!("{number:02}.wav"));
            if !path.exists() {
                break;
            }
            let file = std::fs::read(&path)?;
            let data = wav_data(&file).with_context(|| format!("reading {}", path.display()))?;
            ensure!(
                data.len() % SECTOR_BYTES == 0,
                "{} isn't whole CD sectors",
                path.display()
            );
            tracks.push(Track {
                number,
                session: 1,
                start: (pcm.len() / SECTOR_BYTES) as u32,
                is_audio: true,
                pre_emphasis: false,
            });
            pcm.extend_from_slice(data);
        }
        ensure!(!tracks.is_empty(), "no 01.wav in {}", dir.display());
        let leadout = (pcm.len() / SECTOR_BYTES) as u32;
        Ok(Self {
            toc: Toc { tracks, leadout },
            pcm,
        })
    }
}

/// The PCM payload of a 16-bit stereo 44.1 kHz WAV file.
fn wav_data(file: &[u8]) -> anyhow::Result<&[u8]> {
    ensure!(
        file.len() >= 12 && &file[0..4] == b"RIFF" && &file[8..12] == b"WAVE",
        "not a WAV file"
    );
    let mut pos = 12;
    while pos + 8 <= file.len() {
        let id = &file[pos..pos + 4];
        let len = u32::from_le_bytes(file[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = &file[pos + 8..(pos + 8 + len).min(file.len())];
        if id == b"fmt " {
            ensure!(body.len() >= 16, "short fmt chunk");
            let channels = u16::from_le_bytes([body[2], body[3]]);
            let rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            let bits = u16::from_le_bytes([body[14], body[15]]);
            ensure!((channels, rate, bits) == (2, 44100, 16), "not CD audio");
        } else if id == b"data" {
            return Ok(body);
        }
        pos += 8 + len + len % 2;
    }
    bail!("no data chunk")
}

impl CdDrive for ImageDrive {
    fn read_toc(&mut self) -> anyhow::Result<Toc> {
        Ok(self.toc.clone())
    }

    fn read_audio(&mut self, lba: u32, buf: &mut [u8]) -> anyhow::Result<()> {
        let from = lba as usize * SECTOR_BYTES;
        let src = self
            .pcm
            .get(from..from + buf.len())
            .context("read past the end of the image")?;
        buf.copy_from_slice(src);
        Ok(())
    }

    fn model(&self) -> Option<String> {
        Some("disc image".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(pcm: &[u8]) -> Vec<u8> {
        let mut f = b"RIFF".to_vec();
        f.extend((36 + pcm.len() as u32).to_le_bytes());
        f.extend(b"WAVEfmt ");
        f.extend(16u32.to_le_bytes());
        f.extend([1, 0, 2, 0]);
        f.extend(44100u32.to_le_bytes());
        f.extend((44100u32 * 4).to_le_bytes());
        f.extend([4, 0, 16, 0]);
        f.extend(b"data");
        f.extend((pcm.len() as u32).to_le_bytes());
        f.extend(pcm);
        f
    }

    #[test]
    fn lays_tracks_out_back_to_back() {
        let dir = std::env::temp_dir().join(format!("cdr-image-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("01.wav"), wav(&[1; SECTOR_BYTES * 2])).unwrap();
        std::fs::write(dir.join("02.wav"), wav(&[2; SECTOR_BYTES])).unwrap();

        let mut drive = ImageDrive::open(&dir).unwrap();
        let toc = drive.read_toc().unwrap();
        assert_eq!(
            toc.tracks.iter().map(|t| t.start).collect::<Vec<_>>(),
            [0, 2]
        );
        assert_eq!(toc.leadout, 3);
        let mut buf = vec![0; SECTOR_BYTES];
        drive.read_audio(2, &mut buf).unwrap();
        assert!(buf.iter().all(|&b| b == 2));
        assert!(drive.read_audio(3, &mut buf).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
