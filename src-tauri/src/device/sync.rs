use super::cardpath::{is_safe_card_path, on_card, MUSIC_DIR, PLAYLISTS_DIR, TEMP_SUFFIX};
use super::manifest::{FileEntry, Manifest, MANIFEST_DIR};
use super::plan::{Source, SyncPlan, Wanted};
use crate::library::hash::file_hash;
use anyhow::{anyhow, Result};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// The manifest is saved after this many copied files (and always at the end).
pub const CHECKPOINT_EVERY: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Removing,
    Copying,
}

#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    pub phase: Phase,
    pub done: usize,
    pub total: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub current: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Outcome {
    pub copied: usize,
    pub removed: usize,
    /// Files already on the card with the right content but missing from the manifest.
    pub adopted: usize,
    /// Card paths left alone because a file HeckTunes doesn't own is in the way, or because the
    /// manifest named a path outside /Music and /Playlists.
    pub conflicts: Vec<String>,
    /// Things that changed in the library while the sync ran (a file gone, a file edited).
    pub warnings: Vec<String>,
    pub cancelled: bool,
}

/// Deletes `*.hecktmp` leftovers under /Music, /Playlists and /.hecktunes. Returns how many.
pub fn clean_temp_files(card: &Path) -> Result<usize> {
    let mut n = 0;
    for top in [MUSIC_DIR, PLAYLISTS_DIR, MANIFEST_DIR] {
        n += clean_dir(&card.join(top))?;
    }
    Ok(n)
}

fn clean_dir(dir: &Path) -> Result<usize> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(anyhow!("couldn't read {}: {e}", dir.display())),
    };
    let mut n = 0;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            n += clean_dir(&path)?;
        } else if path.file_name().is_some_and(|f| f.to_string_lossy().ends_with(TEMP_SUFFIX)) {
            fs::remove_file(&path).map_err(|e| anyhow!("couldn't remove {}: {e}", path.display()))?;
            n += 1;
        }
    }
    Ok(n)
}

/// Carries out `plan` on the card. The manifest is saved before starting, every
/// `CHECKPOINT_EVERY` copies, and at the end — also when cancelled or failing part-way.
pub fn execute(
    card: &Path,
    plan: &SyncPlan,
    manifest: &mut Manifest,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<Outcome> {
    if !card.is_dir() {
        return Err(anyhow!("{} isn't available any more. Is the card still connected?", card.display()));
    }
    clean_temp_files(card)?;
    manifest.save(card)?;
    let mut outcome = Outcome::default();
    let result = run(card, plan, manifest, cancel, progress, &mut outcome);
    let saved = manifest.save(card);
    result?;
    saved?;
    Ok(outcome)
}

fn run(
    card: &Path,
    plan: &SyncPlan,
    manifest: &mut Manifest,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
    outcome: &mut Outcome,
) -> Result<()> {
    let total = plan.removals.len();
    for (i, path) in plan.removals.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            outcome.cancelled = true;
            return Ok(());
        }
        progress(&Progress { phase: Phase::Removing, done: i, total, bytes_done: 0, bytes_total: 0, current: path.clone() });
        // The manifest is editable by anyone: never touch a path that isn't safely under /Music or /Playlists.
        if !is_safe_card_path(path) {
            manifest.files.remove(path);
            outcome.conflicts.push(path.clone());
            continue;
        }
        let abs = on_card(card, path);
        match fs::remove_file(&abs) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(anyhow!("couldn't remove {}: {e}", abs.display())),
        }
        manifest.files.remove(path);
        prune_empty_dirs(card, &abs);
        outcome.removed += 1;
    }

    let total = plan.copies.len();
    let mut bytes_done = 0;
    for (i, w) in plan.copies.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            outcome.cancelled = true;
            return Ok(());
        }
        progress(&Progress {
            phase: Phase::Copying,
            done: i,
            total,
            bytes_done,
            bytes_total: plan.bytes_to_copy,
            current: w.card_path.clone(),
        });
        if !is_safe_card_path(&w.card_path) {
            return Err(anyhow!("refusing to write unsafe card path {}", w.card_path));
        }
        let dest = on_card(card, &w.card_path);
        if !manifest.files.contains_key(&w.card_path) && dest.exists() {
            if same_content(&dest, w) {
                manifest.files.insert(w.card_path.clone(), FileEntry { size: w.size, hash: w.hash.clone() });
                outcome.adopted += 1;
            } else {
                outcome.conflicts.push(w.card_path.clone());
            }
        } else {
            let entry = match put(&dest, w)? {
                Put::SourceGone => {
                    outcome.warnings.push(format!(
                        "Skipped {}: its file disappeared from the library during the sync",
                        w.card_path
                    ));
                    bytes_done += w.size;
                    continue;
                }
                Put::Written(actual) => {
                    if actual.hash != w.hash || actual.size != w.size {
                        outcome.warnings.push(format!(
                            "{} changed in the library during the sync; the next sync will check it again",
                            w.card_path
                        ));
                    }
                    actual
                }
            };
            manifest.files.insert(w.card_path.clone(), entry);
            outcome.copied += 1;
            if outcome.copied % CHECKPOINT_EVERY == 0 {
                manifest.save(card)?;
            }
        }
        bytes_done += w.size;
    }
    manifest.synced_at = Some(SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0));
    Ok(())
}

fn same_content(dest: &Path, w: &Wanted) -> bool {
    fs::metadata(dest).map(|m| m.len() == w.size).unwrap_or(false) && file_hash(dest).map(|h| h == w.hash).unwrap_or(false)
}

fn temp_name(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(TEMP_SUFFIX);
    dest.with_file_name(name)
}

enum Put {
    /// Written; the size and hash of what actually landed on the card.
    Written(FileEntry),
    /// The library file was gone by copy time; nothing was written.
    SourceGone,
}

/// Writes `<dest>.hecktmp`, then renames it over `dest` (replacing an older copy). A library file
/// is hashed again as written, since it may have changed after the plan was made.
fn put(dest: &Path, w: &Wanted) -> Result<Put> {
    if let Source::File(p) = &w.source {
        if !p.is_file() {
            return Ok(Put::SourceGone);
        }
    }
    let parent = dest.parent().ok_or_else(|| anyhow!("bad card path {}", dest.display()))?;
    fs::create_dir_all(parent).map_err(|e| anyhow!("couldn't create {}: {e}", parent.display()))?;
    let tmp = temp_name(dest);
    let written = match &w.source {
        Source::File(p) => fs::copy(p, &tmp).and_then(|size| {
            let hash = file_hash(&tmp).map_err(std::io::Error::other)?;
            Ok(FileEntry { size, hash })
        }),
        Source::Bytes(b) => fs::write(&tmp, b).map(|()| FileEntry { size: w.size, hash: w.hash.clone() }),
    };
    match written.and_then(|entry| fs::rename(&tmp, dest).map(|()| entry)) {
        Ok(entry) => Ok(Put::Written(entry)),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            match &w.source {
                // Removed between the check above and the copy.
                Source::File(p) if !p.exists() => Ok(Put::SourceGone),
                _ => Err(anyhow!("couldn't write {}: {e}", dest.display())),
            }
        }
    }
}

/// Removes now-empty folders above a removed file, stopping at /Music, /Playlists and the card root.
fn prune_empty_dirs(card: &Path, removed: &Path) {
    let stops = [card.to_path_buf(), card.join(MUSIC_DIR), card.join(PLAYLISTS_DIR)];
    let mut dir = removed.parent();
    while let Some(d) = dir {
        if stops.iter().any(|s| s == d) || fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::cardpath::track_card_path;
    use crate::device::prepare;
    use crate::library::editing::apply_album_edit;
    use crate::library::{AlbumEdit, Library, SelectionKind};
    use crate::test_support::add_album;
    use std::sync::Mutex;

    struct Env {
        _dir: tempfile::TempDir,
        src: PathBuf,
        card: PathBuf,
        lib: Mutex<Library>,
    }

    fn env() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let card = dir.path().join("card");
        fs::create_dir(&card).unwrap();
        let lib = Mutex::new(Library::open_in_memory(dir.path().join("lib")).unwrap());
        Env { src: dir.path().to_path_buf(), card, lib, _dir: dir }
    }

    fn album(e: &Env, name: &str, artist: &str, n: u32, cover: bool) -> i64 {
        add_album(&mut e.lib.lock().unwrap(), &e.src, name, artist, n, cover)
    }

    fn tick(e: &Env, kind: SelectionKind, reference: &str, on: bool) {
        e.lib.lock().unwrap().set_selected(kind, reference, on).unwrap();
    }

    fn sync(e: &Env) -> Outcome {
        let mut p = prepare(&e.lib, &e.card).unwrap();
        execute(&e.card, &p.plan, &mut p.manifest, &AtomicBool::new(false), &mut |_| {}).unwrap()
    }

    /// Files under /Music and /Playlists, card-relative with '/'.
    fn card_files(card: &Path) -> Vec<String> {
        fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
            for e in fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(base, &p, out);
                } else {
                    let rel = p.strip_prefix(base).unwrap();
                    out.push(rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/"));
                }
            }
        }
        let mut out = Vec::new();
        for top in [MUSIC_DIR, PLAYLISTS_DIR] {
            let d = card.join(top);
            if d.exists() {
                walk(card, &d, &mut out);
            }
        }
        out.sort();
        out
    }

    #[test]
    fn first_sync_writes_music_covers_playlist_and_manifest_then_a_second_sync_does_nothing() {
        let e = env();
        let a = album(&e, "Alb", "Band", 2, true);
        let b = album(&e, "Other", "Solo", 2, false);
        let pl = {
            let mut l = e.lib.lock().unwrap();
            let id = l.create_playlist("Mix").unwrap();
            let t = l.album_tracks(b).unwrap()[1].id;
            l.add_to_playlist(id, &[t]).unwrap();
            id
        };
        tick(&e, SelectionKind::Album, &a.to_string(), true);
        tick(&e, SelectionKind::Playlist, &pl.to_string(), true);

        let out = sync(&e);
        assert_eq!((out.copied, out.removed, out.adopted, out.cancelled), (5, 0, 0, false));
        assert_eq!(
            card_files(&e.card),
            vec![
                "Music/Band/Alb/01 Song 1.wav",
                "Music/Band/Alb/02 Song 2.wav",
                "Music/Band/Alb/cover.jpg",
                "Music/Solo/Other/02 Song 2.wav",
                "Playlists/Mix.m3u8"
            ]
        );
        let m3u = fs::read_to_string(e.card.join(PLAYLISTS_DIR).join("Mix.m3u8")).unwrap();
        assert!(m3u.contains("\n../Music/Solo/Other/02 Song 2.wav\n"), "{m3u}");
        let m = Manifest::load(&e.card).unwrap().unwrap();
        assert_eq!(m.files.len(), 5);
        assert!(m.synced_at.is_some());

        let again = prepare(&e.lib, &e.card).unwrap();
        assert!(!again.first_sync);
        assert_eq!((again.plan.copies.len(), again.plan.removals.len(), again.plan.unchanged), (0, 0, 5));
    }

    #[test]
    fn unticking_removes_only_our_files_and_prunes_empty_folders() {
        let e = env();
        let a = album(&e, "Alb", "Band", 1, false);
        tick(&e, SelectionKind::Album, &a.to_string(), true);
        sync(&e);
        fs::write(e.card.join(MUSIC_DIR).join("mine.mp3"), b"keep").unwrap();
        let lyrics = e.card.join(MUSIC_DIR).join("Band").join("Alb").join("lyrics.txt");
        fs::write(&lyrics, b"words").unwrap();
        fs::create_dir_all(e.card.join(".echo")).unwrap();
        fs::write(e.card.join(".echo").join("favorites.bin"), b"fav").unwrap();

        tick(&e, SelectionKind::Album, &a.to_string(), false);
        let out = sync(&e);
        assert_eq!(out.removed, 1);
        // A foreign file keeps its folders alive; only our files go.
        assert_eq!(fs::read(&lyrics).unwrap(), b"words");
        assert!(e.card.join(MUSIC_DIR).join("Band").join("Alb").is_dir());
        assert_eq!(card_files(&e.card).len(), 2);
        assert!(e.card.join(MUSIC_DIR).exists());
        assert_eq!(fs::read(e.card.join(MUSIC_DIR).join("mine.mp3")).unwrap(), b"keep");
        assert_eq!(fs::read(e.card.join(".echo").join("favorites.bin")).unwrap(), b"fav");
        assert!(Manifest::load(&e.card).unwrap().unwrap().files.is_empty());
    }

    #[test]
    fn edited_tags_are_copied_again() {
        let e = env();
        let a = album(&e, "Alb", "Band", 2, false);
        tick(&e, SelectionKind::Album, &a.to_string(), true);
        sync(&e);
        apply_album_edit(
            &mut e.lib.lock().unwrap(),
            a,
            &AlbumEdit { title: "Alb".into(), album_artist: "Band".into(), year: None, genre: Some("Rock".into()) },
        )
        .unwrap();

        let out = sync(&e);
        assert_eq!((out.copied, out.removed), (2, 0));
        let (root, tracks) = {
            let l = e.lib.lock().unwrap();
            (l.root().to_path_buf(), l.album_tracks(a).unwrap())
        };
        for t in tracks {
            let on = on_card(&e.card, &track_card_path(&root, &t.file_path).unwrap());
            assert_eq!(file_hash(&on).unwrap(), file_hash(&t.file_path).unwrap());
        }
    }

    #[test]
    fn leftovers_are_cleaned_and_unrecorded_files_are_adopted_or_left_alone() {
        let e = env();
        let a = album(&e, "Alb", "Band", 2, false);
        tick(&e, SelectionKind::Album, &a.to_string(), true);
        // An earlier sync was interrupted: song 1 was copied but not recorded, a temp file was left,
        // and something that isn't ours sits where song 2 should go.
        let p = prepare(&e.lib, &e.card).unwrap();
        let (first, second) = (p.plan.copies[0].clone(), p.plan.copies[1].clone());
        let Source::File(src1) = &first.source else { panic!("track source is a file") };
        let d1 = on_card(&e.card, &first.card_path);
        fs::create_dir_all(d1.parent().unwrap()).unwrap();
        fs::copy(src1, &d1).unwrap();
        let d2 = on_card(&e.card, &second.card_path);
        // Same size as the wanted file but different bytes: only the hash comparison tells them apart.
        let foreign = vec![b'x'; second.size as usize];
        fs::write(&d2, &foreign).unwrap();
        let stray = e.card.join(MUSIC_DIR).join("x.flac.hecktmp");
        fs::write(&stray, b"half").unwrap();

        let out = sync(&e);
        assert_eq!((out.copied, out.adopted), (0, 1));
        assert_eq!(out.conflicts, vec![second.card_path.clone()]);
        assert!(!stray.exists());
        assert_eq!(fs::read(&d2).unwrap(), foreign);
        let m = Manifest::load(&e.card).unwrap().unwrap();
        assert!(m.files.contains_key(&first.card_path));
        assert!(!m.files.contains_key(&second.card_path));
    }

    #[test]
    fn cancelling_stops_before_copying_but_still_records_the_card() {
        let e = env();
        let a = album(&e, "Alb", "Band", 1, false);
        tick(&e, SelectionKind::Album, &a.to_string(), true);
        let mut p = prepare(&e.lib, &e.card).unwrap();
        let out = execute(&e.card, &p.plan, &mut p.manifest, &AtomicBool::new(true), &mut |_| {}).unwrap();
        assert!(out.cancelled);
        assert_eq!(out.copied, 0);
        let m = Manifest::load(&e.card).unwrap().unwrap();
        assert!(m.files.is_empty());
        assert!(m.synced_at.is_none());
    }

    #[test]
    fn reports_copy_progress_with_totals() {
        let e = env();
        let a = album(&e, "Alb", "Band", 2, false);
        tick(&e, SelectionKind::Album, &a.to_string(), true);
        let mut p = prepare(&e.lib, &e.card).unwrap();
        let mut seen = Vec::new();
        execute(&e.card, &p.plan, &mut p.manifest, &AtomicBool::new(false), &mut |pr| seen.push(pr.clone())).unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen.iter().all(|s| s.phase == Phase::Copying && s.total == 2 && s.bytes_total == p.plan.bytes_to_copy));
        assert_eq!((seen[0].done, seen[1].done), (0, 1));
        assert_eq!(seen[0].current, p.plan.copies[0].card_path);
    }

    #[test]
    fn a_missing_card_is_a_clear_error() {
        let e = env();
        let mut m = Manifest::new();
        let err = execute(&e.card.join("unplugged"), &SyncPlan::default(), &mut m, &AtomicBool::new(false), &mut |_| {}).unwrap_err();
        assert!(err.to_string().contains("isn't available"), "{err}");
    }

    #[test]
    fn removing_the_last_files_prunes_album_folders_but_keeps_music_and_playlists() {
        let e = env();
        let a = album(&e, "Alb", "Band", 1, false);
        let pl = {
            let mut l = e.lib.lock().unwrap();
            let id = l.create_playlist("Mix").unwrap();
            let t = l.album_tracks(a).unwrap()[0].id;
            l.add_to_playlist(id, &[t]).unwrap();
            id
        };
        tick(&e, SelectionKind::Album, &a.to_string(), true);
        tick(&e, SelectionKind::Playlist, &pl.to_string(), true);
        sync(&e);

        tick(&e, SelectionKind::Album, &a.to_string(), false);
        tick(&e, SelectionKind::Playlist, &pl.to_string(), false);
        let out = sync(&e);
        assert_eq!(out.removed, 2);
        assert!(!e.card.join(MUSIC_DIR).join("Band").exists());
        assert!(e.card.join(MUSIC_DIR).is_dir());
        assert!(e.card.join(PLAYLISTS_DIR).is_dir());
        assert!(card_files(&e.card).is_empty());
    }

    #[test]
    fn an_unsafe_copy_path_is_refused_nothing_escapes_the_card_and_the_manifest_is_still_saved() {
        let e = env();
        let plan = SyncPlan {
            copies: vec![Wanted { card_path: "Music/../x.flac".into(), source: Source::Bytes(b"evil".to_vec()), size: 4, hash: "h".into() }],
            ..SyncPlan::default()
        };
        let mut m = Manifest::new();
        let err = execute(&e.card, &plan, &mut m, &AtomicBool::new(false), &mut |_| {}).unwrap_err();
        assert!(err.to_string().contains("unsafe"), "{err}");
        assert!(!e.src.join("x.flac").exists());
        assert!(!e.card.join("x.flac").exists());
        assert!(!e.card.join(MUSIC_DIR).join("x.flac").exists());
        assert!(m.files.is_empty());
        assert!(Manifest::load(&e.card).unwrap().is_some());
    }

    #[test]
    fn cancelling_mid_run_keeps_what_was_copied_in_the_manifest_without_marking_it_synced() {
        let e = env();
        let a = album(&e, "Alb", "Band", 3, false);
        tick(&e, SelectionKind::Album, &a.to_string(), true);
        let mut p = prepare(&e.lib, &e.card).unwrap();
        let cancel = AtomicBool::new(false);
        let out = execute(&e.card, &p.plan, &mut p.manifest, &cancel, &mut |pr| {
            if pr.done == 0 {
                cancel.store(true, Ordering::SeqCst);
            }
        })
        .unwrap();
        assert!(out.cancelled);
        assert_eq!(out.copied, 1);
        let m = Manifest::load(&e.card).unwrap().unwrap();
        assert_eq!(m.files.len(), 1);
        assert!(m.files.contains_key(&p.plan.copies[0].card_path));
        assert!(on_card(&e.card, &p.plan.copies[0].card_path).is_file());
        assert!(!on_card(&e.card, &p.plan.copies[1].card_path).exists());
        assert!(m.synced_at.is_none());
    }

    #[test]
    fn a_tampered_manifest_cannot_make_us_delete_files_outside_the_card_folders() {
        let e = env();
        let outside = e.src.join("outside.txt");
        fs::write(&outside, b"precious").unwrap();
        let key = "Music/../../outside.txt";
        let mut m = Manifest::new();
        m.files.insert(key.to_string(), FileEntry { size: 8, hash: "x".into() });
        m.save(&e.card).unwrap();

        let out = sync(&e);
        assert_eq!(out.removed, 0);
        assert_eq!(out.conflicts, vec![key.to_string()]);
        assert_eq!(fs::read(&outside).unwrap(), b"precious");
        assert!(!Manifest::load(&e.card).unwrap().unwrap().files.contains_key(key));
    }

    #[test]
    fn a_library_file_gone_by_copy_time_is_skipped_with_a_warning_and_the_rest_still_syncs() {
        let e = env();
        let present = e.src.join("present.flac");
        fs::write(&present, b"abc").unwrap();
        let plan = SyncPlan {
            copies: vec![
                Wanted { card_path: "Music/A/gone.flac".into(), source: Source::File(e.src.join("gone.flac")), size: 5, hash: "h".into() },
                Wanted {
                    card_path: "Music/A/present.flac".into(),
                    source: Source::File(present.clone()),
                    size: 3,
                    hash: file_hash(&present).unwrap(),
                },
            ],
            ..SyncPlan::default()
        };
        let mut m = Manifest::new();
        let out = execute(&e.card, &plan, &mut m, &AtomicBool::new(false), &mut |_| {}).unwrap();
        assert_eq!(out.copied, 1);
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].contains("Music/A/gone.flac"), "{:?}", out.warnings);
        assert!(!on_card(&e.card, "Music/A/gone.flac").exists());
        assert!(!m.files.contains_key("Music/A/gone.flac"));
        assert_eq!(fs::read(on_card(&e.card, "Music/A/present.flac")).unwrap(), b"abc");
        assert!(m.synced_at.is_some());
    }

    #[test]
    fn a_file_that_changed_since_planning_is_recorded_as_copied_and_flagged() {
        let e = env();
        let src = e.src.join("edited.flac");
        fs::write(&src, b"new bytes").unwrap(); // the plan still expects the old content
        let plan = SyncPlan {
            copies: vec![Wanted { card_path: "Music/A/edited.flac".into(), source: Source::File(src.clone()), size: 3, hash: "old-hash".into() }],
            ..SyncPlan::default()
        };
        let mut m = Manifest::new();
        let out = execute(&e.card, &plan, &mut m, &AtomicBool::new(false), &mut |_| {}).unwrap();
        assert_eq!(out.copied, 1);
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].contains("Music/A/edited.flac"), "{:?}", out.warnings);
        let entry = &Manifest::load(&e.card).unwrap().unwrap().files["Music/A/edited.flac"];
        assert_eq!((entry.size, entry.hash.clone()), (9, file_hash(&src).unwrap()));
        assert_eq!(fs::read(on_card(&e.card, "Music/A/edited.flac")).unwrap(), b"new bytes");
    }
}
