//! Frame index of an MP3 file, built from the frame headers alone (no decoding).
use super::header::{self, Header, Version};
use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    pub offset: u64,
    pub len: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Index {
    pub version: Version,
    pub sample_rate: u32,
    pub channels: u16,
    pub samples_per_frame: u32,
    pub delay: u32,
    pub padding: u32,
    /// Audio frames only (a Xing/Info/VBRI header frame is left out), contiguous in the file.
    pub frames: Vec<Frame>,
    /// The audio frames don't all share one bitrate.
    pub variable_bitrate: bool,
    /// The 4 header bytes of the first audio frame.
    pub first_header: [u8; 4],
}

impl Index {
    /// Recording length in samples: frames x spf - delay - padding (not below 0).
    pub fn total_samples(&self) -> u64 {
        (self.frames.len() as u64 * self.samples_per_frame as u64)
            .saturating_sub(self.delay as u64)
            .saturating_sub(self.padding as u64)
    }

    /// First recording sample of frame k (k x spf - delay); negative for priming frames.
    pub fn frame_start(&self, k: usize) -> i64 {
        k as i64 * self.samples_per_frame as i64 - self.delay as i64
    }
}

/// Bytes taken by an ID3v2 tag at the start of the file (0 if none).
fn id3v2_len<R: Read>(r: &mut R) -> Result<u64> {
    let mut h = [0u8; 10];
    let mut got = 0;
    while got < 10 {
        let n = r.read(&mut h[got..])?;
        if n == 0 {
            break;
        }
        got += n;
    }
    if got < 10 || &h[..3] != b"ID3" {
        return Ok(0);
    }
    let size = h[6..10].iter().fold(0u64, |a, &b| (a << 7) | (b & 0x7F) as u64);
    let footer = if h[5] & 0x10 != 0 { 10 } else { 0 };
    Ok(10 + size + footer)
}

fn read4(r: &mut BufReader<File>, at: u64) -> Option<[u8; 4]> {
    r.seek(SeekFrom::Start(at)).ok()?;
    let mut b = [0u8; 4];
    r.read_exact(&mut b).ok()?;
    Some(b)
}

/// (delay, padding) from a Xing/Info frame's LAME extension, if it has one.
fn lame_trim(frame: &[u8], h: &Header) -> Option<(u32, u32)> {
    let tag = 4 + h.side_info_len();
    if frame.len() < tag + 8 {
        return None;
    }
    let flags = u32::from_be_bytes(frame[tag + 4..tag + 8].try_into().ok()?);
    let mut p = tag + 8;
    for (bit, n) in [(1, 4), (2, 4), (4, 100), (8, 4)] {
        if flags & bit != 0 {
            p += n;
        }
    }
    // 9 bytes encoder string, 12 bytes of other fields, then the 3-byte delay/padding.
    if frame.len() < p + 24 {
        return None;
    }
    let enc = &frame[p..p + 9];
    if !(enc.starts_with(b"LAME") || enc.starts_with(b"Lavf") || enc.starts_with(b"Lavc")) {
        return None;
    }
    let t = &frame[p + 21..p + 24];
    let trim = (t[0] as u32) << 16 | (t[1] as u32) << 8 | t[2] as u32;
    Some(((trim >> 12) + 529, (trim & 0xFFF).saturating_sub(529)))
}

/// Scans an MP3's frame headers without decoding.
pub fn scan(path: &Path) -> Result<Index> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let file = File::open(path).with_context(|| format!("cannot open {name}"))?;
    let file_len = file.metadata()?.len();
    let mut r = BufReader::new(file);
    let start = id3v2_len(&mut r)?;

    let mp3_name = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("mp3"));
    let Some(first) = read4(&mut r, start).and_then(header::parse) else {
        if start > 0 || mp3_name {
            bail!("{name} has no MP3 audio");
        }
        bail!("{name} is not a WAV or MP3 file HeckTunes can read");
    };
    if first.layer != 3 || first.bitrate_index == 0 {
        bail!("{name} is not an MP3 (Layer III) file");
    }

    let mut frames: Vec<Frame> = Vec::new();
    let mut bitrates: Vec<u8> = Vec::new();
    let mut offset = start;
    while let Some(h) = read4(&mut r, offset).and_then(header::parse) {
        if !h.same_stream(&first) || h.layer != 3 || h.bitrate_index == 0 {
            break;
        }
        let len = h.frame_len();
        if offset + len as u64 > file_len {
            break;
        }
        frames.push(Frame { offset, len });
        bitrates.push(h.bitrate_index);
        offset += len as u64;
    }
    if frames.is_empty() {
        // The first frame itself was cut short.
        bail!("{name} has no MP3 audio");
    }

    // The first frame may be a Xing/Info/VBRI header rather than audio.
    let (mut delay, mut padding) = (529, 0);
    r.seek(SeekFrom::Start(frames[0].offset))?;
    let mut buf = vec![0u8; frames[0].len as usize];
    r.read_exact(&mut buf)?;
    let tag = 4 + first.side_info_len();
    let is_xing = buf.len() >= tag + 4 && matches!(&buf[tag..tag + 4], b"Xing" | b"Info");
    let is_vbri = buf.len() >= 40 && &buf[36..40] == b"VBRI";
    if is_xing || is_vbri {
        if is_xing {
            if let Some((d, p)) = lame_trim(&buf, &first) {
                (delay, padding) = (d, p);
            }
        }
        frames.remove(0);
        bitrates.remove(0);
    }
    if frames.is_empty() {
        bail!("{name} has no MP3 audio");
    }

    let first_header = read4(&mut r, frames[0].offset).context("short read")?;
    Ok(Index {
        version: first.version,
        sample_rate: first.sample_rate,
        channels: first.channels(),
        samples_per_frame: first.samples_per_frame(),
        delay,
        padding,
        frames,
        variable_bitrate: bitrates.iter().any(|&b| b != bitrates[0]),
        first_header,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vinyl").join(name)
    }

    #[test]
    fn scan_matches_symphonia() {
        use symphonia::core::formats::probe::Hint;
        use symphonia::core::formats::{FormatOptions, TrackType};
        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::meta::MetadataOptions;
        for name in ["cbr.mp3", "vbr.mp3", "mono22k.mp3"] {
            let path = fixture(name);
            let index = scan(&path).unwrap();
            let file = std::fs::File::open(&path).unwrap();
            let mss = MediaSourceStream::new(Box::new(file), Default::default());
            let mut hint = Hint::new();
            hint.with_extension("mp3");
            let format = symphonia::default::get_probe()
                .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
                .unwrap();
            let track = format.default_track(TrackType::Audio).unwrap();
            let params = track.codec_params.as_ref().and_then(|p| p.audio()).unwrap();
            assert_eq!(index.total_samples(), track.num_frames.unwrap(), "{name} length");
            assert_eq!(index.delay, track.delay.unwrap(), "{name} delay");
            assert_eq!(index.padding, track.padding.unwrap(), "{name} padding");
            assert_eq!(Some(index.sample_rate), params.sample_rate, "{name} rate");
            assert_eq!(index.channels as usize, params.channels.as_ref().unwrap().count(), "{name} channels");
            let want = 21.0 * index.sample_rate as f64;
            let got = index.total_samples() as f64;
            assert!((got - want).abs() / want < 0.02, "{name}: {got} vs {want}");
        }
    }

    #[test]
    fn vbr_is_variable_and_cbr_is_not() {
        assert!(scan(&fixture("vbr.mp3")).unwrap().variable_bitrate);
        assert!(!scan(&fixture("cbr.mp3")).unwrap().variable_bitrate);
    }

    #[test]
    fn id3v2_is_skipped() {
        let bytes = std::fs::read(fixture("cbr.mp3")).unwrap();
        assert_eq!(&bytes[..3], b"ID3");
        let index = scan(&fixture("cbr.mp3")).unwrap();
        let off = index.frames[0].offset as usize;
        assert!(off > 10);
        let h = header::parse(bytes[off..off + 4].try_into().unwrap()).unwrap();
        assert_eq!(h.layer, 3);
        assert_eq!(index.first_header, <[u8; 4]>::try_from(&bytes[off..off + 4]).unwrap());
    }

    #[test]
    fn truncated_tail_keeps_whole_frames() {
        let bytes = std::fs::read(fixture("cbr.mp3")).unwrap();
        let full = scan(&fixture("cbr.mp3")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("cut.mp3");
        std::fs::write(&p, &bytes[..bytes.len() - 100]).unwrap();
        let cut = scan(&p).unwrap();
        assert_eq!(cut.frames.len(), full.frames.len() - 1);
    }

    #[test]
    fn frames_are_contiguous() {
        for name in ["cbr.mp3", "vbr.mp3", "mono22k.mp3"] {
            let index = scan(&fixture(name)).unwrap();
            for w in index.frames.windows(2) {
                assert_eq!(w[0].offset + w[0].len as u64, w[1].offset, "{name}");
            }
        }
    }

    #[test]
    fn layer2_is_refused() {
        let err = scan(&fixture("layer2.mp2")).unwrap_err().to_string();
        assert!(err.contains("layer2.mp2 is not an MP3 (Layer III) file"), "{err}");
    }

    #[test]
    fn no_frames() {
        let dir = tempfile::tempdir().unwrap();
        let mk = |name: &str, bytes: &[u8]| {
            let p = dir.path().join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        };
        let e = scan(&mk("empty.mp3", b"ID3\x04\0\0\0\0\0\0")).unwrap_err().to_string();
        assert!(e.contains("empty.mp3 has no MP3 audio"), "{e}");
        let e = scan(&mk("junk.mp3", b"hello world")).unwrap_err().to_string();
        assert!(e.contains("junk.mp3 has no MP3 audio"), "{e}");
        let e = scan(&mk("junk.bin", b"hello world")).unwrap_err().to_string();
        assert!(e.contains("junk.bin is not a WAV or MP3 file HeckTunes can read"), "{e}");
    }
}
