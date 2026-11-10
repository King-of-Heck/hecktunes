use crate::library::{AlbumEdit, AlbumRow, Library, TrackEdit, TrackRow};
use crate::model::TrackTags;
use crate::tags::write_tags;
use anyhow::{anyhow, Result};

fn tags_for(album: &AlbumRow, track: &TrackRow, all: &[TrackRow]) -> TrackTags {
    let disc_total = all.iter().map(|t| t.disc).max().unwrap_or(1);
    TrackTags {
        title: track.title.clone(),
        artist: track.artist.clone(),
        album: album.title.clone(),
        album_artist: album.album_artist.clone(),
        track_no: track.number,
        track_total: all.iter().filter(|t| t.disc == track.disc).count() as u32,
        disc_no: track.disc,
        disc_total,
        year: album.year.clone(),
        genre: album.genre.clone(),
        mb_release_id: album.mb_release_id.clone(),
        mb_recording_id: track.mb_recording_id.clone(),
    }
}

fn rewrite_album_files(lib: &Library, album_id: i64) -> Result<()> {
    let album = lib.album(album_id)?.ok_or_else(|| anyhow!("album {album_id} not found"))?;
    let tracks = lib.album_tracks(album_id)?;
    for t in &tracks {
        write_tags(&t.file_path, &tags_for(&album, t, &tracks), None)?;
    }
    Ok(())
}

/// Updates DB fields and rewrites tags in the album's files. Files are not renamed.
pub fn apply_album_edit(lib: &mut Library, album_id: i64, edit: &AlbumEdit) -> Result<()> {
    lib.update_album_fields(album_id, edit)?;
    rewrite_album_files(lib, album_id)
}

pub fn apply_track_edit(lib: &mut Library, track_id: i64, edit: &TrackEdit) -> Result<()> {
    lib.update_track_fields(track_id, edit)?;
    let track = lib.track(track_id)?.ok_or_else(|| anyhow!("track {track_id} not found"))?;
    let album = lib.album(track.album_id)?.ok_or_else(|| anyhow!("album {} not found", track.album_id))?;
    let all = lib.album_tracks(track.album_id)?;
    write_tags(&track.file_path, &tags_for(&album, &track, &all), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::{apply::apply, scan::scan_paths};
    use crate::tags::read_source_track;
    use crate::test_support::tagged_wav;

    #[test]
    fn album_edit_updates_db_and_every_file() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("s");
        std::fs::create_dir(&src).unwrap();
        tagged_wav(&src.join("1.wav"), 1, "a", "Old", "Band", 1);
        tagged_wav(&src.join("2.wav"), 1, "b", "Old", "Band", 2);
        let mut lib = Library::open_in_memory(dir.path().join("lib")).unwrap();
        let group = scan_paths(&[src]).groups.remove(0);
        let id = apply(&mut lib, &group, None, None, true).unwrap();

        apply_album_edit(&mut lib, id, &AlbumEdit { title: "New".into(), album_artist: "Other".into(), year: Some("1999".into()), genre: None }).unwrap();

        let row = lib.album(id).unwrap().unwrap();
        assert_eq!(row.title, "New");
        assert!(!row.needs_metadata);
        for t in lib.album_tracks(id).unwrap() {
            let s = read_source_track(&t.file_path).unwrap();
            assert_eq!(s.album.as_deref(), Some("New"));
            assert_eq!(s.album_artist.as_deref(), Some("Other"));
            assert_eq!(s.year.as_deref(), Some("1999"));
        }
    }

    #[test]
    fn track_edit_updates_db_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("s");
        std::fs::create_dir(&src).unwrap();
        tagged_wav(&src.join("1.wav"), 1, "a", "Alb", "Band", 1);
        let mut lib = Library::open_in_memory(dir.path().join("lib")).unwrap();
        let group = scan_paths(&[src]).groups.remove(0);
        let id = apply(&mut lib, &group, None, None, true).unwrap();
        let t = lib.album_tracks(id).unwrap().remove(0);

        apply_track_edit(&mut lib, t.id, &TrackEdit { title: "Better".into(), artist: "Guest".into(), disc: 1, number: 4 }).unwrap();

        let s = read_source_track(&t.file_path).unwrap();
        assert_eq!((s.title.as_deref(), s.artist.as_deref(), s.track_no), (Some("Better"), Some("Guest"), Some(4)));
        assert_eq!(lib.track(t.id).unwrap().unwrap().title, "Better");
    }
}
