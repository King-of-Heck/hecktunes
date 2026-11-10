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

fn collect_files(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let Ok(entries) = fs::read_dir(path) else { return };
        let mut children: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        children.sort();
        for child in children {
            let hidden = child.file_name().map(|n| n.to_string_lossy().starts_with('.')).unwrap_or(false);
            if !hidden {
                collect_files(&child, out);
            }
        }
    } else if path.is_file() {
        out.push(path.to_path_buf());
    }
}

pub fn scan_paths(paths: &[PathBuf]) -> ScanResult {
    let mut files = Vec::new();
    for p in paths {
        collect_files(p, &mut files);
    }
    let mut result = ScanResult::default();
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
            Ok(t) => {
                let dir = f.parent().map(Path::to_path_buf).unwrap_or_default();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{tagged_wav, write_wav};
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
}
