//! Cutting an MP3 recording into MP3 tracks without re-encoding: the source's audio frames are
//! copied byte for byte behind a new Xing/Info header frame.
use super::header::{self, layer3_frame_len};
use super::index::Index;
use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::Path;

/// Audio frames [first, last) for recording samples [start, end): each end moves to the nearest
/// frame start; start == 0 gives frame 0 and end >= total_samples() gives frames.len().
pub fn frame_span(index: &Index, start: u64, end: u64) -> Range<usize> {
    let n = index.frames.len();
    let spf = index.samples_per_frame as u64;
    let nearest = |s: u64| (((s + index.delay as u64) + spf / 2) / spf).min(n as u64) as usize;
    let first = if start == 0 { 0 } else { nearest(start) };
    let last = if end >= index.total_samples() { n } else { nearest(end) };
    first..last.max(first)
}

/// The span's audio frames in the source: (offset of the first, total length).
fn audio_range(index: &Index, span: &Range<usize>) -> (u64, u64) {
    let first = index.frames[span.start];
    let last = index.frames[span.end - 1];
    (first.offset, last.offset + last.len as u64 - first.offset)
}

/// The header frame's 4 header bytes, bitrate index, tag position and length.
fn xing_layout(index: &Index) -> Result<([u8; 4], u8, usize, usize)> {
    let mut h = index.first_header;
    h[1] |= 0x01; // no CRC
    h[2] &= !0x02; // no padding
    let parsed = header::parse(h).context("the first MP3 frame header is not valid")?;
    let tag = 4 + parsed.side_info_len();
    let needed = (tag + 16) as u32;
    let b = (1..=14u8)
        .find(|&b| layer3_frame_len(parsed.version, b, parsed.sample_rate, false) >= needed)
        .context("no MP3 bitrate is large enough for a header frame")?;
    h[2] = (h[2] & 0x0F) | (b << 4);
    let len = layer3_frame_len(parsed.version, b, parsed.sample_rate, false) as usize;
    Ok((h, b, tag, len))
}

/// Size of the cut file: the Xing/Info frame plus the span's audio frames.
pub fn cut_bytes(index: &Index, span: Range<usize>) -> u64 {
    if span.is_empty() {
        return 0;
    }
    let xing = xing_layout(index).map(|(_, _, _, len)| len as u64).unwrap_or(0);
    xing + audio_range(index, &span).1
}

fn write_inner(source: &Path, index: &Index, span: &Range<usize>, dest: &Path) -> Result<()> {
    let (h, _, tag, len) = xing_layout(index)?;
    let (offset, audio) = audio_range(index, span);
    let mut frame = vec![0u8; len];
    frame[..4].copy_from_slice(&h);
    frame[tag..tag + 4].copy_from_slice(if index.variable_bitrate { b"Xing" } else { b"Info" });
    frame[tag + 4..tag + 8].copy_from_slice(&3u32.to_be_bytes());
    frame[tag + 8..tag + 12].copy_from_slice(&(span.len() as u32).to_be_bytes());
    frame[tag + 12..tag + 16].copy_from_slice(&((len as u64 + audio) as u32).to_be_bytes());

    let mut src = File::open(source).with_context(|| format!("cannot open {}", source.display()))?;
    src.seek(SeekFrom::Start(offset))?;
    let mut out = BufWriter::new(File::create(dest).with_context(|| format!("cannot create {}", dest.display()))?);
    out.write_all(&frame)?;
    let copied = io::copy(&mut src.take(audio), &mut out)?;
    if copied != audio {
        bail!("{} ended before the track's audio did", source.display());
    }
    out.flush()?;
    Ok(())
}

/// Writes the span as an MP3 at `dest`: a new Xing/Info frame, then the source bytes unchanged.
/// Removes `dest` on failure. Errors on an empty span.
pub fn write_cut(source: &Path, index: &Index, span: Range<usize>, dest: &Path) -> Result<()> {
    if span.is_empty() || span.end > index.frames.len() {
        bail!("there is no audio in this track");
    }
    let result = write_inner(source, index, &span, dest);
    if result.is_err() {
        let _ = std::fs::remove_file(dest);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::super::header::Version;
    use super::super::index::{scan, Frame};
    use super::*;
    use crate::vinyl::source::{self, Format};
    use std::path::PathBuf;
    use std::sync::Arc;
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vinyl").join(name)
    }

    fn synthetic() -> Index {
        Index {
            version: Version::Mpeg1,
            sample_rate: 44100,
            channels: 2,
            samples_per_frame: 1152,
            delay: 1105,
            padding: 600,
            frames: (0..100).map(|i| Frame { offset: 1000 + i * 417, len: 417 }).collect(),
            variable_bitrate: false,
            first_header: [0xFF, 0xFB, 0x90, 0x00],
        }
    }

    fn probed(name: &str) -> (source::SourceInfo, Arc<Index>) {
        let info = source::probe(&fixture(name)).unwrap();
        let Format::Mp3(ix) = info.format.clone() else { panic!("not MP3") };
        (info, ix)
    }

    const NAMES: [&str; 3] = ["cbr.mp3", "vbr.mp3", "mono22k.mp3"];

    #[test]
    fn span_rounds_to_the_nearest_frame() {
        let ix = synthetic();
        assert_eq!(frame_span(&ix, 0, ix.total_samples()), 0..100);
        assert_eq!(10 * 1152 - 1105, 10415);
        assert_eq!(frame_span(&ix, 10415 + 500, 30_000).start, 10);
        assert_eq!(frame_span(&ix, 10415 + 600, 30_000).start, 11);
        assert!(frame_span(&ix, 30_000, 30_000).is_empty());
    }

    #[test]
    fn cut_is_a_byte_copy_with_a_xing_frame() {
        for name in NAMES {
            let (info, ix) = probed(name);
            let rate = info.sample_rate as u64;
            let span = frame_span(&ix, 3 * rate, 9 * rate);
            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("cut.mp3");
            write_cut(&info.path, &ix, span.clone(), &out).unwrap();
            let bytes = std::fs::read(&out).unwrap();
            assert_eq!(bytes.len() as u64, cut_bytes(&ix, span.clone()), "{name}");

            let src = std::fs::read(&info.path).unwrap();
            let a = ix.frames[span.start].offset as usize;
            let last = ix.frames[span.end - 1];
            let b = (last.offset + last.len as u64) as usize;
            let h = header::parse(bytes[..4].try_into().unwrap()).unwrap();
            assert_eq!(&bytes[h.frame_len() as usize..], &src[a..b], "{name}");

            let cut = scan(&out).unwrap();
            assert_eq!(cut.frames.len(), span.len(), "{name}");
            assert_eq!(cut.variable_bitrate, ix.variable_bitrate, "{name}");

            let tag = 4 + h.side_info_len();
            let want: &[u8] = if name == "vbr.mp3" { b"Xing" } else { b"Info" };
            assert_eq!(&bytes[tag..tag + 4], want, "{name}");
            let be = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
            assert_eq!(be(tag + 4), 3, "{name}");
            assert_eq!(be(tag + 8) as usize, span.len(), "{name}");
            assert_eq!(be(tag + 12) as usize, bytes.len(), "{name}");
        }
    }

    #[test]
    fn cut_decodes_to_the_right_length() {
        for name in NAMES {
            let (info, ix) = probed(name);
            let rate = info.sample_rate as u64;
            let span = frame_span(&ix, 3 * rate, 9 * rate);
            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("cut.mp3");
            write_cut(&info.path, &ix, span.clone(), &out).unwrap();

            let file = File::open(&out).unwrap();
            let mss = MediaSourceStream::new(Box::new(file), Default::default());
            let mut hint = Hint::new();
            hint.with_extension("mp3");
            let mut format = symphonia::default::get_probe()
                .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
                .unwrap();
            let track = format.default_track(TrackType::Audio).unwrap();
            let id = track.id;
            let num_frames = track.num_frames;
            let params = track.codec_params.as_ref().and_then(|p| p.audio()).unwrap().clone();
            let mut decoder = symphonia::default::get_codecs().make_audio_decoder(&params, &AudioDecoderOptions::default()).unwrap();
            let mut decoded = 0u64;
            while let Some(packet) = format.next_packet().unwrap() {
                if packet.track_id == id {
                    decoded += decoder.decode(&packet).unwrap().frames() as u64;
                }
            }
            let spf = ix.samples_per_frame as u64;
            let want = span.len() as u64 * spf;
            assert!(decoded.abs_diff(want) <= spf, "{name}: decoded {decoded}, want {want}");
            assert_eq!(num_frames, Some(want), "{name}");
        }
    }

    #[test]
    fn empty_span_is_refused() {
        let (info, ix) = probed("cbr.mp3");
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("cut.mp3");
        assert!(write_cut(&info.path, &ix, 5..5, &out).is_err());
        assert!(!out.exists());
    }

    #[test]
    fn xing_frame_fits_at_low_rates() {
        let (_, ix) = probed("mono22k.mp3");
        let (h, b, tag, len) = xing_layout(&ix).unwrap();
        assert!(b > 1, "bitrate index {b}");
        assert!(len >= tag + 16);
        assert_eq!(header::parse(h).unwrap().frame_len() as usize, len);
    }
}
