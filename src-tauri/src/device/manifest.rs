use super::cardpath::TEMP_SUFFIX;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const MANIFEST_DIR: &str = ".hecktunes";
pub const MANIFEST_FILE: &str = "manifest.json";
/// The only manifest layout this HeckTunes reads and writes.
pub const MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileEntry {
    pub size: u64,
    pub hash: String,
}

/// The files HeckTunes put on a card. Only these are ever replaced or deleted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub card_id: String,
    pub synced_at: Option<i64>,
    pub files: BTreeMap<String, FileEntry>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self::new()
    }
}

impl Manifest {
    pub fn new() -> Self {
        Self { version: MANIFEST_VERSION, card_id: new_card_id(), synced_at: None, files: BTreeMap::new() }
    }

    pub fn path(card: &Path) -> PathBuf {
        card.join(MANIFEST_DIR).join(MANIFEST_FILE)
    }

    /// `Ok(None)` when the card has never been synced by HeckTunes.
    pub fn load(card: &Path) -> Result<Option<Self>> {
        let path = Self::path(card);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(anyhow!("couldn't read {}: {e}", path.display())),
        };
        let m: Self = serde_json::from_str(&text).map_err(|e| {
            anyhow!(
                "{} is damaged ({e}). HeckTunes won't change this card until that file is fixed or deleted.",
                path.display()
            )
        })?;
        if m.version != MANIFEST_VERSION {
            return Err(anyhow!(
                "{} is manifest version {}, which this HeckTunes doesn't understand. HeckTunes won't change this card until that file is fixed or deleted.",
                path.display(),
                m.version
            ));
        }
        Ok(Some(m))
    }

    /// Writes a temp file and renames it, so an unplugged card keeps the previous manifest.
    pub fn save(&self, card: &Path) -> Result<()> {
        let path = Self::path(card);
        let dir = card.join(MANIFEST_DIR);
        std::fs::create_dir_all(&dir).with_context(|| format!("couldn't create {}", dir.display()))?;
        let tmp = dir.join(format!("{MANIFEST_FILE}{TEMP_SUFFIX}"));
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?).with_context(|| format!("couldn't write {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("couldn't replace {}", path.display()))?;
        Ok(())
    }
}

fn new_card_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seed = format!(
        "{:?}-{}-{}",
        std::time::SystemTime::now(),
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    blake3::hash(seed.as_bytes()).to_hex()[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_then_load_round_trips_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = Manifest::new();
        m.files.insert("Music/a.flac".into(), FileEntry { size: 3, hash: "h".into() });
        m.synced_at = Some(5);
        m.save(dir.path()).unwrap();
        assert_eq!(Manifest::load(dir.path()).unwrap(), Some(m));
        let names: Vec<_> = std::fs::read_dir(dir.path().join(MANIFEST_DIR)).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from(MANIFEST_FILE)]);
    }

    #[test]
    fn missing_manifest_means_a_new_card() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Manifest::load(dir.path()).unwrap(), None);
    }

    #[test]
    fn damaged_manifest_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(MANIFEST_DIR)).unwrap();
        std::fs::write(Manifest::path(dir.path()), "{nope").unwrap();
        assert!(Manifest::load(dir.path()).unwrap_err().to_string().contains("damaged"));
    }

    #[test]
    fn card_ids_are_distinct_16_hex_chars() {
        let (a, b) = (Manifest::new().card_id, Manifest::new().card_id);
        assert_ne!(a, b);
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn a_manifest_from_another_version_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = Manifest::new();
        m.version = 2;
        m.save(dir.path()).unwrap();
        let err = Manifest::load(dir.path()).unwrap_err().to_string();
        assert!(err.contains("version 2"), "{err}");
        assert!(err.contains("won't change this card"), "{err}");
    }
}
