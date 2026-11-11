use super::manifest::Manifest;
use serde::Serialize;
use std::path::{Path, PathBuf};
use sysinfo::Disks;

/// Volume label of the Echo Mini's own (internal) storage when connected over USB.
pub const PLAYER_INTERNAL_LABEL: &str = "ECHO MINI";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Volume {
    pub root: PathBuf,
    pub label: String,
    pub file_system: String,
    pub total: u64,
    pub available: u64,
    pub removable: bool,
    /// The player's internal storage: offered but not selectable (music belongs on the card).
    pub player_internal: bool,
    /// Has a HeckTunes manifest, so it was synced before.
    pub hecktunes_card: bool,
    pub last_synced: Option<i64>,
}

pub fn describe(root: PathBuf, label: String, file_system: String, total: u64, available: u64, removable: bool) -> Volume {
    let manifest = Manifest::load(&root).ok().flatten();
    Volume {
        player_internal: label.eq_ignore_ascii_case(PLAYER_INTERNAL_LABEL),
        hecktunes_card: manifest.is_some(),
        last_synced: manifest.and_then(|m| m.synced_at),
        root,
        label,
        file_system,
        total,
        available,
        removable,
    }
}

/// Could a memory card be mounted here? Windows: any drive except the system drive.
/// macOS/Linux: external mount locations only.
pub fn is_candidate(root: &Path) -> bool {
    #[cfg(windows)]
    {
        let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()).to_ascii_uppercase();
        !root.to_string_lossy().to_ascii_uppercase().starts_with(&system)
    }
    #[cfg(not(windows))]
    {
        let r = root.to_string_lossy();
        ["/Volumes/", "/media/", "/run/media/", "/mnt/"].iter().any(|p| r.starts_with(p))
    }
}

/// Drives the user might pick as the Echo Mini card, sorted by mount point.
pub fn list_volumes() -> Vec<Volume> {
    let Ok(disks) = Disks::new_with_refreshed_list() else { return Vec::new() };
    let mut out: Vec<Volume> = disks
        .iter()
        .filter(|d| is_candidate(d.mount_point()))
        .map(|d| {
            describe(
                d.mount_point().to_path_buf(),
                d.name().to_string_lossy().into_owned(),
                d.file_system().to_string_lossy().into_owned(),
                d.total_space(),
                d.available_space(),
                d.is_removable(),
            )
        })
        .collect();
    out.sort_by(|a, b| a.root.cmp(&b.root));
    out.dedup_by(|a, b| a.root == b.root);
    out
}

/// Free bytes on the volume holding `card` (the longest matching mount point).
pub fn available_space(card: &Path) -> Option<u64> {
    let disks = Disks::new_with_refreshed_list().ok()?;
    disks
        .iter()
        .filter(|d| card.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| d.available_space())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::manifest::Manifest;

    #[test]
    fn describe_spots_hecktunes_cards_and_the_players_own_storage() {
        let dir = tempfile::tempdir().unwrap();
        let plain = describe(dir.path().to_path_buf(), "SDCARD".into(), "FAT32".into(), 100, 40, true);
        assert!(!plain.hecktunes_card && !plain.player_internal && plain.last_synced.is_none());

        let mut m = Manifest::new();
        m.synced_at = Some(1234);
        m.save(dir.path()).unwrap();
        let card = describe(dir.path().to_path_buf(), "echo mini".into(), "exFAT".into(), 100, 40, true);
        assert!(card.hecktunes_card && card.player_internal);
        assert_eq!(card.last_synced, Some(1234));
    }

    #[cfg(windows)]
    #[test]
    fn the_windows_system_drive_is_never_offered() {
        let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        assert!(!is_candidate(Path::new(&format!("{system}\\"))));
        assert!(list_volumes().iter().all(|v| !v.root.to_string_lossy().to_ascii_uppercase().starts_with(&system.to_ascii_uppercase())));
    }

    #[cfg(not(windows))]
    #[test]
    fn only_external_mounts_are_offered() {
        assert!(is_candidate(Path::new("/Volumes/SDCARD")));
        assert!(is_candidate(Path::new("/media/me/SDCARD")));
        assert!(!is_candidate(Path::new("/")));
        assert!(!is_candidate(Path::new("/System/Volumes/Data")));
    }

    #[test]
    fn free_space_is_known_for_a_folder_on_a_real_disk() {
        let dir = tempfile::tempdir().unwrap();
        assert!(available_space(dir.path()).is_some());
    }
}
