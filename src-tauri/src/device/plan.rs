use super::cardpath::{fold_key, playlist_card_path, track_card_path};
use super::m3u::{render, M3uEntry};
use super::manifest::{FileEntry, Manifest};
use crate::library::hash::{bytes_hash, file_hash};
use crate::library::{AlbumRow, Library, PlaylistEntry, PlaylistRow, Selection, SongRow};
use anyhow::Result;
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

/// Free space kept in reserve on the card, on top of the biggest single file.
pub const SPACE_MARGIN: u64 = 32 * 1024 * 1024;

/// Library state a sync works from, read in one go so the library lock is held only briefly.
pub struct Snapshot {
    pub root: PathBuf,
    pub albums: Vec<AlbumRow>,
    pub songs: Vec<SongRow>,
    pub playlists: Vec<(PlaylistRow, Vec<PlaylistEntry>)>,
    pub selection: Selection,
}

impl Snapshot {
    pub fn take(lib: &Library) -> Result<Self> {
        let playlists = lib
            .list_playlists()?
            .into_iter()
            .map(|p| {
                let entries = lib.playlist_entries(p.id)?;
                Ok((p, entries))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            root: lib.root().to_path_buf(),
            albums: lib.list_albums()?,
            songs: lib.list_songs()?,
            playlists,
            selection: lib.selection()?,
        })
    }
}

/// Hashes tracks stored before hashes existed. Returns (track id, size, hash) for the caller to save.
/// A track whose file is missing keeps `None` and is skipped (with a warning) by `wanted_files`.
pub fn fill_missing_hashes(s: &mut Snapshot) -> Vec<(i64, u64, String)> {
    let mut updates = Vec::new();
    for song in s.songs.iter_mut().filter(|x| x.track.content_hash.is_none()) {
        let t = &mut song.track;
        if let (Ok(meta), Ok(hash)) = (std::fs::metadata(&t.file_path), file_hash(&t.file_path)) {
            t.size_bytes = meta.len();
            t.content_hash = Some(hash.clone());
            updates.push((t.id, t.size_bytes, hash));
        }
    }
    updates
}

#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    File(PathBuf),
    Bytes(Vec<u8>),
}

/// One file the card should hold.
#[derive(Debug, Clone, PartialEq)]
pub struct Wanted {
    pub card_path: String,
    pub source: Source,
    pub size: u64,
    pub hash: String,
}

#[derive(Default)]
struct Collector {
    out: Vec<Wanted>,
    taken: HashSet<String>,
    warnings: Vec<String>,
}

impl Collector {
    /// Adds the file unless another one already uses the same name (ignoring case).
    fn add(&mut self, w: Wanted) -> bool {
        if self.taken.insert(fold_key(&w.card_path)) {
            self.out.push(w);
            true
        } else {
            self.warnings.push(format!("Skipped {}: another file on the card already has that name", w.card_path));
            false
        }
    }
}

/// Everything the card should hold for the current selection: songs (library order), then covers,
/// then playlists. Second value: warnings to show the user.
pub fn wanted_files(s: &Snapshot) -> (Vec<Wanted>, Vec<String>) {
    let sel = &s.selection;
    let picked: Vec<&(PlaylistRow, Vec<PlaylistEntry>)> = s.playlists.iter().filter(|(p, _)| sel.playlists.contains(&p.id)).collect();
    let playlist_tracks: HashSet<i64> = picked.iter().flat_map(|(_, e)| e.iter().map(|x| x.song.track.id)).collect();
    let albums: HashSet<i64> = s
        .albums
        .iter()
        .filter(|a| sel.albums.contains(&a.id) || sel.artists.contains(&a.album_artist))
        .map(|a| a.id)
        .collect();

    let mut c = Collector::default();
    let mut card_paths: HashMap<i64, String> = HashMap::new();
    let mut cover_albums = BTreeSet::new();
    for song in s.songs.iter().filter(|x| albums.contains(&x.track.album_id) || playlist_tracks.contains(&x.track.id)) {
        let t = &song.track;
        let (Some(path), Some(hash)) = (track_card_path(&s.root, &t.file_path), t.content_hash.clone()) else {
            c.warnings.push(format!("Skipped \"{}\" ({}): its file is missing from the library folder", t.title, song.album_title));
            continue;
        };
        if c.add(Wanted { card_path: path.clone(), source: Source::File(t.file_path.clone()), size: t.size_bytes, hash }) {
            card_paths.insert(t.id, path);
        }
        cover_albums.insert(t.album_id);
    }
    for album in s.albums.iter().filter(|a| cover_albums.contains(&a.id)) {
        let Some(cover) = &album.cover_path else { continue };
        let (Some(path), Ok(meta), Ok(hash)) = (track_card_path(&s.root, cover), std::fs::metadata(cover), file_hash(cover)) else {
            continue;
        };
        c.add(Wanted { card_path: path, source: Source::File(cover.clone()), size: meta.len(), hash });
    }
    for (p, entries) in picked {
        let lines: Vec<M3uEntry> = entries
            .iter()
            .filter_map(|e| {
                let t = &e.song.track;
                card_paths.get(&t.id).map(|cp| M3uEntry { card_path: cp, duration_ms: t.duration_ms, artist: &t.artist, title: &t.title })
            })
            .collect();
        let text = render(&lines).into_bytes();
        c.add(Wanted { card_path: playlist_card_path(&p.name), size: text.len() as u64, hash: bytes_hash(&text), source: Source::Bytes(text) });
    }
    (c.out, c.warnings)
}

/// What a sync will do. `copies` holds new and changed files, sorted by card path.
#[derive(Debug, Clone, Default)]
pub struct SyncPlan {
    pub copies: Vec<Wanted>,
    pub added: usize,
    pub replaced: usize,
    pub removals: Vec<String>,
    pub unchanged: usize,
    pub bytes_to_copy: u64,
    pub bytes_freed: u64,
    pub warnings: Vec<String>,
}

pub fn diff(mut wanted: Vec<Wanted>, manifest: &Manifest, warnings: Vec<String>) -> SyncPlan {
    wanted.sort_by(|a, b| a.card_path.cmp(&b.card_path));
    let keep: HashSet<String> = wanted.iter().map(|w| w.card_path.clone()).collect();
    let mut plan = SyncPlan { warnings, ..Default::default() };
    for (path, entry) in &manifest.files {
        if !keep.contains(path) {
            plan.removals.push(path.clone());
            plan.bytes_freed += entry.size;
        }
    }
    for w in wanted {
        match manifest.files.get(&w.card_path) {
            Some(old) if old.size == w.size && old.hash == w.hash => plan.unchanged += 1,
            Some(FileEntry { size, .. }) => {
                plan.replaced += 1;
                plan.bytes_freed += size;
                plan.bytes_to_copy += w.size;
                plan.copies.push(w);
            }
            None => {
                plan.added += 1;
                plan.bytes_to_copy += w.size;
                plan.copies.push(w);
            }
        }
    }
    plan
}

/// The sync preview the UI shows.
#[derive(Debug, Clone, Serialize)]
pub struct PlanSummary {
    pub add: usize,
    pub replace: usize,
    pub remove: usize,
    pub unchanged: usize,
    pub bytes_to_copy: u64,
    pub bytes_freed: u64,
    pub space_needed: u64,
    pub available: Option<u64>,
    pub fits: bool,
    pub warnings: Vec<String>,
    pub first_sync: bool,
}

impl SyncPlan {
    /// Net growth, plus room for the largest file's temp copy, plus a safety margin. 0 when nothing is copied.
    pub fn space_needed(&self) -> u64 {
        let Some(largest) = self.copies.iter().map(|w| w.size).max() else { return 0 };
        self.bytes_to_copy.saturating_sub(self.bytes_freed) + largest + SPACE_MARGIN
    }

    pub fn fits(&self, available: u64) -> bool {
        self.space_needed() <= available
    }

    pub fn summary(&self, available: Option<u64>, first_sync: bool) -> PlanSummary {
        PlanSummary {
            add: self.added,
            replace: self.replaced,
            remove: self.removals.len(),
            unchanged: self.unchanged,
            bytes_to_copy: self.bytes_to_copy,
            bytes_freed: self.bytes_freed,
            space_needed: self.space_needed(),
            available,
            fits: available.is_none_or(|a| self.fits(a)),
            warnings: self.warnings.clone(),
            first_sync,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::manifest::{FileEntry, Manifest};
    use crate::library::{Library, SelectionKind};
    use crate::test_support::add_album;

    fn paths(w: &[Wanted]) -> Vec<&str> {
        w.iter().map(|x| x.card_path.as_str()).collect()
    }

    fn bytes(card_path: &str, size: u64, hash: &str) -> Wanted {
        Wanted { card_path: card_path.into(), source: Source::Bytes(vec![]), size, hash: hash.into() }
    }

    #[test]
    fn a_ticked_album_brings_its_songs_and_cover() {
        let dir = tempfile::tempdir().unwrap();
        let mut lib = Library::open_in_memory(dir.path().join("lib")).unwrap();
        let a = add_album(&mut lib, dir.path(), "Alb", "Band", 2, true);
        add_album(&mut lib, dir.path(), "Other", "Solo", 1, false);
        lib.set_selected(SelectionKind::Album, &a.to_string(), true).unwrap();
        let (w, warnings) = wanted_files(&Snapshot::take(&lib).unwrap());
        assert_eq!(paths(&w), vec!["Music/Band/Alb/01 Song 1.wav", "Music/Band/Alb/02 Song 2.wav", "Music/Band/Alb/cover.jpg"]);
        assert!(warnings.is_empty());
        let t = &lib.album_tracks(a).unwrap()[0];
        assert_eq!((w[0].size, w[0].hash.as_str()), (t.size_bytes, t.content_hash.as_deref().unwrap()));
        assert_eq!(w[0].source, Source::File(t.file_path.clone()));
    }

    #[test]
    fn ticking_an_artist_or_a_playlist_brings_their_songs_and_an_m3u8() {
        let dir = tempfile::tempdir().unwrap();
        let mut lib = Library::open_in_memory(dir.path().join("lib")).unwrap();
        add_album(&mut lib, dir.path(), "Alb", "Band", 1, false);
        let solo = add_album(&mut lib, dir.path(), "Other", "Solo", 2, false);
        lib.set_selected(SelectionKind::Artist, "Band", true).unwrap();
        let pl = lib.create_playlist("Road Trip").unwrap();
        let second = lib.album_tracks(solo).unwrap()[1].id;
        lib.add_to_playlist(pl, &[second]).unwrap();
        lib.set_selected(SelectionKind::Playlist, &pl.to_string(), true).unwrap();

        let (w, _) = wanted_files(&Snapshot::take(&lib).unwrap());
        assert_eq!(paths(&w), vec!["Music/Band/Alb/01 Song 1.wav", "Music/Solo/Other/02 Song 2.wav", "Playlists/Road Trip.m3u8"]);
        let m3u = w.iter().find(|x| x.card_path == "Playlists/Road Trip.m3u8").unwrap();
        let Source::Bytes(text) = &m3u.source else { panic!("playlist should be generated bytes") };
        assert_eq!(String::from_utf8(text.clone()).unwrap(), "#EXTM3U\n#EXTINF:1,Solo - Song 2\n../Music/Solo/Other/02 Song 2.wav\n");
        assert_eq!((m3u.size, m3u.hash.as_str()), (text.len() as u64, crate::library::hash::bytes_hash(text).as_str()));
    }

    #[test]
    fn nothing_ticked_means_nothing_wanted() {
        let dir = tempfile::tempdir().unwrap();
        let mut lib = Library::open_in_memory(dir.path().join("lib")).unwrap();
        add_album(&mut lib, dir.path(), "Alb", "Band", 1, true);
        let (w, warnings) = wanted_files(&Snapshot::take(&lib).unwrap());
        assert!(w.is_empty() && warnings.is_empty());
    }

    #[test]
    fn missing_hashes_are_filled_and_missing_files_are_skipped_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let mut lib = Library::open_in_memory(dir.path().join("lib")).unwrap();
        let a = add_album(&mut lib, dir.path(), "Alb", "Band", 2, false);
        lib.set_selected(SelectionKind::Album, &a.to_string(), true).unwrap();
        let mut snap = Snapshot::take(&lib).unwrap();
        let (gone, kept) = (snap.songs[0].track.clone(), snap.songs[1].track.clone());
        std::fs::remove_file(&gone.file_path).unwrap();
        for s in &mut snap.songs {
            s.track.content_hash = None; // as for tracks imported before hashes existed
        }
        let updates = fill_missing_hashes(&mut snap);
        assert_eq!(updates, vec![(kept.id, kept.size_bytes, kept.content_hash.clone().unwrap())]);
        let (w, warnings) = wanted_files(&snap);
        assert_eq!(paths(&w), vec!["Music/Band/Alb/02 Song 2.wav"]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("Song 1"), "{warnings:?}");
    }

    #[test]
    fn case_insensitive_name_clashes_keep_the_first_file() {
        let mut c = Collector::default();
        assert!(c.add(bytes("Music/Band/A.flac", 1, "h")));
        assert!(!c.add(bytes("music/band/a.flac", 1, "h")));
        assert_eq!(c.out.len(), 1);
        assert!(c.warnings[0].contains("music/band/a.flac"));
    }

    #[test]
    fn diff_sorts_files_into_new_changed_unchanged_and_stale() {
        let mut m = Manifest::new();
        m.files.insert("Music/same.flac".into(), FileEntry { size: 10, hash: "s".into() });
        m.files.insert("Music/changed.flac".into(), FileEntry { size: 10, hash: "old".into() });
        m.files.insert("Music/stale.flac".into(), FileEntry { size: 7, hash: "x".into() });
        let plan = diff(
            vec![bytes("Music/same.flac", 10, "s"), bytes("Music/new.flac", 5, "n"), bytes("Music/changed.flac", 12, "new")],
            &m,
            vec!["note".into()],
        );
        assert_eq!(paths(&plan.copies), vec!["Music/changed.flac", "Music/new.flac"]);
        assert_eq!((plan.added, plan.replaced, plan.unchanged), (1, 1, 1));
        assert_eq!(plan.removals, vec!["Music/stale.flac".to_string()]);
        assert_eq!((plan.bytes_to_copy, plan.bytes_freed), (17, 17));
        assert_eq!(plan.warnings, vec!["note".to_string()]);
    }

    #[test]
    fn space_check_allows_for_the_largest_file_and_a_margin() {
        assert!(SyncPlan::default().fits(0));
        let plan = diff(vec![bytes("Music/a.flac", 100, "a")], &Manifest::new(), vec![]);
        let needed = 100 + 100 + SPACE_MARGIN;
        assert_eq!(plan.space_needed(), needed);
        assert!(plan.fits(needed));
        assert!(!plan.fits(needed - 1));
        let s = plan.summary(None, true);
        assert!(s.fits && s.first_sync && s.available.is_none());
        assert!(!plan.summary(Some(needed - 1), false).fits);
    }
}
