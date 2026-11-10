use crate::model::SourceTrack;
use crate::tags::{extension, read_source_track, KNOWN_UNSUPPORTED, SUPPORTED_EXTS};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AlbumGroup {
    pub tracks: Vec<SourceTrack>,
    pub source_dir: PathBuf,
}

impl AlbumGroup {
    /// "Artist – Album" from tags, else the folder name.
    pub fn label(&self) -> String {
        let first = |f: fn(&SourceTrack) -> Option<&String>| self.tracks.iter().find_map(f).cloned();
        match (first(|t| t.album_artist.as_ref()).or_else(|| first(|t| t.artist.as_ref())), first(|t| t.album.as_ref())) {
            (Some(artist), Some(album)) => format!("{artist} – {album}"),
            (None, Some(album)) => album,
            _ => self.source_dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        }
    }
}

#[derive(Debug, Default)]
pub struct ScanResult {
    pub groups: Vec<AlbumGroup>,
    pub skipped: Vec<(PathBuf, String)>,
}

/// Canonical form for "is this inside that" checks (resolves case, `..` and links on Windows).
fn canonical(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn is_under(path: &Path, exclude: Option<&Path>) -> bool {
    exclude.is_some_and(|ex| canonical(path).starts_with(ex))
}

/// Collects files under `path`, skipping hidden entries and any folder inside `exclude`
/// (which must be canonical); such folders are pushed to `excluded`.
fn collect_files(path: &Path, exclude: Option<&Path>, out: &mut Vec<PathBuf>, excluded: &mut Vec<PathBuf>) {
    if path.is_dir() {
        if is_under(path, exclude) {
            excluded.push(path.to_path_buf());
            return;
        }
        let Ok(entries) = fs::read_dir(path) else { return };
        let mut children: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        children.sort();
        for child in children {
            let hidden = child.file_name().map(|n| n.to_string_lossy().starts_with('.')).unwrap_or(false);
            if !hidden {
                collect_files(&child, exclude, out, excluded);
            }
        }
    } else if path.is_file() {
        if is_under(path, exclude) {
            excluded.push(path.to_path_buf());
        } else {
            out.push(path.to_path_buf());
        }
    }
}

/// `CD1`, `CD 2`, `Disc 1`, `Disk3`... (case-insensitive) -> the disc number.
fn disc_folder_number(dir: &Path) -> Option<u32> {
    let name = dir.file_name()?.to_string_lossy().to_lowercase();
    let rest = ["disc", "disk", "cd"].iter().find_map(|p| name.strip_prefix(p))?;
    let digits = rest.strip_prefix(' ').unwrap_or(rest);
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|n| *n > 0)
}

pub fn scan_paths(paths: &[PathBuf]) -> ScanResult {
    scan(paths, None)
}

fn scan(paths: &[PathBuf], exclude: Option<&Path>) -> ScanResult {
    let (mut files, mut excluded) = (Vec::new(), Vec::new());
    for p in paths {
        collect_files(p, exclude, &mut files, &mut excluded);
    }
    let mut result = ScanResult::default();
    for p in excluded {
        result.skipped.push((p, "already in the HeckTunes library; not imported again".to_string()));
    }
    // Key: (folder, lowercased album tag) - keeps two albums dumped in one folder apart.
    let mut groups: BTreeMap<(PathBuf, String), Vec<SourceTrack>> = BTreeMap::new();
    for f in files {
        let Some(ext) = extension(&f) else { continue };
        if KNOWN_UNSUPPORTED.contains(&ext.as_str()) {
            result.skipped.push((f, format!(".{ext} files aren't supported by EchoPod")));
            continue;
        }
        if !SUPPORTED_EXTS.contains(&ext.as_str()) {
            continue;
        }
        match read_source_track(&f) {
            Ok(mut t) => {
                let dir = f.parent().map(Path::to_path_buf).unwrap_or_default();
                // Disc folders (Album/CD1, Album/CD2) merge under their parent; the folder number
                // stands in for a missing disc tag.
                let disc = disc_folder_number(&dir);
                let dir = match (disc, dir.parent()) {
                    (Some(_), Some(parent)) => parent.to_path_buf(),
                    _ => dir,
                };
                t.disc_no = t.disc_no.or(disc);
                let album = t.album.clone().unwrap_or_default().to_lowercase();
                groups.entry((dir, album)).or_default().push(t);
            }
            Err(e) => result.skipped.push((f, format!("{e:#}"))),
        }
    }
    for ((dir, _), mut tracks) in groups {
        tracks.sort_by(|a, b| {
            (a.disc_no.unwrap_or(1), a.track_no.unwrap_or(u32::MAX), &a.path)
                .cmp(&(b.disc_no.unwrap_or(1), b.track_no.unwrap_or(u32::MAX), &b.path))
        });
        result.groups.push(AlbumGroup { tracks, source_dir: dir });
    }
    result
}

/// Like `scan_paths`, but never descends into `library_root` (so the library can't be imported
/// into itself). Dropped paths at or under the root are reported as skipped.
pub fn scan_paths_excluding(paths: &[PathBuf], library_root: &Path) -> ScanResult {
    scan(paths, Some(&canonical(library_root)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{tagged_wav, wav_without_disc, write_wav};
    use std::fs;

    #[test]
    fn groups_one_folder_sorted_and_reports_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let album = dir.path().join("My Album");
        fs::create_dir(&album).unwrap();
        tagged_wav(&album.join("b.wav"), 1, "Second", "Album", "Band", 2);
        tagged_wav(&album.join("a.wav"), 1, "First", "Album", "Band", 1);
        fs::write(album.join("cover.jpg"), b"x").unwrap();
        fs::write(album.join("notes.txt"), b"x").unwrap();
        fs::write(album.join("old.wma"), b"x").unwrap();

        let r = scan_paths(&[album.clone()]);
        assert_eq!(r.groups.len(), 1);
        let titles: Vec<_> = r.groups[0].tracks.iter().map(|t| t.title.clone().unwrap()).collect();
        assert_eq!(titles, vec!["First", "Second"]);
        assert_eq!(r.groups[0].source_dir, album);
        assert_eq!(r.skipped.len(), 1);
        assert!(r.skipped[0].0.ends_with("old.wma"));
    }

    #[test]
    fn separate_folders_become_separate_albums_and_files_are_accepted() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["A", "B"] {
            fs::create_dir(dir.path().join(name)).unwrap();
            write_wav(&dir.path().join(name).join("01.wav"), 1);
        }
        let loose = dir.path().join("loose.wav");
        write_wav(&loose, 1);
        let r = scan_paths(&[dir.path().join("A"), dir.path().join("B"), loose]);
        assert_eq!(r.groups.len(), 3);
    }

    #[test]
    fn untagged_tracks_sort_by_filename() {
        let dir = tempfile::tempdir().unwrap();
        write_wav(&dir.path().join("Track 02.wav"), 1);
        write_wav(&dir.path().join("Track 01.wav"), 1);
        let r = scan_paths(&[dir.path().to_path_buf()]);
        assert!(r.groups[0].tracks[0].path.ends_with("Track 01.wav"));
        assert_eq!(r.groups[0].label(), dir.path().file_name().unwrap().to_string_lossy());
    }

    #[test]
    fn corrupt_supported_file_is_skipped_with_reason() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("bad.flac"), b"nope").unwrap();
        let r = scan_paths(&[dir.path().to_path_buf()]);
        assert!(r.groups.is_empty());
        assert_eq!(r.skipped.len(), 1);
    }

    #[test]
    fn disc_subfolders_merge_into_one_album_with_inferred_discs() {
        let dir = tempfile::tempdir().unwrap();
        let album = dir.path().join("Album");
        for d in ["CD1", "cd 2", "Bonus"] {
            fs::create_dir_all(album.join(d)).unwrap();
        }
        wav_without_disc(&album.join("CD1").join("b.wav"), 1, "1-2", "Alb", 2);
        wav_without_disc(&album.join("CD1").join("a.wav"), 1, "1-1", "Alb", 1);
        wav_without_disc(&album.join("cd 2").join("a.wav"), 1, "2-1", "alb", 1);
        // Not a disc folder: stays its own album even with the same album tag.
        wav_without_disc(&album.join("Bonus").join("x.wav"), 1, "bonus", "Alb", 1);

        let r = scan_paths(&[album.clone()]);
        assert_eq!(r.groups.len(), 2, "{:?}", r.groups.iter().map(|g| &g.source_dir).collect::<Vec<_>>());
        let g = r.groups.iter().find(|g| g.tracks.len() == 3).expect("merged disc album");
        assert_eq!(g.source_dir, album);
        let got: Vec<_> = g.tracks.iter().map(|t| (t.disc_no, t.title.clone().unwrap())).collect();
        assert_eq!(got, vec![(Some(1), "1-1".into()), (Some(1), "1-2".into()), (Some(2), "2-1".into())]);
    }

    #[test]
    fn disc_folder_names_are_recognised() {
        for (name, n) in [("CD1", Some(1)), ("CD 2", Some(2)), ("Disc 1", Some(1)), ("disc2", Some(2)), ("Disk 3", Some(3))] {
            assert_eq!(disc_folder_number(Path::new(name)), n, "{name}");
        }
        for name in ["CDs", "Discography", "Bonus", "CD", "Disc 1 (Live)"] {
            assert_eq!(disc_folder_number(Path::new(name)), None, "{name}");
        }
    }

    #[test]
    fn disc_tag_wins_over_folder_number() {
        let dir = tempfile::tempdir().unwrap();
        let cd2 = dir.path().join("Album").join("CD2");
        fs::create_dir_all(&cd2).unwrap();
        tagged_wav(&cd2.join("a.wav"), 1, "t", "Alb", "Band", 1); // disc tag 1
        let r = scan_paths(&[cd2]);
        assert_eq!(r.groups[0].tracks[0].disc_no, Some(1));
    }

    #[test]
    fn paths_inside_the_library_root_are_not_scanned() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("lib");
        let in_lib = root.join("Band").join("Alb");
        let outside = dir.path().join("src");
        fs::create_dir_all(&in_lib).unwrap();
        fs::create_dir_all(&outside).unwrap();
        write_wav(&in_lib.join("01 a.wav"), 1);
        write_wav(&outside.join("b.wav"), 1);

        // Dropping a library folder (or the root itself) directly is skipped with a reason.
        let r = scan_paths_excluding(&[in_lib.clone(), root.clone(), outside.clone()], &root);
        assert_eq!(r.groups.len(), 1);
        assert_eq!(r.groups[0].source_dir, outside);
        assert_eq!(r.skipped.len(), 2, "{:?}", r.skipped);
        assert!(r.skipped.iter().all(|(_, why)| why.contains("library")));

        // Dropping a parent of the root: the root's contents are left out.
        let r = scan_paths_excluding(&[dir.path().to_path_buf()], &root);
        assert_eq!(r.groups.len(), 1);
        assert_eq!(r.groups[0].source_dir, outside);
    }
}
