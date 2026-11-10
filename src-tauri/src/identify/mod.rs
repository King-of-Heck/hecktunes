pub mod acoustid;
pub mod fingerprint;
pub mod musicbrainz;
pub mod score;

use crate::model::{ReleaseCandidate, SourceTrack};
use anyhow::Result;
use score::{decide, score, AlbumEvidence, Decision};

pub const MAX_CANDIDATES: usize = 5;

pub trait MetadataSource: Send + Sync {
    /// Release MBIDs for a tag search, best first.
    fn search(&self, album: &str, artist: &str, track_count: usize) -> Result<Vec<String>>;
    /// Release MBIDs for a free-text search typed by the user.
    fn search_text(&self, query: &str) -> Result<Vec<String>>;
    /// Release MBIDs found by audio fingerprint.
    fn by_fingerprint(&self, track: &SourceTrack) -> Result<Vec<String>>;
    fn release(&self, mbid: &str) -> Result<ReleaseCandidate>;
    fn cover(&self, mbid: &str) -> Result<Option<Vec<u8>>>;
}

#[derive(Debug, Clone)]
pub enum Identification {
    Matched(ReleaseCandidate),
    Ambiguous(Vec<ReleaseCandidate>),
    NotFound,
}

pub fn rank(tracks: &[SourceTrack], candidates: Vec<ReleaseCandidate>) -> Vec<ReleaseCandidate> {
    let ev = AlbumEvidence::from_tracks(tracks);
    let mut scored: Vec<(f64, ReleaseCandidate)> = candidates.into_iter().map(|c| (score(&ev, &c), c)).collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored.into_iter().map(|(_, c)| c).collect()
}

pub fn identify(tracks: &[SourceTrack], source: &dyn MetadataSource) -> Result<Identification> {
    let ev = AlbumEvidence::from_tracks(tracks);
    let mut ids: Vec<String> = Vec::new();
    if let (Some(album), Some(artist)) = (&ev.album, &ev.artist) {
        ids = source.search(album, artist, tracks.len())?;
    }
    if ids.is_empty() {
        // Rank by how many fingerprinted tracks agree on a release (stable: ties keep first-seen order).
        let mut counts: Vec<(String, usize)> = Vec::new();
        for t in tracks.iter().take(3) {
            let mut seen: Vec<String> = Vec::new();
            for id in source.by_fingerprint(t).unwrap_or_default() {
                if seen.contains(&id) {
                    continue;
                }
                seen.push(id.clone());
                match counts.iter_mut().find(|(k, _)| *k == id) {
                    Some((_, n)) => *n += 1,
                    None => counts.push((id, 1)),
                }
            }
        }
        counts.sort_by(|a, b| b.1.cmp(&a.1));
        ids = counts.into_iter().map(|(id, _)| id).collect();
    }
    ids.truncate(MAX_CANDIDATES);
    let candidates: Vec<ReleaseCandidate> = ids.iter().filter_map(|id| source.release(id).ok()).collect();
    if candidates.is_empty() {
        return Ok(Identification::NotFound);
    }
    let ranked = rank(tracks, candidates);
    let scores: Vec<f64> = ranked.iter().map(|c| score(&ev, c)).collect();
    Ok(match decide(&ranked, &scores) {
        Decision::Auto(i) => Identification::Matched(ranked[i].clone()),
        Decision::Ask => Identification::Ambiguous(ranked),
    })
}

pub struct OnlineSource {
    mb: musicbrainz::MusicBrainz,
    acoustid: Option<acoustid::AcoustId>,
    agent: ureq::Agent,
}

impl OnlineSource {
    pub fn new(acoustid_key: Option<String>) -> Self {
        Self {
            mb: musicbrainz::MusicBrainz::new(),
            acoustid: acoustid_key.filter(|k| !k.is_empty()).map(acoustid::AcoustId::new),
            agent: musicbrainz::http_agent(),
        }
    }
}

impl MetadataSource for OnlineSource {
    fn search(&self, album: &str, artist: &str, track_count: usize) -> Result<Vec<String>> {
        let mut hits = self.mb.search_releases(album, artist)?;
        // Stable sort: releases with the right number of tracks first, MB relevance order otherwise.
        hits.sort_by_key(|h| h.track_count as usize != track_count);
        Ok(hits.into_iter().map(|h| h.mbid).collect())
    }

    fn search_text(&self, query: &str) -> Result<Vec<String>> {
        Ok(self.mb.search_text(query)?.into_iter().map(|h| h.mbid).collect())
    }

    fn by_fingerprint(&self, track: &SourceTrack) -> Result<Vec<String>> {
        let Some(client) = &self.acoustid else { return Ok(vec![]) };
        let fp = fingerprint::fingerprint_file(&track.path)?;
        client.lookup(&fp, (track.duration_ms / 1000) as u32)
    }

    fn release(&self, mbid: &str) -> Result<ReleaseCandidate> {
        self.mb.release(mbid)
    }

    fn cover(&self, mbid: &str) -> Result<Option<Vec<u8>>> {
        let url = cover_url(mbid);
        match self.agent.get(&url).header("User-Agent", musicbrainz::USER_AGENT).call() {
            Ok(mut resp) => Ok(Some(resp.body_mut().read_to_vec()?)),
            Err(ureq::Error::StatusCode(404)) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

/// Cover Art Archive front image. The 1200 px thumbnail (not 500) so `prepare_cover` can produce a
/// full 600 px cover; it downscales anything larger.
fn cover_url(mbid: &str) -> String {
    format!("https://coverartarchive.org/release/{mbid}/front-1200")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{candidate, FakeSource};

    fn src(title: Option<&str>, album: Option<&str>, artist: Option<&str>, ms: u64) -> SourceTrack {
        SourceTrack {
            path: "x.wav".into(),
            title: title.map(String::from),
            artist: artist.map(String::from),
            album: album.map(String::from),
            album_artist: None,
            track_no: None,
            disc_no: None,
            year: None,
            genre: None,
            duration_ms: ms,
        }
    }

    const TL: [(&str, u64); 2] = [("One", 100_000), ("Two", 200_000)];

    #[test]
    fn matches_a_clear_winner() {
        let fake = FakeSource { releases: vec![candidate("good", "Album", "Band", &TL)], search_ids: vec!["good".into()] };
        let tracks = [src(Some("One"), Some("Album"), Some("Band"), 100_000), src(Some("Two"), Some("Album"), Some("Band"), 200_000)];
        match identify(&tracks, &fake).unwrap() {
            Identification::Matched(r) => assert_eq!(r.mbid, "good"),
            other => panic!("expected match, got {other:?}"),
        }
    }

    #[test]
    fn ambiguous_returns_candidates_best_first() {
        let other = [("Uno", 100_500), ("Dos", 199_000)];
        let fake = FakeSource {
            releases: vec![candidate("b", "Album", "Band", &other), candidate("a", "Album", "Band", &TL)],
            search_ids: vec!["b".into(), "a".into()],
        };
        let tracks = [src(None, Some("Album"), Some("Band"), 100_000), src(None, Some("Album"), Some("Band"), 200_000)];
        match identify(&tracks, &fake).unwrap() {
            Identification::Ambiguous(c) => {
                assert_eq!(c.len(), 2);
                assert_eq!(c[0].mbid, "a", "exact-duration release must rank first");
            }
            other => panic!("expected ambiguous, got {other:?}"),
        }
    }

    struct PerTrackFp {
        inner: FakeSource,
        by_path: Vec<(String, Vec<String>)>,
    }

    impl MetadataSource for PerTrackFp {
        fn search(&self, a: &str, b: &str, n: usize) -> Result<Vec<String>> {
            self.inner.search(a, b, n)
        }
        fn search_text(&self, q: &str) -> Result<Vec<String>> {
            self.inner.search_text(q)
        }
        fn by_fingerprint(&self, t: &SourceTrack) -> Result<Vec<String>> {
            let p = t.path.to_string_lossy().to_string();
            Ok(self.by_path.iter().find(|(k, _)| *k == p).map(|(_, v)| v.clone()).unwrap_or_default())
        }
        fn release(&self, m: &str) -> Result<ReleaseCandidate> {
            self.inner.release(m)
        }
        fn cover(&self, m: &str) -> Result<Option<Vec<u8>>> {
            self.inner.cover(m)
        }
    }

    #[test]
    fn fingerprint_release_shared_by_all_tracks_beats_one_tracks_many_releases() {
        let tl3 = [("One", 100_000), ("Two", 200_000), ("Three", 150_000)];
        let mut releases = vec![candidate("shared", "Album", "Band", &tl3)];
        let mut noisy: Vec<String> = Vec::new();
        for i in 0..MAX_CANDIDATES {
            let id = format!("noise{i}");
            releases.push(candidate(&id, "Other", "Other", &[("Zzz", 1_000)]));
            noisy.push(id);
        }
        let mut first = noisy.clone();
        first.push("shared".into());
        let fake = PerTrackFp {
            inner: FakeSource { releases, search_ids: vec![] },
            by_path: vec![
                ("t0.wav".into(), first),
                ("t1.wav".into(), vec!["shared".into()]),
                ("t2.wav".into(), vec!["shared".into()]),
            ],
        };
        let mk = |p: &str, t: &str, ms| {
            let mut s = src(Some(t), None, None, ms);
            s.path = p.into();
            s
        };
        let tracks = [mk("t0.wav", "One", 100_000), mk("t1.wav", "Two", 200_000), mk("t2.wav", "Three", 150_000)];
        match identify(&tracks, &fake).unwrap() {
            Identification::Matched(r) => assert_eq!(r.mbid, "shared"),
            Identification::Ambiguous(c) => assert_eq!(c[0].mbid, "shared"),
            Identification::NotFound => panic!("shared release was crowded out"),
        }
    }

    #[test]
    fn nothing_found_without_tags_or_fingerprints() {
        let fake = FakeSource { releases: vec![], search_ids: vec![] };
        let tracks = [src(None, None, None, 100_000)];
        assert!(matches!(identify(&tracks, &fake).unwrap(), Identification::NotFound));
    }

    #[test]
    fn rank_sorts_by_score() {
        let tracks = [src(Some("One"), None, None, 100_000), src(Some("Two"), None, None, 200_000)];
        let ranked = rank(&tracks, vec![candidate("bad", "X", "Y", &[("Q", 5_000)]), candidate("good", "X", "Y", &TL)]);
        assert_eq!(ranked[0].mbid, "good");
    }

    #[test]
    #[ignore]
    fn live_musicbrainz_lookup() {
        let src = OnlineSource::new(None);
        let ids = src.search("OK Computer", "Radiohead", 12).unwrap();
        assert!(!ids.is_empty());
        let r = src.release(&ids[0]).unwrap();
        assert_eq!(r.title, "OK Computer");
        assert!(src.cover(&r.mbid).is_ok());
    }

    #[test]
    fn cover_url_asks_for_an_image_at_least_600_px() {
        assert_eq!(cover_url("abc"), "https://coverartarchive.org/release/abc/front-1200");
    }
}
