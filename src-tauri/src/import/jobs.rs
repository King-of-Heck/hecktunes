use crate::identify::acoustid::KeyRejected;
use crate::identify::net::is_network_failure;
use crate::identify::{identify, rank, rank_candidates, Identification, MetadataSource};
use crate::import::apply::{pick_cover, stage};
use crate::import::scan::{scan_paths, scan_paths_excluding, AlbumGroup};
use crate::library::{paths, Library};
use crate::model::ReleaseCandidate;
use crate::vinyl::{self, PlanTrack, Recording, VinylPlan};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum JobState {
    Queued,
    /// Vinyl: reading the recording and finding the gaps.
    Analysing,
    Identifying,
    NeedsChoice {
        candidates: Vec<ReleaseCandidate>,
        /// The release the album being re-identified has now (None for a normal import).
        #[serde(skip_serializing_if = "Option::is_none")]
        current: Option<String>,
    },
    Duplicate { release: ReleaseCandidate, existing_album_id: i64 },
    /// Vinyl: waiting for the user to check the cuts.
    NeedsVinylReview { plan: VinylPlan },
    Importing,
    Done {
        album_id: i64,
        matched: bool,
        /// Why an import went in unmatched.
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    Skipped,
    Failed { error: String },
}

const NOTE_NOT_FOUND: &str = "No match found online";
const NOTE_NO_MATCH_CHOSEN: &str = "Imported without a match";

/// Why an import went in unmatched after a failed lookup.
fn lookup_failure_note(e: &anyhow::Error) -> String {
    if e.chain().any(|c| c.downcast_ref::<KeyRejected>().is_some()) {
        "AcoustID rejected the key \u{2014} check it in Settings.".into()
    } else if is_network_failure(e) {
        format!("Couldn't reach the music database ({e:#}). Use Fix metadata when you're online.")
    } else {
        format!("The music database lookup failed ({e:#}).")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct JobUpdate {
    pub job_id: u64,
    pub label: String,
    #[serde(flatten)]
    pub state: JobState,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobChoice {
    Pick { mbid: String },
    KeepExisting,
    Skip,
    Replace,
    Search { query: String },
    /// Vinyl: use this release instead and recompute the cuts.
    VinylRealign { mbid: String },
    /// Vinyl: write these tracks; `matched` false imports without the release.
    VinylConfirm { tracks: Vec<PlanTrack>, matched: bool },
}

pub enum Command {
    Import(Vec<PathBuf>),
    ImportVinyl(Vec<PathBuf>),
    Reimport { album_id: i64, job_id: u64 },
    Resolve { job_id: u64, choice: JobChoice },
}

pub struct ImportQueue {
    tx: Sender<Command>,
    /// Shared with the worker so ids can be handed out at enqueue time; unique and monotonic.
    next_id: Arc<AtomicU64>,
}

impl ImportQueue {
    pub fn start(
        lib: Arc<Mutex<Library>>,
        source: Arc<dyn MetadataSource>,
        notify: impl Fn(JobUpdate) + Send + 'static,
    ) -> Self {
        Self::start_with_tmp(lib, source, std::env::temp_dir().join("hecktunes-vinyl"), notify)
    }

    /// `vinyl_tmp` holds the tracks vinyl imports write before they are copied into the library.
    pub fn start_with_tmp(
        lib: Arc<Mutex<Library>>,
        source: Arc<dyn MetadataSource>,
        vinyl_tmp: PathBuf,
        notify: impl Fn(JobUpdate) + Send + 'static,
    ) -> Self {
        let (tx, rx) = channel();
        let next_id = Arc::new(AtomicU64::new(1));
        // The root never changes after the library is opened; keep a copy so files can be staged unlocked.
        let root = lib.lock().unwrap().root().to_path_buf();
        let worker = Worker {
            lib,
            root,
            source,
            notify: Box::new(notify),
            next_id: next_id.clone(),
            pending: HashMap::new(),
            vinyl_tmp,
            pending_vinyl: HashMap::new(),
        };
        thread::spawn(move || worker.run(rx));
        Self { tx, next_id }
    }

    /// Queues a re-identify of `album_id` and returns the job id its updates will carry.
    pub fn reimport(&self, album_id: i64) -> u64 {
        let job_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(Command::Reimport { album_id, job_id });
        job_id
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }
}

/// A folder deleted when dropped: the tracks a vinyl import wrote, kept until its job is done with them.
struct TempFolder(PathBuf);

impl Drop for TempFolder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A vinyl recording waiting on its review.
struct VinylJob {
    id: u64,
    label: String,
    rec: Recording,
    plan: VinylPlan,
}

struct Job {
    id: u64,
    label: String,
    group: AlbumGroup,
    /// Set when re-identifying an album already in the library.
    replace_album_id: Option<i64>,
    /// MusicBrainz release the re-identified album has now.
    current: Option<String>,
    candidates: Vec<ReleaseCandidate>,
    /// Release waiting on a Duplicate decision.
    chosen: Option<ReleaseCandidate>,
    /// Vinyl imports: the temporary tracks, deleted when the job ends.
    _temp: Option<TempFolder>,
}

struct Worker {
    lib: Arc<Mutex<Library>>,
    root: PathBuf,
    source: Arc<dyn MetadataSource>,
    notify: Box<dyn Fn(JobUpdate) + Send>,
    next_id: Arc<AtomicU64>,
    pending: HashMap<u64, Job>,
    vinyl_tmp: PathBuf,
    pending_vinyl: HashMap<u64, VinylJob>,
}

impl Worker {
    fn run(mut self, rx: Receiver<Command>) {
        while let Ok(cmd) = rx.recv() {
            match cmd {
                Command::Import(paths) => self.import(paths),
                Command::ImportVinyl(paths) => self.import_vinyl(paths),
                Command::Reimport { album_id, job_id } => self.reimport(album_id, job_id),
                Command::Resolve { job_id, choice } => self.resolve(job_id, choice),
            }
        }
    }

    fn emit(&self, id: u64, label: &str, state: JobState) {
        (self.notify)(JobUpdate { job_id: id, label: label.to_string(), state });
    }

    fn new_id(&mut self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn import(&mut self, paths: Vec<PathBuf>) {
        let scan = scan_paths_excluding(&paths, &self.root);
        for (path, reason) in scan.skipped {
            let id = self.new_id();
            let label = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            self.emit(id, &label, JobState::Failed { error: reason });
        }
        let jobs: Vec<Job> = scan
            .groups
            .into_iter()
            .map(|group| {
                let id = self.new_id();
                let label = group.label();
                self.emit(id, &label, JobState::Queued);
                Job { id, label, group, replace_album_id: None, current: None, candidates: vec![], chosen: None, _temp: None }
            })
            .collect();
        for job in jobs {
            self.identify_job(job);
        }
    }

    fn reimport(&mut self, album_id: i64, id: u64) {
        let looked_up = self.lib.lock().unwrap().album(album_id);
        let album = match looked_up {
            Ok(Some(a)) => a,
            Ok(None) => {
                self.emit(id, &format!("album {album_id}"), JobState::Failed { error: format!("album {album_id} not found") });
                return;
            }
            Err(e) => {
                self.emit(id, &format!("album {album_id}"), JobState::Failed { error: format!("{e:#}") });
                return;
            }
        };
        let label = format!("{} – {}", album.album_artist, album.title);
        let mut scan = scan_paths(&[album.folder_path.clone()]);
        if scan.groups.len() != 1 {
            self.emit(id, &label, JobState::Failed { error: "album folder is missing or changed".into() });
            return;
        }
        let group = scan.groups.remove(0);
        self.identify_job(Job { id, label, group, replace_album_id: Some(album_id), current: album.mb_release_id.clone(), candidates: vec![], chosen: None, _temp: None });
    }

    fn identify_job(&mut self, mut job: Job) {
        self.emit(job.id, &job.label, JobState::Identifying);
        if job.replace_album_id.is_some() {
            return self.reidentify_job(job);
        }
        match identify(&job.group.tracks, self.source.as_ref()) {
            Ok(Identification::Matched(release)) => self.proceed(job, release),
            Ok(Identification::Ambiguous(candidates)) => {
                job.candidates = candidates.clone();
                self.emit(job.id, &job.label, JobState::NeedsChoice { candidates, current: None });
                self.pending.insert(job.id, job);
            }
            // A user-requested re-identify must say why it failed rather than look like "nothing found".
            Err(e) if job.replace_album_id.is_some() => {
                self.emit(job.id, &job.label, JobState::Failed { error: format!("{e:#}") })
            }
            // Lookup errors on a normal import: import with what we have and say why.
            Err(e) => {
                let note = lookup_failure_note(&e);
                self.finish_unmatched(job, true, Some(note))
            }
            Ok(Identification::NotFound) => self.finish_unmatched(job, true, Some(NOTE_NOT_FOUND.into())),
        }
    }

    /// Fix metadata: always lets the user choose, even when one release is a confident match.
    fn reidentify_job(&mut self, mut job: Job) {
        let mut candidates = match rank_candidates(&job.group.tracks, self.source.as_ref()) {
            Ok(c) => c,
            // A user-requested re-identify must say why it failed rather than look like "nothing found".
            Err(e) => return self.emit(job.id, &job.label, JobState::Failed { error: format!("{e:#}") }),
        };
        if candidates.is_empty() {
            return self.finish_unmatched(job, true, None);
        }
        // The album's current release is always on offer, even if the search no longer finds it.
        if let Some(cur) = job.current.clone() {
            if !candidates.iter().any(|c| c.mbid == cur) {
                if let Ok(release) = self.source.release(&cur) {
                    candidates.push(release);
                }
            }
        }
        job.candidates = candidates.clone();
        self.emit(job.id, &job.label, JobState::NeedsChoice { candidates, current: job.current.clone() });
        self.pending.insert(job.id, job);
    }

    fn proceed(&mut self, mut job: Job, release: ReleaseCandidate) {
        let existing = self.lib.lock().unwrap().find_other_album_by_release(&release.mbid, job.replace_album_id).ok().flatten();
        // During re-identify the album being fixed may legitimately already hold this release.
        if let Some(existing_album_id) = existing {
            job.chosen = Some(release.clone());
            self.emit(job.id, &job.label, JobState::Duplicate { release, existing_album_id });
            self.pending.insert(job.id, job);
            return;
        }
        let replace: Vec<i64> = job.replace_album_id.into_iter().collect();
        self.finalize(job, Some(release), replace);
    }

    fn finish_unmatched(&mut self, job: Job, needs_metadata: bool, note: Option<String>) {
        if job.replace_album_id.is_some() {
            // Re-identify found nothing new: leave the album as it is.
            self.emit(job.id, &job.label, JobState::Skipped);
            return;
        }
        self.emit(job.id, &job.label, JobState::Importing);
        let cover = pick_cover(&job.group, None);
        // Copy and tag without the library lock; take it only for the DB transaction.
        let result = stage(&self.root, &job.group, None, cover, needs_metadata)
            .and_then(|staged| staged.commit(&mut self.lib.lock().unwrap()));
        // Free a vinyl import's temporary tracks before reporting, so the report is the last word.
        let (id, label) = (job.id, job.label.clone());
        drop(job);
        match result {
            Ok(album_id) => self.emit(id, &label, JobState::Done { album_id, matched: false, note }),
            Err(e) => self.emit(id, &label, JobState::Failed { error: format!("{e:#}") }),
        }
    }

    /// Applies the album, then removes the albums in `replace`. Once the new album is committed the
    /// job counts as done: removing the old albums and tidying the folder name are best-effort.
    fn finalize(&mut self, job: Job, release: Option<ReleaseCandidate>, replace: Vec<i64>) {
        self.emit(job.id, &job.label, JobState::Importing);
        let online = release.as_ref().and_then(|r| self.source.cover(&r.mbid).ok().flatten());
        // Fresh imports copy from outside the library, so they stage without the library lock and
        // take it only for the DB transaction and cleanup. A reimport copies the album's own library
        // files, which update_album/update_track rewrite in place under the lock: hold the lock across
        // stage + commit + cleanup so the copy can't be torn and the intact old folder is never
        // deleted in favour of a torn one. Never notify while holding it.
        let mut held = if job.replace_album_id.is_some() { Some(self.lib.lock().unwrap()) } else { None };
        let cover = pick_cover(&job.group, online); // may read the album's own files, so after taking the lock
        let staged = stage(&self.root, &job.group, release.as_ref(), cover, false);
        let mut lib = held.take().unwrap_or_else(|| self.lib.lock().unwrap());
        let result = staged.and_then(|s| s.commit(&mut lib));
        if let (Ok(album_id), false) = (&result, replace.is_empty()) {
            if let Err(e) = replace_cleanup(&mut lib, *album_id, &replace) {
                eprintln!("import job {}: cleanup after replace failed: {}", job.id, ascii(&format!("{e:#}")));
            }
        }
        drop(lib);
        // Free a vinyl import's temporary tracks before reporting, so the report is the last word.
        let (id, label) = (job.id, job.label.clone());
        drop(job);
        match result {
            Ok(album_id) => self.emit(id, &label, JobState::Done { album_id, matched: release.is_some(), note: None }),
            Err(e) => self.emit(id, &label, JobState::Failed { error: format!("{e:#}") }),
        }
    }

    fn import_vinyl(&mut self, paths: Vec<PathBuf>) {
        let id = self.new_id();
        let label = vinyl_label(&paths);
        self.emit(id, &label, JobState::Analysing);
        let rec = match vinyl::load(&paths) {
            Ok(rec) => rec,
            Err(e) => return self.emit(id, &label, JobState::Failed { error: format!("{e:#}") }),
        };
        self.emit(id, &label, JobState::Identifying);
        let (candidates, note) = match vinyl::identify::identify(&rec, self.source.as_ref()) {
            Ok(c) if c.is_empty() => (c, Some(NOTE_NOT_FOUND.to_string())),
            Ok(c) => (c, None),
            Err(e) => (vec![], Some(lookup_failure_note(&e))),
        };
        let plan = vinyl::plan(&rec, candidates.first(), candidates.clone(), note);
        self.vinyl_review(VinylJob { id, label, rec, plan });
    }

    fn vinyl_review(&mut self, job: VinylJob) {
        self.emit(job.id, &job.label, JobState::NeedsVinylReview { plan: job.plan.clone() });
        self.pending_vinyl.insert(job.id, job);
    }

    fn resolve_vinyl(&mut self, mut job: VinylJob, choice: JobChoice) {
        match choice {
            JobChoice::Skip => self.emit(job.id, &job.label, JobState::Skipped),
            JobChoice::VinylRealign { mbid } => {
                let known = job.plan.candidates.iter().find(|c| c.mbid == mbid).cloned();
                match known.map(Ok).unwrap_or_else(|| self.source.release(&mbid)) {
                    Ok(r) => {
                        let mut candidates = job.plan.candidates.clone();
                        if !candidates.iter().any(|c| c.mbid == r.mbid) {
                            candidates.insert(0, r.clone());
                        }
                        job.plan = vinyl::plan(&job.rec, Some(&r), candidates, None);
                        self.vinyl_review(job);
                    }
                    Err(e) => self.vinyl_review_with(job, Some(format!("Couldn't load that release ({e:#})."))),
                }
            }
            // The search's own message replaces any earlier one; the release's length warning stays.
            JobChoice::Search { query } => {
                let message = match self.source.search_text(&query) {
                    Ok(ids) => {
                        let found: Vec<_> =
                            ids.iter().take(crate::identify::MAX_CANDIDATES).filter_map(|id| self.source.release(id).ok()).collect();
                        if found.is_empty() {
                            Some("No releases found for that search.".to_string())
                        } else {
                            job.plan.candidates = found;
                            None
                        }
                    }
                    Err(e) => Some(format!("Search failed ({e:#}).")),
                };
                self.vinyl_review_with(job, message);
            }
            JobChoice::VinylConfirm { tracks, matched } => self.confirm_vinyl(job, tracks, matched),
            // Choices for normal imports don't apply here: keep waiting.
            JobChoice::Pick { .. } | JobChoice::KeepExisting | JobChoice::Replace => self.vinyl_review(job),
        }
    }

    /// Shows the review again with `message` above the release's own warning.
    fn vinyl_review_with(&mut self, mut job: VinylJob, message: Option<String>) {
        job.plan.warning = vinyl::warning(&job.rec, job.plan.release.as_ref(), message);
        self.vinyl_review(job);
    }

    fn confirm_vinyl(&mut self, mut job: VinylJob, tracks: Vec<PlanTrack>, matched: bool) {
        let release = if matched { job.plan.release.clone() } else { None };
        let job_dir = self.vinyl_tmp.join(format!("job-{}", job.id));
        // Refused before anything is written: back to the review with the user's cuts kept.
        let refused = vinyl::check_tracks(&job.rec, &tracks, release.as_ref())
            .err()
            .map(|e| format!("{e:#}"))
            .or_else(|| vinyl::space_problem(&job.rec, &tracks, &job_dir));
        if let Some(problem) = refused {
            job.plan.tracks = tracks;
            return self.vinyl_review_with(job, Some(problem));
        }
        self.emit(job.id, &job.label, JobState::Importing);
        let dir = TempFolder(job_dir);
        let group = match vinyl::write_tracks(&job.rec, &tracks, &dir.0) {
            Ok(g) => g,
            Err(e) => return self.emit(job.id, &job.label, JobState::Failed { error: format!("{e:#}") }),
        };
        let import = Job {
            id: job.id,
            label: job.label.clone(),
            group,
            replace_album_id: None,
            current: None,
            candidates: vec![],
            chosen: None,
            _temp: Some(dir),
        };
        match release {
            Some(r) => self.proceed(import, r),
            None => {
                let note = job.plan.note.clone().unwrap_or_else(|| NOTE_NO_MATCH_CHOSEN.to_string());
                self.finish_unmatched(import, true, Some(note))
            }
        }
    }

    fn resolve(&mut self, job_id: u64, choice: JobChoice) {
        if let Some(job) = self.pending_vinyl.remove(&job_id) {
            return self.resolve_vinyl(job, choice);
        }
        let Some(mut job) = self.pending.remove(&job_id) else { return };
        match choice {
            JobChoice::Skip => self.emit(job.id, &job.label, JobState::Skipped),
            // On a duplicate prompt, keeping the existing album means leaving the library alone.
            JobChoice::KeepExisting if job.chosen.is_some() => self.emit(job.id, &job.label, JobState::Skipped),
            JobChoice::KeepExisting => self.finish_unmatched(job, false, None),
            JobChoice::Replace => {
                let release = job.chosen.take();
                let existing = release.as_ref().and_then(|r| self.lib.lock().unwrap().find_other_album_by_release(&r.mbid, job.replace_album_id).ok().flatten());
                // A re-identified album is superseded too: the new copy is made from its files.
                let mut replace: Vec<i64> = existing.into_iter().chain(job.replace_album_id).collect();
                replace.dedup();
                self.finalize(job, release, replace);
            }
            JobChoice::Pick { mbid } => {
                let found = job.candidates.iter().find(|c| c.mbid == mbid).cloned();
                match found.map(Ok).unwrap_or_else(|| self.source.release(&mbid)) {
                    Ok(release) => self.proceed(job, release),
                    Err(e) => self.emit(job.id, &job.label, JobState::Failed { error: format!("{e:#}") }),
                }
            }
            // Vinyl choices don't apply to a normal import: keep waiting.
            JobChoice::VinylRealign { .. } | JobChoice::VinylConfirm { .. } => {
                self.pending.insert(job.id, job);
            }
            JobChoice::Search { query } => {
                let ids = match self.source.search_text(&query) {
                    Ok(ids) => ids,
                    Err(e) => {
                        self.emit(job.id, &job.label, JobState::Failed { error: format!("{e:#}") });
                        return;
                    }
                };
                let found: Vec<ReleaseCandidate> = ids
                    .iter()
                    .take(crate::identify::MAX_CANDIDATES)
                    .filter_map(|id| self.source.release(id).ok())
                    .collect();
                job.candidates = rank(&job.group.tracks, found);
                self.emit(job.id, &job.label, JobState::NeedsChoice { candidates: job.candidates.clone(), current: job.current.clone() });
                self.pending.insert(job.id, job);
            }
        }
    }
}

/// "Side A + Side B" from the file names.
fn vinyl_label(paths: &[PathBuf]) -> String {
    let mut names: Vec<String> = paths.iter().filter_map(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()).collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.join(" + ")
}

/// Removes the superseded albums (with their files) and moves the new album out of a
/// "<Album> [2]"-style folder if the plain name is now free. Tries every step; returns the first error.
fn replace_cleanup(lib: &mut Library, album_id: i64, old: &[i64]) -> anyhow::Result<()> {
    let mut first_err = None;
    for &id in old.iter().filter(|&&id| id != album_id) {
        if let Err(e) = lib.transfer_album_refs(id, album_id) {
            // Deleting the old album now would drop its playlist lines and tick: keep it.
            first_err.get_or_insert(e);
            continue;
        }
        if let Err(e) = lib.delete_album(id, true) {
            first_err.get_or_insert(e);
        }
    }
    let tidy = (|| -> anyhow::Result<()> {
        if let Some(row) = lib.album(album_id)? {
            let wanted = paths::album_dir(&row.album_artist, &row.title, row.year.as_deref());
            if row.folder_path != lib.root().join(&wanted) && !lib.root().join(&wanted).exists() {
                lib.move_album_folder(album_id, &wanted)?;
            }
        }
        Ok(())
    })();
    if let Err(e) = tidy {
        first_err.get_or_insert(e);
    }
    first_err.map_or(Ok(()), Err)
}

/// Console is not UTF-8 everywhere; keep log lines ASCII.
fn ascii(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii() { c } else { '?' }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Library;
    use crate::test_support::{candidate, lock_folder, tagged_wav, FakeSource};
    use std::sync::mpsc::{channel, Receiver};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    const TL: [(&str, u64); 2] = [("One", 1000), ("Two", 2000)];

    struct Harness {
        _dir: tempfile::TempDir,
        src: PathBuf,
        lib: Arc<Mutex<Library>>,
        queue: ImportQueue,
        rx: Receiver<JobUpdate>,
    }

    fn harness(fake: FakeSource) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("Album Src");
        std::fs::create_dir(&src).unwrap();
        tagged_wav(&src.join("1.wav"), 1, "One", "Album", "Band", 1);
        tagged_wav(&src.join("2.wav"), 2, "Two", "Album", "Band", 2);
        let lib = Arc::new(Mutex::new(Library::open_in_memory(dir.path().join("lib")).unwrap()));
        let (tx, rx) = channel();
        let queue = ImportQueue::start(lib.clone(), Arc::new(fake), move |u| {
            let _ = tx.send(u);
        });
        Harness { _dir: dir, src, lib, queue, rx }
    }

    /// Collect updates until one matches `stop` (or panic after 10 s).
    fn until(rx: &Receiver<JobUpdate>, stop: impl Fn(&JobState) -> bool) -> Vec<JobUpdate> {
        let mut seen = Vec::new();
        loop {
            let u = rx.recv_timeout(Duration::from_secs(10)).expect("timed out waiting for job update");
            let done = stop(&u.state);
            seen.push(u);
            if done {
                return seen;
            }
        }
    }

    fn is_terminal(s: &JobState) -> bool {
        matches!(s, JobState::Done { .. } | JobState::Skipped | JobState::Failed { .. })
    }

    #[test]
    fn clear_match_imports_automatically() {
        let h = harness(FakeSource { releases: vec![candidate("m1", "Album", "Band", &TL)], search_ids: vec!["m1".into()] });
        h.queue.send(Command::Import(vec![h.src.clone()]));
        let ups = until(&h.rx, is_terminal);
        assert!(matches!(ups.last().unwrap().state, JobState::Done { matched: true, .. }));
        assert!(ups.iter().any(|u| matches!(u.state, JobState::Identifying)));
        assert_eq!(ups[0].label, "Band – Album");
        let albums = h.lib.lock().unwrap().list_albums().unwrap();
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].mb_release_id.as_deref(), Some("m1"));
    }

    #[test]
    fn ambiguous_waits_for_pick() {
        // Same tracks, different album title ("Album (Live)") → not equivalent editions, scores ~1.0 vs ~0.96 → ask.
        let h = harness(FakeSource {
            releases: vec![candidate("a", "Album", "Band", &TL), candidate("b", "Album (Live)", "Band", &TL)],
            search_ids: vec!["a".into(), "b".into()],
        });
        h.queue.send(Command::Import(vec![h.src.clone()]));
        let ups = until(&h.rx, |s| matches!(s, JobState::NeedsChoice { .. }) || is_terminal(s));
        let last = ups.last().unwrap();
        let job_id = last.job_id;
        match &last.state {
            JobState::NeedsChoice { candidates, .. } => assert_eq!(candidates.len(), 2),
            other => panic!("expected NeedsChoice, got {other:?}"),
        }
        h.queue.send(Command::Resolve { job_id, choice: JobChoice::Pick { mbid: "b".into() } });
        let ups = until(&h.rx, is_terminal);
        assert!(matches!(ups.last().unwrap().state, JobState::Done { matched: true, .. }));
        let albums = h.lib.lock().unwrap().list_albums().unwrap();
        assert_eq!(albums[0].mb_release_id.as_deref(), Some("b"));
    }

    #[test]
    fn duplicate_asks_then_replace_swaps_album() {
        let h = harness(FakeSource { releases: vec![candidate("m1", "Album", "Band", &TL)], search_ids: vec!["m1".into()] });
        h.queue.send(Command::Import(vec![h.src.clone()]));
        until(&h.rx, is_terminal);
        let first_id = h.lib.lock().unwrap().list_albums().unwrap()[0].id;

        h.queue.send(Command::Import(vec![h.src.clone()]));
        let ups = until(&h.rx, |s| matches!(s, JobState::Duplicate { .. }) || is_terminal(s));
        let last = ups.last().unwrap();
        assert!(matches!(last.state, JobState::Duplicate { existing_album_id, .. } if existing_album_id == first_id));

        h.queue.send(Command::Resolve { job_id: last.job_id, choice: JobChoice::Replace });
        until(&h.rx, is_terminal);
        let albums = h.lib.lock().unwrap().list_albums().unwrap();
        assert_eq!(albums.len(), 1);
        assert_ne!(albums[0].id, first_id);
        assert!(albums[0].folder_path.ends_with("Album (2001)"), "suffix removed after replace: {:?}", albums[0].folder_path);
    }

    #[test]
    fn skip_choice_on_duplicate_leaves_library_unchanged() {
        let h = harness(FakeSource { releases: vec![candidate("m1", "Album", "Band", &TL)], search_ids: vec!["m1".into()] });
        h.queue.send(Command::Import(vec![h.src.clone()]));
        until(&h.rx, is_terminal);
        h.queue.send(Command::Import(vec![h.src.clone()]));
        let ups = until(&h.rx, |s| matches!(s, JobState::Duplicate { .. }));
        h.queue.send(Command::Resolve { job_id: ups.last().unwrap().job_id, choice: JobChoice::Skip });
        let ups = until(&h.rx, is_terminal);
        assert!(matches!(ups.last().unwrap().state, JobState::Skipped));
        assert_eq!(h.lib.lock().unwrap().list_albums().unwrap().len(), 1);
    }

    #[test]
    fn not_found_imports_with_needs_metadata_then_reimport_fixes_it() {
        let h = harness(FakeSource { releases: vec![], search_ids: vec![] });
        h.queue.send(Command::Import(vec![h.src.clone()]));
        let ups = until(&h.rx, is_terminal);
        assert!(matches!(ups.last().unwrap().state, JobState::Done { matched: false, .. }));
        let album = h.lib.lock().unwrap().list_albums().unwrap().remove(0);
        assert!(album.needs_metadata);

        // Swap in a source that now knows the album: simulate by a new queue on the same library.
        let (tx, rx) = channel();
        let fixed = FakeSource { releases: vec![candidate("m9", "Album", "Band", &TL)], search_ids: vec!["m9".into()] };
        let q2 = ImportQueue::start(h.lib.clone(), Arc::new(fixed), move |u| {
            let _ = tx.send(u);
        });
        q2.reimport(album.id);
        pick_when_asked(&q2, &rx, "m9");
        let ups = until(&rx, is_terminal);
        assert!(matches!(ups.last().unwrap().state, JobState::Done { matched: true, .. }));
        let albums = h.lib.lock().unwrap().list_albums().unwrap();
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].mb_release_id.as_deref(), Some("m9"));
        assert!(!albums[0].needs_metadata);
    }

    #[test]
    fn unsupported_file_reported_as_failed_job() {
        let h = harness(FakeSource { releases: vec![], search_ids: vec![] });
        let wma = h.src.parent().unwrap().join("x.wma");
        std::fs::write(&wma, b"x").unwrap();
        h.queue.send(Command::Import(vec![wma]));
        let ups = until(&h.rx, is_terminal);
        assert!(matches!(ups.last().unwrap().state, JobState::Failed { .. }));
        assert_eq!(ups.last().unwrap().label, "x.wma");
    }

    /// FakeSource whose network calls can be made to fail.
    struct FlakySource {
        inner: FakeSource,
        fail_search: bool,
        fail_search_text: bool,
        /// Calls to search, by_fingerprint and release.
        calls: Arc<Mutex<[usize; 3]>>,
    }

    impl MetadataSource for FlakySource {
        fn search(&self, album: &str, artist: &str, n: usize) -> anyhow::Result<Vec<String>> {
            self.calls.lock().unwrap()[0] += 1;
            if self.fail_search {
                return Err(anyhow::Error::from(ureq::Error::HostNotFound).context("network down"));
            }
            self.inner.search(album, artist, n)
        }
        fn search_text(&self, query: &str) -> anyhow::Result<Vec<String>> {
            if self.fail_search_text {
                anyhow::bail!("search service unavailable");
            }
            self.inner.search_text(query)
        }
        fn by_fingerprint(&self, t: &crate::model::SourceTrack) -> anyhow::Result<Vec<String>> {
            self.calls.lock().unwrap()[1] += 1;
            if self.fail_search {
                return Err(anyhow::Error::from(ureq::Error::HostNotFound).context("network down"));
            }
            self.inner.by_fingerprint(t)
        }
        fn release(&self, mbid: &str) -> anyhow::Result<ReleaseCandidate> {
            self.calls.lock().unwrap()[2] += 1;
            self.inner.release(mbid)
        }
        fn cover(&self, mbid: &str) -> anyhow::Result<Option<Vec<u8>>> {
            self.inner.cover(mbid)
        }
    }

    impl FlakySource {
        /// Every lookup fails, as when the machine is offline.
        fn offline() -> Self {
            FlakySource { inner: fake(&[], vec![]), fail_search: true, fail_search_text: true, calls: Default::default() }
        }
    }

    #[test]
    fn unmatched_imports_say_why() {
        let h = harness(fake(&[], vec![]));
        h.queue.send(Command::Import(vec![h.src.clone()]));
        match until(&h.rx, is_terminal).last().unwrap().state.clone() {
            JobState::Done { matched: false, note: Some(n), .. } => assert_eq!(n, "No match found online"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn imports_while_offline_say_so() {
        let h = harness(fake(&[], vec![]));
        let offline = FlakySource::offline();
        let calls = offline.calls.clone();
        let (q2, rx2) = start_queue(h.lib.clone(), offline);
        q2.send(Command::Import(vec![h.src.clone()]));
        match until(&rx2, is_terminal).last().unwrap().state.clone() {
            JobState::Done { matched: false, note: Some(n), .. } => {
                assert!(n.starts_with("Couldn't reach the music database (network down: "), "{n}");
                assert!(n.ends_with("Use Fix metadata when you're online."), "{n}");
            }
            other => panic!("{other:?}"),
        }
        // One network failure ends the lookup: no fingerprint or release lookups after it.
        assert_eq!(*calls.lock().unwrap(), [1, 0, 0]);
    }

    #[test]
    fn lookup_notes_tell_network_trouble_from_other_failures() {
        let net = anyhow::Error::from(ureq::Error::ConnectionFailed).context("release lookup");
        let note = lookup_failure_note(&net);
        assert!(note.starts_with("Couldn't reach the music database (release lookup: "), "{note}");
        assert!(note.ends_with("). Use Fix metadata when you're online."), "{note}");
        let other = anyhow::anyhow!("bad JSON").context("release lookup");
        assert_eq!(lookup_failure_note(&other), "The music database lookup failed (release lookup: bad JSON).");
        let not_found = anyhow::Error::from(ureq::Error::StatusCode(404));
        assert!(lookup_failure_note(&not_found).starts_with("The music database lookup failed ("));
        for code in [400, 401, 403] {
            let rejected = crate::identify::acoustid::lookup_error(ureq::Error::StatusCode(code));
            assert_eq!(lookup_failure_note(&rejected), "AcoustID rejected the key \u{2014} check it in Settings.");
        }
    }

    #[test]
    fn done_without_a_note_has_no_note_field() {
        let v = serde_json::to_value(JobState::Done { album_id: 1, matched: true, note: None }).unwrap();
        assert!(v.get("note").is_none());
    }

    /// FakeSource whose `cover` call (made just before staging) signals the test and then stalls,
    /// giving the test time to take the library lock the way a concurrent update command would.
    struct GateSource {
        inner: FakeSource,
        reached: Mutex<std::sync::mpsc::Sender<()>>,
    }

    impl MetadataSource for GateSource {
        fn search(&self, album: &str, artist: &str, n: usize) -> anyhow::Result<Vec<String>> {
            self.inner.search(album, artist, n)
        }
        fn search_text(&self, query: &str) -> anyhow::Result<Vec<String>> {
            self.inner.search_text(query)
        }
        fn by_fingerprint(&self, t: &crate::model::SourceTrack) -> anyhow::Result<Vec<String>> {
            self.inner.by_fingerprint(t)
        }
        fn release(&self, mbid: &str) -> anyhow::Result<ReleaseCandidate> {
            self.inner.release(mbid)
        }
        fn cover(&self, mbid: &str) -> anyhow::Result<Option<Vec<u8>>> {
            let _ = self.reached.lock().unwrap().send(());
            std::thread::sleep(Duration::from_millis(300));
            self.inner.cover(mbid)
        }
    }

    fn album_dir_count(root: &std::path::Path) -> usize {
        std::fs::read_dir(root.join("Band")).map(|d| d.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()).count()).unwrap_or(0)
    }

    #[test]
    fn reimport_does_not_stage_while_the_library_is_locked() {
        let h = harness(fake(&[], vec![]));
        let x = import_unmatched(&h);
        let root = h.lib.lock().unwrap().root().to_path_buf();
        assert_eq!(album_dir_count(&root), 1);

        let (tx, reached) = channel();
        let (q2, rx2) = start_queue(
            h.lib.clone(),
            GateSource { inner: fake(&["m9"], vec![candidate("m9", "Album", "Band", &TL)]), reached: Mutex::new(tx) },
        );
        q2.reimport(x);
        pick_when_asked(&q2, &rx2, "m9");
        reached.recv_timeout(Duration::from_secs(10)).expect("worker reached the cover lookup");
        // Same moment a concurrent update_album would hold the lock: the reimport must not be copying.
        let guard = h.lib.lock().unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(album_dir_count(&root), 1, "reimport staged its copy while the library lock was held elsewhere");
        drop(guard);

        let ups = until(&rx2, is_terminal);
        assert!(matches!(ups.last().unwrap().state, JobState::Done { matched: true, .. }), "{:?}", ups.last());
        assert_eq!(album_dir_count(&root), 1, "old folder replaced by the new one");
    }

    fn start_queue(lib: Arc<Mutex<Library>>, source: impl MetadataSource + 'static) -> (ImportQueue, Receiver<JobUpdate>) {
        let (tx, rx) = channel();
        let q = ImportQueue::start(lib, Arc::new(source), move |u| {
            let _ = tx.send(u);
        });
        (q, rx)
    }

    fn fake(ids: &[&str], releases: Vec<ReleaseCandidate>) -> FakeSource {
        FakeSource { releases, search_ids: ids.iter().map(|s| s.to_string()).collect() }
    }

    /// Imports the harness album with no match, so it lands as needs_metadata. Returns its id.
    fn import_unmatched(h: &Harness) -> i64 {
        h.queue.send(Command::Import(vec![h.src.clone()]));
        let ups = until(&h.rx, is_terminal);
        match ups.last().unwrap().state {
            JobState::Done { album_id, matched: false, .. } => album_id,
            ref other => panic!("expected unmatched Done, got {other:?}"),
        }
    }

    #[test]
    fn fix_metadata_keeps_playlist_lines_and_echo_mini_tick() {
        use crate::library::SelectionKind;
        let h = harness(fake(&[], vec![]));
        let x = import_unmatched(&h);
        let pl = {
            let mut lib = h.lib.lock().unwrap();
            let ids: Vec<i64> = lib.album_tracks(x).unwrap().iter().map(|t| t.id).collect();
            let pl = lib.create_playlist("Mix").unwrap();
            lib.add_to_playlist(pl, &ids).unwrap();
            lib.set_selected(SelectionKind::Album, &x.to_string(), true).unwrap();
            pl
        };
        let (q2, rx2) = start_queue(h.lib.clone(), fake(&["m9"], vec![candidate("m9", "Album", "Band", &TL)]));
        q2.reimport(x);
        pick_when_asked(&q2, &rx2, "m9");
        let new_id = match until(&rx2, is_terminal).last().unwrap().state {
            JobState::Done { album_id, .. } => album_id,
            ref other => panic!("expected Done, got {other:?}"),
        };
        let lib = h.lib.lock().unwrap();
        let entries = lib.playlist_entries(pl).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.song.track.album_id == new_id));
        assert_eq!(lib.selection().unwrap().albums, vec![new_id]);
    }

    #[test]
    fn reimport_onto_release_owned_by_another_album_asks_duplicate() {
        let h = harness(fake(&[], vec![]));
        let x = import_unmatched(&h);
        // Album Y already holds release m1 (imported from a second folder).
        let other = h.src.parent().unwrap().join("Other Src");
        std::fs::create_dir(&other).unwrap();
        tagged_wav(&other.join("1.wav"), 1, "One", "Album", "Band", 1);
        tagged_wav(&other.join("2.wav"), 2, "Two", "Album", "Band", 2);
        let (q2, rx2) = start_queue(h.lib.clone(), fake(&["m1"], vec![candidate("m1", "Album", "Band", &TL)]));
        q2.send(Command::Import(vec![other]));
        let y = match until(&rx2, is_terminal).last().unwrap().state {
            JobState::Done { album_id, matched: true, .. } => album_id,
            ref other => panic!("expected matched Done, got {other:?}"),
        };

        q2.reimport(x);
        pick_when_asked(&q2, &rx2, "m1");
        let ups = until(&rx2, |s| matches!(s, JobState::Duplicate { .. }) || is_terminal(s));
        let last = ups.last().unwrap();
        assert!(
            matches!(last.state, JobState::Duplicate { existing_album_id, .. } if existing_album_id == y),
            "expected Duplicate of {y}, got {:?}",
            last.state
        );

        // Replace: the re-identified copy supersedes both Y and the old X.
        q2.send(Command::Resolve { job_id: last.job_id, choice: JobChoice::Replace });
        let ups = until(&rx2, is_terminal);
        assert!(matches!(ups.last().unwrap().state, JobState::Done { matched: true, .. }), "{:?}", ups.last());
        let albums = h.lib.lock().unwrap().list_albums().unwrap();
        assert_eq!(albums.len(), 1, "{albums:?}");
        assert_eq!(albums[0].mb_release_id.as_deref(), Some("m1"));
        assert!(albums[0].id != x && albums[0].id != y);
    }

    #[test]
    fn keep_existing_on_duplicate_is_treated_as_skip() {
        let h = harness(fake(&["m1"], vec![candidate("m1", "Album", "Band", &TL)]));
        h.queue.send(Command::Import(vec![h.src.clone()]));
        until(&h.rx, is_terminal);
        h.queue.send(Command::Import(vec![h.src.clone()]));
        let ups = until(&h.rx, |s| matches!(s, JobState::Duplicate { .. }) || is_terminal(s));
        assert!(matches!(ups.last().unwrap().state, JobState::Duplicate { .. }));
        h.queue.send(Command::Resolve { job_id: ups.last().unwrap().job_id, choice: JobChoice::KeepExisting });
        let ups = until(&h.rx, is_terminal);
        assert!(matches!(ups.last().unwrap().state, JobState::Skipped), "{:?}", ups.last());
        assert_eq!(h.lib.lock().unwrap().list_albums().unwrap().len(), 1);
    }

    #[test]
    fn reimport_reports_done_even_if_old_album_cleanup_fails() {
        let h = harness(fake(&[], vec![]));
        let x = import_unmatched(&h);
        let old_folder = h.lib.lock().unwrap().album(x).unwrap().unwrap().folder_path;
        let guard = lock_folder(&old_folder);

        let (q2, rx2) = start_queue(h.lib.clone(), fake(&["m9"], vec![candidate("m9", "Album", "Band", &TL)]));
        q2.reimport(x);
        pick_when_asked(&q2, &rx2, "m9");
        let ups = until(&rx2, is_terminal);
        drop(guard);
        let new_id = match ups.last().unwrap().state {
            JobState::Done { album_id, matched: true, .. } => album_id,
            ref other => panic!("expected matched Done despite cleanup failure, got {other:?}"),
        };
        let lib = h.lib.lock().unwrap();
        let new_album = lib.album(new_id).unwrap().expect("new album committed");
        assert_eq!(new_album.mb_release_id.as_deref(), Some("m9"));
        assert!(new_album.folder_path.exists());
        assert!(lib.album(x).unwrap().is_some(), "a failed cleanup keeps the old album listed");
    }

    #[test]
    fn reimport_with_lookup_error_reports_failed() {
        let h = harness(FakeSource { releases: vec![], search_ids: vec![] });
        let x = import_unmatched(&h);
        let (q2, rx2) = start_queue(
            h.lib.clone(),
            FlakySource { inner: fake(&[], vec![]), fail_search: true, fail_search_text: false, calls: Default::default() },
        );
        q2.reimport(x);
        let ups = until(&rx2, is_terminal);
        match &ups.last().unwrap().state {
            JobState::Failed { error } => assert!(error.contains("network down"), "{error}"),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(h.lib.lock().unwrap().list_albums().unwrap().len(), 1);
    }

    #[test]
    fn import_with_lookup_error_still_imports_with_needs_metadata() {
        let h = harness(FakeSource { releases: vec![], search_ids: vec![] });
        let (q2, rx2) = start_queue(
            h.lib.clone(),
            FlakySource { inner: fake(&[], vec![]), fail_search: true, fail_search_text: false, calls: Default::default() },
        );
        q2.send(Command::Import(vec![h.src.clone()]));
        let ups = until(&rx2, is_terminal);
        assert!(matches!(ups.last().unwrap().state, JobState::Done { matched: false, .. }));
        assert!(h.lib.lock().unwrap().list_albums().unwrap()[0].needs_metadata);
    }

    #[test]
    fn search_choice_with_lookup_error_reports_failed() {
        let releases = vec![candidate("a", "Album", "Band", &TL), candidate("b", "Album (Live)", "Band", &TL)];
        let h = harness(FakeSource { releases: vec![], search_ids: vec![] });
        let (q2, rx2) = start_queue(
            h.lib.clone(),
            FlakySource { inner: fake(&["a", "b"], releases), fail_search: false, fail_search_text: true, calls: Default::default() },
        );
        q2.send(Command::Import(vec![h.src.clone()]));
        let ups = until(&rx2, |s| matches!(s, JobState::NeedsChoice { .. }) || is_terminal(s));
        assert!(matches!(ups.last().unwrap().state, JobState::NeedsChoice { .. }));
        q2.send(Command::Resolve { job_id: ups.last().unwrap().job_id, choice: JobChoice::Search { query: "x".into() } });
        let ups = until(&rx2, |s| matches!(s, JobState::NeedsChoice { .. }) || is_terminal(s));
        match &ups.last().unwrap().state {
            JobState::Failed { error } => assert!(error.contains("search service unavailable"), "{error}"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn reimport_returns_the_job_id_its_updates_carry() {
        let h = harness(FakeSource { releases: vec![candidate("m1", "Album", "Band", &TL)], search_ids: vec!["m1".into()] });
        h.queue.send(Command::Import(vec![h.src.clone()]));
        let first = until(&h.rx, is_terminal);
        let album_id = h.lib.lock().unwrap().list_albums().unwrap()[0].id;
        let id = h.queue.reimport(album_id);
        let ups = until(&h.rx, |s| matches!(s, JobState::NeedsChoice { .. }) || is_terminal(s));
        assert!(ups.len() > 1);
        assert!(ups.iter().all(|u| u.job_id == id), "all updates carry the returned id");
        assert!(first.iter().all(|u| u.job_id < id), "ids stay monotonic across import and reimport");
        assert_ne!(h.queue.reimport(album_id), id);
    }

    #[test]
    fn reimport_of_missing_album_reports_failed() {
        let h = harness(FakeSource { releases: vec![], search_ids: vec![] });
        h.queue.reimport(999);
        let ups = until(&h.rx, is_terminal);
        assert!(matches!(ups.last().unwrap().state, JobState::Failed { .. }), "{:?}", ups.last());
    }

    #[test]
    fn job_update_serializes_flat_for_the_ui() {
        let u = JobUpdate { job_id: 3, label: "L".into(), state: JobState::Done { album_id: 7, matched: true, note: None } };
        let v = serde_json::to_value(&u).unwrap();
        assert_eq!(v, serde_json::json!({"job_id": 3, "label": "L", "state": "done", "album_id": 7, "matched": true}));
        let c: JobChoice = serde_json::from_value(serde_json::json!({"kind": "pick", "mbid": "x"})).unwrap();
        assert!(matches!(c, JobChoice::Pick { mbid } if mbid == "x"));
    }

    #[test]
    fn dropping_a_library_folder_does_not_reimport_it() {
        let h = harness(fake(&[], vec![]));
        let x = import_unmatched(&h);
        let folder = h.lib.lock().unwrap().album(x).unwrap().unwrap().folder_path;
        h.queue.send(Command::Import(vec![folder]));
        let ups = until(&h.rx, is_terminal);
        match &ups.last().unwrap().state {
            JobState::Failed { error } => assert!(error.contains("library"), "{error}"),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(h.lib.lock().unwrap().list_albums().unwrap().len(), 1);
    }

    /// Waits for a re-identify's NeedsChoice, then picks `mbid`. Returns the NeedsChoice update.
    fn pick_when_asked(q: &ImportQueue, rx: &Receiver<JobUpdate>, mbid: &str) -> JobUpdate {
        let last = until(rx, |s| matches!(s, JobState::NeedsChoice { .. }) || is_terminal(s)).pop().unwrap();
        assert!(matches!(last.state, JobState::NeedsChoice { .. }), "expected NeedsChoice, got {:?}", last.state);
        q.send(Command::Resolve { job_id: last.job_id, choice: JobChoice::Pick { mbid: mbid.into() } });
        last
    }

    /// Imports the harness album matched to `mbid` (a confident match), returning its album id.
    fn import_matched(h: &Harness) -> i64 {
        h.queue.send(Command::Import(vec![h.src.clone()]));
        match until(&h.rx, is_terminal).last().unwrap().state {
            JobState::Done { album_id, matched: true, .. } => album_id,
            ref other => panic!("expected matched Done, got {other:?}"),
        }
    }

    #[test]
    fn reidentify_with_a_single_confident_match_still_asks_and_marks_the_current_release() {
        let h = harness(fake(&["m1"], vec![candidate("m1", "Album", "Band", &TL)]));
        let x = import_matched(&h);
        let id = h.queue.reimport(x);
        let ups = until(&h.rx, |s| matches!(s, JobState::NeedsChoice { .. }) || is_terminal(s));
        let last = ups.last().unwrap();
        assert_eq!(last.job_id, id);
        match &last.state {
            JobState::NeedsChoice { candidates, current } => {
                assert_eq!(candidates.iter().map(|c| c.mbid.as_str()).collect::<Vec<_>>(), ["m1"]);
                assert_eq!(current.as_deref(), Some("m1"));
            }
            other => panic!("expected NeedsChoice, got {other:?}"),
        }
        h.queue.send(Command::Resolve { job_id: id, choice: JobChoice::Pick { mbid: "m1".into() } });
        let done = until(&h.rx, is_terminal);
        assert!(matches!(done.last().unwrap().state, JobState::Done { matched: true, .. }), "{:?}", done.last());
        let albums = h.lib.lock().unwrap().list_albums().unwrap();
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].mb_release_id.as_deref(), Some("m1"));
    }

    #[test]
    fn reidentify_lists_candidates_best_first() {
        let other = [("Uno", 1500), ("Dos", 2500)];
        let h = harness(fake(&["m1"], vec![candidate("m1", "Album", "Band", &TL)]));
        let x = import_matched(&h);
        let (q2, rx2) = start_queue(
            h.lib.clone(),
            fake(&["b", "m1"], vec![candidate("b", "Album", "Band", &other), candidate("m1", "Album", "Band", &TL)]),
        );
        q2.reimport(x);
        match until(&rx2, |s| matches!(s, JobState::NeedsChoice { .. }) || is_terminal(s)).last().unwrap().state.clone() {
            JobState::NeedsChoice { candidates, .. } => assert_eq!(candidates[0].mbid, "m1"),
            other => panic!("expected NeedsChoice, got {other:?}"),
        }
    }

    #[test]
    fn reidentify_appends_the_current_release_when_the_search_misses_it() {
        let h = harness(fake(&["m1"], vec![candidate("m1", "Album", "Band", &TL)]));
        let x = import_matched(&h);
        // The source still knows m1 but this search only finds m2.
        let (q2, rx2) = start_queue(
            h.lib.clone(),
            fake(&["m2"], vec![candidate("m2", "Album", "Band", &TL), candidate("m1", "Album", "Band", &TL)]),
        );
        q2.reimport(x);
        match until(&rx2, |s| matches!(s, JobState::NeedsChoice { .. }) || is_terminal(s)).last().unwrap().state.clone() {
            JobState::NeedsChoice { candidates, current } => {
                assert_eq!(candidates.iter().map(|c| c.mbid.as_str()).collect::<Vec<_>>(), ["m2", "m1"]);
                assert_eq!(current.as_deref(), Some("m1"));
            }
            other => panic!("expected NeedsChoice, got {other:?}"),
        }
    }

    #[test]
    fn reidentify_still_asks_when_the_current_release_cannot_be_fetched() {
        let h = harness(fake(&["m1"], vec![candidate("m1", "Album", "Band", &TL)]));
        let x = import_matched(&h);
        let (q2, rx2) = start_queue(h.lib.clone(), fake(&["m2"], vec![candidate("m2", "Album", "Band", &TL)]));
        q2.reimport(x);
        match until(&rx2, |s| matches!(s, JobState::NeedsChoice { .. }) || is_terminal(s)).last().unwrap().state.clone() {
            JobState::NeedsChoice { candidates, current } => {
                assert_eq!(candidates.len(), 1);
                assert_eq!(current.as_deref(), Some("m1"));
            }
            other => panic!("expected NeedsChoice, got {other:?}"),
        }
    }

    #[test]
    fn reidentify_keep_current_tags_leaves_the_album_alone() {
        let h = harness(fake(&["m1"], vec![candidate("m1", "Album", "Band", &TL)]));
        let x = import_matched(&h);
        let id = h.queue.reimport(x);
        until(&h.rx, |s| matches!(s, JobState::NeedsChoice { .. }));
        h.queue.send(Command::Resolve { job_id: id, choice: JobChoice::KeepExisting });
        assert!(matches!(until(&h.rx, is_terminal).last().unwrap().state, JobState::Skipped));
        let albums = h.lib.lock().unwrap().list_albums().unwrap();
        assert_eq!(albums.iter().map(|a| a.id).collect::<Vec<_>>(), [x]);
    }

    #[test]
    fn reidentify_with_nothing_found_is_skipped() {
        let h = harness(fake(&[], vec![]));
        let x = import_unmatched(&h);
        h.queue.reimport(x);
        assert!(matches!(until(&h.rx, is_terminal).last().unwrap().state, JobState::Skipped));
    }

    #[test]
    fn normal_import_has_no_current_release_and_a_confident_match_still_auto_applies() {
        let h = harness(fake(&["m1"], vec![candidate("m1", "Album", "Band", &TL)]));
        h.queue.send(Command::Import(vec![h.src.clone()]));
        let ups = until(&h.rx, is_terminal);
        assert!(matches!(ups.last().unwrap().state, JobState::Done { matched: true, .. }));
        assert!(!ups.iter().any(|u| matches!(u.state, JobState::NeedsChoice { .. })));
        let v = serde_json::to_value(JobState::NeedsChoice { candidates: vec![], current: None }).unwrap();
        assert!(v.get("current").is_none());
    }

    #[test]
    fn replace_cleanup_keeps_an_old_album_whose_references_could_not_be_moved() {
        let dir = tempfile::tempdir().unwrap();
        let mut lib = Library::open_in_memory(dir.path().join("lib")).unwrap();
        let (old, t) = crate::test_support::add_memory_album(&mut lib, "Alb", "Band", 2);
        let (new, _) = crate::test_support::add_memory_album(&mut lib, "Alb (fixed)", "Band", 2);
        let p = lib.create_playlist("Mix").unwrap();
        lib.add_to_playlist(p, &[t[0]]).unwrap();
        lib.exec_sql_for_test(
            "CREATE TEMP TRIGGER no_moves BEFORE UPDATE ON playlist_items BEGIN SELECT RAISE(ABORT, 'forced failure'); END;",
        );
        let err = replace_cleanup(&mut lib, new, &[old]).unwrap_err();
        assert!(format!("{err:#}").contains("forced failure"), "{err:#}");
        // Deleting it now would drop its playlist lines: the old album must stay.
        assert!(lib.album(old).unwrap().is_some());
        assert_eq!(lib.playlist_entries(p).unwrap()[0].song.track.id, t[0]);
    }

    use crate::test_support::{hiss, tone, write_pattern_wav, VinylFake};

    struct VinylHarness {
        dir: tempfile::TempDir,
        side: PathBuf,
        lib: Arc<Mutex<Library>>,
        queue: ImportQueue,
        rx: Receiver<JobUpdate>,
    }

    fn vinyl_harness(source: impl MetadataSource + 'static) -> VinylHarness {
        vinyl_harness_with(source, |dir| {
            let side = dir.join("Side A.wav");
            write_pattern_wav(&side, 8000, 1, 16, &[hiss(3.0), tone(30.0, 0.2), hiss(2.0), tone(40.0, 0.2), hiss(2.5), tone(20.0, 0.2), hiss(4.0)]);
            side
        })
    }

    /// `make_side` creates the recording in the harness's folder and returns its path.
    fn vinyl_harness_with(source: impl MetadataSource + 'static, make_side: impl FnOnce(&std::path::Path) -> PathBuf) -> VinylHarness {
        let dir = tempfile::tempdir().unwrap();
        let side = make_side(dir.path());
        let lib = Arc::new(Mutex::new(Library::open_in_memory(dir.path().join("lib")).unwrap()));
        let (tx, rx) = channel();
        let queue = ImportQueue::start_with_tmp(lib.clone(), Arc::new(source), dir.path().join("tmp"), move |u| {
            let _ = tx.send(u);
        });
        VinylHarness { dir, side, lib, queue, rx }
    }

    const SIDE_TL: [(&str, u64); 3] = [("One", 30_000), ("Two", 40_000), ("Three", 20_000)];

    /// Waits for the next vinyl review. `first` is true for a job's first review (which follows
    /// the analysis); later reviews (realign, search) must not re-analyse.
    fn review(rx: &Receiver<JobUpdate>, first: bool) -> (u64, crate::vinyl::VinylPlan) {
        let ups = until(rx, |s| matches!(s, JobState::NeedsVinylReview { .. }) || is_terminal(s));
        let last = ups.last().unwrap();
        assert_eq!(ups.iter().any(|u| matches!(u.state, JobState::Analysing)), first, "{ups:?}");
        match &last.state {
            JobState::NeedsVinylReview { plan } => (last.job_id, plan.clone()),
            other => panic!("expected a vinyl review, got {other:?}"),
        }
    }

    #[test]
    fn vinyl_match_is_reviewed_then_imported_as_flac() {
        let fake = VinylFake { releases: vec![candidate("m1", "Album", "Band", &SIDE_TL)], fingerprint_ids: vec!["m1".into()], offline: false, no_results_for: None };
        let h = vinyl_harness(fake);
        h.queue.send(Command::ImportVinyl(vec![h.side.clone()]));
        let (job_id, plan) = review(&h.rx, true);
        assert_eq!(plan.release.as_ref().map(|r| r.mbid.as_str()), Some("m1"));
        h.queue.send(Command::Resolve { job_id, choice: JobChoice::VinylConfirm { tracks: plan.tracks, matched: true } });
        let ups = until(&h.rx, is_terminal);
        assert!(matches!(ups.last().unwrap().state, JobState::Done { matched: true, .. }), "{:?}", ups.last());
        let lib = h.lib.lock().unwrap();
        let album = lib.list_albums().unwrap().remove(0);
        let tracks = lib.album_tracks(album.id).unwrap();
        assert_eq!(tracks.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), ["One", "Two", "Three"]);
        assert!(tracks.iter().all(|t| t.format == "flac"));
        assert!(!h.dir.path().join("tmp").join(format!("job-{job_id}")).exists(), "temporary FLACs removed");
    }

    #[test]
    fn vinyl_mp3_is_imported_as_mp3() {
        let release = candidate("m1", "Album", "Band", &[("One", 7_000), ("Two", 7_000), ("Three", 5_000)]);
        let fake = VinylFake { releases: vec![release], fingerprint_ids: vec!["m1".into()], offline: false, no_results_for: None };
        let h = vinyl_harness_with(fake, |dir| {
            let side = dir.join("Side A.mp3");
            std::fs::copy(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vinyl/cbr.mp3"), &side).unwrap();
            side
        });
        h.queue.send(Command::ImportVinyl(vec![h.side.clone()]));
        // The fixture's pieces are shorter than the fingerprinting minimum, so pick the release.
        let (job_id, _) = review(&h.rx, true);
        h.queue.send(Command::Resolve { job_id, choice: JobChoice::VinylRealign { mbid: "m1".into() } });
        let (_, plan) = review(&h.rx, false);
        assert_eq!(plan.release.as_ref().map(|r| r.mbid.as_str()), Some("m1"));
        h.queue.send(Command::Resolve { job_id, choice: JobChoice::VinylConfirm { tracks: plan.tracks, matched: true } });
        let ups = until(&h.rx, is_terminal);
        assert!(matches!(ups.last().unwrap().state, JobState::Done { matched: true, .. }), "{:?}", ups.last());
        let lib = h.lib.lock().unwrap();
        let album = lib.list_albums().unwrap().remove(0);
        let tracks = lib.album_tracks(album.id).unwrap();
        assert_eq!(tracks.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), ["One", "Two", "Three"]);
        assert!(tracks.iter().all(|t| t.format == "mp3"), "{:?}", tracks.iter().map(|t| &t.format).collect::<Vec<_>>());
        let read = crate::tags::read_source_track(&tracks[0].file_path).unwrap();
        assert_eq!(read.title.as_deref(), Some("One"), "tag read back from {:?}", tracks[0].file_path);
        assert!(!h.dir.path().join("tmp").join(format!("job-{job_id}")).exists(), "temporary files removed");
    }

    #[test]
    fn vinyl_without_a_match_imports_needing_metadata() {
        let h = vinyl_harness(VinylFake { releases: vec![], fingerprint_ids: vec![], offline: false, no_results_for: None });
        h.queue.send(Command::ImportVinyl(vec![h.side.clone()]));
        let (job_id, plan) = review(&h.rx, true);
        assert!(plan.release.is_none());
        assert_eq!(plan.note.as_deref(), Some("No match found online"));
        h.queue.send(Command::Resolve { job_id, choice: JobChoice::VinylConfirm { tracks: plan.tracks, matched: false } });
        assert!(matches!(until(&h.rx, is_terminal).last().unwrap().state, JobState::Done { matched: false, .. }));
        let album = h.lib.lock().unwrap().list_albums().unwrap().remove(0);
        assert!(album.needs_metadata);
    }

    #[test]
    fn vinyl_offline_still_offers_the_gaps() {
        let h = vinyl_harness(VinylFake { releases: vec![], fingerprint_ids: vec![], offline: true, no_results_for: None });
        h.queue.send(Command::ImportVinyl(vec![h.side.clone()]));
        let (_, plan) = review(&h.rx, true);
        assert_eq!(plan.tracks.len(), 3);
        assert!(plan.note.unwrap().contains("Couldn't reach"));
    }

    #[test]
    fn vinyl_change_release_replans() {
        let other = candidate("m2", "Album (Remaster)", "Band", &SIDE_TL);
        let fake = VinylFake { releases: vec![candidate("m1", "Album", "Band", &SIDE_TL), other], fingerprint_ids: vec!["m1".into()], offline: false, no_results_for: None };
        let h = vinyl_harness(fake);
        h.queue.send(Command::ImportVinyl(vec![h.side.clone()]));
        let (job_id, _) = review(&h.rx, true);
        h.queue.send(Command::Resolve { job_id, choice: JobChoice::VinylRealign { mbid: "m2".into() } });
        let (_, plan) = review(&h.rx, false);
        assert_eq!(plan.release.unwrap().mbid, "m2");
    }

    #[test]
    fn vinyl_realign_after_no_match_clears_the_stale_note() {
        let fake = VinylFake { releases: vec![candidate("m2", "Album", "Band", &SIDE_TL)], fingerprint_ids: vec![], offline: false, no_results_for: None };
        let h = vinyl_harness(fake);
        h.queue.send(Command::ImportVinyl(vec![h.side.clone()]));
        let (job_id, plan) = review(&h.rx, true);
        assert_eq!(plan.note.as_deref(), Some("No match found online"));
        h.queue.send(Command::Resolve { job_id, choice: JobChoice::VinylRealign { mbid: "m2".into() } });
        let (_, plan) = review(&h.rx, false);
        assert_eq!(plan.release.unwrap().mbid, "m2");
        assert_eq!(plan.note, None);
    }

    #[test]
    fn vinyl_empty_search_keeps_candidates_and_warns() {
        // A release twice the recording's length: the review carries its length warning.
        let long = candidate("m1", "Album", "Band", &[("One", 60_000), ("Two", 60_000), ("Three", 60_000)]);
        let other = candidate("m2", "Album (Remaster)", "Band", &SIDE_TL);
        let fake = VinylFake { releases: vec![long, other], fingerprint_ids: vec!["m1".into()], offline: false, no_results_for: Some("nothing") };
        let h = vinyl_harness(fake);
        h.queue.send(Command::ImportVinyl(vec![h.side.clone()]));
        let (job_id, first) = review(&h.rx, true);
        let fit = first.warning.clone().expect("length warning");
        assert!(fit.contains("different edition"), "{fit}");
        // An empty search keeps the candidates and adds its message without losing the length warning.
        h.queue.send(Command::Resolve { job_id, choice: JobChoice::Search { query: "nothing".into() } });
        let (_, plan) = review(&h.rx, false);
        assert_eq!(plan.candidates, first.candidates);
        let w = plan.warning.unwrap();
        assert!(w.contains("No releases found for that search.") && w.contains(&fit), "{w}");
        // A search that finds releases replaces the list and clears the message; the length warning stays.
        h.queue.send(Command::Resolve { job_id, choice: JobChoice::Search { query: "album".into() } });
        let (_, plan) = review(&h.rx, false);
        assert_eq!(plan.candidates.len(), 2);
        assert_eq!(plan.warning.as_deref(), Some(fit.as_str()));
    }

    #[test]
    fn vinyl_confirm_with_the_wrong_piece_count_returns_to_the_review() {
        let fake = VinylFake { releases: vec![candidate("m1", "Album", "Band", &SIDE_TL)], fingerprint_ids: vec!["m1".into()], offline: false, no_results_for: None };
        let h = vinyl_harness(fake);
        h.queue.send(Command::ImportVinyl(vec![h.side.clone()]));
        let (job_id, first) = review(&h.rx, true);
        let edited = first.tracks[..2].to_vec();
        h.queue.send(Command::Resolve { job_id, choice: JobChoice::VinylConfirm { tracks: edited.clone(), matched: true } });
        // The review comes back with the user's own cuts and the reason; nothing is lost.
        let (again, plan) = review(&h.rx, false);
        assert_eq!(again, job_id);
        assert_eq!(plan.tracks, edited);
        assert!(plan.warning.as_deref().is_some_and(|w| w.contains("pieces")), "{:?}", plan.warning);
        // ...and the job can still be confirmed.
        h.queue.send(Command::Resolve { job_id, choice: JobChoice::VinylConfirm { tracks: first.tracks, matched: true } });
        assert!(matches!(until(&h.rx, is_terminal).last().unwrap().state, JobState::Done { matched: true, .. }));
    }

    #[test]
    fn vinyl_sides_that_differ_fail() {
        let h = vinyl_harness(VinylFake { releases: vec![], fingerprint_ids: vec![], offline: false, no_results_for: None });
        let b = h.dir.path().join("Side B.wav");
        write_pattern_wav(&b, 16000, 1, 16, &[tone(5.0, 0.2)]);
        h.queue.send(Command::ImportVinyl(vec![h.side.clone(), b]));
        match &until(&h.rx, is_terminal).last().unwrap().state {
            JobState::Failed { error } => assert!(error.contains("don't match"), "{error}"),
            other => panic!("{other:?}"),
        }
    }
}
