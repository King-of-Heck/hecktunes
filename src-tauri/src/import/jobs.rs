use crate::identify::{identify, rank, Identification, MetadataSource};
use crate::import::apply::{pick_cover, stage};
use crate::import::scan::{scan_paths, scan_paths_excluding, AlbumGroup};
use crate::library::{paths, Library};
use crate::model::ReleaseCandidate;
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
    Identifying,
    NeedsChoice { candidates: Vec<ReleaseCandidate> },
    Duplicate { release: ReleaseCandidate, existing_album_id: i64 },
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

fn offline_note(e: &anyhow::Error) -> String {
    format!("Couldn't reach the music database ({e}). Use Fix metadata when you're online.")
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
}

pub enum Command {
    Import(Vec<PathBuf>),
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
        let (tx, rx) = channel();
        let next_id = Arc::new(AtomicU64::new(1));
        // The root never changes after the library is opened; keep a copy so files can be staged unlocked.
        let root = lib.lock().unwrap().root().to_path_buf();
        let worker = Worker { lib, root, source, notify: Box::new(notify), next_id: next_id.clone(), pending: HashMap::new() };
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

struct Job {
    id: u64,
    label: String,
    group: AlbumGroup,
    /// Set when re-identifying an album already in the library.
    replace_album_id: Option<i64>,
    candidates: Vec<ReleaseCandidate>,
    /// Release waiting on a Duplicate decision.
    chosen: Option<ReleaseCandidate>,
}

struct Worker {
    lib: Arc<Mutex<Library>>,
    root: PathBuf,
    source: Arc<dyn MetadataSource>,
    notify: Box<dyn Fn(JobUpdate) + Send>,
    next_id: Arc<AtomicU64>,
    pending: HashMap<u64, Job>,
}

impl Worker {
    fn run(mut self, rx: Receiver<Command>) {
        while let Ok(cmd) = rx.recv() {
            match cmd {
                Command::Import(paths) => self.import(paths),
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
                Job { id, label, group, replace_album_id: None, candidates: vec![], chosen: None }
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
        self.identify_job(Job { id, label, group, replace_album_id: Some(album_id), candidates: vec![], chosen: None });
    }

    fn identify_job(&mut self, mut job: Job) {
        self.emit(job.id, &job.label, JobState::Identifying);
        match identify(&job.group.tracks, self.source.as_ref()) {
            Ok(Identification::Matched(release)) => self.proceed(job, release),
            Ok(Identification::Ambiguous(candidates)) => {
                job.candidates = candidates.clone();
                self.emit(job.id, &job.label, JobState::NeedsChoice { candidates });
                self.pending.insert(job.id, job);
            }
            // A user-requested re-identify must say why it failed rather than look like "nothing found".
            Err(e) if job.replace_album_id.is_some() => {
                self.emit(job.id, &job.label, JobState::Failed { error: format!("{e:#}") })
            }
            // Network errors on a normal import: import with what we have and flag it.
            Err(e) => {
                let note = offline_note(&e);
                self.finish_unmatched(job, true, Some(note))
            }
            Ok(Identification::NotFound) => self.finish_unmatched(job, true, Some(NOTE_NOT_FOUND.into())),
        }
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
        match result {
            Ok(album_id) => self.emit(job.id, &job.label, JobState::Done { album_id, matched: false, note }),
            Err(e) => self.emit(job.id, &job.label, JobState::Failed { error: format!("{e:#}") }),
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
        match result {
            Ok(album_id) => self.emit(job.id, &job.label, JobState::Done { album_id, matched: release.is_some(), note: None }),
            Err(e) => self.emit(job.id, &job.label, JobState::Failed { error: format!("{e:#}") }),
        }
    }

    fn resolve(&mut self, job_id: u64, choice: JobChoice) {
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
                self.emit(job.id, &job.label, JobState::NeedsChoice { candidates: job.candidates.clone() });
                self.pending.insert(job.id, job);
            }
        }
    }
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
            JobState::NeedsChoice { candidates } => assert_eq!(candidates.len(), 2),
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
    }

    impl MetadataSource for FlakySource {
        fn search(&self, album: &str, artist: &str, n: usize) -> anyhow::Result<Vec<String>> {
            if self.fail_search {
                anyhow::bail!("network down");
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
            self.inner.by_fingerprint(t)
        }
        fn release(&self, mbid: &str) -> anyhow::Result<ReleaseCandidate> {
            self.inner.release(mbid)
        }
        fn cover(&self, mbid: &str) -> anyhow::Result<Option<Vec<u8>>> {
            self.inner.cover(mbid)
        }
    }

    impl FlakySource {
        /// Every lookup fails, as when the machine is offline.
        fn offline() -> Self {
            FlakySource { inner: fake(&[], vec![]), fail_search: true, fail_search_text: true }
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
        let (q2, rx2) = start_queue(h.lib.clone(), FlakySource::offline());
        q2.send(Command::Import(vec![h.src.clone()]));
        match until(&rx2, is_terminal).last().unwrap().state.clone() {
            JobState::Done { matched: false, note: Some(n), .. } => assert!(n.starts_with("Couldn't reach the music database"), "{n}"),
            other => panic!("{other:?}"),
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
            FlakySource { inner: fake(&[], vec![]), fail_search: true, fail_search_text: false },
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
            FlakySource { inner: fake(&[], vec![]), fail_search: true, fail_search_text: false },
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
            FlakySource { inner: fake(&["a", "b"], releases), fail_search: false, fail_search_text: true },
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
        let ups = until(&h.rx, is_terminal);
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
}
