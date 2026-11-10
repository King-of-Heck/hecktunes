use crate::artwork::{find_folder_cover, prepare_cover};
use crate::import::scan::AlbumGroup;
use crate::library::{paths, Library, NewAlbum, NewTrack};
use crate::model::{ReleaseCandidate, TrackTags};
use crate::tags::{embedded_cover, extension, write_tags};
use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

const UNKNOWN_ARTIST: &str = "Unknown Artist";
const VARIOUS_ARTISTS: &str = "Various Artists";

pub fn plan_tags(group: &AlbumGroup, release: Option<&ReleaseCandidate>) -> Vec<TrackTags> {
    match release {
        Some(r) => plan_from_release(group, r),
        None => plan_from_source(group),
    }
}

fn plan_from_release(group: &AlbumGroup, r: &ReleaseCandidate) -> Vec<TrackTags> {
    let fallback = plan_from_source(group);
    group
        .tracks
        .iter()
        .enumerate()
        .map(|(i, _)| match r.tracks.get(i) {
            Some(rt) => TrackTags {
                title: rt.title.clone(),
                artist: rt.artist.clone(),
                album: r.title.clone(),
                album_artist: r.artist.clone(),
                track_no: rt.number,
                track_total: r.tracks.iter().filter(|t| t.disc == rt.disc).count() as u32,
                disc_no: rt.disc,
                disc_total: r.disc_count,
                year: r.year(),
                genre: r.genre.clone(),
                mb_release_id: Some(r.mbid.clone()),
                mb_recording_id: Some(rt.recording_id.clone()).filter(|s| !s.is_empty()),
            },
            // More files than release tracks (user picked anyway): keep source data, but album-level fields from the release.
            None => TrackTags {
                album: r.title.clone(),
                album_artist: r.artist.clone(),
                year: r.year(),
                mb_release_id: Some(r.mbid.clone()),
                ..fallback[i].clone()
            },
        })
        .collect()
}

fn plan_from_source(group: &AlbumGroup) -> Vec<TrackTags> {
    let tracks = &group.tracks;
    let folder_name = group.source_dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let album = tracks.iter().find_map(|t| t.album.clone()).unwrap_or(folder_name);
    let artists: Vec<&String> = tracks.iter().filter_map(|t| t.artist.as_ref()).collect();
    let album_artist = tracks.iter().find_map(|t| t.album_artist.clone()).unwrap_or_else(|| {
        match artists.first() {
            None => UNKNOWN_ARTIST.to_string(),
            Some(first) if artists.iter().all(|a| a == first) => (*first).clone(),
            Some(_) => VARIOUS_ARTISTS.to_string(),
        }
    });
    let year = tracks.iter().find_map(|t| t.year.clone());
    let genre = tracks.iter().find_map(|t| t.genre.clone());
    let discs: Vec<u32> = tracks.iter().map(|t| t.disc_no.unwrap_or(1)).collect();
    let disc_total = discs.iter().copied().max().unwrap_or(1);
    tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let disc = discs[i];
            TrackTags {
                title: t.title.clone().unwrap_or_else(|| {
                    t.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
                }),
                artist: t.artist.clone().unwrap_or_else(|| album_artist.clone()),
                album: album.clone(),
                album_artist: album_artist.clone(),
                track_no: t.track_no.unwrap_or(i as u32 + 1),
                track_total: discs.iter().filter(|d| **d == disc).count() as u32,
                disc_no: disc,
                disc_total,
                year: year.clone(),
                genre: genre.clone(),
                mb_release_id: None,
                mb_recording_id: None,
            }
        })
        .collect()
}

pub fn pick_cover(group: &AlbumGroup, online: Option<Vec<u8>>) -> Option<Vec<u8>> {
    online
        .or_else(|| group.tracks.iter().take(4).find_map(|t| embedded_cover(&t.path)))
        .or_else(|| find_folder_cover(&group.source_dir))
}

fn unique_dir(root: &Path, rel: PathBuf) -> PathBuf {
    if !root.join(&rel).exists() {
        return rel;
    }
    let name = rel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let parent = rel.parent().map(Path::to_path_buf).unwrap_or_default();
    (2..).map(|n| parent.join(format!("{name} [{n}]"))).find(|p| !root.join(p).exists()).expect("unbounded range")
}

pub fn apply(
    lib: &mut Library,
    group: &AlbumGroup,
    release: Option<&ReleaseCandidate>,
    cover_raw: Option<Vec<u8>>,
    needs_metadata: bool,
) -> Result<i64> {
    let tags = plan_tags(group, release);
    let first = tags.first().ok_or_else(|| anyhow::anyhow!("album has no tracks"))?;
    let rel_dir = unique_dir(lib.root(), paths::album_dir(&first.album_artist, &first.album, first.year.as_deref()));
    let abs_dir = lib.root().join(&rel_dir);
    fs::create_dir_all(&abs_dir)?;
    let result = (|| -> Result<i64> {
        let cover = cover_raw.and_then(|c| prepare_cover(&c).ok());
        let cover_rel = match &cover {
            Some(bytes) => {
                fs::write(abs_dir.join("cover.jpg"), bytes)?;
                Some(rel_dir.join("cover.jpg"))
            }
            None => None,
        };
        let album_id = lib.insert_album(&NewAlbum {
            title: first.album.clone(),
            album_artist: first.album_artist.clone(),
            year: first.year.clone(),
            genre: first.genre.clone(),
            mb_release_id: first.mb_release_id.clone(),
            folder: rel_dir.clone(),
            cover: cover_rel,
            needs_metadata,
        })?;
        for (src, t) in group.tracks.iter().zip(&tags) {
            let ext = extension(&src.path).unwrap_or_default();
            let name = paths::track_file_name(t.disc_no, t.disc_total, t.track_no, &t.title, &ext);
            let dest = abs_dir.join(&name);
            fs::copy(&src.path, &dest)?;
            write_tags(&dest, t, cover.as_deref())?;
            lib.insert_track(&NewTrack {
                album_id,
                disc: t.disc_no,
                number: t.track_no,
                title: t.title.clone(),
                artist: t.artist.clone(),
                duration_ms: src.duration_ms,
                format: ext,
                file: rel_dir.join(&name),
                size_bytes: fs::metadata(&dest)?.len(),
                mb_recording_id: t.mb_recording_id.clone(),
            })?;
        }
        Ok(album_id)
    })();
    if result.is_err() {
        if let Some(id) = lib.list_albums().ok().and_then(|a| a.into_iter().find(|r| r.folder_path == abs_dir).map(|r| r.id)) {
            let _ = lib.delete_album(id, false);
        }
        let _ = fs::remove_dir_all(&abs_dir);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::scan::scan_paths;
    use crate::library::Library;
    use crate::tags::{embedded_cover, read_source_track};
    use crate::test_support::{candidate, png_bytes, tagged_wav, write_wav};
    use std::fs;

    fn setup() -> (tempfile::TempDir, std::path::PathBuf, Library) {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir(&src).unwrap();
        let lib = Library::open_in_memory(dir.path().join("lib")).unwrap();
        (dir, src, lib)
    }

    #[test]
    fn applies_release_tags_layout_and_cover() {
        let (_d, src, mut lib) = setup();
        tagged_wav(&src.join("x1.wav"), 1, "one", "album?", "band", 1);
        tagged_wav(&src.join("x2.wav"), 2, "two", "album?", "band", 2);
        let group = scan_paths(&[src.clone()]).groups.remove(0);
        let rel = candidate("mb-1", "Album: Deluxe", "Band", &[("One", 1000), ("Two", 2000)]);

        let id = apply(&mut lib, &group, Some(&rel), Some(png_bytes(900, 900)), false).unwrap();

        let dir = lib.root().join("Band").join("Album_ Deluxe (2001)");
        let f1 = dir.join("01 One.wav");
        assert!(f1.exists() && dir.join("02 Two.wav").exists() && dir.join("cover.jpg").exists());
        let t = read_source_track(&f1).unwrap();
        assert_eq!(t.title.as_deref(), Some("One"));
        assert_eq!(t.album.as_deref(), Some("Album: Deluxe"));
        assert_eq!(t.year.as_deref(), Some("2001"));
        assert!(embedded_cover(&f1).is_some());
        assert!(src.join("x1.wav").exists(), "original untouched");
        assert_eq!(read_source_track(&src.join("x1.wav")).unwrap().title.as_deref(), Some("one"));

        let row = lib.album(id).unwrap().unwrap();
        assert_eq!(row.mb_release_id.as_deref(), Some("mb-1"));
        assert!(!row.needs_metadata);
        assert_eq!(row.track_count, 2);
        assert_eq!(row.cover_path, Some(dir.join("cover.jpg")));
    }

    #[test]
    fn without_release_uses_source_tags_and_fallbacks() {
        let (_d, src, mut lib) = setup();
        write_wav(&src.join("Intro.wav"), 1);
        let group = scan_paths(&[src.clone()]).groups.remove(0);
        let id = apply(&mut lib, &group, None, None, true).unwrap();
        let row = lib.album(id).unwrap().unwrap();
        assert_eq!(row.title, "src");
        assert_eq!(row.album_artist, "Unknown Artist");
        assert!(row.needs_metadata);
        assert!(row.cover_path.is_none());
        assert!(lib.root().join("Unknown Artist").join("src").join("01 Intro.wav").exists());
    }

    #[test]
    fn mixed_artists_without_album_artist_become_various_artists() {
        let (_d, src, _lib) = setup();
        tagged_wav(&src.join("1.wav"), 1, "a", "Comp", "Ann", 1);
        tagged_wav(&src.join("2.wav"), 1, "b", "Comp", "Bob", 2);
        let mut group = scan_paths(&[src.clone()]).groups.remove(0);
        for t in &mut group.tracks {
            t.album_artist = None;
        }
        let tags = plan_tags(&group, None);
        assert!(tags.iter().all(|t| t.album_artist == "Various Artists"));
        assert_eq!(tags[1].artist, "Bob");
        assert_eq!(tags[0].track_total, 2);
    }

    #[test]
    fn existing_folder_gets_suffix() {
        let (_d, src, mut lib) = setup();
        write_wav(&src.join("a.wav"), 1);
        let group = scan_paths(&[src.clone()]).groups.remove(0);
        apply(&mut lib, &group, None, None, true).unwrap();
        let id2 = apply(&mut lib, &group, None, None, true).unwrap();
        let row = lib.album(id2).unwrap().unwrap();
        assert!(row.folder_path.ends_with("src [2]"));
    }

    #[test]
    fn pick_cover_prefers_online_then_embedded_then_folder() {
        let (_d, src, _lib) = setup();
        write_wav(&src.join("a.wav"), 1);
        fs::write(src.join("folder.png"), b"folderbytes").unwrap();
        let group = scan_paths(&[src.clone()]).groups.remove(0);
        assert_eq!(pick_cover(&group, Some(vec![1])), Some(vec![1]));
        assert_eq!(pick_cover(&group, None), Some(b"folderbytes".to_vec()));
    }
}
