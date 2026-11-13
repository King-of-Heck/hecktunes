//! Reading the recording: 16/24-bit integer or 32-bit float WAV, mono or stereo.

use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

/// Bit depth float WAVs are delivered (and encoded) at.
pub const FLOAT_AS_BITS: u16 = 24;

#[derive(Debug, Clone, PartialEq)]
pub struct WavInfo {
    pub path: PathBuf,
    pub sample_rate: u32,
    pub channels: u16,
    /// Bits per sample of what `RangeReader` delivers: the file's own depth, or 24 for float.
    pub bits: u16,
    float: bool,
    /// Length in frames (one sample per channel).
    pub frames: u64,
}

impl WavInfo {
    pub fn duration_ms(&self) -> u64 {
        self.frames * 1000 / self.sample_rate as u64
    }

    /// The frame at `ms`, clamped to the end of the file.
    pub fn ms_to_frame(&self, ms: u64) -> u64 {
        (ms * self.sample_rate as u64 / 1000).min(self.frames)
    }

    /// Same rate, depth and channels, so two sides can become one album.
    pub fn same_format(&self, other: &WavInfo) -> bool {
        (self.sample_rate, self.channels, self.bits, self.float) == (other.sample_rate, other.channels, other.bits, other.float)
    }

    /// e.g. "44.1 kHz, 16-bit, stereo".
    pub fn describe(&self) -> String {
        let khz = self.sample_rate as f64 / 1000.0;
        let rate = if khz.fract() == 0.0 { format!("{khz:.0}") } else { format!("{khz:.1}") };
        let depth = if self.float { "32-bit float".to_string() } else { format!("{}-bit", self.bits) };
        let layout = if self.channels == 1 { "mono" } else { "stereo" };
        format!("{rate} kHz, {depth}, {layout}")
    }

    /// Bytes of WAV audio per second.
    pub fn byte_rate(&self) -> u64 {
        let bytes = if self.float { 4 } else { self.bits as u64 / 8 };
        self.sample_rate as u64 * self.channels as u64 * bytes
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn open(path: &Path) -> Result<hound::WavReader<BufReader<File>>> {
    hound::WavReader::open(path).with_context(|| format!("{} is not a WAV file HeckTunes can read", file_name(path)))
}

pub fn probe(path: &Path) -> Result<WavInfo> {
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
    Ok(WavInfo {
        path: path.to_path_buf(),
        sample_rate: s.sample_rate,
        channels: s.channels,
        bits: if float { FLOAT_AS_BITS } else { s.bits_per_sample },
        float,
        frames: r.duration() as u64,
    })
}

/// Reads frames `[start, end)` of a WAV in chunks, as interleaved integers at `WavInfo::bits`.
pub struct RangeReader {
    reader: hound::WavReader<BufReader<File>>,
    float: bool,
    channels: usize,
    remaining: u64,
}

impl RangeReader {
    pub fn open(info: &WavInfo, start: u64, end: u64) -> Result<Self> {
        let mut reader = open(&info.path)?;
        let start = start.min(info.frames);
        let end = end.clamp(start, info.frames);
        reader.seek(start as u32).with_context(|| format!("can't seek in {}", file_name(&info.path)))?;
        Ok(Self { reader, float: info.float, channels: info.channels as usize, remaining: end - start })
    }

    /// Appends up to `max_frames` frames to `out`; returns the frames read (0 at the end).
    pub fn read(&mut self, max_frames: usize, out: &mut Vec<i32>) -> Result<usize> {
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

/// All of frames `[start, end)` at once (for short ranges).
pub fn read_range(info: &WavInfo, start: u64, end: u64) -> Result<Vec<i32>> {
    let mut r = RangeReader::open(info, start, end)?;
    let mut out = Vec::new();
    while r.read(65_536, &mut out)? > 0 {}
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{tone, write_pattern_wav};

    #[test]
    fn probe_reads_the_format() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        write_pattern_wav(&p, 48000, 2, 24, &[tone(2.0, 0.5)]);
        let info = probe(&p).unwrap();
        assert_eq!((info.sample_rate, info.channels, info.bits, info.frames), (48000, 2, 24, 96000));
        assert_eq!(info.duration_ms(), 2000);
        assert_eq!(info.ms_to_frame(500), 24000);
        assert_eq!(info.ms_to_frame(99_999), 96000, "clamped to the end");
    }

    #[test]
    fn probe_rejects_unsupported_depths_and_non_wavs() {
        let dir = tempfile::tempdir().unwrap();
        let p8 = dir.path().join("eight.wav");
        let spec = hound::WavSpec { channels: 1, sample_rate: 8000, bits_per_sample: 8, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&p8, spec).unwrap();
        w.write_sample(0i8).unwrap();
        w.finalize().unwrap();
        let e = format!("{:#}", probe(&p8).unwrap_err());
        assert!(e.contains("8-bit") && e.contains("eight.wav"), "{e}");

        let junk = dir.path().join("junk.wav");
        std::fs::write(&junk, b"not a wav").unwrap();
        assert!(probe(&junk).is_err());
    }

    #[test]
    fn probe_rejects_a_zero_sample_rate() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("zero.wav");
        write_pattern_wav(&p, 8000, 1, 16, &[tone(0.1, 0.5)]);
        let mut bytes = std::fs::read(&p).unwrap();
        bytes[24..32].copy_from_slice(&[0; 8]); // fmt chunk: sample rate and byte rate
        std::fs::write(&p, bytes).unwrap();
        let e = format!("{:#}", probe(&p).unwrap_err());
        assert!(e.contains("zero.wav") && e.contains("sample rate"), "{e}");
    }

    #[test]
    fn read_range_returns_exactly_the_requested_frames() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        write_pattern_wav(&p, 8000, 2, 16, &[tone(2.0, 0.5)]);
        let info = probe(&p).unwrap();
        let all: Vec<i32> = hound::WavReader::open(&p).unwrap().samples::<i32>().map(|s| s.unwrap()).collect();
        let got = read_range(&info, 4000, 6000).unwrap();
        assert_eq!(got.len(), 2000 * 2);
        assert_eq!(got, all[8000..12000].to_vec());
        assert!(read_range(&info, 15_000, 99_000).unwrap().len() == 1000 * 2, "end clamped");
    }

    #[test]
    fn float_wavs_are_delivered_as_24_bit() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.wav");
        let spec = hound::WavSpec { channels: 1, sample_rate: 8000, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
        let mut w = hound::WavWriter::create(&p, spec).unwrap();
        for _ in 0..100 {
            w.write_sample(0.5f32).unwrap();
        }
        w.finalize().unwrap();
        let info = probe(&p).unwrap();
        assert_eq!(info.bits, 24);
        let got = read_range(&info, 0, 100).unwrap();
        assert_eq!(got[0], 4_194_303);
    }

    #[test]
    fn same_format_compares_rate_depth_and_channels() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.wav"), dir.path().join("b.wav"));
        write_pattern_wav(&a, 44100, 2, 16, &[tone(0.1, 0.5)]);
        write_pattern_wav(&b, 48000, 2, 16, &[tone(0.1, 0.5)]);
        let (a, b) = (probe(&a).unwrap(), probe(&b).unwrap());
        assert!(!a.same_format(&b));
        assert!(a.same_format(&a.clone()));
        assert_eq!(a.describe(), "44.1 kHz, 16-bit, stereo");
    }
}
