use super::cardpath::PLAYLIST_ENTRY_PREFIX;

pub struct M3uEntry<'a> {
    pub card_path: &'a str,
    pub duration_ms: u64,
    pub artist: &'a str,
    pub title: &'a str,
}

fn one_line(s: &str) -> String {
    s.replace(['\r', '\n'], " ")
}

/// Extended M3U, UTF-8 without BOM: `#EXTINF:<seconds>,<artist> - <title>` then the relative path.
pub fn render(entries: &[M3uEntry]) -> String {
    let mut out = String::from("#EXTM3U\n");
    for e in entries {
        let secs = (e.duration_ms + 500) / 1000;
        out.push_str(&format!(
            "#EXTINF:{secs},{} - {}\n{PLAYLIST_ENTRY_PREFIX}{}\n",
            one_line(e.artist),
            one_line(e.title),
            e.card_path
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_extended_m3u_with_relative_entries() {
        let text = render(&[
            M3uEntry { card_path: "Music/Radiohead/OK Computer (1997)/01 Airbag.flac", duration_ms: 284_400, artist: "Radiohead", title: "Airbag" },
            M3uEntry { card_path: "Music/A/x.mp3", duration_ms: 1_499, artist: "A", title: "B\nC" },
        ]);
        assert_eq!(
            text,
            "#EXTM3U\n#EXTINF:284,Radiohead - Airbag\n../Music/Radiohead/OK Computer (1997)/01 Airbag.flac\n#EXTINF:1,A - B C\n../Music/A/x.mp3\n"
        );
    }

    #[test]
    fn empty_playlist_is_just_the_header() {
        assert_eq!(render(&[]), "#EXTM3U\n");
    }
}
