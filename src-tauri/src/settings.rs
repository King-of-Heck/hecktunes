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

/// Picks the AcoustID key: settings.json, then runtime env var, then the
/// compile-time baked key. Blank/whitespace values count as missing; the key is returned trimmed.
pub fn resolve_acoustid_key(
    settings: Option<String>,
    runtime: Option<String>,
    baked: Option<&str>,
) -> Option<String> {
    [settings.as_deref(), runtime.as_deref(), baked]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|k| !k.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acoustid_key_precedence_is_settings_then_runtime_then_baked() {
        let s = |v: &str| Some(v.to_string());
        assert_eq!(resolve_acoustid_key(s("a"), s("b"), Some("c")), s("a"));
        assert_eq!(resolve_acoustid_key(None, s("b"), Some("c")), s("b"));
        assert_eq!(resolve_acoustid_key(None, None, Some("c")), s("c"));
        assert_eq!(resolve_acoustid_key(None, None, None), None);
    }

    #[test]
    fn acoustid_key_blank_values_count_as_missing() {
        let s = |v: &str| Some(v.to_string());
        assert_eq!(resolve_acoustid_key(s(""), s("b"), Some("c")), s("b"));
        assert_eq!(resolve_acoustid_key(s("  "), s(" \t"), Some("c")), s("c"));
        assert_eq!(resolve_acoustid_key(None, s(""), Some("")), None);
        assert_eq!(resolve_acoustid_key(s("\n"), None, Some("  ")), None);
    }

    #[test]
    fn acoustid_key_is_returned_trimmed() {
        let s = |v: &str| Some(v.to_string());
        assert_eq!(resolve_acoustid_key(s("  abc \n"), s("b"), Some("c")), s("abc"));
        assert_eq!(resolve_acoustid_key(None, s("\tdef "), Some("c")), s("def"));
        assert_eq!(resolve_acoustid_key(None, None, Some(" ghi\r\n")), s("ghi"));
    }

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
