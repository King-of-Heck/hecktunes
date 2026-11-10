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

use crate::model::TrackTags;

/// WAV with album/artist/title/track tags written via tags::write_tags.
#[allow(dead_code)] // used by later tasks (import/library tests)
pub fn tagged_wav(path: &Path, secs: u32, title: &str, album: &str, artist: &str, track: u32) {
    write_wav(path, secs);
    crate::tags::write_tags(
        path,
        &TrackTags {
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            album_artist: artist.into(),
            track_no: track,
            track_total: 0,
            disc_no: 1,
            disc_total: 0,
            year: None,
            genre: None,
            mb_release_id: None,
            mb_recording_id: None,
        },
        None,
    )
    .unwrap();
}

pub fn png_bytes(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(w, h, image::Rgb([200, 30, 30]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img).write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

use crate::model::{CandidateTrack, ReleaseCandidate};

#[allow(dead_code)] // used by identify tests and later tasks
pub fn candidate(mbid: &str, title: &str, artist: &str, tracks: &[(&str, u64)]) -> ReleaseCandidate {
    ReleaseCandidate {
        mbid: mbid.into(),
        title: title.into(),
        artist: artist.into(),
        date: Some("2001-03-04".into()),
        country: Some("GB".into()),
        format: Some("CD".into()),
        genre: Some("rock".into()),
        disc_count: 1,
        tracks: tracks
            .iter()
            .enumerate()
            .map(|(i, (t, ms))| CandidateTrack {
                disc: 1,
                number: i as u32 + 1,
                title: t.to_string(),
                artist: artist.into(),
                length_ms: Some(*ms),
                recording_id: format!("rec-{mbid}-{i}"),
            })
            .collect(),
    }
}

use crate::identify::MetadataSource;
use crate::model::SourceTrack;

pub struct FakeSource {
    pub releases: Vec<ReleaseCandidate>,
    pub search_ids: Vec<String>,
}

impl MetadataSource for FakeSource {
    fn search(&self, _album: &str, _artist: &str, _track_count: usize) -> anyhow::Result<Vec<String>> {
        Ok(self.search_ids.clone())
    }
    fn search_text(&self, _query: &str) -> anyhow::Result<Vec<String>> {
        Ok(self.releases.iter().map(|r| r.mbid.clone()).collect())
    }
    fn by_fingerprint(&self, _track: &SourceTrack) -> anyhow::Result<Vec<String>> {
        Ok(vec![])
    }
    fn release(&self, mbid: &str) -> anyhow::Result<ReleaseCandidate> {
        self.releases.iter().find(|r| r.mbid == mbid).cloned().ok_or_else(|| anyhow::anyhow!("no release {mbid}"))
    }
    fn cover(&self, _mbid: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(Some(png_bytes(800, 800)))
    }
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
