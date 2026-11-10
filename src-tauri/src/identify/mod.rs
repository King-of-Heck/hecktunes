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
        for t in tracks.iter().take(3) {
            for id in source.by_fingerprint(t).unwrap_or_default() {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
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
        let url = format!("https://coverartarchive.org/release/{mbid}/front-500");
        match self.agent.get(&url).header("User-Agent", musicbrainz::USER_AGENT).call() {
            Ok(mut resp) => Ok(Some(resp.body_mut().read_to_vec()?)),
            Err(ureq::Error::StatusCode(404)) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
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
            Identification::Ambiguous(c) => assert_eq!(c.len(), 2),
            other => panic!("expected ambiguous, got {other:?}"),
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
}
