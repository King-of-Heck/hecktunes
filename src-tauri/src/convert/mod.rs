//! Convert to AAC: .m4a copies of an album's FLAC tracks, saved beside them.

pub mod ffmpeg;

use crate::library::editing::tags_for;
use crate::library::hash::file_hash;
use crate::library::{AacCopy, Library, SyncAs, TrackRow};
use crate::model::TrackTags;
use crate::tags::write_tags;
use anyhow::{anyhow, bail, Result};
use lofty::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};

/// The bitrates offered, in kbps. AAC-LC stereo stays sensible over this whole range.
pub const BITRATES: [u32; 5] = [128, 160, 192, 256, 320];

/// What the user chose when starting a conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConvertOptions {
    pub bitrate_kbps: u32,
    /// Set the album to sync its AAC copies once the conversion finishes.
    pub switch_to_aac: bool,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self { bitrate_kbps: 256, switch_to_aac: true }
    }
}

impl ConvertOptions {
    pub fn validate(&self) -> Result<()> {
        if !BITRATES.contains(&self.bitrate_kbps) {
            bail!("{} kbps isn't one of the offered bitrates.", self.bitrate_kbps);
        }
        Ok(())
    }
}

/// Where a track's AAC copy goes: beside it, same name, `.m4a`.
pub fn aac_path_for(source: &Path) -> PathBuf {
    source.with_extension("m4a")
}

/// The copy is written here first and renamed into place once it is complete and tagged.
fn temp_path_for(source: &Path) -> PathBuf {
    source.with_extension("converting.m4a")
}

/// Whether the track can be converted: only FLAC sources (the others are already lossy, or WAV).
pub fn convertible(t: &TrackRow) -> bool {
    t.format.eq_ignore_ascii_case("flac")
}

/// Whether the track still needs converting: a FLAC without a usable AAC copy.
pub fn needs_converting(t: &TrackRow) -> bool {
    convertible(t) && !t.aac.as_ref().is_some_and(|c| c.path.is_file())
}

/// Encodes `source` with ffmpeg, writes `tags` (and the cover) into the result, then renames it
/// into place beside the source. Nothing is left behind when it fails or is cancelled.
pub fn convert_track(exe: &Path, source: &Path, kbps: u32, tags: &TrackTags, cover: Option<&[u8]>, cancel: &AtomicBool) -> Result<AacCopy> {
    let rate = lofty::read_from_path(source)
        .map_err(|e| anyhow!("couldn't read {}: {e}", source.display()))?
        .properties()
        .sample_rate()
        .unwrap_or(44_100);
    let tmp = temp_path_for(source);
    let dest = aac_path_for(source);
    let made = (|| -> Result<()> {
        ffmpeg::run(exe, &ffmpeg::args(source, &tmp, kbps, ffmpeg::target_rate(rate)), cancel)?;
        write_tags(&tmp, tags, cover)?;
        std::fs::rename(&tmp, &dest).map_err(|e| anyhow!("couldn't save {}: {e}", dest.display()))?;
        Ok(())
    })();
    if let Err(e) = made {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let size = std::fs::metadata(&dest)?.len();
    Ok(AacCopy { path: dest.clone(), size, hash: file_hash(&dest)? })
}

/// A track that couldn't be converted, and why.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Failure {
    pub title: String,
    pub error: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AlbumOutcome {
    pub converted: usize,
    pub failed: Vec<Failure>,
    pub cancelled: bool,
}

struct Work {
    track: TrackRow,
    tags: TrackTags,
}

/// Converts every track of the album that still needs it, `workers` at a time. Each finished
/// copy is recorded right away; `progress(done, total)` follows along. When at least one track
/// was converted, the run wasn't cancelled and `opts.switch_to_aac` is set, the album is set to
/// sync as AAC.
pub fn convert_album(
    lib: &Mutex<Library>,
    exe: &Path,
    album_id: i64,
    opts: &ConvertOptions,
    workers: usize,
    cancel: &AtomicBool,
    progress: &dyn Fn(usize, usize),
) -> Result<AlbumOutcome> {
    opts.validate()?;
    let (todo, cover, mut failed) = {
        let l = lib.lock().unwrap();
        let album = l.album(album_id)?.ok_or_else(|| anyhow!("That album is no longer in the library."))?;
        let tracks = l.album_tracks(album_id)?;
        let names: HashSet<PathBuf> = tracks.iter().map(|t| t.file_path.clone()).collect();
        let mut failed = Vec::new();
        let mut todo = Vec::new();
        for t in tracks.iter().filter(|t| needs_converting(t)) {
            if names.contains(&aac_path_for(&t.file_path)) {
                failed.push(Failure { title: t.title.clone(), error: "another track in the album already uses the .m4a name".into() });
                continue;
            }
            todo.push(Work { track: t.clone(), tags: tags_for(&album, t, &tracks) });
        }
        let cover = album.cover_path.as_ref().and_then(|p| std::fs::read(p).ok());
        (todo, cover, failed)
    };
    let total = todo.len();
    progress(0, total);
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel::<(usize, Result<AacCopy>)>();
    let mut converted = 0;
    std::thread::scope(|s| {
        for _ in 0..workers.max(1).min(total.max(1)) {
            let tx = tx.clone();
            let (todo, next, cover) = (&todo, &next, cover.as_deref());
            s.spawn(move || loop {
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(w) = todo.get(i) else { break };
                let r = convert_track(exe, &w.track.file_path, opts.bitrate_kbps, &w.tags, cover, cancel);
                if tx.send((i, r)).is_err() {
                    break;
                }
            });
        }
        drop(tx);
        let mut done = 0;
        for (i, r) in rx {
            let t = &todo[i].track;
            match r {
                Ok(copy) => match lib.lock().unwrap().set_track_aac(t.id, Some(&copy)) {
                    Ok(()) => converted += 1,
                    Err(e) => {
                        let _ = std::fs::remove_file(&copy.path);
                        failed.push(Failure { title: t.title.clone(), error: format!("{e:#}") });
                    }
                },
                Err(_) if cancel.load(Ordering::SeqCst) => {}
                Err(e) => failed.push(Failure { title: t.title.clone(), error: format!("{e:#}") }),
            }
            done += 1;
            progress(done, total);
        }
    });
    let cancelled = cancel.load(Ordering::SeqCst);
    if converted > 0 && !cancelled && opts.switch_to_aac {
        lib.lock().unwrap().set_album_sync_as(album_id, SyncAs::Aac)?;
    }
    Ok(AlbumOutcome { converted, failed, cancelled })
}

/// Deletes the album's AAC copies, forgets them and sets the album back to syncing FLAC.
pub fn remove_copies(lib: &mut Library, album_id: i64) -> Result<usize> {
    let tracks = lib.album_tracks(album_id)?;
    let mut removed = 0;
    for t in &tracks {
        let Some(c) = &t.aac else { continue };
        match std::fs::remove_file(&c.path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => bail!("couldn't delete {}: {e}", c.path.display()),
        }
        lib.set_track_aac(t.id, None)?;
        removed += 1;
    }
    lib.set_album_sync_as(album_id, SyncAs::Flac)?;
    Ok(removed)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConvertState {
    Queued,
    Converting { done: usize, total: usize },
    Finished { converted: usize, failed: Vec<Failure> },
    Cancelled { converted: usize },
    Failed { error: String },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConvertEvent {
    pub album_id: i64,
    #[serde(flatten)]
    pub state: ConvertState,
}

type Notify = dyn Fn(ConvertEvent) + Send + Sync;

struct Shared {
    lib: Arc<Mutex<Library>>,
    ffmpeg: Option<PathBuf>,
    workers: usize,
    /// Albums waiting, in order; the one being converted is in `current`.
    queue: Mutex<VecDeque<(i64, ConvertOptions)>>,
    current: Mutex<Option<i64>>,
    wake: Condvar,
    cancel: AtomicBool,
    notify: Box<Notify>,
}

/// Converts queued albums one at a time on its own thread.
pub struct ConvertService {
    shared: Arc<Shared>,
}

/// How many tracks to encode at once: half the CPU threads, at least one.
pub fn default_workers() -> usize {
    std::thread::available_parallelism().map(|n| n.get() / 2).unwrap_or(1).max(1)
}

impl ConvertService {
    pub fn start(lib: Arc<Mutex<Library>>, ffmpeg: Option<PathBuf>, workers: usize, notify: impl Fn(ConvertEvent) + Send + Sync + 'static) -> Self {
        let shared = Arc::new(Shared {
            lib,
            ffmpeg,
            workers,
            queue: Mutex::new(VecDeque::new()),
            current: Mutex::new(None),
            wake: Condvar::new(),
            cancel: AtomicBool::new(false),
            notify: Box::new(notify),
        });
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("hecktunes-convert".into())
            .spawn(move || worker_loop(&worker))
            .expect("couldn't start the conversion thread");
        Self { shared }
    }

    /// Why conversion can't run on this install, if it can't.
    pub fn unavailable_reason(&self) -> Option<String> {
        self.shared.ffmpeg.is_none().then(|| "ffmpeg isn't installed next to HeckTunes, so albums can't be converted.".to_string())
    }

    /// Queues the album. Refused when it is already queued or converting.
    pub fn enqueue(&self, album_id: i64, opts: ConvertOptions) -> Result<()> {
        if let Some(why) = self.unavailable_reason() {
            bail!("{why}");
        }
        opts.validate()?;
        {
            let mut q = self.shared.queue.lock().unwrap();
            if q.iter().any(|&(id, _)| id == album_id) || *self.shared.current.lock().unwrap() == Some(album_id) {
                bail!("That album is already being converted.");
            }
            q.push_back((album_id, opts));
        }
        (self.shared.notify)(ConvertEvent { album_id, state: ConvertState::Queued });
        self.shared.wake.notify_all();
        Ok(())
    }

    /// Stops the album's conversion (after removing any half-written file), or takes it off the queue.
    pub fn cancel(&self, album_id: i64) {
        let was_queued = {
            let mut q = self.shared.queue.lock().unwrap();
            let before = q.len();
            q.retain(|&(id, _)| id != album_id);
            q.len() != before
        };
        if was_queued {
            (self.shared.notify)(ConvertEvent { album_id, state: ConvertState::Cancelled { converted: 0 } });
        } else if *self.shared.current.lock().unwrap() == Some(album_id) {
            self.shared.cancel.store(true, Ordering::SeqCst);
        }
    }

    /// An album is queued or being converted.
    pub fn is_busy(&self) -> bool {
        // Same lock order as the worker (queue, then current), so the two can never deadlock.
        let q = self.shared.queue.lock().unwrap();
        !q.is_empty() || self.shared.current.lock().unwrap().is_some()
    }
}

fn worker_loop(s: &Shared) {
    loop {
        let (album_id, opts) = {
            let mut q = s.queue.lock().unwrap();
            loop {
                if let Some((id, opts)) = q.pop_front() {
                    // Set while the queue lock is held, so `is_busy` never sees a gap.
                    *s.current.lock().unwrap() = Some(id);
                    break (id, opts);
                }
                q = s.wake.wait(q).unwrap();
            }
        };
        s.cancel.store(false, Ordering::SeqCst);
        let state = match &s.ffmpeg {
            None => ConvertState::Failed { error: "ffmpeg isn't installed next to HeckTunes.".into() },
            Some(exe) => {
                let progress = |done, total| (s.notify)(ConvertEvent { album_id, state: ConvertState::Converting { done, total } });
                let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    convert_album(&s.lib, exe, album_id, &opts, s.workers, &s.cancel, &progress)
                }));
                match run {
                    Ok(Ok(o)) if o.cancelled => ConvertState::Cancelled { converted: o.converted },
                    Ok(Ok(o)) => ConvertState::Finished { converted: o.converted, failed: o.failed },
                    Ok(Err(e)) => ConvertState::Failed { error: format!("{e:#}") },
                    Err(_) => ConvertState::Failed { error: "the conversion stopped unexpectedly".into() },
                }
            }
        };
        *s.current.lock().unwrap() = None;
        (s.notify)(ConvertEvent { album_id, state });
    }
}

#[cfg(test)]
pub(crate) mod tests;
