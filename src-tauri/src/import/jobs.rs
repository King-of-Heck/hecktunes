use crate::identify::{identify, rank, Identification, MetadataSource};
use crate::import::apply::{apply, pick_cover};
use crate::import::scan::{scan_paths, AlbumGroup};
use crate::library::{paths, Library};
use crate::model::ReleaseCandidate;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
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
    Done { album_id: i64, matched: bool },
    Skipped,
    Failed { error: String },
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
    Reimport { album_id: i64 },
    Resolve { job_id: u64, choice: JobChoice },
}

pub struct ImportQueue {
    tx: Sender<Command>,
}

impl ImportQueue {
    pub fn start(
        lib: Arc<Mutex<Library>>,
        source: Arc<dyn MetadataSource>,
        notify: impl Fn(JobUpdate) + Send + 'static,
    ) -> Self {
        let (tx, rx) = channel();
        let worker = Worker { lib, source, notify: Box::new(notify), next_id: 1, pending: HashMap::new() };
        thread::spawn(move || worker.run(rx));
        Self { tx }
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
    source: Arc<dyn MetadataSource>,
    notify: Box<dyn Fn(JobUpdate) + Send>,
    next_id: u64,
    pending: HashMap<u64, Job>,
}

impl Worker {
    fn run(mut self, rx: Receiver<Command>) {
        while let Ok(cmd) = rx.recv() {
            match cmd {
                Command::Import(paths) => self.import(paths),
                Command::Reimport { album_id } => self.reimport(album_id),
                Command::Resolve { job_id, choice } => self.resolve(job_id, choice),
            }
        }
    }

    fn emit(&self, id: u64, label: &str, state: JobState) {
        (self.notify)(JobUpdate { job_id: id, label: label.to_string(), state });
    }

    fn new_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn import(&mut self, paths: Vec<PathBuf>) {
        let scan = scan_paths(&paths);
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

    fn reimport(&mut self, album_id: i64) {
        let album = match self.lib.lock().unwrap().album(album_id) {
            Ok(Some(a)) => a,
            _ => return,
        };
        let id = self.new_id();
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
            // Network errors land here too: import with what we have and flag it.
            Ok(Identification::NotFound) | Err(_) => self.finish_unmatched(job, true),
        }
    }

    fn proceed(&mut self, mut job: Job, release: ReleaseCandidate) {
        if job.replace_album_id.is_none() {
            let existing = self.lib.lock().unwrap().find_album_by_release(&release.mbid).ok().flatten();
            if let Some(existing_album_id) = existing {
                job.chosen = Some(release.clone());
                self.emit(job.id, &job.label, JobState::Duplicate { release, existing_album_id });
                self.pending.insert(job.id, job);
                return;
            }
        }
        let replace = job.replace_album_id;
        self.finalize(job, Some(release), replace);
    }

    fn finish_unmatched(&mut self, job: Job, needs_metadata: bool) {
        if job.replace_album_id.is_some() {
            // Re-identify found nothing new: leave the album as it is.
            self.emit(job.id, &job.label, JobState::Skipped);
            return;
        }
        self.emit(job.id, &job.label, JobState::Importing);
        let cover = pick_cover(&job.group, None);
        let result = apply(&mut self.lib.lock().unwrap(), &job.group, None, cover, needs_metadata);
        match result {
            Ok(album_id) => self.emit(job.id, &job.label, JobState::Done { album_id, matched: false }),
            Err(e) => self.emit(job.id, &job.label, JobState::Failed { error: format!("{e:#}") }),
        }
    }

    fn finalize(&mut self, job: Job, release: Option<ReleaseCandidate>, replace: Option<i64>) {
        self.emit(job.id, &job.label, JobState::Importing);
        let online = release.as_ref().and_then(|r| self.source.cover(&r.mbid).ok().flatten());
        let cover = pick_cover(&job.group, online);
        let mut lib = self.lib.lock().unwrap();
        let result = apply(&mut lib, &job.group, release.as_ref(), cover, false).and_then(|album_id| {
            if let Some(old) = replace {
                lib.delete_album(old, true)?;
                // The new copy may have landed in "<Album> [2]" because the old folder existed; tidy it.
                if let Some(row) = lib.album(album_id)? {
                    let wanted = paths::album_dir(&row.album_artist, &row.title, row.year.as_deref());
                    if row.folder_path != lib.root().join(&wanted) && !lib.root().join(&wanted).exists() {
                        lib.move_album_folder(album_id, &wanted)?;
                    }
                }
            }
            Ok(album_id)
        });
        drop(lib);
        match result {
            Ok(album_id) => self.emit(job.id, &job.label, JobState::Done { album_id, matched: release.is_some() }),
            Err(e) => self.emit(job.id, &job.label, JobState::Failed { error: format!("{e:#}") }),
        }
    }

    fn resolve(&mut self, job_id: u64, choice: JobChoice) {
        let Some(mut job) = self.pending.remove(&job_id) else { return };
        match choice {
            JobChoice::Skip => self.emit(job.id, &job.label, JobState::Skipped),
            JobChoice::KeepExisting => self.finish_unmatched(job, false),
            JobChoice::Replace => {
                let release = job.chosen.take();
                let existing = release.as_ref().and_then(|r| self.lib.lock().unwrap().find_album_by_release(&r.mbid).ok().flatten());
                self.finalize(job, release, existing);
            }
            JobChoice::Pick { mbid } => {
                let found = job.candidates.iter().find(|c| c.mbid == mbid).cloned();
                match found.map(Ok).unwrap_or_else(|| self.source.release(&mbid)) {
                    Ok(release) => self.proceed(job, release),
                    Err(e) => self.emit(job.id, &job.label, JobState::Failed { error: format!("{e:#}") }),
                }
            }
            JobChoice::Search { query } => {
                let ids = self.source.search_text(&query).unwrap_or_default();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Library;
    use crate::test_support::{candidate, tagged_wav, FakeSource};
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
        q2.send(Command::Reimport { album_id: album.id });
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

    #[test]
    fn job_update_serializes_flat_for_the_ui() {
        let u = JobUpdate { job_id: 3, label: "L".into(), state: JobState::Done { album_id: 7, matched: true } };
        let v = serde_json::to_value(&u).unwrap();
        assert_eq!(v, serde_json::json!({"job_id": 3, "label": "L", "state": "done", "album_id": 7, "matched": true}));
        let c: JobChoice = serde_json::from_value(serde_json::json!({"kind": "pick", "mbid": "x"})).unwrap();
        assert!(matches!(c, JobChoice::Pick { mbid } if mbid == "x"));
    }
}
