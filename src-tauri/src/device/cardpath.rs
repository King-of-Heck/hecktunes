use crate::library::paths::sanitize_component;
use std::path::{Component, Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

pub const MUSIC_DIR: &str = "Music";
pub const PLAYLISTS_DIR: &str = "Playlists";
/// Card files are written as `<name>.hecktmp` and then renamed into place.
pub const TEMP_SUFFIX: &str = ".hecktmp";
/// How a playlist in /Playlists points at music in /Music. echo-sync v0.11.1 resolves an entry
/// against the playlist's folder first, then the card root.
pub const PLAYLIST_ENTRY_PREFIX: &str = "../";

fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// "Music/<library-relative path>" for a file inside the library root; `None` otherwise.
pub fn track_card_path(root: &Path, file: &Path) -> Option<String> {
    let rel = file.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => parts.push(nfc(&s.to_string_lossy())),
            _ => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!("{MUSIC_DIR}/{}", parts.join("/")))
}

pub fn playlist_card_path(name: &str) -> String {
    format!("{PLAYLISTS_DIR}/{}.m3u8", nfc(&sanitize_component(name)))
}

/// Absolute location of a card path under the card root.
pub fn on_card(card: &Path, card_path: &str) -> PathBuf {
    card_path.split('/').fold(card.to_path_buf(), |p, part| p.join(part))
}

/// FAT32/exFAT compare names without case: two card paths clash when their keys are equal.
pub fn fold_key(card_path: &str) -> String {
    card_path.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_files_map_under_music_with_forward_slashes() {
        let root = Path::new("/lib");
        let file = root.join("Band").join("Alb (1997)").join("01 Song.flac");
        assert_eq!(track_card_path(root, &file).as_deref(), Some("Music/Band/Alb (1997)/01 Song.flac"));
        assert_eq!(track_card_path(root, Path::new("/elsewhere/x.flac")), None);
        assert_eq!(track_card_path(root, root), None);
    }

    #[test]
    fn card_paths_are_nfc() {
        let root = Path::new("/lib");
        let file = root.join("Beyonce\u{301}").join("x.flac");
        assert_eq!(track_card_path(root, &file).as_deref(), Some("Music/Beyonc\u{e9}/x.flac"));
    }

    #[test]
    fn playlist_names_are_sanitized() {
        assert_eq!(playlist_card_path("Road: Trip?"), "Playlists/Road_ Trip_.m3u8");
        assert_eq!(playlist_card_path("Mix"), "Playlists/Mix.m3u8");
    }

    #[test]
    fn card_paths_resolve_on_the_card_and_fold_for_comparison() {
        let card = Path::new("/card");
        assert_eq!(on_card(card, "Music/a/b.flac"), card.join("Music").join("a").join("b.flac"));
        assert_eq!(fold_key("Music/Band/A.FLAC"), fold_key("music/band/a.flac"));
    }
}
