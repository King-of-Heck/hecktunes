use anyhow::{anyhow, Context, Result};
use base64::Engine;
use rusty_chromaprint::{Configuration, FingerprintCompressor, Fingerprinter};
use std::fs::File;
use std::path::Path;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

pub const MAX_SECONDS: u64 = 120;

fn compress(config: &Configuration, fingerprint: &[u32]) -> String {
    let compressed = FingerprintCompressor::from(config).compress(fingerprint);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(compressed)
}

/// Fingerprint of interleaved 16-bit samples; only the first MAX_SECONDS are used.
pub fn fingerprint_pcm(samples: &[i16], sample_rate: u32, channels: u32) -> Result<String> {
    let config = Configuration::preset_test2();
    let mut printer = Fingerprinter::new(&config);
    printer.start(sample_rate, channels).map_err(|e| anyhow!("fingerprinter: {e:?}"))?;
    let budget = (MAX_SECONDS * sample_rate as u64 * channels as u64) as usize;
    printer.consume(&samples[..samples.len().min(budget)]);
    printer.finish();
    Ok(compress(&config, printer.fingerprint()))
}

pub fn fingerprint_file(path: &Path) -> Result<String> {
    let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .context("unrecognised audio format")?;
    let track = format.default_track(TrackType::Audio).ok_or_else(|| anyhow!("no audio track"))?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| anyhow!("no audio codec parameters"))?
        .clone();
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .context("no decoder for this codec")?;

    let config = Configuration::preset_test2();
    let mut printer = Fingerprinter::new(&config);
    let mut started = false;
    let mut budget: u64 = 0;
    let mut samples: Vec<i16> = Vec::new();

    while let Some(packet) = format.next_packet()? {
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                if !started {
                    let rate = buf.spec().rate();
                    let channels = buf.spec().channels().count() as u32;
                    printer.start(rate, channels).map_err(|e| anyhow!("fingerprinter: {e:?}"))?;
                    budget = MAX_SECONDS * rate as u64 * channels as u64;
                    started = true;
                }
                samples.resize(buf.samples_interleaved(), 0);
                buf.copy_to_slice_interleaved(&mut samples);
                let take = (samples.len() as u64).min(budget) as usize;
                printer.consume(&samples[..take]);
                budget -= take as u64;
                if budget == 0 {
                    break;
                }
            }
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    if !started {
        return Err(anyhow!("no audio decoded"));
    }
    printer.finish();
    Ok(compress(&config, printer.fingerprint()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::write_wav;

    #[test]
    fn fingerprints_a_wav() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        write_wav(&p, 10);
        let fp = fingerprint_file(&p).unwrap();
        assert!(fp.len() > 10);
        assert!(fp.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn missing_file_errors() {
        assert!(fingerprint_file(Path::new("does/not/exist.flac")).is_err());
    }

    #[test]
    fn pcm_fingerprint_matches_the_file_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        write_wav(&p, 10);
        let samples: Vec<i16> = hound::WavReader::open(&p).unwrap().samples::<i16>().map(|s| s.unwrap()).collect();
        assert_eq!(fingerprint_pcm(&samples, 8000, 1).unwrap(), fingerprint_file(&p).unwrap());
    }
}
