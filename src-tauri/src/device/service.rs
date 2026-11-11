use super::echosync::{self, EchoSyncOutcome};
use super::plan::PlanSummary;
use super::sync::{execute, Outcome, Progress};
use super::{detect, prepare};
use crate::library::Library;
use anyhow::{bail, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SyncEvent {
    Preparing,
    Progress(Progress),
    /// Copying is done; echo-sync is building the player's library on the card.
    EchoSync,
    Finished { outcome: Outcome, echo_sync: Option<EchoSyncOutcome>, warnings: Vec<String> },
    Failed { error: String },
}

/// Runs one sync at a time on its own thread.
pub struct SyncService {
    lib: Arc<Mutex<Library>>,
    echo_sync: Option<PathBuf>,
    running: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
}

impl SyncService {
    pub fn new(lib: Arc<Mutex<Library>>, echo_sync: Option<PathBuf>) -> Self {
        Self { lib, echo_sync, running: Arc::new(AtomicBool::new(false)), cancel: Arc::new(AtomicBool::new(false)) }
    }

    pub fn preview(&self, card: &Path) -> Result<PlanSummary> {
        let p = prepare(&self.lib, card)?;
        Ok(p.plan.summary(detect::available_space(card), p.first_sync))
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Asks a running sync to stop after the current file.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn start(&self, card: PathBuf, notify: impl Fn(SyncEvent) + Send + 'static) -> Result<()> {
        if self.running.swap(true, Ordering::SeqCst) {
            bail!("A sync is already running");
        }
        self.cancel.store(false, Ordering::SeqCst);
        let (lib, echo, running, cancel) = (self.lib.clone(), self.echo_sync.clone(), self.running.clone(), self.cancel.clone());
        std::thread::spawn(move || {
            notify(SyncEvent::Preparing);
            let last = match run(&lib, &card, echo.as_deref(), &cancel, &notify) {
                Ok(event) => event,
                Err(e) => SyncEvent::Failed { error: format!("{e:#}") },
            };
            running.store(false, Ordering::SeqCst);
            notify(last);
        });
        Ok(())
    }
}

fn mb(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / 1_048_576.0)
}

fn run(lib: &Mutex<Library>, card: &Path, echo: Option<&Path>, cancel: &AtomicBool, notify: &dyn Fn(SyncEvent)) -> Result<SyncEvent> {
    let mut p = prepare(lib, card)?;
    if let Some(available) = detect::available_space(card) {
        if !p.plan.fits(available) {
            bail!("Not enough space on the card: this sync needs {} but only {} is free.", mb(p.plan.space_needed()), mb(available));
        }
    }
    let outcome = execute(card, &p.plan, &mut p.manifest, cancel, &mut |pr| notify(SyncEvent::Progress(pr.clone())))?;
    let echo_sync = if outcome.cancelled {
        None
    } else {
        notify(SyncEvent::EchoSync);
        Some(match echo {
            Some(exe) => echosync::run(exe, card),
            None => EchoSyncOutcome::Missing { reason: "echo-sync isn't installed next to HeckTunes".into() },
        })
    };
    Ok(SyncEvent::Finished { outcome, echo_sync, warnings: p.plan.warnings })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::sync::Phase;
    use crate::library::SelectionKind;
    use crate::test_support::add_album;
    use std::time::Duration;

    #[test]
    fn sync_runs_in_the_background_and_reports_its_finish() {
        let dir = tempfile::tempdir().unwrap();
        let card = dir.path().join("card");
        std::fs::create_dir(&card).unwrap();
        let lib = Arc::new(Mutex::new(Library::open_in_memory(dir.path().join("lib")).unwrap()));
        {
            let mut l = lib.lock().unwrap();
            let a = add_album(&mut l, dir.path(), "Alb", "Band", 2, false);
            l.set_selected(SelectionKind::Album, &a.to_string(), true).unwrap();
        }
        let svc = SyncService::new(lib.clone(), None);
        let preview = svc.preview(&card).unwrap();
        assert!(preview.first_sync);
        assert_eq!(preview.add, 2);

        let (tx, rx) = std::sync::mpsc::channel();
        svc.start(card.clone(), move |e| {
            let _ = tx.send(e);
        })
        .unwrap();
        let mut events = Vec::new();
        loop {
            let e = rx.recv_timeout(Duration::from_secs(30)).expect("sync event");
            let last = matches!(e, SyncEvent::Finished { .. } | SyncEvent::Failed { .. });
            events.push(e);
            if last {
                break;
            }
        }
        assert!(matches!(events[0], SyncEvent::Preparing));
        assert!(events.iter().any(|e| matches!(e, SyncEvent::Progress(p) if p.phase == Phase::Copying)));
        assert!(events.iter().any(|e| matches!(e, SyncEvent::EchoSync)));
        match events.last().unwrap() {
            SyncEvent::Finished { outcome, echo_sync: Some(EchoSyncOutcome::Missing { .. }), .. } => assert_eq!(outcome.copied, 2),
            other => panic!("expected Finished without echo-sync, got {other:?}"),
        }
        assert!(!svc.is_running());

        let after = svc.preview(&card).unwrap();
        assert!(!after.first_sync);
        assert_eq!((after.add, after.replace, after.remove, after.unchanged), (0, 0, 0, 2));
    }

    #[test]
    fn events_serialize_flat_with_a_state_tag() {
        let p = Progress { phase: Phase::Copying, done: 1, total: 3, bytes_done: 10, bytes_total: 30, current: "Music/x.flac".into() };
        let v = serde_json::to_value(SyncEvent::Progress(p)).unwrap();
        assert_eq!(v["state"], "progress");
        assert_eq!(v["phase"], "copying");
        assert_eq!(v["current"], "Music/x.flac");
        assert_eq!(serde_json::to_value(SyncEvent::EchoSync).unwrap()["state"], "echo_sync");
        assert_eq!(serde_json::to_value(SyncEvent::Failed { error: "e".into() }).unwrap()["error"], "e");
    }

    #[test]
    fn a_vanished_card_ends_in_a_failed_event() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Arc::new(Mutex::new(Library::open_in_memory(dir.path().join("lib")).unwrap()));
        let svc = SyncService::new(lib, None);
        let (tx, rx) = std::sync::mpsc::channel();
        svc.start(dir.path().join("gone"), move |e| {
            let _ = tx.send(e);
        })
        .unwrap();
        let last = std::iter::from_fn(|| rx.recv_timeout(Duration::from_secs(30)).ok())
            .find(|e| matches!(e, SyncEvent::Finished { .. } | SyncEvent::Failed { .. }))
            .unwrap();
        assert!(matches!(last, SyncEvent::Failed { .. }), "{last:?}");
    }
}
