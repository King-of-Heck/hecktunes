//! Reading a WAV recording: 16/24-bit integer or 32-bit float, mono or stereo.

use super::source::{Format, SourceInfo};
use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

/// Bit depth float WAVs are delivered (and encoded) at.
pub const FLOAT_AS_BITS: u16 = 24;

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn open(path: &Path) -> Result<hound::WavReader<BufReader<File>>> {
    hound::WavReader::open(path).with_context(|| format!("{} is not a WAV file HeckTunes can read", file_name(path)))
}

pub(super) fn probe(path: &Path) -> Result<SourceInfo> {
    let r = open(path)?;
    let s = r.spec();
    let float = match (s.sample_format, s.bits_per_sample) {
        (hound::SampleFormat::Int, 16 | 24) => false,
        (hound::SampleFormat::Float, 32) => true,
        (_, bits) => bail!("{} is {bits}-bit; use a 16-bit, 24-bit or 32-bit float WAV", file_name(path)),
    };
    if s.sample_rate == 0 {
        bail!("{} says its sample rate is 0 Hz; the file is damaged", file_name(path));
    }
    if !(1..=2).contains(&s.channels) {
        bail!("{} has {} channels; only mono or stereo recordings are supported", file_name(path), s.channels);
    }
    Ok(SourceInfo {
        path: path.to_path_buf(),
        sample_rate: s.sample_rate,
        channels: s.channels,
        bits: if float { FLOAT_AS_BITS } else { s.bits_per_sample },
        frames: r.duration() as u64,
        format: Format::Wav { float },
    })
}

/// Reads frames `[start, end)` of a WAV in chunks, as interleaved integers at `SourceInfo::bits`.
pub(super) struct WavReader {
    reader: hound::WavReader<BufReader<File>>,
    float: bool,
    channels: usize,
    remaining: u64,
}

impl WavReader {
    pub(super) fn open(info: &SourceInfo, start: u64, end: u64) -> Result<Self> {
        let float = matches!(info.format, Format::Wav { float: true });
        let mut reader = open(&info.path)?;
        let start = start.min(info.frames);
        let end = end.clamp(start, info.frames);
        reader.seek(start as u32).with_context(|| format!("can't seek in {}", file_name(&info.path)))?;
        Ok(Self { reader, float, channels: info.channels as usize, remaining: end - start })
    }

    /// Appends up to `max_frames` frames to `out`; returns the frames read (0 at the end).
    pub(super) fn read(&mut self, max_frames: usize, out: &mut Vec<i32>) -> Result<usize> {
        let want = (max_frames as u64).min(self.remaining) as usize * self.channels;
        let before = out.len();
        if self.float {
            let scale = ((1i32 << (FLOAT_AS_BITS - 1)) - 1) as f32;
            for s in self.reader.samples::<f32>().take(want) {
                out.push((s?.clamp(-1.0, 1.0) * scale) as i32);
            }
        } else {
            for s in self.reader.samples::<i32>().take(want) {
                out.push(s?);
            }
        }
        let frames = (out.len() - before) / self.channels;
        out.truncate(before + frames * self.channels);
        self.remaining = if frames * self.channels < want { 0 } else { self.remaining - frames as u64 };
        Ok(frames)
    }
}
