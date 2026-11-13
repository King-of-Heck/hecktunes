//! Decoding sample ranges of an MP3 recording, positioned by the frame index.

use super::index::Index;
use anyhow::{Context, Result};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use symphonia::core::audio::{Channels, Position};
use symphonia::core::codecs::audio::well_known::CODEC_ID_MP3;
use symphonia::core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::packet::Packet;
use symphonia::core::units::{Duration, Timestamp};

/// A range read starts decoding this many frames before the frame holding its first sample, and
/// discards their output, so the bit reservoir and the filter banks have settled by then.
pub const MP3_PRIMING_FRAMES: usize = 10;

/// Reads samples `[start, end)` of an MP3 recording in chunks, as interleaved 16-bit integers.
///
/// Frame k of the index always contributes exactly `samples_per_frame` sample frames of decoded
/// output (zeros if it can't be decoded), so positions never drift from the index.
pub struct Mp3Reader {
    file: BufReader<File>,
    index: Arc<Index>,
    decoder: Box<dyn AudioDecoder>,
    channels: usize,
    /// Next index frame to decode.
    next: usize,
    /// Decoded sample frames still to throw away before delivering.
    skip: u64,
    /// Sample frames still to deliver.
    remaining: u64,
    /// Decoded interleaved samples not yet delivered, from `pending_at`.
    pending: Vec<i32>,
    pending_at: usize,
    bytes: Vec<u8>,
    decoded: Vec<i16>,
}

impl Mp3Reader {
    /// Samples [start, end) of the recording (clamped to index.total_samples()).
    pub fn open(path: &Path, index: Arc<Index>, start: u64, end: u64) -> Result<Self> {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let end = end.min(index.total_samples());
        let start = start.min(end);
        let spf = index.samples_per_frame as u64;
        let target = start + index.delay as u64;
        let begin = ((target / spf) as usize).saturating_sub(MP3_PRIMING_FRAMES);

        let channels = if index.channels == 1 {
            Channels::Positioned(Position::FRONT_CENTER)
        } else {
            Channels::Positioned(Position::FRONT_LEFT | Position::FRONT_RIGHT)
        };
        let mut params = AudioCodecParameters::new();
        params.for_codec(CODEC_ID_MP3).with_sample_rate(index.sample_rate).with_channels(channels);
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())
            .context("no MP3 decoder")?;

        let mut file = BufReader::new(File::open(path).with_context(|| format!("cannot open {name}"))?);
        if let Some(f) = index.frames.get(begin) {
            file.seek(SeekFrom::Start(f.offset)).with_context(|| format!("can't seek in {name}"))?;
        }
        Ok(Self {
            file,
            channels: index.channels as usize,
            index,
            decoder,
            next: begin,
            skip: target - begin as u64 * spf,
            remaining: end - start,
            pending: Vec::new(),
            pending_at: 0,
            bytes: Vec::new(),
            decoded: Vec::new(),
        })
    }

    /// Appends up to `max_frames` sample frames to `out`; returns the frames read (0 at the end).
    pub fn read(&mut self, max_frames: usize, out: &mut Vec<i32>) -> Result<usize> {
        let ch = self.channels;
        let mut done = 0;
        while done < max_frames && self.remaining > 0 {
            if self.pending_at == self.pending.len() {
                if self.next >= self.index.frames.len() {
                    // Can't happen while `end` is within total_samples(); stop rather than loop.
                    self.remaining = 0;
                    break;
                }
                self.decode_next()?;
                let drop = self.skip.min(self.index.samples_per_frame as u64);
                self.skip -= drop;
                self.pending_at = drop as usize * ch;
                continue;
            }
            let avail = (self.pending.len() - self.pending_at) / ch;
            let n = avail.min(max_frames - done).min(self.remaining as usize);
            out.extend_from_slice(&self.pending[self.pending_at..self.pending_at + n * ch]);
            self.pending_at += n * ch;
            self.remaining -= n as u64;
            done += n;
        }
        Ok(done)
    }

    /// Decodes index frame `next` into `pending`: exactly samples_per_frame sample frames.
    fn decode_next(&mut self) -> Result<()> {
        let frame = self.index.frames[self.next];
        self.next += 1;
        self.bytes.resize(frame.len as usize, 0);
        self.file.read_exact(&mut self.bytes).context("the MP3 file is shorter than when it was scanned")?;
        let spf = self.index.samples_per_frame as usize;
        let ch = self.channels;
        let packet = Packet::new(0, Timestamp::from(0i64), Duration::from(spf as u64), self.bytes.as_slice());
        self.pending.clear();
        self.pending_at = 0;
        match self.decoder.decode(&packet) {
            Ok(buf) if buf.spec().channels().count() == ch => {
                self.decoded.resize(buf.samples_interleaved(), 0);
                buf.copy_to_slice_interleaved(&mut self.decoded);
                let n = (self.decoded.len() / ch).min(spf);
                self.pending.extend(self.decoded[..n * ch].iter().map(|&s| s as i32));
            }
            // A frame that can't be decoded (e.g. its bit reservoir lies before a jump), or one
            // that came out with another channel layout, plays as silence.
            Ok(_) | Err(SymError::DecodeError(_)) => {}
            Err(e) => return Err(e).context("MP3 decoding failed"),
        }
        self.pending.resize(spf * ch, 0);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::vinyl::source::{probe, read_range, RangeReader};
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vinyl").join(name)
    }

    #[test]
    fn middle_read_equals_full_read() {
        for name in ["cbr.mp3", "mono22k.mp3"] {
            let info = probe(&fixture(name)).unwrap();
            let ch = info.channels as usize;
            let all = read_range(&info, 0, info.frames).unwrap();
            assert_eq!(all.len(), info.frames as usize * ch, "{name}");
            assert!(all.iter().any(|s| s.abs() > 1000), "{name}: silent");
            for (s, e) in [(0, 1000), (12_345, 40_000), (info.frames - 3000, info.frames)] {
                let got = read_range(&info, s, e).unwrap();
                assert!(got == all[s as usize * ch..e as usize * ch], "{name}: {s}..{e} differs");
            }
        }
    }

    /// The first `n` interleaved samples symphonia's own reader and decoder give, with gapless
    /// trimming on (`AudioDecoderOptions::gapless`, the default in 0.6.1).
    fn symphonia_gapless(path: &std::path::Path, n: usize) -> Vec<i32> {
        use symphonia::core::codecs::audio::AudioDecoderOptions;
        use symphonia::core::errors::Error as SymError;
        use symphonia::core::formats::probe::Hint;
        use symphonia::core::formats::{FormatOptions, TrackType};
        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::meta::MetadataOptions;
        let mss = MediaSourceStream::new(Box::new(std::fs::File::open(path).unwrap()), Default::default());
        let mut hint = Hint::new();
        hint.with_extension("mp3");
        let mut format = symphonia::default::get_probe()
            .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
            .unwrap();
        let track = format.default_track(TrackType::Audio).unwrap();
        let id = track.id;
        let params = track.codec_params.as_ref().and_then(|p| p.audio()).unwrap().clone();
        let opts = AudioDecoderOptions::default().gapless(true);
        let mut decoder = symphonia::default::get_codecs().make_audio_decoder(&params, &opts).unwrap();
        let (mut out, mut buf) = (Vec::new(), Vec::<i16>::new());
        while out.len() < n {
            let Some(packet) = format.next_packet().unwrap() else { break };
            if packet.track_id != id {
                continue;
            }
            match decoder.decode(&packet) {
                Ok(b) => {
                    buf.resize(b.samples_interleaved(), 0);
                    b.copy_to_slice_interleaved(&mut buf);
                    out.extend(buf.iter().map(|&s| s as i32));
                }
                Err(SymError::DecodeError(_)) => continue,
                Err(e) => panic!("{e}"),
            }
        }
        out.truncate(n);
        out
    }

    #[test]
    fn reads_line_up_with_symphonia_gapless_decoding() {
        for name in ["cbr.mp3", "mono22k.mp3"] {
            let info = probe(&fixture(name)).unwrap();
            let ch = info.channels as usize;
            let want = symphonia_gapless(&info.path, 50_000);
            assert_eq!(want.len(), 50_000, "{name}");
            let got = read_range(&info, 0, (50_000 / ch) as u64).unwrap();
            assert!(got == want, "{name}: differs from symphonia's gapless output");
        }
    }

    #[test]
    fn reads_in_small_chunks() {
        let info = probe(&fixture("cbr.mp3")).unwrap();
        let mut r = RangeReader::open(&info, 5000, 25_000).unwrap();
        let mut got = Vec::new();
        loop {
            let n = r.read(333, &mut got).unwrap();
            assert!(n <= 333);
            if n == 0 {
                break;
            }
        }
        assert!(got == read_range(&info, 5000, 25_000).unwrap());
    }

    #[test]
    fn reads_are_clamped_to_the_recording() {
        let info = probe(&fixture("cbr.mp3")).unwrap();
        assert_eq!(read_range(&info, info.frames - 10, info.frames + 5000).unwrap().len(), 20);
        assert!(read_range(&info, info.frames + 1, info.frames + 2).unwrap().is_empty());
    }
}
