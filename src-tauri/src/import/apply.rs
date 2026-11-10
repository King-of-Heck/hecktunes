use crate::artwork::{find_folder_cover, prepare_cover};
use crate::identify::score::DURATION_TOLERANCE_MS;
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

/// For each source track, the index of the release track it corresponds to (None if the release
/// has fewer tracks). Matches by (disc, track number) first, then by closest duration within the
/// scoring tolerance, then by position, never assigning one release track twice.
fn match_release_tracks(group: &AlbumGroup, r: &ReleaseCandidate) -> Vec<Option<usize>> {
    let n = group.tracks.len();
    let mut out: Vec<Option<usize>> = vec![None; n];
    let mut taken = vec![false; r.tracks.len()];
    for (i, s) in group.tracks.iter().enumerate() {
        let Some(number) = s.track_no else { continue };
        let disc = s.disc_no.unwrap_or(1);
        if let Some(j) = (0..r.tracks.len()).find(|&j| !taken[j] && r.tracks[j].disc == disc && r.tracks[j].number == number) {
            taken[j] = true;
            out[i] = Some(j);
        }
    }
    for (i, s) in group.tracks.iter().enumerate() {
        if out[i].is_some() || s.duration_ms == 0 {
            continue;
        }
        let closest = (0..r.tracks.len())
            .filter(|&j| !taken[j])
            .filter_map(|j| r.tracks[j].length_ms.map(|len| (j, len.abs_diff(s.duration_ms))))
            .filter(|&(_, diff)| diff as f64 <= DURATION_TOLERANCE_MS)
            .min_by_key(|&(_, diff)| diff);
        if let Some((j, _)) = closest {
            taken[j] = true;
            out[i] = Some(j);
        }
    }
    for i in 0..n {
        if out[i].is_some() {
            continue;
        }
        let j = Some(i).filter(|&j| j < taken.len() && !taken[j]).or_else(|| taken.iter().position(|t| !t));
        if let Some(j) = j {
            taken[j] = true;
            out[i] = Some(j);
        }
    }
    out
}

fn plan_from_release(group: &AlbumGroup, r: &ReleaseCandidate) -> Vec<TrackTags> {
    let fallback = plan_from_source(group);
    let matched = match_release_tracks(group, r);
    // The release wins, but a release without a year/genre keeps the source's own value: the copy's
    // tags are rewritten from this plan, so anything not carried here is gone.
    let year = r.year().or_else(|| fallback.first().and_then(|t| t.year.clone()));
    let genre = r.genre.clone().or_else(|| fallback.first().and_then(|t| t.genre.clone()));
    group
        .tracks
        .iter()
        .enumerate()
        .map(|(i, _)| match matched[i].map(|j| &r.tracks[j]) {
            Some(rt) => TrackTags {
                title: rt.title.clone(),
                artist: rt.artist.clone(),
                album: r.title.clone(),
                album_artist: r.artist.clone(),
                track_no: rt.number,
                track_total: r.tracks.iter().filter(|t| t.disc == rt.disc).count() as u32,
                disc_no: rt.disc,
                disc_total: r.disc_count,
                year: year.clone(),
                genre: genre.clone(),
                mb_release_id: Some(r.mbid.clone()),
                mb_recording_id: Some(rt.recording_id.clone()).filter(|s| !s.is_empty()),
            },
            // More files than release tracks (user picked anyway): keep source data, but album-level fields from the release.
            None => TrackTags {
                album: r.title.clone(),
                album_artist: r.artist.clone(),
                year: year.clone(),
                genre: genre.clone(),
                mb_release_id: Some(r.mbid.clone()),
                ..fallback[i].clone()
            },
        })
        .collect()
}

fn non_blank(s: &Option<String>) -> Option<String> {
    s.as_ref().map(|v| v.trim()).filter(|v| !v.is_empty()).map(str::to_string)
}

fn plan_from_source(group: &AlbumGroup) -> Vec<TrackTags> {
    let tracks = &group.tracks;
    let folder_name = group.source_dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let album = tracks.iter().find_map(|t| non_blank(&t.album)).unwrap_or(folder_name);
    let artists: Vec<&String> = tracks.iter().filter_map(|t| t.artist.as_ref().filter(|a| !a.trim().is_empty())).collect();
    let album_artist = tracks.iter().find_map(|t| non_blank(&t.album_artist)).unwrap_or_else(|| {
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
    // Position within the track's own disc (1-based, scan order), for files without a track tag.
    let mut seen_per_disc: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let disc = discs[i];
            let in_disc = seen_per_disc.entry(disc).or_insert(0);
            *in_disc += 1;
            let in_disc = *in_disc;
            TrackTags {
                title: non_blank(&t.title).unwrap_or_else(|| {
                    t.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
                }),
                artist: non_blank(&t.artist).unwrap_or_else(|| album_artist.clone()),
                album: album.clone(),
                album_artist: album_artist.clone(),
                track_no: t.track_no.unwrap_or(in_disc),
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
        // Merged disc folders (Album/CD1, ...) often keep the art next to the first disc's files.
        .or_else(|| group.tracks.first().and_then(|t| t.path.parent()).and_then(find_folder_cover))
}

/// Creates a fresh album folder under `root`, adding " [2]", " [3]"... if the name is taken.
/// `create_dir` fails on an existing folder, so the folder returned belongs to this import alone
/// even if something else creates folders concurrently.
fn reserve_dir(root: &Path, rel: PathBuf) -> Result<PathBuf> {
    let name = rel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let parent = rel.parent().map(Path::to_path_buf).unwrap_or_default();
    fs::create_dir_all(root.join(&parent))?;
    let candidates = std::iter::once(rel).chain((2..).map(|n| parent.join(format!("{name} [{n}]"))));
    for candidate in candidates {
        match fs::create_dir(root.join(&candidate)) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    unreachable!("unbounded range")
}

/// Disambiguate a destination file name already used in this album: "x.wav" -> "x (2).wav".
fn unique_name(used: &mut std::collections::HashSet<String>, name: String) -> String {
    let key = |s: &str| s.to_lowercase();
    if used.insert(key(&name)) {
        return name;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) => (&name[..i], &name[i..]),
        None => (name.as_str(), ""),
    };
    let n = (2..).map(|n| format!("{stem} ({n}){ext}")).find(|c| used.insert(key(c))).expect("unbounded range");
    n
}

/// Copies `src` to `dest` and makes the copy writable: on Windows `fs::copy` carries the source's
/// read-only attribute over, which would make tagging the copy fail. The source is not touched.
fn copy_writable(src: &Path, dest: &Path) -> Result<()> {
    fs::copy(src, dest)?;
    let mut perms = fs::metadata(dest)?.permissions();
    if perms.readonly() {
        #[allow(clippy::permissions_set_readonly_false)] // our own fresh copy, not a shared file
        perms.set_readonly(false);
        fs::set_permissions(dest, perms)?;
    }
    Ok(())
}

/// Removes a staged album folder, then its artist folder if that is now empty (`remove_dir` fails on a
/// non-empty folder, which is what we want; errors are ignored).
fn remove_staged_dir(abs_dir: &Path) {
    let _ = fs::remove_dir_all(abs_dir);
    if let Some(parent) = abs_dir.parent() {
        let _ = fs::remove_dir(parent);
    }
}

/// An album copied and tagged into its own new library folder, not yet in the database.
#[must_use = "commit or discard the staged album"]
pub struct Staged {
    abs_dir: PathBuf,
    album: NewAlbum,
    tracks: Vec<NewTrack>,
}

impl Staged {
    /// Inserts the album and its tracks in one transaction. On failure the staged folder is removed.
    pub fn commit(self, lib: &mut Library) -> Result<i64> {
        match lib.insert_album_with_tracks(&self.album, &self.tracks) {
            Ok(id) => Ok(id),
            Err(e) => {
                self.discard();
                Err(e)
            }
        }
    }

    pub fn discard(self) {
        remove_staged_dir(&self.abs_dir);
    }
}

/// Copies and tags the album's files into a new folder under `root`. Touches only the filesystem,
/// so callers can run it without holding the library lock. On failure nothing is left behind.
pub fn stage(
    root: &Path,
    group: &AlbumGroup,
    release: Option<&ReleaseCandidate>,
    cover_raw: Option<Vec<u8>>,
    needs_metadata: bool,
) -> Result<Staged> {
    let tags = plan_tags(group, release);
    let first = tags.first().ok_or_else(|| anyhow::anyhow!("album has no tracks"))?;
    let rel_dir = reserve_dir(root, paths::album_dir(&first.album_artist, &first.album, first.year.as_deref()))?;
    let abs_dir = root.join(&rel_dir);
    let result = (|| -> Result<Staged> {
        let cover = cover_raw.and_then(|c| prepare_cover(&c).ok());
        let cover_rel = match &cover {
            Some(bytes) => {
                fs::write(abs_dir.join("cover.jpg"), bytes)?;
                Some(rel_dir.join("cover.jpg"))
            }
            None => None,
        };
        let mut used = std::collections::HashSet::new();
        let mut tracks = Vec::with_capacity(tags.len());
        for (src, t) in group.tracks.iter().zip(&tags) {
            let ext = extension(&src.path).unwrap_or_default();
            let name = unique_name(&mut used, paths::track_file_name(t.disc_no, t.disc_total, t.track_no, &t.title, &ext));
            let dest = abs_dir.join(&name);
            copy_writable(&src.path, &dest)?;
            write_tags(&dest, t, cover.as_deref())?;
            tracks.push(NewTrack {
                album_id: 0,
                disc: t.disc_no,
                number: t.track_no,
                title: t.title.clone(),
                artist: t.artist.clone(),
                duration_ms: src.duration_ms,
                format: ext,
                file: rel_dir.join(&name),
                size_bytes: fs::metadata(&dest)?.len(),
                mb_recording_id: t.mb_recording_id.clone(),
            });
        }
        let album = NewAlbum {
            title: first.album.clone(),
            album_artist: first.album_artist.clone(),
            year: first.year.clone(),
            genre: first.genre.clone(),
            mb_release_id: first.mb_release_id.clone(),
            folder: rel_dir.clone(),
            cover: cover_rel,
            needs_metadata,
        };
        Ok(Staged { abs_dir: abs_dir.clone(), album, tracks })
    })();
    if result.is_err() {
        remove_staged_dir(&abs_dir);
    }
    result
}

/// Stages and commits in one go (holds `lib` throughout; the import worker uses `stage` + `commit`).
pub fn apply(
    lib: &mut Library,
    group: &AlbumGroup,
    release: Option<&ReleaseCandidate>,
    cover_raw: Option<Vec<u8>>,
    needs_metadata: bool,
) -> Result<i64> {
    let root = lib.root().to_path_buf();
    stage(&root, group, release, cover_raw, needs_metadata)?.commit(lib)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::scan::scan_paths;
    use crate::library::Library;
    use crate::tags::{embedded_cover, read_source_track, write_tags};
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

    #[test]
    fn failure_mid_album_cleans_up_row_and_folder() {
        let (_d, src, mut lib) = setup();
        write_wav(&src.join("a.wav"), 1);
        write_wav(&src.join("b.wav"), 1);
        let group = scan_paths(&[src.clone()]).groups.remove(0);
        let bad = group.tracks[1].path.clone();
        fs::write(&bad, b"not audio at all").unwrap();

        assert!(apply(&mut lib, &group, None, None, true).is_err());

        assert!(lib.list_albums().unwrap().is_empty());
        assert!(!lib.root().join("Unknown Artist").join("src").exists(), "new folder removed");
        assert!(group.tracks[0].path.exists() && bad.exists(), "originals untouched");
        assert_eq!(fs::read(&bad).unwrap(), b"not audio at all");
    }

    #[test]
    fn colliding_destination_names_are_disambiguated() {
        let (_d, src, mut lib) = setup();
        write_wav(&src.join("a.wav"), 1);
        write_wav(&src.join("b.wav"), 1);
        let mut group = scan_paths(&[src.clone()]).groups.remove(0);
        for t in &mut group.tracks {
            t.title = Some("Same".into());
            t.track_no = Some(1);
        }
        let id = apply(&mut lib, &group, None, None, true).unwrap();
        let dir = lib.root().join("Unknown Artist").join("src");
        assert!(dir.join("01 Same.wav").exists());
        assert!(dir.join("01 Same (2).wav").exists());
        assert_eq!(lib.album(id).unwrap().unwrap().track_count, 2);
    }

    #[test]
    fn blank_source_tags_fall_back() {
        let (_d, src, _lib) = setup();
        write_wav(&src.join("Intro.wav"), 1);
        let mut group = scan_paths(&[src.clone()]).groups.remove(0);
        let t = &mut group.tracks[0];
        t.title = Some("".into());
        t.album = Some("  ".into());
        t.artist = Some("".into());
        t.album_artist = Some(" ".into());
        let tags = plan_tags(&group, None);
        assert_eq!(tags[0].title, "Intro");
        assert_eq!(tags[0].album, "src");
        assert_eq!(tags[0].album_artist, "Unknown Artist");
        assert_eq!(tags[0].artist, "Unknown Artist");
    }

    #[test]
    fn stage_writes_files_without_db_and_reserves_distinct_folders() {
        let (_d, src, mut lib) = setup();
        write_wav(&src.join("a.wav"), 1);
        let group = scan_paths(&[src.clone()]).groups.remove(0);
        let root = lib.root().to_path_buf();
        // Two stagings before any commit (as two importers racing would) must not share a folder.
        let s1 = stage(&root, &group, None, None, true).unwrap();
        let s2 = stage(&root, &group, None, None, true).unwrap();
        assert!(root.join("Unknown Artist").join("src").join("01 a.wav").exists());
        assert!(root.join("Unknown Artist").join("src [2]").join("01 a.wav").exists());
        assert!(lib.list_albums().unwrap().is_empty(), "staging touches no rows");

        let id = s1.commit(&mut lib).unwrap();
        s2.discard();
        assert!(!root.join("Unknown Artist").join("src [2]").exists());
        let row = lib.album(id).unwrap().unwrap();
        assert_eq!(row.track_count, 1);
        assert!(row.folder_path.ends_with("src"));
    }

    #[test]
    fn untagged_track_numbers_restart_on_each_disc() {
        let (_d, src, mut lib) = setup();
        let album = src.join("Album");
        for (disc, n) in [("CD1", 2), ("CD2", 3)] {
            fs::create_dir_all(album.join(disc)).unwrap();
            for i in 1..=n {
                write_wav(&album.join(disc).join(format!("t{i}.wav")), 1);
            }
        }
        let group = scan_paths(&[album]).groups.remove(0);
        assert_eq!(group.tracks.len(), 5);
        let numbers: Vec<(u32, u32)> = plan_from_source(&group).iter().map(|t| (t.disc_no, t.track_no)).collect();
        assert_eq!(numbers, vec![(1, 1), (1, 2), (2, 1), (2, 2), (2, 3)]);
        let id = apply(&mut lib, &group, None, None, true).unwrap();
        let tracks = lib.album_tracks(id).unwrap();
        assert_eq!(tracks.iter().map(|t| (t.disc, t.number)).collect::<Vec<_>>(), numbers);
    }

    #[test]
    fn discarded_stage_removes_empty_artist_folder_but_keeps_a_used_one() {
        let (_d, src, lib) = setup();
        write_wav(&src.join("a.wav"), 1);
        let group = scan_paths(&[src.clone()]).groups.remove(0);
        let root = lib.root().to_path_buf();
        stage(&root, &group, None, None, true).unwrap().discard();
        assert!(!root.join("Unknown Artist").exists());
        let keep = stage(&root, &group, None, None, true).unwrap();
        stage(&root, &group, None, None, true).unwrap().discard();
        assert!(root.join("Unknown Artist").exists(), "artist folder still holds another album");
        keep.discard();
    }

    fn set_readonly(p: &std::path::Path, ro: bool) {
        let mut perms = fs::metadata(p).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(ro);
        fs::set_permissions(p, perms).unwrap();
    }

    #[test]
    fn read_only_source_imports_and_stays_read_only() {
        let (_d, src, mut lib) = setup();
        tagged_wav(&src.join("a.wav"), 1, "one", "Alb", "Band", 1);
        let source = src.join("a.wav");
        let before = fs::read(&source).unwrap();
        set_readonly(&source, true);
        let group = scan_paths(&[src.clone()]).groups.remove(0);
        let result = apply(&mut lib, &group, None, None, true);
        let still_ro = fs::metadata(&source).unwrap().permissions().readonly();
        set_readonly(&source, false); // let the temp dir be cleaned up
        let id = result.expect("read-only source imports");
        assert!(still_ro, "source keeps its read-only flag");
        assert_eq!(fs::read(&source).unwrap(), before, "source bytes unchanged");
        let t = lib.album_tracks(id).unwrap().remove(0);
        assert!(!fs::metadata(&t.file_path).unwrap().permissions().readonly(), "library copy is writable");
        assert_eq!(read_source_track(&t.file_path).unwrap().title.as_deref(), Some("one"));
    }

    const FOUR: [(&str, u64); 4] = [("One", 1000), ("Two", 2000), ("Three", 3000), ("Four", 4000)];

    #[test]
    fn release_tracks_matched_by_number_when_a_track_is_missing() {
        let (_d, src, _lib) = setup();
        tagged_wav(&src.join("a.wav"), 1, "x", "Alb", "Band", 1);
        tagged_wav(&src.join("b.wav"), 2, "y", "Alb", "Band", 2);
        tagged_wav(&src.join("c.wav"), 4, "z", "Alb", "Band", 4);
        let group = scan_paths(&[src.clone()]).groups.remove(0);
        let tags = plan_tags(&group, Some(&candidate("m", "Alb", "Band", &FOUR)));
        let got: Vec<_> = tags.iter().map(|t| (t.track_no, t.title.as_str())).collect();
        assert_eq!(got, vec![(1, "One"), (2, "Two"), (4, "Four")]);
        assert_eq!(tags[2].mb_recording_id.as_deref(), Some("rec-m-3"));
    }

    #[test]
    fn release_tracks_matched_by_duration_without_track_numbers() {
        let (_d, src, _lib) = setup();
        write_wav(&src.join("a.wav"), 3);
        write_wav(&src.join("b.wav"), 1);
        let group = scan_paths(&[src.clone()]).groups.remove(0);
        let tags = plan_tags(&group, Some(&candidate("m", "Alb", "Band", &FOUR)));
        let got: Vec<_> = tags.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(got, vec!["Three", "One"]);
    }

    #[test]
    fn release_tracks_fall_back_to_position_beyond_duration_tolerance() {
        let (_d, src, _lib) = setup();
        write_wav(&src.join("a.wav"), 30);
        write_wav(&src.join("b.wav"), 31);
        let group = scan_paths(&[src.clone()]).groups.remove(0);
        let tags = plan_tags(&group, Some(&candidate("m", "Alb", "Band", &FOUR)));
        let got: Vec<_> = tags.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(got, vec!["One", "Two"]);
    }

    #[test]
    fn pick_cover_finds_folder_art_inside_a_disc_folder() {
        let (_d, src, _lib) = setup();
        let cd1 = src.join("CD1");
        fs::create_dir(&cd1).unwrap();
        crate::test_support::wav_without_disc(&cd1.join("a.wav"), 1, "t", "Alb", 1);
        fs::write(cd1.join("folder.jpg"), b"discart").unwrap();
        let group = scan_paths(&[src.clone()]).groups.remove(0);
        assert_eq!(group.source_dir, src);
        assert_eq!(pick_cover(&group, None), Some(b"discart".to_vec()));
    }

    #[test]
    fn source_genre_and_year_survive_a_release_without_them() {
        let (_d, src, mut lib) = setup();
        write_wav(&src.join("a.wav"), 1);
        let source_tags = TrackTags {
            title: "t".into(),
            artist: "Band".into(),
            album: "Alb".into(),
            album_artist: "Band".into(),
            track_no: 1,
            track_total: 1,
            disc_no: 1,
            disc_total: 1,
            year: Some("1990".into()),
            genre: Some("jazz".into()),
            mb_release_id: None,
            mb_recording_id: None,
        };
        write_tags(&src.join("a.wav"), &source_tags, None).unwrap();
        let group = scan_paths(&[src.clone()]).groups.remove(0);
        let mut rel = candidate("m", "Alb", "Band", &[("One", 1000)]);
        rel.genre = None;
        rel.date = None;

        let tags = plan_tags(&group, Some(&rel));
        assert_eq!(tags[0].genre.as_deref(), Some("jazz"));
        assert_eq!(tags[0].year.as_deref(), Some("1990"));

        let id = apply(&mut lib, &group, Some(&rel), None, false).unwrap();
        let copy = read_source_track(&lib.album_tracks(id).unwrap()[0].file_path).unwrap();
        assert_eq!(copy.genre.as_deref(), Some("jazz"));
        assert_eq!(copy.year.as_deref(), Some("1990"));
        assert_eq!(lib.album(id).unwrap().unwrap().genre.as_deref(), Some("jazz"));
    }
}
