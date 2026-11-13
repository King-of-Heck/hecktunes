//! One interface over the recording formats: probing and reading frame ranges.

use super::wav;
use anyhow::Result;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub enum Format {
    /// `float`: a 32-bit float WAV, delivered as 24-bit.
    Wav { float: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceInfo {
    pub path: PathBuf,
    pub sample_rate: u32,
    pub channels: u16,
    /// Bits per sample of what `RangeReader` delivers.
    pub bits: u16,
    /// Length in frames (one sample per channel).
    pub frames: u64,
    pub format: Format,
}

impl SourceInfo {
    pub fn duration_ms(&self) -> u64 {
        self.frames * 1000 / self.sample_rate as u64
    }

    /// The frame at `ms`, clamped to the end of the file.
    pub fn ms_to_frame(&self, ms: u64) -> u64 {
        (ms * self.sample_rate as u64 / 1000).min(self.frames)
    }

    /// Same rate, depth, channels and format, so two sides can become one album.
    pub fn same_format(&self, other: &SourceInfo) -> bool {
        (self.sample_rate, self.channels, self.bits, &self.format) == (other.sample_rate, other.channels, other.bits, &other.format)
    }

    /// e.g. "44.1 kHz, 16-bit, stereo".
    pub fn describe(&self) -> String {
        let khz = self.sample_rate as f64 / 1000.0;
        let rate = if khz.fract() == 0.0 { format!("{khz:.0}") } else { format!("{khz:.1}") };
        let depth = match self.format {
            Format::Wav { float: true } => "32-bit float".to_string(),
            Format::Wav { float: false } => format!("{}-bit", self.bits),
        };
        let layout = if self.channels == 1 { "mono" } else { "stereo" };
        format!("{rate} kHz, {depth}, {layout}")
    }

    /// The kind of file, for messages.
    pub fn kind(&self) -> &'static str {
        match self.format {
            Format::Wav { .. } => "WAV",
        }
    }

    /// Bytes of WAV audio per second.
    pub fn byte_rate(&self) -> u64 {
        let bytes = match self.format {
            Format::Wav { float: true } => 4,
            Format::Wav { float: false } => self.bits as u64 / 8,
        };
        self.sample_rate as u64 * self.channels as u64 * bytes
    }
}

enum Backend {
    Wav(wav::WavReader),
}

/// Reads frames `[start, end)` of a recording in chunks, as interleaved integers at `SourceInfo::bits`.
pub struct RangeReader(Backend);

impl RangeReader {
    pub fn open(info: &SourceInfo, start: u64, end: u64) -> Result<Self> {
        Ok(Self(match &info.format {
            Format::Wav { .. } => Backend::Wav(wav::WavReader::open(info, start, end)?),
        }))
    }

    /// Appends up to `max_frames` frames to `out` as interleaved integers at `SourceInfo::bits`;
    /// returns the frames read (0 at the end).
    pub fn read(&mut self, max_frames: usize, out: &mut Vec<i32>) -> Result<usize> {
        match &mut self.0 {
            Backend::Wav(r) => r.read(max_frames, out),
        }
    }
}

pub fn probe(path: &Path) -> Result<SourceInfo> {
    wav::probe(path)
}

/// All of frames `[start, end)` at once (for short ranges).
pub fn read_range(info: &SourceInfo, start: u64, end: u64) -> Result<Vec<i32>> {
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
        assert_eq!(info.kind(), "WAV");
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
