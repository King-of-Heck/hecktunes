//! One interface over the recording formats: probing and reading frame ranges.

use super::mp3::cut;
use super::mp3::decode::Mp3Reader;
use super::mp3::index::{self, Index};
use super::wav;
use anyhow::{Context, Result};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// FLAC is assumed to need at most this fraction of the WAV's size.
const FLAC_SIZE_RATIO: f64 = 0.7;

#[derive(Debug, Clone, PartialEq)]
pub enum Format {
    /// `float`: a 32-bit float WAV, delivered as 24-bit.
    Wav { float: bool },
    /// An MP3, delivered as 16-bit; reads are positioned by its frame index.
    Mp3(Arc<Index>),
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

    /// Same kind, rate and channels (and for WAVs the same depth), so two sides can become one
    /// album. Two MP3s may differ in bitrate.
    pub fn same_format(&self, other: &SourceInfo) -> bool {
        let same_layout = (self.sample_rate, self.channels) == (other.sample_rate, other.channels);
        match (&self.format, &other.format) {
            (Format::Wav { float: a }, Format::Wav { float: b }) => same_layout && self.bits == other.bits && a == b,
            (Format::Mp3(_), Format::Mp3(_)) => same_layout,
            _ => false,
        }
    }

    /// e.g. "44.1 kHz, 16-bit, stereo" for a WAV, "MP3, 44.1 kHz, stereo" for an MP3.
    pub fn describe(&self) -> String {
        let khz = self.sample_rate as f64 / 1000.0;
        let rate = if khz.fract() == 0.0 { format!("{khz:.0}") } else { format!("{khz:.2}").trim_end_matches('0').to_string() };
        let layout = if self.channels == 1 { "mono" } else { "stereo" };
        match self.format {
            Format::Wav { float: true } => format!("{rate} kHz, 32-bit float, {layout}"),
            Format::Wav { float: false } => format!("{rate} kHz, {}-bit, {layout}", self.bits),
            Format::Mp3(_) => format!("MP3, {rate} kHz, {layout}"),
        }
    }

    /// The kind of file, for messages.
    pub fn kind(&self) -> &'static str {
        match self.format {
            Format::Wav { .. } => "WAV",
            Format::Mp3(_) => "MP3",
        }
    }

    /// Bytes of WAV audio per second; only meaningful for WAVs.
    fn byte_rate(&self) -> u64 {
        let bytes = match self.format {
            Format::Wav { float: true } => 4,
            Format::Wav { float: false } | Format::Mp3(_) => self.bits as u64 / 8,
        };
        self.sample_rate as u64 * self.channels as u64 * bytes
    }

    /// Bytes a track [start_ms, end_ms) will take: 0.7 x PCM for WAV (FLAC), the cut file for MP3.
    pub fn track_bytes(&self, start_ms: u64, end_ms: u64) -> u64 {
        match &self.format {
            Format::Wav { .. } => ((end_ms.saturating_sub(start_ms) * self.byte_rate() / 1000) as f64 * FLAC_SIZE_RATIO) as u64,
            Format::Mp3(ix) => cut::cut_bytes(ix, cut::frame_span(ix, self.ms_to_frame(start_ms), self.ms_to_frame(end_ms))),
        }
    }

    /// File extension the track is written with: "flac" or "mp3".
    pub fn track_extension(&self) -> &'static str {
        match self.format {
            Format::Wav { .. } => "flac",
            Format::Mp3(_) => "mp3",
        }
    }
}

enum Backend {
    Wav(wav::WavReader),
    Mp3(Mp3Reader),
}

/// Reads frames `[start, end)` of a recording in chunks, as interleaved integers at `SourceInfo::bits`.
pub struct RangeReader(Backend);

impl RangeReader {
    pub fn open(info: &SourceInfo, start: u64, end: u64) -> Result<Self> {
        Ok(Self(match &info.format {
            Format::Wav { .. } => Backend::Wav(wav::WavReader::open(info, start, end)?),
            Format::Mp3(index) => Backend::Mp3(Mp3Reader::open(&info.path, index.clone(), start, end)?),
        }))
    }

    /// Appends up to `max_frames` frames to `out` as interleaved integers at `SourceInfo::bits`;
    /// returns the frames read (0 at the end).
    pub fn read(&mut self, max_frames: usize, out: &mut Vec<i32>) -> Result<usize> {
        match &mut self.0 {
            Backend::Wav(r) => r.read(max_frames, out),
            Backend::Mp3(r) => r.read(max_frames, out),
        }
    }
}

/// A WAV (by its RIFF header) or an MP3 (anything else; `index::scan` explains what it isn't).
pub fn probe(path: &Path) -> Result<SourceInfo> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut magic = Vec::with_capacity(4);
    File::open(path).with_context(|| format!("cannot open {name}"))?.take(4).read_to_end(&mut magic)?;
    if magic == b"RIFF" {
        return wav::probe(path);
    }
    let index = index::scan(path)?;
    Ok(SourceInfo {
        path: path.to_path_buf(),
        sample_rate: index.sample_rate,
        channels: index.channels,
        bits: 16,
        frames: index.total_samples(),
        format: Format::Mp3(Arc::new(index)),
    })
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

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vinyl").join(name)
    }

    #[test]
    fn probe_mp3() {
        let info = probe(&fixture("cbr.mp3")).unwrap();
        let Format::Mp3(index) = &info.format else { panic!("not MP3: {:?}", info.format) };
        assert_eq!(info.kind(), "MP3");
        assert_eq!((info.bits, info.channels, info.sample_rate), (16, 2, 44100));
        assert_eq!(info.frames, index.total_samples());
        assert_eq!(info.describe(), "MP3, 44.1 kHz, stereo");
        assert_eq!(probe(&fixture("mono22k.mp3")).unwrap().describe(), "MP3, 22.05 kHz, mono");
    }

    #[test]
    fn probe_messages() {
        let dir = tempfile::tempdir().unwrap();
        let junk = dir.path().join("junk.bin");
        std::fs::write(&junk, b"hello world").unwrap();
        let e = format!("{:#}", probe(&junk).unwrap_err());
        assert!(e.contains("junk.bin is not a WAV or MP3 file HeckTunes can read"), "{e}");
        let e = format!("{:#}", probe(&fixture("layer2.mp2")).unwrap_err());
        assert!(e.contains("is not an MP3 (Layer III) file"), "{e}");
        let wav = dir.path().join("a.wav");
        write_pattern_wav(&wav, 44100, 2, 16, &[tone(0.1, 0.5)]);
        assert_eq!(probe(&wav).unwrap().kind(), "WAV");
    }

    #[test]
    fn same_format_for_mp3s_ignores_bitrate_but_not_kind() {
        let (cbr, vbr, mono) = (probe(&fixture("cbr.mp3")).unwrap(), probe(&fixture("vbr.mp3")).unwrap(), probe(&fixture("mono22k.mp3")).unwrap());
        assert!(cbr.same_format(&vbr));
        assert!(!cbr.same_format(&mono));
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("a.wav");
        write_pattern_wav(&wav, 44100, 2, 16, &[tone(0.1, 0.5)]);
        let wav = probe(&wav).unwrap();
        assert!(!wav.same_format(&cbr) && !cbr.same_format(&wav));
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
