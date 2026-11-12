//! macOS drive access through the IOCDMediaBSDClient ioctls (see
//! IOKit/storage/IOCDMediaBSDClient.h), as XLD does; no SCSI task user client
//! or exclusive access needed. We open the raw `/dev/rdiskN` node: the block
//! node is busy while cddafs has the disc mounted.

use std::ffi::c_void;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::process::Command;

use anyhow::{Context, bail};

use super::CdDrive;
use crate::SECTOR_BYTES;
use crate::toc::{Toc, parse_full_toc};

#[repr(C)]
struct DkCdRead {
    offset: u64,
    sector_area: u8,
    sector_type: u8,
    reserved: [u8; 10],
    buffer_length: u32,
    buffer: *mut c_void,
}

#[repr(C)]
struct DkCdReadToc {
    format: u8,
    format_as_time: u8,
    reserved0: [u8; 5],
    address: u8,
    reserved1: [u8; 6],
    buffer_length: u16,
    buffer: *mut c_void,
}

const fn iowr(group: u8, num: u8, len: usize) -> libc::c_ulong {
    0xc000_0000
        | ((len as libc::c_ulong & 0x1fff) << 16)
        | ((group as libc::c_ulong) << 8)
        | num as libc::c_ulong
}

const DKIOCCDREAD: libc::c_ulong = iowr(b'd', 96, size_of::<DkCdRead>());
const DKIOCCDREADTOC: libc::c_ulong = iowr(b'd', 100, size_of::<DkCdReadToc>());

const K_CD_SECTOR_AREA_USER: u8 = 0x10;
const K_CD_SECTOR_AREA_ERROR_FLAGS: u8 = 0x02;
const K_CD_SECTOR_TYPE_CDDA: u8 = 0x01;
const K_CD_TOC_FORMAT_TOC: u8 = 0x02;
/// C2 error pointers: one bit per audio byte, after each sector's 2352 bytes.
const C2_BYTES: usize = 294;

pub struct MacDrive {
    file: File,
    model: Option<String>,
    /// Whether the drive returns C2 pointers; `None` until first tried.
    c2: Option<bool>,
}

impl MacDrive {
    pub fn open(device: &str) -> anyhow::Result<Self> {
        let device = device.replacen("/dev/disk", "/dev/rdisk", 1);
        let file = File::open(&device).with_context(|| format!("opening {device}"))?;
        Ok(Self {
            file,
            model: drutil_model(),
            c2: None,
        })
    }

    /// One DKIOCCDREAD of whole CD-DA sectors; `offset` is in units of the
    /// requested sector size, as IOCDMedia::readCD expects.
    fn cd_read(
        &self,
        lba: u32,
        area: u8,
        sector_size: usize,
        buf: &mut [u8],
    ) -> anyhow::Result<()> {
        assert_eq!(buf.len() % sector_size, 0);
        let mut req = DkCdRead {
            offset: lba as u64 * sector_size as u64,
            sector_area: area,
            sector_type: K_CD_SECTOR_TYPE_CDDA,
            reserved: [0; 10],
            buffer_length: buf.len() as u32,
            buffer: buf.as_mut_ptr().cast(),
        };
        // SAFETY: `req` matches dk_cd_read_t and points at a live buffer of the stated length.
        let ret = unsafe { libc::ioctl(self.file.as_raw_fd(), DKIOCCDREAD, &mut req) };
        if ret < 0 {
            bail!(
                "DKIOCCDREAD at LBA {lba} failed: {}",
                std::io::Error::last_os_error()
            );
        }
        if req.buffer_length as usize != buf.len() {
            bail!(
                "short read at LBA {lba}: {} of {} bytes",
                req.buffer_length,
                buf.len()
            );
        }
        Ok(())
    }

    /// Finds the BSD device of the disc in the first optical drive.
    pub fn find_device() -> anyhow::Result<String> {
        let out = Command::new("drutil")
            .arg("status")
            .output()
            .context("running drutil")?;
        let text = String::from_utf8_lossy(&out.stdout);
        text.split_whitespace()
            .find(|w| w.starts_with("/dev/disk"))
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("no disc found in any optical drive (drutil status)"))
    }
}

fn drutil_model() -> Option<String> {
    let out = Command::new("drutil").arg("status").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    // Line 1 is the "Vendor Product Rev" header, line 2 the values; drop the revision.
    let line = text.lines().nth(1)?;
    let mut words: Vec<&str> = line.split_whitespace().collect();
    words.pop()?;
    (!words.is_empty()).then(|| words.join(" "))
}

impl CdDrive for MacDrive {
    fn read_toc(&mut self) -> anyhow::Result<Toc> {
        let mut buf = vec![0u8; 4 + 11 * 110];
        let mut req = DkCdReadToc {
            format: K_CD_TOC_FORMAT_TOC,
            format_as_time: 1,
            reserved0: [0; 5],
            address: 0,
            reserved1: [0; 6],
            buffer_length: buf.len() as u16,
            buffer: buf.as_mut_ptr().cast(),
        };
        // SAFETY: `req` matches dk_cd_read_toc_t and points at a live buffer of the stated length.
        let ret = unsafe { libc::ioctl(self.file.as_raw_fd(), DKIOCCDREADTOC, &mut req) };
        if ret < 0 {
            bail!("DKIOCCDREADTOC failed: {}", std::io::Error::last_os_error());
        }
        buf.truncate(req.buffer_length as usize);
        parse_full_toc(&buf)
    }

    fn read_audio(&mut self, lba: u32, buf: &mut [u8]) -> anyhow::Result<()> {
        self.cd_read(lba, K_CD_SECTOR_AREA_USER, SECTOR_BYTES, buf)
    }

    fn read_audio_c2(&mut self, lba: u32, buf: &mut [u8]) -> anyhow::Result<Option<Vec<bool>>> {
        if self.c2 == Some(false) {
            return self.read_audio(lba, buf).map(|()| None);
        }
        let sectors = buf.len() / SECTOR_BYTES;
        let mut raw = vec![0u8; sectors * (SECTOR_BYTES + C2_BYTES)];
        let area = K_CD_SECTOR_AREA_USER | K_CD_SECTOR_AREA_ERROR_FLAGS;
        if let Err(e) = self.cd_read(lba, area, SECTOR_BYTES + C2_BYTES, &mut raw) {
            // A failure is either a bad spot or a drive without C2 support:
            // a plain read of the same sectors tells them apart.
            self.read_audio(lba, buf).map_err(|_| e)?;
            if self.c2.is_none() {
                self.c2 = Some(false);
            }
            return Ok(None);
        }
        self.c2 = Some(true);
        let flags = raw
            .as_chunks::<{ SECTOR_BYTES + C2_BYTES }>()
            .0
            .iter()
            .zip(buf.as_chunks_mut::<SECTOR_BYTES>().0)
            .map(|(sector, out)| {
                out.copy_from_slice(&sector[..SECTOR_BYTES]);
                sector[SECTOR_BYTES..].iter().any(|&b| b != 0)
            })
            .collect();
        Ok(Some(flags))
    }

    fn model(&self) -> Option<String> {
        self.model.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers_match_sdk() {
        // _IOWR('d', 96, dk_cd_read_t) and _IOWR('d', 100, dk_cd_read_toc_t) on LP64.
        assert_eq!(size_of::<DkCdRead>(), 32);
        assert_eq!(size_of::<DkCdReadToc>(), 24);
        assert_eq!(DKIOCCDREAD, 0xc020_6460);
        assert_eq!(DKIOCCDREADTOC, 0xc018_6464);
    }
}
