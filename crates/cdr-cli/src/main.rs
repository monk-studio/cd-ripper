use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context, bail};
use cdr_core::accuraterip::{self, DiscIds, Submission};
use cdr_core::drive::CdDrive;
use cdr_core::musicbrainz::{self, DiscToc, Release};
use cdr_core::tags::Tags;
use cdr_core::{SECTOR_BYTES, SECTOR_SAMPLES, coverart, flac};
use clap::Parser;

/// Rips an audio CD and verifies it against AccurateRip.
#[derive(Parser)]
struct Args {
    /// Disc device, e.g. /dev/disk4. Defaults to the disc in the first optical drive.
    #[arg(long)]
    device: Option<String>,
    /// Drive read offset in samples. Detected from AccurateRip when omitted.
    #[arg(long, allow_hyphen_values = true)]
    offset: Option<i32>,
    /// Directory to write one tagged FLAC per track, plus cover art, into.
    #[arg(long)]
    out: Option<PathBuf>,
    /// MusicBrainz release ID to tag with. Asked for when several releases match.
    #[arg(long)]
    release: Option<String>,
    /// Only print the TOC, disc IDs and matching releases.
    #[arg(long)]
    toc_only: bool,
}

const SECTORS_PER_READ: u32 = 32;
const READ_ATTEMPTS: u32 = 3;

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut drive = open_drive(args.device.as_deref())?;
    if let Some(model) = drive.model() {
        println!("Drive: {model}");
    }

    let toc = drive.read_toc().context("reading TOC")?;
    println!("Tracks:");
    for (i, t) in toc.tracks.iter().enumerate() {
        let len = toc.track_end(i) - t.start;
        println!(
            "  {:2}  start {:6}  length {:6} ({:2}:{:02})  {}{}",
            t.number,
            t.start,
            len,
            len / 75 / 60,
            len / 75 % 60,
            if t.is_audio { "audio" } else { "data" },
            if t.pre_emphasis { ", pre-emphasis" } else { "" },
        );
    }
    println!("  lead-out {}", toc.leadout);

    let ids = DiscIds::from_toc(&toc);
    println!("AccurateRip: {}", ids.url());

    let disc = DiscToc::from_toc(&toc);
    println!("MusicBrainz: {}", disc.disc_id());
    let releases = musicbrainz::lookup(&disc).unwrap_or_else(|e| {
        eprintln!("  MusicBrainz lookup failed: {e}");
        Vec::new()
    });
    for (i, r) in releases.iter().enumerate() {
        println!("  {}) {}", i + 1, describe(r));
    }
    if args.toc_only {
        return Ok(());
    }
    let release = choose_release(&releases, args.release.as_deref())?;
    match release {
        Some(r) => println!("Tagging as: {} — {}", r.artist, r.title),
        None => println!("  no MusicBrainz match; tracks stay untitled"),
    }
    let db = accuraterip::fetch(&ids).context("querying AccurateRip")?;
    match &db {
        Some(subs) => println!("  {} submission(s) in the database", subs.len()),
        None => println!("  disc not in the database"),
    }

    let audio: Vec<_> = toc
        .tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.is_audio)
        .collect();
    let first = audio[0].1.start;
    let end = toc.audio_end();
    let pcm = read_range(drive.as_mut(), first, end)?;
    let raw = accuraterip::samples(&pcm);
    drop(pcm);

    let offset = match (args.offset, &db) {
        (Some(o), _) => o,
        (None, Some(subs)) => detect_offset(&raw, &toc, first, subs)
            .context("could not detect the read offset from AccurateRip; pass --offset")?,
        (None, None) => {
            bail!("disc not in AccurateRip, so the read offset can't be detected; pass --offset")
        }
    };
    println!(
        "Read offset: {offset:+} samples{}",
        if args.offset.is_some() {
            ""
        } else {
            " (detected)"
        }
    );
    let stream = accuraterip::apply_offset(&raw, offset);
    drop(raw);

    let cover = match (&args.out, release) {
        (Some(dir), Some(r)) => {
            std::fs::create_dir_all(dir)?;
            fetch_cover(dir, r)
        }
        (Some(dir), None) => {
            std::fs::create_dir_all(dir)?;
            None
        }
        _ => None,
    };
    let mut accurate = 0;
    for (n, &(i, t)) in audio.iter().enumerate() {
        let from = (t.start - first) as usize * SECTOR_SAMPLES;
        let to = (toc.track_end(i) - first) as usize * SECTOR_SAMPLES;
        let samples = &stream[from..to];
        let sums = accuraterip::checksums(samples, n == 0, n == audio.len() - 1);
        let verdict = match db.as_deref().map(|subs| best_match(subs, n, sums)) {
            Some(Some((conf, kind))) => {
                accurate += 1;
                format!("accurate ({kind}, confidence {conf})")
            }
            Some(None) => "NOT accurate".to_string(),
            None => "not checked".to_string(),
        };
        let title = release
            .and_then(|r| r.tracks.get(n))
            .map(|m| format!("  {} — {}", m.artist, m.title))
            .unwrap_or_default();
        println!(
            "  track {:2}  v1 {:08x}  v2 {:08x}  {verdict}{title}",
            t.number, sums.v1, sums.v2
        );
        if let Some(dir) = &args.out {
            let tags = Tags::for_track(release, &disc.disc_id(), n, audio.len());
            flac::write(
                &dir.join(format!("{:02}.flac", t.number)),
                samples,
                &tags,
                cover.as_ref(),
            )?;
        }
    }
    if db.is_some() {
        println!("{accurate} of {} tracks accurately ripped", audio.len());
    }
    Ok(())
}

fn open_drive(device: Option<&str>) -> anyhow::Result<Box<dyn CdDrive>> {
    #[cfg(target_os = "macos")]
    {
        use cdr_core::drive::macos::MacDrive;
        let device = match device {
            Some(d) => d.to_owned(),
            None => MacDrive::find_device()?,
        };
        println!("Device: {device}");
        Ok(Box::new(MacDrive::open(&device)?))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = device;
        bail!("no drive backend for this platform yet")
    }
}

/// Fetches the front cover and saves a copy next to the tracks. Failures only warn.
fn fetch_cover(dir: &std::path::Path, release: &Release) -> Option<coverart::Cover> {
    match coverart::fetch(release) {
        Ok(Some(cover)) => {
            let ext = if cover.mime_type == "image/png" {
                "png"
            } else {
                "jpg"
            };
            if let Err(e) = std::fs::write(dir.join(format!("cover.{ext}")), &cover.data) {
                eprintln!("  saving cover failed: {e}");
            }
            println!("Cover: {} KB {}", cover.data.len() / 1024, cover.mime_type);
            Some(cover)
        }
        Ok(None) => {
            println!("  no cover in the Cover Art Archive");
            None
        }
        Err(e) => {
            eprintln!("  cover download failed: {e}");
            None
        }
    }
}

/// One line per candidate: enough to tell pressings apart.
fn describe(r: &Release) -> String {
    let details: Vec<&str> = [
        r.date.as_deref(),
        r.country.as_deref(),
        r.label.as_deref(),
        r.catalog_number.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect();
    format!(
        "{} — {} ({}; disc {}/{}){}  {}",
        r.artist,
        r.title,
        details.join(", "),
        r.disc_number,
        r.disc_total,
        if r.exact { "" } else { " [TOC match]" },
        r.id,
    )
}

/// Picks the release named by `--release`, the only candidate, or asks on a terminal.
fn choose_release<'a>(
    releases: &'a [Release],
    wanted: Option<&str>,
) -> anyhow::Result<Option<&'a Release>> {
    if let Some(id) = wanted {
        return match releases.iter().find(|r| r.id == id) {
            Some(r) => Ok(Some(r)),
            None => bail!("release {id} doesn't match this disc"),
        };
    }
    if releases.len() <= 1 || !std::io::stdin().is_terminal() {
        if releases.len() > 1 {
            println!("  using 1); pass --release to pick another");
        }
        return Ok(releases.first());
    }
    loop {
        print!("Choose a release [1-{}]: ", releases.len());
        std::io::stdout().flush()?;
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line)? == 0 {
            bail!("no release chosen");
        }
        if let Ok(n @ 1..) = line.trim().parse::<usize>()
            && let Some(r) = releases.get(n - 1)
        {
            return Ok(Some(r));
        }
    }
}

/// Reads sectors `[from, to)`, retrying failed chunks and zero-filling what stays unreadable.
fn read_range(drive: &mut dyn CdDrive, from: u32, to: u32) -> anyhow::Result<Vec<u8>> {
    let total = to - from;
    let mut pcm = vec![0u8; total as usize * SECTOR_BYTES];
    let mut failed = 0;
    let started = std::time::Instant::now();
    for chunk_start in (from..to).step_by(SECTORS_PER_READ as usize) {
        let count = SECTORS_PER_READ.min(to - chunk_start);
        let at = (chunk_start - from) as usize * SECTOR_BYTES;
        let buf = &mut pcm[at..at + count as usize * SECTOR_BYTES];
        let mut result = Ok(());
        for _ in 0..READ_ATTEMPTS {
            result = drive.read_audio(chunk_start, buf);
            if result.is_ok() {
                break;
            }
        }
        if let Err(e) = result {
            eprintln!("\n  {e}");
            buf.fill(0);
            failed += count;
        }
        let done = chunk_start + count - from;
        let secs = started.elapsed().as_secs_f64().max(0.001);
        eprint!(
            "\rReading {:5.1}%  {:4.1}x",
            done as f64 * 100.0 / total as f64,
            done as f64 / 75.0 / secs,
        );
        std::io::stderr().flush().ok();
    }
    eprintln!();
    if failed > 0 {
        eprintln!("  {failed} sector(s) unreadable, filled with silence");
    }
    Ok(pcm)
}

/// Tries each track long enough to hold the sector-450 checksum until one matches.
fn detect_offset(
    raw: &[u32],
    toc: &cdr_core::toc::Toc,
    first: u32,
    subs: &[Submission],
) -> Option<i32> {
    toc.audio_tracks().enumerate().find_map(|(n, t)| {
        let known: Vec<u32> = subs
            .iter()
            .filter_map(|s| s.tracks.get(n))
            .map(|e| e.offset_crc)
            .filter(|&c| c != 0)
            .collect();
        let start = (t.start - first) as usize * SECTOR_SAMPLES;
        accuraterip::detect_offset(raw, start, &known)
    })
}

/// Highest confidence among database entries matching either checksum version.
fn best_match(
    subs: &[Submission],
    track: usize,
    sums: accuraterip::Checksums,
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
