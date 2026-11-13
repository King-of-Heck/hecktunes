//! Writing one track: a frame range of the recording, losslessly, as FLAC.

use super::wav::{RangeReader, WavInfo};
use anyhow::{anyhow, Context, Result};
use flacenc::component::BitRepr;
use flacenc::error::{SourceError, Verify};
use std::path::Path;

/// Feeds the encoder from the WAV a block at a time, so a long track is never all in memory as PCM.
struct WavSource {
    reader: RangeReader,
    channels: usize,
    bits: usize,
    rate: usize,
    buf: Vec<i32>,
}

impl flacenc::source::Source for WavSource {
    fn channels(&self) -> usize {
        self.channels
    }
    fn bits_per_sample(&self) -> usize {
        self.bits
    }
    fn sample_rate(&self) -> usize {
        self.rate
    }
    fn read_samples<F: flacenc::source::Fill>(&mut self, block_size: usize, dest: &mut F) -> Result<usize, SourceError> {
        self.buf.clear();
        let frames = self
            .reader
            .read(block_size, &mut self.buf)
            .map_err(|e| SourceError::from_io_error(std::io::Error::other(format!("{e:#}"))))?;
        if frames > 0 {
            dest.fill_interleaved(&self.buf)?;
        }
        Ok(frames)
    }
}

/// Writes frames `[start, end)` of the recording to `dest` as FLAC.
pub fn encode_range(info: &WavInfo, start: u64, end: u64, dest: &Path) -> Result<()> {
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|e| anyhow!("FLAC encoder settings rejected: {e:?}"))?;
    let source = WavSource {
        reader: RangeReader::open(info, start, end)?,
        channels: info.channels as usize,
        bits: info.bits as usize,
        rate: info.sample_rate as usize,
        buf: Vec::new(),
    };
    let mut stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| anyhow!("FLAC encoding failed: {e:?}"))?;
    // flacenc records the short last block as the minimum block size, which makes the stream look
    // variable-blocksize; symphonia (our player) then rejects every fixed-size frame. The FLAC spec
    // excludes the last block from the minimum, so min == max is the correct fixed-size declaration.
    let max_block = stream.stream_info().max_block_size();
    stream
        .stream_info_mut()
        .set_block_sizes(max_block, max_block)
        .map_err(|e| anyhow!("FLAC encoding failed: {e:?}"))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream.write(&mut sink).map_err(|e| anyhow!("FLAC encoding failed: {e:?}"))?;
    std::fs::write(dest, sink.as_slice()).with_context(|| format!("can't write {}", dest.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{tone, write_pattern_wav};
    use crate::vinyl::wav::{probe, read_range};
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    /// (rate, channels, interleaved samples as i32 scaled to the full i32 range).
    fn decode(path: &Path) -> (u32, usize, Vec<i32>) {
        let mss = MediaSourceStream::new(Box::new(std::fs::File::open(path).unwrap()), Default::default());
        let mut hint = Hint::new();
        hint.with_extension("flac");
        let mut format = symphonia::default::get_probe().probe(&hint, mss, FormatOptions::default(), MetadataOptions::default()).unwrap();
        let track = format.default_track(TrackType::Audio).unwrap();
        let id = track.id;
        let params = track.codec_params.as_ref().unwrap().audio().unwrap().clone();
        let mut dec = symphonia::default::get_codecs().make_audio_decoder(&params, &AudioDecoderOptions::default()).unwrap();
        let (mut rate, mut ch, mut out) = (0, 0, Vec::new());
        while let Some(packet) = format.next_packet().unwrap() {
            if packet.track_id != id {
                continue;
            }
            let buf = dec.decode(&packet).unwrap();
            rate = buf.spec().rate();
            ch = buf.spec().channels().count();
            let mut s = vec![0i32; buf.samples_interleaved()];
            buf.copy_to_slice_interleaved(&mut s);
            out.extend(s);
        }
        (rate, ch, out)
    }

    #[test]
    fn a_range_round_trips_losslessly() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("side.wav");
        write_pattern_wav(&wav, 44100, 2, 24, &[tone(3.0, 0.4)]);
        let info = probe(&wav).unwrap();
        let flac = dir.path().join("01.flac");
        encode_range(&info, 44100, 88200, &flac).unwrap();
        let (rate, ch, got) = decode(&flac);
        assert_eq!((rate, ch, got.len()), (44100, 2, 44100 * 2));
        let want = read_range(&info, 44100, 88200).unwrap();
        assert!(got.iter().zip(&want).all(|(g, w)| g >> 8 == *w), "samples differ");
    }
}
