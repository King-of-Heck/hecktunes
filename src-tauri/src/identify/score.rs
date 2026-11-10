use crate::model::{ReleaseCandidate, SourceTrack};

pub const AUTO_MIN: f64 = 0.85;
pub const AUTO_MARGIN: f64 = 0.08;
/// A duration difference of this many ms (or more) scores 0 for that track.
pub(crate) const DURATION_TOLERANCE_MS: f64 = 10_000.0;

#[derive(Debug, Clone)]
pub struct TrackEvidence {
    pub title: Option<String>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone)]
pub struct AlbumEvidence {
    pub album: Option<String>,
    pub artist: Option<String>,
    pub tracks: Vec<TrackEvidence>,
}

impl AlbumEvidence {
    /// `tracks` must already be in album order.
    pub fn from_tracks(tracks: &[SourceTrack]) -> Self {
        let first = |f: fn(&SourceTrack) -> Option<&String>| tracks.iter().find_map(f).cloned();
        AlbumEvidence {
            album: first(|t| t.album.as_ref()),
            artist: first(|t| t.album_artist.as_ref()).or_else(|| first(|t| t.artist.as_ref())),
            tracks: tracks
                .iter()
                .map(|t| TrackEvidence { title: t.title.clone(), duration_ms: t.duration_ms })
                .collect(),
        }
    }
}

pub fn normalize(s: &str) -> String {
    let mapped: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() { c.to_lowercase().next().unwrap_or(c) } else { ' ' })
        .collect();
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn similarity(a: &str, b: &str) -> f64 {
    strsim::normalized_levenshtein(&normalize(a), &normalize(b))
}

fn mean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, n) = values.fold((0.0, 0usize), |(s, n), v| (s + v, n + 1));
    (n > 0).then(|| sum / n as f64)
}

pub fn score(e: &AlbumEvidence, c: &ReleaseCandidate) -> f64 {
    let (n_src, n_rel) = (e.tracks.len(), c.tracks.len());
    if n_src == 0 || n_rel == 0 {
        return 0.0;
    }
    let count = if n_src == n_rel {
        1.0
    } else {
        0.5 * n_src.min(n_rel) as f64 / n_src.max(n_rel) as f64
    };
    let pairs = || e.tracks.iter().zip(c.tracks.iter());
    let durations = mean(pairs().filter_map(|(s, r)| {
        r.length_ms.map(|len| (1.0 - (s.duration_ms as f64 - len as f64).abs() / DURATION_TOLERANCE_MS).max(0.0))
    }));
    let titles = mean(pairs().filter_map(|(s, r)| s.title.as_deref().map(|t| similarity(t, &r.title))));
    let meta = mean(
        [
            e.album.as_deref().map(|a| similarity(a, &c.title)),
            e.artist.as_deref().map(|a| similarity(a, &c.artist)),
        ]
        .into_iter()
        .flatten(),
    );
    let parts = [(Some(count), 0.30), (durations, 0.35), (titles, 0.20), (meta, 0.15)];
    let (weighted, weights) = parts
        .iter()
        .filter_map(|(v, w)| v.map(|v| (v * w, *w)))
        .fold((0.0, 0.0), |(a, b), (v, w)| (a + v, b + w));
    // Track count mismatch caps the score: an album with missing/extra tracks never auto-applies.
    let s = weighted / weights;
    if n_src == n_rel { s } else { s.min(AUTO_MIN - 0.01) }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Decision {
    Auto(usize),
    Ask,
}

/// Two releases are "equivalent" if they have the same track titles in the same order
/// (e.g. different pressings of one album) — picking either gives the same tags.
fn equivalent(a: &ReleaseCandidate, b: &ReleaseCandidate) -> bool {
    a.tracks.len() == b.tracks.len()
        && normalize(&a.title) == normalize(&b.title)
        && a.tracks.iter().zip(&b.tracks).all(|(x, y)| normalize(&x.title) == normalize(&y.title))
}

pub fn decide(candidates: &[ReleaseCandidate], scores: &[f64]) -> Decision {
    let Some((top, &top_score)) = scores.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)) else {
        return Decision::Ask;
    };
    if top_score < AUTO_MIN {
        return Decision::Ask;
    }
    let runner_up = scores
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != top && !equivalent(&candidates[*i], &candidates[top]))
        .map(|(_, s)| *s)
        .fold(f64::NEG_INFINITY, f64::max);
    if top_score - runner_up >= AUTO_MARGIN {
        Decision::Auto(top)
    } else {
        Decision::Ask
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::candidate;

    fn ev(album: Option<&str>, artist: Option<&str>, tracks: &[(Option<&str>, u64)]) -> AlbumEvidence {
        AlbumEvidence {
            album: album.map(String::from),
            artist: artist.map(String::from),
            tracks: tracks.iter().map(|(t, d)| TrackEvidence { title: t.map(String::from), duration_ms: *d }).collect(),
        }
    }

    const TL: [(&str, u64); 3] = [("Intro", 60_000), ("Song Two", 200_000), ("Finale!", 300_000)];

    #[test]
    fn exact_match_scores_high() {
        let e = ev(Some("Album"), Some("Band"), &[(Some("Intro"), 60_400), (Some("Song two"), 199_000), (Some("Finale"), 301_000)]);
        assert!(score(&e, &candidate("m", "Album", "Band", &TL)) > 0.95);
    }

    #[test]
    fn durations_alone_can_match_untitled_tracks() {
        let e = ev(None, None, &[(None, 60_000), (None, 200_000), (None, 300_000)]);
        assert!(score(&e, &candidate("m", "Album", "Band", &TL)) > 0.95);
    }

    #[test]
    fn wrong_track_count_scores_below_auto() {
        let e = ev(Some("Album"), Some("Band"), &[(Some("Intro"), 60_000), (Some("Song Two"), 200_000)]);
        assert!(score(&e, &candidate("m", "Album", "Band", &TL)) < AUTO_MIN);
    }

    #[test]
    fn durations_far_off_score_low() {
        let e = ev(None, None, &[(None, 120_000), (None, 20_000), (None, 500_000)]);
        assert!(score(&e, &candidate("m", "Album", "Band", &TL)) < 0.5);
    }

    #[test]
    fn evidence_from_tracks_prefers_album_artist() {
        let t = SourceTrack {
            path: "a.flac".into(),
            title: Some("T".into()),
            artist: Some("Feat Guy".into()),
            album: Some("Album".into()),
            album_artist: Some("Band".into()),
            track_no: Some(1),
            disc_no: None,
            year: None,
            genre: None,
            duration_ms: 1000,
        };
        let e = AlbumEvidence::from_tracks(&[t]);
        assert_eq!(e.album.as_deref(), Some("Album"));
        assert_eq!(e.artist.as_deref(), Some("Band"));
        assert_eq!(e.tracks.len(), 1);
    }

    #[test]
    fn normalize_strips_punctuation_and_case() {
        assert_eq!(normalize("  The  Bends! (Remastered) "), "the bends remastered");
    }

    #[test]
    fn decide_auto_for_single_strong_candidate() {
        let c = vec![candidate("a", "X", "Y", &TL)];
        assert!(matches!(decide(&c, &[0.9]), Decision::Auto(0)));
    }

    #[test]
    fn decide_asks_when_top_is_weak_or_empty() {
        let c = vec![candidate("a", "X", "Y", &TL)];
        assert!(matches!(decide(&c, &[0.7]), Decision::Ask));
        assert!(matches!(decide(&[], &[]), Decision::Ask));
    }

    #[test]
    fn decide_asks_when_different_releases_are_close() {
        let other = [("Other", 60_000), ("Tracks", 200_000), ("Here", 300_000)];
        let c = vec![candidate("a", "X", "Y", &TL), candidate("b", "X", "Y", &other)];
        assert!(matches!(decide(&c, &[0.90, 0.88]), Decision::Ask));
    }

    #[test]
    fn decide_auto_when_close_runner_up_is_equivalent_edition() {
        let c = vec![candidate("a", "X", "Y", &TL), candidate("b", "X", "Y", &TL)];
        assert!(matches!(decide(&c, &[0.90, 0.91]), Decision::Auto(1)));
    }
}
