use std::io::Write;

/// Writes 44.1 kHz 16-bit stereo PCM as a WAV file.
pub fn write(path: &std::path::Path, samples: &[u32]) -> std::io::Result<()> {
    let data_len = samples.len() as u32 * 4;
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    out.write_all(b"RIFF")?;
    out.write_all(&(36 + data_len).to_le_bytes())?;
    out.write_all(b"WAVEfmt ")?;
    out.write_all(&16u32.to_le_bytes())?;
    out.write_all(&1u16.to_le_bytes())?; // PCM
    out.write_all(&2u16.to_le_bytes())?; // channels
    out.write_all(&44100u32.to_le_bytes())?;
    out.write_all(&(44100u32 * 4).to_le_bytes())?; // byte rate
    out.write_all(&4u16.to_le_bytes())?; // block align
    out.write_all(&16u16.to_le_bytes())?; // bits per sample
    out.write_all(b"data")?;
    out.write_all(&data_len.to_le_bytes())?;
    for s in samples {
        out.write_all(&s.to_le_bytes())?;
    }
    out.flush()
}
