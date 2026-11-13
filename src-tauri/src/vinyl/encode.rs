//! Writing one track: a frame range of the recording, losslessly, as FLAC.

use super::source::{RangeReader, SourceInfo};
use anyhow::{anyhow, bail, ensure, Context as _, Result};
use flacenc::bitsink::ByteSink;
use flacenc::component::{BitRepr, Stream, StreamInfo};
use flacenc::error::Verify;
use flacenc::source::{Context, Fill, FrameBuf};
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

/// "fLaC" + the STREAMINFO block header + STREAMINFO itself.
const HEADER_BYTES: usize = 4 + 4 + 34;

fn flac_err(e: impl std::fmt::Debug) -> anyhow::Error {
    anyhow!("FLAC encoding failed: {e:?}")
}

/// Writes frames `[start, end)` of the recording to `dest` as FLAC.
pub fn encode_range(info: &SourceInfo, start: u64, end: u64, dest: &Path) -> Result<()> {
    if start >= end {
        bail!("Nothing to encode: the range {start}..{end} is empty.");
    }
    let written = write_flac(info, start, end, dest);
    if written.is_err() {
        let _ = std::fs::remove_file(dest);
    }
    written
}

/// Encodes a block at a time and writes each frame straight to the file, so neither the PCM nor the
/// encoded track is ever all in memory. STREAMINFO is written last, over a placeholder at the start.
fn write_flac(info: &SourceInfo, start: u64, end: u64, dest: &Path) -> Result<()> {
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|e| anyhow!("FLAC encoder settings rejected: {e:?}"))?;
    let block = config.block_size;
    let (channels, bits) = (info.channels as usize, info.bits as usize);
    let mut stream_info = StreamInfo::new(info.sample_rate as usize, channels, bits).map_err(flac_err)?;
    stream_info.set_block_sizes(block, block).map_err(flac_err)?;
    let mut reader = RangeReader::open(info, start, end)?;
    let mut fill = (FrameBuf::with_size(channels, block).map_err(flac_err)?, Context::new(bits, channels));
    let cant_write = || format!("can't write {}", dest.display());
    let mut out = BufWriter::new(File::create(dest).with_context(cant_write)?);
    out.write_all(&[0; HEADER_BYTES]).with_context(cant_write)?;
    let (mut pcm, mut sink) = (Vec::with_capacity(block * channels), ByteSink::new());
    loop {
        pcm.clear();
        if reader.read(block, &mut pcm)? == 0 {
            break;
        }
        fill.fill_interleaved(&pcm).map_err(flac_err)?;
        let number = fill.1.current_frame_number().expect("a block was just read");
        let frame = flacenc::encode_fixed_size_frame(&config, &fill.0, number, &stream_info).map_err(flac_err)?;
        stream_info.update_frame_info(&frame);
        sink.clear();
        frame.write(&mut sink).map_err(flac_err)?;
        out.write_all(sink.as_slice()).with_context(cant_write)?;
    }
    ensure!(fill.1.total_samples() > 0, "Nothing to encode: no audio in that range.");
    // update_frame_info records the short last block as the minimum block size, which makes the
    // stream look variable-blocksize; symphonia (our player) then rejects every fixed-size frame. The
    // FLAC spec excludes the last block from the minimum, so min == max is the fixed-size declaration.
    stream_info.set_block_sizes(block, block).map_err(flac_err)?;
    stream_info.set_total_samples(fill.1.total_samples());
    stream_info.set_md5_digest(&fill.1.md5_digest());
    sink.clear();
    Stream::with_stream_info(stream_info).write(&mut sink).map_err(flac_err)?;
    ensure!(sink.as_slice().len() == HEADER_BYTES, "FLAC header is {} bytes, expected {HEADER_BYTES}", sink.as_slice().len());
    let mut file = out.into_inner().map_err(|e| e.into_error()).with_context(cant_write)?;
    file.seek(SeekFrom::Start(0)).with_context(cant_write)?;
    file.write_all(sink.as_slice()).with_context(cant_write)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{tone, write_pattern_wav};
    use crate::vinyl::source::{probe, read_range};
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

    /// Encodes `[start, end)` of a 16-bit mono WAV and checks it decodes to exactly those samples.
    fn round_trip_16_bit_mono(start: u64, end: u64) {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("side.wav");
        write_pattern_wav(&wav, 8000, 1, 16, &[tone(2.0, 0.4)]);
        let info = probe(&wav).unwrap();
        let flac = dir.path().join("01.flac");
        encode_range(&info, start, end, &flac).unwrap();
        let (rate, ch, got) = decode(&flac);
        assert_eq!((rate, ch, got.len() as u64), (8000, 1, end - start));
        let want = read_range(&info, start, end).unwrap();
        assert!(got.iter().zip(&want).all(|(g, w)| g >> 16 == *w), "samples differ");
    }

    #[test]
    fn a_16_bit_mono_range_round_trips() {
        round_trip_16_bit_mono(1000, 13_345);
    }

    #[test]
    fn a_range_shorter_than_one_block_round_trips() {
        round_trip_16_bit_mono(500, 1500);
    }

    #[test]
    fn an_empty_range_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("side.wav");
        write_pattern_wav(&wav, 8000, 1, 16, &[tone(1.0, 0.4)]);
        let info = probe(&wav).unwrap();
        let flac = dir.path().join("01.flac");
        let e = format!("{:#}", encode_range(&info, 100, 100, &flac).unwrap_err());
        assert!(e.contains("empty"), "{e}");
        assert!(encode_range(&info, 200, 100, &flac).is_err());
        assert!(!flac.exists());
    }
}
