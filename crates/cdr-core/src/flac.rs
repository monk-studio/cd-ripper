//! FLAC output: encoding with flacenc, then tags and cover art with metaflac.

use std::path::Path;

use anyhow::Context;
use flacenc::component::BitRepr;
use flacenc::error::Verify;
use metaflac::block::PictureType;

use crate::coverart::Cover;
use crate::tags::Tags;

/// Encodes CD audio (AccurateRip-style samples, left in the low half) to `path`.
pub fn write(
    path: &Path,
    samples: &[u32],
    tags: &Tags,
    cover: Option<&Cover>,
) -> anyhow::Result<()> {
    let interleaved: Vec<i32> = samples
        .iter()
        .flat_map(|&s| {
            [
                (s & 0xffff) as u16 as i16 as i32,
                (s >> 16) as u16 as i16 as i32,
            ]
        })
        .collect();
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|(_, e)| anyhow::anyhow!("FLAC encoder config: {e:?}"))?;
    let source = flacenc::source::MemSource::from_samples(&interleaved, 2, 16, 44100);
    let mut stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| anyhow::anyhow!("FLAC encoding failed: {e:?}"))?;
    // flacenc lowers the minimum to the short final block, but fixed-blocksize
    // streams must declare min == max: decoders derive sample positions from it,
    // and `flac -t` flags every frame (seeking breaks) otherwise.
    stream
        .stream_info_mut()
        .set_block_sizes(config.block_size, config.block_size)
        .map_err(|e| anyhow::anyhow!("FLAC block size: {e:?}"))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| anyhow::anyhow!("FLAC serialisation failed: {e:?}"))?;
    std::fs::write(path, sink.as_slice()).with_context(|| format!("writing {}", path.display()))?;

    let mut tag = metaflac::Tag::read_from_path(path)?;
    tag.vorbis_comments_mut().vendor_string = format!("CDR {}", env!("CARGO_PKG_VERSION"));
    for (key, values) in tags.fields() {
        tag.set_vorbis(key.as_str(), values.clone());
    }
    if let Some(cover) = cover {
        tag.add_picture(
            cover.mime_type.as_str(),
            PictureType::CoverFront,
            cover.data.clone(),
        );
    }
    tag.save()?;
    Ok(())
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
}
