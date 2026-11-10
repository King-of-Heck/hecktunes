#![cfg(test)]
use std::path::Path;

/// Writes a mono 8 kHz 16-bit sine WAV of `secs` seconds.
pub fn write_wav(path: &Path, secs: u32) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 8000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for i in 0..(8000 * secs) {
        let s = ((i as f32 * 440.0 * std::f32::consts::TAU / 8000.0).sin() * 8000.0) as i16;
        w.write_sample(s).unwrap();
    }
    w.finalize().unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use lofty::prelude::*;

    #[test]
    fn wav_helper_writes_readable_file_of_expected_length() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        write_wav(&p, 2);
        let tf = lofty::read_from_path(&p).unwrap();
        assert_eq!(tf.properties().duration().as_millis(), 2000);
    }
}
