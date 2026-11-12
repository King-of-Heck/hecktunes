use crate::library::hash::file_hash;
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

fn record_file_state(lib: &mut Library, track: &TrackRow) -> Result<()> {
    let size = std::fs::metadata(&track.file_path)?.len();
    lib.set_track_file_state(track.id, size, &file_hash(&track.file_path)?)
}

/// After a rewrite that may have stopped part-way, store each file's real size and hash.
fn record_all(lib: &mut Library, tracks: &[TrackRow]) {
    for t in tracks {
        if let Err(e) = record_file_state(lib, t) {
            eprintln!("could not record file state for track {}: {e}", t.id);
        }
    }
}

fn rewrite_album_files(lib: &mut Library, album_id: i64) -> Result<()> {
    let album = lib.album(album_id)?.ok_or_else(|| anyhow!("album {album_id} not found"))?;
    let tracks = lib.album_tracks(album_id)?;
    for t in &tracks {
        if let Err(e) = write_tags(&t.file_path, &tags_for(&album, t, &tracks), None) {
            record_all(lib, &tracks);
            return Err(e);
        }
        record_file_state(lib, t)?;
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
    if let Err(e) = write_tags(&track.file_path, &tags_for(&album, &track, &all), None) {
        if let Err(re) = record_file_state(lib, &track) {
            eprintln!("could not record file state for track {}: {re}", track.id);
        }
        return Err(e);
    }
    record_file_state(lib, &track)
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

    #[test]
    fn imports_and_edits_keep_hash_and_size_in_step_with_the_files() {
        use crate::library::hash::file_hash;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("s");
        std::fs::create_dir(&src).unwrap();
        tagged_wav(&src.join("1.wav"), 1, "a", "Alb", "Band", 1);
        tagged_wav(&src.join("2.wav"), 1, "b", "Alb", "Band", 2);
        let mut lib = Library::open_in_memory(dir.path().join("lib")).unwrap();
        let group = scan_paths(&[src]).groups.remove(0);
        let id = apply(&mut lib, &group, None, None, false).unwrap();
        let before = lib.album_tracks(id).unwrap();
        for t in &before {
            assert_eq!(t.content_hash.as_deref(), Some(file_hash(&t.file_path).unwrap().as_str()));
        }

        apply_album_edit(&mut lib, id, &AlbumEdit { title: "Alb".into(), album_artist: "Band".into(), year: None, genre: Some("Rock".into()) }).unwrap();

        for (b, t) in before.iter().zip(lib.album_tracks(id).unwrap()) {
            assert_ne!(b.content_hash, t.content_hash);
            assert_eq!(t.content_hash.as_deref(), Some(file_hash(&t.file_path).unwrap().as_str()));
            assert_eq!(t.size_bytes, std::fs::metadata(&t.file_path).unwrap().len());
        }
    }

    /// Imports two WAVs, stores a wrong state for the second, and makes that file read-only.
    fn album_with_unwritable_second_track() -> (tempfile::TempDir, Library, i64, Vec<TrackRow>) {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("s");
        std::fs::create_dir(&src).unwrap();
        tagged_wav(&src.join("1.wav"), 1, "a", "Alb", "Band", 1);
        tagged_wav(&src.join("2.wav"), 1, "b", "Alb", "Band", 2);
        let mut lib = Library::open_in_memory(dir.path().join("lib")).unwrap();
        let group = scan_paths(&[src]).groups.remove(0);
        let id = apply(&mut lib, &group, None, None, false).unwrap();
        let tracks = lib.album_tracks(id).unwrap();
        lib.set_track_file_state(tracks[1].id, 0, "stale").unwrap();
        let mut perms = std::fs::metadata(&tracks[1].file_path).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&tracks[1].file_path, perms).unwrap();
        (dir, lib, id, tracks)
    }

    fn make_writable(path: &std::path::Path) {
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(path, perms).unwrap();
    }

    fn assert_state_is_real(lib: &Library, track_id: i64) {
        use crate::library::hash::file_hash;
        let t = lib.track(track_id).unwrap().unwrap();
        assert_eq!(t.content_hash.as_deref(), Some(file_hash(&t.file_path).unwrap().as_str()));
        assert_eq!(t.size_bytes, std::fs::metadata(&t.file_path).unwrap().len());
    }

    #[test]
    fn a_failed_album_edit_still_records_the_files_real_state() {
        let (_dir, mut lib, id, tracks) = album_with_unwritable_second_track();

        let result = apply_album_edit(&mut lib, id, &AlbumEdit { title: "Alb".into(), album_artist: "Band".into(), year: Some("2001".into()), genre: None });

        make_writable(&tracks[1].file_path);
        assert!(result.is_err());
        for t in &tracks {
            assert_state_is_real(&lib, t.id);
        }
    }

    #[test]
    fn a_failed_track_edit_still_records_the_files_real_state() {
        let (_dir, mut lib, _id, tracks) = album_with_unwritable_second_track();

        let result = apply_track_edit(&mut lib, tracks[1].id, &TrackEdit { title: "New".into(), artist: "Band".into(), disc: 1, number: 2 });

        make_writable(&tracks[1].file_path);
        assert!(result.is_err());
        assert_state_is_real(&lib, tracks[1].id);
    }
}
