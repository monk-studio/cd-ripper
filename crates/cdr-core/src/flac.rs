//! FLAC output: frames encoded with flacenc a batch at a time on every
//! core and written as they're done, behind tags and cover art laid out with
//! metaflac. Only the samples and one batch of frames are in memory at once.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::Context;
use flacenc::bitsink::ByteSink;
use flacenc::component::{BitRepr, Frame, Stream, StreamInfo};
use flacenc::config::Encoder;
use flacenc::error::{Verified, Verify};
use flacenc::source::{Fill, FrameBuf};
use md5::Digest;
use metaflac::block::{Block, PictureType};

use crate::coverart::Cover;
use crate::tags::Tags;

const SAMPLE_RATE: usize = 44100;
/// The metadata block type of Vorbis comments (tags).
const VORBIS_COMMENT: u8 = 4;
/// Frames encoded together; each is about 16 KB of samples.
const BATCH_FRAMES: usize = 256;

/// Encodes CD audio (AccurateRip-style samples, left in the low half) to `path`.
pub fn write(
    path: &Path,
    samples: &[u32],
    tags: &Tags,
    cover: Option<&Cover>,
) -> anyhow::Result<()> {
    let config = Encoder::default()
        .into_verified()
        .map_err(|(_, e)| anyhow::anyhow!("FLAC encoder config: {e:?}"))?;
    let block_size = config.block_size;
    let mut info = StreamInfo::new(SAMPLE_RATE, 2, 16)
        .map_err(|e| anyhow::anyhow!("FLAC stream info: {e:?}"))?;
    info.set_block_sizes(block_size, block_size)
        .map_err(|e| anyhow::anyhow!("FLAC block size: {e:?}"))?;
    // Frames are encoded against the stream info as it starts out.
    let start_info = info.clone();

    let mut tag = metadata(&info, tags, cover)?;
    let file = File::create(path).with_context(|| format!("writing {}", path.display()))?;
    let mut out = BufWriter::new(file);
    // The stream info is final only once every frame is written; its size
    // doesn't change, so it's written again in place at the end.
    tag.write_to(&mut out)?;

    let mut md5 = md5::Md5::new();
    for s in samples {
        md5.update(s.to_le_bytes());
    }
    let frames: Vec<&[u32]> = samples.chunks(block_size).collect();
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut sink = ByteSink::new();
    for (b, batch) in frames.chunks(BATCH_FRAMES).enumerate() {
        let first = b * BATCH_FRAMES;
        let per_worker = batch.len().div_ceil(workers);
        let encoded: Vec<Frame> = std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .chunks(per_worker)
                .enumerate()
                .map(|(w, part)| {
                    let (config, info) = (&config, &start_info);
                    scope.spawn(move || {
                        part.iter()
                            .enumerate()
                            .map(|(i, frame)| {
                                encode_frame(config, info, frame, first + w * per_worker + i)
                            })
                            .collect::<anyhow::Result<Vec<_>>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("FLAC encoder thread"))
                .collect::<anyhow::Result<Vec<_>>>()
                .map(|parts| parts.into_iter().flatten().collect())
        })?;
        for frame in &encoded {
            info.update_frame_info(frame);
            sink.clear();
            frame
                .write(&mut sink)
                .map_err(|e| anyhow::anyhow!("FLAC serialisation failed: {e:?}"))?;
            out.write_all(sink.as_slice())?;
        }
    }

    // flacenc lowers the minimum to the short final block, but fixed-blocksize
    // streams must declare min == max: decoders derive sample positions from it,
    // and `flac -t` flags every frame (seeking breaks) otherwise.
    info.set_block_sizes(block_size, block_size)
        .map_err(|e| anyhow::anyhow!("FLAC block size: {e:?}"))?;
    info.set_md5_digest(&md5.finalize().into());
    info.set_total_samples(samples.len());
    let mut file = out.into_inner().map_err(|e| e.into_error())?;
    file.seek(SeekFrom::Start(0))?;
    metadata(&info, tags, cover)?.write_to(&mut file)?;
    file.flush()?;
    Ok(())
}

/// One frame of up to a block of samples, frame `number` of the stream.
fn encode_frame(
    config: &Verified<Encoder>,
    info: &StreamInfo,
    samples: &[u32],
    number: usize,
) -> anyhow::Result<Frame> {
    let interleaved: Vec<i32> = samples
        .iter()
        .flat_map(|&s| {
            [
                (s & 0xffff) as u16 as i16 as i32,
                (s >> 16) as u16 as i16 as i32,
            ]
        })
        .collect();
    let mut buf = FrameBuf::with_size(2, config.block_size)
        .map_err(|e| anyhow::anyhow!("FLAC frame buffer: {e:?}"))?;
    buf.fill_interleaved(&interleaved)
        .map_err(|e| anyhow::anyhow!("FLAC frame: {e:?}"))?;
    flacenc::encode_fixed_size_frame(config, &buf, number, info)
        .map_err(|e| anyhow::anyhow!("FLAC encoding failed: {e:?}"))
}

/// A VORBIS_COMMENT block's body: the vendor, then `KEY=value` per value, in
/// order (metaflac's own block keeps them in a hash map, in no set order).
fn vorbis_comment(vendor: &str, tags: &Tags) -> Vec<u8> {
    let comments: Vec<String> = tags
        .fields()
        .iter()
        .flat_map(|(key, values)| values.iter().map(move |v| format!("{key}={v}")))
        .collect();
    let mut out = Vec::new();
    out.extend((vendor.len() as u32).to_le_bytes());
    out.extend(vendor.as_bytes());
    out.extend((comments.len() as u32).to_le_bytes());
    for c in &comments {
        out.extend((c.len() as u32).to_le_bytes());
        out.extend(c.as_bytes());
    }
    out
}

/// The file's metadata: the stream info, then tags, the cover and 1 KB of
/// padding, laid out as metaflac saves a tagged file. Tags keep the order
/// `tags` has them in, so the same rip always gives the same bytes.
fn metadata(
    info: &StreamInfo,
    tags: &Tags,
    cover: Option<&Cover>,
) -> anyhow::Result<metaflac::Tag> {
    let mut sink = ByteSink::new();
    Stream::with_stream_info(info.clone())
        .write(&mut sink)
        .map_err(|e| anyhow::anyhow!("FLAC stream info: {e:?}"))?;
    let mut tag = metaflac::Tag::read_from(&mut sink.as_slice())?;
    let vendor = format!("CDR {}", env!("CARGO_PKG_VERSION"));
    tag.push_block(Block::Unknown((
        VORBIS_COMMENT,
        vorbis_comment(&vendor, tags),
    )));
    if let Some(cover) = cover {
        tag.add_picture(
            cover.mime_type.as_str(),
            PictureType::CoverFront,
            cover.data.clone(),
        );
    }
    tag.push_block(Block::Padding(1024));
    Ok(tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_samples_and_tags() {
        let dir = std::env::temp_dir().join(format!("cdr-flac-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.flac");

        // Full-scale extremes and a ramp, so sign handling is exercised.
        let mut samples: Vec<u32> = vec![0x8000_7fff, 0x7fff_8000, 0xffff_0001, 0];
        samples.extend((0..20_000u32).map(|i| i.wrapping_mul(2_654_435_761)));
        let mut tags = Tags::default();
        tags.add("TITLE", "メリッサ");
        tags.add("MUSICBRAINZ_ARTISTID", "a");
        tags.add("MUSICBRAINZ_ARTISTID", "b");
        let cover = Cover {
            mime_type: "image/jpeg".into(),
            data: vec![0xff, 0xd8, 0xff, 0xd9],
        };
        write(&path, &samples, &tags, Some(&cover)).unwrap();

        let mut reader = claxon::FlacReader::open(&path).unwrap();
        let info = reader.streaminfo();
        assert_eq!(info.min_block_size, info.max_block_size, "fixed block size");
        assert_eq!(reader.streaminfo().bits_per_sample, 16);
        assert_eq!(reader.streaminfo().channels, 2);
        assert!(
            reader.streaminfo().md5sum != [0; 16],
            "STREAMINFO MD5 must be set"
        );
        let decoded: Vec<i32> = reader.samples().map(Result::unwrap).collect();
        let back: Vec<u32> = decoded
            .as_chunks::<2>()
            .0
            .iter()
            .map(|lr| (lr[0] as u16 as u32) | ((lr[1] as u16 as u32) << 16))
            .collect();
        assert_eq!(back, samples);

        let tag = metaflac::Tag::read_from_path(&path).unwrap();
        assert_eq!(
            tag.get_vorbis("TITLE").unwrap().collect::<Vec<_>>(),
            ["メリッサ"]
        );
        assert_eq!(
            tag.get_vorbis("MUSICBRAINZ_ARTISTID")
                .unwrap()
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(tag.pictures().next().unwrap().data, cover.data);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn same_input_gives_the_same_bytes_with_tags_in_order() {
        let dir = std::env::temp_dir().join(format!("cdr-flac-order-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let samples: Vec<u32> = (0..50_000u32)
            .map(|i| i.wrapping_mul(2_654_435_761))
            .collect();
        let mut tags = Tags::default();
        for key in ["TRACKNUMBER", "ALBUM", "TITLE", "ARTIST", "DATE", "LABEL"] {
            tags.add(key, format!("{key} value"));
        }
        let (a, b) = (dir.join("a.flac"), dir.join("b.flac"));
        write(&a, &samples, &tags, None).unwrap();
        write(&b, &samples, &tags, None).unwrap();
        let bytes = std::fs::read(&a).unwrap();
        assert_eq!(bytes, std::fs::read(&b).unwrap());
        let at = |needle: &str| {
            bytes
                .windows(needle.len())
                .position(|w| w == needle.as_bytes())
                .unwrap()
        };
        let positions: Vec<usize> = [
            "TRACKNUMBER=",
            "ALBUM=",
            "TITLE=",
            "ARTIST=",
            "DATE=",
            "LABEL=",
        ]
        .iter()
        .map(|k| at(k))
        .collect();
        assert!(
            positions.is_sorted(),
            "tags keep their order: {positions:?}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
