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
use anyhow::Result;
use manifest::Manifest;
use plan::{Snapshot, SyncPlan};
use std::path::Path;
use std::sync::Mutex;

pub struct Prepared {
    pub plan: SyncPlan,
    pub manifest: Manifest,
    pub first_sync: bool,
}

/// Reads the library (holding its lock only briefly), backfills missing track hashes outside the
/// lock, reads the card's manifest and works out the sync plan.
pub fn prepare(lib: &Mutex<Library>, card: &Path) -> Result<Prepared> {
    let mut snap = {
        let l = lib.lock().unwrap();
        Snapshot::take(&l)?
    };
    let updates = plan::fill_missing_hashes(&mut snap);
    if !updates.is_empty() {
        let mut l = lib.lock().unwrap();
        for (id, size, hash) in &updates {
            l.set_track_file_state(*id, *size, hash)?;
        }
    }
    let (wanted, warnings) = plan::wanted_files(&snap);
    let existing = Manifest::load(card)?;
    let first_sync = existing.is_none();
    let manifest = existing.unwrap_or_default();
    Ok(Prepared { plan: plan::diff(wanted, &manifest, warnings), manifest, first_sync })
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
}
