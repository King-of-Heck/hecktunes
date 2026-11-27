use crate::artwork::prepare_cover;
use crate::library::hash::file_hash;
use crate::library::{AacCopy, AlbumEdit, AlbumRow, Library, TrackEdit, TrackRow};
use crate::model::TrackTags;
use crate::tags::write_tags;
use anyhow::{anyhow, Result};

pub(crate) fn tags_for(album: &AlbumRow, track: &TrackRow, all: &[TrackRow]) -> TrackTags {
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

/// Writes the same tags (and cover, when given) into the track's AAC copy and records its new size
/// and hash. A copy whose file has gone is left for Convert to AAC to remake.
fn rewrite_aac_copy(lib: &mut Library, track: &TrackRow, tags: &TrackTags, cover: Option<&[u8]>) -> Result<()> {
    let Some(copy) = track.aac.as_ref().filter(|c| c.path.is_file()) else { return Ok(()) };
    let written = write_tags(&copy.path, tags, cover);
    let size = std::fs::metadata(&copy.path)?.len();
    lib.set_track_aac(track.id, Some(&AacCopy { path: copy.path.clone(), size, hash: file_hash(&copy.path)? }))?;
    written
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
        let tags = tags_for(&album, t, &tracks);
        if let Err(e) = write_tags(&t.file_path, &tags, None) {
            record_all(lib, &tracks);
            return Err(e);
        }
        record_file_state(lib, t)?;
        rewrite_aac_copy(lib, t, &tags, None)?;
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
    let tags = tags_for(&album, &track, &all);
    if let Err(e) = write_tags(&track.file_path, &tags, None) {
        if let Err(re) = record_file_state(lib, &track) {
            eprintln!("could not record file state for track {}: {re}", track.id);
        }
        return Err(e);
    }
    record_file_state(lib, &track)?;
    rewrite_aac_copy(lib, &track, &tags, None)
}

/// Replaces the album's cover: writes `cover.jpg` in its folder, embeds the same JPEG in every
/// track, and points the album at the file. A file that isn't an image changes nothing.
pub fn set_album_cover(lib: &mut Library, album_id: i64, image: &[u8]) -> Result<()> {
    let cover = prepare_cover(image).map_err(|_| anyhow!("That file isn't an image HeckTunes can read"))?;
    let album = lib.album(album_id)?.ok_or_else(|| anyhow!("album {album_id} not found"))?;
    let tracks = lib.album_tracks(album_id)?;
    let target = album.folder_path.join("cover.jpg");
    let tmp = album.folder_path.join("cover.jpg.tmp");
    if let Err(e) = std::fs::write(&tmp, &cover).and_then(|_| std::fs::rename(&tmp, &target)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    lib.set_album_cover_path(album_id, Some(&target))?;
    for t in &tracks {
        let tags = tags_for(&album, t, &tracks);
        if let Err(e) = write_tags(&t.file_path, &tags, Some(&cover)) {
            record_all(lib, &tracks);
            return Err(e);
        }
        record_file_state(lib, t)?;
        rewrite_aac_copy(lib, t, &tags, Some(&cover))?;
    }
    Ok(())
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

    fn lib_with_album(cover: bool) -> (tempfile::TempDir, Library, i64) {
        let dir = tempfile::tempdir().unwrap();
        let mut lib = Library::open_in_memory(dir.path().join("lib")).unwrap();
        let id = crate::test_support::add_album(&mut lib, dir.path(), "Alb", "Band", 2, cover);
        (dir, lib, id)
    }

    #[test]
    fn set_cover_writes_a_downsized_cover_jpg_and_points_the_album_at_it() {
        let (_dir, mut lib, id) = lib_with_album(true);
        let folder = lib.album(id).unwrap().unwrap().folder_path;

        set_album_cover(&mut lib, id, &crate::test_support::png_bytes(1200, 900)).unwrap();

        let written = std::fs::read(folder.join("cover.jpg")).unwrap();
        let img = image::load_from_memory(&written).unwrap();
        assert!(img.width() <= 600 && img.height() <= 600);
        assert_eq!(img.width(), 600);
        assert_eq!(lib.album(id).unwrap().unwrap().cover_path, Some(folder.join("cover.jpg")));
        assert!(!folder.join("cover.jpg.tmp").exists());
    }

    #[test]
    fn set_cover_sets_cover_path_when_the_album_had_none() {
        let (_dir, mut lib, id) = lib_with_album(false);
        assert!(lib.album(id).unwrap().unwrap().cover_path.is_none());
        set_album_cover(&mut lib, id, &crate::test_support::png_bytes(64, 64)).unwrap();
        let row = lib.album(id).unwrap().unwrap();
        assert_eq!(row.cover_path, Some(row.folder_path.join("cover.jpg")));
    }

    #[test]
    fn set_cover_embeds_the_new_cover_in_every_track_and_keeps_hashes_true() {
        let (_dir, mut lib, id) = lib_with_album(true);
        let folder = lib.album(id).unwrap().unwrap().folder_path;

        set_album_cover(&mut lib, id, &crate::test_support::png_bytes(300, 300)).unwrap();

        let jpeg = std::fs::read(folder.join("cover.jpg")).unwrap();
        let tracks = lib.album_tracks(id).unwrap();
        assert_eq!(tracks.len(), 2);
        for t in &tracks {
            assert_eq!(crate::tags::embedded_cover(&t.file_path).as_deref(), Some(jpeg.as_slice()));
            assert_state_is_real(&lib, t.id);
        }
    }

    #[test]
    fn set_cover_with_a_non_image_changes_nothing() {
        let (_dir, mut lib, id) = lib_with_album(true);
        let before_album = lib.album(id).unwrap().unwrap();
        let cover_before = std::fs::read(before_album.folder_path.join("cover.jpg")).unwrap();
        let before_tracks = lib.album_tracks(id).unwrap();
        let files_before: Vec<Vec<u8>> = before_tracks.iter().map(|t| std::fs::read(&t.file_path).unwrap()).collect();

        let err = set_album_cover(&mut lib, id, b"definitely not an image").unwrap_err();

        assert!(err.to_string().contains("isn't an image HeckTunes can read"), "{err}");
        assert_eq!(std::fs::read(before_album.folder_path.join("cover.jpg")).unwrap(), cover_before);
        assert_eq!(lib.album(id).unwrap().unwrap().cover_path, before_album.cover_path);
        for (t, bytes) in before_tracks.iter().zip(&files_before) {
            assert_eq!(&std::fs::read(&t.file_path).unwrap(), bytes);
            let now = lib.track(t.id).unwrap().unwrap();
            assert_eq!((now.size_bytes, now.content_hash), (t.size_bytes, t.content_hash.clone()));
        }
    }

    #[test]
    fn edits_and_new_covers_reach_the_aac_copies_too() {
        use lofty::prelude::*;
        let Some(exe) = crate::convert::tests::test_ffmpeg() else { return };
        let dir = tempfile::tempdir().unwrap();
        let mut l = Library::open_in_memory(dir.path().join("lib")).unwrap();
        let a = crate::convert::tests::add_flac_album(&mut l, dir.path(), "Alb", "Band", 2, 44_100, 16, false);
        let lib = std::sync::Mutex::new(l);
        let cancel = std::sync::atomic::AtomicBool::new(false);
        crate::convert::convert_album(&lib, &exe, a, &crate::convert::ConvertOptions::default(), 2, &cancel, &|_, _| {}).unwrap();
        let mut l = lib.into_inner().unwrap();

        apply_album_edit(&mut l, a, &AlbumEdit { title: "Renamed".into(), album_artist: "Band".into(), year: None, genre: None }).unwrap();
        let first = l.album_tracks(a).unwrap()[0].clone();
        apply_track_edit(&mut l, first.id, &TrackEdit { title: "Opener".into(), artist: "Band".into(), disc: 1, number: 1 }).unwrap();
        set_album_cover(&mut l, a, &crate::test_support::png_bytes(32, 32)).unwrap();

        for t in l.album_tracks(a).unwrap() {
            let c = t.aac.unwrap();
            assert_eq!((c.size, c.hash.clone()), (std::fs::metadata(&c.path).unwrap().len(), file_hash(&c.path).unwrap()));
            let tf = lofty::read_from_path(&c.path).unwrap();
            let tag = tf.primary_tag().unwrap();
            assert_eq!(tag.album().as_deref(), Some("Renamed"));
            assert_eq!(tag.title().as_deref(), Some(t.title.as_str()));
            assert_eq!(tag.pictures().len(), 1);
        }
        assert_eq!(l.album_tracks(a).unwrap()[0].title, "Opener");
    }
}