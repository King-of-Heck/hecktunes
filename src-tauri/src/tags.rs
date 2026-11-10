use crate::model::{SourceTrack, TrackTags};
use anyhow::{Context, Result};
use lofty::config::WriteOptions;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::prelude::*;
use lofty::tag::{ItemKey, Tag};
use std::borrow::Cow;
use std::path::Path;

pub const SUPPORTED_EXTS: [&str; 6] = ["flac", "mp3", "m4a", "aac", "opus", "wav"];
pub const KNOWN_UNSUPPORTED: [&str; 9] = ["ogg", "wma", "ape", "dsf", "dff", "aif", "aiff", "wv", "mpc"];

pub fn extension(path: &Path) -> Option<String> {
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase())
}

fn clean(v: Option<Cow<'_, str>>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub fn read_source_track(path: &Path) -> Result<SourceTrack> {
    let tf = lofty::read_from_path(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut t = SourceTrack {
        path: path.to_path_buf(),
        title: None,
        artist: None,
        album: None,
        album_artist: None,
        track_no: None,
        disc_no: None,
        year: None,
        genre: None,
        duration_ms: tf.properties().duration().as_millis() as u64,
    };
    if let Some(tag) = tf.primary_tag().or_else(|| tf.first_tag()) {
        t.title = clean(tag.title());
        t.artist = clean(tag.artist());
        t.album = clean(tag.album());
        t.genre = clean(tag.genre());
        t.album_artist = clean(tag.get_string(ItemKey::AlbumArtist).map(Cow::Borrowed));
        t.year = tag
            .get_string(ItemKey::RecordingDate)
            .or_else(|| tag.get_string(ItemKey::Year))
            .and_then(|d| d.get(0..4))
            .filter(|y| y.chars().all(|c| c.is_ascii_digit()))
            .map(str::to_string);
        t.track_no = tag.track();
        t.disc_no = tag.disk();
    }
    Ok(t)
}

pub fn write_tags(path: &Path, t: &TrackTags, cover_jpeg: Option<&[u8]>) -> Result<()> {
    let mut tf = lofty::read_from_path(path).with_context(|| format!("cannot read {}", path.display()))?;
    let tag_type = tf.primary_tag_type();
    if tf.tag(tag_type).is_none() {
        tf.insert_tag(Tag::new(tag_type));
    }
    let tag = tf.tag_mut(tag_type).expect("tag was just inserted");
    tag.set_title(t.title.clone());
    tag.set_artist(t.artist.clone());
    tag.set_album(t.album.clone());
    tag.insert_text(ItemKey::AlbumArtist, t.album_artist.clone());
    tag.set_track(t.track_no);
    if t.track_total > 0 {
        tag.set_track_total(t.track_total);
    }
    tag.set_disk(t.disc_no);
    if t.disc_total > 0 {
        tag.set_disk_total(t.disc_total);
    }
    // Absent values are removed, so a rewrite never leaves stale data from an earlier tag behind.
    tag.remove_key(ItemKey::Year);
    tag.remove_key(ItemKey::RecordingDate);
    if let Some(y) = &t.year {
        tag.insert_text(ItemKey::RecordingDate, y.clone());
    }
    tag.remove_key(ItemKey::Genre);
    if let Some(g) = &t.genre {
        tag.set_genre(g.clone());
    }
    tag.remove_key(ItemKey::MusicBrainzReleaseId);
    if let Some(id) = &t.mb_release_id {
        tag.insert_text(ItemKey::MusicBrainzReleaseId, id.clone());
    }
    tag.remove_key(ItemKey::MusicBrainzRecordingId);
    if let Some(id) = &t.mb_recording_id {
        tag.insert_text(ItemKey::MusicBrainzRecordingId, id.clone());
    }
    if let Some(jpeg) = cover_jpeg {
        tag.remove_picture_type(PictureType::CoverFront);
        tag.push_picture(
            Picture::unchecked(jpeg.to_vec())
                .pic_type(PictureType::CoverFront)
                .mime_type(MimeType::Jpeg)
                .build(),
        );
    }
    tag.save_to_path(path, WriteOptions::default())
        .with_context(|| format!("cannot write tags to {}", path.display()))?;
    Ok(())
}

pub fn embedded_cover(path: &Path) -> Option<Vec<u8>> {
    let tf = lofty::read_from_path(path).ok()?;
    let pictures = || tf.tags().iter().flat_map(|t| t.pictures().iter());
    pictures()
        .find(|p| p.pic_type() == PictureType::CoverFront)
        .or_else(|| pictures().next())
        .map(|p| p.data().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::write_wav;

    fn full_tags() -> TrackTags {
        TrackTags {
            title: "Airbag".into(),
            artist: "Radiohead".into(),
            album: "OK Computer".into(),
            album_artist: "Radiohead".into(),
            track_no: 1,
            track_total: 12,
            disc_no: 1,
            disc_total: 1,
            year: Some("1997".into()),
            genre: Some("rock".into()),
            mb_release_id: Some("rel-1".into()),
            mb_recording_id: Some("rec-1".into()),
        }
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        write_wav(&p, 2);
        write_tags(&p, &full_tags(), Some(&[0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3])).unwrap();

        let s = read_source_track(&p).unwrap();
        assert_eq!(s.title.as_deref(), Some("Airbag"));
        assert_eq!(s.artist.as_deref(), Some("Radiohead"));
        assert_eq!(s.album.as_deref(), Some("OK Computer"));
        assert_eq!(s.album_artist.as_deref(), Some("Radiohead"));
        assert_eq!(s.track_no, Some(1));
        assert_eq!(s.disc_no, Some(1));
        assert_eq!(s.year.as_deref(), Some("1997"));
        assert_eq!(s.genre.as_deref(), Some("rock"));
        assert_eq!(s.duration_ms, 2000);
        assert_eq!(embedded_cover(&p), Some(vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3]));
    }

    #[test]
    fn untagged_file_reads_as_empty_fields() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("b.wav");
        write_wav(&p, 1);
        let s = read_source_track(&p).unwrap();
        assert_eq!(s.title, None);
        assert_eq!(s.album, None);
        assert_eq!(s.duration_ms, 1000);
        assert_eq!(embedded_cover(&p), None);
    }

    #[test]
    fn writing_without_cover_keeps_existing_cover() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.wav");
        write_wav(&p, 1);
        write_tags(&p, &full_tags(), Some(&[9, 9, 9])).unwrap();
        let mut t = full_tags();
        t.title = "Renamed".into();
        write_tags(&p, &t, None).unwrap();
        assert_eq!(read_source_track(&p).unwrap().title.as_deref(), Some("Renamed"));
        assert_eq!(embedded_cover(&p), Some(vec![9, 9, 9]));
    }

    #[test]
    fn extension_is_lowercased_and_support_lists_are_right() {
        assert_eq!(extension(Path::new("x/Song.FLAC")).as_deref(), Some("flac"));
        assert_eq!(extension(Path::new("noext")), None);
        assert!(SUPPORTED_EXTS.contains(&"opus"));
        assert!(KNOWN_UNSUPPORTED.contains(&"wma"));
    }

    #[test]
    fn unreadable_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.flac");
        std::fs::write(&p, b"not audio").unwrap();
        assert!(read_source_track(&p).is_err());
    }

    #[test]
    fn none_fields_are_removed_from_the_tag() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("d.wav");
        write_wav(&p, 1);
        write_tags(&p, &full_tags(), None).unwrap();
        let cleared = TrackTags { year: None, genre: None, mb_release_id: None, mb_recording_id: None, ..full_tags() };
        write_tags(&p, &cleared, None).unwrap();

        let s = read_source_track(&p).unwrap();
        assert_eq!(s.year, None);
        assert_eq!(s.genre, None);
        assert_eq!(s.title.as_deref(), Some("Airbag"));
        let tf = lofty::read_from_path(&p).unwrap();
        let tag = tf.primary_tag().unwrap();
        for key in [ItemKey::Year, ItemKey::RecordingDate, ItemKey::MusicBrainzReleaseId, ItemKey::MusicBrainzRecordingId] {
            assert_eq!(tag.get_string(key.clone()), None, "{key:?} cleared");
        }
    }
}
