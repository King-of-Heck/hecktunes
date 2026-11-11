//! Putting a chosen part of the library on the Echo Mini's microSD card.
pub mod cardpath;
pub mod detect;
pub mod echosync;
pub mod m3u;
pub mod manifest;
pub mod plan;
pub mod service;
pub mod sync;

use crate::library::Library;
use anyhow::{bail, Result};
use manifest::Manifest;
use plan::{Snapshot, SyncPlan};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Serializes hash backfills across the process (see `prepare`).
static BACKFILL: Mutex<()> = Mutex::new(());

pub struct Prepared {
    pub plan: SyncPlan,
    pub manifest: Manifest,
    pub first_sync: bool,
}

/// Reads the library (holding its lock only briefly), refuses a card that overlaps the library
/// folder, backfills missing track hashes outside the lock, reads the card's manifest and works out
/// the sync plan.
pub fn prepare(lib: &Mutex<Library>, card: &Path) -> Result<Prepared> {
    // Overlapping previews and syncs wait for each other here instead of hashing the same files in
    // parallel; the snapshot is taken under this guard so a waiter sees the hashes just stored.
    // Lock order: BACKFILL, then the Library lock (never the other way round).
    let backfill = BACKFILL.lock().unwrap_or_else(|e| e.into_inner());
    let mut snap = {
        let l = lib.lock().unwrap();
        Snapshot::take(&l)?
    };
    check_card_is_not_the_library(card, &snap.root)?;
    let updates = plan::fill_missing_hashes(&mut snap);
    if !updates.is_empty() {
        let mut l = lib.lock().unwrap();
        for (id, size, hash) in &updates {
            if !l.set_track_hash_if_missing(*id, *size, hash)? {
                // Something (a tag edit) stored a newer hash meanwhile: plan with that one.
                if let (Some(fresh), Some(song)) = (l.track(*id)?, snap.songs.iter_mut().find(|s| s.track.id == *id)) {
                    song.track.size_bytes = fresh.size_bytes;
                    song.track.content_hash = fresh.content_hash;
                }
            }
        }
    }
    drop(backfill);
    let (wanted, warnings) = plan::wanted_files(&snap);
    let existing = Manifest::load(card)?;
    let first_sync = existing.is_none();
    let manifest = existing.unwrap_or_default();
    Ok(Prepared { plan: plan::diff(wanted, &manifest, warnings), manifest, first_sync })
}

/// A sync adopts, overwrites and later deletes files under the card's /Music and /Playlists, so
/// those folders must never be, contain, or lie inside the library folder.
fn check_card_is_not_the_library(card: &Path, library_root: &Path) -> Result<()> {
    let root = resolve(library_root);
    for top in [cardpath::MUSIC_DIR, cardpath::PLAYLISTS_DIR] {
        if overlaps(&resolve(&card.join(top)), &root) {
            bail!(
                "This drive holds your HeckTunes library ({}), so syncing to {} could delete or overwrite your own music. Pick the Echo Mini's card instead.",
                library_root.display(),
                card.display()
            );
        }
    }
    Ok(())
}

/// The canonical form of `p`; for a path that doesn't exist, its nearest existing ancestor
/// canonicalized with the rest joined back on.
fn resolve(p: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(p) {
        return c;
    }
    match (p.parent(), p.file_name()) {
        (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => resolve(parent).join(name),
        _ => p.to_path_buf(),
    }
}

/// Whether one path is the other or lies inside it, comparing whole components (ignoring case on
/// Windows and macOS, whose file systems usually do).
fn overlaps(a: &Path, b: &Path) -> bool {
    let parts = |p: &Path| -> Vec<String> {
        p.components()
            .map(|c| {
                let s = c.as_os_str().to_string_lossy().into_owned();
                if cfg!(any(windows, target_os = "macos")) {
                    s.to_lowercase()
                } else {
                    s
                }
            })
            .collect()
    };
    let (a, b) = (parts(a), parts(b));
    let n = a.len().min(b.len());
    a[..n] == b[..n]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::SelectionKind;
    use crate::test_support::add_album;

    #[test]
    fn prepare_reports_a_first_sync_and_plans_the_selection() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Mutex::new(Library::open_in_memory(dir.path().join("lib")).unwrap());
        let a = add_album(&mut lib.lock().unwrap(), dir.path(), "Alb", "Band", 2, false);
        lib.lock().unwrap().set_selected(SelectionKind::Album, &a.to_string(), true).unwrap();
        let card = dir.path().join("card");
        std::fs::create_dir(&card).unwrap();
        let p = prepare(&lib, &card).unwrap();
        assert!(p.first_sync);
        assert_eq!((p.plan.added, p.plan.removals.len()), (2, 0));
        assert!(p.manifest.files.is_empty());
    }

    /// Every file under `dir` with its bytes, for "nothing changed" checks.
    fn tree(dir: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                out.extend(tree(&p));
            } else {
                out.push((p.clone(), std::fs::read(&p).unwrap()));
            }
        }
        out.sort();
        out
    }

    #[test]
    fn a_card_whose_music_folder_is_the_library_is_refused_and_nothing_changes() {
        let dir = tempfile::tempdir().unwrap();
        let x = dir.path().join("X");
        let lib = Mutex::new(Library::open_in_memory(x.join("Music")).unwrap());
        let a = add_album(&mut lib.lock().unwrap(), dir.path(), "Alb", "Band", 2, true);
        lib.lock().unwrap().set_selected(SelectionKind::Album, &a.to_string(), true).unwrap();
        let before = tree(&x);
        assert!(!before.is_empty());
        let err = prepare(&lib, &x).err().expect("syncing onto the library must be refused");
        assert!(err.to_string().contains("holds your HeckTunes library"), "{err}");
        assert!(err.to_string().contains("Echo Mini's card"), "{err}");
        // The same drive spelled differently (case) is still the same drive on Windows and macOS.
        #[cfg(any(windows, target_os = "macos"))]
        {
            let upper = std::path::PathBuf::from(x.to_string_lossy().to_uppercase());
            assert!(prepare(&lib, &upper).is_err());
        }
        assert_eq!(tree(&x), before);
        assert!(!x.join(manifest::MANIFEST_DIR).exists());
    }

    #[test]
    fn a_card_inside_the_library_is_refused_too() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("lib");
        let lib = Mutex::new(Library::open_in_memory(root.clone()).unwrap());
        add_album(&mut lib.lock().unwrap(), dir.path(), "Alb", "Band", 1, false);
        // The library sits inside card/Music, e.g. card = D:\ and library = D:\Music\Mine.
        let lib2 = Mutex::new(Library::open_in_memory(dir.path().join("card").join("Music").join("Mine")).unwrap());
        std::fs::create_dir_all(dir.path().join("card")).unwrap();
        assert!(prepare(&lib2, &dir.path().join("card")).is_err());
        // card/Playlists containing the library is refused as well.
        let lib3 = Mutex::new(Library::open_in_memory(dir.path().join("card").join("Playlists")).unwrap());
        assert!(prepare(&lib3, &dir.path().join("card")).is_err());
        // A card inside the library folder is refused.
        let inner = root.join("Band");
        assert!(prepare(&lib, &inner).is_err());
    }

    #[test]
    fn a_separate_card_next_to_the_library_still_works() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Mutex::new(Library::open_in_memory(dir.path().join("Music")).unwrap());
        let a = add_album(&mut lib.lock().unwrap(), dir.path(), "Alb", "Band", 1, false);
        lib.lock().unwrap().set_selected(SelectionKind::Album, &a.to_string(), true).unwrap();
        // A card folder whose name merely starts like the library's is not an overlap.
        let card = dir.path().join("MusicCard");
        std::fs::create_dir(&card).unwrap();
        assert_eq!(prepare(&lib, &card).unwrap().plan.added, 1);
    }

    #[test]
    fn only_selected_tracks_without_a_hash_are_hashed() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Mutex::new(Library::open_in_memory(dir.path().join("lib")).unwrap());
        let a = add_album(&mut lib.lock().unwrap(), dir.path(), "Alb", "Band", 2, false);
        let b = add_album(&mut lib.lock().unwrap(), dir.path(), "Other", "Solo", 2, false);
        lib.lock().unwrap().set_selected(SelectionKind::Album, &a.to_string(), true).unwrap();
        lib.lock().unwrap().exec_sql_for_test("UPDATE tracks SET content_hash = NULL");
        let card = dir.path().join("card");
        std::fs::create_dir(&card).unwrap();
        assert_eq!(prepare(&lib, &card).unwrap().plan.added, 2);
        let l = lib.lock().unwrap();
        assert!(l.album_tracks(a).unwrap().iter().all(|t| t.content_hash.is_some()));
        assert!(l.album_tracks(b).unwrap().iter().all(|t| t.content_hash.is_none()), "unselected tracks must not be hashed");
    }
}
