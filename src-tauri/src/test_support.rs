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


/// WAV with title/album/track tags but no disc number tag.
#[allow(dead_code)]
pub fn wav_without_disc(path: &Path, secs: u32, title: &str, album: &str, track: u32) {
    use lofty::prelude::*;
    write_wav(path, secs);
    let mut tf = lofty::read_from_path(path).unwrap();
    let tag_type = tf.primary_tag_type();
    tf.insert_tag(lofty::tag::Tag::new(tag_type));
    let tag = tf.tag_mut(tag_type).unwrap();
    tag.set_title(title.into());
    tag.set_album(album.into());
    tag.set_track(track);
    tag.save_to_path(path, lofty::config::WriteOptions::default()).unwrap();
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
                side: None,
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

/// Holds a file open (Windows) or makes the folder read-only (unix) so the folder can't be removed
/// while the guard lives.
pub struct FolderLock {
    #[cfg(windows)]
    _file: std::fs::File,
    #[cfg(unix)]
    dir: std::path::PathBuf,
}

pub fn lock_folder(dir: &Path) -> FolderLock {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        let file = std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()).find(|e| e.path().is_file()).unwrap().path();
        // Readers may still open it (so it can be copied), but nobody may delete it.
        let f = std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(file).unwrap();
        FolderLock { _file: f }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        FolderLock { dir: dir.to_path_buf() }
    }
}

#[cfg(unix)]
impl Drop for FolderLock {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o755));
    }
}

/// Album + track rows only (no files) for library/DB tests. Returns (album id, track ids).
/// Track n is titled "T<n>" with duration 1000*n ms.
pub fn add_memory_album(lib: &mut crate::library::Library, title: &str, artist: &str, tracks: u32) -> (i64, Vec<i64>) {
    use crate::library::{NewAlbum, NewTrack};
    let folder = std::path::PathBuf::from(artist).join(title);
    let id = lib
        .insert_album(&NewAlbum {
            title: title.into(),
            album_artist: artist.into(),
            year: None,
            genre: None,
            mb_release_id: None,
            folder: folder.clone(),
            cover: None,
            needs_metadata: false,
        })
        .unwrap();
    let ids = (1..=tracks)
        .map(|n| {
            lib.insert_track(&NewTrack {
                album_id: id,
                disc: 1,
                number: n,
                title: format!("T{n}"),
                artist: artist.into(),
                duration_ms: 1000 * n as u64,
                format: "flac".into(),
                file: folder.join(format!("{n:02} T{n}.flac")),
                size_bytes: 10,
                mb_recording_id: None,
                content_hash: None,
            })
            .unwrap()
        })
        .collect();
    (id, ids)
}

/// Imports a tagged WAV album into `lib` without any metadata lookup. Songs are "Song 1".."Song n",
/// 1 s each; the library folder is `<artist>/<album>`. Returns the album id.
pub fn add_album(lib: &mut crate::library::Library, src_parent: &Path, album: &str, artist: &str, n: u32, cover: bool) -> i64 {
    let src = src_parent.join(format!("src {artist} - {album}"));
    std::fs::create_dir_all(&src).unwrap();
    for i in 1..=n {
        tagged_wav(&src.join(format!("{i}.wav")), 1, &format!("Song {i}"), album, artist, i);
    }
    let group = crate::import::scan::scan_paths(&[src]).groups.remove(0);
    crate::import::apply::apply(lib, &group, None, cover.then(|| png_bytes(64, 64)), false).unwrap()
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


/// One stretch of a synthetic vinyl recording: `level` 0.0 is surface hiss (about -65 dBFS RMS),
/// otherwise a 440 Hz tone with that peak amplitude (0..1].
#[derive(Clone, Copy, Debug)]
pub struct Part {
    pub secs: f32,
    pub level: f32,
}

pub const fn tone(secs: f32, level: f32) -> Part {
    Part { secs, level }
}

pub const fn hiss(secs: f32) -> Part {
    Part { secs, level: 0.0 }
}

/// Writes a 16- or 24-bit integer WAV made of `parts`, the same signal on every channel.
pub fn write_pattern_wav(path: &Path, rate: u32, channels: u16, bits: u16, parts: &[Part]) {
    let spec = hound::WavSpec { channels, sample_rate: rate, bits_per_sample: bits, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    let full = ((1i64 << (bits - 1)) - 1) as f32;
    let mut seed: u32 = 12345;
    let mut n: u64 = 0;
    for p in parts {
        for _ in 0..(p.secs * rate as f32).round() as u64 {
            let v = if p.level == 0.0 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                ((seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0) * 0.001
            } else {
                (n as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * p.level
            };
            n += 1;
            let s = (v * full) as i32;
            for _ in 0..channels {
                w.write_sample(s).unwrap();
            }
        }
    }
    w.finalize().unwrap();
}

/// Envelope level used for vinyl hiss in synthetic envelopes.
pub const HISS_DB: f32 = -65.0;

/// A synthetic 50 ms envelope: `(seconds, dB)` stretches. Hiss-level stretches (<= -60 dB) wobble
/// by +/-1.5 dB like real surface noise.
pub fn db_frames(parts: &[(f32, f32)]) -> Vec<f32> {
    let mut out = Vec::new();
    let mut i: u32 = 0;
    for &(secs, db) in parts {
        for _ in 0..(secs * 20.0).round() as usize {
            let wobble = if db <= -60.0 { ((i * 7919) % 31) as f32 / 10.0 - 1.5 } else { 0.0 };
            out.push(db + wobble);
            i += 1;
        }
    }
    out
}

/// A music database for vinyl tests: every fingerprint finds `fingerprint_ids`; `offline` makes
/// every lookup fail like a dropped connection; a text search for `no_results_for` finds nothing
/// (any other search finds every release).
pub struct VinylFake {
    pub releases: Vec<ReleaseCandidate>,
    pub fingerprint_ids: Vec<String>,
    pub offline: bool,
    pub no_results_for: Option<&'static str>,
}

impl VinylFake {
    fn down<T>(&self) -> anyhow::Result<T> {
        Err(anyhow::Error::from(ureq::Error::HostNotFound).context("network down"))
    }
}

impl MetadataSource for VinylFake {
    fn search(&self, _a: &str, _b: &str, _n: usize) -> anyhow::Result<Vec<String>> {
        Ok(vec![])
    }
    fn search_text(&self, q: &str) -> anyhow::Result<Vec<String>> {
        if self.offline { return self.down(); }
        if self.no_results_for == Some(q) { return Ok(vec![]); }
        Ok(self.releases.iter().map(|r| r.mbid.clone()).collect())
    }
    fn by_fingerprint(&self, _t: &SourceTrack) -> anyhow::Result<Vec<String>> {
        Ok(vec![])
    }
    fn by_fingerprint_code(&self, _fp: &str, _secs: u32) -> anyhow::Result<Vec<String>> {
        if self.offline { return self.down(); }
        Ok(self.fingerprint_ids.clone())
    }
    fn release(&self, mbid: &str) -> anyhow::Result<ReleaseCandidate> {
        if self.offline { return self.down(); }
        self.releases.iter().find(|r| r.mbid == mbid).cloned().ok_or_else(|| anyhow::anyhow!("no release {mbid}"))
    }
    fn cover(&self, _mbid: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(Some(png_bytes(800, 800)))
    }
}
