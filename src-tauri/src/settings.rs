use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    pub library_root: Option<PathBuf>,
    pub acoustid_key: Option<String>,
}

/// Reads `<config_dir>/settings.json`; a missing or invalid file means defaults.
pub fn load(config_dir: &Path) -> Settings {
    std::fs::read_to_string(config_dir.join("settings.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_gives_defaults_and_valid_file_parses() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).library_root.is_none());
        std::fs::write(dir.path().join("settings.json"), r#"{"library_root":"D:/Music","acoustid_key":"abc"}"#).unwrap();
        let s = load(dir.path());
        assert_eq!(s.acoustid_key.as_deref(), Some("abc"));
        assert_eq!(s.library_root, Some(PathBuf::from("D:/Music")));
    }
}
