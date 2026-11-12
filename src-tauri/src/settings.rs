use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub library_root: Option<PathBuf>,
    pub acoustid_key: Option<String>,
}

pub const FILE_NAME: &str = "settings.json";

/// Reads `<config_dir>/settings.json`. Missing → defaults. Unreadable or invalid → defaults plus a
/// description of the problem; the file itself is left alone until the user saves new settings.
pub fn load_checked(config_dir: &Path) -> (Settings, Option<String>) {
    let path = config_dir.join(FILE_NAME);
    match std::fs::read_to_string(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Settings::default(), None),
        Err(e) => (Settings::default(), Some(format!("Couldn't read {} ({e}); using default settings.", path.display()))),
        Ok(text) => match serde_json::from_str(&text) {
            Ok(s) => (s, None),
            Err(e) => (
                Settings::default(),
                Some(format!("{} is damaged ({e}); using default settings. Saving settings here will replace it.", path.display())),
            ),
        },
    }
}

pub fn load(config_dir: &Path) -> Settings {
    load_checked(config_dir).0
}

/// Writes the file via a temp file and rename.
pub fn save(config_dir: &Path, s: &Settings) -> Result<()> {
    std::fs::create_dir_all(config_dir).with_context(|| format!("couldn't create {}", config_dir.display()))?;
    let tmp = config_dir.join(format!("{FILE_NAME}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(s)?).with_context(|| format!("couldn't write {}", tmp.display()))?;
    std::fs::rename(&tmp, config_dir.join(FILE_NAME)).context("couldn't replace settings.json")?;
    Ok(())
}

/// "1 album", "3 albums".
pub fn albums_text(n: usize) -> String {
    format!("{n} {}", if n == 1 { "album" } else { "albums" })
}

/// `None` goes back to the default folder. Only allowed while the library is empty, and a chosen
/// folder must be empty too (the library owns everything under it).
pub fn with_library_root(saved: Settings, root: Option<PathBuf>, album_count: usize) -> Result<Settings> {
    if album_count > 0 {
        bail!(
            "Your library already has {}. Moving an existing library to another folder isn't supported yet.",
            albums_text(album_count)
        );
    }
    if let Some(r) = &root {
        if !r.is_dir() {
            bail!("{} isn't a folder.", r.display());
        }
        let has_entries = std::fs::read_dir(r)
            .with_context(|| format!("couldn't read {}", r.display()))?
            .next()
            .is_some();
        if has_entries {
            bail!("{} isn't empty. Choose an empty folder for the library.", r.display());
        }
    }
    Ok(Settings { library_root: root, ..saved })
}

/// What to start a save from: settings.json as read, or, when it is damaged, the settings in
/// effect now, so saving one setting doesn't silently drop the library folder in use.
pub fn save_base(saved: Settings, damaged: bool, in_effect: &Settings) -> Settings {
    if damaged {
        in_effect.clone()
    } else {
        saved
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RootChoice {
    /// The library folder to use.
    pub root: PathBuf,
    /// Whether it exists now (a custom folder on a disconnected drive doesn't).
    pub available: bool,
    /// The non-default folder in effect, as settings.json would record it.
    pub custom: Option<PathBuf>,
}

/// Picks the library folder at startup: settings.json's (if it names an absolute path, even when
/// that folder is missing right now), else the one recorded in the library database if it still
/// exists (covers a damaged or lost settings.json), else the default.
pub fn choose_library_root(
    settings_root: Option<&Path>,
    recorded: Option<&Path>,
    default_root: &Path,
    is_dir: impl Fn(&Path) -> bool,
) -> RootChoice {
    if let Some(r) = settings_root.filter(|r| r.is_absolute() || r.has_root()) {
        return RootChoice { root: r.to_path_buf(), available: is_dir(r), custom: Some(r.to_path_buf()) };
    }
    if let Some(r) = recorded.filter(|r| is_dir(r)) {
        let custom = (r != default_root).then(|| r.to_path_buf());
        return RootChoice { root: r.to_path_buf(), available: true, custom };
    }
    RootChoice { root: default_root.to_path_buf(), available: true, custom: None }
}

pub fn unavailable_message(root: &Path) -> String {
    format!("Your library folder {} isn't available (is the drive connected?)", root.display())
}

/// Blank clears the override (the built-in key, if any, is used again).
pub fn with_acoustid_key(saved: Settings, key: Option<String>) -> Settings {
    let key = key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty());
    Settings { acoustid_key: key, ..saved }
}

#[derive(Debug, Clone, Serialize)]
pub struct SettingsView {
    /// The library folder in use right now.
    pub library_root: String,
    pub default_library_root: String,
    /// What settings.json asks for (takes effect after a restart).
    pub saved_library_root: Option<String>,
    /// The override stored in settings.json.
    pub acoustid_key: Option<String>,
    /// Whether any AcoustID key (override, env or built in) is in use now.
    pub acoustid_key_active: bool,
    pub restart_needed: bool,
    pub problem: Option<String>,
    pub album_count: usize,
    /// Set when the library folder in use is missing (e.g. its drive isn't connected).
    pub library_unavailable: Option<String>,
}

pub fn view(
    saved: &Settings,
    startup: &Settings,
    active_root: &Path,
    default_root: &Path,
    key_active: bool,
    problem: Option<String>,
    album_count: usize,
    library_available: bool,
) -> SettingsView {
    SettingsView {
        library_root: active_root.display().to_string(),
        default_library_root: default_root.display().to_string(),
        saved_library_root: saved.library_root.as_ref().map(|p| p.display().to_string()),
        acoustid_key: saved.acoustid_key.clone(),
        acoustid_key_active: key_active,
        restart_needed: saved != startup,
        problem,
        album_count,
        library_unavailable: (!library_available).then(|| unavailable_message(active_root)),
    }
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
    fn damaged_file_gives_defaults_and_names_the_problem() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE_NAME), "{not json").unwrap();
        let (s, problem) = load_checked(dir.path());
        assert_eq!(s, Settings::default());
        let p = problem.unwrap();
        assert!(p.contains("settings.json"), "{p}");
        // nothing was rewritten
        assert_eq!(std::fs::read_to_string(dir.path().join(FILE_NAME)).unwrap(), "{not json");
    }

    #[test]
    fn missing_file_is_not_a_problem() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_checked(dir.path()), (Settings::default(), None));
    }

    #[test]
    fn save_round_trips_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("cfg");
        let s = Settings { library_root: Some(PathBuf::from("E:/Tunes")), acoustid_key: Some("k".into()) };
        save(&cfg, &s).unwrap();
        assert_eq!(load_checked(&cfg), (s, None));
        let names: Vec<_> = std::fs::read_dir(&cfg).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from(FILE_NAME)]);
    }

    #[test]
    fn library_folder_can_only_change_while_the_library_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let s = with_library_root(Settings::default(), Some(dir.path().to_path_buf()), 0).unwrap();
        assert_eq!(s.library_root.as_deref(), Some(dir.path()));
        let err = with_library_root(Settings::default(), Some(dir.path().to_path_buf()), 3).unwrap_err();
        assert!(err.to_string().contains("3 albums"), "{err}");
        let err = with_library_root(Settings::default(), Some(dir.path().to_path_buf()), 1).unwrap_err();
        assert!(err.to_string().contains("already has 1 album."), "{err}");
        let err = with_library_root(Settings::default(), Some(dir.path().join("missing")), 0).unwrap_err();
        assert!(err.to_string().contains("isn't a folder"), "{err}");
        assert_eq!(with_library_root(s, None, 0).unwrap().library_root, None);
    }

    #[test]
    fn acoustid_key_override_is_trimmed_and_blank_clears_it() {
        let s = with_acoustid_key(Settings::default(), Some("  abc ".into()));
        assert_eq!(s.acoustid_key.as_deref(), Some("abc"));
        assert_eq!(with_acoustid_key(s, Some("   ".into())).acoustid_key, None);
    }

    #[test]
    fn view_reports_what_is_in_use_and_whether_a_restart_is_needed() {
        let startup = Settings::default();
        let saved = Settings { library_root: Some(PathBuf::from("E:/Tunes")), acoustid_key: None };
        let v = view(&saved, &startup, Path::new("C:/Music/HeckTunes"), Path::new("C:/Music/HeckTunes"), true, None, 0, true);
        assert_eq!(v.library_root, "C:/Music/HeckTunes");
        assert_eq!(v.saved_library_root.as_deref(), Some("E:/Tunes"));
        assert!(v.restart_needed && v.acoustid_key_active);
        let same = view(&startup, &startup, Path::new("C:/Music/HeckTunes"), Path::new("C:/Music/HeckTunes"), false, None, 2, true);
        assert!(!same.restart_needed);
        assert_eq!(same.album_count, 2);
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

#[cfg(test)]
mod root_tests {
    use super::*;

    #[test]
    fn the_library_folder_must_be_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.mp3"), b"x").unwrap();
        let err = with_library_root(Settings::default(), Some(dir.path().to_path_buf()), 0).unwrap_err();
        assert!(err.to_string().contains("isn't empty"), "{err}");
    }

    fn exists_only(dirs: &'static [&'static str]) -> impl Fn(&Path) -> bool {
        move |p| dirs.iter().any(|d| Path::new(d) == p)
    }

    #[test]
    fn settings_root_wins_even_when_its_drive_is_missing() {
        let d = Path::new("C:/Music/HeckTunes");
        let c = choose_library_root(Some(Path::new("E:/Tunes")), Some(Path::new("F:/Old")), d, exists_only(&["E:/Tunes", "F:/Old"]));
        assert_eq!(c, RootChoice { root: "E:/Tunes".into(), available: true, custom: Some("E:/Tunes".into()) });
        let c = choose_library_root(Some(Path::new("E:/Tunes")), Some(Path::new("F:/Old")), d, exists_only(&["F:/Old"]));
        assert_eq!(c, RootChoice { root: "E:/Tunes".into(), available: false, custom: Some("E:/Tunes".into()) });
    }

    #[test]
    fn without_a_settings_root_the_recorded_root_is_used_if_it_exists() {
        let d = Path::new("C:/Music/HeckTunes");
        let c = choose_library_root(None, Some(Path::new("F:/Old")), d, exists_only(&["F:/Old"]));
        assert_eq!(c, RootChoice { root: "F:/Old".into(), available: true, custom: Some("F:/Old".into()) });
        let c = choose_library_root(None, Some(Path::new("F:/Old")), d, exists_only(&[]));
        assert_eq!(c, RootChoice { root: d.into(), available: true, custom: None });
        let c = choose_library_root(None, Some(d), d, exists_only(&["C:/Music/HeckTunes"]));
        assert_eq!(c, RootChoice { root: d.into(), available: true, custom: None });
        let c = choose_library_root(None, None, d, exists_only(&[]));
        assert_eq!(c, RootChoice { root: d.into(), available: true, custom: None });
    }

    #[test]
    fn a_relative_settings_root_is_ignored() {
        let d = Path::new("C:/Music/HeckTunes");
        let c = choose_library_root(Some(Path::new("Tunes")), None, d, exists_only(&["Tunes"]));
        assert_eq!(c.root, d);
    }

    #[test]
    fn saving_over_a_damaged_file_keeps_the_library_folder_in_use() {
        let in_effect = Settings { library_root: Some("E:/Tunes".into()), acoustid_key: None };
        let base = save_base(Settings::default(), true, &in_effect);
        assert_eq!(with_acoustid_key(base, Some("k".into())).library_root.as_deref(), Some(Path::new("E:/Tunes")));
        let fine = Settings { library_root: None, acoustid_key: Some("old".into()) };
        assert_eq!(save_base(fine.clone(), false, &in_effect), fine);
    }

    #[test]
    fn unavailable_folder_message_names_the_folder() {
        assert_eq!(
            unavailable_message(Path::new("E:/Tunes")),
            "Your library folder E:/Tunes isn't available (is the drive connected?)"
        );
        let v = view(&Settings::default(), &Settings::default(), Path::new("E:/Tunes"), Path::new("C:/M"), false, None, 0, false);
        assert_eq!(v.library_unavailable.as_deref(), Some("Your library folder E:/Tunes isn't available (is the drive connected?)"));
        let v = view(&Settings::default(), &Settings::default(), Path::new("E:/Tunes"), Path::new("C:/M"), false, None, 0, true);
        assert!(v.library_unavailable.is_none());
    }
}
