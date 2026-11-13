//! Loudness of every 50 ms of a recording: the only view of the audio gap finding needs.

use super::wav::{RangeReader, WavInfo};
use anyhow::Result;

pub const FRAME_MS: u64 = 50;
/// Level reported for digital silence.
pub const SILENCE_DB: f32 = -120.0;

pub fn frame_to_ms(frame: usize) -> u64 {
    frame as u64 * FRAME_MS
}

pub fn ms_to_frame(ms: u64) -> usize {
    (ms / FRAME_MS) as usize
}

fn to_db(rms: f64) -> f32 {
    if rms <= 0.0 {
        SILENCE_DB
    } else {
        (20.0 * rms.log10()).max(SILENCE_DB as f64) as f32
    }
}

/// dBFS (RMS of the mono mix) of every 50 ms frame; the last frame may be shorter.
pub fn envelope(info: &WavInfo) -> Result<Vec<f32>> {
    let frame_len = ((info.sample_rate as u64 * FRAME_MS / 1000) as usize).max(1);
    let ch = info.channels as usize;
    let full = (1i64 << (info.bits - 1)) as f64;
    let mut reader = RangeReader::open(info, 0, info.frames)?;
    let mut buf = Vec::with_capacity(frame_len * ch);
    let mut out = Vec::with_capacity((info.frames / frame_len as u64 + 1) as usize);
    loop {
        buf.clear();
        let got = reader.read(frame_len, &mut buf)?;
        if got == 0 {
            break;
        }
        let sum: f64 = buf
            .chunks_exact(ch)
            .map(|f| {
                let m = f.iter().map(|&s| s as f64).sum::<f64>() / ch as f64 / full;
                m * m
            })
            .sum();
        out.push(to_db((sum / got as f64).sqrt()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{hiss, tone, write_pattern_wav};
    use crate::vinyl::wav::probe;

    #[test]
    fn measures_each_50ms_frame() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        write_pattern_wav(&p, 8000, 2, 16, &[tone(1.0, 0.5), hiss(1.0), tone(1.0, 0.5)]);
        let db = envelope(&probe(&p).unwrap()).unwrap();
        assert_eq!(db.len(), 60);
        // A sine with peak 0.5 has RMS 0.354 = -9.0 dBFS.
        for i in (1..19).chain(41..59) {
            assert!((db[i] + 9.0).abs() < 0.5, "frame {i}: {}", db[i]);
        }
        for i in 21..39 {
            assert!(db[i] < -55.0, "frame {i}: {}", db[i]);
        }
    }

    #[test]
    fn frame_and_ms_convert() {
        assert_eq!(frame_to_ms(20), 1000);
        assert_eq!(ms_to_frame(1049), 20);
    }
}
