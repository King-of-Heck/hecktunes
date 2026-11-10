use std::path::PathBuf;

const MAX_COMPONENT_CHARS: usize = 120;
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

fn trim_name(s: &str) -> &str {
    s.trim().trim_end_matches(|c| c == '.' || c == ' ')
}

pub fn sanitize_component(s: &str) -> String {
    let replaced: String = s
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let capped: String = trim_name(&replaced).chars().take(MAX_COMPONENT_CHARS).collect();
    let name = trim_name(&capped).to_string();
    if name.is_empty() {
        "_".to_string()
    } else if RESERVED.contains(&name.to_ascii_uppercase().as_str()) {
        format!("_{name}")
    } else {
        name
    }
}

pub fn album_dir(album_artist: &str, album: &str, year: Option<&str>) -> PathBuf {
    let album_name = match year {
        Some(y) => format!("{album} ({y})"),
        None => album.to_string(),
    };
    PathBuf::from(sanitize_component(album_artist)).join(sanitize_component(&album_name))
}

pub fn track_file_name(disc_no: u32, disc_total: u32, track_no: u32, title: &str, ext: &str) -> String {
    let prefix = if disc_total > 1 {
        format!("{disc_no}-{track_no:02}")
    } else {
        format!("{track_no:02}")
    };
    format!("{}.{}", sanitize_component(&format!("{prefix} {title}")), ext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_illegal_characters() {
        assert_eq!(sanitize_component("AC/DC: Live?"), "AC_DC_ Live_");
        assert_eq!(sanitize_component("a<b>c\"d|e*f\\g"), "a_b_c_d_e_f_g");
    }

    #[test]
    fn trims_spaces_and_trailing_dots() {
        assert_eq!(sanitize_component("  Hello...  "), "Hello");
        assert_eq!(sanitize_component("Vol. 1."), "Vol. 1");
    }

    #[test]
    fn empty_becomes_underscore_and_reserved_names_prefixed() {
        assert_eq!(sanitize_component("   "), "_");
        assert_eq!(sanitize_component("..."), "_");
        assert_eq!(sanitize_component("con"), "_con");
        assert_eq!(sanitize_component("COM1"), "_COM1");
    }

    #[test]
    fn caps_length_at_120_chars() {
        let long = "x".repeat(300);
        assert_eq!(sanitize_component(&long).chars().count(), 120);
    }

    #[test]
    fn keeps_unicode() {
        assert_eq!(sanitize_component("Sigur Rós – ( )"), "Sigur Rós – ( )");
    }

    #[test]
    fn album_dir_includes_year_when_known() {
        assert_eq!(album_dir("Radiohead", "OK Computer", Some("1997")), PathBuf::from("Radiohead").join("OK Computer (1997)"));
        assert_eq!(album_dir("Radiohead", "OK Computer", None), PathBuf::from("Radiohead").join("OK Computer"));
    }

    #[test]
    fn track_names_single_and_multi_disc() {
        assert_eq!(track_file_name(1, 1, 3, "Airbag", "flac"), "03 Airbag.flac");
        assert_eq!(track_file_name(2, 2, 11, "What?", "mp3"), "2-11 What_.mp3");
        assert_eq!(track_file_name(1, 0, 7, "Seven", "wav"), "07 Seven.wav");
    }
}
